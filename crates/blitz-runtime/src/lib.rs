// SPDX-License-Identifier: MPL-2.0
//! Host capabilities and application primitives for the Blitz runtime.

pub mod app;
/// Private host-side transport primitives used by the `blitz:app` adapter.
#[allow(dead_code)]
pub(crate) mod app_transport;
pub mod effect_assets;
pub mod host;
pub mod net;
pub mod rpc;
pub mod runtime_graph;

pub use app::{App, AppError, AppOptions, Layer, ServeConfig, ServeOptions};
pub use rpc::{
  CallOptions, Decoder, DirectCaller, JsonValue, Procedure, ProcedureBuilder, ProcedureContext,
  ProcedureKind, Router, Rpc, RpcError, RpcKind, RpcRequest, RpcResponse,
};
