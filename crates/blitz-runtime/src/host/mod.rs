// SPDX-License-Identifier: MPL-2.0
//! Operating-system host APIs for the Blitz runtime.
//!
//! The ECMAScript engine is intentionally host-agnostic. This module is the
//! boundary where the Blitz executable installs capabilities supplied by the
//! operating system, such as process metadata, filesystem access, and async
//! I/O. `NodeCompatProfile` controls visible `node:*` roots and their paired
//! filesystem/entropy primitives. The remaining Blitz host modules are
//! installed together and are not a general-purpose sandbox policy.

mod abort;
mod bridge;
mod console;
pub mod event_loop;
mod fs;
mod node_crypto;
mod node_fs;
mod primitives;
mod process;
mod root;
pub mod scheduler;
mod server;
mod state;
mod terminal;
mod url;

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::env;
use std::io::{self, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use denko::errors::RuntimeError;
use denko::gc;
use denko::module::PrelinkedHostModule;
use denko::value::{BuiltinFn, ObjectData, PropertyDescriptor, PropertyKey, Value};
use denko::{HostRoot, Vm};
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
  NODE_FILE_SYSTEM_METHODS, NODE_FILE_SYSTEM_PRIMITIVE, RUNTIME_CONTROL_METHODS,
  RUNTIME_CONTROL_PRIMITIVE, RuntimeGraphError, TERMINAL_METHODS, TERMINAL_PRIMITIVE,
  WEB_SOCKET_METHODS, WEB_SOCKET_PRIMITIVE,
};
use event_loop::{EventRegistry, EventSender, ResourceHandle, ResourceRegistry};
use root::RootSupervisor;
use scheduler::{TimerId, TimerKind, TimerScheduler, WakeupEvent};
use terminal::TerminalEvent;

pub use state::{
  NodeCompatProfile, Output, OutputStream, ProcessOutput, RuntimeHandle, RuntimeOptions,
};
use state::{RuntimeEvent, SchedulerHostState, TimerCallback};

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

/// Install the runtime with a caller-selected console output adapter.
pub fn install_runtime_with_output(
  vm: &mut Vm,
  options: RuntimeOptions,
  output: Rc<dyn Output>,
) -> RuntimeHandle {
  try_install_runtime_with_output(vm, options, output)
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
  try_install_runtime_with_output(vm, options, Rc::new(ProcessOutput))
}

/// Fallible installation with an embedder-selected console output adapter.
pub fn try_install_runtime_with_output(
  vm: &mut Vm,
  options: RuntimeOptions,
  output: Rc<dyn Output>,
) -> Result<RuntimeHandle, RuntimeInstallError> {
  let loader_handle = vm.module_loader_handle();
  if loader_handle
    .borrow()
    .contains_prelinked_specifier("blitz-internal:host")
  {
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
  let root = RootSupervisor::new(events.resources());
  let node_file_system_allowed = options
    .node_compat
    .allowed_modules()
    .iter()
    .any(|module| matches!(*module, "node:fs" | "node:fs/promises"));
  let crypto_random_allowed = options
    .node_compat
    .allowed_modules()
    .contains(&"node:crypto");
  let (host_bridge, capabilities) = create_host_bridge(
    vm,
    HostBridgeInputs {
      requests: requests_tx.clone(),
      websocket_events: websocket_events_tx.clone(),
      resources: events.resources(),
      server: Rc::clone(&server_slot),
      terminal: Rc::clone(&terminal_slot),
      root: root.clone(),
      node_file_system_allowed,
      crypto_random_allowed,
    },
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
  loader_handle
    .borrow_mut()
    .set_condition_names(options.conditions.clone());
  let graph_registration = registration_started.elapsed();
  if timing_enabled {
    eprintln!(
      "blitz runtime graph: decode+generic-validation={decode_and_validation:?} policy-validation={policy_validation:?} registration={graph_registration:?}"
    );
  }
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
  console::install(vm, output);
  abort::install(vm);
  process::install(vm, options);
  url::install(vm);
  Ok(RuntimeHandle {
    server,
    events,
    root,
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
    1,
    timer_set_method(state.clone(), TimerKind::Timeout),
  );
  install_global_function(
    vm,
    "setInterval",
    1,
    timer_set_method(state.clone(), TimerKind::Interval),
  );
  install_global_function(vm, "clearTimeout", 1, timer_clear_method(state.clone()));
  install_global_function(vm, "clearInterval", 1, timer_clear_method(state.clone()));

  let performance = object(vm.realm().object_prototype());
  let now_state = state.clone();
  let now: BuiltinFn = Rc::new(move |_vm, _args| {
    Ok(Value::Number(
      now_state.scheduler.borrow().clock().now_millis(),
    ))
  });
  set(&performance, "now", host_function(vm, "now", 0, now));
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
  length: u32,
  function: BuiltinFn,
) {
  vm.realm()
    .global()
    .borrow_mut()
    .properties
    .insert(
      PropertyKey::String(name.to_string().into()),
      PropertyDescriptor::Data {
        value: host_function(vm, name, length, function),
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
    const MAX_TIMER_ARGUMENTS: usize = 1_024;
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
    if args.len().saturating_sub(callback_index + 2) > MAX_TIMER_ARGUMENTS {
      return Err(RuntimeError::from("timer argument capacity is exhausted"));
    }
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

use bridge::{HostBridgeInputs, create_host_bridge};
use bridge::{effect_fail, effect_function, effect_succeed, effect_suspend, object, set};
pub(crate) use bridge::{host_constructor, host_function};
use primitives::{http_primitive, runtime_primitive, terminal_primitive, websocket_primitive};

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
mod tests;
