// SPDX-License-Identifier: MPL-2.0

use super::*;

/// Destination selected by a runtime console operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutputStream {
  Stdout,
  Stderr,
}

/// Console output adapter used by hosted embedders.
pub trait Output {
  /// Write one complete console line.
  fn write_line(
    &self,
    stream: OutputStream,
    line: &str,
  ) -> Result<(), RuntimeError>;

  /// Clear an interactive output device when supported.
  fn clear(&self) -> Result<(), RuntimeError> {
    Ok(())
  }
}

/// Process stdout/stderr adapter used by the Blitz executable.
#[derive(Clone, Copy, Debug, Default)]
pub struct ProcessOutput;

impl Output for ProcessOutput {
  fn write_line(
    &self,
    stream: OutputStream,
    line: &str,
  ) -> Result<(), RuntimeError> {
    let result = match stream {
      OutputStream::Stdout => writeln!(io::stdout().lock(), "{line}"),
      OutputStream::Stderr => writeln!(io::stderr().lock(), "{line}"),
    };
    result.map_err(|error| RuntimeError::from(format!("console output failed: {error}")))
  }

  fn clear(&self) -> Result<(), RuntimeError> {
    let mut stdout = io::stdout().lock();
    stdout
      .write_all(b"\x1b[2J\x1b[H")
      .and_then(|()| stdout.flush())
      .map_err(|error| RuntimeError::from(format!("console output failed: {error}")))
  }
}

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
  const CORE_MODULES: &'static [&'static str] = &[
    "node:assert",
    "node:assert/strict",
    "node:async_hooks",
    "node:buffer",
    "node:console",
    "node:constants",
    "node:diagnostics_channel",
    "node:events",
    "node:http",
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
    "node:http",
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
    "node:http",
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
  /// Additional package export conditions used by source module resolution.
  pub conditions: Vec<String>,
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
      conditions: Vec::new(),
    }
  }
}

/// VM-thread driver for runtime-owned asynchronous resources.
pub struct RuntimeHandle {
  pub(super) server: server::ServerState,
  pub(super) events: EventRegistry<RuntimeEvent>,
  pub(super) root: RootSupervisor,
  pub(super) scheduler: SchedulerHostState,
  pub(super) terminal: terminal::TerminalState,
}

pub(super) enum RuntimeEvent {
  Http(Box<HttpRequestEvent>),
  WebSocket(WebSocketEvent),
  Terminal(TerminalEvent),
  Wakeup(WakeupEvent),
}

pub(super) struct TimerCallback {
  pub(super) callback: HostRoot,
  pub(super) arguments: Vec<HostRoot>,
}

#[derive(Clone)]
pub(super) struct SchedulerHostState {
  pub(super) scheduler: Rc<RefCell<TimerScheduler<RuntimeEvent>>>,
  pub(super) callbacks: Rc<RefCell<HashMap<TimerId, TimerCallback>>>,
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

  pub(super) fn clear(
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
  vm.release_host_root(callback.callback)?;
  for root in &callback.arguments {
    vm.release_host_root(*root)?;
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
    self.dispatch_event(vm, event)?;
    Ok(true)
  }

  /// Process at most one event, waiting no longer than `timeout`.
  ///
  /// This lets interactive embedders interleave their own input source with
  /// timers and other runtime resources without moving the VM off its thread.
  pub fn run_once_timeout(
    &mut self,
    vm: &mut Vm,
    timeout: Duration,
  ) -> Result<bool, RuntimeError> {
    if !self.server.has_resources() && !self.terminal.is_active() && !self.events.is_live() {
      return Ok(false);
    }
    let Some(mut receiver) = self.events.take_receiver() else {
      return Ok(false);
    };
    let (receiver, event) = vm.block_on(async move {
      let event = tokio::time::timeout(timeout, receiver.recv())
        .await
        .ok()
        .flatten();
      (receiver, event)
    })?;
    self.events.put_receiver(receiver);
    let Some(event) = event else {
      return Ok(false);
    };
    self.dispatch_event(vm, event)?;
    Ok(true)
  }

  fn dispatch_event(
    &mut self,
    vm: &mut Vm,
    event: RuntimeEvent,
  ) -> Result<(), RuntimeError> {
    match event {
      RuntimeEvent::Http(event) => self.server.dispatch(vm, *event)?,
      RuntimeEvent::WebSocket(event) => self.server.dispatch_websocket(vm, event)?,
      RuntimeEvent::Terminal(event) => self.terminal.dispatch(vm, event)?,
      RuntimeEvent::Wakeup(event) => self.scheduler.dispatch(vm, event)?,
    }
    vm.flush_microtasks()?;
    Ok(())
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
    vm.flush_microtasks()?;
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
    let _interrupted = self.root.request_interrupt(vm, 1)?;
    let _finished = self.server.close_current(vm)?;
    Ok(())
  }

  /// Request interruption of the active Effect root.
  ///
  /// Returns `false` when no running root can accept the request.
  ///
  /// # Errors
  ///
  /// Returns a runtime error when the root's interruption callback throws.
  pub fn interrupt_root(
    &self,
    vm: &mut Vm,
    status: i32,
  ) -> Result<bool, RuntimeError> {
    self.root.request_interrupt(vm, status)
  }

  /// Return the exit status recorded after the Effect root settles.
  #[must_use]
  pub fn exit_status(&self) -> Option<i32> {
    self.root.exit_status()
  }
}
