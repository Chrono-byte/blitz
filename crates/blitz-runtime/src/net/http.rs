// SPDX-License-Identifier: MPL-2.0
//! Bounded HTTP/1.1 server transport for the Blitz runtime.

use std::convert::Infallible;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Semaphore, mpsc, oneshot, watch};
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::{Duration, timeout};

use super::websocket::{WebSocketEvent, WebSocketServerOptions, run_upgrade};

/// Default maximum buffered request body: 1 MiB.
pub const DEFAULT_MAX_REQUEST_BODY: usize = 1024 * 1024;
/// Default maximum number of concurrently active HTTP connections.
pub const DEFAULT_MAX_CONNECTIONS: usize = 256;
/// Maximum time spent receiving one request body.
pub const DEFAULT_REQUEST_BODY_TIMEOUT: Duration = Duration::from_secs(30);
/// Maximum time a runtime handler may retain a request.
pub const DEFAULT_HANDLER_TIMEOUT: Duration = Duration::from_secs(30);

/// An HTTP request transferred to the VM thread.
#[derive(Debug)]
pub struct HttpRequestEvent {
  /// HTTP method.
  pub method: String,
  /// Request path, excluding the query string.
  pub path: String,
  /// Absolute request URL.
  pub url: String,
  /// Raw query string, when present.
  pub query: Option<String>,
  /// Fully buffered request body.
  pub body: Vec<u8>,
  /// Request headers as lowercase names and UTF-8 values.
  pub headers: Vec<(String, String)>,
  /// Remote TCP peer address.
  pub remote_addr: SocketAddr,
  upgrade: Option<PendingWebSocketUpgrade>,
  response: oneshot::Sender<HttpCompletion>,
}

impl HttpRequestEvent {
  /// Complete this request with a runtime-produced response.
  pub fn respond(
    self,
    response: HttpResponse,
  ) {
    let _result = self
      .response
      .send(HttpCompletion::Response(response));
  }

  /// Return the connection identifier when this request can be upgraded.
  #[must_use]
  pub fn websocket_id(&self) -> Option<u64> {
    self
      .upgrade
      .as_ref()
      .map(|upgrade| upgrade.connection_id)
  }

  /// Complete this request with its WebSocket switching-protocols response.
  pub fn respond_websocket(mut self) {
    let completion = self.upgrade.take().map_or_else(
      || HttpCompletion::Response(HttpResponse::text(400, "request cannot be upgraded")),
      HttpCompletion::WebSocket,
    );
    let _result = self.response.send(completion);
  }
}

#[derive(Debug)]
struct PendingWebSocketUpgrade {
  connection_id: u64,
  response: Response<http_body_util::Empty<Bytes>>,
  upgrade: fastwebsockets::upgrade::UpgradeFut,
}

#[derive(Debug)]
enum HttpCompletion {
  Response(HttpResponse),
  WebSocket(PendingWebSocketUpgrade),
}

/// Owned HTTP response produced by the VM thread.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HttpResponse {
  /// Numeric HTTP status.
  pub status: u16,
  /// Response headers.
  pub headers: Vec<(String, String)>,
  /// Response body bytes.
  pub body: Vec<u8>,
}

impl HttpResponse {
  /// Construct a UTF-8 plain-text response.
  #[must_use]
  pub fn text(
    status: u16,
    body: impl Into<String>,
  ) -> Self {
    Self {
      status,
      headers: vec![(
        "content-type".to_string(),
        "text/plain; charset=utf-8".to_string(),
      )],
      body: body.into().into_bytes(),
    }
  }
}

/// A listening HTTP server and shutdown handle.
pub struct HttpServer {
  local_addr: SocketAddr,
  shutdown: watch::Sender<bool>,
  task: JoinHandle<()>,
}

#[derive(Clone)]
struct ServerContext {
  local_addr: SocketAddr,
  requests: mpsc::Sender<HttpRequestEvent>,
  websocket_events: mpsc::Sender<WebSocketEvent>,
  websocket_options: WebSocketServerOptions,
  websocket_shutdown: watch::Receiver<bool>,
  next_websocket_id: Arc<AtomicU64>,
}

impl HttpServer {
  /// Bind and begin serving HTTP/1.1 requests.
  ///
  /// # Errors
  ///
  /// Returns an I/O error when the address cannot be bound.
  pub async fn bind(
    address: SocketAddr,
    requests: mpsc::Sender<HttpRequestEvent>,
    websocket_events: mpsc::Sender<WebSocketEvent>,
    websocket_options: WebSocketServerOptions,
  ) -> io::Result<Self> {
    let listener = TcpListener::bind(address).await?;
    let local_addr = listener.local_addr()?;
    let (shutdown, shutdown_rx) = watch::channel(false);
    let task = tokio::spawn(accept_loop(
      listener,
      requests,
      websocket_events,
      websocket_options,
      local_addr,
      shutdown_rx,
    ));
    Ok(Self {
      local_addr,
      shutdown,
      task,
    })
  }

  /// Return the actual bound address.
  #[must_use]
  pub const fn local_addr(&self) -> SocketAddr {
    self.local_addr
  }

  /// Stop the listener and active connections.
  pub async fn close(self) {
    let _result = self.shutdown.send(true);
    let _result = self.task.await;
  }
}

async fn accept_loop(
  listener: TcpListener,
  requests: mpsc::Sender<HttpRequestEvent>,
  websocket_events: mpsc::Sender<WebSocketEvent>,
  websocket_options: WebSocketServerOptions,
  local_addr: SocketAddr,
  mut shutdown: watch::Receiver<bool>,
) {
  let mut connections = JoinSet::new();
  let connection_limit = Arc::new(Semaphore::new(DEFAULT_MAX_CONNECTIONS));
  let context = ServerContext {
    local_addr,
    requests,
    websocket_events,
    websocket_options,
    websocket_shutdown: shutdown.clone(),
    next_websocket_id: Arc::new(AtomicU64::new(1)),
  };
  loop {
    tokio::select! {
      changed = shutdown.changed() => {
        if changed.is_err() || *shutdown.borrow() {
          break;
        }
      }
      accepted = listener.accept() => match accepted {
        Ok((stream, peer)) => {
          let Ok(permit) = Arc::clone(&connection_limit).try_acquire_owned() else {
            drop(stream);
            continue;
          };
          connections.spawn(serve_connection(
            stream,
            peer,
            context.clone(),
            permit,
          ));
        }
        Err(_) => break,
      },
      Some(_finished) = connections.join_next(), if !connections.is_empty() => {}
    }
  }
  connections.abort_all();
  while connections.join_next().await.is_some() {}
}

async fn serve_connection(
  stream: TcpStream,
  remote_addr: SocketAddr,
  context: ServerContext,
  _permit: tokio::sync::OwnedSemaphorePermit,
) {
  let service = service_fn(move |request| handle_request(request, remote_addr, context.clone()));
  let mut builder = hyper::server::conn::http1::Builder::new();
  builder.max_headers(100).max_buf_size(64 * 1024);
  let connection = builder
    .serve_connection(TokioIo::new(stream), service)
    .with_upgrades();
  let _result = connection.await;
}

async fn handle_request(
  request: Request<Incoming>,
  remote_addr: SocketAddr,
  context: ServerContext,
) -> Result<Response<Full<Bytes>>, Infallible> {
  let mut request = request;
  let upgrade = if fastwebsockets::upgrade::is_upgrade_request(&request) {
    fastwebsockets::upgrade::upgrade(&mut request)
      .ok()
      .map(|(response, upgrade)| PendingWebSocketUpgrade {
        connection_id: context
          .next_websocket_id
          .fetch_add(1, Ordering::Relaxed),
        response,
        upgrade,
      })
  } else {
    None
  };
  let method = request.method().as_str().to_string();
  let path = request.uri().path().to_string();
  let query = request.uri().query().map(str::to_string);
  let authority = match trusted_authority(&request, context.local_addr) {
    Some(authority) => authority,
    None => return Ok(response(HttpResponse::text(400, "invalid Host header"))),
  };
  let query_suffix = query
    .as_ref()
    .map_or_else(String::new, |query| format!("?{query}"));
  let url = format!("http://{authority}{path}{query_suffix}");
  let headers = request
    .headers()
    .iter()
    .filter_map(|(name, value)| {
      value
        .to_str()
        .ok()
        .map(|value| (name.as_str().to_string(), value.to_string()))
    })
    .collect();
  let body = match timeout(
    DEFAULT_REQUEST_BODY_TIMEOUT,
    collect_body(request.into_body()),
  )
  .await
  {
    Ok(Ok(body)) => body,
    Ok(Err(error_response)) => return Ok(response(error_response)),
    Err(_) => return Ok(response(HttpResponse::text(408, "request body timed out"))),
  };

  let (response_tx, response_rx) = oneshot::channel();
  let event = HttpRequestEvent {
    method,
    path,
    url,
    query,
    body,
    headers,
    remote_addr,
    upgrade,
    response: response_tx,
  };
  let result = match timeout(DEFAULT_HANDLER_TIMEOUT, async {
    context
      .requests
      .send(event)
      .await
      .map_err(|_| HttpResponse::text(503, "runtime unavailable"))?;
    response_rx
      .await
      .map_err(|_| HttpResponse::text(500, "handler did not return a response"))
  })
  .await
  {
    Ok(Ok(result)) => result,
    Ok(Err(error)) => return Ok(response(error)),
    Err(_) => return Ok(response(HttpResponse::text(504, "handler timed out"))),
  };
  match result {
    HttpCompletion::Response(result) => Ok(response(result)),
    HttpCompletion::WebSocket(pending) => {
      tokio::spawn(run_upgrade(
        pending.connection_id,
        pending.upgrade,
        context.websocket_events,
        context.websocket_options,
        context.websocket_shutdown,
      ));
      let (parts, _body) = pending.response.into_parts();
      Ok(Response::from_parts(parts, Full::new(Bytes::new())))
    }
  }
}

fn trusted_authority(
  request: &Request<Incoming>,
  local_addr: SocketAddr,
) -> Option<String> {
  let authority = request
    .headers()
    .get(hyper::header::HOST)?
    .to_str()
    .ok()?
    .parse::<hyper::http::uri::Authority>()
    .ok()?;
  if authority
    .port_u16()
    .is_some_and(|port| port != local_addr.port())
  {
    return None;
  }
  let host = authority.host();
  let trusted = host
    .parse::<std::net::IpAddr>()
    .is_ok_and(|address| address == local_addr.ip() || address.is_loopback())
    || (local_addr.ip().is_loopback() && host.eq_ignore_ascii_case("localhost"));
  trusted.then(|| authority.to_string())
}

async fn collect_body(mut body: Incoming) -> Result<Vec<u8>, HttpResponse> {
  let mut collected = Vec::new();
  while let Some(frame) = body.frame().await {
    let frame = frame.map_err(|error| HttpResponse::text(400, error.to_string()))?;
    let Ok(data) = frame.into_data() else {
      continue;
    };
    let Some(size) = collected.len().checked_add(data.len()) else {
      return Err(HttpResponse::text(413, "request body too large"));
    };
    if size > DEFAULT_MAX_REQUEST_BODY {
      return Err(HttpResponse::text(413, "request body too large"));
    }
    collected.extend_from_slice(&data);
  }
  Ok(collected)
}

fn response(response: HttpResponse) -> Response<Full<Bytes>> {
  let status = StatusCode::from_u16(response.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
  let mut builder = Response::builder().status(status);
  for (name, value) in response.headers {
    builder = builder.header(name, value);
  }
  builder
    .body(Full::new(Bytes::from(response.body)))
    .unwrap_or_else(|_| Response::new(Full::new(Bytes::from_static(b"failed to build response"))))
}
