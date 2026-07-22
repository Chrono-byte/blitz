//! Immutable procedure builders and unary execution.

use std::collections::BTreeMap;
use std::fmt;
use std::marker::PhantomData;
use std::net::SocketAddr;
use std::sync::Arc;

use super::decoder::{Decoder, NoInput, shared_decoder};
use super::error::{RpcDecodeError, RpcError};
use super::json::{JsonValue, validate_json};
use super::path::RpcPath;

/// Result returned by a Rust-side RPC handler.
pub type RpcResult = Result<JsonValue, RpcError>;

/// The kind of a built procedure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProcedureKind {
  /// A read-only operation.
  Query,
  /// A state-changing operation.
  Mutation,
  /// A streaming operation reserved for the transport slice.
  Subscription,
}

/// Read-only request information supplied to a handler.
#[derive(Clone, Debug)]
pub struct ProcedureContext {
  /// Decoder output for this request.
  pub input: JsonValue,
  /// Canonical router path.
  pub path: RpcPath,
  /// Immutable request headers.
  pub headers: BTreeMap<String, String>,
  /// Remote peer address, when called over a network transport.
  pub remote_addr: Option<SocketAddr>,
  /// Correlation identifier.
  pub request_id: String,
}

/// The continuation passed to middleware.
pub type Next = Arc<dyn Fn(ProcedureContext) -> RpcResult + Send + Sync>;

/// Ordered middleware around a procedure handler.
pub trait Middleware: Send + Sync {
  /// Run middleware and optionally invoke the downstream continuation.
  fn handle(
    &self,
    context: ProcedureContext,
    next: Next,
  ) -> RpcResult;
}

impl<F> Middleware for F
where
  F: Fn(ProcedureContext, Next) -> RpcResult + Send + Sync,
{
  fn handle(
    &self,
    context: ProcedureContext,
    next: Next,
  ) -> RpcResult {
    self(context, next)
  }
}

/// An immutable, executable procedure definition.
#[derive(Clone)]
pub struct Procedure {
  kind: ProcedureKind,
  decoder: Arc<dyn Decoder>,
  metadata: Option<JsonValue>,
  middleware: Arc<Vec<Arc<dyn Middleware>>>,
  handler: Arc<dyn Fn(ProcedureContext) -> RpcResult + Send + Sync>,
}

impl fmt::Debug for Procedure {
  fn fmt(
    &self,
    formatter: &mut fmt::Formatter<'_>,
  ) -> fmt::Result {
    formatter
      .debug_struct("Procedure")
      .field("kind", &self.kind)
      .field("metadata", &self.metadata)
      .field("middleware_count", &self.middleware.len())
      .finish_non_exhaustive()
  }
}

impl Procedure {
  /// Start a compile-safe procedure builder.
  #[must_use]
  pub fn builder() -> ProcedureBuilder<NeedsInput> {
    ProcedureBuilder::new()
  }

  /// Return this procedure's operation kind.
  #[must_use]
  pub const fn kind(&self) -> ProcedureKind {
    self.kind
  }

  /// Return immutable procedure metadata.
  #[must_use]
  pub const fn metadata(&self) -> Option<&JsonValue> {
    self.metadata.as_ref()
  }

  /// Whether this procedure has an explicit input decoder.
  #[must_use]
  pub const fn has_decoder(&self) -> bool {
    true
  }

  /// Decode input and execute this procedure with a request context.
  pub fn execute(
    &self,
    context: ProcedureContext,
  ) -> RpcResult {
    let input = self
      .decoder
      .decode(&context.input)
      .map_err(RpcError::decode)?;
    let context = ProcedureContext { input, ..context };
    let handler = Arc::clone(&self.handler);
    let middleware = Arc::clone(&self.middleware);
    let next: Next = Arc::new(move |context| (handler)(context));
    let result = invoke_middleware(&middleware, 0, context, next);
    match result {
      Ok(value) => {
        validate_json(&value)
          .map_err(|error| RpcError::framework("RpcSerializationError", error.to_string()))?;
        Ok(value)
      }
      Err(error) => safe_error(error),
    }
  }
}

fn safe_error(error: RpcError) -> RpcResult {
  if let RpcError::Domain(value) = &error {
    let tagged = value.as_object().is_some_and(|object| {
      object
        .get("_tag")
        .is_some_and(|tag| tag.as_str().is_some_and(|tag| !tag.is_empty()))
    });
    if !tagged || validate_json(value).is_err() {
      return Err(RpcError::framework(
        "RpcSerializationError",
        "domain error must be a tagged JSON-safe object",
      ));
    }
  }
  Err(error)
}

fn invoke_middleware(
  middleware: &Arc<Vec<Arc<dyn Middleware>>>,
  index: usize,
  context: ProcedureContext,
  terminal: Next,
) -> RpcResult {
  let Some(current) = middleware.get(index).cloned() else {
    return terminal(context);
  };
  let middleware = Arc::clone(middleware);
  let next_terminal = Arc::new(move |context| {
    invoke_middleware(&middleware, index + 1, context, Arc::clone(&terminal))
  });
  current.handle(context, next_terminal)
}

/// Marker for a builder that still needs an input decoder.
#[derive(Clone, Copy, Debug)]
pub struct NeedsInput;
/// Marker for a builder with an input decoder.
#[derive(Clone, Copy, Debug)]
pub struct HasInput;

/// Immutable procedure builder. A handler cannot be attached until `.input()`
/// has been called, including with [`NoInput`].
#[derive(Clone)]
pub struct ProcedureBuilder<State> {
  decoder: Option<Arc<dyn Decoder>>,
  metadata: Option<JsonValue>,
  middleware: Vec<Arc<dyn Middleware>>,
  marker: PhantomData<State>,
}

impl<State> ProcedureBuilder<State> {
  fn with_state<NextState>(
    self,
    decoder: Option<Arc<dyn Decoder>>,
  ) -> ProcedureBuilder<NextState> {
    ProcedureBuilder {
      decoder,
      metadata: self.metadata,
      middleware: self.middleware,
      marker: PhantomData,
    }
  }

  /// Add JSON-safe metadata without changing the previous builder.
  #[must_use]
  pub fn meta(
    self,
    metadata: JsonValue,
  ) -> Self {
    // JsonValue cannot contain cycles or JavaScript-only values. Keep the
    // fallible variant available for callers that want an explicit check.
    let mut builder = self;
    builder.metadata = Some(metadata);
    builder
  }

  /// Add metadata while returning an explicit JSON-safety error.
  pub fn try_meta(
    mut self,
    metadata: JsonValue,
  ) -> Result<Self, ProcedureBuildError> {
    validate_json(&metadata)
      .map_err(|error| ProcedureBuildError::InvalidMetadata(error.to_string()))?;
    self.metadata = Some(metadata);
    Ok(self)
  }

  /// Append middleware without mutating the previous builder.
  pub fn use_middleware<M>(
    mut self,
    middleware: M,
  ) -> Self
  where
    M: Middleware + 'static,
  {
    self.middleware.push(Arc::new(middleware));
    self
  }
}

impl ProcedureBuilder<NeedsInput> {
  fn new() -> Self {
    Self {
      decoder: None,
      metadata: None,
      middleware: Vec::new(),
      marker: PhantomData,
    }
  }

  /// Attach the required decoder and advance the builder state.
  #[must_use]
  pub fn input<D>(
    self,
    decoder: D,
  ) -> ProcedureBuilder<HasInput>
  where
    D: Decoder + 'static,
  {
    self.with_state(Some(shared_decoder(decoder)))
  }
}

impl ProcedureBuilder<HasInput> {
  fn finish<F>(
    self,
    kind: ProcedureKind,
    handler: F,
  ) -> Procedure
  where
    F: Fn(ProcedureContext) -> RpcResult + Send + Sync + 'static,
  {
    Procedure {
      kind,
      decoder: self
        .decoder
        .expect("the HasInput state always has a decoder"),
      metadata: self.metadata,
      middleware: Arc::new(self.middleware),
      handler: Arc::new(handler),
    }
  }

  /// Define a query handler.
  #[must_use]
  pub fn query<F>(
    self,
    handler: F,
  ) -> Procedure
  where
    F: Fn(ProcedureContext) -> RpcResult + Send + Sync + 'static,
  {
    self.finish(ProcedureKind::Query, handler)
  }

  /// Define a mutation handler.
  #[must_use]
  pub fn mutation<F>(
    self,
    handler: F,
  ) -> Procedure
  where
    F: Fn(ProcedureContext) -> RpcResult + Send + Sync + 'static,
  {
    self.finish(ProcedureKind::Mutation, handler)
  }

  /// Define a subscription placeholder. Stream transport is intentionally not
  /// implemented in this slice; the procedure remains discoverable and typed.
  #[must_use]
  pub fn subscription<F>(
    self,
    handler: F,
  ) -> Procedure
  where
    F: Fn(ProcedureContext) -> RpcResult + Send + Sync + 'static,
  {
    self.finish(ProcedureKind::Subscription, handler)
  }
}

/// Errors raised while constructing a procedure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProcedureBuildError {
  /// Metadata was not JSON-safe.
  InvalidMetadata(String),
}

impl fmt::Display for ProcedureBuildError {
  fn fmt(
    &self,
    formatter: &mut fmt::Formatter<'_>,
  ) -> fmt::Result {
    match self {
      Self::InvalidMetadata(message) => write!(formatter, "invalid procedure metadata: {message}"),
    }
  }
}

impl std::error::Error for ProcedureBuildError {}

/// A convenient no-input decoder value.
#[must_use]
pub const fn no_input() -> NoInput {
  NoInput
}

/// Construct a decoder error for use in a closure decoder.
#[must_use]
pub fn decode_error(message: impl Into<String>) -> RpcDecodeError {
  RpcDecodeError::new(message)
}

#[cfg(test)]
mod tests {
  use std::sync::{Arc, Mutex};

  use super::*;

  fn context(input: JsonValue) -> ProcedureContext {
    ProcedureContext {
      input,
      path: RpcPath::parse("test.run").expect("test path"),
      headers: BTreeMap::new(),
      remote_addr: None,
      request_id: "test".to_string(),
    }
  }

  #[test]
  fn builder_is_immutable_and_requires_input() {
    let base = Procedure::builder().meta(serde_json::json!({"version": 1}));
    let first = base
      .clone()
      .input(NoInput)
      .query(|context| Ok(context.input));
    let second = base
      .input(NoInput)
      .mutation(|context| Ok(context.input));
    assert_eq!(first.kind(), ProcedureKind::Query);
    assert_eq!(second.kind(), ProcedureKind::Mutation);
    assert_eq!(first.metadata(), Some(&serde_json::json!({"version": 1})));
  }

  #[test]
  fn middleware_runs_in_declaration_order() {
    let order = Arc::new(Mutex::new(Vec::new()));
    let first_order = Arc::clone(&order);
    let second_order = Arc::clone(&order);
    let procedure = Procedure::builder()
      .input(NoInput)
      .use_middleware(move |context: ProcedureContext, next: Next| {
        first_order
          .lock()
          .expect("first lock")
          .push("first");
        next(context)
      })
      .use_middleware(move |context: ProcedureContext, next: Next| {
        second_order
          .lock()
          .expect("second lock")
          .push("second");
        next(context)
      })
      .query(|context| Ok(context.input));

    procedure
      .execute(context(JsonValue::Null))
      .expect("execute");
    assert_eq!(*order.lock().expect("result lock"), ["first", "second"]);
  }

  #[test]
  fn invalid_domain_errors_are_redacted_before_wire_use() {
    let procedure = Procedure::builder()
      .input(NoInput)
      .query(|_| Err(RpcError::Domain(serde_json::json!({"message": "secret"}))));
    let error = procedure
      .execute(context(JsonValue::Null))
      .expect_err("invalid domain error");
    assert!(matches!(error, RpcError::Framework(_)));
  }
}
