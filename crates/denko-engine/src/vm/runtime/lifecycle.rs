// SPDX-License-Identifier: MPL-2.0

use crate::bytecode::{Chunk, OP_COUNT, Op};
use crate::environment::EnvironmentRecord;
use crate::errors::RuntimeError;
use crate::gc;
use crate::module::ModuleLoader;
use crate::realm::Realm;
use crate::runtime_utils::array_length_descriptor;
use crate::value::{
  ArrayData, ObjectData, PromiseState, PropertyDescriptor, PropertyKey, SharedValue, Value,
};
use crate::vm::Vm;
use crate::vm::runtime::property::get_property;
use crate::vm::state::*;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;
use std::sync::Arc;

impl Vm {
  fn gc_trace_enabled() -> bool {
    std::env::var_os("TSBLITZ_GC_TRACE").is_some() || crate::trace::enabled()
  }

  pub(super) fn prune_stale_handlers(&mut self) {
    let active_frames = self.execution.call_stack.len();
    while self
      .exceptions
      .handlers
      .last()
      .is_some_and(|h| h.frame_count > active_frames)
    {
      self.exceptions.handlers.pop();
    }
  }

  // -----------------------------------------------------------------------
  // Construction & public helpers
  // -----------------------------------------------------------------------

  /// Create a new virtual machine.
  pub fn new(globals: Rc<RefCell<ObjectData>>) -> Self {
    let module_loader = shared_module_loader_for(&globals);
    Self::new_with_module_loader(globals, module_loader)
  }

  /// Create a VM in an existing module registry.
  ///
  /// A nested module evaluation must retain the parent realm's registry and
  /// its selected public/private graph. Keeping this handle explicit avoids
  /// making dynamic import depend on reconstructing host policy from globals.
  pub(crate) fn new_with_module_loader(
    globals: Rc<RefCell<ObjectData>>,
    module_loader: Rc<RefCell<ModuleLoader>>,
  ) -> Self {
    let gc_registration = gc::VmGcRegistration::new();
    let intrinsics = crate::runtime_utils::install_minimal_kernel_constructors(&globals);
    let realm = Realm::with_globals(Rc::clone(&globals));
    let global_env = Rc::new(RefCell::new(EnvironmentRecord::new(None)));
    let mut vm = Self {
      realm_state: RealmState {
        realm,
        globals: Rc::clone(&globals),
        global_env: Rc::clone(&global_env),
      },
      execution: ExecutionState {
        active: ExecutionContext {
          chunk: Arc::new(Chunk::new(Arc::new(String::new()))),
          ip: 0,
          locals: Vec::with_capacity(LOCALS_INITIAL_CAPACITY),
          this_value: Value::Object(globals),
          closure_env: None,
          slots: None,
          lexical_env: global_env,
          yield_star: None,
          arguments: ArgumentsState {
            object: None,
            mapped_count: 0,
          },
          strict: false,
        },
        interpreter_depth: 0,
        builtin_this_arg_prepended: false,
        pending_closure_env: None,
        call_stack: CallStack::with_capacity(FRAMES_INITIAL_CAPACITY),
        root_chunk: None,
        direct_eval_outer: None,
        generator_return_frame_count: None,
      },
      exceptions: ExceptionState {
        handlers: HandlerStack::with_capacity(32),
        pending: None,
        pending_frame_count: None,
        unwound_risc_state: None,
        last_uncaught: None,
      },
      construction: ConstructionState {
        new_target: None,
        recent_return_value: None,
        derived_this: Vec::new(),
      },
      modules: ModuleState {
        loader: module_loader,
        table: Vec::new(),
        current_specifier: None,
      },
      jobs: JobState {
        microtasks: VecDeque::new(),
        dynamic_imports: VecDeque::new(),
        unhandled_rejection_hook: None,
      },
      caches: CacheState {
        property_get: HashMap::new(),
        property_get_epoch: 0,
        quickened: HashMap::new(),
        active_quickened_chunk: 0,
        active_quickened_sites: Arc::from([]),
        active_regions: Arc::from([]),
        active_direct_calls: Rc::new(RefCell::new(Vec::new())),
        optimized_execution_enabled: true,
        native_regions_enabled: std::env::var_os("DENKO_DISABLE_NATIVE_REGIONS").is_none(),
      },
      profiling: ProfilingState {
        enabled: std::env::var("BLITZ_VM_PROFILE")
          .ok()
          .is_some_and(|v| v != "0" && !v.eq_ignore_ascii_case("false")),
        opcode_counts: [0; OP_COUNT],
        run_started_at: None,
        functions: HashMap::new(),
        chunk_ids: HashMap::new(),
        chunk_refs: HashMap::new(),
        next_function_id: 0,
        active_function_chunk: 0,
        active_function_id: None,
        last_hot_chunk: 0,
        hot_chunks: std::collections::HashSet::new(),
        jit: crate::vm::jit::JitRuntime::default(),
        jit_stats: crate::vm::jit::JitStats::default(),
      },
      roots: RootState {
        temporary: Vec::new(),
        kept_alive: Vec::new(),
        host: Rc::new(RefCell::new(HostRootStore::new())),
      },
      interrupts: crate::vm::interrupt::VmInterruptState::new(),
      _gc_registration: gc_registration,
    };
    vm.populate_realm_intrinsics(intrinsics);
    vm
  }

  /// Fork a new VM for nested execution while preserving the current realm.
  ///
  /// This is used for `eval`/`Function`-style dynamic execution that must
  /// observe the same realm intrinsics and globals as the caller.
  pub(crate) fn fork_execution(&self) -> Self {
    let gc_registration = gc::VmGcRegistration::new();
    let globals = Rc::clone(&self.realm_state.globals);
    let this_value = self
      .realm_state
      .globals
      .borrow()
      .properties
      .get(&PropertyKey::String("globalThis".to_string().into()))
      .and_then(|d| match d {
        PropertyDescriptor::Data { value, .. } => Some(value.clone()),
        _ => None,
      })
      .unwrap_or(Value::Undefined);
    Self {
      realm_state: RealmState {
        realm: self.realm_state.realm.clone(),
        globals,
        global_env: Rc::clone(&self.realm_state.global_env),
      },
      execution: ExecutionState {
        active: ExecutionContext {
          chunk: Arc::new(Chunk::new(Arc::new(String::new()))),
          ip: 0,
          locals: Vec::with_capacity(LOCALS_INITIAL_CAPACITY),
          this_value,
          closure_env: None,
          slots: None,
          lexical_env: Rc::clone(&self.execution.active.lexical_env),
          yield_star: None,
          arguments: ArgumentsState {
            object: None,
            mapped_count: 0,
          },
          strict: self.execution.active.strict,
        },
        interpreter_depth: 0,
        builtin_this_arg_prepended: false,
        pending_closure_env: None,
        call_stack: CallStack::with_capacity(FRAMES_INITIAL_CAPACITY),
        root_chunk: None,
        direct_eval_outer: None,
        generator_return_frame_count: None,
      },
      exceptions: ExceptionState {
        handlers: HandlerStack::with_capacity(32),
        pending: None,
        pending_frame_count: None,
        unwound_risc_state: None,
        last_uncaught: None,
      },
      construction: ConstructionState {
        new_target: None,
        recent_return_value: None,
        derived_this: Vec::new(),
      },
      modules: ModuleState {
        loader: Rc::clone(&self.modules.loader),
        table: Vec::new(),
        current_specifier: None,
      },
      jobs: JobState {
        microtasks: VecDeque::new(),
        dynamic_imports: VecDeque::new(),
        unhandled_rejection_hook: self.jobs.unhandled_rejection_hook.clone(),
      },
      caches: CacheState {
        property_get: HashMap::new(),
        property_get_epoch: 0,
        quickened: HashMap::new(),
        active_quickened_chunk: 0,
        active_quickened_sites: Arc::from([]),
        active_regions: Arc::from([]),
        active_direct_calls: Rc::new(RefCell::new(Vec::new())),
        optimized_execution_enabled: true,
        native_regions_enabled: std::env::var_os("DENKO_DISABLE_NATIVE_REGIONS").is_none(),
      },
      profiling: ProfilingState {
        enabled: self.profiling.enabled,
        opcode_counts: [0; OP_COUNT],
        run_started_at: None,
        functions: HashMap::new(),
        chunk_ids: HashMap::new(),
        chunk_refs: HashMap::new(),
        next_function_id: 0,
        active_function_chunk: 0,
        active_function_id: None,
        last_hot_chunk: 0,
        hot_chunks: std::collections::HashSet::new(),
        jit: crate::vm::jit::JitRuntime::default(),
        jit_stats: crate::vm::jit::JitStats::default(),
      },
      roots: RootState {
        temporary: Vec::new(),
        kept_alive: Vec::new(),
        host: Rc::clone(&self.roots.host),
      },
      interrupts: self.interrupts.fork(),
      _gc_registration: gc_registration,
    }
  }

  /// Retain a value that is owned by a host integration rather than the JS object graph.
  pub fn create_host_root(
    &mut self,
    value: Value,
  ) -> HostRoot {
    let mut store = self.roots.host.borrow_mut();
    let slot = if let Some(slot) = store.free.pop() {
      store.slots[slot].value = Some(value);
      slot
    } else {
      let slot = store.slots.len();
      store.slots.push(HostRootSlot {
        generation: 0,
        value: Some(value),
      });
      slot
    };
    HostRoot {
      store_id: store.id,
      slot,
      generation: store.slots[slot].generation,
    }
  }

  /// Replace a retained host value.
  pub fn set_host_root(
    &mut self,
    root: HostRoot,
    value: Value,
  ) -> Result<(), RuntimeError> {
    let mut store = self.roots.host.borrow_mut();
    if root.store_id != store.id {
      return Err(RuntimeError::from("Invalid host root"));
    }
    let slot = store
      .slots
      .get_mut(root.slot)
      .filter(|slot| slot.generation == root.generation && slot.value.is_some())
      .ok_or_else(|| RuntimeError::from("Invalid host root"))?;
    slot.value = Some(value);
    Ok(())
  }

  /// Read a retained host value.
  pub fn host_root(
    &self,
    root: HostRoot,
  ) -> Option<Value> {
    let store = self.roots.host.borrow();
    (root.store_id == store.id)
      .then(|| store.slots.get(root.slot))
      .flatten()
      .filter(|slot| slot.generation == root.generation)
      .and_then(|slot| slot.value.clone())
  }

  /// Release a retained host value and invalidate its handle.
  pub fn release_host_root(
    &mut self,
    root: HostRoot,
  ) -> Result<(), RuntimeError> {
    let mut store = self.roots.host.borrow_mut();
    if root.store_id != store.id {
      return Err(RuntimeError::from("Invalid host root"));
    }
    let slot = store
      .slots
      .get_mut(root.slot)
      .filter(|slot| slot.generation == root.generation && slot.value.is_some())
      .ok_or_else(|| RuntimeError::from("Invalid host root"))?;
    slot.value = None;
    slot.generation = slot.generation.wrapping_add(1);
    store.free.push(root.slot);
    Ok(())
  }

  pub(crate) fn share_host_roots_from(
    &mut self,
    other: &Self,
  ) {
    self.roots.host = Rc::clone(&other.roots.host);
  }

  pub(super) fn begin_opcode_profile_run(&mut self) {
    if self.profiling.enabled {
      self.profiling.opcode_counts = [0; OP_COUNT];
      self.profiling.run_started_at = Some(std::time::Instant::now());
    }
  }

  pub(super) fn finish_opcode_profile_run(
    &mut self,
    outcome: &str,
  ) {
    if !self.profiling.enabled {
      return;
    }

    let elapsed = self
      .profiling
      .run_started_at
      .take()
      .map(|t| t.elapsed())
      .unwrap_or_default();
    let total: u64 = self.profiling.opcode_counts.iter().sum();
    eprintln!(
      "[vm-profile] outcome={} elapsed={:?} total_ops={}",
      outcome, elapsed, total
    );
    if total == 0 {
      return;
    }

    let mut ranked: Vec<(Op, u64)> = self
      .profiling
      .opcode_counts
      .iter()
      .enumerate()
      .filter_map(|(index, &count)| {
        if count == 0 {
          None
        } else {
          // Safe: index is from 0..OP_COUNT which is guaranteed <= u8::MAX
          let index = u8::try_from(index).expect("index from opcode_counts exceeds u8::MAX");
          Op::from_byte(index).map(|op| (op, count))
        }
      })
      .collect();
    ranked.sort_by_key(|b| std::cmp::Reverse(b.1));

    for (op, count) in ranked.into_iter().take(16) {
      // Precision loss acceptable for percentage display calculation
      let pct = ((count as i64) as f64 / (total as i64) as f64) * 100.0;
      eprintln!("[vm-profile] {:>16} {:>12} {:>6.2}%", op.name(), count, pct);
    }
  }

  /// Return the current `this` value.
  pub fn this_value(&self) -> Value {
    self.execution.active.this_value.clone()
  }

  /// Create a thread-safe handle that can request termination of this VM.
  pub fn interrupt_handle(&self) -> crate::vm::VmInterruptHandle {
    self.interrupts.handle()
  }

  pub(super) fn poll_execution_interrupt(&mut self) -> Result<(), RuntimeError> {
    if self.interrupts.poll_and_acknowledge() {
      Err(RuntimeError::execution_interrupted())
    } else {
      Ok(())
    }
  }

  /// Return whether the current host builtin call includes `this` at `args[0]`.
  pub fn builtin_this_arg_prepended(&self) -> bool {
    self.execution.builtin_this_arg_prepended
  }

  /// Return whether a JavaScript exception is currently unwinding to a handler.
  pub(crate) fn has_pending_exception(&self) -> bool {
    self.exceptions.pending.is_some()
  }

  /// Close an iterator from a host callback without disturbing its pending
  /// language-level exception.
  pub(crate) fn close_iterator_in_fork(
    &self,
    iterator: &Value,
  ) -> Result<(), RuntimeError> {
    let mut child = self.fork_execution();
    let return_method =
      child.get_property_host(iterator, &PropertyKey::String("return".to_string().into()))?;
    if matches!(return_method, Value::Undefined | Value::Null) {
      return Ok(());
    }
    if !Self::is_callable_value(&return_method) {
      return Err(child.throw_type_error("Iterator return is not callable"));
    }
    let result = child.call_value_with_this(&return_method, iterator, &[])?;
    if !result.is_object() {
      return Err(child.throw_type_error("Iterator return result is not an object"));
    }
    Ok(())
  }

  /// Return the current ECMAScript execution context's `[[NewTarget]]` value.
  pub fn current_new_target(&self) -> Option<Value> {
    self
      .execution
      .call_stack
      .last()
      .and_then(|frame| frame.new_target.clone())
      .or_else(|| self.construction.new_target.clone())
  }

  /// Return the `new.target` pending for a host constructor call.
  pub(crate) fn pending_constructor_new_target(&self) -> Option<Value> {
    self.construction.new_target.clone()
  }

  /// Return the dynamic eval context flags for the currently executing code.
  ///
  /// `new.target` is valid inside any function-like execution context.
  /// `super` is valid only when the current frame has a home object.
  pub(crate) fn current_eval_context_flags(&self) -> (bool, bool) {
    (
      self.execution.active.closure_env.is_some(),
      self.current_method_home_object().is_some(),
    )
  }

  pub(super) fn get_new_target(&self) -> Value {
    if let Some(crate::value::Value::Closure { proto, props, .. }) = self
      .execution
      .call_stack
      .last()
      .map(|frame| &frame.callee)
      && proto.captures_this
      && let Some(value) = props
        .borrow()
        .internal_slot("[[LexicalNewTarget]]")
    {
      return value.clone();
    }
    self
      .current_new_target()
      .unwrap_or(Value::Undefined)
  }

  pub(super) fn get_this(&self) -> Value {
    self.execution.active.this_value.clone()
  }

  pub(super) fn closure_lexical_this(callee: &Value) -> Option<Value> {
    match callee {
      Value::Closure {
        lexical_this: Some(this),
        ..
      } => Some((**this).clone()),
      _ => None,
    }
  }

  pub(super) fn current_method_home_object(&self) -> Option<Value> {
    let frame = self.execution.call_stack.last()?;
    match &frame.callee {
      Value::Closure { props, .. } | Value::BuiltinObject(_, props) => {
        let props = props.borrow();
        props
          .properties
          .get(&PropertyKey::String("[[HomeObject]]".to_string().into()))
          .and_then(|desc| match desc {
            PropertyDescriptor::Data { value, .. } => Some(value.clone()),
            PropertyDescriptor::Accessor { .. } => None,
          })
      }
      _ => None,
    }
  }

  /// Return the prototype of a value.
  ///
  /// Returns `Value::Null` if the value has no prototype (e.g. `Object.prototype`).
  pub fn get_prototype(
    &self,
    v: &Value,
  ) -> Value {
    match v {
      Value::Object(obj)
      | Value::Array(_, obj)
      | Value::Closure { props: obj, .. }
      | Value::BuiltinObject(_, obj) => obj.borrow().get_prototype(),
      Value::Proxy(proxy) => {
        let proxy = proxy.borrow();
        if proxy.is_revoked() {
          Value::Null
        } else {
          self.get_prototype(&proxy.target)
        }
      }
      Value::Promise(_, properties) => properties.borrow().get_prototype(),
      Value::String(_) => self
        .realm_state
        .realm
        .intrinsics
        .string_prototype
        .clone()
        .unwrap_or(Value::Null),
      Value::Number(_) => self
        .realm_state
        .realm
        .intrinsics
        .number_prototype
        .clone()
        .unwrap_or(Value::Null),
      Value::Bool(_) => self
        .realm_state
        .realm
        .intrinsics
        .boolean_prototype
        .clone()
        .unwrap_or(Value::Null),
      Value::Symbol(_, _) => self
        .realm_state
        .realm
        .intrinsics
        .symbol_prototype
        .clone()
        .unwrap_or(Value::Null),
      Value::BigInt(_) => self
        .realm_state
        .realm
        .intrinsics
        .bigint_prototype
        .clone()
        .unwrap_or(Value::Null),
      Value::Null | Value::Undefined | Value::Uninitialized => Value::Null,
      Value::Builtin(_) => self
        .realm_state
        .realm
        .intrinsics
        .function_prototype
        .clone()
        .unwrap_or(Value::Null),
    }
  }

  /// Resolve `Promise.prototype` from the realm's intrinsic table or from
  /// the current global constructor as a fallback.
  pub(crate) fn promise_prototype_value(&self) -> Value {
    if let Some(proto) = self
      .realm_state
      .realm
      .intrinsics
      .promise_prototype
      .clone()
      && !matches!(proto, Value::Null | Value::Undefined)
    {
      return proto;
    }

    if let Some(ctor) = self.realm_state.realm.get_global("Promise")
      && let Ok(proto) = get_property(
        &ctor,
        &PropertyKey::String("prototype".to_string().into()),
        0,
      )
      && !matches!(proto, Value::Null | Value::Undefined)
    {
      return proto;
    }

    Value::Null
  }

  /// Wrap promise state in an ordinary object using the supplied prototype.
  pub(crate) fn promise_value_with_prototype(
    &self,
    state: Rc<RefCell<PromiseState>>,
    prototype: Value,
  ) -> Value {
    let properties = gc::track_object(Rc::new(RefCell::new(ObjectData::with_prototype(prototype))));
    Value::Promise(state, properties)
  }

  /// Wrap promise state in an object using this realm's Promise prototype.
  pub(crate) fn promise_value(
    &self,
    state: Rc<RefCell<PromiseState>>,
  ) -> Value {
    self.promise_value_with_prototype(state, self.promise_prototype_value())
  }

  /// Create a new Array object with the given elements and correct prototype.
  pub fn new_array(
    &self,
    elems: Vec<Option<Value>>,
  ) -> Value {
    let arr_rc = gc::track_array(Rc::new(RefCell::new(ArrayData::from(elems))));
    let props = gc::track_object(Rc::new(RefCell::new(ObjectData::new())));
    let len = arr_rc.borrow().len();
    props.borrow_mut().properties.insert(
      PropertyKey::String("length".to_string().into()),
      array_length_descriptor(len),
    );

    if let Some(proto) = self
      .realm_state
      .realm
      .intrinsics
      .array_prototype
      .clone()
    {
      props.borrow_mut().set_prototype_value(proto);
    }

    Value::Array(arr_rc, props)
  }

  /// Run the prototype mark/sweep collector and return mark statistics.
  pub fn collect_garbage_prototype(&mut self) -> gc::GcStats {
    // The current prototype GC cannot inspect captures held inside queued
    // host-side microtask/import-job closures. Collecting while those queues
    // are non-empty can reclaim still-live JS values reachable only through
    // those opaque Rust closures, leading to stalled async work.
    if !self.jobs.microtasks.is_empty() || !self.jobs.dynamic_imports.is_empty() {
      if Self::gc_trace_enabled() {
        eprintln!(
          "[gc] skip collect: pending_jobs microtasks={} import_jobs={}",
          self.jobs.microtasks.len(),
          self.jobs.dynamic_imports.len()
        );
      }
      return gc::GcStats {
        deferred: Some(gc::GcDeferredReason::PendingJobs),
        ..gc::GcStats::default()
      };
    }

    let before = if Self::gc_trace_enabled() {
      Some(gc::heap_snapshot())
    } else {
      None
    };

    let mut roots: Vec<Value> = Vec::new();
    let mut env_roots: Vec<Rc<RefCell<Vec<SharedValue>>>> = Vec::new();
    roots.push(Value::Object(Rc::clone(&self.realm_state.globals)));
    roots.extend(
      self
        .execution
        .active
        .locals
        .iter()
        .map(|c| c.borrow().clone()),
    );
    roots.push(self.execution.active.this_value.clone());
    if let Some(value) = &self.construction.new_target {
      roots.push(value.clone());
    }
    if let Some(value) = &self.execution.active.arguments.object {
      roots.push(value.clone());
    }
    if let Some(value) = &self.construction.recent_return_value {
      roots.push(value.clone());
    }
    roots.extend(
      self
        .construction
        .derived_this
        .iter()
        .filter_map(Clone::clone),
    );
    if let Some(value) = &self.exceptions.last_uncaught {
      roots.push(value.clone());
    }
    if let Some(value) = &self.exceptions.pending {
      roots.push(value.clone());
    }
    if let Some(state) = &self.exceptions.unwound_risc_state {
      roots.extend(state.registers.iter().cloned());
      roots.extend(state.spills.iter().cloned());
    }
    if let Some(state) = &self.execution.active.yield_star {
      roots.push(state.iterator.clone());
      roots.push(state.next_method.clone());
    }
    if let Some(env) = &self.execution.active.closure_env {
      env_roots.push(Rc::clone(env));
      roots.extend(
        env
          .borrow()
          .iter()
          .map(|cell| cell.borrow().clone()),
      );
    }
    if let Some(env) = &self.execution.pending_closure_env {
      env_roots.push(Rc::clone(env));
      roots.extend(
        env
          .borrow()
          .iter()
          .map(|cell| cell.borrow().clone()),
      );
    }
    roots.extend(self.roots.temporary.iter().cloned());
    roots.extend(self.roots.kept_alive.iter().cloned());
    roots.extend(
      self
        .roots
        .host
        .borrow()
        .slots
        .iter()
        .filter_map(|slot| slot.value.clone()),
    );
    roots.extend(
      self
        .caches
        .property_get
        .values()
        .map(|entry| entry.value.clone()),
    );
    self
      .execution
      .active
      .lexical_env
      .borrow()
      .collect_binding_values(&mut roots);
    self
      .realm_state
      .global_env
      .borrow()
      .collect_binding_values(&mut roots);
    for frame in self.execution.call_stack.iter() {
      roots.extend(
        frame
          .context
          .locals
          .iter()
          .map(|c| c.borrow().clone()),
      );
      roots.push(frame.context.this_value.clone());
      roots.push(frame.callee.clone());
      if let Some(state) = &frame.risc_state {
        roots.extend(state.registers.iter().cloned());
        roots.extend(state.spills.iter().cloned());
      }
      if let Some(value) = &frame.new_target {
        roots.push(value.clone());
      }
      if let Some(value) = &frame.context.arguments.object {
        roots.push(value.clone());
      }
      if let Some(state) = &frame.context.yield_star {
        roots.push(state.iterator.clone());
        roots.push(state.next_method.clone());
      }
      if let Some(env) = &frame.context.closure_env {
        env_roots.push(Rc::clone(env));
        roots.extend(
          env
            .borrow()
            .iter()
            .map(|cell| cell.borrow().clone()),
        );
      }
      frame
        .context
        .lexical_env
        .borrow()
        .collect_binding_values(&mut roots);
    }
    for module in &self.modules.table {
      roots.extend(module.values().map(|cell| cell.borrow().clone()));
    }
    if let Some(value) = self
      .realm_state
      .realm
      .intrinsics
      .object_constructor
      .clone()
    {
      roots.push(value);
    }
    if let Some(value) = self
      .realm_state
      .realm
      .intrinsics
      .array_constructor
      .clone()
    {
      roots.push(value);
    }
    if let Some(value) = self
      .realm_state
      .realm
      .intrinsics
      .function_constructor
      .clone()
    {
      roots.push(value);
    }
    if let Some(value) = self
      .realm_state
      .realm
      .intrinsics
      .string_constructor
      .clone()
    {
      roots.push(value);
    }
    if let Some(value) = self
      .realm_state
      .realm
      .intrinsics
      .symbol_constructor
      .clone()
    {
      roots.push(value);
    }
    if let Some(value) = self
      .realm_state
      .realm
      .intrinsics
      .error_constructor
      .clone()
    {
      roots.push(value);
    }
    if let Some(value) = self
      .realm_state
      .realm
      .intrinsics
      .type_error_constructor
      .clone()
    {
      roots.push(value);
    }
    if let Some(value) = self
      .realm_state
      .realm
      .intrinsics
      .range_error_constructor
      .clone()
    {
      roots.push(value);
    }
    if let Some(value) = self
      .realm_state
      .realm
      .intrinsics
      .reference_error_constructor
      .clone()
    {
      roots.push(value);
    }
    if let Some(value) = self
      .realm_state
      .realm
      .intrinsics
      .syntax_error_constructor
      .clone()
    {
      roots.push(value);
    }
    if let Some(value) = self
      .realm_state
      .realm
      .intrinsics
      .promise_constructor
      .clone()
    {
      roots.push(value);
    }
    if let Some(value) = self
      .realm_state
      .realm
      .intrinsics
      .date_constructor
      .clone()
    {
      roots.push(value);
    }
    if let Some(value) = self
      .realm_state
      .realm
      .intrinsics
      .map_constructor
      .clone()
    {
      roots.push(value);
    }
    if let Some(value) = self
      .realm_state
      .realm
      .intrinsics
      .weak_map_constructor
      .clone()
    {
      roots.push(value);
    }
    if let Some(value) = self
      .realm_state
      .realm
      .intrinsics
      .weak_set_constructor
      .clone()
    {
      roots.push(value);
    }
    if let Some(value) = self
      .realm_state
      .realm
      .intrinsics
      .number_constructor
      .clone()
    {
      roots.push(value);
    }
    if let Some(value) = self
      .realm_state
      .realm
      .intrinsics
      .object_prototype
      .clone()
    {
      roots.push(value);
    }
    if let Some(value) = self
      .realm_state
      .realm
      .intrinsics
      .array_prototype
      .clone()
    {
      roots.push(value);
    }
    if let Some(value) = self
      .realm_state
      .realm
      .intrinsics
      .function_prototype
      .clone()
    {
      roots.push(value);
    }
    if let Some(value) = self
      .realm_state
      .realm
      .intrinsics
      .string_prototype
      .clone()
    {
      roots.push(value);
    }
    if let Some(value) = self
      .realm_state
      .realm
      .intrinsics
      .symbol_prototype
      .clone()
    {
      roots.push(value);
    }
    if let Some(value) = self
      .realm_state
      .realm
      .intrinsics
      .error_prototype
      .clone()
    {
      roots.push(value);
    }
    if let Some(value) = self
      .realm_state
      .realm
      .intrinsics
      .promise_prototype
      .clone()
    {
      roots.push(value);
    }
    if let Some(value) = self
      .realm_state
      .realm
      .intrinsics
      .date_prototype
      .clone()
    {
      roots.push(value);
    }
    if let Some(value) = self
      .realm_state
      .realm
      .intrinsics
      .map_prototype
      .clone()
    {
      roots.push(value);
    }
    if let Some(value) = self
      .realm_state
      .realm
      .intrinsics
      .weak_map_prototype
      .clone()
    {
      roots.push(value);
    }
    if let Some(value) = self
      .realm_state
      .realm
      .intrinsics
      .weak_set_prototype
      .clone()
    {
      roots.push(value);
    }
    if let Some(value) = self
      .realm_state
      .realm
      .intrinsics
      .boolean_prototype
      .clone()
    {
      roots.push(value);
    }
    if let Some(value) = self
      .realm_state
      .realm
      .intrinsics
      .number_prototype
      .clone()
    {
      roots.push(value);
    }
    if let Some(value) = self
      .realm_state
      .realm
      .intrinsics
      .bigint_prototype
      .clone()
    {
      roots.push(value);
    }
    if let Some(value) = self
      .realm_state
      .realm
      .intrinsics
      .eval_error_constructor
      .clone()
    {
      roots.push(value);
    }
    if let Some(value) = self
      .realm_state
      .realm
      .intrinsics
      .uri_error_constructor
      .clone()
    {
      roots.push(value);
    }
    let stats = gc::collect_with_env_roots(&roots, &env_roots);
    for registry in gc::take_pending_finalization_cleanups() {
      self.queue_finalization_cleanup(registry);
    }
    if Self::gc_trace_enabled() {
      let after = gc::heap_snapshot();
      if let Some(before) = before {
        eprintln!(
          "[gc] collect roots={} env_roots={} marked={} swept(o={}, a={}, e={}, p={}) pressure={} -> {} live(o={}, a={}, e={}, p={}) -> (o={}, a={}, e={}, p={})",
          roots.len(),
          env_roots.len(),
          stats.marked_nodes,
          stats.swept_objects,
          stats.swept_arrays,
          stats.swept_envs,
          stats.swept_promises,
          before.allocation_pressure,
          after.allocation_pressure,
          before.live_objects,
          before.live_arrays,
          before.live_envs,
          before.live_promises,
          after.live_objects,
          after.live_arrays,
          after.live_envs,
          after.live_promises,
        );
      }
    }
    stats
  }

  /// Opportunistically collect garbage during interpreter progress.
  ///
  /// This keeps the prototype collector aggressive without collecting on
  /// every single bytecode step.
  pub(super) fn maybe_collect_garbage_prototype(&mut self) {
    let pressure = gc::allocation_pressure();
    let threshold = gc_allocation_pressure_threshold();
    if pressure < threshold {
      return;
    }
    // Opportunistic prototype-GC is only safe at quiescent points where
    // live JS values are fully represented in the VM root sets. Nested JS
    // calls and pending host jobs still keep additional opaque temporaries.
    if !self.execution.call_stack.is_empty()
      || !self.jobs.microtasks.is_empty()
      || !self.jobs.dynamic_imports.is_empty()
      || self.execution.pending_closure_env.is_some()
    {
      if Self::gc_trace_enabled() {
        eprintln!(
          "[gc] skip maybe_collect pressure={} threshold={} call_stack={} microtasks={} import_jobs={} pending_closure_env={} temp_roots={}",
          pressure,
          threshold,
          self.execution.call_stack.len(),
          self.jobs.microtasks.len(),
          self.jobs.dynamic_imports.len(),
          self.execution.pending_closure_env.is_some(),
          self.roots.temporary.len(),
        );
      }
      return;
    }
    let _ = self.collect_garbage_prototype();
  }

  /// Retain the compatibility builder; `Vm::new` performs initialization.
  pub fn with_runtime(self) -> Self {
    self
  }

  /// Store the kernel-provided intrinsic table on the Realm record.
  pub fn reset(
    &mut self,
    keep_globals: &HashSet<String>,
  ) {
    self.execution.active.locals.clear();
    self.execution.call_stack.clear();
    self.execution.active.strict = self.execution.active.chunk.root_strict;
    self.exceptions.handlers.clear();
    self.exceptions.pending = None;
    self.exceptions.pending_frame_count = None;
    self.exceptions.unwound_risc_state = None;
    self.execution.root_chunk = None;
    self.execution.active.slots = None;
    self.execution.direct_eval_outer = None;
    self.execution.active.this_value = self
      .realm_state
      .globals
      .borrow()
      .properties
      .get(&PropertyKey::String("globalThis".to_string().into()))
      .and_then(|d| match d {
        PropertyDescriptor::Data { value, .. } => Some(value.clone()),
        _ => None,
      })
      .unwrap_or(Value::Undefined);
    self.execution.active.closure_env = None;
    self.execution.pending_closure_env = None;
    self.roots.temporary.clear();
    self.execution.active.lexical_env = Rc::clone(&self.realm_state.global_env);
    self.execution.active.yield_star = None;
    self.execution.generator_return_frame_count = None;
    self.construction.new_target = None;
    self.construction.recent_return_value = None;
    self.construction.derived_this.clear();
    self.exceptions.last_uncaught = None;
    self.execution.active.arguments.object = None;
    self.execution.active.arguments.mapped_count = 0;
    self.execution.active.ip = 0;
    self.execution.active.chunk = Arc::new(Chunk::new(Arc::new(String::new())));
    self.caches.property_get.clear();
    gc::reset_allocation_pressure();
    let mut g = self.realm_state.globals.borrow_mut();
    g.properties.retain(
      |k, _| matches!(k, PropertyKey::String(s) if keep_globals.contains(s.as_str_lossy())),
    );
  }
}
