// SPDX-License-Identifier: MPL-2.0

use std::fmt;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;

use tokio::sync::{Mutex, Notify, mpsc};
use tokio::task::JoinHandle;

use crate::net::http::{HttpRequestEvent, HttpResponse, HttpServer};
use crate::net::websocket::{WebSocketEvent, WebSocketServerOptions};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AppTransportOptions {
  pub(crate) hostname: String,
  pub(crate) port: u16,
  pub(crate) rpc_path: String,
}

impl AppTransportOptions {
  fn address(&self) -> Result<SocketAddr, AppTransportError> {
    if self.hostname.is_empty() {
      return Err(AppTransportError::InvalidOptions(
        "hostname must not be empty".to_string(),
      ));
    }
    format!("{}:{}", self.hostname, self.port)
      .parse()
      .map_err(|_| {
        AppTransportError::InvalidOptions(
          "hostname must be an IP address in the current host adapter".to_string(),
        )
      })
  }

  fn validate(&self) -> Result<(), AppTransportError> {
    if !self.rpc_path.starts_with('/')
      || self.rpc_path.len() <= 1
      || self.rpc_path.contains('?')
      || self.rpc_path.contains('#')
      || self.rpc_path.chars().any(char::is_whitespace)
    {
      return Err(AppTransportError::InvalidOptions(
        "RPC path must be a non-root path without query, fragment, or whitespace".to_string(),
      ));
    }
    let _address = self.address()?;
    Ok(())
  }
}

#[derive(Debug)]
pub(crate) enum AppTransportError {
  InvalidOptions(String),
  Bind(io::Error),
}

impl fmt::Display for AppTransportError {
  fn fmt(
    &self,
    formatter: &mut fmt::Formatter<'_>,
  ) -> fmt::Result {
    match self {
      Self::InvalidOptions(message) => {
        write!(formatter, "invalid application transport: {message}")
      }
      Self::Bind(error) => write!(formatter, "failed to bind application transport: {error}"),
    }
  }
}

impl std::error::Error for AppTransportError {}

struct TransportState {
  server: Option<HttpServer>,
  forwarder: Option<JoinHandle<()>>,
  closing: bool,
  closed: bool,
}

pub(crate) struct AppServerResource {
  state: Arc<Mutex<TransportState>>,
  closed: Notify,
  local_addr: SocketAddr,
}

impl AppServerResource {
  pub(crate) const fn local_addr(&self) -> SocketAddr {
    self.local_addr
  }

  pub(crate) async fn await_shutdown(&self) {
    let notified = self.closed.notified();
    if self.state.lock().await.closed {
      return;
    }
    notified.await;
  }

  pub(crate) async fn close(&self) {
    let (server, forwarder) = {
      let mut state = self.state.lock().await;
      if state.closed {
        return;
      }
      if state.closing {
        drop(state);
        self.await_shutdown().await;
        return;
      }
      state.closing = true;
      let server = state.server.take();
      let forwarder = state.forwarder.take();
      (server, forwarder)
    };

    if let Some(server) = server {
      server.close().await;
    }
    if let Some(forwarder) = forwarder {
      forwarder.abort();
      let _result = forwarder.await;
    }

    let mut state = self.state.lock().await;
    if !state.closed {
      state.closed = true;
      self.closed.notify_waiters();
    }
  }

  pub(crate) async fn close_owned(self: Arc<Self>) {
    self.close().await;
  }
}

pub(crate) async fn open_app_server(
  options: AppTransportOptions,
  requests: mpsc::Sender<HttpRequestEvent>,
  websocket_events: mpsc::Sender<WebSocketEvent>,
) -> Result<AppServerResource, AppTransportError> {
  options.validate()?;
  let address = options.address()?;
  let (transport_requests, mut incoming) = mpsc::channel::<HttpRequestEvent>(64);
  let rpc_path = options.rpc_path.clone();
  let forwarder = tokio::spawn(async move {
    while let Some(event) = incoming.recv().await {
      if event.path == rpc_path {
        if requests.send(event).await.is_err() {
          break;
        }
      } else {
        event.respond(HttpResponse::text(404, "Not Found"));
      }
    }
  });
  let server = match HttpServer::bind(
    address,
    transport_requests,
    websocket_events,
    WebSocketServerOptions::default(),
  )
  .await
  {
    Ok(server) => server,
    Err(error) => {
      forwarder.abort();
      let _result = forwarder.await;
      return Err(AppTransportError::Bind(error));
    }
  };
  let local_addr = server.local_addr();
  Ok(AppServerResource {
    state: Arc::new(Mutex::new(TransportState {
      server: Some(server),
      forwarder: Some(forwarder),
      closing: false,
      closed: false,
    })),
    closed: Notify::new(),
    local_addr,
  })
}

#[cfg(test)]
mod tests {
  use super::*;

  fn options() -> AppTransportOptions {
    AppTransportOptions {
      hostname: "127.0.0.1".to_string(),
      port: 0,
      rpc_path: "/rpc".to_string(),
    }
  }

  #[tokio::test]
  async fn close_is_idempotent_and_releases_the_port() {
    let (requests, _request_rx) = mpsc::channel(4);
    let (websocket_events, _websocket_rx) = mpsc::channel(4);
    let resource = open_app_server(options(), requests, websocket_events)
      .await
      .expect("application listener binds");
    let address = resource.local_addr();

    let (_first, _second) = tokio::join!(resource.close(), resource.close());
    resource.await_shutdown().await;

    let (requests, _request_rx) = mpsc::channel(4);
    let (websocket_events, _websocket_rx) = mpsc::channel(4);
    let rebound = HttpServer::bind(
      address,
      requests,
      websocket_events,
      WebSocketServerOptions::default(),
    )
    .await
    .expect("released port can be rebound");
    rebound.close().await;
  }

  #[tokio::test]
  async fn invalid_options_fail_before_acquisition() {
    let (requests, _request_rx) = mpsc::channel(1);
    let (websocket_events, _websocket_rx) = mpsc::channel(1);
    let mut invalid = options();
    invalid.rpc_path = "/rpc?query".to_string();
    assert!(matches!(
      open_app_server(invalid, requests, websocket_events).await,
      Err(AppTransportError::InvalidOptions(_))
    ));
  }
}
