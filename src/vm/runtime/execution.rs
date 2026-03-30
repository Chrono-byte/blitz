use crate::bytecode::{Chunk, FunctionProto, Op};
use crate::environment::EnvironmentRecord;
use crate::module::ModuleLoader;
use crate::realm::Realm;
use crate::value::{
    BuiltinFn, ObjectData, PromiseState, PropertyDescriptor, PropertyKey, SymbolId, Value,
};
use crate::vm::state::*;
use num_bigint::BigInt as BigIntValue;
use num_traits::{FromPrimitive, Signed, ToPrimitive, Zero};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;

impl Vm {
    fn prune_stale_handlers(&mut self) {
        let active_frames = self.frames.len();
        while self
            .handlers
            .last()
            .is_some_and(|h| h.frame_count > active_frames)
        {
            self.handlers.pop();
        }
    }

    // -----------------------------------------------------------------------
    // Construction & public helpers
    // -----------------------------------------------------------------------

    /// Create a new virtual machine.
    pub fn new(globals: Rc<RefCell<ObjectData>>) -> Self {
        let realm = Realm::with_globals(Rc::clone(&globals));
        let global_env = Rc::new(RefCell::new(EnvironmentRecord::new(None)));
        let mut vm = Self {
            realm,
            chunk: Rc::new(Chunk::new(Rc::new(String::new()))),
            ip: 0,
            stack: Vec::with_capacity(STACK_INITIAL_CAPACITY),
            locals: Vec::with_capacity(LOCALS_INITIAL_CAPACITY),
            this_value: Value::Object(Rc::clone(&globals)),
            current_closure_env: None,
            globals,
            frames: Vec::with_capacity(FRAMES_INITIAL_CAPACITY),
            root_chunk: None,
            current_slot_types: None,
            obj_generations: HashMap::new(),
            bg_runtime: get_global_bg_runtime(),
            handlers: Vec::new(),
            pending_exception: false,
            // Each VM gets its own module loader
            module_loader: Some(ModuleLoader::new()),
            module_table: Vec::new(),
            construct_new_target: None,
            last_uncaught_exception: None,
            microtasks: VecDeque::new(),
            unhandled_rejection_hook: None,
            lexical_env: Rc::clone(&global_env),
            global_env,
            yield_star_queue: None,
        };
        vm.populate_realm_intrinsics();
        vm
    }

    /// Return the current `this` value.
    pub fn this_value(&self) -> Value {
        self.this_value.clone()
    }

    /// Return the current `[[NewTarget]]` value.
    pub fn current_new_target(&self) -> Option<Value> {
        self.construct_new_target.clone()
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
            Value::Promise(_) => self
                .realm
                .intrinsics
                .promise_prototype
                .clone()
                .unwrap_or(Value::Null),
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

    /// Create a new Array object with the given elements and correct prototype.
    pub fn new_array(&self, elems: Vec<Option<Value>>) -> Value {
        let arr_rc = Rc::new(RefCell::new(elems));
        let props = Rc::new(RefCell::new(ObjectData::new()));
        let len = arr_rc.borrow().len();
        props.borrow_mut().properties.insert(
            PropertyKey::String("length".to_string()),
            PropertyDescriptor::data(Value::Number(len as f64)),
        );

        if let Some(proto) = self.realm.intrinsics.array_prototype.clone() {
            props.borrow_mut().prototype = proto;
        }

        Value::Array(arr_rc, props)
    }

    /// Run the prototype mark/sweep collector and return mark statistics.
    #[cfg(feature = "gc_mark_sweep_prototype")]
    pub fn collect_garbage_prototype(&self) -> crate::gc::GcStats {
        let mut roots: Vec<Value> = Vec::new();
        roots.push(Value::Object(Rc::clone(&self.globals)));
        roots.extend(self.stack.iter().cloned());
        roots.extend(self.locals.iter().map(|c| c.borrow().clone()));
        crate::gc::collect(&roots)
    }

    /// Install the minimal kernel helpers and populate intrinsics before use.
    ///
    /// This ensures every VM starts with a fully populated global object and
    /// intrinsic constructors before any bytecode executes.
    pub fn with_runtime(mut self) -> Self {
        // Install minimal kernel constructors/helpers onto the globals map.
        // This is defensive: the installer will only populate missing keys
        // and will not override host-provided helpers.
        crate::runtime_utils::install_minimal_kernel_constructors(&self.globals);
        // Populate realm intrinsics from the globals that were just installed.
        self.populate_realm_intrinsics();
        self
    }

    /// Populate the Realm's intrinsics table from the globals.
    ///
    /// This extracts canonical constructor/prototype references from the
    /// globals map and stores them in the Realm's intrinsics table. Called
    /// after `install_minimal_kernel_constructors` to wire up the Realm.
    fn populate_realm_intrinsics(&mut self) {
        // Helper: look up a constructor from globals
        let get_ctor = |globals: &Rc<RefCell<ObjectData>>, name: &str| -> Option<Value> {
            globals
                .borrow()
                .properties
                .get(&PropertyKey::String(name.to_string()))
                .and_then(|d| match d {
                    PropertyDescriptor::Data { value, .. } => Some(value.clone()),
                    _ => None,
                })
        };

        // Helper: extract prototype from a constructor via free-function get_property
        let get_proto = |ctor: &Value| -> Option<Value> {
            get_property(ctor, &PropertyKey::String("prototype".to_string()), 0)
                .ok()
                .filter(|v| !matches!(v, Value::Null | Value::Undefined))
        };

        let g = &self.globals;

        let obj_ctor = get_ctor(g, "Object");
        let arr_ctor = get_ctor(g, "Array");
        let fn_ctor = get_ctor(g, "Function");
        let str_ctor = get_ctor(g, "String");
        let sym_ctor = get_ctor(g, "Symbol");
        let err_ctor = get_ctor(g, "Error");
        let te_ctor = get_ctor(g, "TypeError");
        let re_ctor = get_ctor(g, "RangeError");
        let refe_ctor = get_ctor(g, "ReferenceError");
        let se_ctor = get_ctor(g, "SyntaxError");
        let prom_ctor = get_ctor(g, "Promise");
        let date_ctor = get_ctor(g, "Date");
        let map_ctor = get_ctor(g, "Map");
        let num_ctor = get_ctor(g, "Number");
        let bool_ctor = get_ctor(g, "Boolean");
        let bigint_ctor = get_ctor(g, "BigInt");

        self.realm.intrinsics.object_constructor = obj_ctor.clone();
        self.realm.intrinsics.object_prototype = obj_ctor.as_ref().and_then(get_proto);
        self.realm.intrinsics.array_constructor = arr_ctor.clone();
        self.realm.intrinsics.array_prototype = arr_ctor.as_ref().and_then(get_proto);
        self.realm.intrinsics.function_constructor = fn_ctor.clone();
        self.realm.intrinsics.function_prototype = fn_ctor.as_ref().and_then(get_proto);
        self.realm.intrinsics.string_constructor = str_ctor.clone();
        self.realm.intrinsics.string_prototype = str_ctor.as_ref().and_then(get_proto);
        self.realm.intrinsics.symbol_constructor = sym_ctor.clone();
        self.realm.intrinsics.symbol_prototype = sym_ctor.as_ref().and_then(get_proto);
        self.realm.intrinsics.error_constructor = err_ctor.clone();
        self.realm.intrinsics.error_prototype = err_ctor.as_ref().and_then(get_proto);
        self.realm.intrinsics.type_error_constructor = te_ctor;
        self.realm.intrinsics.range_error_constructor = re_ctor;
        self.realm.intrinsics.reference_error_constructor = refe_ctor;
        self.realm.intrinsics.syntax_error_constructor = se_ctor;
        self.realm.intrinsics.promise_constructor = prom_ctor.clone();
        self.realm.intrinsics.promise_prototype = prom_ctor.as_ref().and_then(get_proto);
        self.realm.intrinsics.date_constructor = date_ctor.clone();
        self.realm.intrinsics.date_prototype = date_ctor.as_ref().and_then(get_proto);
        self.realm.intrinsics.map_constructor = map_ctor.clone();
        self.realm.intrinsics.map_prototype = map_ctor.as_ref().and_then(get_proto);
        self.realm.intrinsics.number_constructor = num_ctor.clone();
        self.realm.intrinsics.number_prototype = num_ctor.as_ref().and_then(get_proto);
        self.realm.intrinsics.boolean_prototype = bool_ctor.as_ref().and_then(get_proto);
        self.realm.intrinsics.bigint_prototype = bigint_ctor.as_ref().and_then(get_proto);
    }

    /// Provide mutable access to the per-VM module loader, creating it if missing.
    ///
    /// # Panics
    ///
    /// Panics if the module loader is unexpectedly `None` (should not happen).
    pub fn module_loader_mut(&mut self) -> &mut ModuleLoader {
        if self.module_loader.is_none() {
            self.module_loader = Some(ModuleLoader::new());
        }
        // SAFETY: we just ensured module_loader is Some
        self.module_loader.get_or_insert_with(ModuleLoader::new)
    }

    /// Enter a new declarative lexical environment whose outer is the current environment.
    pub fn enter_lexical_scope(&mut self) {
        self.lexical_env = Rc::new(RefCell::new(EnvironmentRecord::new(Some(Rc::clone(
            &self.lexical_env,
        )))));
    }

    /// Leave the current lexical environment and restore its outer.
    pub fn exit_lexical_scope(&mut self) {
        let outer = self.lexical_env.borrow().outer();
        if let Some(outer) = outer {
            self.lexical_env = outer;
        }
    }

    /// Install a module's export slot map into the VM module table at the
    /// specified numeric module index. The provided `exports` map contains
    /// shared `Rc<RefCell<Value>>` slots allocated by the ModuleLoader so both
    /// VM and ModuleLoader share the same live binding slots.
    pub fn install_module_exports(
        &mut self,
        module_index: usize,
        mut exports: HashMap<String, Rc<RefCell<Value>>>,
    ) {
        let namespace = build_module_namespace_object(&exports);
        exports.insert("*".to_string(), Rc::new(RefCell::new(namespace)));
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
        self.frames.last().is_some_and(|f| f.is_strict)
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
        self.frames.clear();
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
        self.lexical_env = Rc::clone(&self.global_env);
        self.yield_star_queue = None;
        self.ip = 0;
        self.chunk = Rc::new(Chunk::new(Rc::new(String::new())));
        self.obj_generations.clear();
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
    pub fn run(&mut self, chunk: Chunk) -> Result<Value, crate::errors::RuntimeError> {
        let chunk_rc = Rc::new(chunk);
        self.root_chunk = Some(Rc::clone(&chunk_rc));
        self.chunk = chunk_rc;
        self.ip = 0;
        self.stack.clear();
        self.locals.clear();
        self.frames.clear();
        self.handlers.clear();
        self.current_slot_types = None;
        self.current_closure_env = None;
        self.lexical_env = Rc::clone(&self.global_env);
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

        loop {
            match self.step(None) {
                Ok(StepOutcome::Continue) => {
                    // `pending_exception` is only meant to swallow the
                    // immediate synthetic error returned by host throw helpers.
                    // If execution continues normally, clear it so unrelated
                    // future runtime errors are not masked.
                    self.pending_exception = false;
                }
                Ok(StepOutcome::Return(result)) => {
                    self.run_microtasks_if_needed()?;
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
                    return Err(err);
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // Opcode helpers (called from the main loop)
    // -----------------------------------------------------------------------

    fn make_iterator_result(value: Value, done: bool) -> Value {
        let obj = Rc::new(RefCell::new(ObjectData::new()));
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

    fn create_generator_object(
        &mut self,
        proto: &Rc<FunctionProto>,
        closure_env: &Rc<RefCell<Vec<Rc<RefCell<Value>>>>>,
        call_args: Vec<Value>,
        this_value: Value,
    ) -> Result<Value, crate::errors::RuntimeError> {
        let mut child = Vm::new(Rc::clone(&self.globals));
        child.push_call_frame(proto, closure_env, call_args, this_value)?;
        let runner = Rc::new(RefCell::new(GeneratorRunner {
            vm: child,
            started: false,
            completed: false,
        }));
        let next_runner = Rc::clone(&runner);
        let throw_runner = Rc::clone(&runner);
        let return_runner = Rc::clone(&runner);
        let gen_obj = Rc::new(RefCell::new(ObjectData::new()));
        gen_obj.borrow_mut().properties.insert(
            PropertyKey::String("next".to_string()),
            PropertyDescriptor::data(Value::Builtin(Rc::new(move |_vm, args| {
                let mut runner = next_runner.borrow_mut();
                if runner.completed {
                    return Ok(Self::make_iterator_result(Value::Undefined, true));
                }

                if !runner.started {
                    runner.started = true;
                } else {
                    runner
                        .vm
                        .stack
                        .push(args.first().cloned().unwrap_or(Value::Undefined));
                }

                loop {
                    match runner.vm.step(Some(1))? {
                        StepOutcome::Continue => runner.vm.run_microtasks_if_needed()?,
                        StepOutcome::Yield(v) => return Ok(Self::make_iterator_result(v, false)),
                        StepOutcome::Return(v) => {
                            runner.completed = true;
                            return Ok(Self::make_iterator_result(v, true));
                        }
                    }
                }
            }))),
        );
        gen_obj.borrow_mut().properties.insert(
            PropertyKey::String("throw".to_string()),
            PropertyDescriptor::data(Value::Builtin(Rc::new(move |_vm, args| {
                let mut runner = throw_runner.borrow_mut();
                if runner.completed {
                    return Err(crate::errors::RuntimeError::from(format!(
                        "Uncaught: {}",
                        args.first().cloned().unwrap_or(Value::Undefined)
                    )));
                }

                let thrown = args.first().cloned().unwrap_or(Value::Undefined);
                if let Err(err) = runner.vm.throw_value(thrown) {
                    runner.completed = true;
                    return Err(err);
                }

                loop {
                    match runner.vm.step(Some(1)) {
                        Ok(StepOutcome::Continue) => {
                            if let Err(e) = runner.vm.run_microtasks_if_needed() {
                                runner.completed = true;
                                return Err(e);
                            }
                        }
                        Ok(StepOutcome::Yield(v)) => {
                            return Ok(Self::make_iterator_result(v, false));
                        }
                        Ok(StepOutcome::Return(v)) => {
                            runner.completed = true;
                            return Ok(Self::make_iterator_result(v, true));
                        }
                        Err(e) => {
                            runner.completed = true;
                            return Err(e);
                        }
                    }
                }
            }))),
        );
        gen_obj.borrow_mut().properties.insert(
            PropertyKey::String("return".to_string()),
            PropertyDescriptor::data(Value::Builtin(Rc::new(move |_vm, args| {
                return_runner.borrow_mut().completed = true;
                Ok(Self::make_iterator_result(
                    args.first().cloned().unwrap_or(Value::Undefined),
                    true,
                ))
            }))),
        );
        Ok(Value::Object(gen_obj))
    }

    fn execute_call(&mut self) -> Result<(), crate::errors::RuntimeError> {
        let nargs = self.read_byte()? as usize;
        let mut args: Vec<Value> = Vec::with_capacity(nargs);
        for _ in 0..nargs {
            args.push(self.pop()?);
        }
        args.reverse();
        let callee = self.pop()?;

        match callee {
            Value::Builtin(f) => {
                let prev_this = std::mem::replace(&mut self.this_value, Value::Undefined);
                let result = (f.as_ref())(self, &args)?;
                self.this_value = prev_this;
                if self.frames.is_empty() {
                    self.run_microtasks_if_needed()?;
                }
                self.stack.push(result);
            }
            Value::BuiltinObject(f, _props) => {
                let prev_this = std::mem::replace(&mut self.this_value, Value::Undefined);
                let result = (f.as_ref())(self, &args)?;
                self.this_value = prev_this;
                if self.frames.is_empty() {
                    self.run_microtasks_if_needed()?;
                }
                self.stack.push(result);
            }
            Value::Closure { ref proto, env, .. } => {
                let call_args = self.prepare_call_args(&args, proto)?;
                // Argument validation removed in JS-first mode.
                if proto.is_generator {
                    let generator =
                        self.create_generator_object(proto, &env, call_args, Value::Undefined)?;
                    self.stack.push(generator);
                } else if proto.is_async {
                    let promise =
                        self.invoke_async_closure(proto, &env, call_args, Value::Undefined)?;
                    self.stack.push(promise);
                } else {
                    self.push_call_frame(proto, &env, call_args, Value::Undefined)?;
                }
            }
            _ => {
                if crate::trace::enabled() {
                    eprintln!("[trace] execute_call: callee not callable, dumping context...");
                    crate::trace::value_string("[trace]   callee", &callee);
                    for (i, v) in self.stack.iter().rev().take(8).enumerate() {
                        crate::trace::value_string(&format!("[trace]   stack[{}]", i), v);
                    }
                    // Try to locate the callee value inside any known constant
                    // pools referenced by the active frames to help map origins.
                    let matches = self.find_constants_containing_value(&callee);
                    if !matches.is_empty() {
                        for (fi, ci) in matches {
                            eprintln!(
                                "[trace]   callee appears in frame[{}] constants[{}]",
                                fi, ci
                            );
                        }
                    }
                }
                self.dump_diag("Call failed: callee not callable", &callee);
                return Err(
                    self.throw_type_error(&format!("{} is not a function", callee.type_name()))
                );
            }
        }

        Ok(())
    }

    fn execute_spread(&mut self) -> Result<(), crate::errors::RuntimeError> {
        self.read_byte()?; // nargs, consumed by VM dispatcher
        let mask = self.pop()?;
        let values = self.pop()?;
        let callee = self.pop()?;

        let values_vec = match values {
            Value::Array(arr, _) => arr.borrow().clone(),
            _ => Vec::new(),
        };
        let mask_vec = match mask {
            Value::Array(arr, _) => arr.borrow().clone(),
            _ => Vec::new(),
        };

        let mut flat: Vec<Value> = Vec::new();
        for (i, value) in values_vec.into_iter().enumerate() {
            let spread = mask_vec
                .get(i)
                .and_then(|v| v.as_ref())
                .is_some_and(|v| self.to_boolean(v));
            if !spread {
                if let Some(v) = value {
                    flat.push(v);
                }
                continue;
            }

            match value {
                Some(Value::Array(arr, _)) => {
                    flat.extend(arr.borrow().iter().filter_map(Clone::clone));
                }
                other => {
                    let this_for_iter = other.as_ref().unwrap_or(&Value::Undefined).clone();
                    let iterator_method = self.get_property_host(
                        &this_for_iter,
                        &PropertyKey::Symbol(SymbolId::well_known_iterator()),
                    )?;

                    let is_callable = matches!(
                        iterator_method,
                        Value::Closure { .. } | Value::Builtin(_) | Value::BuiltinObject(_, _)
                    );

                    if is_callable {
                        let iter_obj =
                            self.call_value_with_this(&iterator_method, &this_for_iter, &[])?;

                        loop {
                            let next_fn = self.get_property_host(
                                &iter_obj,
                                &PropertyKey::String("next".to_string()),
                            )?;
                            let result = self.call_value_with_this(&next_fn, &iter_obj, &[])?;
                            let done_val = self.get_property_host(
                                &result,
                                &PropertyKey::String("done".to_string()),
                            )?;
                            if self.to_boolean(&done_val) {
                                break;
                            }
                            let val = self.get_property_host(
                                &result,
                                &PropertyKey::String("value".to_string()),
                            )?;
                            flat.push(val);
                        }
                    } else {
                        let len_val = self.get_property_host(
                            other.as_ref().unwrap_or(&Value::Undefined),
                            &PropertyKey::String("length".to_string()),
                        )?;
                        let len = self.to_number_value(&len_val)?.max(0.0) as usize;
                        for idx in 0..len {
                            let item = self.get_property_host(
                                other.as_ref().unwrap_or(&Value::Undefined),
                                &PropertyKey::String(idx.to_string()),
                            )?;
                            flat.push(item);
                        }
                    }
                }
            }
        }

        match callee {
            Value::Builtin(f) => {
                let prev_this = std::mem::replace(&mut self.this_value, Value::Undefined);
                let result = (f.as_ref())(self, &flat)?;
                self.this_value = prev_this;
                if self.frames.is_empty() {
                    self.run_microtasks_if_needed()?;
                }
                self.stack.push(result);
            }
            Value::BuiltinObject(f, _props) => {
                let prev_this = std::mem::replace(&mut self.this_value, Value::Undefined);
                let result = (f.as_ref())(self, &flat)?;
                self.this_value = prev_this;
                if self.frames.is_empty() {
                    self.run_microtasks_if_needed()?;
                }
                self.stack.push(result);
            }
            Value::Closure { ref proto, env, .. } => {
                let call_args = self.prepare_call_args(&flat, proto)?;
                if proto.is_async {
                    let promise =
                        self.invoke_async_closure(proto, &env, call_args, Value::Undefined)?;
                    self.stack.push(promise);
                } else {
                    self.push_call_frame(proto, &env, call_args, Value::Undefined)?;
                }
            }
            _ => {
                self.dump_diag("Spread call failed: callee not callable", &callee);
                return Err(
                    self.throw_type_error(&format!("{} is not a function", callee.type_name()))
                );
            }
        }

        Ok(())
    }

    fn execute_call_method(&mut self) -> Result<(), crate::errors::RuntimeError> {
        let nargs = self.read_byte()? as usize;
        if crate::trace::enabled() {
            eprintln!(
                "[trace] execute_call_method nargs={} stack_size={} ip={}",
                nargs,
                self.stack.len(),
                self.ip.saturating_sub(1)
            );
            for (i, v) in self.stack.iter().rev().take(6).enumerate() {
                crate::trace::value_string(&format!("[trace]   stack[{}]", i), v);
            }
        }
        let mut args: Vec<Value> = Vec::with_capacity(nargs);
        for _ in 0..nargs {
            args.push(self.pop()?);
        }
        args.reverse();
        let callee = self.pop()?;
        let receiver = self.pop()?;
        if crate::trace::enabled() {
            eprintln!(
                "[trace] execute_call_method after pops stack_size={} receiver={:?} callee={:?}",
                self.stack.len(),
                receiver,
                callee
            );
        }
        match callee {
            Value::Builtin(f) => {
                // Builtins invoked as methods receive the receiver as the
                // first argument.
                let mut call_args: Vec<Value> = Vec::with_capacity(args.len() + 1);
                call_args.push(receiver.clone());
                call_args.extend_from_slice(&args);
                let prev_this = std::mem::replace(&mut self.this_value, receiver);
                let result = (f.as_ref())(self, &call_args)?;
                self.this_value = prev_this;
                if self.frames.is_empty() {
                    self.run_microtasks_if_needed()?;
                }
                self.stack.push(result);
            }
            Value::BuiltinObject(f, _props) => {
                let mut call_args: Vec<Value> = Vec::with_capacity(args.len() + 1);
                call_args.push(receiver.clone());
                call_args.extend_from_slice(&args);
                let prev_this = std::mem::replace(&mut self.this_value, receiver);
                let result = (f.as_ref())(self, &call_args)?;
                self.this_value = prev_this;
                if self.frames.is_empty() {
                    self.run_microtasks_if_needed()?;
                }
                self.stack.push(result);
            }
            Value::Closure { ref proto, env, .. } => {
                let call_args = self.prepare_call_args(&args, proto)?;
                // Argument validation removed in JS-first mode.
                if proto.is_async {
                    let promise = self.invoke_async_closure(proto, &env, call_args, receiver)?;
                    self.stack.push(promise);
                } else {
                    self.push_call_frame(proto, &env, call_args, receiver)?;
                }
            }
            Value::Undefined => {
                self.dump_diag("CallWithThis failed: callee not callable", &callee);
                return Err(self.throw_type_error("undefined is not a function"));
            }
            Value::Null => {
                return Err(self.throw_type_error("null is not a function"));
            }
            _ => {
                // Debug: print what value type we got and nearby stack when tracing
                if crate::trace::enabled() {
                    eprintln!(
                        "[trace] execute_call_method: callee not callable, receiver and stack follow"
                    );
                    crate::trace::value_string("[trace]   callee", &callee);
                    crate::trace::value_string("[trace]   receiver", &receiver);
                    for (i, v) in self.stack.iter().rev().take(12).enumerate() {
                        crate::trace::value_string(&format!("[trace]   stack[{}]", i), v);
                    }
                    // Attempt to locate the callee in constants for active frames
                    let matches = self.find_constants_containing_value(&callee);
                    if !matches.is_empty() {
                        for (fi, ci) in matches {
                            eprintln!(
                                "[trace]   callee appears in frame[{}] constants[{}]",
                                fi, ci
                            );
                        }
                    }
                } else {
                    eprintln!(
                        "CallWithThis ERROR: callee={:?}, receiver={:?}",
                        callee, receiver
                    );
                }
                self.dump_diag("CallWithThis failed: callee not callable", &callee);
                return Err(
                    self.throw_type_error(&format!("{} is not a function", callee.type_name()))
                );
            }
        }
        Ok(())
    }

    /// Host hook: define an own property with a descriptor.
    ///
    /// # Errors
    ///
    /// Returns a [`crate::errors::RuntimeError`] if the property definition fails.
    pub fn define_property_builtin(
        &mut self,
        args: &[Value],
    ) -> Result<Value, crate::errors::RuntimeError> {
        if args.len() < 3 {
            return Ok(Value::Undefined);
        }
        let target = args[0].clone();
        let ok = self.define_property_value(&target, &args[1], &args[2])?;
        if !ok {
            return Err(self.throw_type_error("Object.defineProperty failed"));
        }
        Ok(target)
    }

    /// Host hook: define an own property from explicit arguments.
    ///
    /// # Errors
    ///
    /// Returns a [`crate::errors::RuntimeError`] if the property definition fails.
    pub fn define_property_value(
        &mut self,
        target: &Value,
        key: &Value,
        descriptor: &Value,
    ) -> Result<bool, crate::errors::RuntimeError> {
        if !matches!(
            target,
            Value::Object(..)
                | Value::Array(..)
                | Value::Closure { .. }
                | Value::BuiltinObject(_, _)
                | Value::Promise(_)
        ) {
            return Err(self.throw_type_error("Object.defineProperty called on non-object"));
        }
        let key = self.to_property_key(key)?;
        let desc = self.read_descriptor_from_value(descriptor)?;
        let ok = self.define_own_property(target, key, desc)?;
        if ok {
            self.bump_object_generation(target);
        }
        Ok(ok)
    }

    /// Apply ECMAScript `ToObject` coercion.
    ///
    /// # Errors
    ///
    /// Returns a [`crate::errors::RuntimeError`] if the coercion fails.
    pub fn to_object_value(&mut self, v: Value) -> Result<Value, crate::errors::RuntimeError> {
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
                    PropertyKey::String("__isSymbol".to_string()),
                    PropertyDescriptor::data(Value::Bool(true)),
                );
                data.properties.insert(
                    PropertyKey::String("_id".to_string()),
                    PropertyDescriptor::data(Value::Number(id.as_u64() as f64)),
                );
                if let Some(d) = desc {
                    data.properties.insert(
                        PropertyKey::String("description".to_string()),
                        PropertyDescriptor::data(Value::String(d)),
                    );
                }
                // Try to find Symbol.prototype
                if let Some(proto) = self.realm.intrinsics.symbol_prototype.clone() {
                    data.prototype = proto;
                }
                Ok(Value::Object(Rc::new(RefCell::new(data))))
            }
            Value::Uninitialized => Err(self.throw_type_error("Uninitialized value")),
            Value::Builtin(_) => {
                // Builtins are callable but might not be full objects.
                // However, in this VM they are often treated as such or wrapped.
                Ok(v)
            }
        }
    }

    /// Host hook: invoke a callable with an explicit receiver.
    ///
    /// # Errors
    ///
    /// Returns a [`crate::errors::RuntimeError`] if the call fails.
    pub fn call_value_with_this(
        &mut self,
        callee: &Value,
        this_arg: &Value,
        args: &[Value],
    ) -> Result<Value, crate::errors::RuntimeError> {
        self.call_with_this(callee, this_arg, args)
    }

    /// Parse and evaluate a single expression source string in an isolated VM.
    ///
    /// The expression result is written to `__tsblitz_eval_result` and returned.
    ///
    /// # Errors
    ///
    /// Returns a [`crate::errors::RuntimeError`] if the evaluation fails.
    pub fn eval_expression_source(
        &mut self,
        source: &str,
    ) -> Result<Value, crate::errors::RuntimeError> {
        let wrapped = format!("__tsblitz_eval_result = ({});", source);
        let session = crate::parser::Parser::parse_source(&wrapped)
            .map_err(|e| crate::errors::RuntimeError::from(format!("parse: {}", e)))?;
        let prog = session.program();
        let chunk = crate::compiler::compile_oxc(prog, &wrapped)
            .map_err(|e| crate::errors::RuntimeError::from(format!("compile: {}", e)))?;
        let mut child = Vm::new(Rc::clone(&self.globals));
        let _ = child.run(chunk)?;
        Ok(child
            .get_global("__tsblitz_eval_result")
            .unwrap_or(Value::Undefined))
    }

    /// Host hook: construct a callable with `new` semantics.
    ///
    /// # Errors
    ///
    /// Returns a [`crate::errors::RuntimeError`] if the construction fails.
    pub fn construct_value(
        &mut self,
        callee: &Value,
        args: &[Value],
    ) -> Result<Value, crate::errors::RuntimeError> {
        if !callee.is_constructable() {
            if crate::trace::enabled() {
                eprintln!(
                    "[trace] construct_value: callee not constructable ip={} actual={}",
                    self.ip.saturating_sub(1),
                    callee.type_name()
                );
                crate::trace::value_string("[trace]   callee", callee);
                for (i, v) in self.stack.iter().rev().take(6).enumerate() {
                    crate::trace::value_string(&format!("[trace]   stack[{}]", i), v);
                }
                // Dump a small window of bytecode around the current ip to help
                // correlate the failing instruction with constants and nearby ops.
                let cur_ip = self.ip.saturating_sub(1);
                let start = cur_ip.saturating_sub(16);
                let end = (cur_ip + 16).min(self.chunk.code.len());
                eprintln!("[trace]   bytecode window ({}..{}):", start, end);
                for i in start..end {
                    eprintln!("[trace]     {:04} 0x{:02x}", i, self.chunk.code[i]);
                }
                // Print a short constant pool summary
                eprintln!("[trace]   constants (len={}):", self.chunk.constants.len());
                for (i, c) in self.chunk.constants.iter().enumerate().take(32) {
                    crate::trace::value_string(&format!("[trace]     const[{}]", i), c);
                }
            }
            return Err(self.throw_type_error(&format!(
                "Construct: not a constructor (actual={})",
                callee.type_name()
            )));
        }
        match callee {
            Value::Closure { .. } => {
                let proto = self
                    .get_property_value(callee, &PropertyKey::String("prototype".to_string()))
                    .unwrap_or(Value::Null);
                let instance_proto = match proto {
                    Value::Object(_)
                    | Value::Array(..)
                    | Value::Closure { .. }
                    | Value::BuiltinObject(_, _) => proto,
                    _ => Value::Null,
                };
                let instance = Value::Object(Rc::new(RefCell::new(ObjectData::with_prototype(
                    instance_proto,
                ))));
                let prev_new_target = self.construct_new_target.clone();
                self.construct_new_target = Some(callee.clone());
                let result = self.call_with_this(callee, &instance, args);
                self.construct_new_target = prev_new_target;
                let result = result?;
                Ok(match result {
                    Value::Object(_)
                    | Value::Array(..)
                    | Value::Closure { .. }
                    | Value::BuiltinObject(_, _)
                    | Value::Promise(_) => result,
                    _ => instance,
                })
            }
            Value::BuiltinObject(_, _) => {
                let proto = self
                    .get_property_value(callee, &PropertyKey::String("prototype".to_string()))
                    .unwrap_or(Value::Null);
                let instance_proto = match proto {
                    Value::Object(_)
                    | Value::Array(..)
                    | Value::Closure { .. }
                    | Value::BuiltinObject(_, _) => proto,
                    _ => Value::Null,
                };
                let instance = Value::Object(Rc::new(RefCell::new(ObjectData::with_prototype(
                    instance_proto,
                ))));
                // Builtin constructors receive:
                // args[0] = new instance (via call_with_this)
                // args[1..] = user-provided arguments
                // Keep [[NewTarget]] in VM state so builtins can inspect it
                // without perturbing user-visible argument positions.
                let prev_new_target = self.construct_new_target.clone();
                self.construct_new_target = Some(callee.clone());
                let result = self.call_with_this(callee, &instance, args);
                self.construct_new_target = prev_new_target;
                let result = result?;
                Ok(match result {
                    Value::Object(_)
                    | Value::Array(..)
                    | Value::Closure { .. }
                    | Value::BuiltinObject(_, _)
                    | Value::Promise(_) => result,
                    _ => instance,
                })
            }
            Value::Builtin(f) => (f.as_ref())(self, args),
            _ => Err(crate::errors::RuntimeError::from(format!(
                "Construct: not a constructor (actual={})",
                callee.type_name()
            ))),
        }
    }

    /// Host hook: read a property value.
    ///
    /// # Errors
    ///
    /// Returns a [`crate::errors::RuntimeError`] if the property read fails.
    pub fn get_property_host(
        &mut self,
        obj: &Value,
        key: &PropertyKey,
    ) -> Result<Value, crate::errors::RuntimeError> {
        self.get_property_value(obj, key)
    }

    /// Host hook: test whether a property exists on the receiver or its prototype chain.
    ///
    /// # Errors
    ///
    /// Returns a [`crate::errors::RuntimeError`] if the property lookup fails.
    pub fn has_property_host(
        &self,
        obj: &Value,
        key: &PropertyKey,
    ) -> Result<bool, crate::errors::RuntimeError> {
        Ok(self.lookup_property_descriptor(obj, key, 0)?.is_some())
    }

    /// Host hook: delete an own property.
    ///
    /// # Errors
    ///
    /// Returns a [`crate::errors::RuntimeError`] if the property deletion fails.
    pub fn delete_property_host(
        &mut self,
        obj: &Value,
        key: &PropertyKey,
    ) -> Result<bool, crate::errors::RuntimeError> {
        let deleted_res: Result<bool, crate::errors::RuntimeError> = match obj {
            Value::Object(data) => {
                let mut d = data.borrow_mut();
                match d.properties.get(key) {
                    Some(PropertyDescriptor::Data { configurable, .. })
                    | Some(PropertyDescriptor::Accessor { configurable, .. })
                        if !*configurable =>
                    {
                        Ok(false)
                    }
                    Some(_) => {
                        // Property exists and is configurable, delete it
                        d.properties.remove(key);
                        Ok(true)
                    }
                    None => {
                        // Property doesn't exist, return true
                        Ok(true)
                    }
                }
            }
            Value::Array(arr, props) => {
                if let PropertyKey::String(s) = key
                    && let Ok(i) = s.parse::<usize>()
                    && i < arr.borrow().len()
                {
                    arr.borrow_mut()[i] = None;
                    return Ok(true);
                }
                let mut p = props.borrow_mut();
                match p.properties.get(key) {
                    Some(PropertyDescriptor::Data { configurable, .. })
                    | Some(PropertyDescriptor::Accessor { configurable, .. })
                        if !*configurable =>
                    {
                        Ok(false)
                    }
                    Some(_) => {
                        // Property exists and is configurable, delete it
                        p.properties.remove(key);
                        Ok(true)
                    }
                    None => {
                        // Property doesn't exist, return true
                        Ok(true)
                    }
                }
            }
            Value::Closure { props, .. } | Value::BuiltinObject(_, props) => {
                let mut p = props.borrow_mut();
                match p.properties.get(key) {
                    Some(PropertyDescriptor::Data { configurable, .. })
                    | Some(PropertyDescriptor::Accessor { configurable, .. })
                        if !*configurable =>
                    {
                        Ok(false)
                    }
                    Some(_) => {
                        // Property exists and is configurable, delete it
                        p.properties.remove(key);
                        Ok(true)
                    }
                    None => {
                        // Property doesn't exist, return true
                        Ok(true)
                    }
                }
            }
            _ => Ok(false),
        };
        let deleted = deleted_res?;
        if deleted {
            self.bump_object_generation(obj);
        }
        Ok(deleted)
    }

    /// Host hook: set a property value on a target object-like value.
    ///
    /// # Errors
    ///
    /// Returns a [`crate::errors::RuntimeError`] if the property setting fails.
    pub fn set_property_host(
        &mut self,
        obj: &Value,
        key: &Value,
        value: Value,
    ) -> Result<Value, crate::errors::RuntimeError> {
        let prop = self.to_property_key(key)?;
        let result = value.clone();
        self.set_property_tracked(obj, &prop, value)?;
        Ok(result)
    }

    fn call_with_this(
        &mut self,
        callee: &Value,
        this_arg: &Value,
        args: &[Value],
    ) -> Result<Value, crate::errors::RuntimeError> {
        match callee {
            Value::Builtin(f) => {
                // For builtins invoked with an explicit `this`, pass the receiver
                // as the first argument so native functions that expect the
                // receiver can access it (used by accessor getters/setters).
                let mut call_args: Vec<Value> = Vec::with_capacity(args.len() + 1);
                call_args.push(this_arg.clone());
                call_args.extend_from_slice(args);
                let prev_this = std::mem::replace(&mut self.this_value, this_arg.clone());
                let result = (f.as_ref())(self, &call_args)?;
                self.this_value = prev_this;
                self.run_microtasks_if_needed()?;
                Ok(result)
            }
            Value::BuiltinObject(f, _props) => {
                let mut call_args: Vec<Value> = Vec::with_capacity(args.len() + 1);
                call_args.push(this_arg.clone());
                call_args.extend_from_slice(args);
                let prev_this = std::mem::replace(&mut self.this_value, this_arg.clone());
                let result = (f.as_ref())(self, &call_args)?;
                self.this_value = prev_this;
                self.run_microtasks_if_needed()?;
                Ok(result)
            }
            Value::Closure { proto, .. } => {
                let mut call_args: Vec<Value> = Vec::with_capacity(args.len());
                call_args.extend_from_slice(args);
                let call_args = self.prepare_call_args(&call_args, proto)?;
                self.validate_call_args(&call_args, proto)?;
                let closure_env = match callee {
                    Value::Closure { env, .. } => Rc::clone(env),
                    _ => unreachable!(),
                };
                // If the closure is a generator, create a generator object instead
                // of pushing a new frame / invoking immediately.
                if proto.is_generator {
                    let generator = self.create_generator_object(
                        proto,
                        &closure_env,
                        call_args,
                        this_arg.clone(),
                    )?;
                    return Ok(generator);
                }
                if proto.is_async {
                    return self.invoke_async_closure(
                        proto,
                        &closure_env,
                        call_args,
                        this_arg.clone(),
                    );
                }
                let stack_base = self.stack.len();
                let saved_stack = self.stack[..stack_base].to_vec();
                if crate::trace::enabled() {
                    eprintln!(
                        "[trace] call_with_this closure stack_base={} stack_len={} proto='{}'",
                        stack_base,
                        self.stack.len(),
                        proto.name
                    );
                }
                let caller_chunk = Rc::clone(&self.chunk);
                let caller_ip = self.ip;
                let caller_locals = self.locals.clone();
                let caller_this = self.this_value.clone();
                let caller_env = self.current_closure_env.clone();
                let caller_slot_types = self.current_slot_types.clone();
                let caller_frame_len = self.frames.len();
                let caller_handler_len = self.handlers.len();
                self.push_call_frame(proto, &closure_env, call_args, this_arg.clone())?;
                let result = self.run_until_return(stack_base, true);
                // Restore caller state (run_until_return already truncated stack)
                self.chunk = caller_chunk;
                self.ip = caller_ip;
                self.locals = caller_locals;
                self.this_value = caller_this;
                self.current_closure_env = caller_env;
                self.current_slot_types = caller_slot_types;
                self.stack = saved_stack;
                self.frames.truncate(caller_frame_len);
                self.handlers.truncate(caller_handler_len);
                result
            }
            _ => {
                if crate::trace::enabled() {
                    eprintln!("[trace] call_with_this: callee not callable (final fallback)");
                    crate::trace::value_string("[trace]   callee", callee);
                    crate::trace::value_string("[trace]   this_arg", this_arg);
                    for (i, v) in self.stack.iter().rev().take(8).enumerate() {
                        crate::trace::value_string(&format!("[trace]   stack[{}]", i), v);
                    }
                }
                self.dump_diag("CallWithThis failed: callee not callable", callee);
                Err(crate::errors::RuntimeError::from(
                    "CallWithThis: not a function",
                ))
            }
        }
    }

    pub(super) fn run_until_return(
        &mut self,
        stack_base: usize,
        drain_microtasks: bool,
    ) -> Result<Value, crate::errors::RuntimeError> {
        let target_frame_depth = self.frames.len();
        loop {
            match self.step(Some(target_frame_depth)) {
                Ok(StepOutcome::Continue) => {
                    self.pending_exception = false;
                }
                Ok(StepOutcome::Return(result)) => {
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
    fn step(
        &mut self,
        stop_at_frame_depth: Option<usize>,
    ) -> Result<StepOutcome, crate::errors::RuntimeError> {
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
                return Err(crate::errors::RuntimeError::from(format!(
                    "Invalid opcode 0x{:02x} at ip {}",
                    op_byte,
                    self.ip.saturating_sub(1)
                )));
            }
        };

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
                let v = self
                    .locals
                    .get(slot)
                    .map(|cell| cell.borrow().clone())
                    .ok_or("Bad local slot")?;
                if matches!(v, Value::Uninitialized) {
                    return Err(self.throw_reference_error(
                        "Cannot access lexical binding before initialization",
                    ));
                }
                self.stack.push(v);
            }
            Op::SetLocal => {
                let slot = self.read_byte()? as usize;
                let v = self.pop()?;
                ensure_slot(&mut self.locals, slot);
                *self.locals[slot].borrow_mut() = v;
            }
            Op::CloneLocalCell => {
                let slot = self.read_byte()? as usize;
                ensure_slot(&mut self.locals, slot);
                let value = self.locals[slot].borrow().clone();
                self.locals[slot] = Rc::new(RefCell::new(value));
            }
            Op::GetGlobal => {
                let idx = self.read_short()? as usize;
                let name = self.string_constant(idx)?;
                let v = self
                    .globals
                    .borrow()
                    .properties
                    .get(&PropertyKey::String(name.clone()))
                    .and_then(|d| match d {
                        PropertyDescriptor::Data { value, .. } => Some(value.clone()),
                        _ => None,
                    });
                let v = match v {
                    Some(v) => v,
                    None => {
                        return Err(self.throw_reference_error(&format!("{name} is not defined")));
                    }
                };
                if crate::trace::enabled() {
                    eprintln!("[trace] GetGlobal '{}' -> {}", name, v.type_name());
                    crate::trace::value_string("[trace]   value", &v);
                }
                self.stack.push(v);
            }
            Op::SetGlobal => {
                let idx = self.read_short()? as usize;
                let name = self.string_constant(idx)?;
                let v = self.pop()?;
                self.globals
                    .borrow_mut()
                    .properties
                    .insert(PropertyKey::String(name), PropertyDescriptor::data(v));
            }
            // New op: GetThis
            Op::GetThis => {
                self.stack.push(self.this_value.clone());
            }
            Op::GetUpvalue => {
                let idx = self.read_byte()? as usize;
                let env = self
                    .current_closure_env
                    .as_ref()
                    .ok_or("GetUpvalue without closure env")?;
                let value = env
                    .borrow()
                    .get(idx)
                    .map(|cell| cell.borrow().clone())
                    .ok_or("Bad upvalue index")?;
                self.stack.push(value);
            }
            Op::SetUpvalue => {
                let idx = self.read_byte()? as usize;
                let value = self.pop()?;
                let env = self
                    .current_closure_env
                    .as_ref()
                    .ok_or("SetUpvalue without closure env")?;
                let cell = env.borrow().get(idx).cloned().ok_or("Bad upvalue index")?;
                *cell.borrow_mut() = value;
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
                        let thrown = self
                            .construct_value(&ctor, &[])
                            .unwrap_or(Value::Object(Rc::new(RefCell::new(ObjectData::new()))));
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
                            let thrown = self
                                .construct_value(&ctor, &[])
                                .unwrap_or(Value::Object(Rc::new(RefCell::new(ObjectData::new()))));
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
            Op::Mod => self.binary_num_bigint(|a, b| a % b, |a, b| Ok(a % b))?,
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

                let to_numeric = |vm: &mut Vm,
                                  v: Value|
                 -> Result<Numeric, crate::errors::RuntimeError> {
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
                        let thrown = self
                            .construct_value(&ctor, &[])
                            .unwrap_or(Value::Object(Rc::new(RefCell::new(ObjectData::new()))));
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
                        let thrown = self
                            .construct_value(&ctor, &[])
                            .unwrap_or(Value::Object(Rc::new(RefCell::new(ObjectData::new()))));
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
                            let thrown = self
                                .construct_value(&ctor, &[])
                                .unwrap_or(Value::Object(Rc::new(RefCell::new(ObjectData::new()))));
                            match self.unwind_to_handler(thrown) {
                                Ok(()) => return Ok(StepOutcome::Continue),
                                Err(err) => return Err(err),
                            }
                        }
                        let exp_u32 = exp.to_u32().ok_or_else(|| {
                            crate::errors::RuntimeError::from("Pow: bigint exponent too large")
                        })?;
                        self.stack.push(Value::BigInt(x.pow(exp_u32)));
                    }
                    (Numeric::Number(x), Numeric::Number(y)) => {
                        let r = x.powf(y);
                        self.stack.push(Value::Number(r));
                    }
                    // Mixing BigInt and Number -> throw TypeError as JS semantics
                    _ => {
                        let ctor = self.get_global("TypeError").unwrap_or(Value::Undefined);
                        let thrown = self
                            .construct_value(&ctor, &[])
                            .unwrap_or(Value::Object(Rc::new(RefCell::new(ObjectData::new()))));
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
            Op::Call => self.execute_call()?,
            Op::CallMethod => self.execute_call_method()?,
            Op::Spread => self.execute_spread()?,
            Op::Return => {
                if self.frames.is_empty() {
                    let result = self.stack.pop().unwrap_or(Value::Undefined);
                    return Ok(StepOutcome::Return(result));
                }
                self.execute_return()?;
                // Only stop if we've returned PAST the target frame depth
                // (i.e., we've returned from the function we were waiting for)
                if let Some(target_depth) = stop_at_frame_depth
                    && self.frames.len() < target_depth
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
            Op::BuildArray => self.build_array()?,
            Op::BuildObject => self.build_object()?,
            Op::GetProperty => {
                let key = self.pop()?;
                let obj = self.pop()?;
                let k = self.to_property_key(&key)?;
                let v = self.get_property_value(&obj, &k)?;
                if crate::trace::enabled() {
                    if let PropertyKey::String(s) = k.clone() {
                        eprintln!(
                            "[trace] GetProperty receiver={} key='{}' -> {}",
                            obj.type_name(),
                            s,
                            v.type_name()
                        );
                    } else {
                        eprintln!(
                            "[trace] GetProperty receiver={} key=symbol -> {}",
                            obj.type_name(),
                            v.type_name()
                        );
                    }
                    crate::trace::value_string("[trace]   result", &v);
                }
                self.stack.push(v);
            }
            Op::GetPropertyIC => self.get_property_ic()?,
            Op::SetProperty => {
                if crate::trace::enabled() {
                    eprintln!(
                        "[trace] SetProperty stack_size={} ip={}",
                        self.stack.len(),
                        self.ip.saturating_sub(1)
                    );
                    for (i, v) in self.stack.iter().rev().take(6).enumerate() {
                        crate::trace::value_string(&format!("[trace]   stack[{}]", i), v);
                    }
                }
                let value = self.pop()?;
                let key = self.pop()?;
                let obj = self.pop()?;
                let k = self.to_property_key(&key)?;
                self.set_property_tracked(&obj, &k, value.clone())?;
                self.stack.push(value);
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
                        let new_slot = Rc::new(RefCell::new(v));
                        m.insert(name, new_slot);
                    }
                } else {
                    while self.module_table.len() <= module_idx {
                        self.module_table.push(HashMap::new());
                    }
                    let new_slot = Rc::new(RefCell::new(v));
                    self.module_table[module_idx].insert(name, new_slot);
                }
            }
            Op::MakeClosure => self.make_closure()?,
            Op::GetBuiltin => {
                let idx = self.read_byte()? as usize;
                let name = self.string_constant(idx)?;
                let g = self.globals.borrow();
                let v = g
                    .properties
                    .get(&PropertyKey::String(name.clone()))
                    .and_then(|d| match d {
                        PropertyDescriptor::Data { value, .. } => Some(value.clone()),
                        _ => None,
                    })
                    .ok_or_else(|| {
                        crate::errors::RuntimeError::from(format!(
                            "GetBuiltin: '{}' not found",
                            name
                        ))
                    })?;
                match v {
                    Value::Builtin(_) | Value::BuiltinObject(_, _) => self.stack.push(v),
                    _ => {
                        return Err(crate::errors::RuntimeError::from(format!(
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
                    chunk: Rc::clone(&self.chunk),
                    handler_ip,
                    saved_stack_len: self.stack.len(),
                    saved_locals_len: self.locals.len(),
                    frame_count: self.frames.len(),
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
                        for _ in 0..1024 {
                            match &*state.borrow() {
                                PromiseState::Fulfilled(v) => {
                                    self.stack.push(v.clone());
                                    settled = true;
                                    break;
                                }
                                PromiseState::Rejected(reason) => {
                                    self.throw_value(reason.clone())?;
                                    settled = true;
                                    break;
                                }
                                PromiseState::Pending { .. } => {
                                    if self.microtasks.is_empty() {
                                        break;
                                    }
                                    self.run_microtasks_if_needed()?;
                                }
                            }
                        }
                        if !settled {
                            self.stack.push(Value::Undefined);
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
    pub fn to_primitive(
        &mut self,
        v: &Value,
        hint: Option<&str>,
    ) -> Result<Value, crate::errors::RuntimeError> {
        match v {
            Value::Object(_)
            | Value::Array(..)
            | Value::Closure { .. }
            | Value::Builtin(_)
            | Value::BuiltinObject(_, _) => {
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
                if let Ok(method) = get_property(v, &to_prim_key, 0)
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
                    ) {
                        if crate::trace::enabled() {
                            crate::trace::value_string(
                                "[trace] @@toPrimitive accepted primitive",
                                &rv,
                            );
                        }
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
                    if let Ok(method) = get_property(v, &key, 0)
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
                        if let Ok(rv) = self.call_with_this(&method, v, &[]) {
                            if crate::trace::enabled() {
                                crate::trace::value_string(
                                    &format!("[trace] {} returned", name),
                                    &rv,
                                );
                                eprintln!("[trace] {} return type = {}", name, rv.type_name());
                            }
                            if !matches!(
                                rv,
                                Value::Object(_)
                                    | Value::Array(..)
                                    | Value::Closure { .. }
                                    | Value::Builtin(_)
                                    | Value::BuiltinObject(_, _)
                            ) {
                                return Ok(rv);
                            }
                        }
                    }
                }
                // Pragmatic fallback for object formatting paths used heavily
                // by the harness: if custom coercion hooks fail to produce a
                // primitive, fall back to an Object-tag string instead of
                // throwing.
                let fallback = Value::String(format!("[object {}]", v.object_tag()));
                if crate::trace::enabled() {
                    eprintln!("[trace] to_primitive fallback -> {:?}", fallback);
                }
                Ok(fallback)
            }
            _ => Ok(v.clone()),
        }
    }

    /// Apply ECMAScript `ToString` coercion.
    ///
    /// # Errors
    ///
    /// Returns a [`crate::errors::RuntimeError`] if the coercion fails.
    pub fn to_string(&mut self, v: &Value) -> Result<String, crate::errors::RuntimeError> {
        match v {
            Value::String(s) => Ok(s.clone()),
            Value::Number(n) => Ok(n.to_string()),
            Value::Bool(b) => Ok(if *b {
                "true".to_string()
            } else {
                "false".to_string()
            }),
            Value::Undefined => Ok("undefined".to_string()),
            Value::Null => Ok("null".to_string()),
            Value::BigInt(n) => Ok(n.to_string()),
            Value::Symbol(_, desc) => Ok(match desc {
                Some(d) => format!("Symbol({})", d),
                None => "Symbol()".to_string(),
            }),
            _ => {
                let prim = self.to_primitive(v, Some("string"))?;
                self.to_string(&prim)
            }
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
    ) -> Result<String, crate::errors::RuntimeError> {
        if !Self::is_callable_value(value) {
            return Err(self.throw_type_error("Function.prototype.toString called on non-callable"));
        }
        if let Value::Closure { proto, .. } = value
            && let Some(src) = proto.source_text()
        {
            return Ok(src);
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
    pub fn to_number_value(&mut self, v: &Value) -> Result<f64, crate::errors::RuntimeError> {
        match v {
            Value::Number(n) => Ok(*n),
            Value::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
            Value::String(s) => Ok(parse_js_number_string(s)),
            Value::Undefined => Ok(f64::NAN),
            Value::Null => Ok(0.0),
            Value::BigInt(n) => Ok(n.to_f64().unwrap_or_else(|| {
                if n.is_negative() {
                    f64::NEG_INFINITY
                } else {
                    f64::INFINITY
                }
            })),
            Value::Symbol(_, _) => Err(crate::errors::RuntimeError::from(
                "Cannot convert Symbol to number",
            )),
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
    ) -> Result<PropertyKey, crate::errors::RuntimeError> {
        match v {
            Value::Symbol(sym, _) => Ok(PropertyKey::Symbol(sym.clone())),
            Value::Object(data) => {
                let d = data.borrow();
                if let Some(PropertyDescriptor::Data { value, .. }) = d
                    .properties
                    .get(&PropertyKey::String("__isSymbol".to_string()))
                    && matches!(value, Value::Bool(true))
                    && let Some(PropertyDescriptor::Data { value, .. }) =
                        d.properties.get(&PropertyKey::String("_id".to_string()))
                    && let Value::Number(n) = value
                {
                    return Ok(PropertyKey::Symbol(SymbolId::well_known(*n as u64)));
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
    pub fn abstract_equality(
        &mut self,
        x: &Value,
        y: &Value,
    ) -> Result<bool, crate::errors::RuntimeError> {
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
    fn read_descriptor_from_value(
        &mut self,
        v: &Value,
    ) -> Result<PartialDescriptor, crate::errors::RuntimeError> {
        let mut desc = PartialDescriptor::default();
        let data = match v {
            Value::Object(obj) => obj.borrow(),
            _ => {
                return Err(self.throw_type_error("Property description must be an object"));
            }
        };
        let get_field = |field: &str| {
            data.properties
                .get(&PropertyKey::String(field.to_string()))
                .and_then(|desc| match desc {
                    PropertyDescriptor::Data { value, .. } => Some(value.clone()),
                    _ => None,
                })
        };

        if let Some(value) = get_field("value") {
            desc.value = Some(value);
            desc.has_value = true;
        }
        if let Some(value) = get_field("writable") {
            desc.writable = Some(self.to_boolean(&value));
            desc.has_writable = true;
        }
        if let Some(value) = get_field("enumerable") {
            desc.enumerable = Some(self.to_boolean(&value));
            desc.has_enumerable = true;
        }
        if let Some(value) = get_field("configurable") {
            desc.configurable = Some(self.to_boolean(&value));
            desc.has_configurable = true;
        }
        if let Some(getter) = get_field("get") {
            if !matches!(getter, Value::Undefined) && !Self::is_callable_value(&getter) {
                return Err(self.throw_type_error("Getter must be callable"));
            }
            desc.get = Some(getter);
            desc.has_get = true;
        }
        if let Some(setter) = get_field("set") {
            if !matches!(setter, Value::Undefined) && !Self::is_callable_value(&setter) {
                return Err(self.throw_type_error("Setter must be callable"));
            }
            desc.set = Some(setter);
            desc.has_set = true;
        }

        let has_data = desc.has_value || desc.has_writable;
        let has_accessor = desc.has_get || desc.has_set;
        if has_data && has_accessor {
            return Err(self.throw_type_error(
                "Invalid property descriptor: cannot specify both data and accessor fields",
            ));
        }

        Ok(desc)
    }

    /// Define a property on an object, replacing any existing descriptor.
    ///
    /// # Errors
    ///
    /// Returns a [`crate::errors::RuntimeError`] if the operation fails.
    fn define_own_property(
        &mut self,
        obj: &Value,
        key: PropertyKey,
        desc: PartialDescriptor,
    ) -> Result<bool, crate::errors::RuntimeError> {
        let target = match obj {
            Value::Object(data) => data,
            Value::Array(_, props) => props,
            Value::Closure { props, .. } => props,
            Value::BuiltinObject(_, props) => props,
            _ => return Ok(false),
        };
        let mut data = target.borrow_mut();
        let current = data.properties.get(&key).cloned();

        if current.is_none() && !data.extensible {
            return Ok(false);
        }

        if let Some(ref cur) = current {
            // Non-configurable restrictions.
            let cur_configurable = match &cur {
                PropertyDescriptor::Data { configurable, .. } => *configurable,
                PropertyDescriptor::Accessor { configurable, .. } => *configurable,
            };
            if !cur_configurable {
                if let Some(true) = desc.configurable {
                    return Ok(false);
                }
                if let Some(en) = desc.enumerable {
                    let cur_en = match &cur {
                        PropertyDescriptor::Data { enumerable, .. } => *enumerable,
                        PropertyDescriptor::Accessor { enumerable, .. } => *enumerable,
                    };
                    if en != cur_en {
                        return Ok(false);
                    }
                }
                if let PropertyDescriptor::Data {
                    writable, value, ..
                } = &cur
                    && !*writable
                {
                    if let Some(true) = desc.writable {
                        return Ok(false);
                    }
                    if let Some(new_val) = &desc.value
                        && !values_equal(new_val, value)
                    {
                        return Ok(false);
                    }
                }
            }
        }

        let is_accessor = desc.get.is_some() || desc.set.is_some();
        let new_desc = if is_accessor {
            PropertyDescriptor::Accessor {
                get: desc.get,
                set: desc.set,
                enumerable: desc.enumerable.unwrap_or_else(|| {
                    current.as_ref().is_some_and(|d| match d {
                        PropertyDescriptor::Data { enumerable, .. } => *enumerable,
                        PropertyDescriptor::Accessor { enumerable, .. } => *enumerable,
                    })
                }),
                configurable: desc.configurable.unwrap_or_else(|| {
                    current.as_ref().is_some_and(|d| match d {
                        PropertyDescriptor::Data { configurable, .. } => *configurable,
                        PropertyDescriptor::Accessor { configurable, .. } => *configurable,
                    })
                }),
            }
        } else {
            let (cur_value, cur_writable, cur_enumerable, cur_configurable) = match &current {
                Some(PropertyDescriptor::Data {
                    value,
                    writable,
                    enumerable,
                    configurable,
                }) => (value.clone(), *writable, *enumerable, *configurable),
                Some(PropertyDescriptor::Accessor {
                    enumerable,
                    configurable,
                    ..
                }) => (Value::Undefined, false, *enumerable, *configurable),
                None => (Value::Undefined, false, false, false),
            };
            PropertyDescriptor::Data {
                value: desc.value.unwrap_or(cur_value),
                writable: desc.writable.unwrap_or(cur_writable),
                enumerable: desc.enumerable.unwrap_or(cur_enumerable),
                configurable: desc.configurable.unwrap_or(cur_configurable),
            }
        };

        data.properties.insert(key, new_desc);
        Ok(true)
    }

    /// Get the value of a property on an object, following the prototype chain.
    fn get_property_value(
        &mut self,
        obj: &Value,
        key: &PropertyKey,
    ) -> Result<Value, crate::errors::RuntimeError> {
        if matches!(obj, Value::Undefined | Value::Null) {
            return Err(self.throw_type_error(&format!(
                "Cannot read property '{:?}' of {}",
                key,
                obj.type_name()
            )));
        }
        if let Some((desc, _holder)) = self.lookup_property_descriptor(obj, key, 0)? {
            return match desc {
                PropertyDescriptor::Data { value, .. } => Ok(value),
                PropertyDescriptor::Accessor { get, .. } => {
                    if let Some(getter) = get {
                        self.call_with_this(&getter, obj, &[])
                    } else {
                        Ok(Value::Undefined)
                    }
                }
            };
        }
        if let Value::String(text) = obj {
            if matches!(key, PropertyKey::String(name) if name == "length") {
                return Ok(Value::Number(text.chars().count() as f64));
            }
            if let Some(i) = array_index_from_key(key)
                && let Some(ch) = text.chars().nth(i)
            {
                return Ok(Value::String(ch.to_string()));
            }
            let string_ctor = self
                .globals
                .borrow()
                .properties
                .get(&PropertyKey::String("String".to_string()))
                .and_then(|d| match d {
                    PropertyDescriptor::Data { value, .. } => Some(value.clone()),
                    _ => None,
                });
            if let Some(string_ctor) = string_ctor
                && let Ok(proto) = self
                    .get_property_value(&string_ctor, &PropertyKey::String("prototype".to_string()))
                && !matches!(proto, Value::Null)
                && let Ok(v) = get_property(&proto, key, 0)
            {
                return Ok(v);
            }
        }
        if let Value::Array(_, _) = obj {
            let array_ctor = self
                .globals
                .borrow()
                .properties
                .get(&PropertyKey::String("Array".to_string()))
                .and_then(|d| match d {
                    PropertyDescriptor::Data { value, .. } => Some(value.clone()),
                    _ => None,
                });
            if let Some(array_ctor) = array_ctor
                && let Ok(proto) = self
                    .get_property_value(&array_ctor, &PropertyKey::String("prototype".to_string()))
                && !matches!(proto, Value::Null)
                && let Ok(v) = get_property(&proto, key, 0)
            {
                return Ok(v);
            }
        }
        if let Value::Symbol(_, desc) = obj {
            if matches!(key, PropertyKey::String(name) if name == "description") {
                return Ok(match desc {
                    Some(d) => Value::String(d.clone()),
                    None => Value::Undefined,
                });
            }
            let symbol_ctor = self
                .globals
                .borrow()
                .properties
                .get(&PropertyKey::String("Symbol".to_string()))
                .and_then(|d| match d {
                    PropertyDescriptor::Data { value, .. } => Some(value.clone()),
                    _ => None,
                });
            if let Some(symbol_ctor) = symbol_ctor
                && let Ok(proto) = self
                    .get_property_value(&symbol_ctor, &PropertyKey::String("prototype".to_string()))
                && !matches!(proto, Value::Null)
                && let Ok(v) = get_property(&proto, key, 0)
            {
                return Ok(v);
            }
        }
        Ok(Value::Undefined)
    }

    /// Lookup a property descriptor on an object, following the prototype chain.
    fn lookup_property_descriptor(
        &self,
        obj: &Value,
        key: &PropertyKey,
        depth: usize,
    ) -> Result<Option<(PropertyDescriptor, Value)>, crate::errors::RuntimeError> {
        if depth > MAX_PROTOTYPE_DEPTH {
            return Err(crate::errors::RuntimeError::from(
                "Prototype chain cycle detected or too deep",
            ));
        }
        match obj {
            Value::Object(data) => {
                let d = data.borrow();
                if let Some(desc) = d.properties.get(key) {
                    return Ok(Some((desc.clone(), obj.clone())));
                }
                if !matches!(d.prototype, Value::Null) {
                    return self.lookup_property_descriptor(&d.prototype, key, depth + 1);
                }
            }
            Value::Array(arr, props) => {
                if let Some(i) = array_index_from_key(key)
                    && let Some(opt_v) = arr.borrow().get(i).cloned()
                    && let Some(v) = opt_v
                {
                    return Ok(Some((
                        PropertyDescriptor::Data {
                            value: v,
                            writable: true,
                            enumerable: true,
                            configurable: true,
                        },
                        obj.clone(),
                    )));
                }
                let p = props.borrow();
                if let Some(desc) = p.properties.get(key) {
                    return Ok(Some((desc.clone(), obj.clone())));
                }
                if !matches!(p.prototype, Value::Null) {
                    return self.lookup_property_descriptor(&p.prototype, key, depth + 1);
                }
            }
            Value::Closure { props, .. } => {
                let p = props.borrow();
                if let Some(desc) = p.properties.get(key) {
                    return Ok(Some((desc.clone(), obj.clone())));
                }
                if !matches!(p.prototype, Value::Null) {
                    return self.lookup_property_descriptor(&p.prototype, key, depth + 1);
                }
            }
            Value::BuiltinObject(_, props) => {
                let p = props.borrow();
                if let Some(desc) = p.properties.get(key) {
                    return Ok(Some((desc.clone(), obj.clone())));
                }
                if !matches!(p.prototype, Value::Null) {
                    return self.lookup_property_descriptor(&p.prototype, key, depth + 1);
                }
            }
            Value::Promise(_) => {
                if let Some(proto) = self.realm.intrinsics.promise_prototype.clone() {
                    return self.lookup_property_descriptor(&proto, key, depth + 1);
                }
            }
            Value::Bool(_) => {
                if let Some(proto) = self.realm.intrinsics.boolean_prototype.clone() {
                    return self.lookup_property_descriptor(&proto, key, depth + 1);
                }
            }
            Value::Number(_) => {
                if let Some(proto) = self.realm.intrinsics.number_prototype.clone() {
                    return self.lookup_property_descriptor(&proto, key, depth + 1);
                }
            }
            Value::BigInt(_) => {
                if let Some(proto) = self.realm.intrinsics.bigint_prototype.clone() {
                    return self.lookup_property_descriptor(&proto, key, depth + 1);
                }
            }
            _ => {}
        }
        Ok(None)
    }

    /// Pop the return value, restore the caller's frame, and push the result.
    fn execute_return(&mut self) -> Result<(), crate::errors::RuntimeError> {
        let frame = self.frames.pop().ok_or("Return without call frame")?;
        let result = self.stack.pop().unwrap_or(Value::Undefined);

        if crate::trace::enabled() {
            eprintln!(
                "[trace] execute_return stack_before_truncate={} frame.stack_base={} result={:?}",
                self.stack.len(),
                frame.stack_base,
                result
            );
            for (i, v) in self.stack.iter().rev().take(6).enumerate() {
                crate::trace::value_string(&format!("[trace]   ret_stack[{}]", i), v);
            }
        }

        // Runtime return-type check intentionally disabled in JS-first mode.
        // TypeScript return annotations are erased at runtime.

        // Restore caller state.
        self.current_slot_types = frame.slot_types;
        self.chunk = frame.chunk;
        self.ip = frame.ip;
        self.stack = frame.stack;
        self.locals = frame.locals;
        self.this_value = frame.this_value;
        self.current_closure_env = frame.closure_env;
        self.lexical_env = frame.lexical_env;
        self.yield_star_queue = frame.yield_star_queue;
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

    fn prepare_call_args(
        &self,
        args: &[Value],
        proto: &FunctionProto,
    ) -> Result<Vec<Value>, crate::errors::RuntimeError> {
        if !proto.has_rest {
            return Ok(args.to_vec());
        }

        let ar = proto.arity;
        let fixed_count = ar.saturating_sub(1);
        let mut fixed = Vec::with_capacity(ar);

        for i in 0..fixed_count {
            fixed.push(args.get(i).cloned().unwrap_or(Value::Undefined));
        }

        let rest_slice: Vec<Option<Value>> = if args.len() > fixed_count {
            args[fixed_count..]
                .iter()
                .map(|v| Some(v.clone()))
                .collect()
        } else {
            Vec::new()
        };

        let arr_rc = Rc::new(RefCell::new(rest_slice));
        let props: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(ObjectData::new()));
        props.borrow_mut().properties.insert(
            PropertyKey::String("length".to_string()),
            PropertyDescriptor::data(Value::Number(arr_rc.borrow().len() as f64)),
        );

        if let Some(array_val) = self
            .globals
            .borrow()
            .properties
            .get(&PropertyKey::String("Array".to_string()))
            .and_then(|d| match d {
                PropertyDescriptor::Data { value, .. } => Some(value.clone()),
                _ => None,
            })
            && let Ok(proto_val) =
                get_property(&array_val, &PropertyKey::String("prototype".to_string()), 0)
            && !matches!(proto_val, Value::Null)
        {
            props.borrow_mut().prototype = proto_val;
            let proto_for_lookup = props.borrow().prototype.clone();
            if let Ok(for_each_val) = get_property(
                &proto_for_lookup,
                &PropertyKey::String("forEach".to_string()),
                0,
            ) && !matches!(for_each_val, Value::Undefined)
            {
                props.borrow_mut().properties.insert(
                    PropertyKey::String("forEach".to_string()),
                    PropertyDescriptor::data(for_each_val),
                );
            }
        }

        fixed.push(Value::Array(arr_rc, props));
        if crate::trace::enabled() {
            // Log allocation site for rest-array used as arguments so we can
            // correlate a later callee pointing at the same props pointer.
            if let Value::Array(_arr, props_rc) = fixed.last().cloned().unwrap() {
                eprintln!(
                    "[trace] prepare_call_args: created rest-Array props_ptr={:p} proto='{}'",
                    Rc::as_ptr(&props_rc),
                    proto.name
                );
            }
        }
        Ok(fixed)
    }

    fn validate_call_args(
        &self,
        _call_args: &[Value],
        _proto: &FunctionProto,
    ) -> Result<(), crate::errors::RuntimeError> {
        // Argument type validation is intentionally disabled in JS-first mode.
        // TypeScript annotations are treated as comments and should not affect
        // runtime behaviour. Keep this function for tooling and future opt-in
        // checked modes.
        Ok(())
    }

    /// Save the caller's state and set up the callee.
    pub(super) fn push_call_frame(
        &mut self,
        proto: &Rc<FunctionProto>,
        closure_env: &Rc<RefCell<Vec<Rc<RefCell<Value>>>>>,
        call_args: Vec<Value>,
        mut this_value: Value,
    ) -> Result<(), crate::errors::RuntimeError> {
        if !proto.is_strict {
            if matches!(this_value, Value::Undefined | Value::Null) {
                this_value = Value::Object(Rc::clone(&self.globals));
            } else {
                this_value = self.to_object_value(this_value)?;
            }
        }

        let caller_stack = std::mem::take(&mut self.stack);
        let stack_base = caller_stack.len();

        if crate::trace::enabled() {
            eprintln!(
                "[trace] push_call_frame stack_base={} proto='{}' arity={} stack_len={}",
                stack_base,
                proto.name,
                proto.arity,
                self.stack.len()
            );
        }

        self.frames.push(Frame {
            chunk: Rc::clone(&self.chunk),
            ip: self.ip,
            stack: caller_stack,
            locals: std::mem::replace(
                &mut self.locals,
                Vec::with_capacity(LOCALS_INITIAL_CAPACITY),
            ),
            this_value: std::mem::replace(&mut self.this_value, this_value),
            closure_env: self.current_closure_env.take(),
            slot_types: self.current_slot_types.take(),
            stack_base,
            is_strict: proto.is_strict,
            lexical_env: Rc::clone(&self.lexical_env),
            yield_star_queue: self.yield_star_queue.take(),
        });

        self.chunk = Rc::clone(&proto.chunk);
        self.ip = 0;
        self.stack.clear();
        self.current_closure_env = Some(Rc::clone(closure_env));
        self.current_slot_types = Some(proto.slot_types.clone());
        self.yield_star_queue = None;

        // Pre-allocate locals to at least `arity` slots, filling missing
        // arguments with `undefined` per JavaScript semantics.
        let args_for_arguments: Vec<Option<Value>> =
            call_args.iter().map(|v| Some(v.clone())).collect();
        self.locals = std::iter::repeat_with(|| Rc::new(RefCell::new(Value::Undefined)))
            .take(proto.slot_types.len().max(proto.arity))
            .collect();
        for (idx, is_tdz) in proto.lexical_tdz.iter().copied().enumerate() {
            if is_tdz && idx < self.locals.len() {
                *self.locals[idx].borrow_mut() = Value::Uninitialized;
            }
        }
        let fn_env = Rc::new(RefCell::new(EnvironmentRecord::new(Some(Rc::clone(
            &self.lexical_env,
        )))));
        for (idx, maybe_name) in proto.slot_names.iter().enumerate() {
            if let Some(name) = maybe_name
                && idx < self.locals.len()
            {
                let initialized = !proto.lexical_tdz.get(idx).copied().unwrap_or(false);
                fn_env.borrow_mut().create_mutable_binding(
                    name.clone(),
                    Rc::clone(&self.locals[idx]),
                    initialized,
                );
            }
        }
        self.lexical_env = fn_env;
        for (i, value) in call_args.into_iter().enumerate().take(proto.arity) {
            *self.locals[i].borrow_mut() = value;
        }
        if let Some(slot) = proto.arguments_slot
            && slot < self.locals.len()
        {
            if proto.has_simple_parameter_list && !proto.is_strict {
                let args_obj = self.build_mapped_arguments_object(proto, &args_for_arguments);
                self.locals[slot] = Rc::new(RefCell::new(args_obj));
            } else {
                let arr_rc = Rc::new(RefCell::new(args_for_arguments));
                let props: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(ObjectData::new()));
                props.borrow_mut().properties.insert(
                    PropertyKey::String("length".to_string()),
                    PropertyDescriptor::data(Value::Number(arr_rc.borrow().len() as f64)),
                );
                self.locals[slot] = Rc::new(RefCell::new(Value::Array(arr_rc, props)));
            }
        }

        Ok(())
    }

    fn build_mapped_arguments_object(
        &self,
        proto: &FunctionProto,
        args_for_arguments: &[Option<Value>],
    ) -> Value {
        let obj_rc = Rc::new(RefCell::new(ObjectData::new()));
        let arg_count = args_for_arguments.len();
        let mapped_count = proto.arity.min(self.locals.len());
        for idx in 0..arg_count {
            let key = PropertyKey::String(idx.to_string());
            if idx < mapped_count {
                let getter_cell = Rc::clone(&self.locals[idx]);
                let setter_cell = Rc::clone(&self.locals[idx]);
                let getter =
                    Value::Builtin(Rc::new(move |_vm, _args| Ok(getter_cell.borrow().clone())));
                let setter = Value::Builtin(Rc::new(move |_vm, args| {
                    let value = args
                        .get(1)
                        .cloned()
                        .unwrap_or_else(|| args.first().cloned().unwrap_or(Value::Undefined));
                    *setter_cell.borrow_mut() = value;
                    Ok(Value::Undefined)
                }));
                obj_rc.borrow_mut().properties.insert(
                    key,
                    PropertyDescriptor::Accessor {
                        get: Some(getter),
                        set: Some(setter),
                        enumerable: true,
                        configurable: true,
                    },
                );
            } else {
                obj_rc.borrow_mut().properties.insert(
                    key,
                    PropertyDescriptor::data(
                        args_for_arguments
                            .get(idx)
                            .cloned()
                            .flatten()
                            .unwrap_or(Value::Undefined),
                    ),
                );
            }
        }
        obj_rc.borrow_mut().properties.insert(
            PropertyKey::String("length".to_string()),
            PropertyDescriptor::data(Value::Number(arg_count as f64)),
        );
        Value::Object(obj_rc)
    }

    fn build_array(&mut self) -> Result<(), crate::errors::RuntimeError> {
        let n = self.read_byte()? as usize;
        let mut elems: Vec<Option<Value>> = Vec::with_capacity(n);
        for _ in 0..n {
            elems.push(Some(self.pop()?));
        }
        elems.reverse();

        let arr_rc = Rc::new(RefCell::new(elems));
        let props: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(ObjectData::new()));
        props.borrow_mut().properties.insert(
            PropertyKey::String("length".to_string()),
            PropertyDescriptor::data(Value::Number(n as f64)),
        );

        if let Some(array_val) = self
            .globals
            .borrow()
            .properties
            .get(&PropertyKey::String("Array".to_string()))
            .and_then(|d| match d {
                PropertyDescriptor::Data { value, .. } => Some(value.clone()),
                _ => None,
            })
            && let Ok(proto_val) =
                get_property(&array_val, &PropertyKey::String("prototype".to_string()), 0)
            && !matches!(proto_val, Value::Null)
        {
            props.borrow_mut().prototype = proto_val;
        }

        self.stack
            .push(Value::Array(Rc::clone(&arr_rc), Rc::clone(&props)));
        if crate::trace::enabled() {
            eprintln!(
                "[trace] build_array: created Array arr_ptr={:p} props_ptr={:p} ip={}",
                Rc::as_ptr(&arr_rc),
                Rc::as_ptr(&props),
                self.ip.saturating_sub(1)
            );
        }
        Ok(())
    }

    fn build_object(&mut self) -> Result<(), crate::errors::RuntimeError> {
        let n = self.read_byte()? as usize;
        let mut map: HashMap<PropertyKey, PropertyDescriptor> = HashMap::with_capacity(n);
        for _ in 0..n {
            let value = self.pop()?;
            let key_val = self.pop()?;
            let key = match key_val {
                Value::String(s) => PropertyKey::String(s),
                Value::Symbol(sym, _) => PropertyKey::Symbol(sym),
                other => {
                    self.dump_diag("BuildObject: key must be string", &other);
                    return Err(crate::errors::RuntimeError::from(format!(
                        "BuildObject: key must be string (actual={})",
                        other.type_name()
                    )));
                }
            };
            map.insert(key, PropertyDescriptor::data(value));
        }
        let mut data = ObjectData::new();
        data.properties = map;
        if let Some(object_val) = self
            .globals
            .borrow()
            .properties
            .get(&PropertyKey::String("Object".to_string()))
            .and_then(|d| match d {
                PropertyDescriptor::Data { value, .. } => Some(value.clone()),
                _ => None,
            })
            && let Ok(proto_val) = get_property(
                &object_val,
                &PropertyKey::String("prototype".to_string()),
                0,
            )
            && !matches!(proto_val, Value::Null)
        {
            data.prototype = proto_val;
        }
        let obj_rc = Rc::new(RefCell::new(data));
        self.stack.push(Value::Object(Rc::clone(&obj_rc)));
        if crate::trace::enabled() {
            eprintln!(
                "[trace] build_object: created Object obj_ptr={:p} ip={}",
                Rc::as_ptr(&obj_rc),
                self.ip.saturating_sub(1)
            );
        }
        Ok(())
    }

    fn make_closure(&mut self) -> Result<(), crate::errors::RuntimeError> {
        let idx = self.read_byte()? as usize;
        let proto = self
            .chunk
            .functions
            .get(idx)
            .cloned()
            .ok_or("Bad function index")?;
        let has_prototype_property = proto.has_prototype_property;

        let props: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(ObjectData::new()));
        if let Some(function_val) = self
            .globals
            .borrow()
            .properties
            .get(&PropertyKey::String("Function".to_string()))
            .and_then(|d| match d {
                PropertyDescriptor::Data { value, .. } => Some(value.clone()),
                _ => None,
            })
            && let Ok(function_proto) = get_property(
                &function_val,
                &PropertyKey::String("prototype".to_string()),
                0,
            )
            && !matches!(function_proto, Value::Null)
        {
            props.borrow_mut().prototype = function_proto;
        }

        let closure_proto = Rc::new(proto);
        let mut env_cells = Vec::with_capacity(closure_proto.upvalues.len());
        for upvalue in &closure_proto.upvalues {
            let cell = if upvalue.is_local {
                let slot = upvalue.index as usize;
                ensure_slot(&mut self.locals, slot);
                self.locals
                    .get(slot)
                    .cloned()
                    .ok_or("Bad upvalue local slot")?
            } else {
                let env = self
                    .current_closure_env
                    .as_ref()
                    .ok_or("Bad enclosing closure env")?;
                env.borrow()
                    .get(upvalue.index as usize)
                    .cloned()
                    .ok_or("Bad enclosing upvalue index")?
            };
            env_cells.push(cell);
        }

        let closure = Value::Closure {
            proto: Rc::clone(&closure_proto),
            env: Rc::new(RefCell::new(env_cells)),
            lexical_this: None,
            props: Rc::clone(&props),
        };

        props.borrow_mut().properties.insert(
            PropertyKey::String("name".to_string()),
            PropertyDescriptor::data(Value::String(closure_proto.name.clone())),
        );
        props.borrow_mut().properties.insert(
            PropertyKey::String("length".to_string()),
            PropertyDescriptor::data(Value::Number(closure_proto.arity as f64)),
        );

        if has_prototype_property {
            let object_proto = self
                .realm
                .intrinsics
                .object_prototype
                .clone()
                .unwrap_or(Value::Null);
            let proto_obj_map: Rc<RefCell<ObjectData>> =
                Rc::new(RefCell::new(ObjectData::with_prototype(object_proto)));
            let proto_obj = Value::Object(Rc::clone(&proto_obj_map));

            // Set F.prototype.constructor = F
            proto_obj_map.borrow_mut().properties.insert(
                PropertyKey::String("constructor".to_string()),
                PropertyDescriptor::Data {
                    value: closure.clone(),
                    writable: true,
                    enumerable: false,
                    configurable: true,
                },
            );

            props.borrow_mut().properties.insert(
                PropertyKey::String("prototype".to_string()),
                PropertyDescriptor::data(proto_obj),
            );
            proto_obj_map.borrow_mut().properties.insert(
                PropertyKey::String("constructor".to_string()),
                PropertyDescriptor::data(closure.clone()),
            );
        }

        self.stack.push(closure.clone());
        if crate::trace::enabled() {
            // Log closure and optional prototype object pointers so traces can
            // be correlated back to the allocation site.
            if let Value::Closure { props, .. } = &closure {
                eprintln!(
                    "[trace] make_closure: created Closure props_ptr={:p} proto_idx={} proto_chunk_ptr={:p} ip={}",
                    Rc::as_ptr(props),
                    idx,
                    Rc::as_ptr(&closure_proto.chunk),
                    self.ip.saturating_sub(1)
                );
            }
            if has_prototype_property {
                // proto_obj_map was allocated when creating the prototype
                // object above; print its pointer if available via the props
                // object we just attached.
                if let Value::Closure { props, .. } = &closure
                    && let Some(PropertyDescriptor::Data { value, .. }) = props
                        .borrow()
                        .properties
                        .get(&PropertyKey::String("prototype".to_string()))
                    && let Value::Object(proto_obj_map) = value
                {
                    eprintln!(
                        "[trace] make_closure: created proto_obj props_ptr={:p}",
                        Rc::as_ptr(proto_obj_map)
                    );
                }
            }
        }
        Ok(())
    }

    /// Inline-cached property access with generation-based invalidation.
    fn get_property_ic(&mut self) -> Result<(), crate::errors::RuntimeError> {
        let _slot = self.read_byte()? as usize;
        let key_val = self.pop()?;
        let obj = self.pop()?;
        let key = self.to_property_key(&key_val)?;
        // Disabled IC fast path for now: pointer reuse can produce stale hits
        // across short-lived objects and descriptor churn in harness tests.
        // Keep opcode shape intact, but always do authoritative lookup.
        let v = self.get_property_value(&obj, &key)?;
        if crate::trace::enabled() {
            match &key {
                PropertyKey::String(s) => {
                    eprintln!(
                        "[trace] GetPropertyIC receiver={} key='{}' -> {}",
                        obj.type_name(),
                        s,
                        v.type_name()
                    );
                }
                PropertyKey::Symbol(_) => {
                    eprintln!(
                        "[trace] GetPropertyIC receiver={} key=symbol -> {}",
                        obj.type_name(),
                        v.type_name()
                    );
                }
            }
            crate::trace::value_string("[trace]   result", &v);
        }
        self.stack.push(v);
        Ok(())
    }

    /// `SetProperty` wrapper that bumps the generation counter so that stale
    /// IC entries are invalidated on next access.
    fn set_property_tracked(
        &mut self,
        obj: &Value,
        key: &PropertyKey,
        value: Value,
    ) -> Result<(), crate::errors::RuntimeError> {
        if matches!(obj, Value::Undefined | Value::Null) {
            return Err(self.throw_type_error(&format!(
                "Cannot set property '{:?}' of {}",
                key,
                obj.type_name()
            )));
        }
        if let Some((desc, _holder)) = self.lookup_property_descriptor(obj, key, 0)?
            && let PropertyDescriptor::Accessor { set, .. } = desc
        {
            if let Some(setter) = set {
                let _ = self.call_with_this(&setter, obj, &[value])?;
                return Ok(());
            }
            return Ok(());
        }
        set_property(obj, key, value)?;
        self.bump_object_generation(obj);

        Ok(())
    }

    fn bump_object_generation(&mut self, obj: &Value) {
        let ptr = match obj {
            Value::Object(m) => Some(Rc::as_ptr(m) as usize),
            Value::Array(_, props) => Some(Rc::as_ptr(props) as usize),
            Value::Closure { props, .. } => Some(Rc::as_ptr(props) as usize),
            Value::BuiltinObject(_, props) => Some(Rc::as_ptr(props) as usize),
            _ => None,
        };
        if let Some(p) = ptr {
            *self.obj_generations.entry(p).or_insert(0) += 1;
        }
    }

    /// Unwind the call stack and operand stack to the nearest exception
    /// handler, or propagate the error if none exists.
    fn unwind_to_handler(&mut self, thrown: Value) -> Result<(), crate::errors::RuntimeError> {
        let handler = match self.handlers.pop() {
            Some(h) => h,
            None => {
                // No VM-level handler found. Store the exception value and
                // propagate an error. Note: The exception may still be caught
                // at a higher level (e.g., by the test runner or JavaScript
                // try-catch blocks that don't create VM-level handlers).
                self.last_uncaught_exception = Some(thrown.clone());
                self.stack.push(thrown.clone());
                return Err(match thrown {
                    Value::String(s) => {
                        crate::errors::RuntimeError::from(format!("Uncaught: {}", s))
                    }
                    other => {
                        if let Ok(Value::String(msg)) = self
                            .get_property_value(&other, &PropertyKey::String("message".to_string()))
                        {
                            crate::errors::RuntimeError::from(format!("Uncaught: {}", msg))
                        } else {
                            crate::errors::RuntimeError::from("Uncaught exception")
                        }
                    }
                });
            }
        };

        // Pop frames back to the handler's level, restoring caller state
        // at each step so closure env/this/chunk stay consistent.
        while self.frames.len() > handler.frame_count {
            if let Some(frame) = self.frames.pop() {
                self.chunk = frame.chunk;
                self.ip = frame.ip;
                self.stack = frame.stack;
                self.locals = frame.locals;
                self.this_value = frame.this_value;
                self.current_closure_env = frame.closure_env;
                self.current_slot_types = frame.slot_types;
            }
        }
        // Drop handler records that belong to frames we just unwound.
        self.prune_stale_handlers();

        // Restore chunk and instruction pointer from the handler record.
        self.chunk = handler.chunk;
        self.ip = handler.handler_ip;

        // Truncate stacks to the snapshot saved when PushHandler ran.
        self.stack.truncate(handler.saved_stack_len);
        self.locals.truncate(handler.saved_locals_len);

        // Push the thrown value so the catch block can bind it.
        self.stack.push(thrown);
        Ok(())
    }

    /// Helper to throw a JS exception value from bytecode or builtins.
    pub(crate) fn throw_value(&mut self, thrown: Value) -> Result<(), crate::errors::RuntimeError> {
        match self.unwind_to_handler(thrown) {
            Ok(()) => {
                self.pending_exception = true;
                Ok(())
            }
            Err(err) => {
                // Ensure pending_exception is cleared for uncaught exceptions.
                // Only caught exceptions should set it.
                self.pending_exception = false;
                Err(err)
            }
        }
    }

    /// Helper used by host code to interrupt execution via a JS exception.
    pub(crate) fn throw_exception(&mut self, thrown: Value) -> crate::errors::RuntimeError {
        match self.throw_value(thrown) {
            Ok(()) => crate::errors::RuntimeError::from("JS exception"),
            Err(err) => err,
        }
    }

    pub(crate) fn throw_type_error(&mut self, message: &str) -> crate::errors::RuntimeError {
        let err_val = self
            .create_intrinsic_error("TypeError", message)
            .unwrap_or(Value::String(message.to_string()));
        self.throw_exception(err_val)
    }

    pub(crate) fn throw_range_error(&mut self, message: &str) -> crate::errors::RuntimeError {
        let err_val = self
            .create_intrinsic_error("RangeError", message)
            .unwrap_or(Value::String(message.to_string()));
        self.throw_exception(err_val)
    }

    pub(crate) fn throw_reference_error(&mut self, message: &str) -> crate::errors::RuntimeError {
        let err_val = self
            .create_intrinsic_error("ReferenceError", message)
            .unwrap_or(Value::String(message.to_string()));
        self.throw_exception(err_val)
    }

    pub(crate) fn is_callable_value(value: &Value) -> bool {
        matches!(
            value,
            Value::Closure { .. } | Value::Builtin(_) | Value::BuiltinObject(_, _)
        )
    }

    pub(crate) fn create_intrinsic_error(
        &mut self,
        name: &str,
        message: &str,
    ) -> Result<Value, crate::errors::RuntimeError> {
        let ctor = self
            .realm
            .get_intrinsic_constructor(name)
            .or_else(|| self.get_global(name))
            .unwrap_or(Value::Undefined);
        if matches!(ctor, Value::Undefined) {
            let mut obj = ObjectData::new();
            obj.properties.insert(
                PropertyKey::String("message".to_string()),
                PropertyDescriptor::data(Value::String(message.to_string())),
            );
            return Ok(Value::Object(Rc::new(RefCell::new(obj))));
        }
        let msg = Value::String(message.to_string());
        self.construct_value(&ctor, &[msg])
    }

    // -----------------------------------------------------------------------
    // Control flow
    // -----------------------------------------------------------------------

    fn jump(&mut self, offset: isize) -> Result<(), crate::errors::RuntimeError> {
        let new_ip = (self.ip as isize) + offset;
        if new_ip < 0 || (new_ip as usize) > self.chunk.code.len() {
            return Err(crate::errors::RuntimeError::from(format!(
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
    fn read_byte(&mut self) -> Result<u8, crate::errors::RuntimeError> {
        if self.ip >= self.chunk.code.len() {
            return Err(crate::errors::RuntimeError::from(format!(
                "Unexpected end of bytecode at ip {}",
                self.ip
            )));
        }
        let b = self.chunk.code[self.ip];
        self.ip += 1;
        Ok(b)
    }

    #[inline(always)]
    fn read_short_signed(&mut self) -> Result<isize, crate::errors::RuntimeError> {
        let hi = self.read_byte()? as u16;
        let lo = self.read_byte()? as u16;
        let u = (hi << 8) | lo;
        Ok((u as i16) as isize)
    }

    #[inline(always)]
    fn read_short(&mut self) -> Result<u16, crate::errors::RuntimeError> {
        let hi = self.read_byte()? as u16;
        let lo = self.read_byte()? as u16;
        Ok((hi << 8) | lo)
    }

    // -----------------------------------------------------------------------
    // Constant pool helpers
    // -----------------------------------------------------------------------

    #[inline(always)]
    fn constant(&self, idx: usize) -> Result<Value, crate::errors::RuntimeError> {
        self.chunk
            .constants
            .get(idx)
            .cloned()
            .ok_or_else(|| crate::errors::RuntimeError::from("Bad constant index"))
    }

    #[inline(always)]
    fn string_constant(&self, idx: usize) -> Result<String, crate::errors::RuntimeError> {
        match self.chunk.constants.get(idx) {
            Some(Value::String(s)) => Ok(s.clone()),
            _ => Err(crate::errors::RuntimeError::from("Constant not a string")),
        }
    }

    // -----------------------------------------------------------------------
    // Stack helpers
    // -----------------------------------------------------------------------

    #[inline(always)]
    fn pop(&mut self) -> Result<Value, crate::errors::RuntimeError> {
        self.stack.pop().ok_or_else(|| {
            if crate::trace::enabled() {
                eprintln!("[diag] Stack underflow");
                // Extra diagnostics: print some VM state to help map the
                // underflow to the currently-executing frame and chunk.
                eprintln!(
                    "[diag] chunk_ptr={:p} chunk_len={} frames={} handlers={}",
                    Rc::as_ptr(&self.chunk),
                    self.chunk.code.len(),
                    self.frames.len(),
                    self.handlers.len()
                );
                if let Some(f) = self.frames.last() {
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
                            Op::Jump | Op::JumpIfFalse | Op::PushHandler => {
                                let hi = self.chunk.code.get(j + 1).cloned().unwrap_or(0);
                                let lo = self.chunk.code.get(j + 2).cloned().unwrap_or(0);
                                let offset = ((hi as i16) << 8) | (lo as i16);
                                eprintln!("[diag]   {}: {:?} {}", j, op, offset);
                            }
                            Op::SetLocal
                            | Op::GetLocal
                            | Op::Call
                            | Op::GetBuiltin
                            | Op::GetProperty
                            | Op::SetProperty
                            | Op::GetPropertyIC
                            | Op::MakeClosure => {
                                let arg = self.chunk.code.get(j + 1).cloned().unwrap_or(0);
                                eprintln!("[diag]   {}: {:?} {}", j, op, arg);
                            }
                            Op::SetGlobal | Op::GetGlobal => {
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
                eprintln!("[diag] Stack size: {}", self.stack.len());
            }
            crate::errors::RuntimeError::from("Stack underflow")
        })
    }

    // -----------------------------------------------------------------------
    // Arithmetic helpers
    // -----------------------------------------------------------------------

    #[inline(always)]
    fn binary_num_bigint<F, G>(&mut self, f: F, g: G) -> Result<(), crate::errors::RuntimeError>
    where
        F: FnOnce(f64, f64) -> f64,
        G: FnOnce(BigIntValue, BigIntValue) -> Result<BigIntValue, crate::errors::RuntimeError>,
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

    fn dump_diag(&self, label: &str, offender: &Value) {
        if !crate::trace::enabled() {
            return;
        }

        eprintln!("[diag] {}: actual={}", label, offender.type_name());
        crate::trace::value_string("[diag] offending value", offender);

        eprintln!("[diag] Stack snapshot (top -> bottom):");
        for (idx, v) in self.stack.iter().rev().take(32).enumerate() {
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
                            Op::Jump | Op::JumpIfFalse | Op::PushHandler => {
                                let hi = proto.chunk.code.get(j + 1).cloned().unwrap_or(0) as u16;
                                let lo = proto.chunk.code.get(j + 2).cloned().unwrap_or(0) as u16;
                                let val = ((hi << 8) | lo) as i16;
                                eprintln!("[diag] {:#06}  {:12?} {}", j, op, val);
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
                    Op::GetGlobal | Op::SetGlobal => {
                        let hi = self.chunk.code.get(j + 1).cloned().unwrap_or(0) as u16;
                        let lo = self.chunk.code.get(j + 2).cloned().unwrap_or(0) as u16;
                        let arg = ((hi << 8) | lo) as usize;
                        let name = match self.chunk.constants.get(arg) {
                            Some(Value::String(s)) => s.clone(),
                            _ => "<non-string>".to_string(),
                        };
                        eprintln!("[diag] {:#06}  {:12?} {} ({})", j, op, arg, name);
                    }
                    Op::Jump | Op::JumpIfFalse | Op::PushHandler => {
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
        eprintln!("[diag] Frames (count={}):", self.frames.len());
        for (fi, frame) in self.frames.iter().enumerate().take(8) {
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
                        Op::GetGlobal | Op::SetGlobal => {
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
    fn find_constants_containing_value(&self, v: &Value) -> Vec<(usize, usize)> {
        let mut found = Vec::new();
        for (fi, frame) in self.frames.iter().enumerate().take(12) {
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
    ) -> Result<bool, crate::errors::RuntimeError> {
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
            Value::Object(d) => d.borrow().prototype.clone(),
            Value::Array(_, props) => props.borrow().prototype.clone(),
            Value::Closure { props, .. } => props.borrow().prototype.clone(),
            Value::BuiltinObject(_, props) => props.borrow().prototype.clone(),
            Value::Promise(_) => self
                .realm
                .intrinsics
                .promise_prototype
                .clone()
                .unwrap_or(Value::Null),
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
                Value::Object(d) => d.borrow().prototype.clone(),
                Value::Array(_, props) => props.borrow().prototype.clone(),
                Value::Closure { props, .. } => props.borrow().prototype.clone(),
                Value::BuiltinObject(_, props) => props.borrow().prototype.clone(),
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
    ) -> Result<bool, crate::errors::RuntimeError> {
        let target_proto = match proto {
            Value::Object(_)
            | Value::Array(..)
            | Value::Closure { .. }
            | Value::BuiltinObject(_, _)
            | Value::Promise(_) => proto.clone(),
            _ => return Ok(false),
        };

        let mut current = match value {
            Value::Object(d) => d.borrow().prototype.clone(),
            Value::Array(_, props) => props.borrow().prototype.clone(),
            Value::Closure { props, .. } => props.borrow().prototype.clone(),
            Value::BuiltinObject(_, props) => props.borrow().prototype.clone(),
            Value::Promise(_) => self
                .realm
                .intrinsics
                .promise_prototype
                .clone()
                .unwrap_or(Value::Null),
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
                Value::Object(d) => d.borrow().prototype.clone(),
                Value::Array(_, props) => props.borrow().prototype.clone(),
                Value::Closure { props, .. } => props.borrow().prototype.clone(),
                Value::BuiltinObject(_, props) => props.borrow().prototype.clone(),
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
    ) -> Result<bool, crate::errors::RuntimeError> {
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
        Op::GetGlobal | Op::SetGlobal => 3,
        Op::GetLocal
        | Op::SetLocal
        | Op::CloneLocalCell
        | Op::Call
        | Op::CallMethod
        | Op::MakeClosure
        | Op::GetUpvalue
        | Op::SetUpvalue
        | Op::GetPropertyIC
        | Op::GetBuiltin
        | Op::BuildArray
        | Op::BuildObject => 2,
        Op::Jump | Op::JumpIfFalse | Op::PushHandler => 3,
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
fn ensure_slot(locals: &mut Vec<Rc<RefCell<Value>>>, slot: usize) {
    if slot >= locals.len() {
        locals.resize_with(slot + 1, || Rc::new(RefCell::new(Value::Undefined)));
    }
}

fn object_identity(obj: &Value) -> Option<usize> {
    match obj {
        Value::Object(map) => Some(Rc::as_ptr(map) as usize),
        // Use the array's props object for identity so IC keys remain stable
        // across dense storage reallocations and avoid pointer-reuse hits.
        Value::Array(_, props) => Some(Rc::as_ptr(props) as usize),
        Value::Closure { props, .. } => Some(Rc::as_ptr(props) as usize),
        Value::BuiltinObject(_, props) => Some(Rc::as_ptr(props) as usize),
        _ => None,
    }
}

fn to_number(v: &Value) -> Result<f64, crate::errors::RuntimeError> {
    match v {
        Value::Number(n) => Ok(*n),
        Value::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
        Value::String(s) => Ok(parse_js_number_string(s)),
        Value::Undefined => Ok(f64::NAN),
        Value::Null => Ok(0.0),
        Value::BigInt(n) => Ok(n.to_f64().unwrap_or_else(|| {
            if n.is_negative() {
                f64::NEG_INFINITY
            } else {
                f64::INFINITY
            }
        })),
        _ => Err(crate::errors::RuntimeError::from("Expected number")),
    }
}

fn to_int32(v: &Value) -> Result<i32, crate::errors::RuntimeError> {
    // Per ECMAScript ToInt32: compute ToUint32(n) then reinterpret as signed 32-bit
    let u = to_uint32(v)?;
    if u & 0x8000_0000 != 0 {
        // negative in signed 32-bit space
        Ok(((u as i64) - 0x1_0000_0000) as i32)
    } else {
        Ok(u as i32)
    }
}

fn to_uint32(v: &Value) -> Result<u32, crate::errors::RuntimeError> {
    // ToUint32 per ECMAScript: ToNumber(v), then take the integer part modulo 2^32
    let n = match v {
        Value::Number(x) => *x,
        Value::BigInt(x) => x.to_f64().unwrap_or_else(|| {
            if x.is_negative() {
                f64::NEG_INFINITY
            } else {
                f64::INFINITY
            }
        }),
        _ => {
            return Err(crate::errors::RuntimeError::from(
                "Expected number for bitwise operation",
            ));
        }
    };

    if n.is_nan() || n == 0.0 || n.is_infinite() {
        return Ok(0);
    }

    const TWO32: f64 = 4294967296.0; // 2^32
    // Use truncation toward zero for the integer part, then Euclidean remainder
    let int_part = n.trunc();
    let vmod = int_part.rem_euclid(TWO32);
    Ok(vmod as u32)
}

fn parse_js_number_string(s: &str) -> f64 {
    let s = s.trim();
    if s.is_empty() {
        return 0.0;
    }
    if matches!(s, "Infinity" | "+Infinity") {
        return f64::INFINITY;
    }
    if s == "-Infinity" {
        return f64::NEG_INFINITY;
    }

    let (sign, body) = match s.as_bytes().first() {
        Some(b'+') => (1.0, &s[1..]),
        Some(b'-') => (-1.0, &s[1..]),
        _ => (1.0, s),
    };

    let parsed = if let Some(hex) = body.strip_prefix("0x").or_else(|| body.strip_prefix("0X")) {
        BigIntValue::parse_bytes(hex.as_bytes(), 16)
            .and_then(|n| n.to_f64())
            .unwrap_or(f64::NAN)
    } else if let Some(bin) = body.strip_prefix("0b").or_else(|| body.strip_prefix("0B")) {
        BigIntValue::parse_bytes(bin.as_bytes(), 2)
            .and_then(|n| n.to_f64())
            .unwrap_or(f64::NAN)
    } else if let Some(oct) = body.strip_prefix("0o").or_else(|| body.strip_prefix("0O")) {
        BigIntValue::parse_bytes(oct.as_bytes(), 8)
            .and_then(|n| n.to_f64())
            .unwrap_or(f64::NAN)
    } else {
        body.parse::<f64>().unwrap_or(f64::NAN)
    };

    sign * parsed
}

fn parse_js_bigint_string(s: &str) -> Option<BigIntValue> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let (neg, body) = match s.as_bytes().first() {
        Some(b'+') => (false, &s[1..]),
        Some(b'-') => (true, &s[1..]),
        _ => (false, s),
    };
    let mut parsed = if let Some(hex) = body.strip_prefix("0x").or_else(|| body.strip_prefix("0X"))
    {
        BigIntValue::parse_bytes(hex.as_bytes(), 16)?
    } else if let Some(bin) = body.strip_prefix("0b").or_else(|| body.strip_prefix("0B")) {
        BigIntValue::parse_bytes(bin.as_bytes(), 2)?
    } else if let Some(oct) = body.strip_prefix("0o").or_else(|| body.strip_prefix("0O")) {
        BigIntValue::parse_bytes(oct.as_bytes(), 8)?
    } else {
        BigIntValue::parse_bytes(body.as_bytes(), 10)?
    };
    if neg {
        parsed = -parsed;
    }
    Some(parsed)
}

// Helper methods on Vm for coercion that delegate to free functions but
// provide clearer naming when used in opcode handlers.
impl Vm {
    /// Convert a runtime value with `ToInt32` semantics.
    pub fn to_int32_value(&self, v: &Value) -> Result<i32, crate::errors::RuntimeError> {
        to_int32(v)
    }

    /// Convert a runtime value with `ToUint32` semantics.
    pub fn to_uint32_value(&self, v: &Value) -> Result<u32, crate::errors::RuntimeError> {
        to_uint32(v)
    }
}

/// Reference-equality semantics for objects/arrays/closures, structural
/// equality for primitives. Matches JavaScript `===` behaviour.
fn values_equal(a: &Value, b: &Value) -> bool {
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
        _ => false,
    }
}

fn array_index_from_key(key: &PropertyKey) -> Option<usize> {
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
    if idx.to_string() != *s {
        return None;
    }
    Some(idx as usize)
}

fn array_length(props: &ObjectData) -> usize {
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
fn get_property(
    obj: &Value,
    key: &PropertyKey,
    depth: usize,
) -> Result<Value, crate::errors::RuntimeError> {
    if depth > MAX_PROTOTYPE_DEPTH {
        return Err(crate::errors::RuntimeError::from(
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
            if !matches!(d.prototype, Value::Null) {
                return get_property(&d.prototype, key, depth + 1);
            }
            Ok(Value::Undefined)
        }
        Value::Array(arr, props) => {
            if let Some(i) = array_index_from_key(key)
                && let Some(opt_value) = arr.borrow().get(i).cloned()
                && let Some(value) = opt_value
            {
                return Ok(value);
            }
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
            if !matches!(p.prototype, Value::Null) {
                return get_property(&p.prototype, key, depth + 1);
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
            if !matches!(p.prototype, Value::Null) {
                return get_property(&p.prototype, key, depth + 1);
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
            if !matches!(p.prototype, Value::Null) {
                return get_property(&p.prototype, key, depth + 1);
            }
            Ok(Value::Undefined)
        }
        _ => Ok(Value::Undefined),
    }
}

fn set_property(
    obj: &Value,
    key: &PropertyKey,
    value: Value,
) -> Result<(), crate::errors::RuntimeError> {
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
                        a.resize(i + 1, Some(Value::Undefined));
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
                    PropertyDescriptor::data(Value::Number(a.len() as f64)),
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

fn build_module_namespace_object(exports: &HashMap<String, Rc<RefCell<Value>>>) -> Value {
    let namespace = Rc::new(RefCell::new(ObjectData::new()));
    for (name, slot) in exports {
        if name == "*" {
            continue;
        }
        let slot = Rc::clone(slot);
        let getter: BuiltinFn = Rc::new(move |_vm, _args| Ok(slot.borrow().clone()));
        namespace.borrow_mut().properties.insert(
            PropertyKey::String(name.clone()),
            PropertyDescriptor::Accessor {
                get: Some(Value::Builtin(getter)),
                set: None,
                enumerable: true,
                configurable: false,
            },
        );
    }
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
mod tests {
    use super::*;
    use crate::compiler;
    use crate::parser::Parser;
    use crate::runtime_utils;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn test_globals() -> Rc<RefCell<ObjectData>> {
        Rc::new(RefCell::new(ObjectData::new()))
    }

    #[test]
    fn builtin_object_property_lookup_uses_props_and_prototype() {
        let proto = Rc::new(RefCell::new(ObjectData::new()));
        proto.borrow_mut().properties.insert(
            PropertyKey::String("inherited".to_string()),
            PropertyDescriptor::data(Value::Number(7.0)),
        );

        let props = Rc::new(RefCell::new(ObjectData::with_prototype(Value::Object(
            Rc::clone(&proto),
        ))));
        props.borrow_mut().properties.insert(
            PropertyKey::String("own".to_string()),
            PropertyDescriptor::data(Value::Number(3.0)),
        );

        let builtin = Value::BuiltinObject(Rc::new(|_, _| Ok(Value::Undefined)), props);

        assert!(matches!(
            get_property(&builtin, &PropertyKey::String("own".to_string()), 0).unwrap(),
            Value::Number(3.0)
        ));
        assert!(matches!(
            get_property(&builtin, &PropertyKey::String("inherited".to_string()), 0).unwrap(),
            Value::Number(7.0)
        ));
    }

    #[test]
    fn set_property_tracked_mutates_builtin_object_props() {
        let globals = test_globals();
        let mut vm = Vm::new(globals);
        let props = Rc::new(RefCell::new(ObjectData::new()));
        let builtin = Value::BuiltinObject(Rc::new(|_, _| Ok(Value::Undefined)), Rc::clone(&props));

        vm.set_property_tracked(
            &builtin,
            &PropertyKey::String("prototype".to_string()),
            Value::Number(11.0),
        )
        .unwrap_or_default();

        let stored = props
            .borrow()
            .properties
            .get(&PropertyKey::String("prototype".to_string()))
            .cloned();
        assert!(matches!(
            stored,
            Some(PropertyDescriptor::Data {
                value: Value::Number(_),
                ..
            })
        ));
    }

    #[test]
    fn get_property_ic_distinguishes_keys_at_same_slot() {
        let globals = test_globals();
        let mut vm = Vm::new(globals);

        let obj_data = Rc::new(RefCell::new(ObjectData::new()));
        obj_data.borrow_mut().properties.insert(
            PropertyKey::String("x".to_string()),
            PropertyDescriptor::data(Value::Number(1.0)),
        );
        obj_data.borrow_mut().properties.insert(
            PropertyKey::String("y".to_string()),
            PropertyDescriptor::data(Value::Number(2.0)),
        );
        let obj = Value::Object(Rc::clone(&obj_data));

        vm.chunk = Rc::new(Chunk {
            code: vec![0],
            constants: vec![],
            functions: vec![],
            lines: vec![1],
            source_text: Rc::new(String::new()),
        });
        vm.ip = 0;
        vm.stack.push(obj.clone());
        vm.stack.push(Value::String("x".to_string()));
        vm.get_property_ic().unwrap();
        assert!(matches!(vm.pop().unwrap(), Value::Number(1.0)));

        vm.ip = 0;
        vm.stack.push(obj);
        vm.stack.push(Value::String("y".to_string()));
        vm.get_property_ic().unwrap();
        assert!(matches!(vm.pop().unwrap(), Value::Number(2.0)));
    }

    #[test]
    fn same_value_treats_nan_as_equal() {
        assert!(same_value(
            &Value::Number(f64::NAN),
            &Value::Number(f64::NAN)
        ));
    }

    #[test]
    fn same_value_distinguishes_signed_zero() {
        assert!(!same_value(&Value::Number(0.0), &Value::Number(-0.0)));
        assert!(same_value(&Value::Number(-0.0), &Value::Number(-0.0)));
    }

    #[test]
    fn same_value_uses_reference_identity_for_objects() {
        let obj = Rc::new(RefCell::new(ObjectData::new()));
        let a = Value::Object(Rc::clone(&obj));
        let b = Value::Object(Rc::clone(&obj));
        let c = Value::Object(Rc::new(RefCell::new(ObjectData::new())));

        assert!(same_value(&a, &b));
        assert!(!same_value(&a, &c));
    }

    #[test]
    fn dense_array_index_beyond_max_doesnt_dense_resize() {
        let arr_data = Rc::new(RefCell::new(vec![Some(Value::Number(0.0))]));
        let props = Rc::new(RefCell::new(ObjectData::new()));
        props.borrow_mut().properties.insert(
            PropertyKey::String("length".to_string()),
            PropertyDescriptor::data(Value::Number(1.0)),
        );
        let arr = Value::Array(Rc::clone(&arr_data), Rc::clone(&props));
        let key = PropertyKey::String("4294967295".to_string());

        set_property(&arr, &key, Value::String("x".to_string())).unwrap();

        assert_eq!(arr_data.borrow().len(), 1);
        assert!(matches!(
            get_property(&arr, &key, 0).unwrap(),
            Value::String(ref s) if s == "x"
        ));
        assert!(matches!(
            get_property(&arr, &PropertyKey::String("length".to_string()), 0).unwrap(),
            Value::Number(n) if (n - 1.0).abs() < f64::EPSILON
        ));
    }

    #[test]
    fn sparse_array_index_updates_length_without_dense_resize() {
        let arr_data = Rc::new(RefCell::new(vec![Some(Value::Number(0.0))]));
        let props = Rc::new(RefCell::new(ObjectData::new()));
        props.borrow_mut().properties.insert(
            PropertyKey::String("length".to_string()),
            PropertyDescriptor::data(Value::Number(1.0)),
        );
        let arr = Value::Array(Rc::clone(&arr_data), Rc::clone(&props));
        let key = PropertyKey::String("4294967294".to_string());

        set_property(&arr, &key, Value::Number(1.0)).unwrap();

        assert_eq!(arr_data.borrow().len(), 1);
        assert!(matches!(
            get_property(&arr, &key, 0).unwrap(),
            Value::Number(n) if (n - 1.0).abs() < f64::EPSILON
        ));
        assert!(matches!(
            get_property(&arr, &PropertyKey::String("length".to_string()), 0).unwrap(),
            Value::Number(n) if (n - 4294967295.0).abs() < f64::EPSILON
        ));
    }

    #[test]
    fn top_level_assignment_updates_global_binding() {
        let session = Parser::parse_source("var x; x = 5;").expect("parse ok");
        let program = session.program();
        let chunk = compiler::compile(program, session.source()).expect("compile ok");

        let globals = test_globals();
        let mut vm = Vm::new(Rc::clone(&globals));
        let result = vm.run(chunk).expect("run ok");

        assert!(matches!(result, Value::Undefined));
        assert!(
            matches!(vm.get_global("x"), Some(Value::Number(n)) if (n - 5.0).abs() < f64::EPSILON)
        );
    }

    #[test]
    fn eval_for_loop_completion_is_undefined() {
        let src = r#"
            var supreme, count;
            supreme = 5;
            var evaluated = eval("for(count=0;;) {if (count===supreme)break;else count++; }");
        "#;
        let session = Parser::parse_source(src).expect("parse ok");
        let program = session.program();
        let chunk = compiler::compile(program, session.source()).expect("compile ok");

        let globals = test_globals();
        runtime_utils::install_minimal_kernel_constructors(&globals);
        let mut vm = Vm::new(Rc::clone(&globals));
        let result = vm.run(chunk).expect("run ok");

        assert!(matches!(result, Value::Undefined));
        assert!(
            matches!(vm.get_global("supreme"), Some(Value::Number(n)) if (n - 5.0).abs() < f64::EPSILON)
        );
        assert!(
            matches!(vm.get_global("count"), Some(Value::Number(n)) if (n - 5.0).abs() < f64::EPSILON)
        );
        assert!(matches!(vm.get_global("evaluated"), Some(Value::Undefined)));
    }

    #[test]
    fn void_operator_evaluates_to_undefined() {
        let session = Parser::parse_source("var x = void 0;").expect("parse ok");
        let program = session.program();
        let chunk = compiler::compile(program, session.source()).expect("compile ok");

        let globals = test_globals();
        let mut vm = Vm::new(Rc::clone(&globals));
        let result = vm.run(chunk).expect("run ok");

        assert!(matches!(result, Value::Undefined));
        assert!(matches!(vm.get_global("x"), Some(Value::Undefined)));
    }

    #[test]
    fn logical_and_short_circuits_and_preserves_left_operand() {
        let session = Parser::parse_source("var x = 0 && bump();").expect("parse ok");
        let program = session.program();
        let chunk = compiler::compile(program, session.source()).expect("compile ok");

        let calls = Rc::new(RefCell::new(0usize));
        let globals = test_globals();
        globals.borrow_mut().properties.insert(
            PropertyKey::String("bump".to_string()),
            PropertyDescriptor::data(Value::Builtin(Rc::new({
                let calls = Rc::clone(&calls);
                move |_vm, _args| {
                    *calls.borrow_mut() += 1;
                    Ok(Value::Number(99.0))
                }
            }))),
        );

        let mut vm = Vm::new(Rc::clone(&globals));
        let result = vm.run(chunk).expect("run ok");

        assert_eq!(*calls.borrow(), 0);
        assert!(matches!(result, Value::Undefined));
        assert!(matches!(vm.get_global("x"), Some(Value::Number(n)) if n == 0.0));
    }

    #[test]
    fn logical_or_short_circuits_and_preserves_left_operand() {
        let session = Parser::parse_source("var x = 1 || bump();").expect("parse ok");
        let program = session.program();
        let chunk = compiler::compile(program, session.source()).expect("compile ok");

        let calls = Rc::new(RefCell::new(0usize));
        let globals = test_globals();
        globals.borrow_mut().properties.insert(
            PropertyKey::String("bump".to_string()),
            PropertyDescriptor::data(Value::Builtin(Rc::new({
                let calls = Rc::clone(&calls);
                move |_vm, _args| {
                    *calls.borrow_mut() += 1;
                    Ok(Value::Number(99.0))
                }
            }))),
        );

        let mut vm = Vm::new(Rc::clone(&globals));
        let result = vm.run(chunk).expect("run ok");

        assert_eq!(*calls.borrow(), 0);
        assert!(matches!(result, Value::Undefined));
        assert!(matches!(vm.get_global("x"), Some(Value::Number(n)) if n == 1.0));
    }

    #[test]
    fn lexical_binding_reads_before_init_throw_reference_error() {
        let session = Parser::parse_source("function f(){ x; let x = 1; } f();").expect("parse ok");
        let program = session.program();
        let chunk = compiler::compile(program, session.source()).expect("compile ok");
        let globals = test_globals();
        runtime_utils::install_minimal_kernel_constructors(&globals);
        let mut vm = Vm::new(globals);
        let err = vm.run(chunk).expect_err("run should fail for TDZ read");
        let msg = err.to_string();
        assert!(
            msg.contains("Cannot access lexical binding before initialization"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn var_binding_reads_before_init_return_undefined() {
        let session = Parser::parse_source("function f(){ return x; var x = 1; } var out = f();")
            .expect("parse ok");
        let program = session.program();
        let chunk = compiler::compile(program, session.source()).expect("compile ok");
        let globals = test_globals();
        runtime_utils::install_minimal_kernel_constructors(&globals);
        let mut vm = Vm::new(Rc::clone(&globals));
        vm.run(chunk).expect("run ok");
        assert!(matches!(vm.get_global("out"), Some(Value::Undefined)));
    }

    #[test]
    fn generator_next_yields_values_and_done() {
        let src = r#"
            function* g() { yield 1; yield 2; return 3; }
            var it = g();
            var a = it.next();
            var b = it.next();
            var c = it.next();
            var d = it.next();
        "#;
        let session = Parser::parse_source(src).expect("parse ok");
        let program = session.program();
        let chunk = compiler::compile(program, session.source()).expect("compile ok");
        let globals = test_globals();
        let mut vm = Vm::new(Rc::clone(&globals));
        vm.run(chunk).expect("run ok");

        let a = vm.get_global("a").expect("a exists");
        let b = vm.get_global("b").expect("b exists");
        let c = vm.get_global("c").expect("c exists");
        let d = vm.get_global("d").expect("d exists");

        assert!(matches!(
            get_property(&a, &PropertyKey::String("value".to_string()), 0).unwrap(),
            Value::Number(n) if n == 1.0
        ));
        assert!(matches!(
            get_property(&a, &PropertyKey::String("done".to_string()), 0).unwrap(),
            Value::Bool(false)
        ));
        assert!(matches!(
            get_property(&b, &PropertyKey::String("value".to_string()), 0).unwrap(),
            Value::Number(n) if n == 2.0
        ));
        assert!(matches!(
            get_property(&c, &PropertyKey::String("value".to_string()), 0).unwrap(),
            Value::Number(n) if n == 3.0
        ));
        assert!(matches!(
            get_property(&c, &PropertyKey::String("done".to_string()), 0).unwrap(),
            Value::Bool(true)
        ));
        assert!(matches!(
            get_property(&d, &PropertyKey::String("done".to_string()), 0).unwrap(),
            Value::Bool(true)
        ));
    }

    #[test]
    fn class_declaration_binds_constructor_function() {
        let src = r#"
            class A {}
        "#;
        let session = Parser::parse_source(src).expect("parse ok");
        let chunk = compiler::compile(session.program(), session.source()).expect("compile ok");
        let globals = test_globals();
        let mut vm = Vm::new(Rc::clone(&globals));
        vm.run(chunk).expect("run ok");
        assert!(matches!(vm.get_global("A"), Some(Value::Closure { .. })));
    }
}
