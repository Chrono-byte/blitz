#![allow(trivial_numeric_casts)]
// SPDX-License-Identifier: MPL-2.0

use crate::bytecode::{
  Chunk, OP_COUNT, Op, decode_aux8, decode_compare_jump, decode_rd, decode_risc_aux16,
  decode_risc_cond_offset, decode_rs,
};
use crate::environment::EnvironmentRecord;
use crate::errors::RuntimeError;
use crate::gc;
use crate::module::ModuleLoader;
use crate::realm::{Intrinsics, Realm};
use crate::runtime_utils::{
  array_from_values, array_length_descriptor, js_number_to_string, number_exponentiate,
  usize_to_f64,
};
use crate::value::{
  ArrayData, BuiltinFn, ObjectData, PromiseState, PropertyDescriptor, PropertyKey, SharedValue,
  SymbolId, Value,
};
use crate::vm::runtime::coercions::{parse_js_bigint_string, parse_js_number_string};
use crate::vm::state::*;
use num_bigint::BigInt as BigIntValue;
use num_traits::{FromPrimitive, ToPrimitive, Zero};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;
use std::sync::Arc;

enum YieldStarStep {
  Yield(Value),
  Complete(Value),
  Return(Value),
}

impl Vm {
  fn gc_trace_enabled() -> bool {
    std::env::var_os("TSBLITZ_GC_TRACE").is_some() || crate::trace::enabled()
  }

  pub(super) fn prune_stale_handlers(&mut self) {
    let active_frames = self.call_stack.len();
    while self
      .handlers
      .last()
      .is_some_and(|h| h.frame_count > active_frames)
    {
      self.handlers.pop();
    }
  }

  /// Clear the property get inline cache.
  ///
  /// This is a blunt invalidation used when allocating fresh objects whose
  /// addresses may be recycled by `Rc`. It avoids stale cache hits across
  /// distinct allocations that happen to reuse the same pointer value.
  pub(crate) fn clear_property_get_ic_cache(&mut self) {
    self.property_get_ic.clear();
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
    let intrinsics = crate::runtime_utils::install_minimal_kernel_constructors(&globals);
    let realm = Realm::with_globals(Rc::clone(&globals));
    let global_env = Rc::new(RefCell::new(EnvironmentRecord::new(None)));
    let mut vm = Self {
      realm,
      chunk: Arc::new(Chunk::new(Arc::new(String::new()))),
      ip: 0,
      locals: Vec::with_capacity(LOCALS_INITIAL_CAPACITY),
      this_value: Value::Object(Rc::clone(&globals)),
      builtin_this_arg_prepended: false,
      current_closure_env: None,
      pending_closure_env: None,
      gc_temp_roots: Vec::new(),
      host_roots: Vec::new(),
      globals,
      call_stack: CallStack::with_capacity(FRAMES_INITIAL_CAPACITY),
      root_chunk: None,
      current_slot_types: None,
      current_slot_names: None,
      current_slot_immutable: None,
      current_slot_sloppy_immutable_silent: None,
      direct_eval_outer_slot_names: None,
      direct_eval_outer_slot_immutable: None,
      direct_eval_outer_slot_sloppy_immutable_silent: None,
      obj_generations: HashMap::new(),
      property_get_ic: HashMap::new(),
      bg_runtime: get_global_bg_runtime(),
      handlers: HandlerStack::with_capacity(32),
      pending_exception: None,
      pending_exception_frame_count: None,
      unwound_risc_state: None,
      module_loader,
      module_table: Vec::new(),
      current_module_specifier: None,
      construct_new_target: None,
      recent_return_value: None,
      derived_constructor_this: Vec::new(),
      last_uncaught_exception: None,
      microtasks: VecDeque::new(),
      dynamic_import_jobs: VecDeque::new(),
      unhandled_rejection_hook: None,
      lexical_env: Rc::clone(&global_env),
      global_env,
      yield_star_state: None,
      generator_return_frame_count: None,
      current_arguments_object: None,
      current_arguments_mapped_count: 0,
      current_strict_mode: false,
      opcode_profiling_enabled: std::env::var("BLITZ_VM_PROFILE")
        .ok()
        .is_some_and(|v| v != "0" && !v.eq_ignore_ascii_case("false")),
      opcode_counts: [0; OP_COUNT],
      run_started_at: None,
    };
    vm.populate_realm_intrinsics(intrinsics);
    gc::reset_allocation_pressure();
    vm
  }

  /// Fork a new VM for nested execution while preserving the current realm.
  ///
  /// This is used for `eval`/`Function`-style dynamic execution that must
  /// observe the same realm intrinsics and globals as the caller.
  pub(crate) fn fork_execution(&self) -> Self {
    let globals = Rc::clone(&self.globals);
    let this_value = self
      .globals
      .borrow()
      .properties
      .get(&PropertyKey::String("globalThis".to_string()))
      .and_then(|d| match d {
        PropertyDescriptor::Data { value, .. } => Some(value.clone()),
        _ => None,
      })
      .unwrap_or(Value::Undefined);
    Self {
      realm: self.realm.clone(),
      chunk: Arc::new(Chunk::new(Arc::new(String::new()))),
      ip: 0,
      locals: Vec::with_capacity(LOCALS_INITIAL_CAPACITY),
      this_value,
      builtin_this_arg_prepended: false,
      current_closure_env: None,
      pending_closure_env: None,
      gc_temp_roots: Vec::new(),
      host_roots: self.host_roots.clone(),
      globals,
      call_stack: CallStack::with_capacity(FRAMES_INITIAL_CAPACITY),
      root_chunk: None,
      current_slot_types: None,
      current_slot_names: None,
      current_slot_immutable: None,
      current_slot_sloppy_immutable_silent: None,
      direct_eval_outer_slot_names: None,
      direct_eval_outer_slot_immutable: None,
      direct_eval_outer_slot_sloppy_immutable_silent: None,
      obj_generations: HashMap::new(),
      property_get_ic: HashMap::new(),
      bg_runtime: Arc::clone(&self.bg_runtime),
      handlers: HandlerStack::with_capacity(32),
      pending_exception: None,
      pending_exception_frame_count: None,
      unwound_risc_state: None,
      module_loader: Rc::clone(&self.module_loader),
      module_table: Vec::new(),
      current_module_specifier: None,
      construct_new_target: None,
      recent_return_value: None,
      derived_constructor_this: Vec::new(),
      last_uncaught_exception: None,
      microtasks: VecDeque::new(),
      dynamic_import_jobs: VecDeque::new(),
      unhandled_rejection_hook: self.unhandled_rejection_hook.clone(),
      lexical_env: Rc::clone(&self.lexical_env),
      global_env: Rc::clone(&self.global_env),
      yield_star_state: None,
      generator_return_frame_count: None,
      current_arguments_object: None,
      current_arguments_mapped_count: 0,
      current_strict_mode: self.current_strict_mode,
      opcode_profiling_enabled: self.opcode_profiling_enabled,
      opcode_counts: [0; OP_COUNT],
      run_started_at: None,
    }
  }

  /// Retain a value that is owned by a host integration rather than the JS object graph.
  pub fn create_host_root(
    &mut self,
    value: Value,
  ) -> HostRoot {
    let root = HostRoot(self.host_roots.len());
    self.host_roots.push(value);
    root
  }

  /// Replace a retained host value.
  pub fn set_host_root(
    &mut self,
    root: HostRoot,
    value: Value,
  ) -> Result<(), RuntimeError> {
    let slot = self
      .host_roots
      .get_mut(root.0)
      .ok_or_else(|| RuntimeError::from("Invalid host root"))?;
    *slot = value;
    Ok(())
  }

  /// Read a retained host value.
  pub fn host_root(
    &self,
    root: HostRoot,
  ) -> Option<Value> {
    self.host_roots.get(root.0).cloned()
  }

  fn begin_opcode_profile_run(&mut self) {
    if self.opcode_profiling_enabled {
      self.opcode_counts = [0; OP_COUNT];
      self.run_started_at = Some(std::time::Instant::now());
    }
  }

  fn finish_opcode_profile_run(
    &mut self,
    outcome: &str,
  ) {
    if !self.opcode_profiling_enabled {
      return;
    }

    let elapsed = self
      .run_started_at
      .take()
      .map(|t| t.elapsed())
      .unwrap_or_default();
    let total: u64 = self.opcode_counts.iter().sum();
    eprintln!(
      "[vm-profile] outcome={} elapsed={:?} total_ops={}",
      outcome, elapsed, total
    );
    if total == 0 {
      return;
    }

    let mut ranked: Vec<(Op, u64)> = self
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
    ranked.sort_by(|a, b| b.1.cmp(&a.1));

    for (op, count) in ranked.into_iter().take(16) {
      // Precision loss acceptable for percentage display calculation
      let pct = ((count as i64) as f64 / (total as i64) as f64) * 100.0;
      eprintln!("[vm-profile] {:>16} {:>12} {:>6.2}%", op.name(), count, pct);
    }
  }

  /// Return the current `this` value.
  pub fn this_value(&self) -> Value {
    self.this_value.clone()
  }

  /// Return whether the current host builtin call includes `this` at `args[0]`.
  pub fn builtin_this_arg_prepended(&self) -> bool {
    self.builtin_this_arg_prepended
  }

  /// Return whether a JavaScript exception is currently unwinding to a handler.
  pub(crate) fn has_pending_exception(&self) -> bool {
    self.pending_exception.is_some()
  }

  /// Close an iterator from a host callback without disturbing its pending
  /// language-level exception.
  pub(crate) fn close_iterator_in_fork(
    &self,
    iterator: &Value,
  ) -> Result<(), RuntimeError> {
    let mut child = self.fork_execution();
    let return_method =
      child.get_property_host(iterator, &PropertyKey::String("return".to_string()))?;
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

  /// Return the current `[[NewTarget]]` value.
  pub fn current_new_target(&self) -> Option<Value> {
    self
      .call_stack
      .last()
      .and_then(|frame| frame.new_target.clone())
      // During constructor entry the pending value has not yet been moved
      // into the suspended caller frame.
      .or_else(|| self.construct_new_target.clone())
  }

  /// Return the dynamic eval context flags for the currently executing code.
  ///
  /// `new.target` is valid inside any function-like execution context.
  /// `super` is valid only when the current frame has a home object.
  pub(crate) fn current_eval_context_flags(&self) -> (bool, bool) {
    (
      self.current_closure_env.is_some(),
      self.current_method_home_object().is_some(),
    )
  }

  fn get_new_target(&self) -> Value {
    self
      .current_new_target()
      .unwrap_or(Value::Undefined)
  }

  fn get_this(&self) -> Value {
    self.this_value.clone()
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
    let frame = self.call_stack.last()?;
    match &frame.callee {
      Value::Closure { props, .. } | Value::BuiltinObject(_, props) => {
        let props = props.borrow();
        props
          .properties
          .get(&PropertyKey::String("[[HomeObject]]".to_string()))
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
      Value::Promise(_) => self.promise_prototype_value(),
      Value::String(_) => self
        .realm
        .intrinsics
        .string_prototype
        .clone()
        .unwrap_or(Value::Null),
      Value::Number(_) => self
        .realm
        .intrinsics
        .number_prototype
        .clone()
        .unwrap_or(Value::Null),
      Value::Bool(_) => self
        .realm
        .intrinsics
        .boolean_prototype
        .clone()
        .unwrap_or(Value::Null),
      Value::Symbol(_, _) => self
        .realm
        .intrinsics
        .symbol_prototype
        .clone()
        .unwrap_or(Value::Null),
      Value::BigInt(_) => self
        .realm
        .intrinsics
        .bigint_prototype
        .clone()
        .unwrap_or(Value::Null),
      Value::Null | Value::Undefined | Value::Uninitialized => Value::Null,
      Value::Builtin(_) => self
        .realm
        .intrinsics
        .function_prototype
        .clone()
        .unwrap_or(Value::Null),
    }
  }

  /// Resolve `Promise.prototype` from the realm's intrinsic table or from
  /// the current global constructor as a fallback.
  pub(super) fn promise_prototype_value(&self) -> Value {
    if let Some(proto) = self.realm.intrinsics.promise_prototype.clone()
      && !matches!(proto, Value::Null | Value::Undefined)
    {
      return proto;
    }

    if let Some(ctor) = self.realm.get_global("Promise")
      && let Ok(proto) = get_property(&ctor, &PropertyKey::String("prototype".to_string()), 0)
      && !matches!(proto, Value::Null | Value::Undefined)
    {
      return proto;
    }

    Value::Null
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
      PropertyKey::String("length".to_string()),
      array_length_descriptor(len),
    );

    if let Some(proto) = self.realm.intrinsics.array_prototype.clone() {
      props.borrow_mut().set_prototype_value(proto);
    }

    Value::Array(arr_rc, props)
  }

  /// Run the prototype mark/sweep collector and return mark statistics.
  pub fn collect_garbage_prototype(&self) -> gc::GcStats {
    // The current prototype GC cannot inspect captures held inside queued
    // host-side microtask/import-job closures. Collecting while those queues
    // are non-empty can reclaim still-live JS values reachable only through
    // those opaque Rust closures, leading to stalled async work.
    if !self.microtasks.is_empty() || !self.dynamic_import_jobs.is_empty() {
      if Self::gc_trace_enabled() {
        eprintln!(
          "[gc] skip collect: pending_jobs microtasks={} import_jobs={}",
          self.microtasks.len(),
          self.dynamic_import_jobs.len()
        );
      }
      return gc::GcStats::default();
    }

    let before = if Self::gc_trace_enabled() {
      Some(gc::heap_snapshot())
    } else {
      None
    };

    let mut roots: Vec<Value> = Vec::new();
    let mut env_roots: Vec<Rc<RefCell<Vec<SharedValue>>>> = Vec::new();
    roots.push(Value::Object(Rc::clone(&self.globals)));
    roots.extend(self.locals.iter().map(|c| c.borrow().clone()));
    roots.push(self.this_value.clone());
    if let Some(value) = &self.construct_new_target {
      roots.push(value.clone());
    }
    if let Some(value) = &self.current_arguments_object {
      roots.push(value.clone());
    }
    if let Some(value) = &self.recent_return_value {
      roots.push(value.clone());
    }
    roots.extend(
      self
        .derived_constructor_this
        .iter()
        .filter_map(Clone::clone),
    );
    if let Some(value) = &self.last_uncaught_exception {
      roots.push(value.clone());
    }
    if let Some(value) = &self.pending_exception {
      roots.push(value.clone());
    }
    if let Some(state) = &self.unwound_risc_state {
      roots.extend(state.registers.iter().cloned());
      roots.extend(state.spills.iter().cloned());
    }
    if let Some(state) = &self.yield_star_state {
      roots.push(state.iterator.clone());
      roots.push(state.next_method.clone());
    }
    if let Some(env) = &self.current_closure_env {
      env_roots.push(Rc::clone(env));
      roots.extend(
        env
          .borrow()
          .iter()
          .map(|cell| cell.borrow().clone()),
      );
    }
    if let Some(env) = &self.pending_closure_env {
      env_roots.push(Rc::clone(env));
      roots.extend(
        env
          .borrow()
          .iter()
          .map(|cell| cell.borrow().clone()),
      );
    }
    roots.extend(self.gc_temp_roots.iter().cloned());
    roots.extend(self.host_roots.iter().cloned());
    roots.extend(
      self
        .property_get_ic
        .values()
        .map(|entry| entry.value.clone()),
    );
    self
      .lexical_env
      .borrow()
      .collect_binding_values(&mut roots);
    self
      .global_env
      .borrow()
      .collect_binding_values(&mut roots);
    for frame in self.call_stack.iter() {
      roots.extend(frame.locals.iter().map(|c| c.borrow().clone()));
      roots.push(frame.this_value.clone());
      roots.push(frame.callee.clone());
      if let Some(state) = &frame.risc_state {
        roots.extend(state.registers.iter().cloned());
        roots.extend(state.spills.iter().cloned());
      }
      if let Some(value) = &frame.new_target {
        roots.push(value.clone());
      }
      if let Some(value) = &frame.arguments_object {
        roots.push(value.clone());
      }
      if let Some(state) = &frame.yield_star_state {
        roots.push(state.iterator.clone());
        roots.push(state.next_method.clone());
      }
      if let Some(env) = &frame.closure_env {
        env_roots.push(Rc::clone(env));
        roots.extend(
          env
            .borrow()
            .iter()
            .map(|cell| cell.borrow().clone()),
        );
      }
      frame
        .lexical_env
        .borrow()
        .collect_binding_values(&mut roots);
    }
    for module in &self.module_table {
      roots.extend(module.values().map(|cell| cell.borrow().clone()));
    }
    if let Some(value) = self.realm.intrinsics.object_constructor.clone() {
      roots.push(value);
    }
    if let Some(value) = self.realm.intrinsics.array_constructor.clone() {
      roots.push(value);
    }
    if let Some(value) = self.realm.intrinsics.function_constructor.clone() {
      roots.push(value);
    }
    if let Some(value) = self.realm.intrinsics.string_constructor.clone() {
      roots.push(value);
    }
    if let Some(value) = self.realm.intrinsics.symbol_constructor.clone() {
      roots.push(value);
    }
    if let Some(value) = self.realm.intrinsics.error_constructor.clone() {
      roots.push(value);
    }
    if let Some(value) = self
      .realm
      .intrinsics
      .type_error_constructor
      .clone()
    {
      roots.push(value);
    }
    if let Some(value) = self
      .realm
      .intrinsics
      .range_error_constructor
      .clone()
    {
      roots.push(value);
    }
    if let Some(value) = self
      .realm
      .intrinsics
      .reference_error_constructor
      .clone()
    {
      roots.push(value);
    }
    if let Some(value) = self
      .realm
      .intrinsics
      .syntax_error_constructor
      .clone()
    {
      roots.push(value);
    }
    if let Some(value) = self.realm.intrinsics.promise_constructor.clone() {
      roots.push(value);
    }
    if let Some(value) = self.realm.intrinsics.date_constructor.clone() {
      roots.push(value);
    }
    if let Some(value) = self.realm.intrinsics.map_constructor.clone() {
      roots.push(value);
    }
    if let Some(value) = self.realm.intrinsics.number_constructor.clone() {
      roots.push(value);
    }
    if let Some(value) = self.realm.intrinsics.object_prototype.clone() {
      roots.push(value);
    }
    if let Some(value) = self.realm.intrinsics.array_prototype.clone() {
      roots.push(value);
    }
    if let Some(value) = self.realm.intrinsics.function_prototype.clone() {
      roots.push(value);
    }
    if let Some(value) = self.realm.intrinsics.string_prototype.clone() {
      roots.push(value);
    }
    if let Some(value) = self.realm.intrinsics.symbol_prototype.clone() {
      roots.push(value);
    }
    if let Some(value) = self.realm.intrinsics.error_prototype.clone() {
      roots.push(value);
    }
    if let Some(value) = self.realm.intrinsics.promise_prototype.clone() {
      roots.push(value);
    }
    if let Some(value) = self.realm.intrinsics.date_prototype.clone() {
      roots.push(value);
    }
    if let Some(value) = self.realm.intrinsics.map_prototype.clone() {
      roots.push(value);
    }
    if let Some(value) = self.realm.intrinsics.boolean_prototype.clone() {
      roots.push(value);
    }
    if let Some(value) = self.realm.intrinsics.number_prototype.clone() {
      roots.push(value);
    }
    if let Some(value) = self.realm.intrinsics.bigint_prototype.clone() {
      roots.push(value);
    }
    if let Some(value) = self
      .realm
      .intrinsics
      .eval_error_constructor
      .clone()
    {
      roots.push(value);
    }
    if let Some(value) = self
      .realm
      .intrinsics
      .uri_error_constructor
      .clone()
    {
      roots.push(value);
    }
    let stats = gc::collect_with_env_roots(&roots, &env_roots);
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
    if !self.call_stack.is_empty()
      || !self.microtasks.is_empty()
      || !self.dynamic_import_jobs.is_empty()
      || self.pending_closure_env.is_some()
    {
      if Self::gc_trace_enabled() {
        eprintln!(
          "[gc] skip maybe_collect pressure={} threshold={} call_stack={} microtasks={} import_jobs={} pending_closure_env={} temp_roots={}",
          pressure,
          threshold,
          self.call_stack.len(),
          self.microtasks.len(),
          self.dynamic_import_jobs.len(),
          self.pending_closure_env.is_some(),
          self.gc_temp_roots.len(),
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
  fn populate_realm_intrinsics(
    &mut self,
    intrinsics: Intrinsics,
  ) {
    self.realm.set_intrinsics(intrinsics);
  }

  /// Provide mutable access to the per-VM module loader, creating it if missing.
  ///
  /// # Panics
  ///
  /// Panics if the module loader cannot be borrowed mutably.
  pub fn module_loader_mut(&self) -> std::cell::RefMut<'_, ModuleLoader> {
    self.module_loader.borrow_mut()
  }

  /// Return a snapshot of this VM realm's module-loader pipeline counters.
  #[must_use]
  pub fn module_loader_pipeline_snapshot(&self) -> crate::module::ModuleLoaderPipelineSnapshot {
    self.module_loader.borrow().pipeline_snapshot()
  }

  /// Validate and register a parser-free prelinked graph in this VM's realm.
  ///
  /// # Errors
  ///
  /// Returns an error for invalid graph metadata or a specifier/index
  /// collision with an existing module registration.
  pub fn register_prelinked_graph(
    &self,
    graph: crate::prelinked::PrelinkedModuleGraph,
  ) -> Result<(), crate::errors::BundlerError> {
    self
      .module_loader
      .borrow_mut()
      .register_prelinked_graph(graph)
  }

  /// Register a parser-free graph and declaration sources atomically.
  ///
  /// Declaration targets must be unique public roots in `graph`; the loader
  /// remains unchanged when declaration validation fails.
  ///
  /// # Errors
  ///
  /// Returns an error if graph validation, declaration validation, or an
  /// existing specifier/index collision rejects registration.
  pub fn register_prelinked_graph_with_declarations<I, S, T>(
    &self,
    graph: crate::prelinked::PrelinkedModuleGraph,
    declarations: I,
  ) -> Result<(), crate::errors::BundlerError>
  where
    I: IntoIterator<Item = (S, T)>,
    S: Into<String>,
    T: Into<String>,
  {
    self
      .module_loader
      .borrow_mut()
      .register_prelinked_graph_with_declarations(graph, declarations)
  }

  /// Register a parser-free graph, declarations, and private host modules atomically.
  ///
  /// # Errors
  ///
  /// Returns an error if graph, declaration, host-module, or collision
  /// validation fails.
  pub fn register_prelinked_graph_with_declarations_and_host_modules<I, S, T>(
    &mut self,
    graph: crate::prelinked::PrelinkedModuleGraph,
    declarations: I,
    host_modules: Vec<crate::module::PrelinkedHostModule>,
  ) -> Result<(), crate::errors::BundlerError>
  where
    I: IntoIterator<Item = (S, T)>,
    S: Into<String>,
    T: Into<String>,
  {
    let host_roots = host_modules
      .iter()
      .flat_map(|module| module.exports.values().cloned())
      .collect::<Vec<_>>();
    self
      .module_loader
      .borrow_mut()
      .register_prelinked_graph_with_declarations_and_host_modules(
        graph,
        declarations,
        host_modules,
      )?;
    self.host_roots.extend(host_roots);
    Ok(())
  }

  /// Register one host-provided virtual module in this VM's realm loader.
  ///
  /// # Errors
  ///
  /// Returns an error for an invalid or duplicate specifier.
  pub fn register_virtual_module(
    &self,
    module: crate::module::VirtualModule,
  ) -> Result<(), crate::errors::BundlerError> {
    self
      .module_loader
      .borrow_mut()
      .register_virtual_module(module)
  }

  /// Register an alias for a host-provided virtual module.
  ///
  /// # Errors
  ///
  /// Returns an error when the target is not registered or the alias collides.
  pub fn register_virtual_module_alias(
    &self,
    alias: impl Into<String>,
    canonical_specifier: impl Into<String>,
  ) -> Result<(), crate::errors::BundlerError> {
    self
      .module_loader
      .borrow_mut()
      .register_virtual_alias(alias, canonical_specifier)
  }

  /// Register a host-provided virtual module registry in this VM's realm loader.
  ///
  /// # Errors
  ///
  /// Returns an error for an invalid or duplicate module or alias.
  pub fn register_virtual_modules(
    &self,
    registry: crate::module::VirtualModuleRegistry,
  ) -> Result<(), crate::errors::BundlerError> {
    self
      .module_loader
      .borrow_mut()
      .register_virtual_modules(registry)
  }

  /// Return the canonical specifier of the module currently being evaluated.
  pub(crate) fn current_module_specifier(&self) -> Option<&str> {
    self.current_module_specifier.as_deref()
  }

  /// Set the canonical specifier of the module currently being evaluated.
  pub(crate) fn set_current_module_specifier(
    &mut self,
    specifier: Option<String>,
  ) {
    self.current_module_specifier = specifier;
  }

  /// Return a shared handle to the module loader for this VM's realm.
  pub fn module_loader_handle(&self) -> Rc<RefCell<ModuleLoader>> {
    Rc::clone(&self.module_loader)
  }

  /// Install a module's export slot map into the VM module table at the
  /// specified numeric module index. The provided `exports` map contains
  /// shared value slots allocated by the ModuleLoader so both VM and
  /// ModuleLoader share the same live binding slots.
  pub fn install_module_exports(
    &mut self,
    module_index: usize,
    mut exports: HashMap<String, SharedValue>,
  ) {
    let namespace = build_module_namespace_object(&exports);
    exports.insert("*".to_string(), Rc::new(RefCell::new(namespace)));
    // Ensure table is large enough
    while self.module_table.len() <= module_index {
      self.module_table.push(HashMap::new());
    }
    self.module_table[module_index] = exports;
  }

  /// Install exports only when this VM has not seen the module yet.
  ///
  /// Dynamic imports evaluate in a child execution context, but the namespace
  /// returned to source code belongs to the caller's realm. Preserving an
  /// existing table entry keeps static and dynamic access paths on one
  /// namespace object.
  pub(crate) fn ensure_module_exports(
    &mut self,
    module_index: usize,
    exports: HashMap<String, SharedValue>,
  ) {
    let installed = self
      .module_table
      .get(module_index)
      .is_some_and(|module| module.contains_key("*"));
    if !installed {
      self.install_module_exports(module_index, exports);
    }
  }

  /// Return the current namespace object for a module, if available.
  pub fn module_namespace_value(
    &self,
    module_index: usize,
  ) -> Option<Value> {
    self
      .module_table
      .get(module_index)
      .and_then(|exports| exports.get("*"))
      .map(|slot| slot.borrow().clone())
  }

  /// Debug helper: get a global value by name (cloned).
  pub fn get_global(
    &self,
    name: &str,
  ) -> Option<Value> {
    self
      .globals
      .borrow()
      .properties
      .get(&PropertyKey::String(name.to_string()))
      .and_then(|d| match d {
        PropertyDescriptor::Data { value, .. } => Some(value.clone()),
        _ => None,
      })
  }

  /// Returns the current globals object for the active realm.
  pub fn globals_rc(&self) -> Rc<RefCell<ObjectData>> {
    Rc::clone(&self.globals)
  }

  /// Returns a reference to the Realm this VM operates in.
  pub fn realm(&self) -> &Realm {
    &self.realm
  }

  /// Returns a mutable reference to the Realm this VM operates in.
  pub fn realm_mut(&mut self) -> &mut Realm {
    &mut self.realm
  }

  /// Returns whether the current execution frame is in strict mode.
  /// If there are no frames (root-level execution), returns false.
  pub fn is_strict_mode(&self) -> bool {
    self.current_strict_mode
  }

  /// Returns whether the current execution context is in strict mode.
  pub(crate) fn active_strict_mode(&self) -> bool {
    self.current_strict_mode
  }

  /// Sets the current strict mode flag.
  pub(crate) fn set_current_strict_mode(
    &mut self,
    strict: bool,
  ) {
    self.current_strict_mode = strict;
  }

  /// Return own keys for a global value if it is an object or closure.
  pub fn list_global_own_keys(
    &self,
    name: &str,
  ) -> Option<Vec<String>> {
    let g = self.globals.borrow();
    let v = g
      .properties
      .get(&PropertyKey::String(name.to_string()))
      .and_then(|d| match d {
        PropertyDescriptor::Data { value, .. } => Some(value.clone()),
        _ => None,
      })?;
    list_own_keys_of_value(&v)
  }

  /// Return prototype keys for a global's prototype property.
  pub fn list_global_prototype_keys(
    &self,
    name: &str,
  ) -> Option<Vec<String>> {
    let g = self.globals.borrow();
    let v = g
      .properties
      .get(&PropertyKey::String(name.to_string()))
      .and_then(|d| match d {
        PropertyDescriptor::Data { value, .. } => Some(value.clone()),
        _ => None,
      })?;
    match get_property(&v, &PropertyKey::String("prototype".to_string()), 0) {
      Ok(pv) => list_own_keys_of_value(&pv),
      Err(_) => None,
    }
  }

  /// Return own keys for a value when applicable.
  pub fn list_own_keys_of_value(
    &self,
    v: &Value,
  ) -> Option<Vec<String>> {
    list_own_keys_of_value(v)
  }

  /// Return enumerable string keys for `for-in`, walking the prototype
  /// chain and skipping duplicates.
  pub fn for_in_keys_of_value(
    &mut self,
    v: &Value,
  ) -> Result<Vec<String>, RuntimeError> {
    let mut keys = Vec::new();
    let mut seen = HashSet::new();
    let mut current = v.clone();
    let mut depth = 0usize;

    while !matches!(current, Value::Null) {
      if depth > MAX_PROTOTYPE_DEPTH {
        return Err(RuntimeError::from(
          "Prototype chain cycle detected or too deep",
        ));
      }

      for key in self.own_enumerable_keys_value(&current)? {
        if let PropertyKey::String(name) = key
          && seen.insert(name.clone())
        {
          keys.push(name);
        }
      }

      current = self.get_prototype(&current);
      depth += 1;
    }

    Ok(keys)
  }

  /// Shallow reference equality check for object-like runtime values.
  pub fn values_same_object(
    &self,
    a: &Value,
    b: &Value,
  ) -> bool {
    match (a, b) {
      (Value::Object(am), Value::Object(bm)) => Rc::ptr_eq(am, bm),
      (Value::Array(ar, _), Value::Array(br, _)) => Rc::ptr_eq(ar, br),
      (Value::Closure { props: ap, .. }, Value::Closure { props: bp, .. }) => Rc::ptr_eq(ap, bp),
      _ => false,
    }
  }

  /// Reset mutable runtime state while preserving selected globals.
  pub fn reset(
    &mut self,
    keep_globals: &HashSet<String>,
  ) {
    self.locals.clear();
    self.call_stack.clear();
    self.current_strict_mode = self.chunk.root_strict;
    self.handlers.clear();
    self.pending_exception = None;
    self.pending_exception_frame_count = None;
    self.unwound_risc_state = None;
    self.root_chunk = None;
    self.current_slot_types = None;
    self.current_slot_names = None;
    self.current_slot_immutable = None;
    self.current_slot_sloppy_immutable_silent = None;
    self.direct_eval_outer_slot_names = None;
    self.direct_eval_outer_slot_immutable = None;
    self.direct_eval_outer_slot_sloppy_immutable_silent = None;
    self.this_value = self
      .globals
      .borrow()
      .properties
      .get(&PropertyKey::String("globalThis".to_string()))
      .and_then(|d| match d {
        PropertyDescriptor::Data { value, .. } => Some(value.clone()),
        _ => None,
      })
      .unwrap_or(Value::Undefined);
    self.current_closure_env = None;
    self.pending_closure_env = None;
    self.gc_temp_roots.clear();
    self.lexical_env = Rc::clone(&self.global_env);
    self.yield_star_state = None;
    self.generator_return_frame_count = None;
    self.ip = 0;
    self.chunk = Arc::new(Chunk::new(Arc::new(String::new())));
    self.obj_generations.clear();
    self.property_get_ic.clear();
    gc::reset_allocation_pressure();
    let mut g = self.globals.borrow_mut();
    g.properties
      .retain(|k, _| matches!(k, PropertyKey::String(s) if keep_globals.contains(s)));
  }

  // -----------------------------------------------------------------------
  // Main execution loop
  // -----------------------------------------------------------------------

  /// Execute a chunk of bytecode to completion.
  ///
  /// # Errors
  ///
  /// Returns a [`crate::errors::RuntimeError`] if the chunk execution fails.
  pub fn run(
    &mut self,
    chunk: Chunk,
  ) -> Result<Value, RuntimeError> {
    chunk
      .verify()
      .map_err(|error| RuntimeError::from(format!("Invalid bytecode: {}", error)))?;
    self.run_risc_root(chunk)
  }

  fn run_risc_root(
    &mut self,
    chunk: Chunk,
  ) -> Result<Value, RuntimeError> {
    let chunk_rc = Arc::new(chunk);
    self.root_chunk = Some(Arc::clone(&chunk_rc));
    self.chunk = chunk_rc;
    self.ip = 0;
    self.locals.clear();
    self.call_stack.clear();
    self.current_strict_mode = self.chunk.root_strict;
    self.handlers.clear();
    self.pending_exception = None;
    self.pending_exception_frame_count = None;
    self.unwound_risc_state = None;
    self.property_get_ic.clear();
    self.current_slot_types = None;
    self.current_slot_names = None;
    self.current_slot_immutable = None;
    self.current_slot_sloppy_immutable_silent = None;
    self.direct_eval_outer_slot_names = None;
    self.direct_eval_outer_slot_immutable = None;
    self.direct_eval_outer_slot_sloppy_immutable_silent = None;
    self.current_closure_env = None;
    self.pending_closure_env = None;
    self.gc_temp_roots.clear();
    self.lexical_env = Rc::clone(&self.global_env);
    self.yield_star_state = None;
    self.generator_return_frame_count = None;
    self.this_value = self
      .globals
      .borrow()
      .properties
      .get(&PropertyKey::String("globalThis".to_string()))
      .and_then(|descriptor| match descriptor {
        PropertyDescriptor::Data { value, .. } => Some(value.clone()),
        _ => None,
      })
      .unwrap_or(Value::Undefined);
    self.initialize_root_bindings();
    gc::reset_allocation_pressure();
    self.begin_opcode_profile_run();

    self.run_risc_loop(true)
  }

  pub(super) fn run_risc_loop(
    &mut self,
    _root_execution: bool,
  ) -> Result<Value, RuntimeError> {
    let mut state = RiscExecutionState::new(
      self.this_value.clone(),
      self.chunk.risc_spill_count as usize,
    );
    match self.run_risc_state(&mut state, false)? {
      RiscRunOutcome::Return(value) => Ok(value),
      RiscRunOutcome::Yield(_) | RiscRunOutcome::YieldStar(_) => {
        Err(RuntimeError::from("yield outside generator execution"))
      }
      RiscRunOutcome::Initialized => Err(RuntimeError::from(
        "generator boundary outside generator execution",
      )),
    }
  }

  pub(super) fn run_risc_state(
    &mut self,
    state: &mut RiscExecutionState,
    stop_at_generator_init: bool,
  ) -> Result<RiscRunOutcome, RuntimeError> {
    let boundary_frame_count = self.call_stack.len();
    let registers = &mut state.registers;
    let spills = &mut state.spills;

    if self
      .pending_exception_frame_count
      .is_some_and(|target| target >= boundary_frame_count)
      && let Some(exception) = self.pending_exception.take()
    {
      self.pending_exception_frame_count = None;
      registers[7] = exception;
    }

    loop {
      let word = *self
        .chunk
        .code
        .get(self.ip)
        .ok_or_else(|| RuntimeError::from("RISC instruction pointer out of bounds"))?;
      let instruction_ip = self.ip;
      self.ip += 1;
      let op = Op::from_byte((word & 0xFF) as u8)
        .ok_or_else(|| RuntimeError::from("invalid RISC opcode"))?;
      let rd = decode_rd(word) as usize;
      let rs = decode_rs(word) as usize;
      if self.opcode_profiling_enabled {
        self.opcode_counts[op as usize] = self.opcode_counts[op as usize].saturating_add(1);
      }
      if crate::trace::enabled() {
        eprintln!(
          "[trace] risc {}",
          self
            .chunk
            .disassemble_at(instruction_ip)
            .unwrap_or_else(|_| format!("ip={instruction_ip}"))
        );
      }

      let mut switched_risc_context = false;
      let step_result = (|| -> Result<Option<RiscRunOutcome>, RuntimeError> {
        match op {
          Op::Const => {
            registers[rd] = self
              .chunk
              .constants
              .get(decode_risc_aux16(word) as usize)
              .map(crate::bytecode::Constant::to_value)
              .ok_or_else(|| RuntimeError::from("RISC constant index out of bounds"))?;
          }
          Op::Null => registers[rd] = Value::Null,
          Op::True => registers[rd] = Value::Bool(true),
          Op::False => registers[rd] = Value::Bool(false),
          Op::Undefined => registers[rd] = Value::Undefined,
          Op::Move => registers[rd] = registers[rs].clone(),
          Op::LoadSpill => {
            registers[rd] = spills
              .get(decode_risc_aux16(word) as usize)
              .cloned()
              .ok_or_else(|| RuntimeError::from("RISC spill load out of bounds"))?;
          }
          Op::StoreSpill => {
            let spill = spills
              .get_mut(decode_risc_aux16(word) as usize)
              .ok_or_else(|| RuntimeError::from("RISC spill store out of bounds"))?;
            *spill = registers[rd].clone();
          }
          Op::MakeClosure => {
            registers[rd] = self.make_closure_value(decode_risc_aux16(word) as usize)?;
          }
          Op::LoadLocal => {
            let slot = decode_risc_aux16(word) as usize;
            let value = self
              .locals
              .get(slot)
              .map(|cell| cell.borrow().clone())
              .ok_or_else(|| RuntimeError::from("bad RISC local slot"))?;
            if matches!(value, Value::Uninitialized) {
              return Err(
                self.throw_reference_error("Cannot access lexical binding before initialization"),
              );
            }
            registers[rd] = value;
          }
          Op::StoreLocal => {
            let slot = decode_risc_aux16(word) as usize;
            ensure_slot(&mut self.locals, slot);
            let was_uninitialized = matches!(*self.locals[slot].borrow(), Value::Uninitialized);
            let immutable = self
              .current_slot_immutable
              .as_ref()
              .and_then(|flags| flags.get(slot))
              .copied()
              .unwrap_or(false);
            let initializes = immutable && was_uninitialized;
            if immutable && !initializes {
              let sloppy_silent = self
                .current_slot_sloppy_immutable_silent
                .as_ref()
                .and_then(|flags| flags.get(slot))
                .copied()
                .unwrap_or(false);
              if self.is_strict_mode() || !sloppy_silent {
                return Err(self.throw_type_error("Assignment to constant variable"));
              }
            } else {
              let has_object_environment = self.lexical_env.borrow().has_object_environment();
              let slot_name = (has_object_environment || was_uninitialized)
                .then(|| {
                  self
                    .current_slot_names
                    .as_ref()
                    .and_then(|names| names.get(slot))
                    .and_then(Clone::clone)
                    .or_else(|| {
                      self
                        .chunk
                        .root_slot_names
                        .get(slot)
                        .and_then(Clone::clone)
                    })
                })
                .flatten();
              let wrote_with_environment =
                if has_object_environment && let Some(name) = slot_name.as_ref() {
                  self.set_with_env_binding_value(name, registers[rd].clone())?
                } else {
                  false
                };
              if !wrote_with_environment {
                *self.locals[slot].borrow_mut() = registers[rd].clone();
                if was_uninitialized && let Some(name) = slot_name {
                  let _ = self
                    .lexical_env
                    .borrow_mut()
                    .set_binding_initialized_in_chain(&name, true);
                }
                if let Some(arguments_object) = self.current_arguments_object.clone()
                  && slot < self.current_arguments_mapped_count
                {
                  let key = PropertyKey::String(slot.to_string());
                  if let Some((PropertyDescriptor::Data { writable: true, .. }, _)) =
                    self.lookup_property_descriptor(&arguments_object, &key, 0)?
                  {
                    set_property(&arguments_object, &key, registers[rd].clone())?;
                    self.bump_object_generation(&arguments_object);
                  }
                }
              }
            }
          }
          Op::CloneLocalCell => {
            let slot = decode_risc_aux16(word) as usize;
            ensure_slot(&mut self.locals, slot);
            let value = self.locals[slot].borrow().clone();
            self.locals[slot] = Rc::new(RefCell::new(value));
          }
          Op::ResetLocalCell => {
            let slot = decode_risc_aux16(word) as usize;
            ensure_slot(&mut self.locals, slot);
            self.locals[slot] = Rc::new(RefCell::new(Value::Uninitialized));
          }
          Op::GetUpvalue => {
            let index = decode_risc_aux16(word) as usize;
            let value = {
              let environment = self
                .current_closure_env
                .as_ref()
                .ok_or("GetUpvalue without closure environment")?;
              let environment = environment.borrow();
              environment
                .get(index)
                .map(|cell| cell.borrow().clone())
                .ok_or_else(|| {
                  format!(
                    "Bad upvalue index {index} (closure environment has {} entries)",
                    environment.len()
                  )
                })?
            };
            if matches!(value, Value::Uninitialized) {
              return Err(
                self.throw_reference_error("Cannot access lexical binding before initialization"),
              );
            }
            registers[rd] = value;
          }
          Op::SetUpvalue => {
            let index = decode_risc_aux16(word) as usize;
            let (mirror_global_name, immutable, sloppy_immutable_silent) = self
              .call_stack
              .last()
              .and_then(|frame| frame.upvalues.get(index))
              .map_or((None, false, false), |spec| {
                (
                  spec.mirror_global_name.clone(),
                  spec.immutable,
                  spec.sloppy_immutable_silent,
                )
              });
            if immutable {
              if self.is_strict_mode() || !sloppy_immutable_silent {
                return Err(self.throw_type_error("Assignment to constant variable"));
              }
            } else {
              let environment = self
                .current_closure_env
                .as_ref()
                .ok_or("SetUpvalue without closure environment")?;
              let cell = {
                let environment = environment.borrow();
                environment.get(index).cloned().ok_or_else(|| {
                  format!(
                    "Bad upvalue index {index} (closure environment has {} entries)",
                    environment.len()
                  )
                })?
              };
              *cell.borrow_mut() = registers[rd].clone();
              if let Some(name) = mirror_global_name {
                let current = cell.borrow().clone();
                self.sync_named_binding_to_global_and_frames(&name, &current);
              }
            }
          }
          Op::GetGlobal | Op::TypeofGlobal => {
            let name = self.string_constant(decode_risc_aux16(word) as usize)?;
            let value = self.resolve_identifier_value(&name)?;
            registers[rd] = if matches!(op, Op::TypeofGlobal) {
              Value::String(
                value
                  .as_ref()
                  .map_or_else(|| "undefined".to_string(), |value| self.typeof_value(value)),
              )
            } else {
              value.ok_or_else(|| self.throw_reference_error(&format!("{name} is not defined")))?
            };
          }
          Op::SetGlobal => {
            let name = self.string_constant(decode_risc_aux16(word) as usize)?;
            let value = registers[rd].clone();
            self.set_global_value(&name, value)?;
          }
          Op::GetBuiltin => {
            let name = self.string_constant(decode_risc_aux16(word) as usize)?;
            let value = self
              .realm
              .get_hidden_builtin(&name)
              .ok_or_else(|| RuntimeError::from(format!("GetBuiltin: '{name}' not found")))?;
            if !matches!(value, Value::Builtin(_) | Value::BuiltinObject(_, _)) {
              return Err(RuntimeError::from(format!(
                "GetBuiltin: '{name}' is not a builtin"
              )));
            }
            registers[rd] = value;
          }
          Op::GetModuleExport => {
            let (module_index, name) =
              self.module_export_descriptor(decode_risc_aux16(word) as usize)?;
            registers[rd] = self
              .module_table
              .get(module_index)
              .and_then(|module| module.get(&name).cloned())
              .map_or(Value::Undefined, |slot| slot.borrow().clone());
          }
          Op::SetModuleExport => {
            let (module_index, name) =
              self.module_export_descriptor(decode_risc_aux16(word) as usize)?;
            while self.module_table.len() <= module_index {
              self.module_table.push(HashMap::new());
            }
            if let Some(slot) = self.module_table[module_index].get(&name) {
              *slot.borrow_mut() = registers[rd].clone();
            } else {
              self.module_table[module_index]
                .insert(name, Rc::new(RefCell::new(registers[rd].clone())));
            }
          }
          Op::GetThis => registers[rd] = self.get_this(),
          Op::GetNewTarget => registers[rd] = self.get_new_target(),
          Op::Neg => {
            registers[rd] = Value::Number(-self.to_number_value(&registers[rs])?);
          }
          Op::Not => registers[rd] = Value::Bool(!registers[rs].is_truthy()),
          Op::BitNot => {
            registers[rd] = match registers[rs].clone() {
              Value::BigInt(value) => Value::BigInt(!value),
              value => Value::Number((!self.to_int32_value(&value)?) as f64),
            };
          }
          Op::Typeof => registers[rd] = Value::String(registers[rs].type_name().to_string()),
          Op::Add => {
            let left = self.to_primitive(&registers[rd], None)?;
            let right = self.to_primitive(&registers[rs], None)?;
            registers[rd] = if matches!(left, Value::String(_)) || matches!(right, Value::String(_))
            {
              Value::String(format!(
                "{}{}",
                self.to_string(&left)?,
                self.to_string(&right)?
              ))
            } else {
              match (&left, &right) {
                (Value::BigInt(left), Value::BigInt(right)) => {
                  Value::BigInt(left.clone() + right.clone())
                }
                (Value::BigInt(_), _) | (_, Value::BigInt(_)) => {
                  return Err(self.throw_type_error("Cannot mix BigInt and other types"));
                }
                _ => Value::Number(self.to_number_value(&left)? + self.to_number_value(&right)?),
              }
            };
          }
          Op::Sub | Op::Mul | Op::Div | Op::Mod | Op::Pow => {
            let left = registers[rd].clone();
            let right = registers[rs].clone();
            registers[rd] = match (left, right) {
              (Value::BigInt(left), Value::BigInt(right)) => match op {
                Op::Sub => Value::BigInt(left - right),
                Op::Mul => Value::BigInt(left * right),
                Op::Div if right.is_zero() => {
                  return Err(self.throw_range_error("Division by zero"));
                }
                Op::Div => Value::BigInt(left / right),
                Op::Mod if right.is_zero() => {
                  return Err(self.throw_range_error("Division by zero"));
                }
                Op::Mod => Value::BigInt(left % right),
                Op::Pow => {
                  let exponent = right
                    .to_u32()
                    .ok_or_else(|| self.throw_range_error("Invalid BigInt exponent"))?;
                  Value::BigInt(left.pow(exponent))
                }
                _ => unreachable!(),
              },
              (Value::BigInt(_), _) | (_, Value::BigInt(_)) => {
                return Err(self.throw_type_error("Cannot mix BigInt and other types"));
              }
              (left, right) => {
                let left = self.to_number_value(&left)?;
                let right = self.to_number_value(&right)?;
                Value::Number(match op {
                  Op::Sub => left - right,
                  Op::Mul => left * right,
                  Op::Div => left / right,
                  Op::Mod => left % right,
                  Op::Pow => number_exponentiate(left, right),
                  _ => unreachable!(),
                })
              }
            };
          }
          Op::BitAnd | Op::BitOr | Op::BitXor | Op::Shl | Op::Shr | Op::Ushr => {
            let left = registers[rd].clone();
            let right = registers[rs].clone();
            registers[rd] = match op {
              Op::BitAnd => {
                Value::Number((self.to_int32_value(&left)? & self.to_int32_value(&right)?) as f64)
              }
              Op::BitOr => {
                Value::Number((self.to_int32_value(&left)? | self.to_int32_value(&right)?) as f64)
              }
              Op::BitXor => {
                Value::Number((self.to_int32_value(&left)? ^ self.to_int32_value(&right)?) as f64)
              }
              Op::Shl => Value::Number(
                self
                  .to_int32_value(&left)?
                  .wrapping_shl(self.to_uint32_value(&right)? & 0x1f) as f64,
              ),
              Op::Shr => Value::Number(
                self
                  .to_int32_value(&left)?
                  .wrapping_shr(self.to_uint32_value(&right)? & 0x1f) as f64,
              ),
              Op::Ushr => Value::Number(
                self
                  .to_uint32_value(&left)?
                  .wrapping_shr(self.to_uint32_value(&right)? & 0x1f) as f64,
              ),
              _ => unreachable!(),
            };
          }
          Op::And => {
            registers[rd] = Value::Bool(registers[rd].is_truthy() && registers[rs].is_truthy());
          }
          Op::Or => {
            registers[rd] = Value::Bool(registers[rd].is_truthy() || registers[rs].is_truthy());
          }
          Op::Jump => {
            let offset = ((word >> 8) & 0x00FF_FFFF) as i32;
            let offset = if offset & 0x0080_0000 != 0 {
              offset | !0x00FF_FFFF
            } else {
              offset
            };
            self.ip = usize::try_from(self.ip as i64 + offset as i64)
              .map_err(|_| RuntimeError::from("negative RISC jump target"))?;
          }
          Op::JumpIfFalse | Op::JumpIfTrue | Op::JumpIfNullish => {
            let value = &registers[rd];
            let take = match op {
              Op::JumpIfFalse => !value.is_truthy(),
              Op::JumpIfTrue => value.is_truthy(),
              Op::JumpIfNullish => matches!(value, Value::Null | Value::Undefined),
              _ => unreachable!(),
            };
            if take {
              self.ip = usize::try_from(self.ip as i64 + decode_risc_cond_offset(word) as i64)
                .map_err(|_| RuntimeError::from("negative RISC conditional target"))?;
            }
          }
          Op::CompareJump => {
            let (condition, left, right, offset) = decode_compare_jump(word);
            let left = &registers[left as usize];
            let right = &registers[right as usize];
            let result = match condition {
              0 => self.abstract_equality(left, right)?,
              1 => !self.abstract_equality(left, right)?,
              2 => self
                .abstract_relational_comparison(left, right)?
                .unwrap_or(false),
              3 => self
                .abstract_relational_comparison(right, left)?
                .is_some_and(|result| !result),
              4 => self
                .abstract_relational_comparison(right, left)?
                .unwrap_or(false),
              5 => self
                .abstract_relational_comparison(left, right)?
                .is_some_and(|result| !result),
              6 => values_equal(left, right),
              7 => !values_equal(left, right),
              _ => unreachable!(),
            };
            if !result {
              self.ip = usize::try_from(self.ip as i64 + offset as i64)
                .map_err(|_| RuntimeError::from("negative RISC compare target"))?;
            }
          }
          Op::GetProperty => {
            let object = registers[rd].clone();
            let key = registers[rs].clone();
            let key = self.to_property_key(&key)?;
            registers[rd] = self.get_property_value_cached(instruction_ip, &object, &key)?;
          }
          Op::SetProperty => {
            let object = registers[rd].clone();
            let key = self.to_property_key(&registers[rs])?;
            let value = registers[(decode_aux8(word) & 0xF) as usize].clone();
            self.set_property_tracked(&object, &key, value.clone())?;
            registers[rd] = value;
          }
          Op::DeleteProperty => {
            let object = registers[rd].clone();
            let key = self.to_property_key(&registers[rs])?;
            let deleted = self.delete_property_host(&object, &key)?;
            if !deleted && self.is_strict_mode() {
              return Err(self.throw_type_error("Cannot delete property in strict mode"));
            }
            registers[rd] = Value::Bool(deleted);
          }
          Op::HasProperty => {
            let object = registers[rd].clone();
            let key = self.to_property_key(&registers[rs])?;
            registers[rd] = Value::Bool(self.has_property_host(&object, &key)?);
          }
          Op::InstanceOf => {
            let value = registers[rd].clone();
            let constructor = registers[rs].clone();
            registers[rd] = Value::Bool(self.instance_of(&value, &constructor)?);
          }
          Op::GetSuperProperty => {
            let receiver = registers[rd].clone();
            let key = registers[rs].clone();
            registers[rd] = self.get_super_property(&receiver, &key)?;
          }
          Op::SetSuperProperty => {
            let value_register = (decode_aux8(word) & 0xF) as usize;
            let receiver = registers[rd].clone();
            let key = registers[rs].clone();
            let value = registers[value_register].clone();
            registers[rd] = self.set_super_property(&receiver, &key, value)?;
          }
          Op::BuildArray => {
            let count = decode_risc_aux16(word) as usize;
            let base = self.chunk.risc_value_spill_count as usize;
            let values = spills
              .get(base..base + count)
              .ok_or_else(|| RuntimeError::from("RISC array operand window out of bounds"))?
              .to_vec();
            registers[rd] = self.build_array_value(values)?;
          }
          Op::BuildObject => {
            let count = decode_risc_aux16(word) as usize;
            let operand_count = count.saturating_mul(2);
            let base = self.chunk.risc_value_spill_count as usize;
            let values = spills
              .get(base..base + operand_count)
              .ok_or_else(|| RuntimeError::from("RISC object operand window out of bounds"))?
              .to_vec();
            registers[rd] = self.build_object_value(values)?;
          }
          Op::ArrayFromSpread => {
            let values = registers[rd].clone();
            let mask = registers[rs].clone();
            registers[rd] = array_from_values(self.flatten_spread_values(values, mask)?);
          }
          Op::Spread => {
            let callee = registers[rd].clone();
            let values = registers[rs].clone();
            let mask = registers[(decode_aux8(word) & 0xF) as usize].clone();
            let arguments = self.flatten_spread_values(values, mask)?;
            registers[rd] = self.call_value(&callee, &arguments)?;
          }
          Op::Call | Op::TailCall | Op::CallBuiltin => {
            let count = decode_risc_aux16(word) as usize;
            let mut args: Vec<Value> = (0..count.min(6))
              .map(|index| registers[2 + index].clone())
              .collect();
            let overflow = count.saturating_sub(6);
            let base = self.chunk.risc_value_spill_count as usize;
            args.extend_from_slice(
              spills
                .get(base..base + overflow)
                .ok_or_else(|| RuntimeError::from("RISC call operand window out of bounds"))?,
            );
            let callee = registers[0].clone();
            if op != Op::TailCall
              && let Value::Closure { proto, env, .. } = &callee
              && !proto.is_generator
              && !proto.is_async
            {
              if proto.is_class_constructor && self.construct_new_target.is_none() {
                return Err(self.throw_class_constructor_call_error(proto));
              }
              let call_args = self.prepare_call_args(&args, proto)?;
              let receiver = if op == Op::CallBuiltin {
                registers[1].clone()
              } else {
                Value::Undefined
              };
              let this_arg = Self::closure_lexical_this(&callee).unwrap_or(receiver);
              let this_arg = if proto.is_derived_constructor {
                Value::Uninitialized
              } else {
                this_arg
              };
              let caller_state = RiscExecutionState {
                registers: registers.clone(),
                spills: spills.clone(),
                resume_register: state.resume_register,
              };
              self.push_call_frame(proto, env, call_args, this_arg, callee.clone())?;
              self
                .call_stack
                .last_mut()
                .expect("call frame was just pushed")
                .risc_state = Some(caller_state);
              registers.fill(Value::Undefined);
              registers[1] = self.this_value.clone();
              spills.clear();
              spills.resize(self.chunk.risc_spill_count as usize, Value::Undefined);
              state.resume_register = None;
              switched_risc_context = true;
            } else if op != Op::TailCall {
              registers[0] = if matches!(op, Op::CallBuiltin) {
                let receiver = registers[1].clone();
                self.call_value_with_this(&callee, &receiver, &args)?
              } else {
                self.call_value(&callee, &args)?
              };
            } else if let Value::Closure { proto, env, .. } = &callee
              && !proto.is_generator
              && !proto.is_async
            {
              if proto.is_class_constructor && self.construct_new_target.is_none() {
                return Err(self.throw_class_constructor_call_error(proto));
              }
              let call_args = self.prepare_call_args(&args, proto)?;
              let this_arg = Self::closure_lexical_this(&callee).unwrap_or(Value::Undefined);
              let this_arg = if proto.is_derived_constructor {
                Value::Uninitialized
              } else {
                this_arg
              };
              let active_depth = self.call_stack.len();
              while self
                .handlers
                .last()
                .is_some_and(|handler| handler.frame_count >= active_depth)
              {
                self.handlers.pop();
              }
              if let Some(frame) = self.call_stack.last_mut() {
                frame.callee = callee.clone();
                frame.new_target = None;
                frame.upvalues = proto.upvalues.clone();
              }
              self.configure_call_frame(proto, env, call_args, this_arg, callee.clone())?;
              registers.fill(Value::Undefined);
              registers[1] = self.this_value.clone();
              spills.clear();
              spills.resize(self.chunk.risc_spill_count as usize, Value::Undefined);
              state.resume_register = None;
              switched_risc_context = true;
            } else {
              let result = self.call_value(&callee, &args)?;
              return Ok(Some(RiscRunOutcome::Return(result)));
            }
          }
          Op::Construct => {
            let count = decode_risc_aux16(word) as usize;
            let mut args: Vec<Value> = (0..count.min(6))
              .map(|index| registers[2 + index].clone())
              .collect();
            let overflow = count.saturating_sub(6);
            let base = self.chunk.risc_value_spill_count as usize;
            args.extend_from_slice(
              spills
                .get(base..base + overflow)
                .ok_or_else(|| RuntimeError::from("RISC construct operand window out of bounds"))?,
            );
            let callee = registers[0].clone();
            registers[0] = self.construct_value(&callee, &args)?;
          }
          Op::SuperConstruct => {
            let count = decode_risc_aux16(word) as usize;
            let base = self.chunk.risc_value_spill_count as usize;
            let args = spills
              .get(base..base + count)
              .ok_or_else(|| RuntimeError::from("RISC super operand window out of bounds"))?
              .to_vec();
            registers[rd] = self.construct_super_value(args)?;
          }
          Op::PushHandler => {
            let offset = ((word >> 8) & 0x00FF_FFFF) as i32;
            let offset = if offset & 0x0080_0000 != 0 {
              offset | !0x00FF_FFFF
            } else {
              offset
            };
            let handler_ip = usize::try_from(self.ip as i64 + offset as i64)
              .map_err(|_| RuntimeError::from("negative RISC handler target"))?;
            let finally = self
              .chunk
              .finally_handlers
              .iter()
              .find(|handler| handler.push_ip == instruction_ip)
              .map(|handler| FinallyHandlerRecord {
                finalizer_ip: handler.finalizer_ip,
                return_slot: handler.return_slot,
                did_return_slot: handler.did_return_slot,
              });
            self.handlers.push(HandlerRecord {
              chunk: Arc::clone(&self.chunk),
              handler_ip,
              saved_locals_len: self.locals.len(),
              frame_count: self.call_stack.len(),
              saved_lexical_env: Rc::clone(&self.lexical_env),
              finally,
            });
          }
          Op::PopHandler => {
            self.handlers.pop();
          }
          Op::Throw => {
            self.throw_value(registers[rd].clone())?;
            return Err(RuntimeError::from("JS exception"));
          }
          Op::DeleteIdentifier => {
            registers[rd] = self.delete_identifier(registers[rd].clone())?;
          }
          Op::IteratorNext => {
            registers[rd] = self.iterator_next(registers[rd].clone())?;
          }
          Op::IteratorClose => {
            registers[rd] = self.iterator_close(registers[rd].clone())?;
          }
          Op::PushWithEnv => {
            let object = self.to_object_value(registers[rd].clone())?;
            self.lexical_env = Rc::new(RefCell::new(EnvironmentRecord::new_object(
              Some(Rc::clone(&self.lexical_env)),
              object,
              true,
            )));
          }
          Op::PopEnv => {
            let outer = self.lexical_env.borrow().outer();
            self.lexical_env = outer.unwrap_or_else(|| Rc::clone(&self.global_env));
          }
          Op::GeneratorInitDone if stop_at_generator_init => {
            return Ok(Some(RiscRunOutcome::Initialized));
          }
          Op::GeneratorInitDone => {}
          Op::Yield => {
            state.resume_register = Some(rd);
            return Ok(Some(RiscRunOutcome::Yield(registers[rd].clone())));
          }
          Op::YieldStar => {
            if self.yield_star_state.is_none() {
              self.yield_star_state = Some(self.create_yield_star_state(registers[rd].clone())?);
            }
            match self.resume_yield_star()? {
              YieldStarStep::Yield(value) => {
                self.ip = self.ip.saturating_sub(1);
                return Ok(Some(RiscRunOutcome::YieldStar(value)));
              }
              YieldStarStep::Complete(value) => registers[rd] = value,
              YieldStarStep::Return(value) => {
                if !self.begin_generator_return(value.clone()) {
                  return Ok(Some(RiscRunOutcome::Return(value)));
                }
              }
            }
          }
          Op::AwaitPromise => {
            let awaited = registers[rd].clone();
            registers[rd] = match awaited {
              Value::Promise(promise) => loop {
                let settled = {
                  let promise = promise.borrow();
                  match &*promise {
                    PromiseState::Fulfilled(value) => Some(Ok(value.clone())),
                    PromiseState::Rejected { reason, .. } => Some(Err(reason.clone())),
                    PromiseState::Pending { .. } => None,
                  }
                };
                match settled {
                  Some(Ok(value)) => break value,
                  Some(Err(reason)) => {
                    self.throw_value(reason)?;
                    return Err(RuntimeError::from("JS exception"));
                  }
                  None if !self.microtasks.is_empty() || !self.dynamic_import_jobs.is_empty() => {
                    self.run_microtasks_if_needed()?;
                    self.maybe_collect_garbage_prototype();
                  }
                  None => {
                    return Err(RuntimeError::from(
                      "Promise did not settle during synchronous await",
                    ));
                  }
                }
              },
              other => other,
            };
          }
          Op::Return => {
            let result = registers[rd].clone();
            if self.generator_return_frame_count == Some(self.call_stack.len()) {
              if self.begin_generator_return(result.clone()) {
                return Ok(None);
              }
              self.generator_return_frame_count = None;
            }
            if let Some(caller_state) = self
              .call_stack
              .last_mut()
              .and_then(|frame| frame.risc_state.take())
            {
              self.execute_return(result.clone())?;
              registers.clone_from(&caller_state.registers);
              registers[0] = result;
              spills.clone_from(&caller_state.spills);
              state.resume_register = caller_state.resume_register;
              switched_risc_context = true;
              return Ok(None);
            }
            if self.call_stack.is_empty() {
              let temp_root_base = self.gc_temp_roots.len();
              self.gc_temp_roots.push(result.clone());
              self.run_microtasks_if_needed()?;
              let _ = self.collect_garbage_prototype();
              self.gc_temp_roots.truncate(temp_root_base);
              self.finish_opcode_profile_run("risc-return");
            }
            return Ok(Some(RiscRunOutcome::Return(result)));
          }
        }
        Ok(None)
      })();
      match step_result {
        Ok(Some(result)) => return Ok(result),
        Ok(None) if switched_risc_context => continue,
        Ok(None) => {}
        Err(_error)
          if self.pending_exception.is_some()
            && self
              .pending_exception_frame_count
              .is_some_and(|target| target >= boundary_frame_count) =>
        {
          if let Some(caller_state) = self.unwound_risc_state.take() {
            registers.clone_from(&caller_state.registers);
            spills.clone_from(&caller_state.spills);
            state.resume_register = caller_state.resume_register;
          }
          self.pending_exception_frame_count = None;
          registers[7] = self
            .pending_exception
            .take()
            .unwrap_or(Value::Undefined);
          continue;
        }
        Err(error) => return Err(error),
      }
      if gc::allocation_pressure() >= gc_allocation_pressure_threshold() {
        let temp_root_base = self.gc_temp_roots.len();
        self
          .gc_temp_roots
          .extend(registers.iter().cloned());
        self.gc_temp_roots.extend(spills.iter().cloned());
        self.maybe_collect_garbage_prototype();
        self.gc_temp_roots.truncate(temp_root_base);
      }
    }
  }

  /// Execute a chunk without resetting the caller's live lexical bindings.
  ///
  /// This is used for direct `eval`, where the evaluated code must observe
  /// and update the current execution context.
  #[allow(clippy::too_many_lines)]
  pub(crate) fn run_nested(
    &mut self,
    chunk: Chunk,
    direct_eval: bool,
  ) -> Result<Value, RuntimeError> {
    chunk
      .verify()
      .map_err(|error| RuntimeError::from(format!("Invalid bytecode: {}", error)))?;
    let saved_chunk = Arc::clone(&self.chunk);
    let saved_ip = self.ip;
    let saved_root_chunk = self.root_chunk.clone();
    let saved_current_strict_mode = self.current_strict_mode;
    let saved_current_closure_env = self.current_closure_env.clone();
    let saved_pending_closure_env = self.pending_closure_env.clone();
    let saved_gc_temp_roots = self.gc_temp_roots.clone();
    let saved_this_value = self.this_value.clone();
    let saved_yield_star_state = self.yield_star_state.clone();
    let saved_current_arguments_object = self.current_arguments_object.clone();
    let saved_current_arguments_mapped_count = self.current_arguments_mapped_count;
    let saved_recent_return_value = self.recent_return_value.clone();
    let saved_last_uncaught_exception = self.last_uncaught_exception.clone();
    let saved_pending_exception = self.pending_exception.clone();
    let saved_pending_exception_frame_count = self.pending_exception_frame_count;
    let saved_lexical_env = if direct_eval {
      Some(Rc::clone(&self.lexical_env))
    } else {
      None
    };
    let saved_locals = if direct_eval {
      Some(self.locals.clone())
    } else {
      None
    };
    let saved_current_slot_types = if direct_eval {
      self.current_slot_types.clone()
    } else {
      None
    };
    let saved_current_slot_names = if direct_eval {
      self.current_slot_names.clone()
    } else {
      None
    };
    let saved_current_slot_immutable = if direct_eval {
      self.current_slot_immutable.clone()
    } else {
      None
    };
    let saved_current_slot_sloppy_immutable_silent = if direct_eval {
      self.current_slot_sloppy_immutable_silent.clone()
    } else {
      None
    };
    let saved_direct_eval_outer_slot_names = if direct_eval {
      self.direct_eval_outer_slot_names.clone()
    } else {
      None
    };
    let saved_direct_eval_outer_slot_immutable = if direct_eval {
      self.direct_eval_outer_slot_immutable.clone()
    } else {
      None
    };
    let saved_direct_eval_outer_slot_sloppy_immutable_silent = if direct_eval {
      self
        .direct_eval_outer_slot_sloppy_immutable_silent
        .clone()
    } else {
      None
    };

    if direct_eval {
      self.direct_eval_outer_slot_names = self.current_slot_names.clone();
      self.direct_eval_outer_slot_immutable = self.current_slot_immutable.clone();
      self.direct_eval_outer_slot_sloppy_immutable_silent =
        self.current_slot_sloppy_immutable_silent.clone();
    }

    let chunk_rc = Arc::new(chunk);
    self.root_chunk = Some(Arc::clone(&chunk_rc));
    self.chunk = chunk_rc;
    self.ip = 0;
    self.current_strict_mode = self.chunk.root_strict;
    self.gc_temp_roots.clear();
    self.yield_star_state = None;
    if self.chunk.root_slot_names.is_empty() {
      if !direct_eval {
        self.locals.clear();
        self.current_slot_types = None;
        self.current_slot_names = None;
        self.current_slot_immutable = None;
        self.current_slot_sloppy_immutable_silent = None;
      }
    } else {
      self.initialize_root_bindings();
    }
    if !direct_eval {
      self.this_value = self
        .globals
        .borrow()
        .properties
        .get(&PropertyKey::String("globalThis".to_string()))
        .and_then(|d| match d {
          PropertyDescriptor::Data { value, .. } => Some(value.clone()),
          _ => None,
        })
        .unwrap_or(Value::Undefined);
    }
    self.begin_opcode_profile_run();

    let result = self.run_risc_loop(true);

    self.chunk = saved_chunk;
    self.ip = saved_ip;
    self.root_chunk = saved_root_chunk;
    self.current_strict_mode = saved_current_strict_mode;
    self.current_closure_env = saved_current_closure_env;
    self.pending_closure_env = saved_pending_closure_env;
    self.gc_temp_roots = saved_gc_temp_roots;
    self.this_value = saved_this_value;
    self.yield_star_state = saved_yield_star_state;
    self.current_arguments_object = saved_current_arguments_object;
    self.current_arguments_mapped_count = saved_current_arguments_mapped_count;
    self.recent_return_value = saved_recent_return_value;
    self.last_uncaught_exception = saved_last_uncaught_exception;
    self.pending_exception = saved_pending_exception;
    self.pending_exception_frame_count = saved_pending_exception_frame_count;
    if direct_eval {
      if let Some(lexical_env) = saved_lexical_env {
        self.lexical_env = lexical_env;
      }
      if let Some(locals) = saved_locals {
        self.locals = locals;
      }
      self.current_slot_types = saved_current_slot_types;
      self.current_slot_names = saved_current_slot_names;
      self.current_slot_immutable = saved_current_slot_immutable;
      self.current_slot_sloppy_immutable_silent = saved_current_slot_sloppy_immutable_silent;
      self.direct_eval_outer_slot_names = saved_direct_eval_outer_slot_names;
      self.direct_eval_outer_slot_immutable = saved_direct_eval_outer_slot_immutable;
      self.direct_eval_outer_slot_sloppy_immutable_silent =
        saved_direct_eval_outer_slot_sloppy_immutable_silent;
    }

    result
  }

  fn initialize_root_bindings(&mut self) {
    let slot_names = self.chunk.root_slot_names.clone();
    let slot_tdz = self.chunk.root_lexical_tdz.clone();
    let slot_types = self.chunk.root_slot_types.clone();
    let slot_immutable = self.chunk.root_slot_immutable.clone();
    let slot_sloppy_immutable_silent = vec![false; slot_names.len()];
    let slot_count = slot_names
      .len()
      .max(slot_tdz.len())
      .max(slot_types.len())
      .max(slot_immutable.len());
    if slot_count == 0 {
      return;
    }

    self.locals = std::iter::repeat_with(|| Rc::new(RefCell::new(Value::Undefined)))
      .take(slot_count)
      .collect();
    for (idx, is_tdz) in slot_tdz.iter().copied().enumerate() {
      if is_tdz && idx < self.locals.len() {
        *self.locals[idx].borrow_mut() = Value::Uninitialized;
      }
    }

    let root_env = Rc::new(RefCell::new(EnvironmentRecord::new(Some(Rc::clone(
      &self.lexical_env,
    )))));
    for (idx, maybe_name) in slot_names.iter().enumerate() {
      if let Some(name) = maybe_name
        && idx < self.locals.len()
      {
        let initialized = !slot_tdz.get(idx).copied().unwrap_or(false);
        if slot_immutable.get(idx).copied().unwrap_or(false) {
          root_env.borrow_mut().create_immutable_binding(
            name.clone(),
            Rc::clone(&self.locals[idx]),
            initialized,
          );
        } else {
          root_env.borrow_mut().create_mutable_binding(
            name.clone(),
            Rc::clone(&self.locals[idx]),
            initialized,
          );
        }
        if initialized {
          let value = self.locals[idx].borrow().clone();
          self.globals.borrow_mut().properties.insert(
            PropertyKey::String(name.clone()),
            PropertyDescriptor::data(value),
          );
        }
      }
    }
    self.lexical_env = root_env;
    self.current_slot_types = Some(slot_types);
    self.current_slot_names = Some(slot_names);
    self.current_slot_immutable = Some(slot_immutable);
    self.current_slot_sloppy_immutable_silent = Some(slot_sloppy_immutable_silent);
  }

  // -----------------------------------------------------------------------
  // Opcode helpers (called from the main loop)
  // -----------------------------------------------------------------------

  pub(super) fn make_iterator_result(
    value: Value,
    done: bool,
  ) -> Value {
    let obj = gc::track_object(Rc::new(RefCell::new(ObjectData::new())));
    obj.borrow_mut().properties.insert(
      PropertyKey::String("value".to_string()),
      PropertyDescriptor::data(value),
    );
    obj.borrow_mut().properties.insert(
      PropertyKey::String("done".to_string()),
      PropertyDescriptor::data(Value::Bool(done)),
    );
    Value::Object(obj)
  }

  // Iterator factories own their state in iterator objects; this helper only
  // creates the `{ value, done }` result returned by generator operations.

  /// Apply ECMAScript `ToObject` coercion.
  ///
  /// # Errors
  ///
  /// Returns a [`crate::errors::RuntimeError`] if the coercion fails.
  pub fn to_object_value(
    &mut self,
    v: Value,
  ) -> Result<Value, RuntimeError> {
    match v {
      Value::Object(_)
      | Value::Array(..)
      | Value::Proxy(_)
      | Value::Closure { .. }
      | Value::BuiltinObject(_, _)
      | Value::Promise(_) => Ok(v),
      Value::Undefined | Value::Null => {
        Err(self.throw_type_error("Cannot convert undefined or null to object"))
      }
      Value::Bool(b) => {
        let ctor = self
          .get_global("Boolean")
          .unwrap_or(Value::Undefined);
        self.construct_value(&ctor, &[Value::Bool(b)])
      }
      Value::Number(n) => {
        let ctor = self
          .get_global("Number")
          .unwrap_or(Value::Undefined);
        self.construct_value(&ctor, &[Value::Number(n)])
      }
      Value::String(s) => {
        let ctor = self
          .get_global("String")
          .unwrap_or(Value::Undefined);
        self.construct_value(&ctor, &[Value::String(s)])
      }
      Value::BigInt(n) => {
        let ctor = self
          .get_global("BigInt")
          .unwrap_or(Value::Undefined);
        self.construct_value(&ctor, &[Value::BigInt(n)])
      }
      Value::Symbol(id, desc) => {
        // Wrap Symbol in an object.
        let mut data = ObjectData::new();
        data.properties.insert(
          PropertyKey::String("[[PrimitiveValue]]".to_string()),
          PropertyDescriptor::data(Value::Symbol(id, desc.clone())),
        );
        data.properties.insert(
          PropertyKey::String("__isSymbol".to_string()),
          PropertyDescriptor::data(Value::Bool(true)),
        );
        if let Some(d) = &desc {
          data.properties.insert(
            PropertyKey::String("description".to_string()),
            PropertyDescriptor::data(Value::String(d.clone())),
          );
        }
        // Try to find Symbol.prototype
        if let Some(proto) = self.realm.intrinsics.symbol_prototype.clone() {
          data.set_prototype_value(proto);
        }
        Ok(Value::Object(gc::track_object(Rc::new(RefCell::new(data)))))
      }
      Value::Uninitialized => Err(self.throw_type_error("Uninitialized value")),
      Value::Builtin(_) => {
        // Builtins are callable values and are returned unchanged here.
        Ok(v)
      }
    }
  }

  /// Parse and evaluate a single expression source string in a forked VM
  /// execution context.
  ///
  /// The expression result is returned directly.
  ///
  /// # Errors
  ///
  /// Returns a [`crate::errors::RuntimeError`] if the evaluation fails.
  pub fn eval_expression_source(
    &mut self,
    source: &str,
  ) -> Result<Value, RuntimeError> {
    let strict = self.active_strict_mode();
    let wrapped = if strict {
      crate::js!(
        format(script),
        "'use strict'; (function() {{ return ({}); }})()",
        source
      )
    } else {
      crate::js!(format(script), "(function() {{ return ({}); }})()", source)
    };
    let session = crate::parser::Parser::parse_source(&wrapped)
      .map_err(|e| RuntimeError::from(format!("parse: {}", e)))?;
    let prog = session.program();
    let chunk = crate::compiler::compile_oxc_repl(prog, &wrapped)
      .map_err(|e| RuntimeError::from(format!("compile: {}", e)))?;
    let mut child = self.fork_execution();
    child.adopt_eval_context(self);
    child.set_current_strict_mode(strict);
    match child.run_nested(chunk, false) {
      Ok(value) => Ok(value),
      Err(err) => {
        if let Some(thrown) = child.last_uncaught_exception.take() {
          Err(self.throw_exception(thrown))
        } else {
          Err(err)
        }
      }
    }
  }

  /// Parse and evaluate a single expression source string in the current VM.
  ///
  /// This is the direct-eval equivalent of [`Vm::eval_expression_source`]:
  /// it preserves the caller's execution context and leaves any new `var`
  /// bindings in the current lexical environment chain.
  pub fn eval_expression_source_direct(
    &mut self,
    source: &str,
  ) -> Result<Value, RuntimeError> {
    let strict = self.active_strict_mode();
    let wrapped = if strict {
      crate::js!(
        format(script),
        "'use strict'; (function() {{ return ({}); }})()",
        source
      )
    } else {
      crate::js!(format(script), "(function() {{ return ({}); }})()", source)
    };
    let session = crate::parser::Parser::parse_source(&wrapped)
      .map_err(|e| RuntimeError::from(format!("parse: {}", e)))?;
    let prog = session.program();
    let chunk = crate::compiler::compile_oxc_repl(prog, &wrapped)
      .map_err(|e| RuntimeError::from(format!("compile: {}", e)))?;
    match self.run_nested(chunk, true) {
      Ok(value) => Ok(value),
      Err(err) => {
        if let Some(thrown) = self.last_uncaught_exception.take() {
          Err(self.throw_exception(thrown))
        } else {
          Err(err)
        }
      }
    }
  }

  /// Apply ECMAScript `ToPrimitive` coercion with an optional preferred hint.
  ///
  /// # Errors
  ///
  /// Returns a [`crate::errors::RuntimeError`] if the coercion fails.
  pub fn to_primitive(
    &mut self,
    v: &Value,
    hint: Option<&str>,
  ) -> Result<Value, RuntimeError> {
    match v {
      Value::Object(_)
      | Value::Array(..)
      | Value::Proxy(_)
      | Value::Closure { .. }
      | Value::Builtin(_)
      | Value::BuiltinObject(_, _)
      | Value::Promise(_) => {
        let temp_root_base = self.gc_temp_roots.len();
        self.gc_temp_roots.push(v.clone());
        if crate::trace::enabled() {
          eprintln!(
            "[trace] to_primitive called on {} with hint={:?}",
            v.type_name(),
            hint
          );
          // Print the full value being coerced to help debug which
          // object instance is visited during ToPrimitive calls.
          crate::trace::value_string("[trace] to_primitive value", v);
        }
        // Try @@toPrimitive if present.
        let to_prim_key = PropertyKey::Symbol(SymbolId::well_known_to_primitive());
        if let Some((desc, _holder)) = self.lookup_property_descriptor(v, &to_prim_key, 0)?
          && let PropertyDescriptor::Data { value: method, .. } = desc
          && matches!(
            method,
            Value::Closure { .. } | Value::Builtin(_) | Value::BuiltinObject(_, _)
          )
        {
          if crate::trace::enabled() {
            crate::trace::value_string("[trace] @@toPrimitive candidate method", &method);
            crate::trace::value_string("[trace] @@toPrimitive receiver", v);
            if let Some(id) = object_identity(v) {
              eprintln!("[trace] @@toPrimitive receiver identity ptr={:#x}", id);
            }
          }
          let rv = self.call_with_this(
            &method,
            v,
            &[Value::String(hint.unwrap_or("default").to_string())],
          )?;
          if crate::trace::enabled() {
            crate::trace::value_string("[trace] @@toPrimitive returned", &rv);
            eprintln!("[trace] @@toPrimitive return type = {}", rv.type_name());
          }
          if !matches!(
            rv,
            Value::Object(_)
              | Value::Array(..)
              | Value::Proxy(_)
              | Value::Closure { .. }
              | Value::Builtin(_)
              | Value::BuiltinObject(_, _)
              | Value::Promise(_)
          ) {
            if crate::trace::enabled() {
              crate::trace::value_string("[trace] @@toPrimitive accepted primitive", &rv);
            }
            self.gc_temp_roots.truncate(temp_root_base);
            return Ok(rv);
          } else if crate::trace::enabled() {
            crate::trace::value_string("[trace] @@toPrimitive produced non-primitive", &rv);
          }
        }

        let order = match hint {
          Some("string") => ["toString", "valueOf"],
          Some("number") => ["valueOf", "toString"],
          _ => ["valueOf", "toString"],
        };
        for name in order {
          let key = PropertyKey::String(name.to_string());
          if let Some((desc, _holder)) = self.lookup_property_descriptor(v, &key, 0)?
            && let PropertyDescriptor::Data { value: method, .. } = desc
            && matches!(
              method,
              Value::Closure { .. } | Value::Builtin(_) | Value::BuiltinObject(_, _)
            )
          {
            if crate::trace::enabled() {
              crate::trace::value_string(&format!("[trace] candidate method '{}':", name), &method);
              crate::trace::value_string("[trace] candidate receiver", v);
              if let Some(id) = object_identity(v) {
                eprintln!("[trace] candidate receiver identity ptr={:#x}", id);
              }
            }
            let rv = self.call_with_this(&method, v, &[])?;
            if crate::trace::enabled() {
              crate::trace::value_string(&format!("[trace] {} returned", name), &rv);
              eprintln!("[trace] {} return type = {}", name, rv.type_name());
            }
            if !matches!(
              rv,
              Value::Object(_)
                | Value::Array(..)
                | Value::Proxy(_)
                | Value::Closure { .. }
                | Value::Builtin(_)
                | Value::BuiltinObject(_, _)
                | Value::Promise(_)
            ) {
              self.gc_temp_roots.truncate(temp_root_base);
              return Ok(rv);
            }
          }
        }
        self.gc_temp_roots.truncate(temp_root_base);
        Err(self.throw_type_error("Cannot convert object to primitive value"))
      }
      _ => Ok(v.clone()),
    }
  }

  /// Apply ECMAScript `ToString` coercion.
  ///
  /// # Errors
  ///
  /// Returns a [`crate::errors::RuntimeError`] if the coercion fails.
  pub fn to_string(
    &mut self,
    v: &Value,
  ) -> Result<String, RuntimeError> {
    match v {
      Value::String(s) => Ok(s.clone()),
      Value::Number(n) => Ok(js_number_to_string(*n)),
      Value::Bool(b) => Ok(if *b {
        "true".to_string()
      } else {
        "false".to_string()
      }),
      Value::Undefined => Ok("undefined".to_string()),
      Value::Null => Ok("null".to_string()),
      Value::BigInt(n) => Ok(n.to_string()),
      Value::Symbol(_, _) => {
        Err(self.throw_type_error("Cannot convert a Symbol value to a string"))
      }
      Value::Promise(_) => Ok("[object Promise]".to_string()),
      _ => {
        let prim = self.to_primitive(v, Some("string"))?;
        self.to_string(&prim)
      }
    }
  }

  /// Return a string reference for a string value, or throw a type error if the value is not a string.
  pub fn to_string_ref<'a>(
    &mut self,
    v: &'a Value,
  ) -> Result<&'a str, RuntimeError> {
    match v {
      Value::String(s) => Ok(s),
      _ => Err(self.throw_type_error("Cannot convert to string reference")),
    }
  }
  /// Return the stored source or native template for a callable's `toString`.
  ///
  /// # Errors
  ///
  /// Returns a [`crate::errors::RuntimeError`] if the value is not a callable.
  pub fn function_to_string(
    &mut self,
    value: &Value,
  ) -> Result<String, RuntimeError> {
    if !Self::is_callable_value(value) {
      return Err(self.throw_type_error("Function.prototype.toString called on non-callable"));
    }
    if let Value::Closure { proto, .. } = value
      && let Some(src) = proto.source_text()
    {
      return Ok(src.to_string());
    }
    Ok(self.format_native_function_source(value))
  }

  fn native_function_name(
    &mut self,
    value: &Value,
  ) -> Option<String> {
    match self.get_property_host(value, &PropertyKey::String("name".to_string())) {
      Ok(Value::String(name)) => Some(name),
      Ok(_) => None,
      Err(_) => None,
    }
  }

  fn format_native_function_source(
    &mut self,
    value: &Value,
  ) -> String {
    let name = self
      .native_function_name(value)
      .unwrap_or_default();
    if name.is_empty() {
      "function () { [native code] }".to_string()
    } else {
      format!("function {}() {{ [native code] }}", name)
    }
  }

  /// Apply ECMAScript `ToNumber` coercion.
  ///
  /// # Errors
  ///
  /// Returns a [`crate::errors::RuntimeError`] if the coercion fails.
  pub fn to_number_value(
    &mut self,
    v: &Value,
  ) -> Result<f64, RuntimeError> {
    match v {
      Value::Number(n) => Ok(*n),
      Value::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
      Value::String(s) => Ok(parse_js_number_string(s)),
      Value::Undefined => Ok(f64::NAN),
      Value::Null => Ok(0.0),
      Value::Uninitialized => {
        Err(self.throw_reference_error("Cannot access lexical binding before initialization"))
      }
      Value::BigInt(_) => Err(self.throw_type_error("Cannot convert BigInt to number")),
      Value::Symbol(_, _) => Err(self.throw_type_error("Cannot convert Symbol to number")),
      _ => {
        let prim = self.to_primitive(v, Some("number"))?;
        self.to_number_value(&prim)
      }
    }
  }

  /// Apply ECMAScript `ToPropertyKey` coercion.
  ///
  /// # Errors
  ///
  /// Returns a [`crate::errors::RuntimeError`] if the coercion fails.
  pub fn to_property_key(
    &mut self,
    v: &Value,
  ) -> Result<PropertyKey, RuntimeError> {
    // Fast path: String values are already property keys, skip ToPrimitive entirely
    if let Value::String(s) = v {
      return Ok(PropertyKey::String(s.clone()));
    }

    match v {
      Value::Symbol(sym, _) => Ok(PropertyKey::Symbol(sym.clone())),
      Value::Object(data) => {
        if let Some(symbol_key) = {
          let d = data.borrow();
          if let Some(PropertyDescriptor::Data {
            value: Value::Symbol(sym, _),
            ..
          }) = d
            .properties
            .get(&PropertyKey::String("[[PrimitiveValue]]".to_string()))
          {
            Some(PropertyKey::Symbol(sym.clone()))
          } else {
            None
          }
        } {
          return Ok(symbol_key);
        }
        let prim = self.to_primitive(v, Some("string"))?;
        match prim {
          Value::Symbol(sym, _) => Ok(PropertyKey::Symbol(sym)),
          _ => Ok(PropertyKey::String(self.to_string(&prim)?)),
        }
      }
      _ => {
        let prim = self.to_primitive(v, Some("string"))?;
        match prim {
          Value::Symbol(sym, _) => Ok(PropertyKey::Symbol(sym)),
          _ => Ok(PropertyKey::String(self.to_string(&prim)?)),
        }
      }
    }
  }

  /// Apply ECMAScript `ToBoolean` coercion.
  pub fn to_boolean(
    &self,
    v: &Value,
  ) -> bool {
    v.is_truthy()
  }

  /// ECMAScript abstract equality comparison (`==`).
  ///
  /// # Errors
  ///
  /// Returns a [`crate::errors::RuntimeError`] if the coercion fails.
  pub fn abstract_equality(
    &mut self,
    x: &Value,
    y: &Value,
  ) -> Result<bool, RuntimeError> {
    // Same-value for identical discriminants
    if std::mem::discriminant(x) == std::mem::discriminant(y) {
      return Ok(values_equal(x, y));
    }

    // null == undefined
    if matches!(x, Value::Null) && matches!(y, Value::Undefined) {
      return Ok(true);
    }
    if matches!(x, Value::Undefined) && matches!(y, Value::Null) {
      return Ok(true);
    }

    // Number == String
    if matches!(x, Value::Number(_)) && matches!(y, Value::String(_)) {
      let yn = self.to_number_value(y)?;
      if let Value::Number(xn) = x {
        return Ok(*xn == yn);
      }
    }
    if matches!(x, Value::String(_)) && matches!(y, Value::Number(_)) {
      let xn = self.to_number_value(x)?;
      if let Value::Number(yn) = y {
        return Ok(xn == *yn);
      }
    }

    // BigInt and String
    if matches!(x, Value::BigInt(_)) && matches!(y, Value::String(_)) {
      if let (Value::BigInt(xn), Value::String(ys)) = (x, y)
        && let Some(yn) = parse_js_bigint_string(ys)
      {
        return Ok(xn == &yn);
      }
      return Ok(false);
    }
    if matches!(x, Value::String(_)) && matches!(y, Value::BigInt(_)) {
      return self.abstract_equality(y, x);
    }

    // BigInt and Number
    if let (Value::BigInt(xn), Value::Number(yn)) = (x, y) {
      if !yn.is_finite() {
        return Ok(false);
      }
      if let Some(yn_big) = BigIntValue::from_f64(*yn) {
        return Ok(xn == &yn_big);
      }
      return Ok(false);
    }
    if let (Value::Number(_), Value::BigInt(_)) = (x, y) {
      return self.abstract_equality(y, x);
    }

    // Boolean -> Number coercion
    if matches!(x, Value::Bool(_)) {
      let xn = if let Value::Bool(b) = x {
        if *b { 1.0 } else { 0.0 }
      } else {
        0.0
      };
      return self.abstract_equality(&Value::Number(xn), y);
    }
    if matches!(y, Value::Bool(_)) {
      let yn = if let Value::Bool(b) = y {
        if *b { 1.0 } else { 0.0 }
      } else {
        0.0
      };
      return self.abstract_equality(x, &Value::Number(yn));
    }

    // Object compared with primitive: ToPrimitive on object
    if matches!(
      x,
      Value::Object(_)
        | Value::Array(..)
        | Value::Closure { .. }
        | Value::Builtin(_)
        | Value::BuiltinObject(_, _)
    ) && matches!(
      y,
      Value::String(_) | Value::Number(_) | Value::BigInt(_) | Value::Symbol(_, _) | Value::Bool(_)
    ) {
      let xprim = self.to_primitive(x, None)?;
      return self.abstract_equality(&xprim, y);
    }
    if matches!(
      y,
      Value::Object(_)
        | Value::Array(..)
        | Value::Closure { .. }
        | Value::Builtin(_)
        | Value::BuiltinObject(_, _)
    ) && matches!(
      x,
      Value::String(_) | Value::Number(_) | Value::BigInt(_) | Value::Symbol(_, _) | Value::Bool(_)
    ) {
      let yprim = self.to_primitive(y, None)?;
      return self.abstract_equality(x, &yprim);
    }

    Ok(false)
  }

  /// ECMAScript Abstract Relational Comparison with left-to-right
  /// `ToPrimitive` evaluation. An unordered comparison (for example one
  /// involving `NaN`) produces `false`.
  pub fn abstract_relational_comparison(
    &mut self,
    left: &Value,
    right: &Value,
  ) -> Result<Option<bool>, RuntimeError> {
    let left = self.to_primitive(left, Some("number"))?;
    let right = self.to_primitive(right, Some("number"))?;

    if let (Value::String(left), Value::String(right)) = (&left, &right) {
      return Ok(Some(
        left
          .encode_utf16()
          .cmp(right.encode_utf16())
          .is_lt(),
      ));
    }

    Ok(
      left
        .partial_cmp(&right)
        .map(std::cmp::Ordering::is_lt),
    )
  }

  /// Restore the caller's frame and return the callee's result.
  pub(super) fn execute_return(
    &mut self,
    result: Value,
  ) -> Result<Value, RuntimeError> {
    let frame = self
      .call_stack
      .pop()
      .ok_or("Return without call frame")?;

    // Runtime return-type check intentionally disabled in JS-first mode.
    // TypeScript return annotations are erased at runtime.

    self.current_slot_types = frame.slot_types;
    self.current_slot_names = frame.slot_names;
    self.current_slot_immutable = frame.slot_immutable;
    self.current_slot_sloppy_immutable_silent = frame.slot_sloppy_immutable_silent;
    self.chunk = frame.chunk;
    self.ip = frame.ip;
    self.locals = frame.locals;
    self.this_value = frame.this_value;
    self.current_closure_env = frame.closure_env;
    self.lexical_env = frame.lexical_env;
    self.yield_star_state = frame.yield_star_state;
    self.current_arguments_object = frame.arguments_object;
    self.current_arguments_mapped_count = frame.arguments_mapped_count;
    self.current_strict_mode = frame.is_strict;
    self.prune_stale_handlers();

    Ok(result)
  }

  fn delete_identifier(
    &mut self,
    name_val: Value,
  ) -> Result<Value, RuntimeError> {
    let name = if let Value::String(s) = name_val {
      s
    } else {
      return Err(self.throw_type_error("DeleteIdentifier requires string constant"));
    };
    let mut env = Some(Rc::clone(&self.lexical_env));
    while let Some(current) = env {
      let (has_binding, binding_object, with_environment, outer) = {
        let borrowed = current.borrow();
        (
          borrowed.has_binding(&name),
          borrowed.binding_object(),
          borrowed.is_with_environment(),
          borrowed.outer(),
        )
      };
      if has_binding {
        return Ok(Value::Bool(false));
      }
      if let Some(binding_object) = binding_object {
        let key = PropertyKey::String(name.clone());
        let found = self.has_property_host(&binding_object, &key)?;
        if found && (!with_environment || !self.is_unscopable_binding(&binding_object, &name)?) {
          let deleted = self.delete_property_host(&binding_object, &key)?;
          self.bump_object_generation(&binding_object);
          return Ok(Value::Bool(deleted));
        }
      }
      env = outer;
    }

    let global_obj = Value::Object(Rc::clone(&self.globals));
    let key = PropertyKey::String(name.clone());
    let deleted = self.delete_property_host(&global_obj, &key)?;
    self.bump_object_generation(&global_obj);
    Ok(Value::Bool(deleted))
  }

  fn iterator_next(
    &mut self,
    iterator: Value,
  ) -> Result<Value, RuntimeError> {
    let next_fn = self.get_property_host(&iterator, &PropertyKey::String("next".to_string()))?;
    let result = self.call_value_with_this(&next_fn, &iterator, &[])?;
    let done = self.get_property_host(&result, &PropertyKey::String("done".to_string()))?;
    if self.to_boolean(&done) {
      let done_key = PropertyKey::String("__tsblitz_iter_done".to_string());
      match &iterator {
        Value::Object(obj) => {
          obj
            .borrow_mut()
            .properties
            .insert(done_key, PropertyDescriptor::data(Value::Bool(true)));
        }
        Value::Array(_, props) | Value::Closure { props, .. } | Value::BuiltinObject(_, props) => {
          props
            .borrow_mut()
            .properties
            .insert(done_key, PropertyDescriptor::data(Value::Bool(true)));
        }
        _ => {}
      }
    }
    Ok(result)
  }

  fn create_yield_star_state(
    &mut self,
    iterable: Value,
  ) -> Result<YieldStarState, RuntimeError> {
    let iterator_method = self.get_property_host(
      &iterable,
      &PropertyKey::Symbol(SymbolId::well_known_iterator()),
    )?;
    if !Self::is_callable_value(&iterator_method) {
      return Err(self.throw_type_error("Yield-star value is not iterable"));
    }

    let iterator = self.call_value_with_this(&iterator_method, &iterable, &[])?;
    if !iterator.is_object() {
      return Err(self.throw_type_error("Iterator method must return an object"));
    }

    let next_method =
      self.get_property_host(&iterator, &PropertyKey::String("next".to_string()))?;
    if !Self::is_callable_value(&next_method) {
      return Err(self.throw_type_error("Iterator next method is not callable"));
    }

    Ok(YieldStarState {
      iterator,
      next_method,
      resume: Some(YieldStarResume::Next(Value::Undefined)),
    })
  }

  pub(crate) fn begin_generator_return(
    &mut self,
    value: Value,
  ) -> bool {
    let Some(handler_index) = self.handlers.iter().rposition(|handler| {
      handler.frame_count == self.call_stack.len()
        && Arc::ptr_eq(&handler.chunk, &self.chunk)
        && handler.finally.is_some()
    }) else {
      return false;
    };
    let handler = self.handlers.remove(handler_index);
    self.handlers.truncate(handler_index);
    let finally = handler.finally.expect("checked above");
    ensure_slot(&mut self.locals, finally.return_slot);
    ensure_slot(&mut self.locals, finally.did_return_slot);
    *self.locals[finally.return_slot].borrow_mut() = value;
    *self.locals[finally.did_return_slot].borrow_mut() = Value::Bool(true);
    self.lexical_env = handler.saved_lexical_env;
    self.ip = finally.finalizer_ip;
    self.yield_star_state = None;
    self
      .generator_return_frame_count
      .get_or_insert(self.call_stack.len());
    true
  }

  fn resume_yield_star(&mut self) -> Result<YieldStarStep, RuntimeError> {
    let mut state = self
      .yield_star_state
      .take()
      .ok_or_else(|| RuntimeError::from("Missing yield-star state"))?;
    let resume = state
      .resume
      .take()
      .ok_or_else(|| RuntimeError::from("Missing yield-star resume operation"))?;
    let is_return = matches!(resume, YieldStarResume::Return(_));

    let result = match resume {
      YieldStarResume::Next(value) => {
        self.call_value_with_this(&state.next_method, &state.iterator, &[value])?
      }
      YieldStarResume::Throw(value) => {
        let throw_method =
          self.get_property_host(&state.iterator, &PropertyKey::String("throw".to_string()))?;
        if matches!(throw_method, Value::Undefined | Value::Null) {
          let return_method =
            self.get_property_host(&state.iterator, &PropertyKey::String("return".to_string()))?;
          if !matches!(return_method, Value::Undefined | Value::Null) {
            if !Self::is_callable_value(&return_method) {
              return Err(self.throw_type_error("Iterator return method is not callable"));
            }
            let close_result = self.call_value_with_this(&return_method, &state.iterator, &[])?;
            if !close_result.is_object() {
              return Err(self.throw_type_error("Iterator return method must return an object"));
            }
          }
          return Err(self.throw_type_error("Delegated iterator has no throw method"));
        }
        if !Self::is_callable_value(&throw_method) {
          return Err(self.throw_type_error("Iterator throw method is not callable"));
        }
        self.call_value_with_this(&throw_method, &state.iterator, &[value])?
      }
      YieldStarResume::Return(value) => {
        let return_method =
          self.get_property_host(&state.iterator, &PropertyKey::String("return".to_string()))?;
        if matches!(return_method, Value::Undefined | Value::Null) {
          return Ok(YieldStarStep::Return(value));
        }
        if !Self::is_callable_value(&return_method) {
          return Err(self.throw_type_error("Iterator return method is not callable"));
        }
        self.call_value_with_this(&return_method, &state.iterator, &[value])?
      }
    };

    if !result.is_object() {
      return Err(self.throw_type_error("Iterator result must be an object"));
    }
    let done = self.get_property_host(&result, &PropertyKey::String("done".to_string()))?;
    if self.to_boolean(&done) {
      let value = self.get_property_host(&result, &PropertyKey::String("value".to_string()))?;
      if is_return {
        Ok(YieldStarStep::Return(value))
      } else {
        Ok(YieldStarStep::Complete(value))
      }
    } else {
      self.yield_star_state = Some(state);
      Ok(YieldStarStep::Yield(result))
    }
  }

  fn iterator_close(
    &mut self,
    iterator: Value,
  ) -> Result<Value, RuntimeError> {
    if matches!(iterator, Value::Undefined | Value::Null) {
      return Ok(iterator);
    }
    let done_key = PropertyKey::String("__tsblitz_iter_done".to_string());
    let already_done = match &iterator {
      Value::Object(obj) => obj
        .borrow()
        .properties
        .get(&done_key)
        .is_some_and(|desc| {
          matches!(
            desc,
            PropertyDescriptor::Data {
              value: Value::Bool(true),
              ..
            }
          )
        }),
      Value::Array(_, props) | Value::Closure { props, .. } | Value::BuiltinObject(_, props) => {
        props
          .borrow()
          .properties
          .get(&done_key)
          .is_some_and(|desc| {
            matches!(
              desc,
              PropertyDescriptor::Data {
                value: Value::Bool(true),
                ..
              }
            )
          })
      }
      _ => false,
    };
    if already_done {
      return Ok(iterator);
    }
    let return_fn =
      self.get_property_host(&iterator, &PropertyKey::String("return".to_string()))?;
    if matches!(return_fn, Value::Undefined | Value::Null) {
      return Ok(iterator);
    }
    if !Self::is_callable_value(&return_fn) {
      return Err(self.throw_type_error("Iterator return is not callable"));
    }
    let ret = self.call_value_with_this(&return_fn, &iterator, &[])?;
    if !ret.is_object() {
      return Err(self.throw_type_error("Iterator return result is not an object"));
    }
    Ok(iterator)
  }

  pub(super) fn bump_object_generation(
    &mut self,
    obj: &Value,
  ) {
    let ptr = match obj {
      Value::Object(m) => Some(m.borrow().identity as usize),
      Value::Array(_, props) => Some(props.borrow().identity as usize),
      Value::Closure { props, .. } => Some(props.borrow().identity as usize),
      Value::BuiltinObject(_, props) => Some(props.borrow().identity as usize),
      _ => None,
    };
    if let Some(p) = ptr {
      *self.obj_generations.entry(p).or_insert(0) += 1;
    }
  }

  fn set_global_value(
    &mut self,
    name: &str,
    value: Value,
  ) -> Result<(), RuntimeError> {
    // Root bindings mirrored onto the global object have already been written
    // by the StoreLocal immediately preceding SetGlobal. Avoid resolving and
    // writing that same lexical cell a second time in the common script path.
    let is_root_binding = self.call_stack.is_empty()
      && self.direct_eval_outer_slot_names.is_none()
      && !self.lexical_env.borrow().has_object_environment()
      && self
        .current_slot_names
        .as_ref()
        .is_some_and(|names| {
          names
            .iter()
            .any(|maybe_name| maybe_name.as_deref() == Some(name))
        });
    if is_root_binding {
      self.globals.borrow_mut().properties.insert(
        PropertyKey::String(name.to_string()),
        PropertyDescriptor::data(value),
      );
      return Ok(());
    }

    if self.set_lexical_identifier_value(name, value.clone())?
      || self.set_current_frame_local_by_name(name, value.clone())?
    {
      return Ok(());
    }
    let key = PropertyKey::String(name.to_string());
    let current = self
      .globals
      .borrow()
      .properties
      .get(&key)
      .cloned();
    match current {
      Some(PropertyDescriptor::Data {
        writable: false, ..
      }) => {
        if self.is_strict_mode() {
          return Err(
            self.throw_type_error(&format!("Cannot assign to read only global '{name}'")),
          );
        }
      }
      Some(PropertyDescriptor::Accessor { .. }) => {
        let global_obj = Value::Object(Rc::clone(&self.globals));
        self.set_property_tracked(&global_obj, &key, value)?;
      }
      None if self.is_strict_mode() => {
        return Err(self.throw_reference_error(&format!("{name} is not defined")));
      }
      _ => {
        self
          .globals
          .borrow_mut()
          .properties
          .insert(key, PropertyDescriptor::data(value));
      }
    }
    Ok(())
  }

  // -----------------------------------------------------------------------
  // Stack helpers
  // -----------------------------------------------------------------------

  #[inline(always)]
  fn string_constant(
    &self,
    idx: usize,
  ) -> Result<String, RuntimeError> {
    match self.chunk.constants.get(idx) {
      Some(crate::bytecode::Constant::String(value)) => Ok(value.clone()),
      Some(_) => Err(RuntimeError::from("Constant is not a string")),
      None => Err(RuntimeError::from("String constant index out of bounds")),
    }
  }

  #[inline(always)]
  fn module_export_descriptor(
    &self,
    index: usize,
  ) -> Result<(usize, String), RuntimeError> {
    let descriptor = self.string_constant(index)?;
    let (module, name) = descriptor
      .split_once('\0')
      .ok_or_else(|| RuntimeError::from("Invalid module export descriptor"))?;
    let module = module
      .parse::<usize>()
      .map_err(|_| RuntimeError::from("Invalid module export index"))?;
    Ok((module, name.to_string()))
  }

  pub(super) fn dump_diag(
    &self,
    label: &str,
    offender: &Value,
  ) {
    if !crate::trace::enabled() {
      return;
    }
    eprintln!("[diag] {label}: actual={}", offender.type_name());
    crate::trace::value_string("[diag] offending value", offender);
    let start = self.ip.saturating_sub(8);
    let end = usize::min(self.ip + 8, self.chunk.code.len());
    for ip in start..end {
      let instruction = self
        .chunk
        .disassemble_at(ip)
        .unwrap_or_else(|error| format!("{ip:#06} <{error}>"));
      eprintln!("[diag] {instruction}");
    }
  }

  /// Apply ECMAScript `instanceof` semantics.
  pub fn instance_of(
    &mut self,
    value: &Value,
    constructor: &Value,
  ) -> Result<bool, RuntimeError> {
    // Per spec: if constructor has @@hasInstance, call it.
    let has_instance_key = PropertyKey::Symbol(SymbolId::well_known_has_instance());
    if let Ok(method) = get_property(constructor, &has_instance_key, 0)
      && matches!(
        method,
        Value::Closure { .. } | Value::Builtin(_) | Value::BuiltinObject(_, _)
      )
    {
      let result = self.call_with_this(&method, constructor, std::slice::from_ref(value))?;
      return Ok(result.is_truthy());
    }

    // Only objects have prototype chains.
    match value {
      Value::Object(_)
      | Value::Array(..)
      | Value::Closure { .. }
      | Value::BuiltinObject(_, _)
      | Value::Promise(_) => {}
      _ => return Ok(false),
    }

    // Get constructor.prototype
    let proto_key = PropertyKey::String("prototype".to_string());
    let ctor_proto = get_property(constructor, &proto_key, 0)?;
    // prototype must be an object
    match ctor_proto {
      Value::Object(_) | Value::Array(..) | Value::Closure { .. } | Value::BuiltinObject(_, _) => {}
      _ => return Ok(false),
    }

    // Walk value's prototype chain
    let mut current = match value {
      Value::Object(d) => d.borrow().get_prototype(),
      Value::Array(_, props) => props.borrow().get_prototype(),
      Value::Closure { props, .. } => props.borrow().get_prototype(),
      Value::BuiltinObject(_, props) => props.borrow().get_prototype(),
      Value::Promise(_) => self.promise_prototype_value(),
      _ => Value::Null,
    };
    let mut seen: HashSet<usize> = HashSet::new();

    while !matches!(current, Value::Null) {
      if values_equal(&current, &ctor_proto) {
        return Ok(true);
      }
      let Some(identity) = object_identity(&current) else {
        break;
      };
      if !seen.insert(identity) {
        return Ok(false);
      }
      // advance
      current = match &current {
        Value::Object(d) => d.borrow().get_prototype(),
        Value::Array(_, props) => props.borrow().get_prototype(),
        Value::Closure { props, .. } => props.borrow().get_prototype(),
        Value::BuiltinObject(_, props) => props.borrow().get_prototype(),
        _ => Value::Null,
      };
    }

    Ok(false)
  }

  /// ECMAScript `Object.prototype.isPrototypeOf` semantics.
  pub fn is_prototype_of(
    &mut self,
    proto: &Value,
    value: &Value,
  ) -> Result<bool, RuntimeError> {
    let target_proto = match proto {
      Value::Object(_)
      | Value::Array(..)
      | Value::Closure { .. }
      | Value::BuiltinObject(_, _)
      | Value::Promise(_) => proto.clone(),
      _ => return Ok(false),
    };

    let mut current = match value {
      Value::Object(d) => d.borrow().get_prototype(),
      Value::Array(_, props) => props.borrow().get_prototype(),
      Value::Closure { props, .. } => props.borrow().get_prototype(),
      Value::BuiltinObject(_, props) => props.borrow().get_prototype(),
      Value::Promise(_) => self.promise_prototype_value(),
      _ => return Ok(false),
    };

    let mut seen: HashSet<usize> = HashSet::new();
    while !matches!(current, Value::Null) {
      if values_equal(&current, &target_proto) {
        return Ok(true);
      }
      let Some(identity) = object_identity(&current) else {
        break;
      };
      if !seen.insert(identity) {
        return Ok(false);
      }
      current = match &current {
        Value::Object(d) => d.borrow().get_prototype(),
        Value::Array(_, props) => props.borrow().get_prototype(),
        Value::Closure { props, .. } => props.borrow().get_prototype(),
        Value::BuiltinObject(_, props) => props.borrow().get_prototype(),
        _ => Value::Null,
      };
    }
    Ok(false)
  }

  /// 'in' operator helper. Returns true if property (after ToPropertyKey) is in object.
  ///
  /// # Errors
  ///
  /// Returns a [`crate::errors::RuntimeError`] if the operation fails.
  pub fn in_property(
    &mut self,
    property: &Value,
    object: &Value,
  ) -> Result<bool, RuntimeError> {
    let key = self.to_property_key(property)?;
    Ok(
      self
        .lookup_property_descriptor(object, &key, 0)?
        .is_some(),
    )
  }

  /// typeof helper returning the type string for a value.
  pub fn typeof_value(
    &self,
    value: &Value,
  ) -> String {
    // `typeof null` is 'object'
    value.type_name().to_string()
  }
}

// ---------------------------------------------------------------------------
// Free functions
// ---------------------------------------------------------------------------

/// Return own keys for a value when applicable.
fn list_own_keys_of_value(v: &Value) -> Option<Vec<String>> {
  match v {
    Value::Object(data) => Some(
      data
        .borrow()
        .properties
        .keys()
        .filter_map(|k| match k {
          PropertyKey::String(s) => Some(s.clone()),
          PropertyKey::Symbol(_) => None,
        })
        .collect(),
    ),
    Value::Closure { props, .. } => Some(
      props
        .borrow()
        .properties
        .keys()
        .filter_map(|k| match k {
          PropertyKey::String(s) => Some(s.clone()),
          PropertyKey::Symbol(_) => None,
        })
        .collect(),
    ),
    Value::Proxy(proxy) => {
      let proxy = proxy.borrow();
      if proxy.is_revoked() {
        Some(Vec::new())
      } else {
        list_own_keys_of_value(&proxy.target)
      }
    }
    _ => None,
  }
}

/// Grow a locals vector so that `slot` is a valid index.
#[inline(always)]
pub(super) fn ensure_slot(
  locals: &mut Vec<SharedValue>,
  slot: usize,
) {
  if slot >= locals.len() {
    locals.resize_with(slot + 1, || Rc::new(RefCell::new(Value::Undefined)));
  }
}

pub(super) fn object_identity(obj: &Value) -> Option<usize> {
  match obj {
    Value::Object(map) => Some(map.borrow().identity as usize),
    // Use the array's props object identity so IC keys remain stable across
    // dense storage reallocations.
    Value::Array(_, props) => Some(props.borrow().identity as usize),
    Value::Proxy(proxy) => Some(Rc::as_ptr(proxy) as usize),
    Value::Closure { props, .. } => Some(props.borrow().identity as usize),
    Value::BuiltinObject(_, props) => Some(props.borrow().identity as usize),
    _ => None,
  }
}

/// Reference-equality semantics for objects/arrays/closures, structural
/// equality for primitives. Matches JavaScript `===` behaviour.
pub(super) fn values_equal(
  a: &Value,
  b: &Value,
) -> bool {
  match (a, b) {
    (Value::Undefined, Value::Undefined) => true,
    (Value::Number(x), Value::Number(y)) => {
      // IEEE 754: NaN !== NaN, +0 === -0
      x == y
    }
    (Value::String(x), Value::String(y)) => x == y,
    (Value::Bool(x), Value::Bool(y)) => x == y,
    (Value::BigInt(x), Value::BigInt(y)) => x == y,
    (Value::Symbol(x, _), Value::Symbol(y, _)) => x == y,
    (Value::Null, Value::Null) => true,
    // Reference equality for heap objects.
    (Value::Object(am), Value::Object(bm)) => Rc::ptr_eq(am, bm),
    (Value::Array(ar, _), Value::Array(br, _)) => Rc::ptr_eq(ar, br),
    (Value::Proxy(ap), Value::Proxy(bp)) => Rc::ptr_eq(ap, bp),
    (Value::Closure { props: ap, .. }, Value::Closure { props: bp, .. }) => Rc::ptr_eq(ap, bp),
    (Value::BuiltinObject(_, ap), Value::BuiltinObject(_, bp)) => Rc::ptr_eq(ap, bp),
    (Value::Promise(ap), Value::Promise(bp)) => Rc::ptr_eq(ap, bp),
    (Value::Builtin(af), Value::Builtin(bf)) => Rc::ptr_eq(af, bf),
    _ => false,
  }
}

/// ECMAScript SameValue semantics used by `Object.is`.
pub fn same_value(
  a: &Value,
  b: &Value,
) -> bool {
  match (a, b) {
    (Value::Undefined, Value::Undefined) => true,
    (Value::Null, Value::Null) => true,
    (Value::Bool(x), Value::Bool(y)) => x == y,
    (Value::String(x), Value::String(y)) => x == y,
    (Value::BigInt(x), Value::BigInt(y)) => x == y,
    (Value::Symbol(x, _), Value::Symbol(y, _)) => x == y,
    (Value::Number(x), Value::Number(y)) => {
      if x.is_nan() && y.is_nan() {
        true
      } else if *x == 0.0 && *y == 0.0 {
        x.is_sign_positive() == y.is_sign_positive()
      } else {
        x == y
      }
    }
    (Value::Object(am), Value::Object(bm)) => Rc::ptr_eq(am, bm),
    (Value::Array(ar, _), Value::Array(br, _)) => Rc::ptr_eq(ar, br),
    (Value::Proxy(ap), Value::Proxy(bp)) => Rc::ptr_eq(ap, bp),
    (Value::Closure { props: ap, .. }, Value::Closure { props: bp, .. }) => Rc::ptr_eq(ap, bp),
    (Value::BuiltinObject(_, ap), Value::BuiltinObject(_, bp)) => Rc::ptr_eq(ap, bp),
    (Value::Promise(ap), Value::Promise(bp)) => Rc::ptr_eq(ap, bp),
    _ => false,
  }
}

pub(super) fn array_index_from_key(key: &PropertyKey) -> Option<usize> {
  let PropertyKey::String(s) = key else {
    return None;
  };
  if s.is_empty() {
    return None;
  }
  if s != "0" && s.starts_with('0') {
    return None;
  }
  let idx = s.parse::<u32>().ok()?;
  if idx == u32::MAX {
    return None;
  }
  if !crate::runtime_utils::decimal_u64_eq_str(idx as u64, s) {
    return None;
  }
  Some(idx as usize)
}

pub(super) fn array_length(props: &ObjectData) -> usize {
  props
    .properties
    .get(&PropertyKey::String("length".to_string()))
    .and_then(|desc| match desc {
      PropertyDescriptor::Data {
        value: Value::Number(n),
        ..
      } if n.is_finite() && *n >= 0.0 => Some(*n as usize),
      _ => None,
    })
    .unwrap_or(0)
}

/// Walk the prototype chain with cycle detection.
pub(super) fn get_property(
  obj: &Value,
  key: &PropertyKey,
  depth: usize,
) -> Result<Value, RuntimeError> {
  if depth > MAX_PROTOTYPE_DEPTH {
    return Err(RuntimeError::from(
      "Prototype chain cycle detected or too deep",
    ));
  }

  match obj {
    Value::Object(data) => {
      let d = data.borrow();
      if let Some(desc) = d.properties.get(key) {
        return match desc {
          PropertyDescriptor::Data { value, .. } => Ok(value.clone()),
          PropertyDescriptor::Accessor { get, .. } => {
            if get.is_some() {
              // Accessor invocation is handled in Vm::get_property_value.
              Ok(Value::Undefined)
            } else {
              Ok(Value::Undefined)
            }
          }
        };
      }
      let proto = d.get_prototype();
      if !matches!(proto, Value::Null) {
        return get_property(&proto, key, depth + 1);
      }
      Ok(Value::Undefined)
    }
    Value::Array(arr, props) => {
      let props_ref = props.borrow();
      if let Some(desc) = props_ref.properties.get(key) {
        return match desc {
          PropertyDescriptor::Data { value, .. } => Ok(value.clone()),
          PropertyDescriptor::Accessor { get, .. } => {
            if get.is_some() {
              Ok(Value::Undefined)
            } else {
              Ok(Value::Undefined)
            }
          }
        };
      }
      if let Some(i) = array_index_from_key(key)
        && i < array_length(&props_ref)
        && let Some(opt_value) = arr.borrow().get(i).cloned()
        && let Some(value) = opt_value
      {
        return Ok(value);
      }
      let proto = props_ref.get_prototype();
      if !matches!(proto, Value::Null) {
        return get_property(&proto, key, depth + 1);
      }
      Ok(Value::Undefined)
    }
    Value::Closure { props, .. } => {
      let p = props.borrow();
      if let Some(desc) = p.properties.get(key) {
        return match desc {
          PropertyDescriptor::Data { value, .. } => Ok(value.clone()),
          PropertyDescriptor::Accessor { get, .. } => {
            if let Some(Value::Closure { .. } | Value::Builtin(_)) = get {
              Ok(Value::Undefined)
            } else {
              Ok(Value::Undefined)
            }
          }
        };
      }
      let proto = p.get_prototype();
      if !matches!(proto, Value::Null) {
        return get_property(&proto, key, depth + 1);
      }
      Ok(Value::Undefined)
    }
    Value::BuiltinObject(_, props) => {
      let p = props.borrow();
      if let Some(desc) = p.properties.get(key) {
        return match desc {
          PropertyDescriptor::Data { value, .. } => Ok(value.clone()),
          PropertyDescriptor::Accessor { get, .. } => {
            if let Some(Value::Closure { .. } | Value::Builtin(_)) = get {
              Ok(Value::Undefined)
            } else {
              Ok(Value::Undefined)
            }
          }
        };
      }
      let proto = p.get_prototype();
      if !matches!(proto, Value::Null) {
        return get_property(&proto, key, depth + 1);
      }
      Ok(Value::Undefined)
    }
    _ => Ok(Value::Undefined),
  }
}

/// Sets the property with the given key on the given object to the given value.
pub(super) fn set_property(
  obj: &Value,
  key: &PropertyKey,
  value: Value,
) -> Result<(), RuntimeError> {
  match obj {
    Value::Object(map) => {
      let mut m = map.borrow_mut();
      if let Some(desc) = m.properties.get_mut(key) {
        match desc {
          PropertyDescriptor::Data {
            value: v, writable, ..
          } => {
            if !*writable {
              return Ok(());
            }
            *v = value;
            return Ok(());
          }
          PropertyDescriptor::Accessor { set, .. } => {
            if set.is_some() {
              return Ok(());
            }
            return Ok(());
          }
        }
      }
      if !m.extensible {
        return Ok(());
      }
      m.properties
        .insert(key.clone(), PropertyDescriptor::data(value));
      Ok(())
    }
    Value::Array(arr, props) => {
      if let Some(i) = array_index_from_key(key) {
        let mut p = props.borrow_mut();
        let current_len = array_length(&p);
        let next_len = current_len.max(i.saturating_add(1));
        let dense_limit = arr
          .borrow()
          .len()
          .saturating_add(MAX_DENSE_ARRAY_GROWTH);
        if i <= dense_limit {
          let mut a = arr.borrow_mut();
          if i >= a.len() {
            a.resize(i + 1, None);
          }
          a[i] = Some(value);
          p.properties.insert(
            PropertyKey::String("length".to_string()),
            PropertyDescriptor::data(Value::Number(usize_to_f64(next_len))),
          );
        } else {
          p.properties
            .insert(key.clone(), PropertyDescriptor::data(value));
          p.properties.insert(
            PropertyKey::String("length".to_string()),
            PropertyDescriptor::data(Value::Number(usize_to_f64(next_len))),
          );
        }
        return Ok(());
      }
      // Special handling: setting array.length truncates the dense store.
      if matches!(key, PropertyKey::String(s) if s == "length")
        && let Value::Number(new_len_f) = value
      {
        let new_len = if new_len_f.is_nan() || new_len_f < 0.0 {
          0usize
        } else {
          new_len_f as usize
        };
        let mut a = arr.borrow_mut();
        if new_len < a.len() {
          a.truncate(new_len);
        }
        let mut p = props.borrow_mut();
        p.properties.insert(
          PropertyKey::String("length".to_string()),
          PropertyDescriptor::data(Value::Number(usize_to_f64(new_len))),
        );
        return Ok(());
      }
      let mut p = props.borrow_mut();
      if let Some(desc) = p.properties.get_mut(key) {
        match desc {
          PropertyDescriptor::Data {
            value: v, writable, ..
          } => {
            if !*writable {
              return Ok(());
            }
            *v = value;
            return Ok(());
          }
          PropertyDescriptor::Accessor { set, .. } => {
            if set.is_some() {
              return Ok(());
            }
            return Ok(());
          }
        }
      }
      if !p.extensible {
        return Ok(());
      }
      p.properties
        .insert(key.clone(), PropertyDescriptor::data(value));
      Ok(())
    }
    Value::Closure { props, .. } => {
      let mut p = props.borrow_mut();
      if let Some(desc) = p.properties.get_mut(key) {
        match desc {
          PropertyDescriptor::Data {
            value: v, writable, ..
          } => {
            if !*writable {
              return Ok(());
            }
            *v = value;
            return Ok(());
          }
          PropertyDescriptor::Accessor { set, .. } => {
            if set.is_some() {
              return Ok(());
            }
            return Ok(());
          }
        }
      }
      if !p.extensible {
        return Ok(());
      }
      p.properties
        .insert(key.clone(), PropertyDescriptor::data(value));
      Ok(())
    }
    Value::BuiltinObject(_, props) => {
      let mut p = props.borrow_mut();
      if let Some(desc) = p.properties.get_mut(key) {
        match desc {
          PropertyDescriptor::Data {
            value: v, writable, ..
          } => {
            if !*writable {
              return Ok(());
            }
            *v = value;
            return Ok(());
          }
          PropertyDescriptor::Accessor { set, .. } => {
            if set.is_some() {
              return Ok(());
            }
            return Ok(());
          }
        }
      }
      if !p.extensible {
        return Ok(());
      }
      p.properties
        .insert(key.clone(), PropertyDescriptor::data(value));
      Ok(())
    }
    _ => {
      // Non-object property assignments are no-ops in sloppy mode.
      //
      // this should probably check the strict mode behavior as well
      Ok(())
    }
  }
}

/// Build a module namespace object from exported bindings.
///
/// Creates an object with all exported names as properties, excluding the "*" wildcard export.
pub(crate) fn build_module_namespace_object(exports: &HashMap<String, SharedValue>) -> Value {
  let namespace = gc::track_object(Rc::new(RefCell::new(ObjectData::new())));
  let mut names: Vec<String> = exports
    .keys()
    .filter(|name| name.as_str() != "*")
    .cloned()
    .collect();
  names.sort();
  for name in names {
    let Some(slot) = exports.get(&name).cloned() else {
      continue;
    };
    let getter: BuiltinFn = Rc::new(move |_vm, _args| Ok(slot.borrow().clone()));
    namespace.borrow_mut().properties.insert(
      PropertyKey::String(name),
      PropertyDescriptor::Accessor {
        get: Some(Value::Builtin(getter)),
        set: None,
        enumerable: true,
        configurable: false,
      },
    );
  }
  namespace.borrow_mut().properties.insert(
    PropertyKey::Symbol(SymbolId::well_known_to_string_tag()),
    PropertyDescriptor::Data {
      value: Value::String("Module".to_string()),
      writable: false,
      enumerable: false,
      configurable: false,
    },
  );
  // Namespace objects are non-extensible per the ECMAScript specification.
  namespace.borrow_mut().extensible = false;
  Value::Object(namespace)
}

impl PartialOrd for Value {
  fn partial_cmp(
    &self,
    other: &Value,
  ) -> Option<std::cmp::Ordering> {
    match (self, other) {
      // Both numbers: numeric comparison
      (Value::Number(a), Value::Number(b)) => a.partial_cmp(b),
      // Both strings: string comparison
      (Value::String(a), Value::String(b)) => a.partial_cmp(b),
      (Value::BigInt(a), Value::BigInt(b)) => a.partial_cmp(b),
      // Number and String: convert string to number
      (Value::Number(a), Value::String(b)) => {
        let b_num = parse_js_number_string(b);
        if b_num.is_nan() {
          None
        } else {
          a.partial_cmp(&b_num)
        }
      }
      // String and Number: convert string to number
      (Value::String(a), Value::Number(b)) => {
        let a_num = parse_js_number_string(a);
        if a_num.is_nan() {
          None
        } else {
          a_num.partial_cmp(b)
        }
      }
      (Value::BigInt(a), Value::Number(b)) => a.to_f64().and_then(|x| x.partial_cmp(b)),
      (Value::Number(a), Value::BigInt(b)) => b.to_f64().and_then(|x| a.partial_cmp(&x)),
      (Value::BigInt(a), Value::String(b)) => {
        parse_js_bigint_string(b).and_then(|bb| a.partial_cmp(&bb))
      }
      (Value::String(a), Value::BigInt(b)) => {
        parse_js_bigint_string(a).and_then(|aa| aa.partial_cmp(b))
      }
      // Boolean: convert to number (1.0 or 0.0)
      (Value::Bool(a), other) => {
        let a_num = if *a { 1.0 } else { 0.0 };
        Value::Number(a_num).partial_cmp(other)
      }
      (other, Value::Bool(b)) => {
        let b_num = if *b { 1.0 } else { 0.0 };
        other.partial_cmp(&Value::Number(b_num))
      }
      _ => None,
    }
  }
}

#[cfg(test)]
mod tests;
