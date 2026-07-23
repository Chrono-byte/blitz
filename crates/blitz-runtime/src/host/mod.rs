//! Operating-system host APIs for the Blitz runtime.
//!
//! The ECMAScript engine is intentionally host-agnostic. This module is the
//! boundary where the Blitz executable installs capabilities supplied by the
//! operating system, such as process metadata, filesystem access, and async
//! I/O. Keeping that boundary explicit lets embedders select a smaller or
//! sandboxed host surface without changing VM semantics.

mod abort;
mod console;
pub mod event_loop;
mod process;
pub mod scheduler;
mod server;
mod terminal;
mod url;

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::env;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::rc::{Rc, Weak};
use std::sync::Arc;

use blitz::Vm;
use blitz::errors::RuntimeError;
use blitz::gc;
use blitz::value::{BuiltinFn, ObjectData, PropertyDescriptor, PropertyKey, Value};
use tokio::sync::mpsc;

use crate::app_transport::{AppServerResource, AppTransportOptions, open_app_server};
use crate::net::http::HttpRequestEvent;
use crate::net::websocket::WebSocketEvent;
use crate::runtime_graph::{
  APP_TRANSPORT_METHODS, APP_TRANSPORT_PRIMITIVE, HOST_BRIDGE_ABI_VERSION, HostBridgeCapabilities,
  RuntimeGraphError,
};
use event_loop::{EventRegistry, EventSender, ResourceHandle};
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
  events: EventRegistry<RuntimeEvent>,
  terminal: terminal::TerminalState,
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
    if !self.server.has_resources() && !self.terminal.is_active() && !self.events.is_live() {
      return Ok(false);
    }
    let Some(mut receiver) = self.events.take_receiver() else {
      return Ok(false);
    };
    let (receiver, event) = vm.block_on(async move {
      let event = receiver.recv().await;
      (receiver, event)
    })?;
    self.events.put_receiver(receiver);
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

  /// Register a VM-thread-owned resource that participates in runtime liveness.
  ///
  /// The returned handle is intentionally lightweight and can be retained by a
  /// host adapter. Resource cleanup remains owned by that adapter; calling
  /// [`ResourceHandle::close`] only updates the scheduler's liveness state.
  pub fn register_resource(
    &self,
    name: impl Into<String>,
    referenced: bool,
  ) -> ResourceHandle {
    self.events.register(name, referenced)
  }

  /// Whether the server, terminal, queued events, or a referenced resource
  /// keeps this runtime alive.
  #[must_use]
  pub fn is_live(&self) -> bool {
    self.server.has_resources() || self.terminal.is_active() || self.events.is_live()
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

/// Failure while installing the immutable runtime graph and host bridge.
#[derive(Debug)]
pub enum RuntimeInstallError {
  /// The committed artifact could not be decoded or failed pinned policy.
  Artifact(String),
  /// The artifact is incompatible with the host installed by this runtime.
  Policy(RuntimeGraphError),
  /// The VM's module loader rejected the graph before it became visible.
  Registration(String),
  /// This VM already owns the singleton runtime graph.
  AlreadyInstalled,
}

impl std::fmt::Display for RuntimeInstallError {
  fn fmt(
    &self,
    formatter: &mut std::fmt::Formatter<'_>,
  ) -> std::fmt::Result {
    match self {
      Self::Artifact(message) => write!(formatter, "runtime artifact error: {message}"),
      Self::Policy(error) => error.fmt(formatter),
      Self::Registration(message) => {
        write!(formatter, "runtime graph registration failed: {message}")
      }
      Self::AlreadyInstalled => {
        write!(formatter, "runtime graph is already installed in this VM")
      }
    }
  }
}

impl std::error::Error for RuntimeInstallError {}

thread_local! {
  static INSTALLED_GRAPHS: RefCell<Vec<Weak<RefCell<blitz::ModuleLoader>>>> =
    const { RefCell::new(Vec::new()) };
}

/// Install the default Blitz runtime APIs into an initialized VM.
///
/// # Panics
///
/// Panics if checked-in generated runtime assets violate their verified
/// canonical-specifier invariants or if this VM already has the runtime.
pub fn install_runtime(
  vm: &mut Vm,
  options: RuntimeOptions,
) -> RuntimeHandle {
  try_install_runtime(vm, options)
    .unwrap_or_else(|error| panic!("runtime installation failed: {error}"))
}

/// Fallible runtime installation used by embedders that need atomic policy
/// errors instead of the convenience panic from [`install_runtime`].
///
/// # Errors
///
/// Returns an error before graph visibility when artifact, policy, host ABI,
/// declaration, collision, or singleton validation fails.
///
/// # Panics
///
/// Panics only if the compile-time nonzero event-queue capacity is rejected.
pub fn try_install_runtime(
  vm: &mut Vm,
  options: RuntimeOptions,
) -> Result<RuntimeHandle, RuntimeInstallError> {
  let loader_handle = vm.module_loader_handle();
  let already_installed = INSTALLED_GRAPHS.with(|installed| {
    let mut installed = installed.borrow_mut();
    installed.retain(|loader| loader.strong_count() > 0);
    installed
      .iter()
      .any(|loader| loader.ptr_eq(&Rc::downgrade(&loader_handle)))
  });
  if already_installed {
    return Err(RuntimeInstallError::AlreadyInstalled);
  }
  let timing_enabled = env::var_os("BLITZ_RUNTIME_TIMINGS").is_some();
  let decode_started = std::time::Instant::now();
  let bundle = crate::runtime_graph::validate_embedded_runtime_bundle()
    .map_err(|error| RuntimeInstallError::Artifact(error.to_string()))?;
  let decode_and_validation = decode_started.elapsed();
  let (requests_tx, requests_rx) = mpsc::channel(64);
  let (websocket_events_tx, websocket_events_rx) = mpsc::channel(64);
  let (terminal_events_tx, terminal_events_rx) = mpsc::channel(64);
  let previous_bridge = vm.get_global("__blitz_host_v1");
  let capabilities = install_host_bridge(vm, requests_tx.clone(), websocket_events_tx.clone());
  let policy_started = std::time::Instant::now();
  if let Err(error) = bundle.validate_pinned_for_host(
    &capabilities,
    crate::effect_assets::EFFECT_MANIFEST_BYTES,
    crate::effect_assets::STDLIB_MANIFEST_BYTES,
  ) {
    restore_host_bridge(vm, previous_bridge);
    return Err(RuntimeInstallError::Policy(error));
  }
  let policy_validation = policy_started.elapsed();
  let registration_started = std::time::Instant::now();
  if let Err(error) = vm.register_prelinked_graph_with_declarations(
    bundle.graph.clone(),
    crate::effect_assets::public_declaration_sources(),
  ) {
    restore_host_bridge(vm, previous_bridge);
    return Err(RuntimeInstallError::Registration(error.to_string()));
  }
  let graph_registration = registration_started.elapsed();
  if timing_enabled {
    eprintln!(
      "blitz runtime graph: decode+generic-validation={decode_and_validation:?} policy-validation={policy_validation:?} registration={graph_registration:?}"
    );
  }
  INSTALLED_GRAPHS.with(|installed| {
    installed
      .borrow_mut()
      .push(Rc::downgrade(&loader_handle));
  });
  let events = EventRegistry::new(64).expect("runtime event capacity is non-zero");
  let event_sender = events.sender();
  forward_events(vm, requests_rx, event_sender.clone(), |event| {
    RuntimeEvent::Http(Box::new(event))
  });
  forward_events(
    vm,
    websocket_events_rx,
    event_sender.clone(),
    RuntimeEvent::WebSocket,
  );
  forward_events(vm, terminal_events_rx, event_sender, RuntimeEvent::Terminal);
  let server = server::install(vm, options.listen_addr, requests_tx, websocket_events_tx);
  let terminal = terminal::install(vm, terminal_events_tx);
  console::install(vm);
  abort::install(vm);
  process::install(vm, options);
  url::install(vm);
  Ok(RuntimeHandle {
    server,
    events,
    terminal,
  })
}

/// Install the ABI object consumed by the prelinked private adapter.
///
/// The existing transport owns binding and lifecycle correctly. Its
/// transitional limitation remains that it forwards requests to the ordinary
/// runtime event channel rather than invoking the `blitz:app` router/layers;
/// that behavior is deliberately not hidden by host capability validation.
fn install_host_bridge(
  vm: &mut Vm,
  requests: mpsc::Sender<HttpRequestEvent>,
  websocket_events: mpsc::Sender<WebSocketEvent>,
) -> HostBridgeCapabilities {
  let app_transport = object(vm.realm().object_prototype());
  set(
    &app_transport,
    "openAppServer",
    Value::Builtin(open_app_server_method(requests, websocket_events)),
  );
  set(
    &app_transport,
    "closeAppServer",
    Value::Builtin(close_app_server_method()),
  );
  set(
    &app_transport,
    "awaitShutdown",
    Value::Builtin(await_shutdown_method()),
  );
  if let Value::Object(object) = &app_transport {
    object.borrow_mut().extensible = false;
  }
  let bridge = object(vm.realm().object_prototype());
  set(
    &bridge,
    "abiVersion",
    Value::Number(f64::from(HOST_BRIDGE_ABI_VERSION)),
  );
  set(&bridge, "appTransport", app_transport);
  if let Value::Object(object) = &bridge {
    object.borrow_mut().extensible = false;
  }
  vm.realm()
    .global()
    .borrow_mut()
    .properties
    .insert(
      PropertyKey::String("__blitz_host_v1".to_string()),
      PropertyDescriptor::Data {
        value: bridge,
        writable: false,
        enumerable: false,
        configurable: false,
      },
    );
  HostBridgeCapabilities::new(HOST_BRIDGE_ABI_VERSION).with_primitive(
    APP_TRANSPORT_PRIMITIVE,
    HOST_BRIDGE_ABI_VERSION,
    APP_TRANSPORT_METHODS.iter().copied(),
  )
}

fn restore_host_bridge(
  vm: &mut Vm,
  previous: Option<Value>,
) {
  if let Some(previous) = previous {
    vm.realm().set_global("__blitz_host_v1", previous);
  } else {
    vm.realm()
      .global()
      .borrow_mut()
      .properties
      .shift_remove(&PropertyKey::String("__blitz_host_v1".to_string()));
  }
}

fn object(prototype: Value) -> Value {
  Value::Object(gc::track_object(Rc::new(RefCell::new(
    ObjectData::with_prototype(prototype),
  ))))
}

fn set(
  target: &Value,
  name: &str,
  value: Value,
) {
  if let Value::Object(object) = target {
    object.borrow_mut().properties.insert(
      PropertyKey::String(name.to_string()),
      PropertyDescriptor::Data {
        value,
        writable: false,
        enumerable: false,
        configurable: false,
      },
    );
  }
}

fn effect_function(
  vm: &mut Vm,
  name: &str,
) -> Result<Value, RuntimeError> {
  let record = {
    let loader = vm.module_loader_handle();
    let loader = loader.borrow();
    loader
      .map
      .values()
      .find(|record| record.borrow().specifier == "effect/Effect")
      .cloned()
  };
  let record = record.ok_or_else(|| RuntimeError::from("effect/Effect is not instantiated"))?;
  let slot = record
    .borrow()
    .exports
    .get(name)
    .cloned()
    .ok_or_else(|| RuntimeError::from(format!("effect/Effect has no {name} export")))?;
  Ok(
    slot
      .lock()
      .map_err(|_| RuntimeError::from("effect/Effect export lock is poisoned"))?
      .clone(),
  )
}

fn effect_sync(
  vm: &mut Vm,
  thunk: BuiltinFn,
) -> Result<Value, RuntimeError> {
  let sync = effect_function(vm, "sync")?;
  vm.call_value_with_this(&sync, &Value::Undefined, &[Value::Builtin(thunk)])
}

fn transport_options(
  vm: &mut Vm,
  value: &Value,
) -> Result<AppTransportOptions, RuntimeError> {
  let hostname_value = vm.get_property_host(value, &PropertyKey::String("hostname".to_string()))?;
  let hostname = vm.to_string(&hostname_value)?;
  let port_value = vm.get_property_host(value, &PropertyKey::String("port".to_string()))?;
  let port_number = vm.to_number_value(&port_value)?.trunc();
  if !port_number.is_finite() || !(0.0_f64..=f64::from(u16::MAX)).contains(&port_number) {
    return Err(RuntimeError::from("application transport port is invalid"));
  }
  let port = port_number
    .to_string()
    .parse::<u16>()
    .map_err(|_| RuntimeError::from("application transport port is invalid"))?;
  let rpc_path_value = vm.get_property_host(value, &PropertyKey::String("rpcPath".to_string()))?;
  let rpc_path = vm.to_string(&rpc_path_value)?;
  Ok(AppTransportOptions {
    hostname,
    port,
    rpc_path,
  })
}

fn open_app_server_method(
  requests: mpsc::Sender<HttpRequestEvent>,
  websocket_events: mpsc::Sender<WebSocketEvent>,
) -> BuiltinFn {
  Rc::new(move |vm, args| {
    let options = transport_options(
      vm,
      args
        .get(1)
        .ok_or_else(|| RuntimeError::from("openAppServer requires options"))?,
    )?;
    let thunk: BuiltinFn = {
      let requests = requests.clone();
      let websocket_events = websocket_events.clone();
      Rc::new(move |vm, _args| {
        let resource = vm
          .block_on(open_app_server(
            options.clone(),
            requests.clone(),
            websocket_events.clone(),
          ))?
          .map_err(|error| RuntimeError::from(error.to_string()))?;
        Ok(resource_value(vm, Arc::new(resource)))
      })
    };
    effect_sync(vm, thunk)
  })
}

fn resource_value(
  vm: &mut Vm,
  resource: Arc<AppServerResource>,
) -> Value {
  let server = object(vm.realm().object_prototype());
  set(
    &server,
    "localAddr",
    Value::String(resource.local_addr().to_string()),
  );
  let close_resource = Arc::clone(&resource);
  let close: BuiltinFn = Rc::new(move |vm, _args| {
    vm.block_on(Arc::clone(&close_resource).close_owned())?;
    Ok(Value::Undefined)
  });
  let await_resource = resource;
  let await_shutdown: BuiltinFn = Rc::new(move |vm, _args| {
    vm.block_on(Arc::clone(&await_resource).await_shutdown_owned())?;
    Ok(Value::Undefined)
  });
  set(&server, "__blitz_close", Value::Builtin(close));
  set(
    &server,
    "__blitz_await_shutdown",
    Value::Builtin(await_shutdown),
  );
  server
}

fn close_app_server_method() -> BuiltinFn {
  Rc::new(|vm, args| {
    let server = args
      .get(1)
      .ok_or_else(|| RuntimeError::from("closeAppServer requires a server"))?;
    let close = vm.get_property_host(server, &PropertyKey::String("__blitz_close".to_string()))?;
    let thunk = match close {
      Value::Builtin(close) => close,
      _ => return Err(RuntimeError::from("invalid application server resource")),
    };
    effect_sync(vm, thunk)
  })
}

fn await_shutdown_method() -> BuiltinFn {
  Rc::new(|vm, args| {
    let server = args
      .get(1)
      .ok_or_else(|| RuntimeError::from("awaitShutdown requires a server"))?;
    let await_shutdown = vm.get_property_host(
      server,
      &PropertyKey::String("__blitz_await_shutdown".to_string()),
    )?;
    let thunk = match await_shutdown {
      Value::Builtin(await_shutdown) => await_shutdown,
      _ => return Err(RuntimeError::from("invalid application server resource")),
    };
    effect_sync(vm, thunk)
  })
}

fn forward_events<T>(
  vm: &Vm,
  mut receiver: mpsc::Receiver<T>,
  sender: EventSender<RuntimeEvent>,
  map: fn(T) -> RuntimeEvent,
) where
  T: Send + 'static,
{
  vm.spawn_bg(async move {
    while let Some(event) = receiver.recv().await {
      if sender.send(map(event)).await.is_err() {
        break;
      }
    }
  });
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
  fn second_runtime_installation_is_rejected_before_side_effects() {
    let globals = Rc::new(RefCell::new(ObjectData::new()));
    let mut vm = Vm::new(globals);
    let options = RuntimeOptions::from_host(vec!["blitz-test".to_string()]);
    let _runtime = install_runtime(&mut vm, options.clone());
    let result = try_install_runtime(&mut vm, options);
    assert!(matches!(result, Err(RuntimeInstallError::AlreadyInstalled)));
  }

  #[test]
  fn private_host_bridge_is_immutable_and_non_enumerable() {
    let globals = Rc::new(RefCell::new(ObjectData::new()));
    let mut vm = Vm::new(Rc::clone(&globals));
    let _runtime = install_runtime(
      &mut vm,
      RuntimeOptions::from_host(vec!["blitz-test".to_string()]),
    );
    let descriptor = globals
      .borrow()
      .properties
      .get(&PropertyKey::String("__blitz_host_v1".to_string()))
      .cloned()
      .expect("host bridge descriptor");
    let PropertyDescriptor::Data {
      value: Value::Object(bridge),
      writable,
      enumerable,
      configurable,
    } = descriptor
    else {
      panic!("host bridge is an object data property");
    };
    assert!(!writable);
    assert!(!enumerable);
    assert!(!configurable);
    assert!(!bridge.borrow().extensible);
    let app_transport = bridge
      .borrow()
      .properties
      .get(&PropertyKey::String("appTransport".to_string()))
      .and_then(|descriptor| match descriptor {
        PropertyDescriptor::Data { value, .. } => Some(value.clone()),
        PropertyDescriptor::Accessor { .. } => None,
      })
      .expect("app transport primitive");
    assert!(matches!(app_transport, Value::Object(object) if !object.borrow().extensible));
  }

  #[test]
  fn runtime_handle_tracks_additional_resource_liveness() {
    let globals = Rc::new(RefCell::new(ObjectData::new()));
    let mut vm = Vm::new(globals);
    let runtime = install_runtime(
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
    assert!(!runtime.is_live());
    let resource = runtime.register_resource("test-resource", true);
    assert!(runtime.is_live());
    resource.unreference();
    assert!(!runtime.is_live());
    assert!(resource.close());
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
  fn effect_irc_chat_v2_scripts_parse_and_compile() {
    let server = include_str!("../../examples/irc-chat-v2/server.mjs");
    let session = JsParser::parse_source_with_type(server, oxc_span::SourceType::mjs())
      .expect("parse Effect IRC server module");
    compiler::compile_oxc(session.program(), server).expect("compile Effect IRC server module");

    for (name, source) in [
      ("alice", include_str!("../../examples/irc-chat-v2/alice.js")),
      ("bob", include_str!("../../examples/irc-chat-v2/bob.js")),
    ] {
      let session = JsParser::parse_source(source)
        .unwrap_or_else(|error| panic!("parse Effect IRC {name} client: {error}"));
      compiler::compile_oxc(session.program(), source)
        .unwrap_or_else(|error| panic!("compile Effect IRC {name} client: {error}"));
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
