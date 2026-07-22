//! Immutable nested routers and direct invocation.

use std::collections::BTreeMap;
use std::fmt;
use std::net::SocketAddr;
use std::sync::Arc;

use super::error::RpcError;
use super::json::JsonValue;
use super::path::{PathError, RpcPath};
use super::procedure::{Procedure, ProcedureContext, ProcedureKind, RpcResult};
use super::protocol::{RpcKind, RpcRequest, RpcResponse};

#[derive(Clone)]
enum Route {
  Procedure(Arc<Procedure>),
  Router(Router),
}

/// An immutable tree of named procedures and nested routers.
#[derive(Clone, Default)]
pub struct Router {
  routes: Arc<BTreeMap<String, Route>>,
}

impl fmt::Debug for Router {
  fn fmt(
    &self,
    formatter: &mut fmt::Formatter<'_>,
  ) -> fmt::Result {
    formatter
      .debug_struct("Router")
      .field("paths", &self.paths())
      .finish()
  }
}

impl Router {
  /// Construct an empty router.
  #[must_use]
  pub fn new() -> Self {
    Self::default()
  }

  /// Start an immutable router builder.
  #[must_use]
  pub fn builder() -> RouterBuilder {
    RouterBuilder::new()
  }

  /// Add a procedure at a dotted path.
  pub fn route(
    &self,
    path: impl AsRef<str>,
    procedure: Procedure,
  ) -> Result<Self, RouterError> {
    self.try_route(path, procedure)
  }

  /// Alias for [`Router::route`] that makes immutable use explicit.
  pub fn try_route(
    &self,
    path: impl AsRef<str>,
    procedure: Procedure,
  ) -> Result<Self, RouterError> {
    let path = RpcPath::parse(path)?;
    let routes = insert_procedure(&self.routes, path.segments().collect(), Arc::new(procedure))?;
    Ok(Self {
      routes: Arc::new(routes),
    })
  }

  /// Add a complete child router beneath a path prefix.
  pub fn nest(
    &self,
    prefix: impl AsRef<str>,
    child: Router,
  ) -> Result<Self, RouterError> {
    let prefix = RpcPath::parse(prefix)?;
    let mut result = self.clone();
    for path in child.paths() {
      let full_path = prefix.join(&path)?;
      let Some(procedure) = child.resolve(&path) else {
        continue;
      };
      result = result.try_route(full_path, (*procedure).clone())?;
    }
    Ok(result)
  }

  /// Resolve a dotted path to a procedure.
  #[must_use]
  pub fn resolve(
    &self,
    path: impl AsRef<str>,
  ) -> Option<Arc<Procedure>> {
    let path = RpcPath::parse(path).ok()?;
    resolve_segments(&self.routes, &path.segments().collect::<Vec<_>>())
  }

  /// Return all leaf procedure paths in stable order.
  #[must_use]
  pub fn paths(&self) -> Vec<String> {
    let mut paths = Vec::new();
    collect_paths(&self.routes, String::new(), &mut paths);
    paths
  }

  /// Create an in-process caller for this router.
  #[must_use]
  pub fn create_caller(&self) -> DirectCaller {
    DirectCaller::new(self.clone())
  }

  /// Dispatch a validated wire request without starting a network server.
  #[must_use]
  pub fn dispatch(
    &self,
    request: &RpcRequest,
  ) -> RpcResponse {
    let result = self.create_caller().call_kind(
      request.kind,
      &request.path,
      request.input.clone(),
      CallOptions {
        request_id: request.id.clone(),
        ..CallOptions::default()
      },
    );
    match result {
      Ok(data) => {
        RpcResponse::success(&request.id, data).unwrap_or_else(|_| framework_response(&request.id))
      }
      Err(error) => {
        RpcResponse::failure(&request.id, error).unwrap_or_else(|_| framework_response(&request.id))
      }
    }
  }
}

fn insert_procedure(
  routes: &BTreeMap<String, Route>,
  segments: Vec<&str>,
  procedure: Arc<Procedure>,
) -> Result<BTreeMap<String, Route>, RouterError> {
  let Some((head, tail)) = segments.split_first() else {
    return Err(RouterError::new("router path must not be empty"));
  };
  let mut result = routes.clone();
  if tail.is_empty() {
    if result.contains_key(*head) {
      return Err(RouterError::new(format!("route already exists: {head}")));
    }
    result.insert((*head).to_string(), Route::Procedure(procedure));
    return Ok(result);
  }

  let nested = match result.remove(*head) {
    None => Router::new(),
    Some(Route::Router(nested_router)) => nested_router,
    Some(Route::Procedure(_)) => {
      return Err(RouterError::new(format!(
        "route prefix is already a procedure: {head}"
      )));
    }
  };
  let nested_routes = insert_procedure(&nested.routes, tail.to_vec(), procedure)?;
  result.insert(
    (*head).to_string(),
    Route::Router(Router {
      routes: Arc::new(nested_routes),
    }),
  );
  Ok(result)
}

fn resolve_segments(
  routes: &BTreeMap<String, Route>,
  segments: &[&str],
) -> Option<Arc<Procedure>> {
  let (head, tail) = segments.split_first()?;
  match routes.get(*head)? {
    Route::Procedure(procedure) if tail.is_empty() => Some(Arc::clone(procedure)),
    Route::Router(nested_router) => resolve_segments(&nested_router.routes, tail),
    Route::Procedure(_) => None,
  }
}

fn collect_paths(
  routes: &BTreeMap<String, Route>,
  prefix: String,
  output: &mut Vec<String>,
) {
  for (name, route) in routes {
    let path = if prefix.is_empty() {
      name.clone()
    } else {
      format!("{prefix}.{name}")
    };
    match route {
      Route::Procedure(_) => output.push(path),
      Route::Router(nested_router) => collect_paths(&nested_router.routes, path, output),
    }
  }
}

fn framework_response(id: &str) -> RpcResponse {
  RpcResponse {
    id: id.to_string(),
    ok: false,
    data: None,
    error: Some(
      RpcError::framework("RpcFrameworkError", "failed to encode RPC response").to_json(),
    ),
  }
}

/// Fluent immutable router builder.
#[derive(Clone, Debug, Default)]
pub struct RouterBuilder {
  router: Router,
  error: Option<RouterError>,
}

impl RouterBuilder {
  fn new() -> Self {
    Self::default()
  }

  /// Add a procedure and defer path errors until [`Self::build`].
  #[must_use]
  pub fn procedure(
    mut self,
    path: impl AsRef<str>,
    procedure: Procedure,
  ) -> Self {
    if self.error.is_none() {
      match self.router.try_route(path, procedure) {
        Ok(router) => self.router = router,
        Err(error) => self.error = Some(error),
      }
    }
    self
  }

  /// Add a nested router beneath a prefix.
  #[must_use]
  pub fn router(
    mut self,
    prefix: impl AsRef<str>,
    child: Router,
  ) -> Self {
    if self.error.is_none() {
      match self.router.nest(prefix, child) {
        Ok(router) => self.router = router,
        Err(error) => self.error = Some(error),
      }
    }
    self
  }

  /// Finish the builder.
  pub fn build(self) -> Result<Router, RouterError> {
    self.error.map_or(Ok(self.router), Err)
  }
}

/// Options supplied to an in-process call.
#[derive(Clone, Debug, Default)]
pub struct CallOptions {
  /// Correlation identifier used in diagnostics.
  pub request_id: String,
  /// Lowercase request headers.
  pub headers: BTreeMap<String, String>,
  /// Remote peer address, if any.
  pub remote_addr: Option<SocketAddr>,
}

/// A direct, serialization-free caller for testing and in-process use.
#[derive(Clone, Debug)]
pub struct DirectCaller {
  router: Router,
}

impl DirectCaller {
  /// Construct a caller from a router.
  #[must_use]
  pub fn new(router: Router) -> Self {
    Self { router }
  }

  /// Call a query or mutation by path.
  pub fn call(
    &self,
    path: impl AsRef<str>,
    input: JsonValue,
  ) -> RpcResult {
    self.call_kind(RpcKind::Query, path, input, CallOptions::default())
  }

  /// Call with explicit request metadata and operation kind.
  pub fn call_kind(
    &self,
    kind: RpcKind,
    path: impl AsRef<str>,
    input: JsonValue,
    options: CallOptions,
  ) -> RpcResult {
    let path_string = path.as_ref().to_string();
    let procedure = self
      .router
      .resolve(&path_string)
      .ok_or_else(|| RpcError::unknown_path(&path_string))?;
    if !kind_matches(kind, procedure.kind()) {
      return Err(RpcError::framework(
        "RpcKindError",
        format!("request kind does not match procedure {path_string}"),
      ));
    }
    let path = RpcPath::parse(&path_string)
      .map_err(|error| RpcError::framework("RpcRouteError", error.to_string()))?;
    procedure
      .execute(ProcedureContext {
        input,
        path,
        headers: options.headers,
        remote_addr: options.remote_addr,
        request_id: options.request_id.clone(),
      })
      .map_err(|error| error.with_request_id(&options.request_id))
  }
}

fn kind_matches(
  request: RpcKind,
  procedure: ProcedureKind,
) -> bool {
  matches!(
    (request, procedure),
    (RpcKind::Query, ProcedureKind::Query)
      | (RpcKind::Mutation, ProcedureKind::Mutation)
      | (RpcKind::Subscription, ProcedureKind::Subscription)
  )
}

/// Errors raised while assembling a router.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RouterError {
  /// A human-readable route error.
  pub message: String,
}

impl RouterError {
  fn new(message: impl Into<String>) -> Self {
    Self {
      message: message.into(),
    }
  }
}

impl From<PathError> for RouterError {
  fn from(error: PathError) -> Self {
    Self::new(error.message)
  }
}

impl fmt::Display for RouterError {
  fn fmt(
    &self,
    formatter: &mut fmt::Formatter<'_>,
  ) -> fmt::Result {
    self.message.fmt(formatter)
  }
}

impl std::error::Error for RouterError {}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::rpc::decoder::NoInput;

  fn procedure() -> Procedure {
    Procedure::builder()
      .input(NoInput)
      .query(|context| Ok(context.input))
  }

  #[test]
  fn nested_routes_are_immutable_and_resolvable() {
    let users = Router::new()
      .route("get", procedure())
      .expect("user route");
    let root = Router::new()
      .nest("users", users)
      .expect("nested route");
    assert!(root.resolve("users.get").is_some());
    assert!(root.resolve("get").is_none());
    assert!(root.paths().contains(&"users.get".to_string()));
  }

  #[test]
  fn direct_caller_preserves_decoder_failures() {
    let procedure = Procedure::builder()
      .input(crate::rpc::decoder(|value| {
        if value.is_string() {
          Ok(value.clone())
        } else {
          Err(crate::rpc::decode_error("expected a string"))
        }
      }))
      .query(|context| Ok(context.input));
    let router = Router::new()
      .route("echo", procedure)
      .expect("route");
    let caller = router.create_caller();
    assert_eq!(
      caller
        .call("echo", serde_json::json!("ok"))
        .expect("call"),
      serde_json::json!("ok")
    );
    assert!(matches!(
      caller.call("echo", serde_json::json!(1)),
      Err(RpcError::Decode(_))
    ));
  }
}
