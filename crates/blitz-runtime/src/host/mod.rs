// SPDX-License-Identifier: MPL-2.0
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
mod fs;
mod node_crypto;
mod node_fs;
mod process;
pub mod scheduler;
mod server;
mod terminal;
mod url;

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::env;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::time::{Duration, Instant};

use blitz::errors::RuntimeError;
use blitz::gc;
use blitz::module::PrelinkedHostModule;
use blitz::value::{BuiltinFn, ObjectData, PropertyDescriptor, PropertyKey, Value};
use blitz::{HostRoot, Vm};
use tokio::sync::mpsc;

use crate::app_transport::{
  AppServerResource, AppTransportError, AppTransportOptions, open_app_server,
};
use crate::net::http::HttpRequestEvent;
use crate::net::websocket::WebSocketEvent;
use crate::runtime_graph::{
  APP_TRANSPORT_METHODS, APP_TRANSPORT_PRIMITIVE, FILE_SYSTEM_METHODS, FILE_SYSTEM_PRIMITIVE,
  HOST_BRIDGE_ABI_VERSION, HTTP_METHODS, HTTP_PRIMITIVE, HostBridgeCapabilities,
  NODE_CRYPTO_RANDOM_ABI_VERSION, NODE_CRYPTO_RANDOM_METHODS, NODE_CRYPTO_RANDOM_PRIMITIVE,
  NODE_FILE_SYSTEM_METHODS, NODE_FILE_SYSTEM_PRIMITIVE, RuntimeGraphError, TERMINAL_METHODS,
  TERMINAL_PRIMITIVE, WEB_SOCKET_METHODS, WEB_SOCKET_PRIMITIVE,
};
use event_loop::{EventRegistry, EventSender, ResourceHandle, ResourceRegistry};
use scheduler::{TimerId, TimerKind, TimerScheduler, WakeupEvent};
use terminal::TerminalEvent;

/// Node compatibility modules exposed to an application realm.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum NodeCompatProfile {
  /// Do not expose any `node:*` modules.
  Off,
  /// Expose the completed process-local compatibility modules.
  #[default]
  Core,
  /// Expose completed modules needed by development tooling.
  Tooling,
  /// Expose every completed Node compatibility module.
  Full,
}

impl NodeCompatProfile {
  // Keep these lists explicit. A new generated `node:*` root must not become
  // available merely by being added to the runtime graph; it needs a reviewed
  // compatibility entry in each profile that should expose it.
  const CORE_MODULES: &'static [&'static str] = &[
    "node:assert",
    "node:assert/strict",
    "node:async_hooks",
    "node:buffer",
    "node:console",
    "node:constants",
    "node:diagnostics_channel",
    "node:events",
    "node:module",
    "node:os",
    "node:path",
    "node:path/posix",
    "node:path/win32",
    "node:perf_hooks",
    "node:process",
    "node:querystring",
    "node:stream",
    "node:stream/consumers",
    "node:stream/promises",
    "node:string_decoder",
    "node:timers",
    "node:timers/promises",
    "node:tty",
    "node:url",
    "node:util",
    "node:util/types",
  ];
  const TOOLING_MODULES: &'static [&'static str] = &[
    "node:assert",
    "node:assert/strict",
    "node:async_hooks",
    "node:buffer",
    "node:console",
    "node:constants",
    "node:crypto",
    "node:diagnostics_channel",
    "node:events",
    "node:fs",
    "node:fs/promises",
    "node:module",
    "node:os",
    "node:path",
    "node:path/posix",
    "node:path/win32",
    "node:perf_hooks",
    "node:process",
    "node:querystring",
    "node:readline",
    "node:readline/promises",
    "node:stream",
    "node:stream/consumers",
    "node:stream/promises",
    "node:string_decoder",
    "node:timers",
    "node:timers/promises",
    "node:tty",
    "node:url",
    "node:util",
    "node:util/types",
  ];
  const FULL_MODULES: &'static [&'static str] = &[
    "node:assert",
    "node:assert/strict",
    "node:async_hooks",
    "node:buffer",
    "node:console",
    "node:constants",
    "node:crypto",
    "node:diagnostics_channel",
    "node:domain",
    "node:events",
    "node:fs",
    "node:fs/promises",
    "node:module",
    "node:os",
    "node:path",
    "node:path/posix",
    "node:path/win32",
    "node:perf_hooks",
    "node:process",
    "node:punycode",
    "node:querystring",
    "node:readline",
    "node:readline/promises",
    "node:sea",
    "node:stream",
    "node:stream/consumers",
    "node:stream/promises",
    "node:string_decoder",
    "node:sys",
    "node:timers",
    "node:timers/promises",
    "node:trace_events",
    "node:tty",
    "node:url",
    "node:util",
    "node:util/types",
  ];

  /// Return the canonical `node:*` roots enabled by this profile.
  #[must_use]
  pub const fn allowed_modules(self) -> &'static [&'static str] {
    match self {
      Self::Off => &[],
      Self::Core => Self::CORE_MODULES,
      Self::Tooling => Self::TOOLING_MODULES,
      Self::Full => Self::FULL_MODULES,
    }
  }

  /// Return the stable name used by host-facing diagnostics.
  #[must_use]
  pub const fn name(self) -> &'static str {
    match self {
      Self::Off => "off",
      Self::Core => "core",
      Self::Tooling => "tooling",
      Self::Full => "full",
    }
  }
}

/// Host configuration installed into a Blitz VM.
#[derive(Clone, Debug)]
pub struct RuntimeOptions {
  /// Process arguments exposed to JavaScript.
  pub argv: Vec<String>,
  /// Environment variables exposed to JavaScript.
  pub env: BTreeMap<String, String>,
  /// Path of the running Blitz executable.
  pub exec_path: String,
  /// Address used by the optional compatibility HTTP server.
  pub listen_addr: SocketAddr,
  /// Install the legacy Deno-shaped `Blitz.*` compatibility namespace.
  ///
  /// The default runtime leaves this disabled. Public runtime capabilities are
  /// exposed through Effect-native `blitz:*` modules instead.
  pub install_compatibility_globals: bool,
  /// Select the allowlisted `node:*` compatibility surface.
  pub node_compat: NodeCompatProfile,
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
      install_compatibility_globals: false,
      node_compat: NodeCompatProfile::default(),
    }
  }
}

/// VM-thread driver for runtime-owned asynchronous resources.
pub struct RuntimeHandle {
  server: server::ServerState,
  events: EventRegistry<RuntimeEvent>,
  scheduler: SchedulerHostState,
  terminal: terminal::TerminalState,
}

enum RuntimeEvent {
  Http(Box<HttpRequestEvent>),
  WebSocket(WebSocketEvent),
  Terminal(TerminalEvent),
  Wakeup(WakeupEvent),
}

struct TimerCallback {
  callback: HostRoot,
  arguments: Vec<HostRoot>,
}

#[derive(Clone)]
struct SchedulerHostState {
  scheduler: Rc<RefCell<TimerScheduler<RuntimeEvent>>>,
  callbacks: Rc<RefCell<HashMap<TimerId, TimerCallback>>>,
}

impl SchedulerHostState {
  fn dispatch(
    &self,
    vm: &mut Vm,
    event: WakeupEvent,
  ) -> Result<(), RuntimeError> {
    let Some(wakeup) = event.timer() else {
      return Ok(());
    };
    let Some(fired) = self
      .scheduler
      .borrow_mut()
      .handle_wakeup(vm, wakeup, Instant::now())
    else {
      return Ok(());
    };

    let callback = if fired.kind() == TimerKind::Timeout {
      self.callbacks.borrow_mut().remove(&fired.id())
    } else {
      self
        .callbacks
        .borrow()
        .get(&fired.id())
        .map(|callback| TimerCallback {
          callback: callback.callback,
          arguments: callback.arguments.clone(),
        })
    };
    let Some(callback) = callback else {
      return Ok(());
    };
    let function = vm
      .host_root(callback.callback)
      .unwrap_or(Value::Undefined);
    let arguments = callback
      .arguments
      .iter()
      .filter_map(|root| vm.host_root(*root))
      .collect::<Vec<_>>();
    if fired.kind() == TimerKind::Timeout {
      release_timer_roots(vm, &callback)?;
    }
    if function.is_callable() {
      vm.call_value_with_this(&function, &Value::Undefined, &arguments)?;
    }
    Ok(())
  }

  fn clear(
    &self,
    vm: &mut Vm,
    id: TimerId,
  ) -> Result<(), RuntimeError> {
    self.scheduler.borrow_mut().clear_timer(id);
    if let Some(callback) = self.callbacks.borrow_mut().remove(&id) {
      release_timer_roots(vm, &callback)?;
    }
    Ok(())
  }
}

fn release_timer_roots(
  vm: &mut Vm,
  callback: &TimerCallback,
) -> Result<(), RuntimeError> {
  vm.set_host_root(callback.callback, Value::Undefined)?;
  for root in &callback.arguments {
    vm.set_host_root(*root, Value::Undefined)?;
  }
  Ok(())
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
      RuntimeEvent::Wakeup(event) => self.scheduler.dispatch(vm, event)?,
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
    vm: &mut Vm,
  ) -> Result<(), RuntimeError> {
    let _finished = self.server.close_current(vm)?;
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
  let decode_started = Instant::now();
  let bundle = crate::runtime_graph::validate_embedded_runtime_bundle()
    .map_err(|error| RuntimeInstallError::Artifact(error.to_string()))?;
  let decode_and_validation = decode_started.elapsed();
  let (requests_tx, requests_rx) = mpsc::channel(64);
  let (websocket_events_tx, websocket_events_rx) = mpsc::channel(64);
  let (terminal_events_tx, terminal_events_rx) = mpsc::channel(64);
  let events = EventRegistry::new(64).expect("runtime event capacity is non-zero");
  let server_slot = Rc::new(RefCell::new(None));
  let terminal_slot = Rc::new(RefCell::new(None));
  let (host_bridge, capabilities) = create_host_bridge(
    vm,
    requests_tx.clone(),
    websocket_events_tx.clone(),
    events.resources(),
    Rc::clone(&server_slot),
    Rc::clone(&terminal_slot),
    options
      .node_compat
      .allowed_modules()
      .iter()
      .any(|module| matches!(*module, "node:fs" | "node:fs/promises")),
    options
      .node_compat
      .allowed_modules()
      .contains(&"node:crypto"),
  );
  let policy_started = Instant::now();
  if let Err(error) = bundle.validate_pinned_for_host(
    &capabilities,
    crate::effect_assets::EFFECT_MANIFEST_BYTES,
    crate::effect_assets::STDLIB_MANIFEST_BYTES,
  ) {
    return Err(RuntimeInstallError::Policy(error));
  }
  let policy_validation = policy_started.elapsed();
  let registration_started = Instant::now();
  let mut graph = bundle.graph.clone();
  let allowed_node_modules = options.node_compat.allowed_modules();
  let disabled_node_roots = graph
    .public_roots
    .iter()
    .copied()
    .filter(|module_index| {
      graph.module(*module_index).is_some_and(|module| {
        module.specifier.starts_with("node:")
          && !allowed_node_modules.contains(&module.specifier.as_str())
      })
    })
    .collect::<Vec<_>>();
  graph
    .public_roots
    .retain(|module_index| !disabled_node_roots.contains(module_index));
  let declarations = crate::effect_assets::public_declaration_sources()
    .into_iter()
    .filter(|(specifier, _)| {
      !specifier.starts_with("node:") || allowed_node_modules.contains(&specifier.as_str())
    });
  let host_module = PrelinkedHostModule {
    specifier: "blitz-internal:host".to_string(),
    exports: HashMap::from([("host".to_string(), host_bridge)]),
  };
  if let Err(error) = vm.register_prelinked_graph_with_declarations_and_host_modules(
    graph,
    declarations,
    vec![host_module],
  ) {
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
  let event_sender = events.sender();
  let scheduler = install_scheduler(vm, events.resources(), event_sender.clone());
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
  let server = server::install(
    vm,
    options.listen_addr,
    requests_tx,
    websocket_events_tx,
    events.resources(),
    options.install_compatibility_globals,
  );
  *server_slot.borrow_mut() = Some(server.clone());
  let terminal = terminal::install(vm, terminal_events_tx);
  *terminal_slot.borrow_mut() = Some(terminal.clone());
  console::install(vm);
  abort::install(vm);
  process::install(vm, options);
  url::install(vm);
  Ok(RuntimeHandle {
    server,
    events,
    scheduler,
    terminal,
  })
}

fn install_scheduler(
  vm: &mut Vm,
  resources: ResourceRegistry,
  events: EventSender<RuntimeEvent>,
) -> SchedulerHostState {
  let state = SchedulerHostState {
    scheduler: Rc::new(RefCell::new(TimerScheduler::with_event_mapper(
      resources,
      events,
      RuntimeEvent::Wakeup,
    ))),
    callbacks: Rc::new(RefCell::new(HashMap::new())),
  };

  install_global_function(
    vm,
    "setTimeout",
    timer_set_method(state.clone(), TimerKind::Timeout),
  );
  install_global_function(
    vm,
    "setInterval",
    timer_set_method(state.clone(), TimerKind::Interval),
  );
  install_global_function(vm, "clearTimeout", timer_clear_method(state.clone()));
  install_global_function(vm, "clearInterval", timer_clear_method(state.clone()));

  let performance = object(vm.realm().object_prototype());
  let now_state = state.clone();
  let now: BuiltinFn = Rc::new(move |_vm, _args| {
    Ok(Value::Number(
      now_state.scheduler.borrow().clock().now_millis(),
    ))
  });
  set(&performance, "now", Value::Builtin(now));
  vm.realm()
    .global()
    .borrow_mut()
    .properties
    .insert(
      PropertyKey::String("performance".to_string().into()),
      PropertyDescriptor::Data {
        value: performance,
        writable: true,
        enumerable: false,
        configurable: true,
      },
    );
  state
}

fn install_global_function(
  vm: &Vm,
  name: &str,
  function: BuiltinFn,
) {
  vm.realm()
    .global()
    .borrow_mut()
    .properties
    .insert(
      PropertyKey::String(name.to_string().into()),
      PropertyDescriptor::Data {
        value: Value::Builtin(function),
        writable: true,
        enumerable: false,
        configurable: true,
      },
    );
}

fn timer_set_method(
  state: SchedulerHostState,
  kind: TimerKind,
) -> BuiltinFn {
  Rc::new(move |vm, args| {
    let callback_index = if args.first().is_some_and(Value::is_callable) {
      0
    } else {
      1
    };
    let callback = args
      .get(callback_index)
      .cloned()
      .ok_or_else(|| RuntimeError::from("timer requires a callback"))?;
    if !callback.is_callable() {
      return Err(RuntimeError::from("timer callback must be callable"));
    }
    let delay = args
      .get(callback_index + 1)
      .map(|value| vm.to_number_value(value))
      .transpose()?
      .unwrap_or(0.0);
    let delay = if delay.is_nan() || delay < 0.0 {
      Duration::ZERO
    } else {
      Duration::try_from_secs_f64(delay / 1_000.0)
        .map_err(|_| RuntimeError::from("timer delay is outside the supported range"))?
    };
    let id = match kind {
      TimerKind::Timeout => state
        .scheduler
        .borrow_mut()
        .set_timeout(vm, delay, true),
      TimerKind::Interval => state
        .scheduler
        .borrow_mut()
        .set_interval(vm, delay, true),
    }
    .map_err(|error| RuntimeError::from(error.to_string()))?;
    let callback = TimerCallback {
      callback: vm.create_host_root(callback),
      arguments: args
        .iter()
        .skip(callback_index + 2)
        .cloned()
        .map(|value| vm.create_host_root(value))
        .collect(),
    };
    state.callbacks.borrow_mut().insert(id, callback);
    let numeric_id = id
      .value()
      .to_string()
      .parse::<f64>()
      .map_err(|_| RuntimeError::from("timer identifier cannot be represented"))?;
    Ok(Value::Number(numeric_id))
  })
}

fn timer_clear_method(state: SchedulerHostState) -> BuiltinFn {
  Rc::new(move |vm, args| {
    let Some(value) = args
      .first()
      .filter(|value| matches!(value, Value::Number(_)))
      .or_else(|| args.get(1))
    else {
      return Ok(Value::Undefined);
    };
    let numeric = vm.to_number_value(value)?.trunc();
    if !numeric.is_finite() || numeric < 0.0 {
      return Ok(Value::Undefined);
    }
    let Ok(value) = numeric.to_string().parse::<u64>() else {
      return Ok(Value::Undefined);
    };
    state.clear(vm, TimerId::from_value(value))?;
    Ok(Value::Undefined)
  })
}

fn http_primitive(
  vm: &Vm,
  server: Rc<RefCell<Option<server::ServerState>>>,
) -> Value {
  let primitive = object(vm.realm().object_prototype());
  let open_server = Rc::clone(&server);
  let open: BuiltinFn = Rc::new(move |vm, args| {
    let Some(options) = args.get(1).cloned() else {
      let error = http_error(
        vm,
        "InvalidOptions",
        "listen",
        "openServer requires options",
      );
      return effect_fail(vm, error);
    };
    let handler =
      match vm.get_property_host(&options, &PropertyKey::String("handler".to_string().into())) {
        Ok(handler) if handler.is_callable() => handler,
        _ => {
          let error = http_error(
            vm,
            "InvalidOptions",
            "listen",
            "HTTP handler must be callable",
          );
          return effect_fail(vm, error);
        }
      };
    let Some(state) = open_server.borrow().clone() else {
      let error = http_error(vm, "Transport", "listen", "HTTP host is not initialized");
      return effect_fail(vm, error);
    };
    match server::serve_method(state)(vm, &[Value::Undefined, options, handler]) {
      Ok(handle) => {
        let local_address = match (
          vm.get_property_host(&handle, &PropertyKey::String("hostname".to_string().into())),
          vm.get_property_host(&handle, &PropertyKey::String("port".to_string().into())),
        ) {
          (Ok(Value::String(hostname)), Ok(Value::Number(port))) => {
            Value::String(format!("{hostname}:{}", port.trunc()).into())
          }
          _ => Value::String(String::new().into()),
        };
        if let Value::Object(object) = &handle {
          object.borrow_mut().properties.insert(
            PropertyKey::String("localAddress".to_string().into()),
            PropertyDescriptor::ordinary_data(local_address),
          );
        }
        effect_succeed(vm, handle)
      }
      Err(error) => {
        let message = error.to_string();
        let reason = if message.contains("bind") {
          "Bind"
        } else {
          "InvalidOptions"
        };
        let error = http_error(vm, reason, "listen", &message);
        effect_fail(vm, error)
      }
    }
  });
  set(&primitive, "openServer", Value::Builtin(open));

  let close: BuiltinFn = Rc::new(|vm, args| {
    let Some(handle) = args.get(1) else {
      return effect_succeed(vm, Value::Undefined);
    };
    let shutdown =
      vm.get_property_host(handle, &PropertyKey::String("shutdown".to_string().into()))?;
    if shutdown.is_callable() {
      let result = vm.call_value_with_this(&shutdown, handle, &[])?;
      let _value = vm.await_value(result)?;
    }
    effect_succeed(vm, Value::Undefined)
  });
  set(&primitive, "closeServer", Value::Builtin(close));

  let await_shutdown: BuiltinFn = Rc::new(|vm, args| {
    if args.get(1).is_none() {
      let error = http_error(vm, "Transport", "serve", "awaitShutdown requires a server");
      return effect_fail(vm, error);
    }
    effect_function(vm, "never")
  });
  set(&primitive, "awaitShutdown", Value::Builtin(await_shutdown));
  if let Value::Object(object) = &primitive {
    object.borrow_mut().extensible = false;
  }
  primitive
}

fn http_error(
  vm: &Vm,
  reason: &str,
  operation: &str,
  message: &str,
) -> Value {
  let error = object(vm.realm().object_prototype());
  set(
    &error,
    "_tag",
    Value::String("HttpServerError".to_string().into()),
  );
  set(&error, "reason", Value::String(reason.to_string().into()));
  set(
    &error,
    "operation",
    Value::String(operation.to_string().into()),
  );
  set(&error, "message", Value::String(message.to_string().into()));
  error
}

fn websocket_primitive(
  vm: &Vm,
  server: Rc<RefCell<Option<server::ServerState>>>,
) -> Value {
  let primitive = object(vm.realm().object_prototype());

  let connect: BuiltinFn = Rc::new(|vm, args| {
    let options = args.get(1).cloned().unwrap_or(Value::Undefined);
    let complete = args.get(2).cloned().unwrap_or(Value::Undefined);
    let url = vm.get_property_host(&options, &PropertyKey::String("url".to_string().into()))?;
    let constructor = vm
      .get_global("WebSocket")
      .ok_or_else(|| RuntimeError::from("WebSocket is not installed"))?;
    let socket = vm.construct_value(&constructor, &[url])?;
    let callback_root = vm.create_host_root(complete);
    let callback_socket = socket.clone();
    let on_open: BuiltinFn = Rc::new(move |vm, _args| {
      if let Some(callback) = vm
        .host_root(callback_root)
        .filter(Value::is_callable)
      {
        complete_success(vm, &callback, callback_socket.clone())?;
        vm.set_host_root(callback_root, Value::Undefined)?;
      }
      Ok(Value::Undefined)
    });
    let error_root = callback_root;
    let on_error: BuiltinFn = Rc::new(move |vm, _args| {
      if let Some(callback) = vm
        .host_root(error_root)
        .filter(Value::is_callable)
      {
        complete_failure(vm, &callback, "connect", "WebSocket connection failed")?;
        vm.set_host_root(error_root, Value::Undefined)?;
      }
      Ok(Value::Undefined)
    });
    set_socket_handler(vm, &socket, "onopen", Value::Builtin(on_open))?;
    set_socket_handler(vm, &socket, "onerror", Value::Builtin(on_error))?;
    Ok(socket)
  });
  set(&primitive, "connect", Value::Builtin(connect));

  let upgrade_server = Rc::clone(&server);
  let prepare_upgrade: BuiltinFn = Rc::new(move |vm, args| {
    let request = args.get(1).cloned().unwrap_or(Value::Undefined);
    let complete = args.get(3).cloned().unwrap_or(Value::Undefined);
    let Some(state) = upgrade_server.borrow().clone() else {
      complete_failure(
        vm,
        &complete,
        "prepareUpgrade",
        "WebSocket host is not initialized",
      )?;
      return Ok(Value::Undefined);
    };
    let prototype = state
      .websocket_prototype()
      .unwrap_or_else(|| vm.realm().object_prototype());
    match server::upgrade_websocket_method(state, prototype)(vm, &[Value::Undefined, request]) {
      Ok(upgrade) => {
        if complete.is_callable() {
          complete_success(vm, &complete, upgrade.clone())?;
        }
        Ok(upgrade)
      }
      Err(error) if complete.is_callable() => {
        complete_failure(vm, &complete, "prepareUpgrade", &error.to_string())?;
        Ok(Value::Undefined)
      }
      Err(error) => Err(error),
    }
  });
  set(
    &primitive,
    "prepareUpgrade",
    Value::Builtin(prepare_upgrade),
  );

  let accept: BuiltinFn = Rc::new(|vm, args| {
    let upgrade = args.get(1).cloned().unwrap_or(Value::Undefined);
    let complete = args.get(2).cloned().unwrap_or(Value::Undefined);
    let socket =
      vm.get_property_host(&upgrade, &PropertyKey::String("socket".to_string().into()))?;
    if complete.is_callable() {
      complete_success(vm, &complete, socket.clone())?;
    }
    Ok(socket)
  });
  set(&primitive, "accept", Value::Builtin(accept));

  let send: BuiltinFn = Rc::new(|vm, args| {
    let socket = args.get(1).cloned().unwrap_or(Value::Undefined);
    let message = args.get(2).cloned().unwrap_or(Value::Undefined);
    let complete = args.get(3).cloned().unwrap_or(Value::Undefined);
    let method = vm.get_property_host(&socket, &PropertyKey::String("send".to_string().into()))?;
    match vm.call_value_with_this(&method, &socket, &[message]) {
      Ok(_) if complete.is_callable() => complete_success(vm, &complete, Value::Undefined)?,
      Ok(_) => {}
      Err(error) if complete.is_callable() => {
        complete_failure(vm, &complete, "send", &error.to_string())?;
      }
      Err(error) => return Err(error),
    }
    Ok(Value::Undefined)
  });
  set(&primitive, "send", Value::Builtin(send));

  let close: BuiltinFn = Rc::new(|vm, args| {
    let socket = args.get(1).cloned().unwrap_or(Value::Undefined);
    let options = args.get(2).cloned().unwrap_or(Value::Undefined);
    let complete = args.get(3).cloned().unwrap_or(Value::Undefined);
    let code = vm.get_property_host(&options, &PropertyKey::String("code".to_string().into()))?;
    let reason =
      vm.get_property_host(&options, &PropertyKey::String("reason".to_string().into()))?;
    let method = vm.get_property_host(&socket, &PropertyKey::String("close".to_string().into()))?;
    let arguments = if matches!(code, Value::Undefined) {
      Vec::new()
    } else if matches!(reason, Value::Undefined) {
      vec![code]
    } else {
      vec![code, reason]
    };
    match vm.call_value_with_this(&method, &socket, &arguments) {
      Ok(_) if complete.is_callable() => complete_success(vm, &complete, Value::Undefined)?,
      Ok(_) => {}
      Err(error) if complete.is_callable() => {
        complete_failure(vm, &complete, "close", &error.to_string())?;
      }
      Err(error) => return Err(error),
    }
    Ok(Value::Undefined)
  });
  set(&primitive, "close", Value::Builtin(close));

  let subscribe: BuiltinFn = Rc::new(|vm, args| {
    let socket = args.get(1).cloned().unwrap_or(Value::Undefined);
    let on_event = args.get(2).cloned().unwrap_or(Value::Undefined);
    for (property, event_type) in [
      ("onmessage", "message"),
      ("onclose", "close"),
      ("onerror", "error"),
    ] {
      let callback = on_event.clone();
      let event_type = event_type.to_string();
      let listener: BuiltinFn = Rc::new(move |vm, args| {
        let source = args.get(1).cloned().unwrap_or(Value::Undefined);
        let event = object(vm.realm().object_prototype());
        if event_type == "message" {
          let data =
            vm.get_property_host(&source, &PropertyKey::String("data".to_string().into()))?;
          let kind = if matches!(data, Value::String(_)) {
            "text"
          } else {
            "binary"
          };
          set(&event, "type", Value::String(kind.to_string().into()));
          set(&event, "data", data);
        } else {
          set(&event, "type", Value::String(event_type.clone().into()));
          if event_type == "error" {
            set(&event, "error", source);
          }
        }
        vm.call_value_with_this(&callback, &Value::Undefined, &[event])?;
        Ok(Value::Undefined)
      });
      set_socket_handler(vm, &socket, property, Value::Builtin(listener))?;
    }
    let subscription = object(vm.realm().object_prototype());
    let cancel_socket = socket.clone();
    let cancel: BuiltinFn = Rc::new(move |vm, _args| {
      for property in ["onmessage", "onclose", "onerror"] {
        set_socket_handler(vm, &cancel_socket, property, Value::Undefined)?;
      }
      Ok(Value::Undefined)
    });
    set(&subscription, "__cancel", Value::Builtin(cancel));
    Ok(subscription)
  });
  set(&primitive, "subscribe", Value::Builtin(subscribe));

  let cancel: BuiltinFn = Rc::new(|vm, args| {
    let operation = args.get(1).cloned().unwrap_or(Value::Undefined);
    let cancel = vm.get_property_host(
      &operation,
      &PropertyKey::String("__cancel".to_string().into()),
    )?;
    if cancel.is_callable() {
      vm.call_value_with_this(&cancel, &operation, &[])?;
    }
    Ok(Value::Undefined)
  });
  set(&primitive, "cancel", Value::Builtin(cancel));
  if let Value::Object(object) = &primitive {
    object.borrow_mut().extensible = false;
  }
  primitive
}

fn set_socket_handler(
  vm: &mut Vm,
  socket: &Value,
  property: &str,
  value: Value,
) -> Result<(), RuntimeError> {
  vm.set_property_host(socket, &Value::String(property.to_string().into()), value)?;
  Ok(())
}

fn complete_success(
  vm: &mut Vm,
  callback: &Value,
  value: Value,
) -> Result<(), RuntimeError> {
  let result = object(vm.realm().object_prototype());
  set(&result, "ok", Value::Bool(true));
  set(&result, "value", value);
  vm.call_value_with_this(callback, &Value::Undefined, &[result])?;
  Ok(())
}

fn complete_failure(
  vm: &mut Vm,
  callback: &Value,
  operation: &str,
  message: &str,
) -> Result<(), RuntimeError> {
  let error = object(vm.realm().object_prototype());
  set(
    &error,
    "_tag",
    Value::String("WebSocketError".to_string().into()),
  );
  set(&error, "reason", Value::String("Host".to_string().into()));
  set(
    &error,
    "operation",
    Value::String(operation.to_string().into()),
  );
  set(&error, "message", Value::String(message.to_string().into()));
  let result = object(vm.realm().object_prototype());
  set(&result, "ok", Value::Bool(false));
  set(&result, "error", error);
  vm.call_value_with_this(callback, &Value::Undefined, &[result])?;
  Ok(())
}

fn terminal_primitive(
  vm: &Vm,
  terminal: Rc<RefCell<Option<terminal::TerminalState>>>,
) -> Value {
  let primitive = object(vm.realm().object_prototype());

  for name in ["write", "writeLine"] {
    let state = Rc::clone(&terminal);
    let method: BuiltinFn = Rc::new(move |vm, args| {
      let text = args.get(1).map_or_else(
        || Ok(String::new()),
        |value| {
          vm.to_string(value)
            .map(|value| value.to_string_lossy())
        },
      )?;
      let complete = args.get(2).cloned().unwrap_or(Value::Undefined);
      if let Some(terminal) = state.borrow().clone() {
        terminal.write(text);
        complete_success(vm, &complete, Value::Undefined)?;
      } else {
        complete_failure(vm, &complete, "write", "terminal host is not initialized")?;
      }
      Ok(Value::Undefined)
    });
    set(&primitive, name, Value::Builtin(method));
  }

  let prompt_state = Rc::clone(&terminal);
  let set_prompt: BuiltinFn = Rc::new(move |vm, args| {
    let prompt = args.get(1).map_or_else(
      || Ok(String::new()),
      |value| {
        vm.to_string(value)
          .map(|value| value.to_string_lossy())
      },
    )?;
    let complete = args.get(2).cloned().unwrap_or(Value::Undefined);
    if let Some(terminal) = prompt_state.borrow().clone() {
      terminal.set_prompt(prompt);
      complete_success(vm, &complete, Value::Undefined)?;
    } else {
      complete_failure(
        vm,
        &complete,
        "setPrompt",
        "terminal host is not initialized",
      )?;
    }
    Ok(Value::Undefined)
  });
  set(&primitive, "setPrompt", Value::Builtin(set_prompt));

  let subscribe_state = Rc::clone(&terminal);
  let subscribe: BuiltinFn = Rc::new(move |vm, args| {
    let on_event = args.get(1).cloned().unwrap_or(Value::Undefined);
    let Some(terminal) = subscribe_state.borrow().clone() else {
      return Err(RuntimeError::from("terminal host is not initialized"));
    };
    let callback_root = vm.create_host_root(on_event);
    let line: BuiltinFn = Rc::new(move |vm, args| {
      let event = object(vm.realm().object_prototype());
      set(&event, "type", Value::String("line".to_string().into()));
      set(
        &event,
        "line",
        args
          .get(1)
          .cloned()
          .unwrap_or(Value::String(String::new().into())),
      );
      if let Some(callback) = vm.host_root(callback_root) {
        vm.call_value_with_this(&callback, &Value::Undefined, &[event])?;
      }
      Ok(Value::Undefined)
    });
    let close_root = callback_root;
    let close: BuiltinFn = Rc::new(move |vm, _args| {
      let event = object(vm.realm().object_prototype());
      set(&event, "type", Value::String("close".to_string().into()));
      if let Some(callback) = vm.host_root(close_root) {
        vm.call_value_with_this(&callback, &Value::Undefined, &[event])?;
      }
      Ok(Value::Undefined)
    });
    terminal.subscribe_lines(vm, Value::Builtin(line))?;
    terminal.subscribe_close(vm, Value::Builtin(close))?;
    let subscription = object(vm.realm().object_prototype());
    let cancel_terminal = terminal.clone();
    let cancel: BuiltinFn = Rc::new(move |vm, _args| {
      cancel_terminal.clear_line_subscription(vm)?;
      vm.set_host_root(callback_root, Value::Undefined)?;
      Ok(Value::Undefined)
    });
    set(&subscription, "__cancel", Value::Builtin(cancel));
    Ok(subscription)
  });
  set(&primitive, "subscribeLines", Value::Builtin(subscribe));

  let close_state = Rc::clone(&terminal);
  let close: BuiltinFn = Rc::new(move |vm, args| {
    let complete = args.get(1).cloned().unwrap_or(Value::Undefined);
    if let Some(terminal) = close_state.borrow().clone() {
      terminal.close_and_notify(vm)?;
      complete_success(vm, &complete, Value::Undefined)?;
    } else {
      complete_failure(vm, &complete, "close", "terminal host is not initialized")?;
    }
    Ok(Value::Undefined)
  });
  set(&primitive, "close", Value::Builtin(close));

  let cancel: BuiltinFn = Rc::new(|vm, args| {
    let operation = args.get(1).cloned().unwrap_or(Value::Undefined);
    let cancel = vm.get_property_host(
      &operation,
      &PropertyKey::String("__cancel".to_string().into()),
    )?;
    if cancel.is_callable() {
      vm.call_value_with_this(&cancel, &operation, &[])?;
    }
    Ok(Value::Undefined)
  });
  set(&primitive, "cancel", Value::Builtin(cancel));
  if let Value::Object(object) = &primitive {
    object.borrow_mut().extensible = false;
  }
  primitive
}

/// Install the ABI object consumed by the prelinked private adapter.
///
/// Acquisition is lazy, expected validation/bind failures stay in the Effect
/// error channel, and scoped release closes the listener and active
/// connections idempotently. The transitional limitation remains that requests
/// are forwarded to the ordinary runtime event channel rather than invoking
/// the `blitz:app` router/layers; host capability validation does not hide it.
fn create_host_bridge(
  vm: &mut Vm,
  requests: mpsc::Sender<HttpRequestEvent>,
  websocket_events: mpsc::Sender<WebSocketEvent>,
  resources: ResourceRegistry,
  server: Rc<RefCell<Option<server::ServerState>>>,
  terminal: Rc<RefCell<Option<terminal::TerminalState>>>,
  node_file_system_allowed: bool,
  crypto_random_allowed: bool,
) -> (Value, HostBridgeCapabilities) {
  let app_transport = object(vm.realm().object_prototype());
  set(
    &app_transport,
    "openAppServer",
    Value::Builtin(open_app_server_method(
      requests,
      websocket_events,
      resources,
    )),
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
  set(&bridge, "fileSystem", fs::install(vm));
  set(
    &bridge,
    "nodeFileSystem",
    node_fs::install(vm, node_file_system_allowed),
  );
  set(
    &bridge,
    "nodeCryptoRandom",
    node_crypto::install(vm, crypto_random_allowed),
  );
  set(&bridge, "http", http_primitive(vm, Rc::clone(&server)));
  set(&bridge, "webSocket", websocket_primitive(vm, server));
  set(&bridge, "terminal", terminal_primitive(vm, terminal));
  if let Value::Object(object) = &bridge {
    object.borrow_mut().extensible = false;
  }
  let capabilities = HostBridgeCapabilities::new(HOST_BRIDGE_ABI_VERSION)
    .with_primitive(
      APP_TRANSPORT_PRIMITIVE,
      HOST_BRIDGE_ABI_VERSION,
      APP_TRANSPORT_METHODS.iter().copied(),
    )
    .with_primitive(
      FILE_SYSTEM_PRIMITIVE,
      HOST_BRIDGE_ABI_VERSION,
      FILE_SYSTEM_METHODS.iter().copied(),
    )
    .with_primitive(
      NODE_FILE_SYSTEM_PRIMITIVE,
      HOST_BRIDGE_ABI_VERSION,
      NODE_FILE_SYSTEM_METHODS.iter().copied(),
    )
    .with_primitive(
      NODE_CRYPTO_RANDOM_PRIMITIVE,
      NODE_CRYPTO_RANDOM_ABI_VERSION,
      NODE_CRYPTO_RANDOM_METHODS.iter().copied(),
    )
    .with_primitive(
      HTTP_PRIMITIVE,
      HOST_BRIDGE_ABI_VERSION,
      HTTP_METHODS.iter().copied(),
    )
    .with_primitive(
      TERMINAL_PRIMITIVE,
      HOST_BRIDGE_ABI_VERSION,
      TERMINAL_METHODS.iter().copied(),
    )
    .with_primitive(
      WEB_SOCKET_PRIMITIVE,
      HOST_BRIDGE_ABI_VERSION,
      WEB_SOCKET_METHODS.iter().copied(),
    );
  (bridge, capabilities)
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
      PropertyKey::String(name.to_string().into()),
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
  Ok(slot.borrow().clone())
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
  let hostname_value =
    vm.get_property_host(value, &PropertyKey::String("hostname".to_string().into()))?;
  let hostname = vm.to_string(&hostname_value)?;
  let port_value = vm.get_property_host(value, &PropertyKey::String("port".to_string().into()))?;
  let port_number = vm.to_number_value(&port_value)?.trunc();
  if !port_number.is_finite() || !(0.0_f64..=f64::from(u16::MAX)).contains(&port_number) {
    return Err(RuntimeError::from("application transport port is invalid"));
  }
  let port = port_number
    .to_string()
    .parse::<u16>()
    .map_err(|_| RuntimeError::from("application transport port is invalid"))?;
  let rpc_path_value =
    vm.get_property_host(value, &PropertyKey::String("rpcPath".to_string().into()))?;
  let rpc_path = vm.to_string(&rpc_path_value)?.to_string_lossy();
  Ok(AppTransportOptions {
    hostname: hostname.to_string_lossy(),
    port,
    rpc_path,
  })
}

fn serve_error_value(
  vm: &Vm,
  reason: &str,
  message: String,
) -> Value {
  let error = object(vm.realm().object_prototype());
  set(
    &error,
    "_tag",
    Value::String("ServeError".to_string().into()),
  );
  set(&error, "reason", Value::String(reason.to_string().into()));
  set(&error, "message", Value::String(message.into()));
  error
}

fn effect_fail(
  vm: &mut Vm,
  error: Value,
) -> Result<Value, RuntimeError> {
  let fail = effect_function(vm, "fail")?;
  vm.call_value_with_this(&fail, &Value::Undefined, &[error])
}

fn effect_succeed(
  vm: &mut Vm,
  value: Value,
) -> Result<Value, RuntimeError> {
  let succeed = effect_function(vm, "succeed")?;
  vm.call_value_with_this(&succeed, &Value::Undefined, &[value])
}

fn effect_suspend(
  vm: &mut Vm,
  thunk: BuiltinFn,
) -> Result<Value, RuntimeError> {
  let suspend = effect_function(vm, "suspend")?;
  vm.call_value_with_this(&suspend, &Value::Undefined, &[Value::Builtin(thunk)])
}

fn open_app_server_method(
  requests: mpsc::Sender<HttpRequestEvent>,
  websocket_events: mpsc::Sender<WebSocketEvent>,
  resources: ResourceRegistry,
) -> BuiltinFn {
  Rc::new(move |vm, args| {
    let options = match args.get(1) {
      Some(value) => match transport_options(vm, value) {
        Ok(options) => options,
        Err(error) => {
          let error = serve_error_value(vm, "InvalidOptions", error.to_string());
          return effect_fail(vm, error);
        }
      },
      None => {
        let error = serve_error_value(
          vm,
          "InvalidOptions",
          "openAppServer requires options".to_string(),
        );
        return effect_fail(vm, error);
      }
    };
    let thunk: BuiltinFn = {
      let requests = requests.clone();
      let websocket_events = websocket_events.clone();
      let resources = resources.clone();
      Rc::new(move |vm, _args| {
        match vm.block_on(open_app_server(
          options.clone(),
          requests.clone(),
          websocket_events.clone(),
        ))? {
          Ok(resource) => {
            let resource_handle =
              resources.register(format!("app-server:{}", resource.local_addr()), true);
            let resource = resource_value(vm, Arc::new(resource), resource_handle);
            effect_succeed(vm, resource)
          }
          Err(error) => {
            let reason = match error {
              AppTransportError::InvalidOptions(_) => "InvalidOptions",
              AppTransportError::Bind(_) => "Bind",
            };
            let error = serve_error_value(vm, reason, error.to_string());
            effect_fail(vm, error)
          }
        }
      })
    };
    effect_suspend(vm, thunk)
  })
}

fn resource_value(
  vm: &mut Vm,
  resource: Arc<AppServerResource>,
  resource_handle: ResourceHandle,
) -> Value {
  let server = object(vm.realm().object_prototype());
  set(
    &server,
    "localAddr",
    Value::String(resource.local_addr().to_string().into()),
  );
  let close_resource = Arc::clone(&resource);
  let close: BuiltinFn = Rc::new(move |vm, _args| {
    vm.block_on(Arc::clone(&close_resource).close_owned())?;
    resource_handle.close();
    Ok(Value::Undefined)
  });
  set(&server, "__blitz_close", Value::Builtin(close));
  server
}

fn close_app_server_method() -> BuiltinFn {
  Rc::new(|vm, args| {
    let server = args
      .get(1)
      .ok_or_else(|| RuntimeError::from("closeAppServer requires a server"))?;
    let close = vm.get_property_host(
      server,
      &PropertyKey::String("__blitz_close".to_string().into()),
    )?;
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
    let close = vm.get_property_host(
      server,
      &PropertyKey::String("__blitz_close".to_string().into()),
    )?;
    if !matches!(close, Value::Builtin(_)) {
      return Err(RuntimeError::from("invalid application server resource"));
    }
    effect_function(vm, "never")
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
  fn deno_surface_manifest_is_well_formed_and_tracks_server_lifecycle() {
    let manifest: serde_json::Value =
      serde_json::from_str(include_str!("../../compat/deno-surface.json"))
        .expect("parse Deno surface manifest");
    assert_eq!(manifest["schemaVersion"], 1);
    assert_eq!(manifest["target"], "Deno 2.9.3");
    let members = manifest["members"]
      .as_array()
      .expect("surface members are an array");
    let mut paths = std::collections::BTreeSet::new();
    for member in members {
      let path = member["path"].as_str().expect("surface path");
      assert!(paths.insert(path), "duplicate surface path: {path}");
      assert!(matches!(
        member["status"].as_str(),
        Some("compatible" | "partial" | "extension" | "planned")
      ));
    }
    for required in [
      "Blitz.serve().addr",
      "Blitz.serve().finished",
      "Blitz.serve().shutdown",
      "Blitz.serve().ref",
      "Blitz.serve().unref",
    ] {
      assert!(
        paths.contains(required),
        "missing surface member: {required}"
      );
    }

    let declarations = include_str!("../../types/blitz.d.ts");
    for declaration in [
      "readonly addr: BlitzNetAddr",
      "readonly finished: Promise<void>",
      "shutdown(): Promise<void>",
      "ref(): void",
      "unref(): void",
    ] {
      assert!(
        declarations.contains(declaration),
        "missing host declaration: {declaration}"
      );
    }
  }

  #[test]
  fn compatibility_globals_are_disabled_by_default() {
    let globals = Rc::new(RefCell::new(ObjectData::new()));
    let mut vm = Vm::new(globals);
    let options = RuntimeOptions::from_host(vec!["blitz-test".to_string()]);
    let _runtime = install_runtime(&mut vm, options);
    assert_eq!(vm.get_global("Blitz"), None);
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
  fn private_host_bridge_is_not_exposed_on_the_global_object() {
    let globals = Rc::new(RefCell::new(ObjectData::new()));
    let mut vm = Vm::new(Rc::clone(&globals));
    let _runtime = install_runtime(
      &mut vm,
      RuntimeOptions::from_host(vec!["blitz-test".to_string()]),
    );
    assert_eq!(vm.get_global("__blitz_host_v1"), None);
    assert!(
      !globals
        .borrow()
        .properties
        .contains_key(&PropertyKey::String("__blitz_host_v1".to_string().into()))
    );
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
        install_compatibility_globals: false,
        node_compat: NodeCompatProfile::Core,
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
  fn timer_globals_dispatch_callbacks_and_track_liveness() {
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
        install_compatibility_globals: false,
        node_compat: NodeCompatProfile::Core,
      },
    );
    let source = blitz::js!(
      r#"
      globalThis.timerResult = 0;
      globalThis.clockIsMonotonic = performance.now() >= 0;
      const cancelled = setInterval(() => { timerResult = -1; }, 1000);
      clearInterval(cancelled);
      clearInterval(cancelled);
      setTimeout((left, right) => { timerResult = left + right; }, 1, 2, 3);
    "#
    );
    let session = JsParser::parse_source(source).expect("parse timer script");
    let chunk = compiler::compile_oxc(session.program(), source).expect("compile timer script");
    vm.run(chunk).expect("evaluate timer script");

    assert!(runtime.is_live());
    assert!(
      runtime
        .run_once(&mut vm)
        .expect("dispatch timer callback")
    );
    assert_eq!(vm.get_global("timerResult"), Some(Value::Number(5.0)));
    assert_eq!(vm.get_global("clockIsMonotonic"), Some(Value::Bool(true)));
    assert!(!runtime.is_live());
  }

  #[test]
  fn blitz_serve_returns_a_lifecycle_server_handle() {
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
        install_compatibility_globals: true,
        node_compat: NodeCompatProfile::Core,
      },
    );
    let source = blitz::js!(
      r#"
      globalThis.serverFinished = false;
      globalThis.server = Blitz.serve(
        { hostname: "127.0.0.1", port: 0 },
        () => new Response("ok")
      );
      server.finished.then(() => { serverFinished = true; });
      globalThis.serverShape =
        server.addr.transport === "tcp" &&
        server.addr.hostname === server.hostname &&
        server.addr.port === server.port &&
        typeof server.shutdown === "function" &&
        typeof server.ref === "function" &&
        typeof server.unref === "function";
    "#
    );
    let session = JsParser::parse_source(source).expect("parse server handle script");
    let chunk =
      compiler::compile_oxc(session.program(), source).expect("compile server handle script");
    vm.run(chunk)
      .expect("evaluate server handle script");
    let address = runtime
      .local_addr()
      .expect("bound server address");
    assert_eq!(vm.get_global("serverShape"), Some(Value::Bool(true)));
    assert!(runtime.is_live());

    let source = blitz::js!(
      r#"
      server.unref();
      globalThis.unrefCalled = true;
    "#
    );
    let session = JsParser::parse_source(source).expect("parse server unref script");
    let chunk = compiler::compile_oxc(session.program(), source).expect("compile unref script");
    vm.run(chunk).expect("unref server");
    assert_eq!(vm.get_global("unrefCalled"), Some(Value::Bool(true)));
    assert!(!runtime.is_live());

    let source = blitz::js!(
      r#"
      server.ref();
      globalThis.shutdownIdentity = server.shutdown() === server.finished;
      server.shutdown();
    "#
    );
    let session = JsParser::parse_source(source).expect("parse server shutdown script");
    let chunk = compiler::compile_oxc(session.program(), source).expect("compile shutdown script");
    vm.run(chunk).expect("shut down server");
    vm.flush_microtasks()
      .expect("settle server completion");
    assert_eq!(vm.get_global("shutdownIdentity"), Some(Value::Bool(true)));
    assert_eq!(vm.get_global("serverFinished"), Some(Value::Bool(true)));
    assert!(!runtime.is_live());

    let rebound =
      std::net::TcpListener::bind(address).expect("released server port can be rebound");
    drop(rebound);
  }

  #[test]
  fn deno_server_lifecycle_fixture_matches_blitz() {
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
        install_compatibility_globals: true,
        node_compat: NodeCompatProfile::Core,
      },
    );
    let source = format!(
      "{}\nobserveServerLifecycle(Blitz).then((result) => {{\n\
       globalThis.compatTransport = result.transport;\n\
       globalThis.compatPort = result.portIsAssigned;\n\
       globalThis.compatShutdown = result.shutdownReturnsPromise;\n\
       globalThis.compatFinished = result.finishedSettled;\n\
       }});",
      include_str!("../../compat/fixtures/server-lifecycle.js")
    );
    let session = JsParser::parse_source(&source).expect("parse Deno compatibility fixture");
    let chunk =
      compiler::compile_oxc(session.program(), &source).expect("compile compatibility fixture");
    vm.run(chunk)
      .expect("evaluate compatibility fixture");
    vm.flush_microtasks()
      .expect("settle compatibility fixture");

    assert_eq!(
      vm.get_global("compatTransport"),
      Some(Value::String("tcp".to_string().into()))
    );
    assert_eq!(vm.get_global("compatPort"), Some(Value::Bool(true)));
    assert_eq!(vm.get_global("compatShutdown"), Some(Value::Bool(true)));
    assert_eq!(vm.get_global("compatFinished"), Some(Value::Bool(true)));
    assert!(!runtime.is_live());
  }

  #[test]
  fn effect_http_serves_an_effect_handler_without_blitz_globals() {
    thread::Builder::new()
      .name("effect-http-host".to_string())
      .stack_size(16 * 1024 * 1024)
      .spawn(|| {
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
            install_compatibility_globals: false,
            node_compat: NodeCompatProfile::Core,
          },
        );
        let directory = env::temp_dir().join(format!("blitz-effect-http-{}", std::process::id()));
        std::fs::create_dir_all(&directory).expect("create Effect HTTP directory");
        let entry = directory.join("entry.mjs");
        std::fs::write(
          &entry,
          blitz::js!(
            r#"
        import * as Effect from "effect/Effect";
        import * as Http from "blitz:http";
        import * as Runtime from "blitz:runtime";

        Runtime.runMain(Http.serve({
          hostname: "127.0.0.1",
          port: 0,
          handler: () => Effect.succeed(new Response("effect-http"))
        }));
      "#
          ),
        )
        .expect("write Effect HTTP entry");
        let loader = vm.module_loader_handle();
        {
          let mut loader = loader.borrow_mut();
          loader
            .construct(&directory, "./entry.mjs")
            .expect("construct Effect HTTP graph");
          loader
            .instantiate_all()
            .expect("instantiate Effect HTTP graph");
        }
        blitz::module::evaluate_entry_shared(&loader, &entry, &mut vm, false)
          .expect("evaluate Effect HTTP entry");
        assert_eq!(vm.get_global("Blitz"), None);

        let address = runtime
          .local_addr()
          .expect("Effect HTTP listener address");
        let client = thread::spawn(move || {
          let mut stream = TcpStream::connect(address).expect("connect Effect HTTP client");
          stream
            .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .expect("write Effect HTTP request");
          let mut response = String::new();
          stream
            .read_to_string(&mut response)
            .expect("read Effect HTTP response");
          response
        });
        assert!(
          runtime
            .run_once(&mut vm)
            .expect("dispatch Effect HTTP request")
        );
        let response = client.join().expect("Effect HTTP client thread");
        assert!(response.ends_with("effect-http"), "{response}");
        runtime
          .shutdown(&mut vm)
          .expect("shutdown Effect HTTP server");
        std::fs::remove_dir_all(directory).expect("remove Effect HTTP directory");
      })
      .expect("spawn Effect HTTP host thread")
      .join()
      .expect("Effect HTTP host thread succeeds");
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
        install_compatibility_globals: true,
        node_compat: NodeCompatProfile::Core,
      },
    );
    let source = blitz::js!(
      r#"
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
    "#
    );
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
    runtime
      .shutdown(&mut vm)
      .expect("stop HTTP server");
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
        install_compatibility_globals: true,
        node_compat: NodeCompatProfile::Core,
      },
    );
    let source = blitz::js!(
      r#"
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
    "#
    );
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
    runtime
      .shutdown(&mut vm)
      .expect("stop HTTP server");
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
        install_compatibility_globals: true,
        node_compat: NodeCompatProfile::Core,
      },
    );
    let source = blitz::js!(
      r#"
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
    "#
    );
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
      if vm.get_global("clientResult") == Some(Value::String("echo:hello".to_string().into())) {
        break;
      }
    }
    assert_eq!(
      vm.get_global("clientResult"),
      Some(Value::String("echo:hello".to_string().into()))
    );
    runtime
      .shutdown(&mut vm)
      .expect("stop client and server");
  }

  #[test]
  fn effect_irc_chat_v2_scripts_parse_and_compile() {
    let server = include_str!("../../../../examples/irc-chat-v2/server.ts");
    let session =
      JsParser::parse_source_with_type(server, oxc_span::SourceType::ts().with_module(true))
        .expect("parse Effect IRC server module");
    compiler::compile_oxc(session.program(), server).expect("compile Effect IRC server module");

    for (name, source) in [
      (
        "alice",
        include_str!("../../../../examples/irc-chat-v2/alice.ts"),
      ),
      (
        "bob",
        include_str!("../../../../examples/irc-chat-v2/bob.ts"),
      ),
    ] {
      let session =
        JsParser::parse_source_with_type(source, oxc_span::SourceType::ts().with_module(true))
          .unwrap_or_else(|error| panic!("parse Effect IRC {name} client: {error}"));
      compiler::compile_oxc(session.program(), source)
        .unwrap_or_else(|error| panic!("compile Effect IRC {name} client: {error}"));
    }
  }

  #[test]
  fn effect_irc_chat_v2_server_runs_join_and_message_protocol() {
    thread::Builder::new()
      .name("effect-irc-v2".to_string())
      .stack_size(16 * 1024 * 1024)
      .spawn(|| {
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
            install_compatibility_globals: false,
            node_compat: NodeCompatProfile::Core,
          },
        );
        let source = include_str!("../../../../examples/irc-chat-v2/server.ts")
          .replace("port: 8000", "port: 0");
        let directory = env::temp_dir().join(format!("blitz-effect-irc-v2-{}", std::process::id()));
        std::fs::create_dir_all(&directory).expect("create Effect IRC directory");
        let entry = directory.join("server.ts");
        std::fs::write(&entry, source).expect("write Effect IRC server");
        let loader = vm.module_loader_handle();
        {
          let mut loader = loader.borrow_mut();
          loader
            .construct(&directory, "./server.ts")
            .expect("construct Effect IRC graph");
          loader
            .instantiate_all()
            .expect("instantiate Effect IRC graph");
        }
        blitz::module::evaluate_entry_shared(&loader, &entry, &mut vm, false)
          .expect("evaluate Effect IRC server");
        assert_eq!(vm.get_global("Blitz"), None);

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
              async fn connect(
                address: SocketAddr
              ) -> fastwebsockets::WebSocket<TokioIo<Upgraded>> {
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
              let alice_join = alice.read_frame().await.expect("read Alice join");

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
              let bob_join = bob
                .read_frame()
                .await
                .expect("read Bob join on Bob");

              alice
                .write_frame(Frame::text(Payload::Owned(b"MSG hello".to_vec())))
                .await
                .expect("send Alice message");
              let alice_message = alice
                .read_frame()
                .await
                .expect("read Alice message on Alice");
              let bob_message = bob
                .read_frame()
                .await
                .expect("read Alice message on Bob");
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
                alice_join,
                bob_welcome,
                alice_sees_bob,
                bob_join,
                alice_message,
                bob_message,
              )
            })
        });

        let mut event_count = 0;
        while !client.is_finished() {
          assert!(
            runtime
              .run_once(&mut vm)
              .expect("drive IRC event")
          );
          event_count += 1;
          assert!(event_count < 100, "IRC protocol did not quiesce");
        }
        let (
          alice_welcome,
          alice_join,
          bob_welcome,
          alice_sees_bob,
          bob_join,
          alice_message,
          bob_message,
        ) = client.join().expect("IRC client thread");
        assert_eq!(
          alice_welcome.payload.as_ref(),
          b"* Welcome, Alice. Type /quit to leave."
        );
        assert_eq!(
          bob_welcome.payload.as_ref(),
          b"* Welcome, Bob. Type /quit to leave."
        );
        assert_eq!(alice_join.payload.as_ref(), b"* Alice joined the room");
        assert_eq!(alice_sees_bob.payload.as_ref(), b"* Bob joined the room");
        assert_eq!(bob_join.payload.as_ref(), b"* Bob joined the room");
        assert_eq!(alice_message.payload.as_ref(), b"<Alice> hello");
        assert_eq!(bob_message.payload.as_ref(), b"<Alice> hello");
        runtime
          .shutdown(&mut vm)
          .expect("stop IRC server");
        std::fs::remove_dir_all(directory).expect("remove Effect IRC directory");
      })
      .expect("spawn Effect IRC thread")
      .join()
      .expect("Effect IRC thread succeeds");
  }
}
