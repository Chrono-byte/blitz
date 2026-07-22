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
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::{JoinHandle, JoinSet};

use super::websocket::{WebSocketEvent, WebSocketServerOptions, run_upgrade};

/// Default maximum buffered request body: 1 MiB.
pub const DEFAULT_MAX_REQUEST_BODY: usize = 1024 * 1024;

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
  mut shutdown: watch::Receiver<bool>,
) {
  let mut connections = JoinSet::new();
  let next_websocket_id = Arc::new(AtomicU64::new(1));
  loop {
    tokio::select! {
      changed = shutdown.changed() => {
        if changed.is_err() || *shutdown.borrow() {
          break;
        }
      }
      accepted = listener.accept() => match accepted {
        Ok((stream, peer)) => {
          let requests = requests.clone();
          let websocket_events = websocket_events.clone();
          let websocket_options = websocket_options.clone();
          let websocket_shutdown = shutdown.clone();
          let next_websocket_id = Arc::clone(&next_websocket_id);
          connections.spawn(serve_connection(
            stream,
            peer,
            requests,
            websocket_events,
            websocket_options,
            websocket_shutdown,
            next_websocket_id,
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
  requests: mpsc::Sender<HttpRequestEvent>,
  websocket_events: mpsc::Sender<WebSocketEvent>,
  websocket_options: WebSocketServerOptions,
  websocket_shutdown: watch::Receiver<bool>,
  next_websocket_id: Arc<AtomicU64>,
) {
  let service = service_fn(move |request| {
    handle_request(
      request,
      remote_addr,
      requests.clone(),
      websocket_events.clone(),
      websocket_options.clone(),
      websocket_shutdown.clone(),
      Arc::clone(&next_websocket_id),
    )
  });
  let connection = hyper::server::conn::http1::Builder::new()
    .serve_connection(TokioIo::new(stream), service)
    .with_upgrades();
  let _result = connection.await;
}

async fn handle_request(
  request: Request<Incoming>,
  remote_addr: SocketAddr,
  requests: mpsc::Sender<HttpRequestEvent>,
  websocket_events: mpsc::Sender<WebSocketEvent>,
  websocket_options: WebSocketServerOptions,
  websocket_shutdown: watch::Receiver<bool>,
  next_websocket_id: Arc<AtomicU64>,
) -> Result<Response<Full<Bytes>>, Infallible> {
  let mut request = request;
  let upgrade = if fastwebsockets::upgrade::is_upgrade_request(&request) {
    fastwebsockets::upgrade::upgrade(&mut request)
      .ok()
      .map(|(response, upgrade)| PendingWebSocketUpgrade {
        connection_id: next_websocket_id.fetch_add(1, Ordering::Relaxed),
        response,
        upgrade,
      })
  } else {
    None
  };
  let method = request.method().as_str().to_string();
  let path = request.uri().path().to_string();
  let query = request.uri().query().map(str::to_string);
  let authority = request
    .headers()
    .get(hyper::header::HOST)
    .and_then(|value| value.to_str().ok())
    .unwrap_or("localhost");
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
  let body = match collect_body(request.into_body()).await {
    Ok(body) => body,
    Err(error_response) => return Ok(response(error_response)),
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
  if requests.send(event).await.is_err() {
    return Ok(response(HttpResponse::text(503, "runtime unavailable")));
  }
  let result = response_rx.await.unwrap_or_else(|_| {
    HttpCompletion::Response(HttpResponse::text(500, "handler did not return a response"))
  });
  match result {
    HttpCompletion::Response(result) => Ok(response(result)),
    HttpCompletion::WebSocket(pending) => {
      tokio::spawn(run_upgrade(
        pending.connection_id,
        pending.upgrade,
        websocket_events,
        websocket_options,
        websocket_shutdown,
      ));
      let (parts, _body) = pending.response.into_parts();
      Ok(Response::from_parts(parts, Full::new(Bytes::new())))
    }
  }
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
