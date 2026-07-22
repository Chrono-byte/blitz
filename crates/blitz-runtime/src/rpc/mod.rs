//! Rust-side RPC descriptions for Blitz applications.
//!
//! This module intentionally stops at validated JSON envelopes and direct
//! execution. HTTP and WebSocket adapters can consume these types later
//! without changing procedure or router semantics.

mod decoder;
mod error;
mod json;
mod path;
mod procedure;
mod protocol;
mod router;

pub use decoder::{Decoder, FnDecoder, NoInput, SerdeDecoder, decoder, shared_decoder};
pub use error::{RpcDecodeError, RpcError, RpcFrameworkError};
pub use json::{JsonSafetyError, JsonValue, validate_json};
pub use path::{PathError, RpcPath};
pub use procedure::{
  HasInput, Middleware, NeedsInput, Next, Procedure, ProcedureBuildError, ProcedureBuilder,
  ProcedureContext, ProcedureKind, RpcResult, decode_error, no_input,
};
pub use protocol::{
  DEFAULT_MAX_BATCH_SIZE, ProtocolError, RpcKind, RpcRequest, RpcResponse, parse_requests,
};
pub use router::{CallOptions, DirectCaller, Router, RouterBuilder, RouterError};

/// Start an immutable procedure builder.
#[must_use]
pub fn procedure() -> ProcedureBuilder<NeedsInput> {
  Procedure::builder()
}

/// Start an immutable nested-router builder.
#[must_use]
pub fn router() -> RouterBuilder {
  Router::builder()
}

/// RPC namespace with the construction helpers used by the runtime API.
pub struct Rpc;

impl Rpc {
  /// Start an immutable procedure builder.
  #[must_use]
  pub fn procedure() -> ProcedureBuilder<NeedsInput> {
    Procedure::builder()
  }

  /// Return the explicit no-input decoder.
  #[must_use]
  pub const fn no_input() -> NoInput {
    no_input()
  }

  /// Construct a decoder error for a custom decoder.
  #[must_use]
  pub fn decode_error(message: impl Into<String>) -> RpcDecodeError {
    decode_error(message)
  }

  /// Start an immutable nested-router builder.
  #[must_use]
  pub fn router() -> RouterBuilder {
    Router::builder()
  }

  /// Create an in-process caller without serializing values.
  #[must_use]
  pub fn create_caller(router: &Router) -> DirectCaller {
    router.create_caller()
  }

  /// Parse one request or a bounded batch from a JSON value.
  pub fn parse_requests(value: JsonValue) -> Result<Vec<RpcRequest>, ProtocolError> {
    parse_requests(value, DEFAULT_MAX_BATCH_SIZE)
  }
}
