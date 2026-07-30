// SPDX-License-Identifier: MPL-2.0
//! Bounded, runtime-facing WebSocket server built on FastWebSockets.
//!
//! The server owns all socket tasks. It exchanges only owned byte buffers and
//! numeric connection identifiers with the runtime, so VM values never cross
//! an I/O task boundary.

use std::convert::Infallible;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicU64, Ordering};

use bytes::Bytes;
use fastwebsockets::{Frame, OpCode, Payload, WebSocketError};
use http_body_util::Empty;
use hyper::body::Incoming;
use hyper::header::{CONNECTION, UPGRADE};
use hyper::rt::Executor;
use hyper::service::service_fn;
use hyper::upgrade::Upgraded;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Notify, Semaphore, mpsc, watch};
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::{Duration, timeout};

use std::future::Future;

/// Default maximum WebSocket message size: 1 MiB.
pub const DEFAULT_MAX_MESSAGE_SIZE: usize = 1024 * 1024;
/// Default capacity of each connection's outbound command queue.
pub const DEFAULT_COMMAND_CAPACITY: usize = 64;
/// Default maximum time a WebSocket may remain idle.
pub const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(120);
/// Default maximum number of concurrently active WebSocket handshakes.
pub const DEFAULT_MAX_CONNECTIONS: usize = 256;

struct SpawnExecutor;

impl<Fut> Executor<Fut> for SpawnExecutor
where
  Fut: Future + Send + 'static,
  Fut::Output: Send + 'static,
{
  fn execute(
    &self,
    future: Fut,
  ) {
    tokio::spawn(future);
  }
}

/// Configuration for a WebSocket server.
#[derive(Clone, Debug)]
pub struct WebSocketServerOptions {
  /// Maximum accepted payload size, in bytes.
  pub max_message_size: usize,
  /// Number of pending outbound commands allowed per connection.
  pub command_capacity: usize,
}

impl Default for WebSocketServerOptions {
  fn default() -> Self {
    Self {
      max_message_size: DEFAULT_MAX_MESSAGE_SIZE,
      command_capacity: DEFAULT_COMMAND_CAPACITY,
    }
  }
}

/// A command sent from the runtime to one WebSocket connection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WebSocketCommand {
  /// Send a UTF-8 text message.
  Text(String),
  /// Send a binary message.
  Binary(Vec<u8>),
  /// Start a WebSocket close handshake.
  Close {
    /// RFC 6455 close code.
    code: u16,
    /// UTF-8 close reason.
    reason: String,
  },
}

/// An event emitted from socket tasks to the runtime thread.
#[derive(Debug)]
pub enum WebSocketEvent {
  /// A connection completed its opening handshake.
  Open {
    /// Runtime-unique connection identifier.
    connection_id: u64,
    /// Bounded command channel for this connection.
    commands: mpsc::Sender<WebSocketCommand>,
  },
  /// A complete text message was received.
  Text {
    /// Connection that received the message.
    connection_id: u64,
    /// Validated UTF-8 payload.
    text: String,
  },
  /// A complete binary message was received.
  Binary {
    /// Connection that received the message.
    connection_id: u64,
    /// Owned message bytes.
    data: Vec<u8>,
  },
  /// Bytes from the runtime command queue were written to the socket.
  Drain {
    /// Connection whose outbound queue drained.
    connection_id: u64,
    /// Number of payload bytes written.
    bytes: usize,
  },
  /// A connection ended.
  Close {
    /// Connection that ended.
    connection_id: u64,
    /// Peer close code, when one was supplied.
    code: Option<u16>,
    /// Peer close reason, when valid UTF-8 was supplied.
    reason: String,
  },
  /// A connection failed.
  Error {
    /// Connection that failed.
    connection_id: u64,
    /// Stable, displayable error detail.
    message: String,
  },
}

/// A listening WebSocket server and its graceful-shutdown handle.
pub struct WebSocketServer {
  local_addr: SocketAddr,
  shutdown: watch::Sender<bool>,
  task: JoinHandle<()>,
}

impl WebSocketServer {
  /// Bind a WebSocket server and begin accepting connections.
  ///
  /// # Errors
  ///
  /// Returns an I/O error when the address cannot be bound.
  pub async fn bind(
    address: SocketAddr,
    events: mpsc::Sender<WebSocketEvent>,
    options: WebSocketServerOptions,
  ) -> io::Result<Self> {
    if options.max_message_size == 0 || options.command_capacity == 0 {
      return Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        "WebSocket limits must be greater than zero",
      ));
    }
    let listener = TcpListener::bind(address).await?;
    let local_addr = listener.local_addr()?;
    let (shutdown, shutdown_rx) = watch::channel(false);
    let task = tokio::spawn(accept_loop(listener, events, options, shutdown_rx));
    Ok(Self {
      local_addr,
      shutdown,
      task,
    })
  }

  /// Return the actual bound address, including an OS-selected port.
  #[must_use]
  pub const fn local_addr(&self) -> SocketAddr {
    self.local_addr
  }

  /// Stop accepting, close active connections, and wait for all tasks.
  pub async fn close(self) {
    let _result = self.shutdown.send(true);
    let _result = self.task.await;
  }
}

async fn accept_loop(
  listener: TcpListener,
  events: mpsc::Sender<WebSocketEvent>,
  options: WebSocketServerOptions,
  mut shutdown: watch::Receiver<bool>,
) {
  let next_id = Arc::new(AtomicU64::new(1));
  let active = Arc::new(ActiveConnections::default());
  let connection_limit = Arc::new(Semaphore::new(DEFAULT_MAX_CONNECTIONS));
  let mut connections = JoinSet::new();

  loop {
    tokio::select! {
      changed = shutdown.changed() => {
        if changed.is_err() || *shutdown.borrow() {
          break;
        }
      }
      accepted = listener.accept() => {
        match accepted {
          Ok((stream, _peer)) => {
            let Ok(permit) = Arc::clone(&connection_limit).try_acquire_owned() else {
              drop(stream);
              continue;
            };
            let events = events.clone();
            let options = options.clone();
            let shutdown = shutdown.clone();
            let next_id = Arc::clone(&next_id);
            let active = Arc::clone(&active);
            connections.spawn(async move {
              let _permit = permit;
              serve_http_connection(stream, events, options, shutdown, next_id, active).await;
            });
          }
          Err(_) => break,
        }
      }
      Some(_finished) = connections.join_next(), if !connections.is_empty() => {}
    }
  }

  connections.abort_all();
  while connections.join_next().await.is_some() {}
  active.wait_until_empty().await;
}

async fn serve_http_connection(
  stream: TcpStream,
  events: mpsc::Sender<WebSocketEvent>,
  options: WebSocketServerOptions,
  shutdown: watch::Receiver<bool>,
  next_id: Arc<AtomicU64>,
  active: Arc<ActiveConnections>,
) {
  let service = service_fn(move |mut request: Request<Incoming>| {
    let events = events.clone();
    let options = options.clone();
    let shutdown = shutdown.clone();
    let next_id = Arc::clone(&next_id);
    let active = Arc::clone(&active);
    async move {
      match fastwebsockets::upgrade::upgrade(&mut request) {
        Ok((response, upgrade)) => {
          active.start();
          tokio::spawn(async move {
            if let Ok(socket) = upgrade.await {
              let id = next_id.fetch_add(1, Ordering::Relaxed);
              run_connection(id, socket, events, options, shutdown).await;
            }
            active.finish();
          });
          Ok::<_, Infallible>(response)
        }
        Err(_error) => Ok(upgrade_required()),
      }
    }
  });

  let connection = hyper::server::conn::http1::Builder::new()
    .serve_connection(TokioIo::new(stream), service)
    .with_upgrades();
  let _result = connection.await;
}

#[derive(Default)]
struct ActiveConnections {
  count: AtomicUsize,
  empty: Notify,
}

impl ActiveConnections {
  fn start(&self) {
    self.count.fetch_add(1, Ordering::AcqRel);
  }

  fn finish(&self) {
    if self.count.fetch_sub(1, Ordering::AcqRel) == 1 {
      self.empty.notify_waiters();
    }
  }

  async fn wait_until_empty(&self) {
    loop {
      let notified = self.empty.notified();
      if self.count.load(Ordering::Acquire) == 0 {
        break;
      }
      notified.await;
    }
  }
}

fn upgrade_required() -> Response<Empty<Bytes>> {
  Response::builder()
    .status(StatusCode::UPGRADE_REQUIRED)
    .body(Empty::new())
    .unwrap_or_else(|_| Response::new(Empty::new()))
}

async fn run_connection<S>(
  connection_id: u64,
  socket: fastwebsockets::WebSocket<S>,
  events: mpsc::Sender<WebSocketEvent>,
  options: WebSocketServerOptions,
  mut shutdown: watch::Receiver<bool>,
) where
  S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
  let (commands_tx, mut commands_rx) = mpsc::channel(options.command_capacity);
  if events
    .send(WebSocketEvent::Open {
      connection_id,
      commands: commands_tx,
    })
    .await
    .is_err()
  {
    return;
  }

  let mut socket = socket;
  socket.set_max_message_size(options.max_message_size);
  let mut fragments = MessageFragments::default();
  let mut close_emitted = false;
  loop {
    tokio::select! {
      changed = shutdown.changed() => {
        if changed.is_err() || *shutdown.borrow() {
          let _result = socket.write_frame(close_frame(1001, "server shutdown")).await;
          emit_close(&events, connection_id, Some(1001), "server shutdown").await;
          close_emitted = true;
          break;
        }
      }
      command = commands_rx.recv() => {
        let Some(command) = command else {
          break;
        };
        let sent_bytes = match &command {
          WebSocketCommand::Text(text) => text.len(),
          WebSocketCommand::Binary(data) => data.len(),
          WebSocketCommand::Close { .. } => 0,
        };
        let close = match &command {
          WebSocketCommand::Close { code, reason } => Some((*code, reason.clone())),
          _ => None,
        };
        if let Err(error) = write_command(&mut socket, command).await {
          emit_error(&events, connection_id, &error).await;
          break;
        }
        if sent_bytes > 0 {
          let _result = events
            .send(WebSocketEvent::Drain {
              connection_id,
              bytes: sent_bytes,
            })
            .await;
        }
        if let Some((code, reason)) = close {
          emit_close(&events, connection_id, Some(code), &reason).await;
          close_emitted = true;
          break;
        }
      }
      frame = timeout(DEFAULT_IDLE_TIMEOUT, socket.read_frame()) => {
        match frame {
          Ok(Ok(frame)) => {
            if handle_frame(
              &events,
              connection_id,
              frame,
              options.max_message_size,
              &mut fragments,
              &mut close_emitted,
            ).await {
              break;
            }
          }
          Ok(Err(error)) => {
            emit_error(&events, connection_id, &error).await;
            break;
          }
          Err(_) => {
            let _result = socket.write_frame(close_frame(1001, "idle timeout")).await;
            emit_close(&events, connection_id, Some(1001), "idle timeout").await;
            close_emitted = true;
            break;
          }
        }
      }
    }
  }
  if !close_emitted {
    emit_close(&events, connection_id, None, "").await;
  }
}

/// Complete an HTTP upgrade and run the resulting WebSocket connection.
pub(crate) async fn run_upgrade(
  connection_id: u64,
  upgrade: fastwebsockets::upgrade::UpgradeFut,
  events: mpsc::Sender<WebSocketEvent>,
  options: WebSocketServerOptions,
  shutdown: watch::Receiver<bool>,
) {
  if let Ok(socket) = upgrade.await {
    run_connection(connection_id, socket, events, options, shutdown).await;
  }
}

/// Connect a client WebSocket and feed it into the shared runtime event path.
pub(crate) async fn connect_client(
  connection_id: u64,
  url: String,
  events: mpsc::Sender<WebSocketEvent>,
  options: WebSocketServerOptions,
  shutdown: watch::Receiver<bool>,
) {
  let result = async {
    let (authority, path) = parse_client_url(&url)?;
    let stream = TcpStream::connect(&authority)
      .await
      .map_err(|error| error.to_string())?;
    let request = Request::builder()
      .method("GET")
      .uri(format!("http://{authority}{path}"))
      .header("Host", &authority)
      .header(UPGRADE, "websocket")
      .header(CONNECTION, "upgrade")
      .header(
        "Sec-WebSocket-Key",
        fastwebsockets::handshake::generate_key(),
      )
      .header("Sec-WebSocket-Version", "13")
      .body(Empty::<Bytes>::new())
      .map_err(|error| error.to_string())?;
    let (socket, response): (fastwebsockets::WebSocket<TokioIo<Upgraded>>, _) =
      fastwebsockets::handshake::client(&SpawnExecutor, request, stream)
        .await
        .map_err(|error| error.to_string())?;
    if response.status() != StatusCode::SWITCHING_PROTOCOLS {
      return Err(format!("WebSocket server returned {}", response.status()));
    }
    run_connection(connection_id, socket, events.clone(), options, shutdown).await;
    Ok::<(), String>(())
  }
  .await;
  if let Err(message) = result {
    let _result = events
      .send(WebSocketEvent::Error {
        connection_id,
        message,
      })
      .await;
    emit_close(&events, connection_id, None, "").await;
  }
}

fn parse_client_url(url: &str) -> Result<(String, String), String> {
  let remainder = url
    .strip_prefix("ws://")
    .ok_or_else(|| "only ws:// WebSocket URLs are supported".to_string())?;
  let (authority, path) = remainder.split_once('/').map_or_else(
    || (remainder, "/".to_string()),
    |(authority, path)| (authority, format!("/{path}")),
  );
  if authority.is_empty() {
    return Err("WebSocket URL requires a host".to_string());
  }
  let authority = if authority.contains(':') {
    authority.to_string()
  } else {
    format!("{authority}:80")
  };
  Ok((authority, path))
}

async fn write_command<S>(
  socket: &mut fastwebsockets::WebSocket<S>,
  command: WebSocketCommand,
) -> Result<(), WebSocketError>
where
  S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
  let frame = match command {
    WebSocketCommand::Text(text) => Frame::text(Payload::Owned(text.into_bytes())),
    WebSocketCommand::Binary(data) => Frame::binary(Payload::Owned(data)),
    WebSocketCommand::Close { code, reason } => close_frame(code, &reason),
  };
  socket.write_frame(frame).await
}

async fn handle_frame(
  events: &mpsc::Sender<WebSocketEvent>,
  connection_id: u64,
  frame: Frame<'_>,
  max_message_size: usize,
  fragments: &mut MessageFragments,
  close_emitted: &mut bool,
) -> bool {
  let event = match fragments.push(frame, max_message_size) {
    Ok(Some(CompletedMessage::Text(text))) => WebSocketEvent::Text {
      connection_id,
      text,
    },
    Ok(Some(CompletedMessage::Binary(data))) => WebSocketEvent::Binary {
      connection_id,
      data,
    },
    Ok(Some(CompletedMessage::Close { code, reason })) => {
      emit_close(events, connection_id, code, &reason).await;
      *close_emitted = true;
      return true;
    }
    Ok(None) => return false,
    Err(message) => WebSocketEvent::Error {
      connection_id,
      message,
    },
  };

  let should_close = matches!(event, WebSocketEvent::Error { .. });
  events.send(event).await.is_err() || should_close
}

#[derive(Default)]
struct MessageFragments {
  current: Option<(OpCode, Vec<u8>)>,
}

enum CompletedMessage {
  Text(String),
  Binary(Vec<u8>),
  Close { code: Option<u16>, reason: String },
}

impl MessageFragments {
  fn push(
    &mut self,
    frame: Frame<'_>,
    max_message_size: usize,
  ) -> Result<Option<CompletedMessage>, String> {
    match frame.opcode {
      OpCode::Text | OpCode::Binary if frame.fin => {
        if self.current.is_some() {
          return Err("received a new message before fragmented message completed".to_string());
        }
        complete_message(frame.opcode, frame.payload.to_vec(), max_message_size).map(Some)
      }
      OpCode::Text | OpCode::Binary => {
        if self.current.is_some() {
          return Err("received overlapping fragmented messages".to_string());
        }
        ensure_message_size(frame.payload.len(), max_message_size)?;
        self.current = Some((frame.opcode, frame.payload.to_vec()));
        Ok(None)
      }
      OpCode::Continuation => {
        let Some((opcode, mut data)) = self.current.take() else {
          return Err("received a continuation without a fragmented message".to_string());
        };
        let size = data
          .len()
          .checked_add(frame.payload.len())
          .ok_or_else(|| "WebSocket message size overflow".to_string())?;
        ensure_message_size(size, max_message_size)?;
        data.extend_from_slice(&frame.payload);
        if frame.fin {
          complete_message(opcode, data, max_message_size).map(Some)
        } else {
          self.current = Some((opcode, data));
          Ok(None)
        }
      }
      OpCode::Close => {
        let (code, reason) = parse_close_payload(&frame.payload);
        Ok(Some(CompletedMessage::Close { code, reason }))
      }
      _ => Ok(None),
    }
  }
}

fn ensure_message_size(
  size: usize,
  max_message_size: usize,
) -> Result<(), String> {
  if size > max_message_size {
    Err(format!(
      "WebSocket message exceeds the {max_message_size}-byte limit"
    ))
  } else {
    Ok(())
  }
}

fn complete_message(
  opcode: OpCode,
  data: Vec<u8>,
  max_message_size: usize,
) -> Result<CompletedMessage, String> {
  ensure_message_size(data.len(), max_message_size)?;
  match opcode {
    OpCode::Text => String::from_utf8(data)
      .map(CompletedMessage::Text)
      .map_err(|error| error.to_string()),
    OpCode::Binary => Ok(CompletedMessage::Binary(data)),
    _ => Err("invalid fragmented message opcode".to_string()),
  }
}

fn parse_close_payload(payload: &[u8]) -> (Option<u16>, String) {
  if payload.len() < 2 {
    return (None, String::new());
  }
  let code = u16::from_be_bytes([payload[0], payload[1]]);
  let reason = String::from_utf8_lossy(&payload[2..]).into_owned();
  (Some(code), reason)
}

fn close_frame(
  code: u16,
  reason: &str,
) -> Frame<'static> {
  Frame::close(code, reason.as_bytes())
}

async fn emit_error(
  events: &mpsc::Sender<WebSocketEvent>,
  connection_id: u64,
  error: &WebSocketError,
) {
  let _result = events
    .send(WebSocketEvent::Error {
      connection_id,
      message: error.to_string(),
    })
    .await;
}

async fn emit_close(
  events: &mpsc::Sender<WebSocketEvent>,
  connection_id: u64,
  code: Option<u16>,
  reason: &str,
) {
  let _result = events
    .send(WebSocketEvent::Close {
      connection_id,
      code,
      reason: reason.to_string(),
    })
    .await;
}

#[cfg(test)]
mod tests {
  use std::future::Future;

  use hyper::header::{CONNECTION, UPGRADE};
  use hyper::rt::Executor;
  use hyper::upgrade::Upgraded;
  use tokio::time::{Duration, timeout};

  use super::*;

  struct SpawnExecutor;

  impl<Fut> Executor<Fut> for SpawnExecutor
  where
    Fut: Future + Send + 'static,
    Fut::Output: Send + 'static,
  {
    fn execute(
      &self,
      future: Fut,
    ) {
      tokio::spawn(future);
    }
  }

  type ClientSocket = fastwebsockets::WebSocket<TokioIo<Upgraded>>;

  async fn connect(address: SocketAddr) -> ClientSocket {
    let stream = TcpStream::connect(address)
      .await
      .expect("connect client");
    let request = Request::builder()
      .method("GET")
      .uri(format!("http://{address}/socket"))
      .header("Host", address.to_string())
      .header(UPGRADE, "websocket")
      .header(CONNECTION, "upgrade")
      .header(
        "Sec-WebSocket-Key",
        fastwebsockets::handshake::generate_key(),
      )
      .header("Sec-WebSocket-Version", "13")
      .body(Empty::<Bytes>::new())
      .expect("client request");
    let (socket, response) = fastwebsockets::handshake::client(&SpawnExecutor, request, stream)
      .await
      .expect("client handshake");
    assert_eq!(response.status(), StatusCode::SWITCHING_PROTOCOLS);
    socket
  }

  async fn next_event(events: &mut mpsc::Receiver<WebSocketEvent>) -> WebSocketEvent {
    timeout(Duration::from_secs(2), events.recv())
      .await
      .expect("event timeout")
      .expect("event channel closed")
  }

  #[tokio::test]
  async fn exchanges_text_binary_and_close_events() {
    let (events_tx, mut events_rx) = mpsc::channel(16);
    let server = WebSocketServer::bind(
      "127.0.0.1:0".parse().expect("loopback address"),
      events_tx,
      WebSocketServerOptions::default(),
    )
    .await
    .expect("bind server");
    let mut client = connect(server.local_addr()).await;

    let (connection_id, commands) = match next_event(&mut events_rx).await {
      WebSocketEvent::Open {
        connection_id,
        commands,
      } => (connection_id, commands),
      event => panic!("expected open, got {event:?}"),
    };

    client
      .write_frame(Frame::text(Payload::Owned(b"hello".to_vec())))
      .await
      .expect("write client text");
    match next_event(&mut events_rx).await {
      WebSocketEvent::Text {
        connection_id: actual_id,
        text,
      } => {
        assert_eq!(actual_id, connection_id);
        assert_eq!(text, "hello");
      }
      event => panic!("expected text, got {event:?}"),
    }

    commands
      .send(WebSocketCommand::Binary(vec![1, 2, 3]))
      .await
      .expect("queue server binary");
    let frame = timeout(Duration::from_secs(2), client.read_frame())
      .await
      .expect("frame timeout")
      .expect("read server frame");
    assert_eq!(frame.opcode, OpCode::Binary);
    assert_eq!(frame.payload.as_ref(), &[1, 2, 3]);
    match next_event(&mut events_rx).await {
      WebSocketEvent::Drain {
        connection_id: actual_id,
        bytes,
      } => {
        assert_eq!(actual_id, connection_id);
        assert_eq!(bytes, 3);
      }
      event => panic!("expected drain, got {event:?}"),
    }

    client
      .write_frame(Frame::close(1000, b"finished"))
      .await
      .expect("write client close");
    match next_event(&mut events_rx).await {
      WebSocketEvent::Close {
        connection_id: actual_id,
        code,
        reason,
      } => {
        assert_eq!(actual_id, connection_id);
        assert_eq!(code, Some(1000));
        assert_eq!(reason, "finished");
      }
      event => panic!("expected close, got {event:?}"),
    }

    server.close().await;
  }

  #[tokio::test]
  async fn rejects_messages_over_the_configured_limit() {
    let (events_tx, mut events_rx) = mpsc::channel(16);
    let server = WebSocketServer::bind(
      "127.0.0.1:0".parse().expect("loopback address"),
      events_tx,
      WebSocketServerOptions {
        max_message_size: 4,
        command_capacity: 4,
      },
    )
    .await
    .expect("bind server");
    let mut client = connect(server.local_addr()).await;
    let _commands = match next_event(&mut events_rx).await {
      WebSocketEvent::Open { commands, .. } => commands,
      event => panic!("expected open, got {event:?}"),
    };

    client
      .write_frame(Frame::text(Payload::Owned(b"oversized".to_vec())))
      .await
      .expect("write oversized frame");
    match next_event(&mut events_rx).await {
      WebSocketEvent::Error { message, .. } => {
        assert!(!message.is_empty());
      }
      event => panic!("expected size error, got {event:?}"),
    }

    server.close().await;
  }

  #[tokio::test]
  async fn graceful_shutdown_closes_active_connections() {
    let (events_tx, mut events_rx) = mpsc::channel(16);
    let server = WebSocketServer::bind(
      "127.0.0.1:0".parse().expect("loopback address"),
      events_tx,
      WebSocketServerOptions::default(),
    )
    .await
    .expect("bind server");
    let mut client = connect(server.local_addr()).await;
    let _commands = match next_event(&mut events_rx).await {
      WebSocketEvent::Open { commands, .. } => commands,
      event => panic!("expected open, got {event:?}"),
    };

    let close_task = tokio::spawn(server.close());
    let frame = timeout(Duration::from_secs(2), client.read_frame())
      .await
      .expect("shutdown frame timeout")
      .expect("read shutdown frame");
    assert_eq!(frame.opcode, OpCode::Close);
    assert_eq!(parse_close_payload(&frame.payload).0, Some(1001));
    timeout(Duration::from_secs(2), close_task)
      .await
      .expect("server shutdown timeout")
      .expect("server shutdown task");
  }

  #[tokio::test]
  async fn rejects_zero_sized_limits() {
    let (events_tx, _events_rx) = mpsc::channel(1);
    let result = WebSocketServer::bind(
      "127.0.0.1:0".parse().expect("loopback address"),
      events_tx,
      WebSocketServerOptions {
        max_message_size: 0,
        command_capacity: 1,
      },
    )
    .await;
    assert!(matches!(
      result,
      Err(error) if error.kind() == io::ErrorKind::InvalidInput
    ));
  }

  #[test]
  fn parses_empty_and_populated_close_payloads() {
    assert_eq!(parse_close_payload(&[]), (None, String::new()));
    assert_eq!(
      parse_close_payload(&[0x03, 0xe8, b'o', b'k']),
      (Some(1000), "ok".to_string())
    );
  }

  #[test]
  fn collects_fragmented_utf8_and_enforces_aggregate_limit() {
    let mut fragments = MessageFragments::default();
    let first = Frame::new(false, OpCode::Text, None, Payload::Owned(vec![0xe2]));
    assert!(
      fragments
        .push(first, 3)
        .expect("first fragment")
        .is_none()
    );
    let last = Frame::new(
      true,
      OpCode::Continuation,
      None,
      Payload::Owned(vec![0x82, 0xac]),
    );
    match fragments.push(last, 3).expect("last fragment") {
      Some(CompletedMessage::Text(text)) => assert_eq!(text, "€"),
      _ => panic!("expected completed text"),
    }

    let mut oversized = MessageFragments::default();
    let first = Frame::new(false, OpCode::Binary, None, Payload::Owned(vec![1, 2]));
    assert!(
      oversized
        .push(first, 3)
        .expect("first fragment")
        .is_none()
    );
    let last = Frame::new(true, OpCode::Continuation, None, Payload::Owned(vec![3, 4]));
    assert!(oversized.push(last, 3).is_err());
  }
}
