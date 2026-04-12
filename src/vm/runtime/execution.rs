use crate::bytecode::{Chunk, OP_COUNT, Op};
use crate::environment::EnvironmentRecord;
use crate::errors::RuntimeError;
use crate::gc;
use crate::module::ModuleLoader;
use crate::mutex_compat::MutexBorrowExt;
use crate::realm::{Intrinsics, Realm};
use crate::runtime_utils::{array_length_descriptor, js_number_to_string};
use crate::value::{
    BuiltinFn, ObjectData, PromiseState, PropertyDescriptor, PropertyKey, SymbolId, Value,
    ValueSlot,
};
use crate::vm::runtime::coercions::{parse_js_bigint_string, parse_js_number_string, to_number};
use crate::vm::state::*;
use num_bigint::BigInt as BigIntValue;
use num_traits::{FromPrimitive, Signed, ToPrimitive, Zero};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;
use std::sync::{Arc, Mutex};

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
        self.property_get_ic_cache.clear();
    }

    // -----------------------------------------------------------------------
    // Construction & public helpers
    // -----------------------------------------------------------------------

    /// Create a new virtual machine.
    pub fn new(globals: Rc<RefCell<ObjectData>>) -> Self {
        let intrinsics = crate::runtime_utils::install_minimal_kernel_constructors(&globals);
        let realm = Realm::with_globals(globals.clone());
        let global_env = Rc::new(RefCell::new(EnvironmentRecord::new(None)));
        let module_loader = shared_module_loader_for(&globals);
        let mut vm = Self {
            realm,
            chunk: Rc::new(Chunk::new(Rc::new(String::new()))),
            ip: 0,
            stack: OperandStack::with_capacity(STACK_INITIAL_CAPACITY),
            locals: Vec::with_capacity(LOCALS_INITIAL_CAPACITY),
            this_value: Value::Object(globals.clone()),
            builtin_this_arg_prepended: false,
            current_closure_env: None,
            pending_closure_env: None,
            gc_temp_roots: Vec::new(),
            globals,
            call_stack: CallStack::with_capacity(FRAMES_INITIAL_CAPACITY),
            root_chunk: None,
            current_slot_types: None,
            current_slot_names: None,
            obj_generations: HashMap::new(),
            property_get_ic_cache: HashMap::new(),
            bg_runtime: get_global_bg_runtime(),
            handlers: HandlerStack::with_capacity(32),
            pending_exception: false,
            module_loader,
            module_table: Vec::new(),
            construct_new_target: None,
            recent_return_value: None,
            last_uncaught_exception: None,
            microtasks: VecDeque::new(),
            dynamic_import_jobs: VecDeque::new(),
            unhandled_rejection_hook: None,
            reference_stack: ReferenceStack::new(),
            lexical_env: global_env.clone(),
            global_env,
            yield_star_queue: None,
            current_arguments_object: None,
            current_arguments_mapped_count: 0,
            current_strict_mode: false,
            gc_step_counter: 0,
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
        let globals = self.globals.clone();
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
            chunk: Rc::new(Chunk::new(Rc::new(String::new()))),
            ip: 0,
            stack: OperandStack::with_capacity(STACK_INITIAL_CAPACITY),
            locals: Vec::with_capacity(LOCALS_INITIAL_CAPACITY),
            this_value,
            builtin_this_arg_prepended: false,
            current_closure_env: None,
            pending_closure_env: None,
            gc_temp_roots: Vec::new(),
            globals,
            call_stack: CallStack::with_capacity(FRAMES_INITIAL_CAPACITY),
            root_chunk: None,
            current_slot_types: None,
            current_slot_names: None,
            obj_generations: HashMap::new(),
            property_get_ic_cache: HashMap::new(),
            bg_runtime: Arc::clone(&self.bg_runtime),
            handlers: HandlerStack::with_capacity(32),
            pending_exception: false,
            module_loader: self.module_loader.clone(),
            module_table: Vec::new(),
            construct_new_target: None,
            recent_return_value: None,
            last_uncaught_exception: None,
            microtasks: VecDeque::new(),
            dynamic_import_jobs: VecDeque::new(),
            unhandled_rejection_hook: self.unhandled_rejection_hook.clone(),
            reference_stack: ReferenceStack::new(),
            lexical_env: self.lexical_env.clone(),
            global_env: self.global_env.clone(),
            yield_star_queue: None,
            current_arguments_object: None,
            current_arguments_mapped_count: 0,
            current_strict_mode: self.current_strict_mode,
            gc_step_counter: 0,
            opcode_profiling_enabled: self.opcode_profiling_enabled,
            opcode_counts: [0; OP_COUNT],
            run_started_at: None,
        }
    }

    fn begin_opcode_profile_run(&mut self) {
        if self.opcode_profiling_enabled {
            self.opcode_counts = [0; OP_COUNT];
            self.run_started_at = Some(std::time::Instant::now());
        }
    }

    fn record_opcode(&mut self, op: Op) {
        if self.opcode_profiling_enabled {
            self.opcode_counts[op as usize] = self.opcode_counts[op as usize].saturating_add(1);
        }
    }

    fn finish_opcode_profile_run(&mut self, outcome: &str) {
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
                    Op::from_byte(index as u8).map(|op| (op, count))
                }
            })
            .collect();
        ranked.sort_by(|a, b| b.1.cmp(&a.1));

        for (op, count) in ranked.into_iter().take(16) {
            let pct = (count as f64 / total as f64) * 100.0;
            eprintln!("[vm-profile] {:>16} {:>12} {:>6.2}%", op.name(), count, pct);
        }
    }

    /// Return the current `this` value.
    pub fn this_value(&self) -> Value {
        self.this_value.clone()
    }

    /// Return whether the current builtin call includes `this` at args[0].
    pub(crate) fn builtin_this_arg_prepended(&self) -> bool {
        self.builtin_this_arg_prepended
    }

    /// Return the current `[[NewTarget]]` value.
    pub fn current_new_target(&self) -> Option<Value> {
        self.call_stack
            .last()
            .and_then(|frame| frame.new_target.clone())
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
        self.current_new_target().unwrap_or(Value::Undefined)
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
    pub fn get_prototype(&self, v: &Value) -> Value {
        match v {
            Value::Object(obj)
            | Value::Array(_, obj)
            | Value::Closure { props: obj, .. }
            | Value::BuiltinObject(_, obj) => obj.borrow().get_prototype(),
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
    pub fn new_array(&self, elems: Vec<Option<Value>>) -> Value {
        let arr_rc = gc::track_array(Rc::new(RefCell::new(elems)));
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
        let mut env_roots: Vec<Rc<Mutex<Vec<ValueSlot>>>> = Vec::new();
        roots.push(Value::Object(self.globals.clone()));
        roots.extend(self.stack.iter().cloned());
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
        if let Some(value) = &self.last_uncaught_exception {
            roots.push(value.clone());
        }
        if let Some(queue) = &self.yield_star_queue {
            roots.extend(queue.iter().cloned());
        }
        if let Some(env) = &self.current_closure_env {
            env_roots.push(Rc::clone(env));
            roots.extend(env.borrow().iter().map(|cell| cell.borrow().clone()));
        }
        if let Some(env) = &self.pending_closure_env {
            env_roots.push(Rc::clone(env));
            roots.extend(env.borrow().iter().map(|cell| cell.borrow().clone()));
        }
        roots.extend(self.gc_temp_roots.iter().cloned());
        self.lexical_env.borrow().collect_binding_values(&mut roots);
        self.global_env.borrow().collect_binding_values(&mut roots);
        for frame in self.call_stack.iter() {
            roots.extend(frame.stack.iter().cloned());
            roots.extend(frame.locals.iter().map(|c| c.borrow().clone()));
            roots.push(frame.this_value.clone());
            roots.push(frame.callee.clone());
            if let Some(value) = &frame.new_target {
                roots.push(value.clone());
            }
            if let Some(value) = &frame.arguments_object {
                roots.push(value.clone());
            }
            if let Some(queue) = &frame.yield_star_queue {
                roots.extend(queue.iter().cloned());
            }
            if let Some(env) = &frame.closure_env {
                env_roots.push(Rc::clone(env));
                roots.extend(env.borrow().iter().map(|cell| cell.borrow().clone()));
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
        if let Some(value) = self.realm.intrinsics.type_error_constructor.clone() {
            roots.push(value);
        }
        if let Some(value) = self.realm.intrinsics.range_error_constructor.clone() {
            roots.push(value);
        }
        if let Some(value) = self.realm.intrinsics.reference_error_constructor.clone() {
            roots.push(value);
        }
        if let Some(value) = self.realm.intrinsics.syntax_error_constructor.clone() {
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
        if let Some(value) = self.realm.intrinsics.eval_error_constructor.clone() {
            roots.push(value);
        }
        if let Some(value) = self.realm.intrinsics.uri_error_constructor.clone() {
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
        self.gc_step_counter = 0;
        let _ = self.collect_garbage_prototype();
    }

    /// Install the minimal kernel helpers and populate intrinsics before use.
    ///
    /// This ensures every VM starts with a fully populated global object and
    /// intrinsic constructors before any bytecode executes.
    pub fn with_runtime(self) -> Self {
        self
    }

    /// Populate the Realm's intrinsics table from the globals.
    ///
    /// This extracts canonical constructor/prototype references from the
    /// Populate the Realm's intrinsics table.
    ///
    /// This records the kernel-provided intrinsics on the realm record.
    fn populate_realm_intrinsics(&mut self, intrinsics: Intrinsics) {
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

    /// Return a shared handle to the module loader for this VM's realm.
    pub fn module_loader_handle(&self) -> Rc<RefCell<ModuleLoader>> {
        self.module_loader.clone()
    }

    /// Install a module's export slot map into the VM module table at the
    /// specified numeric module index. The provided `exports` map contains
    /// shared value slots allocated by the ModuleLoader so both VM and
    /// ModuleLoader share the same live binding slots.
    pub fn install_module_exports(
        &mut self,
        module_index: usize,
        mut exports: HashMap<String, ValueSlot>,
    ) {
        let namespace = build_module_namespace_object(&exports);
        exports.insert("*".to_string(), Rc::new(Mutex::new(namespace)));
        // Ensure table is large enough
        while self.module_table.len() <= module_index {
            self.module_table.push(HashMap::new());
        }
        self.module_table[module_index] = exports;
    }

    /// Return the current namespace object for a module, if available.
    pub fn module_namespace_value(&self, module_index: usize) -> Option<Value> {
        self.module_table
            .get(module_index)
            .and_then(|exports| exports.get("*"))
            .map(|slot| slot.borrow().clone())
    }

    /// Debug helper: get a global value by name (cloned).
    pub fn get_global(&self, name: &str) -> Option<Value> {
        self.globals
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
        self.globals.clone()
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

    pub(crate) fn active_strict_mode(&self) -> bool {
        self.call_stack.last().is_some_and(|f| f.is_strict)
    }

    pub(crate) fn set_current_strict_mode(&mut self, strict: bool) {
        self.current_strict_mode = strict;
    }

    /// Return own keys for a global value if it is an object or closure.
    pub fn list_global_own_keys(&self, name: &str) -> Option<Vec<String>> {
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
    pub fn list_global_prototype_keys(&self, name: &str) -> Option<Vec<String>> {
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
    pub fn list_own_keys_of_value(&self, v: &Value) -> Option<Vec<String>> {
        list_own_keys_of_value(v)
    }

    /// Return enumerable string keys for `for-in`, walking the prototype
    /// chain and skipping duplicates.
    pub fn for_in_keys_of_value(&self, v: &Value) -> Result<Vec<String>, RuntimeError> {
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

            for key in crate::runtime_utils::own_enumerable_keys(&current) {
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
    pub fn values_same_object(&self, a: &Value, b: &Value) -> bool {
        match (a, b) {
            (Value::Object(am), Value::Object(bm)) => Rc::ptr_eq(am, bm),
            (Value::Array(ar, _), Value::Array(br, _)) => Rc::ptr_eq(ar, br),
            (Value::Closure { props: ap, .. }, Value::Closure { props: bp, .. }) => {
                Rc::ptr_eq(ap, bp)
            }
            _ => false,
        }
    }

    /// Reset mutable runtime state while preserving selected globals.
    pub fn reset(&mut self, keep_globals: &HashSet<String>) {
        self.stack.clear();
        self.locals.clear();
        self.call_stack.clear();
        self.current_strict_mode = self.chunk.root_strict;
        self.handlers.clear();
        self.root_chunk = None;
        self.current_slot_types = None;
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
        self.lexical_env = self.global_env.clone();
        self.yield_star_queue = None;
        self.ip = 0;
        self.chunk = Rc::new(Chunk::new(Rc::new(String::new())));
        self.obj_generations.clear();
        self.property_get_ic_cache.clear();
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
    pub fn run(&mut self, chunk: Chunk) -> Result<Value, RuntimeError> {
        let chunk_rc = Rc::new(chunk);
        self.root_chunk = Some(chunk_rc.clone());
        self.chunk = chunk_rc;
        self.ip = 0;
        self.stack.clear();
        self.locals.clear();
        self.call_stack.clear();
        self.current_strict_mode = self.chunk.root_strict;
        self.handlers.clear();
        self.current_slot_types = None;
        self.current_closure_env = None;
        self.pending_closure_env = None;
        self.gc_temp_roots.clear();
        self.lexical_env = self.global_env.clone();
        self.yield_star_queue = None;
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
        self.initialize_root_bindings();
        gc::reset_allocation_pressure();
        self.begin_opcode_profile_run();
        self.property_get_ic_cache.clear();

        loop {
            match self.step(None) {
                Ok(StepOutcome::Continue) => {
                    // `pending_exception` is only meant to swallow the
                    // immediate synthetic error returned by host throw helpers.
                    // If execution continues normally, clear it so unrelated
                    // future runtime errors are not masked.
                    self.pending_exception = false;
                    self.maybe_collect_garbage_prototype();
                }
                Ok(StepOutcome::Return(result)) => {
                    self.pending_exception = false;
                    self.run_microtasks_if_needed()?;

                    {
                        let _ = self.collect_garbage_prototype();
                    }
                    self.finish_opcode_profile_run("return");
                    return Ok(result);
                }
                Ok(StepOutcome::Yield(_)) => {
                    return Err(self.throw_type_error("yield used outside generator resume"));
                }
                Err(err) => {
                    if self.pending_exception {
                        self.pending_exception = false;
                        continue;
                    }

                    {
                        let _ = self.collect_garbage_prototype();
                    }
                    self.finish_opcode_profile_run("error");
                    return Err(err);
                }
            }
        }
    }

    /// Execute a chunk without resetting the caller's live lexical bindings.
    ///
    /// This is used for direct `eval`, where the evaluated code must observe
    /// and update the current execution context.
    pub(crate) fn run_nested(
        &mut self,
        chunk: Chunk,
        direct_eval: bool,
    ) -> Result<Value, RuntimeError> {
        let saved_chunk = self.chunk.clone();
        let saved_ip = self.ip;
        let saved_stack = self.stack.snapshot();
        let saved_root_chunk = self.root_chunk.clone();
        let saved_current_strict_mode = self.current_strict_mode;
        let saved_current_closure_env = self.current_closure_env.clone();
        let saved_pending_closure_env = self.pending_closure_env.clone();
        let saved_gc_temp_roots = self.gc_temp_roots.clone();
        let saved_this_value = self.this_value.clone();
        let saved_yield_star_queue = self.yield_star_queue.clone();
        let saved_current_arguments_object = self.current_arguments_object.clone();
        let saved_current_arguments_mapped_count = self.current_arguments_mapped_count;
        let saved_recent_return_value = self.recent_return_value.clone();
        let saved_last_uncaught_exception = self.last_uncaught_exception.clone();
        let saved_pending_exception = self.pending_exception;
        let saved_lexical_env = if direct_eval {
            Some(self.lexical_env.clone())
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

        let chunk_rc = Rc::new(chunk);
        self.root_chunk = Some(chunk_rc.clone());
        self.chunk = chunk_rc;
        self.ip = 0;
        self.stack.clear();
        self.current_strict_mode = self.chunk.root_strict;
        self.gc_temp_roots.clear();
        self.yield_star_queue = None;
        if self.chunk.root_slot_names.is_empty() {
            self.locals.clear();
            self.current_slot_types = None;
            self.current_slot_names = None;
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

        let result = loop {
            match self.step(None) {
                Ok(StepOutcome::Continue) => {
                    self.pending_exception = false;
                    self.maybe_collect_garbage_prototype();
                }
                Ok(StepOutcome::Return(result)) => {
                    self.pending_exception = false;
                    self.run_microtasks_if_needed()?;

                    {
                        let _ = self.collect_garbage_prototype();
                    }
                    self.finish_opcode_profile_run("return");
                    break Ok(result);
                }
                Ok(StepOutcome::Yield(_)) => {
                    break Err(self.throw_type_error("yield used outside generator resume"));
                }
                Err(err) => {
                    if self.pending_exception {
                        self.pending_exception = false;
                        continue;
                    }

                    {
                        let _ = self.collect_garbage_prototype();
                    }
                    self.finish_opcode_profile_run("error");
                    break Err(err);
                }
            }
        };

        self.chunk = saved_chunk;
        self.ip = saved_ip;
        self.stack.restore_from(saved_stack);
        self.root_chunk = saved_root_chunk;
        self.current_strict_mode = saved_current_strict_mode;
        self.current_closure_env = saved_current_closure_env;
        self.pending_closure_env = saved_pending_closure_env;
        self.gc_temp_roots = saved_gc_temp_roots;
        self.this_value = saved_this_value;
        self.yield_star_queue = saved_yield_star_queue;
        self.current_arguments_object = saved_current_arguments_object;
        self.current_arguments_mapped_count = saved_current_arguments_mapped_count;
        self.recent_return_value = saved_recent_return_value;
        self.last_uncaught_exception = saved_last_uncaught_exception;
        self.pending_exception = saved_pending_exception;
        if direct_eval {
            if let Some(lexical_env) = saved_lexical_env {
                self.lexical_env = lexical_env;
            }
            if let Some(locals) = saved_locals {
                self.locals = locals;
            }
            self.current_slot_types = saved_current_slot_types;
            self.current_slot_names = saved_current_slot_names;
        }

        result
    }

    fn initialize_root_bindings(&mut self) {
        let slot_names = self.chunk.root_slot_names.clone();
        let slot_tdz = self.chunk.root_lexical_tdz.clone();
        let slot_types = self.chunk.root_slot_types.clone();
        let slot_count = slot_names.len().max(slot_tdz.len()).max(slot_types.len());
        if slot_count == 0 {
            return;
        }

        self.locals = std::iter::repeat_with(|| Rc::new(Mutex::new(Value::Undefined)))
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
                root_env.borrow_mut().create_mutable_binding(
                    name.clone(),
                    self.locals[idx].clone(),
                    initialized,
                );
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
    }

    // -----------------------------------------------------------------------
    // Opcode helpers (called from the main loop)
    // -----------------------------------------------------------------------

    pub(super) fn make_iterator_result(value: Value, done: bool) -> Value {
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

    // -----------------------------------------------------------------------
    // Iterator / iterator protocol notes
    // -----------------------------------------------------------------------
    //
    // The VM exposes `make_iterator_result` as a helper to build objects of
    // the form { value, done }. However, several iterator factories in the
    // runtime are lightweight and currently produce simplified iterator
    // objects. A full ECMAScript-compliant iterator should be an object that
    // implements the `next` method and preserves iteration state across
    // successive calls. Where the runtime uses simplified iterators (for
    // example, array iterator factories), those implementations should be
    // replaced with proper iterator objects that maintain internal iteration
    // state and follow the spec's IteratorClose semantics for proper
    // conformance.

    /// Apply ECMAScript `ToObject` coercion.
    ///
    /// # Errors
    ///
    /// Returns a [`crate::errors::RuntimeError`] if the coercion fails.
    pub fn to_object_value(&mut self, v: Value) -> Result<Value, RuntimeError> {
        match v {
            Value::Object(_)
            | Value::Array(..)
            | Value::Closure { .. }
            | Value::BuiltinObject(_, _)
            | Value::Promise(_) => Ok(v),
            Value::Undefined | Value::Null => {
                Err(self.throw_type_error("Cannot convert undefined or null to object"))
            }
            Value::Bool(b) => {
                let ctor = self.get_global("Boolean").unwrap_or(Value::Undefined);
                self.construct_value(&ctor, &[Value::Bool(b)])
            }
            Value::Number(n) => {
                let ctor = self.get_global("Number").unwrap_or(Value::Undefined);
                self.construct_value(&ctor, &[Value::Number(n)])
            }
            Value::String(s) => {
                let ctor = self.get_global("String").unwrap_or(Value::Undefined);
                self.construct_value(&ctor, &[Value::String(s)])
            }
            Value::BigInt(n) => {
                let ctor = self.get_global("BigInt").unwrap_or(Value::Undefined);
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
                // Builtins are callable but might not be full objects.
                // However, in this VM they are often treated as such or wrapped.
                Ok(v)
            }
        }
    }

    /// Parse and evaluate a single expression source string in an isolated VM.
    ///
    /// The expression result is written to `__tsblitz_eval_result` and returned.
    ///
    /// # Errors
    ///
    /// Returns a [`crate::errors::RuntimeError`] if the evaluation fails.
    pub fn eval_expression_source(&mut self, source: &str) -> Result<Value, RuntimeError> {
        let strict = self.active_strict_mode();
        let wrapped = if strict {
            format!("'use strict'; (function() {{ return ({}); }})()", source)
        } else {
            format!("(function() {{ return ({}); }})()", source)
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
    pub fn eval_expression_source_direct(&mut self, source: &str) -> Result<Value, RuntimeError> {
        let strict = self.active_strict_mode();
        let wrapped = if strict {
            format!("'use strict'; (function() {{ return ({}); }})()", source)
        } else {
            format!("(function() {{ return ({}); }})()", source)
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

    pub(super) fn run_until_return(
        &mut self,
        stack_base: usize,
        drain_microtasks: bool,
    ) -> Result<Value, RuntimeError> {
        let target_frame_depth = self.call_stack.len();
        loop {
            match self.step(Some(target_frame_depth)) {
                Ok(StepOutcome::Continue) => {
                    if self.pending_exception {
                        if self.call_stack.len() < target_frame_depth {
                            return Err(RuntimeError::from("JS exception"));
                        }
                        self.pending_exception = false;
                    }
                    self.maybe_collect_garbage_prototype();
                }
                Ok(StepOutcome::Return(result)) => {
                    self.pending_exception = false;
                    self.stack.truncate(stack_base);
                    if drain_microtasks {
                        self.run_microtasks_if_needed()?;
                    }
                    return Ok(result);
                }
                Ok(StepOutcome::Yield(_)) => {
                    return Err(self.throw_type_error("yield used outside generator resume"));
                }
                Err(err) => {
                    if self.pending_exception {
                        if self.call_stack.len() < target_frame_depth {
                            return Err(err);
                        }
                        self.pending_exception = false;
                        continue;
                    }
                    return Err(err);
                }
            }
        }
    }

    /// Executes a single instruction, returning the outcome (return value or yield).
    ///
    /// # Errors
    ///
    /// Returns a [`crate::errors::RuntimeError`] if the instruction fails.
    pub(super) fn step(
        &mut self,
        stop_at_frame_depth: Option<usize>,
    ) -> Result<StepOutcome, RuntimeError> {
        let op_byte = self.read_byte()?;
        if crate::trace::enabled() {
            eprintln!(
                "[trace] step ip={} stack_size={} op_byte=0x{:02x}",
                self.ip.saturating_sub(1),
                self.stack.len(),
                op_byte
            );
        }
        let op = match Op::from_byte(op_byte) {
            Some(o) => o,
            None => {
                return Err(RuntimeError::from(format!(
                    "Invalid opcode 0x{:02x} at ip {}",
                    op_byte,
                    self.ip.saturating_sub(1)
                )));
            }
        };
        self.record_opcode(op);

        match op {
            Op::Const => {
                let idx = self.read_short()? as usize;
                let v = self.constant(idx)?;
                self.stack.push(v);
            }
            Op::Null => self.stack.push(Value::Null),
            Op::Undefined => self.stack.push(Value::Undefined),
            Op::True => self.stack.push(Value::Bool(true)),
            Op::False => self.stack.push(Value::Bool(false)),
            Op::GetLocal => {
                let slot = self.read_byte()? as usize;
                if let Some(name) = self.chunk.root_slot_names.get(slot).and_then(Clone::clone)
                    && let Some(v) = self.get_with_env_binding_value(&name)?
                {
                    if crate::trace::enabled() && name.starts_with("st_") {
                        eprintln!(
                            "[trace] GetLocal '{}' via with-env -> {}",
                            name,
                            v.type_name()
                        );
                        crate::trace::value_string("[trace]   value", &v);
                    }
                    self.stack.push(v);
                    return Ok(StepOutcome::Continue);
                }
                let v = self
                    .locals
                    .get(slot)
                    .map(|cell| cell.borrow().clone())
                    .ok_or("Bad local slot")?;
                if crate::trace::enabled()
                    && matches!(v, Value::Uninitialized)
                    && let Some(name) = self.chunk.root_slot_names.get(slot).and_then(Clone::clone)
                {
                    eprintln!("[trace] GetLocal TDZ name='{}' slot={}", name, slot);
                }
                if matches!(v, Value::Uninitialized) {
                    return Err(self.throw_reference_error(
                        "Cannot access lexical binding before initialization",
                    ));
                }
                if crate::trace::enabled()
                    && self
                        .chunk
                        .root_slot_names
                        .get(slot)
                        .and_then(Clone::clone)
                        .as_deref()
                        .is_some_and(|name| name.starts_with("st_"))
                {
                    let name = self
                        .chunk
                        .root_slot_names
                        .get(slot)
                        .and_then(Clone::clone)
                        .unwrap();
                    eprintln!("[trace] GetLocal '{}' -> {}", name, v.type_name());
                    crate::trace::value_string("[trace]   value", &v);
                }
                self.stack.push(v);
            }
            Op::SetLocal => {
                let slot = self.read_byte()? as usize;
                let v = self.pop()?;
                if let Some(name) = self.chunk.root_slot_names.get(slot).and_then(Clone::clone)
                    && self.set_with_env_binding_value(&name, v.clone())?
                {
                    return Ok(StepOutcome::Continue);
                }
                ensure_slot(&mut self.locals, slot);
                *self.locals[slot].borrow_mut() = v;
                if let Some(name) = self.chunk.root_slot_names.get(slot).and_then(Clone::clone) {
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
                        let value = self.locals[slot].borrow().clone();
                        set_property(&arguments_object, &key, value)?;
                        self.bump_object_generation(&arguments_object);
                    }
                }
            }
            Op::CloneLocalCell => {
                let slot = self.read_byte()? as usize;
                ensure_slot(&mut self.locals, slot);
                let value = self.locals[slot].borrow().clone();
                self.locals[slot] = Rc::new(Mutex::new(value));
            }
            Op::GetGlobal => {
                let idx = self.read_short()? as usize;
                let name = self.string_constant(idx)?;
                let v = if let Some(v) = self.resolve_identifier_value(&name)? {
                    v
                } else {
                    return Err(self.throw_reference_error(&format!("{name} is not defined")));
                };
                self.stack.push(v);
            }
            Op::SetGlobal => {
                let idx = self.read_short()? as usize;
                let name = self.string_constant(idx)?;
                let v = self.pop()?;
                if !self.set_lexical_identifier_value(&name, v.clone())? {
                    if self.set_current_frame_local_by_name(&name, v.clone())? {
                        return Ok(StepOutcome::Continue);
                    }
                    let key = PropertyKey::String(name.clone());
                    let current = self.globals.borrow().properties.get(&key).cloned();
                    match current {
                        Some(PropertyDescriptor::Data {
                            writable: false, ..
                        }) => {
                            if self.is_strict_mode() {
                                return Err(self.throw_type_error(&format!(
                                    "Cannot assign to read only global '{}'",
                                    name
                                )));
                            }
                        }
                        Some(PropertyDescriptor::Accessor { .. }) => {
                            let global_obj = Value::Object(self.globals.clone());
                            self.set_property_tracked(&global_obj, &key, v)?;
                        }
                        None if self.is_strict_mode() => {
                            return Err(
                                self.throw_reference_error(&format!("{name} is not defined"))
                            );
                        }
                        _ => {
                            self.globals
                                .borrow_mut()
                                .properties
                                .insert(key, PropertyDescriptor::data(v));
                        }
                    }
                }
            }
            // New op: GetThis
            Op::GetThis => {
                self.stack.push(self.get_this());
            }
            Op::GetUpvalue => {
                let idx = self.read_byte()? as usize;
                let value = {
                    let env = self
                        .current_closure_env
                        .as_ref()
                        .ok_or("GetUpvalue without closure env")?;
                    let env_guard = env.borrow();
                    env_guard.get(idx).map(|cell| cell.borrow().clone()).ok_or_else(|| {
                        if crate::trace::enabled() {
                            eprintln!(
                                "[diag] GetUpvalue bad index={} env_len={} ip={} chunk_ptr={:p} chunk_len={}",
                                idx,
                                env_guard.len(),
                                self.ip.saturating_sub(1),
                                Rc::as_ptr(&self.chunk),
                                self.chunk.code.len()
                            );
                        }
                        "Bad upvalue index"
                    })?
                };
                if matches!(value, Value::Uninitialized) {
                    return Err(self.throw_reference_error(
                        "Cannot access lexical binding before initialization",
                    ));
                }
                self.stack.push(value);
            }
            Op::SetUpvalue => {
                let idx = self.read_byte()? as usize;
                let value = self.pop()?;
                let mirror_global_name = self
                    .call_stack
                    .last()
                    .and_then(|frame| frame.upvalues.get(idx))
                    .and_then(|spec| spec.mirror_global_name.clone());
                let env = self
                    .current_closure_env
                    .as_ref()
                    .ok_or("SetUpvalue without closure env")?;
                let cell = {
                    let env_guard = env.borrow();
                    env_guard.get(idx).cloned().ok_or_else(|| {
                        if crate::trace::enabled() {
                            eprintln!(
                                "[diag] SetUpvalue bad index={} env_len={} ip={} chunk_ptr={:p} chunk_len={}",
                                idx,
                                env_guard.len(),
                                self.ip.saturating_sub(1),
                                Rc::as_ptr(&self.chunk),
                                self.chunk.code.len()
                            );
                        }
                        "Bad upvalue index"
                    })?
                };
                *cell.borrow_mut() = value;

                if let Some(name) = mirror_global_name {
                    let current = cell.borrow().clone();
                    self.sync_named_binding_to_global_and_frames(&name, &current);
                }
            }
            Op::Pop => {
                self.pop()?;
            }
            Op::Dup => {
                let v = self.stack.last().cloned().ok_or("Stack underflow")?;
                if crate::trace::enabled() {
                    crate::trace::value_string("[trace] Dup top value", &v);
                    eprintln!("[trace] Dup before push stack_size={} ", self.stack.len());
                }
                self.stack.push(v);
                if crate::trace::enabled() {
                    eprintln!("[trace] Dup after push stack_size={} ", self.stack.len());
                }
            }
            Op::Add => {
                let b = self.pop()?;
                let a = self.pop()?;
                // ECMAScript Add: ToPrimitive first; string concatenation takes
                // precedence, otherwise numeric/bigint addition.
                let a_prim = self.to_primitive(&a, None)?;
                let b_prim = self.to_primitive(&b, None)?;
                if matches!(a_prim, Value::String(_)) || matches!(b_prim, Value::String(_)) {
                    let sa = self.to_string(&a_prim)?;
                    let sb = self.to_string(&b_prim)?;
                    self.stack.push(Value::String(format!("{}{}", sa, sb)));
                } else if matches!(a_prim, Value::BigInt(_)) || matches!(b_prim, Value::BigInt(_)) {
                    if let (Value::BigInt(x), Value::BigInt(y)) = (a_prim, b_prim) {
                        self.stack.push(Value::BigInt(x + y));
                    } else {
                        // Mixing BigInt and other numeric types should throw a JS TypeError
                        let ctor = self.get_global("TypeError").unwrap_or(Value::Undefined);
                        let thrown = self.construct_value(&ctor, &[]).unwrap_or(Value::Object(
                            gc::track_object(Rc::new(RefCell::new(ObjectData::new()))),
                        ));
                        match self.unwind_to_handler(thrown) {
                            Ok(()) => return Ok(StepOutcome::Continue),
                            Err(err) => return Err(err),
                        }
                    }
                } else {
                    let na = self.to_number_value(&a_prim)?;
                    let nb = self.to_number_value(&b_prim)?;
                    self.stack.push(Value::Number(na + nb));
                }
            }
            Op::AddNumber => {
                let b = self.pop()?;
                let a = self.pop()?;
                match (a, b) {
                    (Value::BigInt(x), Value::BigInt(y)) => self.stack.push(Value::BigInt(x + y)),
                    (Value::Number(x), Value::Number(y)) => self.stack.push(Value::Number(x + y)),
                    _ => return Err("AddNumber: number expected".into()),
                }
            }
            Op::Sub => self.binary_num_bigint(|a, b| a - b, |a, b| Ok(a - b))?,
            Op::Mul => self.binary_num_bigint(|a, b| a * b, |a, b| Ok(a * b))?,
            Op::Div => {
                // Division supports BigInt/BigInt -> BigInt (trunc toward zero)
                // and numeric division for other types.
                let b = self.pop()?;
                let a = self.pop()?;
                match (a, b) {
                    (Value::BigInt(x), Value::BigInt(y)) => {
                        if y.is_zero() {
                            // Division by zero for BigInt throws RangeError
                            let ctor = self.get_global("RangeError").unwrap_or(Value::Undefined);
                            let thrown = self.construct_value(&ctor, &[]).unwrap_or(Value::Object(
                                gc::track_object(Rc::new(RefCell::new(ObjectData::new()))),
                            ));
                            match self.unwind_to_handler(thrown) {
                                Ok(()) => return Ok(StepOutcome::Continue),
                                Err(err) => return Err(err),
                            }
                        }
                        // Use BigInt division (num_bigint division semantics align with trunc toward zero)
                        let res = x / y;
                        self.stack.push(Value::BigInt(res));
                    }
                    (a, b) => {
                        let (x, y) = (to_number(&a)?, to_number(&b)?);
                        self.stack.push(Value::Number(x / y));
                    }
                }
            }
            Op::Mod => {
                let b = self.pop()?;
                let a = self.pop()?;
                match (a, b) {
                    (Value::BigInt(x), Value::BigInt(y)) => {
                        if y.is_zero() {
                            let ctor = self.get_global("RangeError").unwrap_or(Value::Undefined);
                            let thrown = self.construct_value(&ctor, &[]).unwrap_or(Value::Object(
                                gc::track_object(Rc::new(RefCell::new(ObjectData::new()))),
                            ));
                            match self.unwind_to_handler(thrown) {
                                Ok(()) => return Ok(StepOutcome::Continue),
                                Err(err) => return Err(err),
                            }
                        }
                        self.stack.push(Value::BigInt(x % y));
                    }
                    (a, b) => {
                        let (x, y) = (to_number(&a)?, to_number(&b)?);
                        self.stack.push(Value::Number(x % y));
                    }
                }
            }
            Op::Pow => {
                // Exponentiation: handle BigInt**BigInt and Number**Number semantics.
                // Pop right then left.
                let b = self.pop()?;
                let a = self.pop()?;
                // Helper: ToNumeric approximation returning either Number(f64) or BigInt(i128)
                enum Numeric {
                    Number(f64),
                    BigInt(BigIntValue),
                }

                let to_numeric = |vm: &mut Vm, v: Value| -> Result<Numeric, RuntimeError> {
                    match v {
                        Value::BigInt(n) => Ok(Numeric::BigInt(n)),
                        Value::Number(n) => Ok(Numeric::Number(n)),
                        other => {
                            // ToPrimitive with number hint then interpret
                            let prim = vm.to_primitive(&other, Some("number"))?;
                            match prim {
                                Value::BigInt(n) => Ok(Numeric::BigInt(n)),
                                Value::Number(n) => Ok(Numeric::Number(n)),
                                Value::String(s) => Ok(Numeric::Number(parse_js_number_string(&s))),
                                Value::Bool(b) => Ok(Numeric::Number(if b { 1.0 } else { 0.0 })),
                                Value::Null => Ok(Numeric::Number(0.0)),
                                Value::Undefined => Ok(Numeric::Number(f64::NAN)),
                                _ => {
                                    // Default: try ToNumber
                                    let n = vm.to_number_value(&prim)?;
                                    Ok(Numeric::Number(n))
                                }
                            }
                        }
                    }
                };

                let a_num = match to_numeric(self, a) {
                    Ok(n) => n,
                    Err(_e) => {
                        // Convert runtime error into a JS TypeError where appropriate
                        let ctor = self.get_global("TypeError").unwrap_or(Value::Undefined);
                        let thrown = self.construct_value(&ctor, &[]).unwrap_or(Value::Object(
                            gc::track_object(Rc::new(RefCell::new(ObjectData::new()))),
                        ));
                        match self.unwind_to_handler(thrown) {
                            Ok(()) => return Ok(StepOutcome::Continue),
                            Err(err) => return Err(err),
                        }
                    }
                };
                let b_num = match to_numeric(self, b) {
                    Ok(n) => n,
                    Err(_e) => {
                        let ctor = self.get_global("TypeError").unwrap_or(Value::Undefined);
                        let thrown = self.construct_value(&ctor, &[]).unwrap_or(Value::Object(
                            gc::track_object(Rc::new(RefCell::new(ObjectData::new()))),
                        ));
                        match self.unwind_to_handler(thrown) {
                            Ok(()) => return Ok(StepOutcome::Continue),
                            Err(err) => return Err(err),
                        }
                    }
                };

                match (a_num, b_num) {
                    (Numeric::BigInt(x), Numeric::BigInt(y)) => {
                        // BigInt exponent must be non-negative
                        let exp = y;
                        if exp.is_negative() {
                            let ctor = self.get_global("RangeError").unwrap_or(Value::Undefined);
                            let thrown = self.construct_value(&ctor, &[]).unwrap_or(Value::Object(
                                gc::track_object(Rc::new(RefCell::new(ObjectData::new()))),
                            ));
                            match self.unwind_to_handler(thrown) {
                                Ok(()) => return Ok(StepOutcome::Continue),
                                Err(err) => return Err(err),
                            }
                        }
                        let exp_u32 = exp
                            .to_u32()
                            .ok_or_else(|| RuntimeError::from("Pow: bigint exponent too large"))?;
                        self.stack.push(Value::BigInt(x.pow(exp_u32)));
                    }
                    (Numeric::Number(x), Numeric::Number(y)) => {
                        let r = x.powf(y);
                        self.stack.push(Value::Number(r));
                    }
                    // Mixing BigInt and Number -> throw TypeError as JS semantics
                    _ => {
                        let ctor = self.get_global("TypeError").unwrap_or(Value::Undefined);
                        let thrown = self.construct_value(&ctor, &[]).unwrap_or(Value::Object(
                            gc::track_object(Rc::new(RefCell::new(ObjectData::new()))),
                        ));
                        match self.unwind_to_handler(thrown) {
                            Ok(()) => return Ok(StepOutcome::Continue),
                            Err(err) => return Err(err),
                        }
                    }
                }
            }
            Op::Eq => {
                let b = self.pop()?;
                let a = self.pop()?;
                let result = self.abstract_equality(&a, &b)?;
                self.stack.push(Value::Bool(result));
            }
            Op::Neq => {
                let b = self.pop()?;
                let a = self.pop()?;
                let result = self.abstract_equality(&a, &b)?;
                self.stack.push(Value::Bool(!result));
            }
            Op::Lt => {
                let b = self.pop()?;
                let a = self.pop()?;
                self.stack.push(Value::Bool(a < b));
            }
            Op::Le => {
                let b = self.pop()?;
                let a = self.pop()?;
                self.stack.push(Value::Bool(a <= b));
            }
            Op::Gt => {
                let b = self.pop()?;
                let a = self.pop()?;
                self.stack.push(Value::Bool(a > b));
            }
            Op::Ge => {
                let b = self.pop()?;
                let a = self.pop()?;
                self.stack.push(Value::Bool(a >= b));
            }
            Op::And => {
                let b = self.pop()?;
                let a = self.pop()?;
                self.stack.push(Value::Bool(a.is_truthy() && b.is_truthy()));
            }
            Op::Or => {
                let b = self.pop()?;
                let a = self.pop()?;
                self.stack.push(Value::Bool(a.is_truthy() || b.is_truthy()));
            }
            Op::Neg => {
                let v = self.pop()?;
                // Perform ToNumber semantics for unary negation
                let n = self.to_number_value(&v)?;
                self.stack.push(Value::Number(-n));
            }
            Op::Not => {
                let v = self.pop()?;
                self.stack.push(Value::Bool(!v.is_truthy()));
            }
            Op::BitAnd => {
                // Perform ToInt32 on operands and bitwise-and like JS
                let b = self.pop()?;
                let a = self.pop()?;
                let ai = self.to_int32_value(&a)?;
                let bi = self.to_int32_value(&b)?;
                let r = ai & bi;
                self.stack.push(Value::Number(r as f64));
            }
            Op::BitOr => {
                let b = self.pop()?;
                let a = self.pop()?;
                let ai = self.to_int32_value(&a)?;
                let bi = self.to_int32_value(&b)?;
                let r = ai | bi;
                self.stack.push(Value::Number(r as f64));
            }
            Op::BitXor => {
                let b = self.pop()?;
                let a = self.pop()?;
                let ai = self.to_int32_value(&a)?;
                let bi = self.to_int32_value(&b)?;
                let r = ai ^ bi;
                self.stack.push(Value::Number(r as f64));
            }
            Op::Shl => {
                let b = self.pop()?;
                let a = self.pop()?;
                let ai = self.to_int32_value(&a)?;
                let shift = self.to_uint32_value(&b)? & 0x1f;
                let r = ai.wrapping_shl(shift);
                self.stack.push(Value::Number(r as f64));
            }
            Op::Shr => {
                let b = self.pop()?;
                let a = self.pop()?;
                let ai = self.to_int32_value(&a)?;
                let shift = self.to_uint32_value(&b)? & 0x1f;
                let r = ai.wrapping_shr(shift);
                self.stack.push(Value::Number(r as f64));
            }
            Op::Ushr => {
                let b = self.pop()?;
                let a = self.pop()?;
                let au = self.to_uint32_value(&a)?;
                let shift = self.to_uint32_value(&b)? & 0x1f;
                let r = au.wrapping_shr(shift);
                // JavaScript >>> yields an unsigned 32-bit number, represent as Number
                self.stack.push(Value::Number(r as f64));
            }
            Op::Typeof => {
                let v = self.pop()?;
                self.stack.push(Value::String(v.type_name().to_string()));
            }
            Op::TypeofIdentifier => {
                let name_idx = self.read_short()? as usize;
                let name = self.string_constant(name_idx)?;
                let value = if let Some(binding) = self.resolve_identifier_value(&name)? {
                    binding
                } else {
                    Value::Undefined
                };
                self.stack
                    .push(Value::String(value.type_name().to_string()));
            }
            Op::Call => self.execute_call()?,
            Op::CallIC => self.execute_call_ic()?,
            Op::TailCallIC => self.execute_tail_call()?,
            Op::PushWithEnv => {
                let obj = self.pop()?;
                let obj = self.to_object_value(obj)?;
                let env = Rc::new(RefCell::new(EnvironmentRecord::new_object(
                    Some(self.lexical_env.clone()),
                    obj,
                    true,
                )));
                self.lexical_env = env;
            }
            Op::PopEnv => {
                let outer = self.lexical_env.borrow().outer();
                self.lexical_env = outer.unwrap_or_else(|| self.global_env.clone());
            }
            Op::GetNewTarget => {
                let new_target = self.get_new_target();
                self.stack.push(new_target);
            }
            Op::GetSuperProperty => {
                let key = self.pop()?;
                let receiver = self.pop()?;
                self.get_super_property(&receiver, &key)?;
            }
            Op::SetSuperProperty => {
                let value = self.pop()?;
                let key = self.pop()?;
                let receiver = self.pop()?;
                self.set_super_property(&receiver, &key, value)?;
            }
            Op::CallMethod => self.execute_call_method()?,
            Op::TailCallMethod => self.execute_tail_call_method()?,
            Op::Spread => self.execute_spread()?,
            Op::ArrayFromSpread => self.execute_array_from_spread()?,
            Op::Construct => self.execute_construct()?,
            Op::Return => {
                if self.call_stack.is_empty() {
                    let result = self.stack.pop().unwrap_or(Value::Undefined);
                    return Ok(StepOutcome::Return(result));
                }
                self.execute_return()?;
                // Only stop if we've returned PAST the target frame depth
                // (i.e., we've returned from the function we were waiting for)
                if let Some(target_depth) = stop_at_frame_depth
                    && self.call_stack.len() < target_depth
                {
                    let result = self.pop().unwrap_or(Value::Undefined);
                    return Ok(StepOutcome::Return(result));
                }
            }
            Op::Jump => {
                let offset = self.read_short_signed()?;
                self.jump(offset)?;
            }
            Op::JumpIfFalse => {
                let offset = self.read_short_signed()?;
                let v = self.pop()?;
                if !v.is_truthy() {
                    self.jump(offset)?;
                }
            }
            Op::CompareJumpIfFalse => {
                let compare_op = self.read_byte()?;
                let offset = self.read_short_signed()?;
                let b = self.pop()?;
                let a = self.pop()?;
                let result = match Op::from_byte(compare_op) {
                    Some(Op::Eq) => self.abstract_equality(&a, &b)?,
                    Some(Op::Neq) => !self.abstract_equality(&a, &b)?,
                    Some(Op::StrictEq) => values_equal(&a, &b),
                    Some(Op::StrictNeq) => !values_equal(&a, &b),
                    Some(Op::Lt) => a < b,
                    Some(Op::Le) => a <= b,
                    Some(Op::Gt) => a > b,
                    Some(Op::Ge) => a >= b,
                    _ => return Err(RuntimeError::from("Invalid compare-jump opcode")),
                };
                if !result {
                    self.jump(offset)?;
                }
            }
            Op::JumpIfTrue => {
                let offset = self.read_short_signed()?;
                let v = self.pop()?;
                if v.is_truthy() {
                    self.jump(offset)?;
                }
            }
            Op::JumpIfNullish => {
                let offset = self.read_short_signed()?;
                let v = self.pop()?;
                if matches!(v, Value::Null | Value::Undefined) {
                    self.jump(offset)?;
                }
            }
            Op::BuildArray => self.build_array()?,
            Op::BuildObject => self.build_object()?,
            Op::GetProperty => {
                let key = self.pop()?;
                let obj = self.pop()?;
                let v = if let Some(i) = array_index_from_value(&key) {
                    if let Some(v) = array_element_value(&obj, i) {
                        v
                    } else {
                        let k = self.to_property_key(&key)?;
                        self.get_property_value(&obj, &k)?
                    }
                } else {
                    let k = self.to_property_key(&key)?;
                    self.get_property_value(&obj, &k)?
                };
                if crate::trace::enabled() {
                    match self.to_property_key(&key) {
                        Ok(PropertyKey::String(s)) => {
                            eprintln!(
                                "[trace] GetProperty receiver={} key='{}' -> {}",
                                obj.type_name(),
                                s,
                                v.type_name()
                            );
                        }
                        Ok(PropertyKey::Symbol(_)) => {
                            eprintln!(
                                "[trace] GetProperty receiver={} key=symbol -> {}",
                                obj.type_name(),
                                v.type_name()
                            );
                        }
                        Err(_) => {
                            eprintln!(
                                "[trace] GetProperty receiver={} key=<error> -> {}",
                                obj.type_name(),
                                v.type_name()
                            );
                        }
                    }
                    crate::trace::value_string("[trace]   result", &v);
                }
                self.stack.push(v);
            }
            Op::GetPropertyIC => self.get_property_ic()?,
            Op::GetGlobalIC => self.get_global_ic()?,
            Op::DeleteProperty => self.delete_property_op()?,
            Op::DeleteIdentifier => self.delete_identifier_op()?,
            Op::HasProperty => self.has_property_op()?,
            Op::InstanceOf => {
                let constructor = self.pop()?;
                let value = self.pop()?;
                let result = self.instance_of(&value, &constructor)?;
                self.stack.push(Value::Bool(result));
            }
            Op::SetProperty => {
                if crate::trace::enabled() {
                    eprintln!(
                        "[trace] SetProperty stack_size={} ip={}",
                        self.stack.len(),
                        self.ip.saturating_sub(1)
                    );
                    for (i, v) in self.stack.iter_rev().take(6).enumerate() {
                        crate::trace::value_string(&format!("[trace]   stack[{}]", i), v);
                    }
                }
                let value = self.pop()?;
                let key = self.pop()?;
                let obj = self.pop()?;
                if let Some(i) = array_index_from_value(&key)
                    && matches!(obj, Value::Array(..))
                {
                    let strict = self.is_strict_mode();
                    self.reference_stack.push(ReferenceRecord::new(
                        obj.clone(),
                        PropertyKey::String(i.to_string()),
                        strict,
                    ));
                    let result = set_array_index_property(&obj, i, value.clone());
                    if crate::trace::enabled()
                        && let Some(reference) = self.reference_stack.last()
                    {
                        crate::trace::value_string("[trace]   reference base", &reference.base);
                        match &reference.referenced_name {
                            PropertyKey::String(name) => {
                                eprintln!("[trace]   reference key='{}'", name);
                            }
                            PropertyKey::Symbol(_) => {
                                eprintln!("[trace]   reference key=symbol");
                            }
                        }
                        eprintln!("[trace]   reference strict={}", reference.strict);
                    }
                    let _ = self.reference_stack.pop();
                    result?;
                    self.bump_object_generation(&obj);
                    self.stack.push(value);
                    return Ok(StepOutcome::Continue);
                }
                let k = self.to_property_key(&key)?;
                let strict = self.is_strict_mode();
                self.reference_stack
                    .push(ReferenceRecord::new(obj.clone(), k.clone(), strict));
                let result = self.set_property_tracked(&obj, &k, value.clone());
                if crate::trace::enabled()
                    && let Some(reference) = self.reference_stack.last()
                {
                    crate::trace::value_string("[trace]   reference base", &reference.base);
                    match &reference.referenced_name {
                        PropertyKey::String(name) => {
                            eprintln!("[trace]   reference key='{}'", name);
                        }
                        PropertyKey::Symbol(_) => {
                            eprintln!("[trace]   reference key=symbol");
                        }
                    }
                    eprintln!("[trace]   reference strict={}", reference.strict);
                }
                let _ = self.reference_stack.pop();
                result?;
                self.stack.push(value);
            }
            Op::SetPropertyIC => self.set_property_ic()?,
            Op::IteratorNext => self.iterator_next_op()?,
            Op::IteratorClose => self.iterator_close_op()?,
            Op::Swap => {
                let len = self.stack.len();
                if len < 2 {
                    return Err(RuntimeError::from("Stack underflow"));
                }
                self.stack.swap(len - 1, len - 2);
            }
            Op::PopN => {
                let count = self.read_byte()? as usize;
                if count > self.stack.len() {
                    return Err(RuntimeError::from("Stack underflow"));
                }
                self.stack.truncate(self.stack.len() - count);
            }
            Op::GetModuleExport => {
                let module_idx = self.read_byte()? as usize;
                let name_idx = self.read_byte()? as usize;
                let name = self.string_constant(name_idx)?;
                let v = self
                    .module_table
                    .get(module_idx)
                    .and_then(|m| m.get(&name).cloned())
                    .map_or(Value::Undefined, |slot| slot.borrow().clone());
                self.stack.push(v);
            }
            Op::SetModuleExport => {
                let module_idx = self.read_byte()? as usize;
                let name_idx = self.read_byte()? as usize;
                let name = self.string_constant(name_idx)?;
                let v = self.pop()?;
                if let Some(m) = self.module_table.get_mut(module_idx) {
                    if let Some(slot) = m.get(&name) {
                        *slot.borrow_mut() = v;
                    } else {
                        let new_slot = Rc::new(Mutex::new(v));
                        m.insert(name, new_slot);
                    }
                } else {
                    while self.module_table.len() <= module_idx {
                        self.module_table.push(HashMap::new());
                    }
                    let new_slot = Rc::new(Mutex::new(v));
                    self.module_table[module_idx].insert(name, new_slot);
                }
            }
            Op::MakeClosure => self.make_closure()?,
            Op::GetBuiltin => {
                let idx = self.read_byte()? as usize;
                let name = self.string_constant(idx)?;
                let v = self.realm.get_hidden_builtin(&name).ok_or_else(|| {
                    RuntimeError::from(format!("GetBuiltin: '{}' not found", name))
                })?;
                match v {
                    Value::Builtin(_) | Value::BuiltinObject(_, _) => self.stack.push(v),
                    _ => {
                        return Err(RuntimeError::from(format!(
                            "GetBuiltin: '{}' is not a builtin",
                            name
                        )));
                    }
                }
            }
            Op::PushHandler => {
                let offset = self.read_short_signed()?;
                let handler_ip = (self.ip as isize + offset) as usize;
                self.handlers.push(HandlerRecord {
                    chunk: self.chunk.clone(),
                    handler_ip,
                    saved_stack_len: self.stack.len(),
                    saved_locals_len: self.locals.len(),
                    frame_count: self.call_stack.len(),
                    saved_lexical_env: self.lexical_env.clone(),
                });
                if crate::trace::enabled() {
                    eprintln!(
                        "[trace] PushHandler handler_ip={} saved_stack_len={} handlers={}",
                        handler_ip,
                        self.stack.len(),
                        self.handlers.len()
                    );
                }
            }
            Op::PopHandler => {
                self.handlers.pop();
                if crate::trace::enabled() {
                    eprintln!("[trace] PopHandler handlers={}", self.handlers.len());
                }
            }
            Op::Throw => {
                let thrown = self.pop()?;
                if crate::trace::enabled() {
                    crate::trace::value_string("[trace] Throw thrown", &thrown);
                    eprintln!(
                        "[trace] Throw stack_size={} handlers={}",
                        self.stack.len(),
                        self.handlers.len()
                    );
                }
                self.throw_value(thrown)?;
            }
            Op::StrictEq => {
                let b = self.pop()?;
                let a = self.pop()?;
                self.stack.push(Value::Bool(values_equal(&a, &b)));
            }
            Op::StrictNeq => {
                let b = self.pop()?;
                let a = self.pop()?;
                self.stack.push(Value::Bool(!values_equal(&a, &b)));
            }
            Op::Yield => {
                let yielded = self.pop()?;
                return Ok(StepOutcome::Yield(yielded));
            }
            Op::YieldStar => {
                if self.yield_star_queue.is_none() {
                    let iterand = self.stack.pop().unwrap_or(Value::Undefined);
                    let mut queue = VecDeque::new();
                    match iterand {
                        Value::Array(arr, _) => {
                            for value in arr.borrow().iter().flatten() {
                                queue.push_back(value.clone());
                            }
                        }
                        other => queue.push_back(other),
                    }
                    self.yield_star_queue = Some(queue);
                }
                if let Some(queue) = &mut self.yield_star_queue
                    && let Some(value) = queue.pop_front()
                {
                    self.ip = self.ip.saturating_sub(1);
                    return Ok(StepOutcome::Yield(value));
                }
                self.yield_star_queue = None;
                self.stack.push(Value::Undefined);
            }
            Op::AwaitPromise => {
                let awaited = self.pop()?;
                match awaited {
                    Value::Promise(state) => {
                        let mut settled = false;
                        loop {
                            let settled_state = {
                                let state_ref = state.borrow();
                                match &*state_ref {
                                    PromiseState::Fulfilled(v) => Some(Ok(v.clone())),
                                    PromiseState::Rejected { reason, .. } => {
                                        Some(Err(reason.clone()))
                                    }
                                    PromiseState::Pending { .. } => None,
                                }
                            };

                            match settled_state {
                                Some(Ok(value)) => {
                                    self.stack.push(value);
                                    settled = true;
                                    break;
                                }
                                Some(Err(reason)) => {
                                    self.throw_value(reason)?;
                                    settled = true;
                                    break;
                                }
                                None => {
                                    if !self.microtasks.is_empty()
                                        || !self.dynamic_import_jobs.is_empty()
                                    {
                                        self.run_microtasks_if_needed()?;
                                        self.maybe_collect_garbage_prototype();
                                        continue;
                                    }
                                    break;
                                }
                            }
                        }
                        if !settled {
                            return Err(RuntimeError::from(
                                "Promise did not settle during synchronous await",
                            ));
                        }
                    }
                    other => self.stack.push(other),
                }
            }
        }
        Ok(StepOutcome::Continue)
    }

    // -----------------------------------------------------------------------
    // ECMAScript coercions
    // -----------------------------------------------------------------------

    /// Apply ECMAScript `ToPrimitive` coercion with an optional preferred hint.
    ///
    /// # Errors
    ///
    /// Returns a [`crate::errors::RuntimeError`] if the coercion fails.
    pub fn to_primitive(&mut self, v: &Value, hint: Option<&str>) -> Result<Value, RuntimeError> {
        match v {
            Value::Object(_)
            | Value::Array(..)
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
                if let Some((desc, _holder)) =
                    self.lookup_property_descriptor(v, &to_prim_key, 0)?
                    && let PropertyDescriptor::Data { value: method, .. } = desc
                    && matches!(
                        method,
                        Value::Closure { .. } | Value::Builtin(_) | Value::BuiltinObject(_, _)
                    )
                {
                    if crate::trace::enabled() {
                        crate::trace::value_string(
                            "[trace] @@toPrimitive candidate method",
                            &method,
                        );
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
                            | Value::Closure { .. }
                            | Value::Builtin(_)
                            | Value::BuiltinObject(_, _)
                            | Value::Promise(_)
                    ) {
                        if crate::trace::enabled() {
                            crate::trace::value_string(
                                "[trace] @@toPrimitive accepted primitive",
                                &rv,
                            );
                        }
                        self.gc_temp_roots.truncate(temp_root_base);
                        return Ok(rv);
                    } else if crate::trace::enabled() {
                        crate::trace::value_string(
                            "[trace] @@toPrimitive produced non-primitive",
                            &rv,
                        );
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
                            crate::trace::value_string(
                                &format!("[trace] candidate method '{}':", name),
                                &method,
                            );
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
    pub fn to_string(&mut self, v: &Value) -> Result<String, RuntimeError> {
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
    pub fn to_string_ref<'a>(&mut self, v: &'a Value) -> Result<&'a str, RuntimeError> {
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
    pub fn function_to_string(&mut self, value: &Value) -> Result<String, RuntimeError> {
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

    fn native_function_name(&mut self, value: &Value) -> Option<String> {
        match self.get_property_host(value, &PropertyKey::String("name".to_string())) {
            Ok(Value::String(name)) => Some(name),
            Ok(_) => None,
            Err(_) => None,
        }
    }

    fn format_native_function_source(&mut self, value: &Value) -> String {
        let name = self.native_function_name(value).unwrap_or_default();
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
    pub fn to_number_value(&mut self, v: &Value) -> Result<f64, RuntimeError> {
        match v {
            Value::Number(n) => Ok(*n),
            Value::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
            Value::String(s) => Ok(parse_js_number_string(s)),
            Value::Undefined => Ok(f64::NAN),
            Value::Null => Ok(0.0),
            Value::Uninitialized => {
                Err(self
                    .throw_reference_error("Cannot access lexical binding before initialization"))
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
    pub fn to_property_key(&mut self, v: &Value) -> Result<PropertyKey, RuntimeError> {
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
    pub fn to_boolean(&self, v: &Value) -> bool {
        v.is_truthy()
    }

    /// Kernel helper: abstract equality comparison (==). Exposed so the
    /// soft layer can delegate equality semantics to the kernel during
    /// migration.
    ///
    /// # Errors
    ///
    /// Returns a [`crate::errors::RuntimeError`] if the coercion fails.
    pub fn abstract_equality(&mut self, x: &Value, y: &Value) -> Result<bool, RuntimeError> {
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
            Value::String(_)
                | Value::Number(_)
                | Value::BigInt(_)
                | Value::Symbol(_, _)
                | Value::Bool(_)
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
            Value::String(_)
                | Value::Number(_)
                | Value::BigInt(_)
                | Value::Symbol(_, _)
                | Value::Bool(_)
        ) {
            let yprim = self.to_primitive(y, None)?;
            return self.abstract_equality(x, &yprim);
        }

        Ok(false)
    }

    /// Read a property descriptor from a value, following the prototype chain.
    ///
    /// # Errors
    ///
    /// Returns a [`crate::errors::RuntimeError`] if the operation fails.
    /// Pop the return value, restore the caller's frame, and push the result.
    pub(super) fn execute_return(&mut self) -> Result<(), RuntimeError> {
        let frame = self.call_stack.pop().ok_or("Return without call frame")?;
        let result = self.stack.pop().unwrap_or(Value::Undefined);

        if crate::trace::enabled() {
            eprintln!(
                "[trace] execute_return stack_before_truncate={} frame.stack_base={} result={:?}",
                self.stack.len(),
                frame.stack_base,
                result
            );
            for (i, v) in self.stack.iter_rev().take(6).enumerate() {
                crate::trace::value_string(&format!("[trace]   ret_stack[{}]", i), v);
            }
        }

        // Runtime return-type check intentionally disabled in JS-first mode.
        // TypeScript return annotations are erased at runtime.

        // Restore caller state.
        self.current_slot_types = frame.slot_types;
        self.current_slot_names = frame.slot_names;
        self.chunk = frame.chunk;
        self.ip = frame.ip;
        self.stack.restore_from(frame.stack);
        self.locals = frame.locals;
        self.this_value = frame.this_value;
        self.current_closure_env = frame.closure_env;
        self.lexical_env = frame.lexical_env;
        self.yield_star_queue = frame.yield_star_queue;
        self.current_arguments_object = frame.arguments_object;
        self.current_arguments_mapped_count = frame.arguments_mapped_count;
        self.current_strict_mode = self.call_stack.last().is_some_and(|f| f.is_strict);
        // Handlers registered in the returning frame are now invalid.
        self.prune_stale_handlers();
        self.stack.push(result);

        if crate::trace::enabled() {
            eprintln!(
                "[trace] execute_return stack_after_push={}",
                self.stack.len()
            );
        }

        Ok(())
    }

    fn delete_property_op(&mut self) -> Result<(), RuntimeError> {
        let key = self.pop()?;
        let obj = self.pop()?;
        let prop = self.to_property_key(&key)?;
        let deleted = self.delete_property_host(&obj, &prop)?;
        if !deleted && self.is_strict_mode() {
            return Err(self.throw_type_error("Cannot delete property in strict mode"));
        }
        self.stack.push(Value::Bool(deleted));
        Ok(())
    }

    fn delete_identifier_op(&mut self) -> Result<(), RuntimeError> {
        let name_val = self.pop()?;
        let name = if let Value::String(s) = name_val {
            s
        } else {
            return Err(self.throw_type_error("DeleteIdentifier requires string constant"));
        };
        let mut env = Some(self.lexical_env.clone());
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
                self.stack.push(Value::Bool(false));
                return Ok(());
            }
            if let Some(binding_object) = binding_object {
                let key = PropertyKey::String(name.clone());
                let found = self.has_property_host(&binding_object, &key)?;
                if found
                    && (!with_environment || !self.is_unscopable_binding(&binding_object, &name)?)
                {
                    let deleted = self.delete_property_host(&binding_object, &key)?;
                    self.bump_object_generation(&binding_object);
                    self.stack.push(Value::Bool(deleted));
                    return Ok(());
                }
            }
            env = outer;
        }

        let global_obj = Value::Object(self.globals.clone());
        let key = PropertyKey::String(name.clone());
        let deleted = self.delete_property_host(&global_obj, &key)?;
        self.bump_object_generation(&global_obj);
        self.stack.push(Value::Bool(deleted));
        Ok(())
    }

    fn has_property_op(&mut self) -> Result<(), RuntimeError> {
        let key = self.pop()?;
        let obj = self.pop()?;
        let prop = self.to_property_key(&key)?;
        let has = self.has_property_host(&obj, &prop)?;
        self.stack.push(Value::Bool(has));
        Ok(())
    }

    fn iterator_next_op(&mut self) -> Result<(), RuntimeError> {
        let iterator = self.pop()?;
        let next_fn =
            self.get_property_host(&iterator, &PropertyKey::String("next".to_string()))?;
        let result = self.call_value_with_this(&next_fn, &iterator, &[])?;
        self.stack.push(result);
        Ok(())
    }

    fn iterator_close_op(&mut self) -> Result<(), RuntimeError> {
        let iterator = self.pop()?;
        let return_fn =
            self.get_property_host(&iterator, &PropertyKey::String("return".to_string()))?;
        let is_callable = matches!(
            return_fn,
            Value::Closure { .. } | Value::Builtin(_) | Value::BuiltinObject(_, _)
        );
        if is_callable {
            let _ = self.call_value_with_this(&return_fn, &iterator, &[])?;
        }
        self.stack.push(iterator);
        Ok(())
    }

    fn get_global_ic(&mut self) -> Result<(), RuntimeError> {
        let idx = self.read_short()? as usize;
        let name = self.string_constant(idx)?;
        let value = if let Some(value) = self.resolve_identifier_value(&name)? {
            value
        } else {
            return Err(self.throw_reference_error(&format!("{name} is not defined")));
        };
        self.stack.push(value);
        Ok(())
    }

    pub(super) fn bump_object_generation(&mut self, obj: &Value) {
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

    // -----------------------------------------------------------------------
    // Control flow
    // -----------------------------------------------------------------------

    fn jump(&mut self, offset: isize) -> Result<(), RuntimeError> {
        let new_ip = (self.ip as isize) + offset;
        if new_ip < 0 || (new_ip as usize) > self.chunk.code.len() {
            return Err(RuntimeError::from(format!(
                "Jump target out of bounds (ip={} offset={})",
                self.ip, offset
            )));
        }
        self.ip = new_ip as usize;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Byte-reading helpers
    // -----------------------------------------------------------------------

    #[inline(always)]
    pub(super) fn read_byte(&mut self) -> Result<u8, RuntimeError> {
        if self.ip >= self.chunk.code.len() {
            return Err(RuntimeError::from(format!(
                "Unexpected end of bytecode at ip {}",
                self.ip
            )));
        }
        let b = self.chunk.code[self.ip];
        self.ip += 1;
        Ok(b)
    }

    #[inline(always)]
    fn read_short_signed(&mut self) -> Result<isize, RuntimeError> {
        let lo = self.read_byte()? as u16;
        let hi = self.read_byte()? as u16;
        let u = (hi << 8) | lo;
        Ok((u as i16) as isize)
    }

    #[inline(always)]
    fn read_short(&mut self) -> Result<u16, RuntimeError> {
        let lo = self.read_byte()? as u16;
        let hi = self.read_byte()? as u16;
        Ok((hi << 8) | lo)
    }

    // -----------------------------------------------------------------------
    // Constant pool helpers
    // -----------------------------------------------------------------------

    #[inline(always)]
    fn constant(&self, idx: usize) -> Result<Value, RuntimeError> {
        self.chunk
            .constants
            .get(idx)
            .cloned()
            .ok_or_else(|| RuntimeError::from("Bad constant index"))
    }

    #[inline(always)]
    fn string_constant(&self, idx: usize) -> Result<String, RuntimeError> {
        match self.chunk.constants.get(idx) {
            Some(Value::String(s)) => Ok(s.clone()),
            _ => Err(RuntimeError::from("Constant not a string")),
        }
    }

    // -----------------------------------------------------------------------
    // Stack helpers
    // -----------------------------------------------------------------------

    #[inline(always)]
    pub(super) fn pop(&mut self) -> Result<Value, RuntimeError> {
        self.stack.pop().ok_or_else(|| {
        if crate::trace::enabled() {
            eprintln!("[diag] Stack underflow");
            // Extra diagnostics: print some VM state to help map the
            // underflow to the currently-executing frame and chunk.
                eprintln!(
                    "[diag] chunk_ptr={:p} chunk_len={} frames={} handlers={}",
                    Rc::as_ptr(&self.chunk),
                    self.chunk.code.len(),
                    self.call_stack.len(),
                    self.handlers.len()
                );
                if let Some(f) = self.call_stack.last() {
                    eprintln!("[diag] top_frame.stack_base={} top_frame.ip={} ", f.stack_base, f.ip);
                    // Print the owning chunk for the top frame and a summary of
                    // any function prototypes referenced by the current chunk.
                    eprintln!(
                        "[diag] top_frame.chunk_ptr={:p} chunk_code_len={} functions={}",
                        Rc::as_ptr(&f.chunk),
                        f.chunk.code.len(),
                        f.chunk.functions.len()
                    );
                    // List function proto summaries (limited to first 20)
                    for (i, proto) in f.chunk.functions.iter().enumerate().take(20) {
                        eprintln!(
                            "[diag]   fn[{}] name='{}' arity={} captures_this={} proto_chunk_ptr={:p} chunk_len={}",
                            i,
                            proto.name,
                            proto.arity,
                            proto.captures_this,
                            Rc::as_ptr(&proto.chunk),
                            proto.chunk.code.len()
                        );
                }
            }
                let mut found_owner = false;
                for (fi, frame) in self.call_stack.iter().enumerate().rev().take(8) {
                    if let Some(proto) = frame
                        .chunk
                        .functions
                        .iter()
                        .find(|proto| Rc::as_ptr(&proto.chunk) == Rc::as_ptr(&self.chunk))
                    {
                        eprintln!(
                            "[diag] current chunk owner: frame[{}] proto='{}' span={}..{} proto_chunk_ptr={:p} proto_chunk_len={}",
                            fi,
                            proto.name,
                            proto.span.start,
                            proto.span.end,
                            Rc::as_ptr(&proto.chunk),
                            proto.chunk.code.len()
                        );
                        if let Some(src) = proto.source_text() {
                            eprintln!("[diag] current chunk source:");
                            for (line_no, line) in src.lines().take(12).enumerate() {
                                eprintln!("[diag]   {:>5}: {}", line_no + 1, line);
                            }
                        }
                        found_owner = true;
                        break;
                    }
                }
                if !found_owner {
                    eprintln!(
                        "[diag] current chunk owner: <unknown> ptr={:p} len={}",
                        Rc::as_ptr(&self.chunk),
                        self.chunk.code.len()
                    );
                }
            eprintln!("[diag] Active frames (top -> bottom):");
            for (fi, frame) in self.call_stack.iter().enumerate().rev().take(8) {
                eprintln!(
                    "[diag]  frame[{}] chunk_ptr={:p} chunk_len={} ip={} stack_base={} is_strict={}",
                    fi,
                    Rc::as_ptr(&frame.chunk),
                    frame.chunk.code.len(),
                    frame.ip,
                    frame.stack_base,
                    frame.is_strict
                );
                if let Some(&line_no) = frame.chunk.lines.get(frame.ip.saturating_sub(1)) {
                    eprintln!("[diag]  frame[{}] source line {}:", fi, line_no);
                    let line_idx = line_no.saturating_sub(1) as usize;
                    for (idx, line) in frame.chunk.source_text.lines().enumerate() {
                        if idx + 1 >= line_idx.saturating_sub(2)
                            && idx < line_idx.saturating_add(2)
                        {
                            eprintln!("[diag]    {:>5}: {}", idx + 1, line);
                        }
                    }
                }
                let fstart = frame.ip.saturating_sub(8);
                let fend = usize::min(frame.ip + 8, frame.chunk.code.len());
                eprintln!("[diag]  frame[{}] disasm ({}..{}):", fi, fstart, fend);
                let mut k = fstart;
                while k < fend {
                    let b = frame.chunk.code[k];
                    if let Some(op) = Op::from_byte(b) {
                        let width = opcode_width(op);
                        match op {
                            Op::Const => {
                                let hi = frame.chunk.code.get(k + 1).cloned().unwrap_or(0) as u16;
                                let lo = frame.chunk.code.get(k + 2).cloned().unwrap_or(0) as u16;
                                let arg = (hi << 8) | lo;
                                eprintln!("[diag]    {:#06}  Const       {}", k, arg);
                            }
                            Op::Jump
                            | Op::JumpIfFalse
                            | Op::JumpIfTrue
                            | Op::JumpIfNullish
                            | Op::PushHandler => {
                                let hi = frame.chunk.code.get(k + 1).cloned().unwrap_or(0);
                                let lo = frame.chunk.code.get(k + 2).cloned().unwrap_or(0);
                                let offset = ((hi as i16) << 8) | (lo as i16);
                                eprintln!("[diag]    {:#06}  {:?} {}", k, op, offset);
                            }
                            Op::CompareJumpIfFalse => {
                                let cmp = frame.chunk.code.get(k + 1).cloned().unwrap_or(0);
                                let hi = frame.chunk.code.get(k + 2).cloned().unwrap_or(0);
                                let lo = frame.chunk.code.get(k + 3).cloned().unwrap_or(0);
                                let offset = ((hi as i16) << 8) | (lo as i16);
                                eprintln!("[diag]    {:#06}  {:?} cmp={} {}", k, op, cmp, offset);
                            }
                            _ if width == 2 => {
                                let arg = frame.chunk.code.get(k + 1).cloned().unwrap_or(0);
                                eprintln!("[diag]    {:#06}  {:?} {}", k, op, arg);
                            }
                            _ => eprintln!("[diag]    {:#06}  {:?}", k, op),
                        }
                        k += width;
                    } else {
                        eprintln!("[diag]    {:#06}  ???  0x{:02x}", k, b);
                        k += 1;
                    }
                }
                if let Some(proto_src) = frame
                    .chunk
                    .functions
                    .iter()
                    .find(|proto| Rc::as_ptr(&proto.chunk) == Rc::as_ptr(&frame.chunk))
                    .and_then(|proto| proto.source_text())
                {
                    eprintln!("[diag]  frame[{}] source:", fi);
                    for (line_no, line) in proto_src.lines().take(8).enumerate() {
                        eprintln!("[diag]    {:>5}: {}", line_no + 1, line);
                    }
                }
            }
            // Print a short constant pool dump for the current chunk (limited).
            eprintln!("[diag] chunk constants (len={}):", self.chunk.constants.len());
            for (i, c) in self.chunk.constants.iter().enumerate().take(32) {
                eprintln!("[diag]   const[{}] = {:?}", i, c);
            }
                let start = self.ip.saturating_sub(24);
                let end = usize::min(self.ip + 24, self.chunk.code.len());
                eprintln!("[diag] disasm window ({}..{}):", start, end);
                let mut j = start;
                while j < end {
                    let b = self.chunk.code[j];
                    if let Some(op) = Op::from_byte(b) {
                        let width = opcode_width(op);
                        match op {
                            Op::Const => {
                                let hi = self.chunk.code.get(j + 1).cloned().unwrap_or(0) as u16;
                                let lo = self.chunk.code.get(j + 2).cloned().unwrap_or(0) as u16;
                                let arg = (hi << 8) | lo;
                                if let Some(c) = self.chunk.constants.get(arg as usize) {
                                    eprintln!("[diag]   {}: {:?} {:?}", j, op, c);
                                } else {
                                    eprintln!("[diag]   {}: {:?} {}", j, op, arg);
                                }
                            }
                            Op::Jump
                            | Op::JumpIfFalse
                            | Op::JumpIfTrue
                            | Op::JumpIfNullish
                            | Op::PushHandler => {
                                let hi = self.chunk.code.get(j + 1).cloned().unwrap_or(0);
                                let lo = self.chunk.code.get(j + 2).cloned().unwrap_or(0);
                                let offset = ((hi as i16) << 8) | (lo as i16);
                                eprintln!("[diag]   {}: {:?} {}", j, op, offset);
                            }
                            Op::CompareJumpIfFalse => {
                                let cmp = self.chunk.code.get(j + 1).cloned().unwrap_or(0);
                                let hi = self.chunk.code.get(j + 2).cloned().unwrap_or(0);
                                let lo = self.chunk.code.get(j + 3).cloned().unwrap_or(0);
                                let offset = ((hi as i16) << 8) | (lo as i16);
                                eprintln!("[diag]   {}: {:?} cmp={} {}", j, op, cmp, offset);
                            }
                            Op::SetLocal
                            | Op::GetLocal
                            | Op::Call
                            | Op::Construct
                            | Op::PopN
                            | Op::GetBuiltin
                            | Op::GetProperty
                            | Op::SetProperty
                            | Op::GetPropertyIC
                            | Op::SetPropertyIC
                            | Op::MakeClosure => {
                                let arg = self.chunk.code.get(j + 1).cloned().unwrap_or(0);
                                eprintln!("[diag]   {}: {:?} {}", j, op, arg);
                            }
                            Op::SetGlobal | Op::GetGlobal | Op::GetGlobalIC => {
                                let hi = self.chunk.code.get(j + 1).cloned().unwrap_or(0) as u16;
                                let lo = self.chunk.code.get(j + 2).cloned().unwrap_or(0) as u16;
                                let arg = ((hi << 8) | lo) as usize;
                                eprintln!("[diag]   {}: {:?} {}", j, op, arg);
                            }
                            _ => eprintln!("[diag]   {}: {:?}", j, op),
                        }
                        j += width;
                    } else {
                        eprintln!("[diag]   {}: <invalid {}>", j, b);
                        j += 1;
                    }
                }
                if self.chunk.code.len() <= 256 {
                    eprintln!("[diag] full current-chunk disasm:");
                    let mut full = 0usize;
                    while full < self.chunk.code.len() {
                        let b = self.chunk.code[full];
                        if let Some(op) = Op::from_byte(b) {
                            let width = opcode_width(op);
                            match op {
                                Op::Const => {
                                    let hi = self.chunk.code.get(full + 1).cloned().unwrap_or(0) as u16;
                                    let lo = self.chunk.code.get(full + 2).cloned().unwrap_or(0) as u16;
                                    let arg = (hi << 8) | lo;
                                    eprintln!("[diag]   {:#06}  Const       {}", full, arg);
                                }
                                Op::Jump
                                | Op::JumpIfFalse
                                | Op::JumpIfTrue
                                | Op::JumpIfNullish
                                | Op::PushHandler => {
                                    let hi = self.chunk.code.get(full + 1).cloned().unwrap_or(0);
                                    let lo = self.chunk.code.get(full + 2).cloned().unwrap_or(0);
                                    let offset = ((hi as i16) << 8) | (lo as i16);
                                    eprintln!("[diag]   {:#06}  {:?} {}", full, op, offset);
                                }
                                Op::CompareJumpIfFalse => {
                                    let cmp = self.chunk.code.get(full + 1).cloned().unwrap_or(0);
                                    let hi = self.chunk.code.get(full + 2).cloned().unwrap_or(0);
                                    let lo = self.chunk.code.get(full + 3).cloned().unwrap_or(0);
                                    let offset = ((hi as i16) << 8) | (lo as i16);
                                    eprintln!("[diag]   {:#06}  {:?} cmp={} {}", full, op, cmp, offset);
                                }
                                _ if width == 2 => {
                                    let arg = self.chunk.code.get(full + 1).cloned().unwrap_or(0);
                                    eprintln!("[diag]   {:#06}  {:?} {}", full, op, arg);
                                }
                                _ => eprintln!("[diag]   {:#06}  {:?}", full, op),
                            }
                            full += width;
                        } else {
                            eprintln!("[diag]   {:#06}  ???  0x{:02x}", full, b);
                            full += 1;
                        }
                    }
                }
                let raw_start = 44usize;
                let raw_end = usize::min(56usize, self.chunk.code.len());
                eprintln!("[diag] raw bytes ({}..{}):", raw_start, raw_end);
                for i in raw_start..raw_end {
                    eprintln!("[diag]   {:>5}: 0x{:02x}", i, self.chunk.code[i]);
                }
                let raw_start2 = 188usize;
                let raw_end2 = usize::min(196usize, self.chunk.code.len());
                eprintln!("[diag] raw bytes ({}..{}):", raw_start2, raw_end2);
                for i in raw_start2..raw_end2 {
                    eprintln!("[diag]   {:>5}: 0x{:02x}", i, self.chunk.code[i]);
                }
                eprintln!("[diag] Stack size: {}", self.stack.len());
                if let Some(&line_no) = self.chunk.lines.get(self.ip.saturating_sub(1)) {
                    eprintln!("[diag] source line {}:", line_no);
                    let line_idx = line_no.saturating_sub(1) as usize;
                    for (idx, line) in self.chunk.source_text.lines().enumerate() {
                        if idx + 1 >= line_idx.saturating_sub(2)
                            && idx < line_idx.saturating_add(2)
                        {
                            eprintln!("[diag]   {:>5}: {}", idx + 1, line);
                        }
                    }
                }
            }
            RuntimeError::from("Stack underflow")
        })
    }

    // -----------------------------------------------------------------------
    // Arithmetic helpers
    // -----------------------------------------------------------------------

    #[inline(always)]
    fn binary_num_bigint<F, G>(&mut self, f: F, g: G) -> Result<(), RuntimeError>
    where
        F: FnOnce(f64, f64) -> f64,
        G: FnOnce(BigIntValue, BigIntValue) -> Result<BigIntValue, RuntimeError>,
    {
        let b = self.pop()?;
        let a = self.pop()?;
        match (a, b) {
            (Value::BigInt(x), Value::BigInt(y)) => {
                let v = g(x, y)?;
                self.stack.push(Value::BigInt(v));
                Ok(())
            }
            (a, b) => {
                let (x, y) = (to_number(&a)?, to_number(&b)?);
                self.stack.push(Value::Number(f(x, y)));
                Ok(())
            }
        }
    }

    // -----------------------------------------------------------------------
    // Diagnostics (only emitted when tracing is enabled)
    // -----------------------------------------------------------------------

    pub(super) fn dump_diag(&self, label: &str, offender: &Value) {
        if !crate::trace::enabled() {
            return;
        }

        eprintln!("[diag] {}: actual={}", label, offender.type_name());
        crate::trace::value_string("[diag] offending value", offender);

        eprintln!("[diag] Stack snapshot (top -> bottom):");
        for (idx, v) in self.stack.iter_rev().take(32).enumerate() {
            crate::trace::value_string(&format!("[diag]   stack[{}]", idx), v);
        }

        let start = self.ip.saturating_sub(8);
        let end = usize::min(self.ip + 8, self.chunk.code.len());
        eprintln!("[diag] disasm window ({}..{}):", start, end);

        // Print current chunk pointer to help correlate disasm with function proto
        eprintln!(
            "[diag] current chunk ptr={:p} chunk_len={}",
            Rc::as_ptr(&self.chunk),
            self.chunk.code.len()
        );

        // Print a brief summary of any function prototypes present in the
        // current chunk to help map MakeClosure indexes to human-readable
        // names when diagnosing callee/runtime mismatches.
        eprintln!(
            "[diag] current chunk functions (len={}):",
            self.chunk.functions.len()
        );
        for (i, proto) in self.chunk.functions.iter().enumerate().take(40) {
            eprintln!(
                "[diag]   fn[{}] name='{}' arity={} has_prototype_property={} is_async={} proto_chunk_ptr={:p} chunk_len={}",
                i,
                proto.name,
                proto.arity,
                proto.has_prototype_property,
                proto.is_async,
                Rc::as_ptr(&proto.chunk),
                proto.chunk.code.len()
            );
        }

        // Also print a short dump of the constant pool for this chunk which
        // helps map large Const indices back to concrete values during
        // diagnostic analysis.
        eprintln!(
            "[diag] chunk constants (len={}):",
            self.chunk.constants.len()
        );
        for (i, c) in self.chunk.constants.iter().enumerate().take(64) {
            eprintln!("[diag]   const[{}] = {:?}", i, c);
        }

        // Additionally, some Const operands printed in caller disassembly may
        // refer to constants in a function prototype's own chunk. To help
        // correlate large Const indices back to their owning proto, print a
        // brief summary and constant dump for the first few function proto
        // chunks referenced by the current chunk. This is intentionally
        // limited and only for diagnostic builds (gated by tracing).
        for (i, proto) in self.chunk.functions.iter().enumerate().take(4) {
            eprintln!(
                "[diag] proto[{}] name='{}' proto_chunk_ptr={:p} proto_chunk_len={} proto_consts_len={}",
                i,
                proto.name,
                Rc::as_ptr(&proto.chunk),
                proto.chunk.code.len(),
                proto.chunk.constants.len()
            );
            // Print a small slice of the proto's constant pool to help map
            // Const operands that are actually meant to index this proto.
            for (ci, cc) in proto.chunk.constants.iter().enumerate().take(16) {
                eprintln!("[diag]   proto[{}].const[{}] = {:?}", i, ci, cc);
            }

            // Disassemble the start of the proto chunk to give context where
            // MakeClosure/Call sequences often appear (IIFE patterns, etc.).
            let end = usize::min(128, proto.chunk.code.len());
            if end > 0 {
                eprintln!("[diag] proto[{}] disasm (0..{}):", i, end);
                let mut j = 0usize;
                while j < end {
                    let b = proto.chunk.code[j];
                    if let Some(op) = Op::from_byte(b) {
                        let width = opcode_width(op);
                        match op {
                            Op::Const => {
                                let hi = proto.chunk.code.get(j + 1).cloned().unwrap_or(0) as u16;
                                let lo = proto.chunk.code.get(j + 2).cloned().unwrap_or(0) as u16;
                                let arg = (hi << 8) | lo;
                                let repr = match proto.chunk.constants.get(arg as usize) {
                                    Some(Value::String(s)) => format!("'{}'", s),
                                    Some(v) => v.type_name().to_string(),
                                    None => "<undef>".to_string(),
                                };
                                eprintln!("[diag] {:#06}  Const       {} ({})", j, arg, repr);
                            }
                            Op::GetGlobal | Op::SetGlobal => {
                                let hi = proto.chunk.code.get(j + 1).cloned().unwrap_or(0) as u16;
                                let lo = proto.chunk.code.get(j + 2).cloned().unwrap_or(0) as u16;
                                let arg = ((hi << 8) | lo) as usize;
                                let name = match proto.chunk.constants.get(arg) {
                                    Some(Value::String(s)) => s.clone(),
                                    _ => "<non-string>".to_string(),
                                };
                                eprintln!("[diag] {:#06}  {:12?} {} ({})", j, op, arg, name);
                            }
                            Op::Jump
                            | Op::JumpIfFalse
                            | Op::JumpIfTrue
                            | Op::JumpIfNullish
                            | Op::PushHandler => {
                                let hi = proto.chunk.code.get(j + 1).cloned().unwrap_or(0) as u16;
                                let lo = proto.chunk.code.get(j + 2).cloned().unwrap_or(0) as u16;
                                let val = ((hi << 8) | lo) as i16;
                                eprintln!("[diag] {:#06}  {:12?} {}", j, op, val);
                            }
                            Op::CompareJumpIfFalse => {
                                let cmp = proto.chunk.code.get(j + 1).cloned().unwrap_or(0);
                                let hi = proto.chunk.code.get(j + 2).cloned().unwrap_or(0) as u16;
                                let lo = proto.chunk.code.get(j + 3).cloned().unwrap_or(0) as u16;
                                let val = ((hi << 8) | lo) as i16;
                                eprintln!("[diag] {:#06}  {:12?} cmp={} {}", j, op, cmp, val);
                            }
                            _ if opcode_width(op) == 2 => {
                                let arg = proto.chunk.code.get(j + 1).cloned().unwrap_or(0);
                                eprintln!("[diag] {:#06}  {:12?} {}", j, op, arg);
                            }
                            _ => {
                                eprintln!("[diag] {:#06}  {:?}", j, op);
                            }
                        }
                        j += width;
                    } else {
                        eprintln!("[diag] {:#06}  ???  0x{:02x}", j, b);
                        j += 1;
                    }
                }
            }
        }

        let mut j = start;
        while j < end {
            let b = self.chunk.code[j];
            if let Some(op) = Op::from_byte(b) {
                let width = opcode_width(op);
                match op {
                    Op::Const => {
                        let hi = self.chunk.code.get(j + 1).cloned().unwrap_or(0) as u16;
                        let lo = self.chunk.code.get(j + 2).cloned().unwrap_or(0) as u16;
                        let arg = (hi << 8) | lo;
                        if (arg as usize) < self.chunk.constants.len() {
                            let repr = match self.chunk.constants.get(arg as usize) {
                                Some(Value::String(s)) => format!("'{}'", s),
                                Some(v) => v.type_name().to_string(),
                                None => "<undef>".to_string(),
                            };
                            eprintln!("[diag] {:#06}  Const       {} ({})", j, arg, repr);
                        } else {
                            eprintln!(
                                "[diag] {:#06}  Const       {} (<out-of-range for current chunk constants len={}>)",
                                j,
                                arg,
                                self.chunk.constants.len()
                            );
                            // Try to locate a function proto whose constant pool would contain this index
                            for (pi, proto) in self.chunk.functions.iter().enumerate() {
                                if (arg as usize) < proto.chunk.constants.len() {
                                    eprintln!(
                                        "[diag]   -> const {} appears valid in proto[{}] '{}', proto_chunk_ptr={:p} proto_consts_len={}",
                                        arg,
                                        pi,
                                        proto.name,
                                        Rc::as_ptr(&proto.chunk),
                                        proto.chunk.constants.len()
                                    );
                                    // Print a small slice of that proto's constants
                                    for (ci, cc) in proto
                                        .chunk
                                        .constants
                                        .iter()
                                        .enumerate()
                                        .skip((arg as usize).saturating_sub(4))
                                        .take(8)
                                    {
                                        eprintln!(
                                            "[diag]     proto[{}].const[{}] = {:?}",
                                            pi, ci, cc
                                        );
                                    }
                                }
                            }
                        }
                    }
                    Op::GetGlobal | Op::SetGlobal | Op::GetGlobalIC => {
                        let hi = self.chunk.code.get(j + 1).cloned().unwrap_or(0) as u16;
                        let lo = self.chunk.code.get(j + 2).cloned().unwrap_or(0) as u16;
                        let arg = ((hi << 8) | lo) as usize;
                        let name = match self.chunk.constants.get(arg) {
                            Some(Value::String(s)) => s.clone(),
                            _ => "<non-string>".to_string(),
                        };
                        eprintln!("[diag] {:#06}  {:12?} {} ({})", j, op, arg, name);
                    }
                    Op::Jump
                    | Op::JumpIfFalse
                    | Op::JumpIfTrue
                    | Op::JumpIfNullish
                    | Op::PushHandler => {
                        let hi = self.chunk.code.get(j + 1).cloned().unwrap_or(0) as u16;
                        let lo = self.chunk.code.get(j + 2).cloned().unwrap_or(0) as u16;
                        let val = ((hi << 8) | lo) as i16;
                        eprintln!("[diag] {:#06}  {:12?} {}", j, op, val);
                    }
                    _ if width == 2 => {
                        let arg = self.chunk.code.get(j + 1).cloned().unwrap_or(0);
                        eprintln!("[diag] {:#06}  {:12?} {}", j, op, arg);
                    }
                    _ => {
                        eprintln!("[diag] {:#06}  {:?}", j, op);
                    }
                }
                j += width;
            } else {
                eprintln!("[diag] {:#06}  ???  0x{:02x}", j, b);
                j += 1;
            }
        }

        // Print active call frames to help map which chunk/ip belongs to which
        // JavaScript function. Limit to first 8 frames for brevity.
        eprintln!("[diag] Frames (count={}):", self.call_stack.len());
        for (fi, frame) in self.call_stack.iter().enumerate().take(8) {
            eprintln!(
                "[diag]  frame[{}] chunk_ptr={:p} chunk_len={} ip={} stack_base={} is_strict={}",
                fi,
                Rc::as_ptr(&frame.chunk),
                frame.chunk.code.len(),
                frame.ip,
                frame.stack_base,
                frame.is_strict,
            );

            // Disassemble a small window around the frame ip to see recent ops
            let fstart = frame.ip.saturating_sub(8);
            let fend = usize::min(frame.ip + 8, frame.chunk.code.len());
            eprintln!("[diag]  frame[{}] disasm ({}..{}):", fi, fstart, fend);
            let mut k = fstart;
            while k < fend {
                let b = frame.chunk.code[k];
                if let Some(op) = Op::from_byte(b) {
                    let width = opcode_width(op);
                    match op {
                        Op::Const => {
                            let hi = frame.chunk.code.get(k + 1).cloned().unwrap_or(0) as u16;
                            let lo = frame.chunk.code.get(k + 2).cloned().unwrap_or(0) as u16;
                            let arg = (hi << 8) | lo;
                            let repr = match frame.chunk.constants.get(arg as usize) {
                                Some(Value::String(s)) => format!("'{}'", s),
                                Some(v) => v.type_name().to_string(),
                                None => "<undef>".to_string(),
                            };
                            eprintln!("[diag]    {:#06}  Const       {} ({})", k, arg, repr);
                        }
                        Op::GetGlobal | Op::SetGlobal | Op::GetGlobalIC => {
                            let hi = frame.chunk.code.get(k + 1).cloned().unwrap_or(0) as u16;
                            let lo = frame.chunk.code.get(k + 2).cloned().unwrap_or(0) as u16;
                            let arg = ((hi << 8) | lo) as usize;
                            let name = match frame.chunk.constants.get(arg) {
                                Some(Value::String(s)) => s.clone(),
                                _ => "<non-string>".to_string(),
                            };
                            eprintln!("[diag]    {:#06}  {:12?} {} ({})", k, op, arg, name);
                        }
                        _ if opcode_width(op) == 2 => {
                            let arg = frame.chunk.code.get(k + 1).cloned().unwrap_or(0);
                            eprintln!("[diag]    {:#06}  {:12?} {}", k, op, arg);
                        }
                        _ => eprintln!("[diag]    {:#06}  {:?}", k, op),
                    }
                    k += width;
                } else {
                    eprintln!("[diag]    {:#06}  ???  0x{:02x}", k, b);
                    k += 1;
                }
            }
        }
    }

    /// Try to find a runtime value in the constant pools of recent frames.
    /// Returns a vector of (frame_index, const_index) matches.
    pub(super) fn find_constants_containing_value(&self, v: &Value) -> Vec<(usize, usize)> {
        let mut found = Vec::new();
        for (fi, frame) in self.call_stack.iter().enumerate().take(12) {
            for (ci, c) in frame.chunk.constants.iter().enumerate() {
                if values_equal(c, v) {
                    found.push((fi, ci));
                }
            }
        }
        found
    }

    /// Instanceof helper: checks whether `value` has `constructor.prototype` in its prototype chain.
    ///
    /// # Errors
    ///
    /// Returns a [`crate::errors::RuntimeError`] if the operation fails.
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
            let result = self.call_with_this(&method, constructor, &[value.clone()])?;
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
            Value::Object(_)
            | Value::Array(..)
            | Value::Closure { .. }
            | Value::BuiltinObject(_, _) => {}
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
    pub fn is_prototype_of(&mut self, proto: &Value, value: &Value) -> Result<bool, RuntimeError> {
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
    pub fn in_property(&mut self, property: &Value, object: &Value) -> Result<bool, RuntimeError> {
        let key = self.to_property_key(property)?;
        Ok(self.lookup_property_descriptor(object, &key, 0)?.is_some())
    }

    /// typeof helper returning the type string for a value.
    pub fn typeof_value(&self, value: &Value) -> String {
        // `typeof null` is 'object'
        value.type_name().to_string()
    }
}

// ---------------------------------------------------------------------------
// Free functions
// ---------------------------------------------------------------------------

/// Return the bytecode width of an opcode (opcode byte + operand bytes).
fn opcode_width(op: Op) -> usize {
    match op {
        Op::Const => 3,
        Op::GetGlobal | Op::SetGlobal | Op::GetGlobalIC => 3,
        Op::GetLocal
        | Op::SetLocal
        | Op::CloneLocalCell
        | Op::Call
        | Op::CallMethod
        | Op::CallIC
        | Op::TailCallIC
        | Op::TailCallMethod
        | Op::Construct
        | Op::PopN
        | Op::MakeClosure
        | Op::GetUpvalue
        | Op::SetUpvalue
        | Op::GetPropertyIC
        | Op::SetPropertyIC
        | Op::GetBuiltin
        | Op::BuildArray
        | Op::BuildObject
        | Op::Spread
        | Op::ArrayFromSpread => 2,
        Op::Jump | Op::JumpIfFalse | Op::JumpIfTrue | Op::JumpIfNullish | Op::PushHandler => 3,
        Op::CompareJumpIfFalse => 4,
        _ => 1,
    }
}

/// Return own keys for a value when applicable.
fn list_own_keys_of_value(v: &Value) -> Option<Vec<String>> {
    match v {
        Value::Object(data) => Some(
            data.borrow()
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
        _ => None,
    }
}

/// Grow a locals vector so that `slot` is a valid index.
#[inline(always)]
pub(super) fn ensure_slot(locals: &mut Vec<ValueSlot>, slot: usize) {
    if slot >= locals.len() {
        locals.resize_with(slot + 1, || Rc::new(Mutex::new(Value::Undefined)));
    }
}

pub(super) fn object_identity(obj: &Value) -> Option<usize> {
    match obj {
        Value::Object(map) => Some(map.borrow().identity as usize),
        // Use the array's props object identity so IC keys remain stable across
        // dense storage reallocations.
        Value::Array(_, props) => Some(props.borrow().identity as usize),
        Value::Closure { props, .. } => Some(props.borrow().identity as usize),
        Value::BuiltinObject(_, props) => Some(props.borrow().identity as usize),
        _ => None,
    }
}

/// Reference-equality semantics for objects/arrays/closures, structural
/// equality for primitives. Matches JavaScript `===` behaviour.
pub(super) fn values_equal(a: &Value, b: &Value) -> bool {
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
        (Value::Closure { props: ap, .. }, Value::Closure { props: bp, .. }) => Rc::ptr_eq(ap, bp),
        (Value::BuiltinObject(_, ap), Value::BuiltinObject(_, bp)) => Rc::ptr_eq(ap, bp),
        (Value::Promise(ap), Value::Promise(bp)) => Rc::ptr_eq(ap, bp),
        (Value::Builtin(af), Value::Builtin(bf)) => Rc::ptr_eq(af, bf),
        _ => false,
    }
}

/// ECMAScript SameValue semantics used by `Object.is`.
pub fn same_value(a: &Value, b: &Value) -> bool {
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

fn array_index_from_value(value: &Value) -> Option<usize> {
    let Value::Number(n) = value else {
        return None;
    };
    if !n.is_finite() || *n < 0.0 {
        return None;
    }
    let idx = n.trunc();
    if idx != *n || idx >= u32::MAX as f64 {
        return None;
    }
    Some(idx as usize)
}

fn array_element_value(obj: &Value, index: usize) -> Option<Value> {
    let Value::Array(arr, _) = obj else {
        return None;
    };
    arr.borrow().get(index).cloned().flatten()
}

fn set_array_index_property(obj: &Value, index: usize, value: Value) -> Result<(), RuntimeError> {
    let Value::Array(arr, props) = obj else {
        return Ok(());
    };
    let mut p = props.borrow_mut();
    let current_len = array_length(&p);
    let next_len = current_len.max(index.saturating_add(1));
    let dense_limit = arr.borrow().len().saturating_add(MAX_DENSE_ARRAY_GROWTH);
    if index <= dense_limit {
        let mut a = arr.borrow_mut();
        if index >= a.len() {
            a.resize(index + 1, None);
        }
        a[index] = Some(value);
        let next_len = next_len.max(a.len());
        p.properties.insert(
            PropertyKey::String("length".to_string()),
            PropertyDescriptor::data(Value::Number(next_len as f64)),
        );
    } else {
        p.properties.insert(
            PropertyKey::String(index.to_string()),
            PropertyDescriptor::data(value),
        );
        p.properties.insert(
            PropertyKey::String("length".to_string()),
            PropertyDescriptor::data(Value::Number(next_len as f64)),
        );
    }
    Ok(())
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
                let dense_limit = arr.borrow().len().saturating_add(MAX_DENSE_ARRAY_GROWTH);
                if i <= dense_limit {
                    let mut a = arr.borrow_mut();
                    if i >= a.len() {
                        a.resize(i + 1, None);
                    }
                    a[i] = Some(value);
                    p.properties.insert(
                        PropertyKey::String("length".to_string()),
                        PropertyDescriptor::data(Value::Number(a.len() as f64)),
                    );
                } else {
                    p.properties
                        .insert(key.clone(), PropertyDescriptor::data(value));
                    p.properties.insert(
                        PropertyKey::String("length".to_string()),
                        PropertyDescriptor::data(Value::Number(next_len as f64)),
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
                    PropertyDescriptor::data(Value::Number(new_len as f64)),
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

pub(crate) fn build_module_namespace_object(exports: &HashMap<String, ValueSlot>) -> Value {
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
    fn partial_cmp(&self, other: &Value) -> Option<std::cmp::Ordering> {
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
