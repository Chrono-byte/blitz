//! Operating-system host APIs for the Blitz runtime.
//!
//! The ECMAScript engine is intentionally host-agnostic. This module is the
//! boundary where the Blitz executable installs capabilities supplied by the
//! operating system, such as process metadata, filesystem access, and async
//! I/O. Keeping that boundary explicit lets embedders select a smaller or
//! sandboxed host surface without changing VM semantics.

mod console;
mod process;
mod server;
mod terminal;

use std::collections::BTreeMap;
use std::env;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use blitz::Vm;
use blitz::errors::RuntimeError;
use tokio::sync::mpsc;

use crate::net::http::HttpRequestEvent;
use crate::net::websocket::WebSocketEvent;
use terminal::TerminalEvent;

/// Host configuration installed into a Blitz VM.
#[derive(Clone, Debug)]
pub struct RuntimeOptions {
  /// Process arguments exposed to JavaScript.
  pub argv: Vec<String>,
  /// Environment variables exposed to JavaScript.
  pub env: BTreeMap<String, String>,
  /// Path of the running Blitz executable.
  pub exec_path: String,
  /// Address used by the first `Blitz.serve` call.
  pub listen_addr: SocketAddr,
}

impl RuntimeOptions {
  /// Capture host process state with the supplied JavaScript argument vector.
  pub fn from_host(argv: Vec<String>) -> Self {
    let env: BTreeMap<String, String> = env::vars_os()
      .map(|(key, value)| {
        (
          key.to_string_lossy().into_owned(),
          value.to_string_lossy().into_owned(),
        )
      })
      .collect();
    let exec_path = env::current_exe().map_or_else(
      |_| argv.first().cloned().unwrap_or_default(),
      |path| path.to_string_lossy().into_owned(),
    );
    let host = env
      .get("BLITZ_HOST")
      .and_then(|value| value.parse::<IpAddr>().ok())
      .unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST));
    let port = env
      .get("BLITZ_PORT")
      .and_then(|value| value.parse::<u16>().ok())
      .unwrap_or(8000);

    Self {
      argv,
      env,
      exec_path,
      listen_addr: SocketAddr::new(host, port),
    }
  }
}

/// VM-thread driver for runtime-owned asynchronous resources.
pub struct RuntimeHandle {
  server: server::ServerState,
  requests: Option<mpsc::Receiver<HttpRequestEvent>>,
  websocket_events: Option<mpsc::Receiver<WebSocketEvent>>,
  terminal: terminal::TerminalState,
  terminal_events: Option<mpsc::Receiver<TerminalEvent>>,
}

enum RuntimeEvent {
  Http(Box<HttpRequestEvent>),
  WebSocket(WebSocketEvent),
  Terminal(TerminalEvent),
}

impl RuntimeHandle {
  /// Process one HTTP request, blocking until one arrives.
  ///
  /// # Errors
  ///
  /// Returns a runtime error when a route callback throws.
  pub fn run_once(
    &mut self,
    vm: &mut Vm,
  ) -> Result<bool, RuntimeError> {
    if !self.server.has_resources() && !self.terminal.is_active() {
      return Ok(false);
    }
    let Some(receiver) = self.requests.take() else {
      return Ok(false);
    };
    let Some(websocket_events) = self.websocket_events.take() else {
      self.requests = Some(receiver);
      return Ok(false);
    };
    let Some(terminal_events) = self.terminal_events.take() else {
      self.requests = Some(receiver);
      self.websocket_events = Some(websocket_events);
      return Ok(false);
    };
    let (receiver, websocket_events, terminal_events, event) = vm.block_on(async move {
      let mut receiver = receiver;
      let mut websocket_events = websocket_events;
      let mut terminal_events = terminal_events;
      let event = tokio::select! {
        event = receiver.recv() => event.map(Box::new).map(RuntimeEvent::Http),
        event = websocket_events.recv() => event.map(RuntimeEvent::WebSocket),
        event = terminal_events.recv() => event.map(RuntimeEvent::Terminal),
      };
      (receiver, websocket_events, terminal_events, event)
    })?;
    self.requests = Some(receiver);
    self.websocket_events = Some(websocket_events);
    self.terminal_events = Some(terminal_events);
    let Some(event) = event else {
      return Ok(false);
    };
    match event {
      RuntimeEvent::Http(event) => self.server.dispatch(vm, *event)?,
      RuntimeEvent::WebSocket(event) => self.server.dispatch_websocket(vm, event)?,
      RuntimeEvent::Terminal(event) => self.terminal.dispatch(vm, event)?,
    }
    vm.flush_microtasks()?;
    Ok(true)
  }

  /// Drive HTTP resources until the server is stopped or its channel closes.
  ///
  /// # Errors
  ///
  /// Returns a runtime error when a route callback throws.
  pub fn run(
    &mut self,
    vm: &mut Vm,
  ) -> Result<(), RuntimeError> {
    while self.run_once(vm)? {}
    Ok(())
  }

  /// Return the bound HTTP address, when `Blitz.serve` has started a server.
  #[must_use]
  pub fn local_addr(&self) -> Option<SocketAddr> {
    self.server.local_addr()
  }

  /// Stop the HTTP server and release its listener.
  ///
  /// # Errors
  ///
  /// Returns a runtime error if the background shutdown task fails.
  pub fn shutdown(
    &mut self,
    vm: &Vm,
  ) -> Result<(), RuntimeError> {
    self.server.shutdown_websockets();
    if let Some(server) = self.server.take_server() {
      vm.block_on(server.close())?;
    }
    Ok(())
  }
}

/// Install the default Blitz runtime APIs into an initialized VM.
pub fn install_runtime(
  vm: &mut Vm,
  options: RuntimeOptions,
) -> RuntimeHandle {
  let (requests_tx, requests_rx) = mpsc::channel(64);
  let (websocket_events_tx, websocket_events_rx) = mpsc::channel(64);
  let (terminal_events_tx, terminal_events_rx) = mpsc::channel(64);
  let server = server::install(vm, options.listen_addr, requests_tx, websocket_events_tx);
  let terminal = terminal::install(vm, terminal_events_tx);
  console::install(vm);
  process::install(vm, options);
  RuntimeHandle {
    server,
    requests: Some(requests_rx),
    websocket_events: Some(websocket_events_rx),
    terminal,
    terminal_events: Some(terminal_events_rx),
  }
}

#[cfg(test)]
mod tests {
  use std::cell::RefCell;
  use std::future::Future;
  use std::io::{Read, Write};
  use std::net::TcpStream;
  use std::rc::Rc;
  use std::thread;

  use blitz::compiler;
  use blitz::parser::Parser as JsParser;
  use blitz::value::{ObjectData, Value};
  use bytes::Bytes;
  use fastwebsockets::{Frame, OpCode, Payload};
  use http_body_util::Empty;
  use hyper::Request;
  use hyper::header::{CONNECTION, UPGRADE};
  use hyper::rt::Executor;
  use hyper::upgrade::Upgraded;
  use hyper_util::rt::TokioIo;

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

  #[test]
  fn blitz_serve_dispatches_deno_style_callback_over_http() {
    let globals = Rc::new(RefCell::new(ObjectData::new()));
    let mut vm = Vm::new(globals);
    let mut runtime = install_runtime(
      &mut vm,
      RuntimeOptions {
        argv: vec!["blitz".to_string()],
        env: BTreeMap::new(),
        exec_path: "blitz".to_string(),
        listen_addr: "127.0.0.1:0"
          .parse()
          .expect("test listen address"),
      },
    );
    let source = r#"
      async function callbackFunc(request, info) {
        const body = await request.text();
        const parsed = await new Response('{"value":"json"}').json();
        const headers = new Headers({ "x-blitz-response": "present" });
        headers.append("x-blitz-response", "appended");
        return new Response(
          request.method + " " + request.url + " " +
            request.headers.get("x-blitz-test") + " " + info.remoteAddr.transport + " " +
            body + " " + parsed.value,
          { status: 201, headers: headers }
        );
      }
      Blitz.serve({ hostname: "127.0.0.1", port: 0 }, callbackFunc);
    "#;
    let session = JsParser::parse_source(source).expect("parse server script");
    let chunk = compiler::compile_oxc(session.program(), source).expect("compile server script");
    vm.run(chunk).expect("evaluate server script");

    let address = runtime.local_addr().expect("bound HTTP address");
    let client = thread::spawn(move || {
      let mut stream = TcpStream::connect(address).expect("connect HTTP client");
      stream
        .write_all(
          b"POST /hello?from=test HTTP/1.1\r\nHost: localhost\r\nX-Blitz-Test: present\r\nContent-Length: 7\r\nConnection: close\r\n\r\npayload",
        )
        .expect("write HTTP request");
      let mut response = String::new();
      stream
        .read_to_string(&mut response)
        .expect("read HTTP response");
      response
    });

    assert!(
      runtime
        .run_once(&mut vm)
        .expect("dispatch HTTP request")
    );
    let response = client.join().expect("HTTP client thread");
    assert!(response.starts_with("HTTP/1.1 201 Created"), "{response}");
    assert!(
      response
        .to_ascii_lowercase()
        .contains("x-blitz-response: present, appended"),
      "{response}"
    );
    assert!(
      response.ends_with("POST http://localhost/hello?from=test present tcp payload json"),
      "{response}"
    );
    runtime.shutdown(&vm).expect("stop HTTP server");
  }

  #[test]
  fn blitz_upgrade_websocket_exchanges_events_and_messages() {
    let globals = Rc::new(RefCell::new(ObjectData::new()));
    let mut vm = Vm::new(globals);
    let mut runtime = install_runtime(
      &mut vm,
      RuntimeOptions {
        argv: vec!["blitz".to_string()],
        env: BTreeMap::new(),
        exec_path: "blitz".to_string(),
        listen_addr: "127.0.0.1:0"
          .parse()
          .expect("test listen address"),
      },
    );
    let source = r#"
      function handler(request) {
        if (request.headers.get("upgrade") !== "websocket") {
          return new Response("upgrade required", { status: 426 });
        }
        const upgraded = Blitz.upgradeWebSocket(request);
        globalThis.serverSocketHasWebSocketPrototype =
          Object.getPrototypeOf(upgraded.socket) === WebSocket.prototype &&
          upgraded.socket instanceof WebSocket;
        upgraded.socket.addEventListener("open", function() {
          upgraded.socket.send("ready");
        });
        upgraded.socket.onmessage = function(event) {
          upgraded.socket.send("echo:" + event.data);
        };
        return upgraded.response;
      }
      Blitz.serve(handler);
    "#;
    let session = JsParser::parse_source(source).expect("parse WebSocket server script");
    let chunk =
      compiler::compile_oxc(session.program(), source).expect("compile WebSocket server script");
    vm.run(chunk)
      .expect("evaluate WebSocket server script");

    let address = runtime.local_addr().expect("bound HTTP address");
    let client = thread::spawn(move || {
      tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("client runtime")
        .block_on(async move {
          let stream = tokio::net::TcpStream::connect(address)
            .await
            .expect("connect WebSocket client");
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
            .expect("WebSocket request");
          let (mut socket, response): (fastwebsockets::WebSocket<TokioIo<Upgraded>>, _) =
            fastwebsockets::handshake::client(&SpawnExecutor, request, stream)
              .await
              .expect("WebSocket handshake");
          assert_eq!(response.status(), hyper::StatusCode::SWITCHING_PROTOCOLS);
          let ready = socket
            .read_frame()
            .await
            .expect("read ready frame");
          socket
            .write_frame(Frame::text(Payload::Owned(b"hello".to_vec())))
            .await
            .expect("write client message");
          let echo = socket
            .read_frame()
            .await
            .expect("read echo frame");
          socket
            .write_frame(Frame::close(1000, b"done"))
            .await
            .expect("write close frame");
          (ready, echo)
        })
    });

    assert!(
      runtime
        .run_once(&mut vm)
        .expect("dispatch upgrade request")
    );
    assert_eq!(
      vm.get_global("serverSocketHasWebSocketPrototype"),
      Some(Value::Bool(true))
    );
    assert!(
      runtime
        .run_once(&mut vm)
        .expect("dispatch open event")
    );
    assert!(
      runtime
        .run_once(&mut vm)
        .expect("dispatch ready drain")
    );
    assert!(
      runtime
        .run_once(&mut vm)
        .expect("dispatch message event")
    );
    assert!(
      runtime
        .run_once(&mut vm)
        .expect("dispatch echo drain")
    );
    assert!(
      runtime
        .run_once(&mut vm)
        .expect("dispatch close event")
    );
    let (ready, echo) = client.join().expect("WebSocket client thread");
    assert_eq!(ready.opcode, OpCode::Text);
    assert_eq!(ready.payload.as_ref(), b"ready");
    assert_eq!(echo.opcode, OpCode::Text);
    assert_eq!(echo.payload.as_ref(), b"echo:hello");
    runtime.shutdown(&vm).expect("stop HTTP server");
  }

  #[test]
  fn websocket_constructor_connects_to_blitz_server() {
    let globals = Rc::new(RefCell::new(ObjectData::new()));
    let mut vm = Vm::new(globals);
    let mut runtime = install_runtime(
      &mut vm,
      RuntimeOptions {
        argv: vec!["blitz".to_string()],
        env: BTreeMap::new(),
        exec_path: "blitz".to_string(),
        listen_addr: "127.0.0.1:0"
          .parse()
          .expect("test listen address"),
      },
    );
    let source = r#"
      const server = Blitz.serve((request) => {
        const upgraded = Blitz.upgradeWebSocket(request);
        upgraded.socket.onmessage = (event) => upgraded.socket.send("echo:" + event.data);
        return upgraded.response;
      });
      const client = new WebSocket("ws://127.0.0.1:" + server.port + "/chat");
      globalThis.clientSocketHasWebSocketPrototype =
        Object.getPrototypeOf(client) === WebSocket.prototype &&
        client instanceof WebSocket;
      client.onopen = () => client.send("hello");
      client.onmessage = (event) => {
        globalThis.clientResult = event.data;
        client.close(1000, "done");
      };
    "#;
    let session = JsParser::parse_source(source).expect("parse WebSocket client script");
    let chunk =
      compiler::compile_oxc(session.program(), source).expect("compile WebSocket client script");
    vm.run(chunk)
      .expect("evaluate WebSocket client script");
    assert_eq!(
      vm.get_global("clientSocketHasWebSocketPrototype"),
      Some(Value::Bool(true))
    );

    for _step in 0..16 {
      assert!(
        runtime
          .run_once(&mut vm)
          .expect("drive client event")
      );
      if vm.get_global("clientResult") == Some(Value::String("echo:hello".to_string())) {
        break;
      }
    }
    assert_eq!(
      vm.get_global("clientResult"),
      Some(Value::String("echo:hello".to_string()))
    );
    runtime
      .shutdown(&vm)
      .expect("stop client and server");
  }

  #[test]
  fn irc_chat_demo_scripts_parse_and_compile() {
    for (name, source) in [
      ("server", include_str!("../../examples/irc-chat/server.js")),
      ("alice", include_str!("../../examples/irc-chat/alice.js")),
      ("bob", include_str!("../../examples/irc-chat/bob.js")),
    ] {
      let session =
        JsParser::parse_source(source).unwrap_or_else(|error| panic!("parse {name} demo: {error}"));
      compiler::compile_oxc(session.program(), source)
        .unwrap_or_else(|error| panic!("compile {name} demo: {error}"));
    }
  }

  #[test]
  fn irc_chat_server_runs_join_and_message_protocol() {
    let globals = Rc::new(RefCell::new(ObjectData::new()));
    let mut vm = Vm::new(globals);
    let mut runtime = install_runtime(
      &mut vm,
      RuntimeOptions {
        argv: vec!["blitz".to_string()],
        env: BTreeMap::new(),
        exec_path: "blitz".to_string(),
        listen_addr: "127.0.0.1:0"
          .parse()
          .expect("test listen address"),
      },
    );
    let source = include_str!("../../examples/irc-chat/server.js").replace("port: 8000", "port: 0");
    let session = JsParser::parse_source(&source).expect("parse IRC server");
    let chunk = compiler::compile_oxc(session.program(), &source).expect("compile IRC server");
    vm.run(chunk).expect("evaluate IRC server");

    let address = runtime.local_addr().expect("IRC server address");
    let wrong_route = thread::spawn(move || {
      let mut stream = TcpStream::connect(address).expect("connect to IRC HTTP server");
      write!(
        stream,
        "GET /wrong HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n"
      )
      .expect("write wrong-route request");
      let mut response = String::new();
      stream
        .read_to_string(&mut response)
        .expect("read wrong-route response");
      response
    });
    assert!(
      runtime
        .run_once(&mut vm)
        .expect("dispatch wrong-route request")
    );
    let wrong_route = wrong_route
      .join()
      .expect("wrong-route client thread");
    assert!(
      wrong_route.starts_with("HTTP/1.1 404"),
      "unexpected wrong-route response: {wrong_route}"
    );

    let client = thread::spawn(move || {
      tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("IRC client runtime")
        .block_on(async move {
          async fn connect(address: SocketAddr) -> fastwebsockets::WebSocket<TokioIo<Upgraded>> {
            let stream = tokio::net::TcpStream::connect(address)
              .await
              .expect("connect IRC client");
            let request = Request::builder()
              .method("GET")
              .uri(format!("http://{address}/chat"))
              .header("Host", address.to_string())
              .header(UPGRADE, "websocket")
              .header(CONNECTION, "upgrade")
              .header(
                "Sec-WebSocket-Key",
                fastwebsockets::handshake::generate_key(),
              )
              .header("Sec-WebSocket-Version", "13")
              .body(Empty::<Bytes>::new())
              .expect("IRC upgrade request");
            fastwebsockets::handshake::client(&SpawnExecutor, request, stream)
              .await
              .expect("IRC handshake")
              .0
          }

          let mut alice = connect(address).await;
          alice
            .write_frame(Frame::text(Payload::Owned(b"JOIN Alice".to_vec())))
            .await
            .expect("send Alice join");
          let alice_welcome = alice
            .read_frame()
            .await
            .expect("read Alice welcome");

          let mut bob = connect(address).await;
          bob
            .write_frame(Frame::text(Payload::Owned(b"JOIN Bob".to_vec())))
            .await
            .expect("send Bob join");
          let bob_welcome = bob.read_frame().await.expect("read Bob welcome");
          let alice_sees_bob = alice
            .read_frame()
            .await
            .expect("read Bob join on Alice");

          alice
            .write_frame(Frame::text(Payload::Owned(b"MSG hello".to_vec())))
            .await
            .expect("send Alice message");
          let bob_message = bob
            .read_frame()
            .await
            .expect("read Alice message on Bob");
          let alice_echoed =
            tokio::time::timeout(tokio::time::Duration::from_millis(100), alice.read_frame())
              .await
              .is_ok();
          alice
            .write_frame(Frame::close(1000, b"done"))
            .await
            .expect("close Alice");
          bob
            .write_frame(Frame::close(1000, b"done"))
            .await
            .expect("close Bob");
          (
            alice_welcome,
            bob_welcome,
            alice_sees_bob,
            bob_message,
            alice_echoed,
          )
        })
    });

    for _event in 0..11 {
      assert!(
        runtime
          .run_once(&mut vm)
          .expect("drive IRC event")
      );
    }
    let (alice_welcome, bob_welcome, alice_sees_bob, bob_message, alice_echoed) =
      client.join().expect("IRC client thread");
    assert_eq!(
      alice_welcome.payload.as_ref(),
      b"* Welcome, Alice. Type /quit to leave."
    );
    assert_eq!(
      bob_welcome.payload.as_ref(),
      b"* Welcome, Bob. Type /quit to leave."
    );
    assert_eq!(alice_sees_bob.payload.as_ref(), b"* Bob joined the room");
    assert_eq!(bob_message.payload.as_ref(), b"<Alice> hello");
    assert!(!alice_echoed, "Alice should not receive her own message");
    runtime.shutdown(&vm).expect("stop IRC server");
  }
}
