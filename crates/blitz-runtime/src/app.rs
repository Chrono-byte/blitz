// SPDX-License-Identifier: MPL-2.0

use std::fmt;
use std::sync::Arc;

use crate::rpc::{DirectCaller, Router, RouterError};

/// Default RPC endpoint used by application transport adapters.
pub const DEFAULT_RPC_PATH: &str = "/rpc";

/// A named application layer descriptor.
///
/// Layers are recorded immutably in this host-neutral description. Effect
/// layer evaluation belongs to the JavaScript facade.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Layer {
  name: String,
}

impl Layer {
  /// Create a named layer descriptor.
  pub fn named(name: impl Into<String>) -> Self {
    Self { name: name.into() }
  }

  /// Return the layer name.
  #[must_use]
  pub fn name(&self) -> &str {
    &self.name
  }
}

/// Application construction options.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppOptions {
  /// RPC endpoint path.
  pub rpc_path: String,
}

impl Default for AppOptions {
  fn default() -> Self {
    Self {
      rpc_path: DEFAULT_RPC_PATH.to_string(),
    }
  }
}

/// Immutable runtime application description.
#[derive(Clone, Debug)]
pub struct App {
  /// The application RPC router.
  pub rpc: Router,
  layers: Arc<Vec<Layer>>,
  options: AppOptions,
}

impl App {
  /// Construct an application from a router.
  #[must_use]
  pub fn make(router: Router) -> Self {
    Self {
      rpc: router,
      layers: Arc::new(Vec::new()),
      options: AppOptions::default(),
    }
  }

  /// Return a copy with one additional provided layer.
  #[must_use]
  pub fn provide(
    &self,
    layer: Layer,
  ) -> Self {
    let mut layers = (*self.layers).clone();
    layers.push(layer);
    Self {
      rpc: self.rpc.clone(),
      layers: Arc::new(layers),
      options: self.options.clone(),
    }
  }

  /// Return a copy with application options.
  ///
  /// # Errors
  ///
  /// Returns an error when the configured RPC endpoint is not a valid path.
  pub fn with_options(
    &self,
    options: AppOptions,
  ) -> Result<Self, AppError> {
    validate_rpc_path(&options.rpc_path)?;
    Ok(Self {
      rpc: self.rpc.clone(),
      layers: Arc::clone(&self.layers),
      options,
    })
  }

  /// Return the configured layers.
  #[must_use]
  pub fn layers(&self) -> &[Layer] {
    self.layers.as_ref()
  }

  /// Return the configured RPC path.
  #[must_use]
  pub fn rpc_path(&self) -> &str {
    &self.options.rpc_path
  }

  /// Create an in-process caller for the application's router.
  #[must_use]
  pub fn create_caller(&self) -> DirectCaller {
    self.rpc.create_caller()
  }

  /// Create a lazy host configuration for an application transport.
  ///
  /// No socket is bound by this method. The JavaScript `blitz:app` facade
  /// builds the actual scoped Effect; this value is only a Rust-side adapter
  /// configuration and preserves the low-level `Blitz.serve` API separately.
  #[must_use]
  pub fn serve(
    &self,
    options: ServeOptions,
  ) -> ServeConfig {
    ServeConfig {
      app: self.clone(),
      options,
    }
  }
}

/// Options passed to an application transport adapter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServeOptions {
  /// Bind hostname or address.
  pub hostname: String,
  /// Bind port, where zero requests an OS-selected port.
  pub port: u16,
  /// RPC endpoint path.
  pub rpc_path: String,
}

impl Default for ServeOptions {
  fn default() -> Self {
    Self {
      hostname: "127.0.0.1".to_string(),
      port: 8000,
      rpc_path: DEFAULT_RPC_PATH.to_string(),
    }
  }
}

/// A lazy Rust-side configuration of application serving.
///
/// This is intentionally not the JavaScript Effect returned by
/// `AppDescription.serve`.
#[derive(Clone, Debug)]
pub struct ServeConfig {
  app: App,
  options: ServeOptions,
}

impl ServeConfig {
  /// Return the application captured by this plan.
  #[must_use]
  pub fn app(&self) -> &App {
    &self.app
  }

  /// Return transport options.
  #[must_use]
  pub fn options(&self) -> &ServeOptions {
    &self.options
  }
}

/// Errors in application configuration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AppError {
  /// The configured endpoint path is invalid.
  InvalidRpcPath(String),
  /// A router could not be assembled.
  Router(RouterError),
}

impl fmt::Display for AppError {
  fn fmt(
    &self,
    formatter: &mut fmt::Formatter<'_>,
  ) -> fmt::Result {
    match self {
      Self::InvalidRpcPath(message) => write!(formatter, "invalid RPC path: {message}"),
      Self::Router(error) => error.fmt(formatter),
    }
  }
}

impl std::error::Error for AppError {}

impl From<RouterError> for AppError {
  fn from(error: RouterError) -> Self {
    Self::Router(error)
  }
}

fn validate_rpc_path(path: &str) -> Result<(), AppError> {
  if !path.starts_with('/') || path.len() == 1 || path.contains('?') || path.contains('#') {
    return Err(AppError::InvalidRpcPath(
      "RPC endpoint must be a non-root path without query or fragment".to_string(),
    ));
  }
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::rpc::{Rpc, RpcKind, RpcRequest};

  #[test]
  fn app_builders_preserve_previous_values() {
    let procedure = Rpc::procedure()
      .input(Rpc::no_input())
      .query(|_| Ok(serde_json::json!({"ok": true})));
    let router = Router::new()
      .route("health", procedure)
      .expect("route");
    let app = App::make(router);
    let layered = app.provide(Layer::named("database"));
    assert!(app.layers().is_empty());
    assert_eq!(layered.layers()[0].name(), "database");
    assert_eq!(app.rpc_path(), "/rpc");
  }

  #[test]
  fn serve_preserves_the_requested_configuration() {
    let router = Router::new();
    let app = App::make(router);
    let config = app.serve(ServeOptions {
      hostname: "127.0.0.1".to_string(),
      port: 4321,
      rpc_path: "/custom".to_string(),
    });
    assert_eq!(config.options().port, 4321);
    assert_eq!(config.options().hostname, "127.0.0.1");
    assert_eq!(config.options().rpc_path, "/custom");
    assert!(config.app().layers().is_empty());
  }

  #[test]
  fn app_router_can_dispatch_without_transport() {
    let procedure = Rpc::procedure()
      .input(Rpc::no_input())
      .query(|_| Ok(serde_json::json!("ok")));
    let router = Router::new()
      .route("health", procedure)
      .expect("route");
    let app = App::make(router);
    let request =
      RpcRequest::new("1", RpcKind::Query, "health", serde_json::Value::Null).expect("request");
    assert_eq!(
      app.rpc.dispatch(&request).data,
      Some(serde_json::json!("ok"))
    );
  }
}
