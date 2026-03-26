//! Stack-based bytecode interpreter with runtime type enforcement.
//!
//! The VM executes compiled bytecode in a stack-based environment. It provides:
//! - Stack and local variable management
//! - Function call frames and closures
//! - Property access with generation-guarded inline caching
//! - Exception handling (try/catch)
//! - Runtime type checking
//!
//! # Architecture
//!
//! The VM maintains:
//! - A value stack for operands and intermediate results
//! - A locals array for function-local variables
//! - A globals map for global variables
//! - A call stack of frames for function calls
//! - Exception handler records for try/catch
//! - A per-object generation table for IC invalidation
//!
//! # Execution
//!
//! The VM uses a fetch-decode-execute loop over bytecode instructions.
//! Each instruction manipulates the stack or control flow.
//!
//! # Type Safety
//!
//! When type annotations are available, the VM enforces them at runtime:
//! - Local variable assignments check slot types
//! - Global variable assignments check global types
//! - Function arguments are validated on call
//!
//! # Example
//!
//! ```
//! use tsblitz::vm::Vm;
//! use tsblitz::bytecode::Chunk;
//! use std::cell::RefCell;
//! use std::rc::Rc;
//! use tsblitz::value::ObjectData;
//!
//! let globals = Rc::new(RefCell::new(ObjectData::new()));
//! let mut vm = Vm::new(globals);
//! // vm.run(chunk)?;
//! ```

use crate::bytecode::{Chunk, Op};
use crate::module::ModuleLoader;
use crate::typecheck::BuiltinType;
use crate::typecheck::Type;
use crate::value::{BuiltinFn, ObjectData, PropertyDescriptor, PropertyKey, Value};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::env;
use std::future::Future;
use std::rc::Rc;
use std::sync::{Arc, OnceLock};
use tokio::runtime::Runtime;

// ---------------------------------------------------------------------------
// Tuning constants
// ---------------------------------------------------------------------------

/// Initial operand stack capacity. Avoids reallocation for most programs.
const STACK_INITIAL_CAPACITY: usize = 512;

/// Initial locals vector capacity per call frame.
const LOCALS_INITIAL_CAPACITY: usize = 64;

/// Initial call-frame stack capacity.
const FRAMES_INITIAL_CAPACITY: usize = 64;

/// Maximum prototype chain traversal depth before we assume a cycle.
const MAX_PROTOTYPE_DEPTH: usize = 128;

/// Default IC slot count per chunk (grows on demand).
const IC_SLOTS_DEFAULT: usize = 256;

/// Maximum gap we materialize into the dense backing store before treating an
/// array write as sparse and storing it on the property bag instead.
const MAX_DENSE_ARRAY_GROWTH: usize = 1024;

// ---------------------------------------------------------------------------
// Inline-cache entry
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct IcEntry {
    /// Raw `Rc` pointer used as an object identity token.
    obj_ptr: usize,
    /// Property key cached at this site.
    key: PropertyKey,
    /// Object generation at the time the entry was cached. If the object
    /// has been mutated since, the generation will have advanced and this
    /// entry is stale.
    generation: u64,
    /// Cached property value.
    value: Value,
}

// Call frame
struct Frame {
    chunk: Rc<Chunk>,
    ip: usize,
    locals: Vec<Rc<RefCell<Value>>>,
    this_value: Value,
    closure_env: Option<Rc<RefCell<Vec<Rc<RefCell<Value>>>>>>,
    slot_types: Option<Vec<Option<Type>>>,
    /// Length of `Vm::stack` immediately before the callee's first operand.
    /// On return the stack is truncated back to this point and the return
    /// value is pushed on top.
    stack_base: usize,
}

// ---------------------------------------------------------------------------
// Exception handler record
// ---------------------------------------------------------------------------

struct HandlerRecord {
    chunk: Rc<Chunk>,
    handler_ip: usize,
    saved_stack_len: usize,
    saved_locals_len: usize,
    frame_count: usize,
}

enum StepOutcome {
    Continue,
    Return(Value),
}

#[derive(Clone, Debug, Default)]
struct PartialDescriptor {
    value: Option<Value>,
    writable: Option<bool>,
    enumerable: Option<bool>,
    configurable: Option<bool>,
    get: Option<Value>,
    set: Option<Value>,
}

// ---------------------------------------------------------------------------
// Global background Tokio runtime (lazy, shared across all Vm instances)
// ---------------------------------------------------------------------------

static GLOBAL_BG_RUNTIME: OnceLock<Arc<Runtime>> = OnceLock::new();

fn get_global_bg_runtime() -> Arc<Runtime> {
    GLOBAL_BG_RUNTIME
        .get_or_init(|| {
            let threads_from_env = env::var("TSBLITZ_BG_THREADS")
                .ok()
                .and_then(|s| s.parse::<usize>().ok())
                .filter(|&n| n > 0);

            let cpus = std::thread::available_parallelism()
                .map(std::num::NonZero::get)
                .unwrap_or(4);

            let threads = threads_from_env.unwrap_or_else(|| if cpus > 1 { cpus - 1 } else { 1 });

            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(threads)
                .enable_all()
                .build()
                .expect("Failed to build Tokio runtime for VM background tasks");

            Arc::new(rt)
        })
        .clone()
}

// ---------------------------------------------------------------------------
// VM
// ---------------------------------------------------------------------------

/// Virtual machine for executing bytecode.
pub struct Vm {
    /// Chunk being executed.
    chunk: Rc<Chunk>,
    /// Instruction pointer within the chunk.
    ip: usize,
    /// Operand stack. Shared across all frames; each frame records its base
    /// offset so the stack is truncated correctly on return.
    stack: Vec<Value>,
    /// Local variables for the current frame.
    locals: Vec<Rc<RefCell<Value>>>,
    /// Current `this` binding.
    this_value: Value,
    /// Upvalue environment for the currently executing closure, if any.
    current_closure_env: Option<Rc<RefCell<Vec<Rc<RefCell<Value>>>>>>,
    /// Global variables.
    globals: Rc<RefCell<ObjectData>>,
    /// Call-frame stack.
    frames: Vec<Frame>,
    /// Root chunk (kept alive for global type metadata).
    root_chunk: Option<Rc<Chunk>>,
    /// Slot-type annotations for the current function (formerly used by TS checks).
    current_slot_types: Option<Vec<Option<Type>>>,
    /// Per-chunk inline-cache storage, keyed by chunk `Rc` pointer address.
    ic_caches: HashMap<usize, Vec<Option<IcEntry>>>,
    /// Per-object mutation generation counter, keyed by `Rc` pointer address.
    /// Incremented on every `SetProperty` so that stale IC entries are
    /// detected and evicted on the next lookup.
    obj_generations: HashMap<usize, u64>,
    /// Background Tokio runtime handle.
    bg_runtime: Arc<Runtime>,
    /// Exception-handler stack.
    handlers: Vec<HandlerRecord>,
    /// Per-VM module loader responsible for module construction/instantiation/eval
    module_loader: Option<ModuleLoader>,
    /// Per-VM module table: vector indexed by module index, each mapping export name -> shared slot
    module_table: Vec<HashMap<String, Rc<RefCell<Value>>>>,
}

impl Vm {
    // -----------------------------------------------------------------------
    // Construction & public helpers
    // -----------------------------------------------------------------------

    /// Create a new virtual machine.
    pub fn new(globals: Rc<RefCell<ObjectData>>) -> Self {
        Self {
            chunk: Rc::new(Chunk::new()),
            ip: 0,
            stack: Vec::with_capacity(STACK_INITIAL_CAPACITY),
            locals: Vec::with_capacity(LOCALS_INITIAL_CAPACITY),
            this_value: Value::Undefined,
            current_closure_env: None,
            globals,
            frames: Vec::with_capacity(FRAMES_INITIAL_CAPACITY),
            root_chunk: None,
            current_slot_types: None,
            ic_caches: HashMap::new(),
            obj_generations: HashMap::new(),
            bg_runtime: get_global_bg_runtime(),
            handlers: Vec::new(),
            // Each VM gets its own module loader
            module_loader: Some(ModuleLoader::new()),
            module_table: Vec::new(),
        }
    }

    /// Create a new VM and ensure the minimal kernel constructors and
    /// runtime helpers are installed on the provided globals. Doing this here
    /// ensures every VM has the expected helpers (including a `globalThis`
    /// object) available for the soft prelude without requiring callers to
    /// remember to invoke runtime setup.
    pub fn with_runtime(self) -> Self {
        // Install minimal kernel constructors/helpers onto the globals map.
        // This is defensive: the installer will only populate missing keys
        // and will not override host-provided helpers.
        crate::runtime_utils::install_minimal_kernel_constructors(&self.globals);
        self
    }

    /// Provide mutable access to the per-VM module loader, creating it if missing.
    pub fn module_loader_mut(&mut self) -> &mut ModuleLoader {
        if self.module_loader.is_none() {
            self.module_loader = Some(ModuleLoader::new());
        }
        // SAFETY: we just ensured module_loader is Some
        self.module_loader.as_mut().unwrap()
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
        self.ip = 0;
        self.chunk = Rc::new(Chunk::new());
        self.ic_caches.clear();
        self.obj_generations.clear();
        let mut g = self.globals.borrow_mut();
        g.properties
            .retain(|k, _| matches!(k, PropertyKey::String(s) if keep_globals.contains(s)));
    }

    // -----------------------------------------------------------------------
    // Background runtime helpers
    // -----------------------------------------------------------------------

    /// Run a blocking closure on the background runtime and wait for the
    /// result synchronously.
    pub fn run_blocking<F, R>(&self, f: F) -> Result<R, crate::errors::RuntimeError>
    where
        F: FnOnce() -> R + Send + 'static,
        R: Send + 'static,
    {
        let rt = Arc::clone(&self.bg_runtime);
        let handle = rt.spawn_blocking(f);
        rt.block_on(handle).map_err(|e| {
            crate::errors::RuntimeError::from(format!("Background task failed: {}", e))
        })
    }

    /// Synchronously execute an async future on the background runtime.
    pub fn block_on<Fut>(&self, fut: Fut) -> Result<Fut::Output, crate::errors::RuntimeError>
    where
        Fut: Future + Send + 'static,
        Fut::Output: Send + 'static,
    {
        Ok(Arc::clone(&self.bg_runtime).block_on(fut))
    }

    /// Spawn a fire-and-forget task on the background runtime.
    pub fn spawn_bg<Fut>(&self, fut: Fut)
    where
        Fut: Future<Output = ()> + Send + 'static,
    {
        let _ = self.bg_runtime.spawn(fut);
    }

    // -----------------------------------------------------------------------
    // Main execution loop
    // -----------------------------------------------------------------------

    /// Run a chunk of bytecode to completion.
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
            match self.step(false)? {
                StepOutcome::Continue => {}
                StepOutcome::Return(result) => return Ok(result),
            }
        }
    }

    // -----------------------------------------------------------------------
    // Opcode helpers (called from the main loop)
    // -----------------------------------------------------------------------

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
                let result = (f.as_ref())(self, &args)?;
                self.stack.push(result);
            }
            Value::BuiltinObject(f, _props) => {
                let result = (f.as_ref())(self, &args)?;
                self.stack.push(result);
            }
            Value::Closure { ref proto, env, .. } => {
                let call_args = self.prepare_call_args(&args, proto)?;
                // Argument validation removed in JS-first mode.
                self.push_call_frame(proto, &env, call_args, Value::Undefined)?;
            }
            _ => {
                self.dump_diag("Call failed: callee not callable", &callee);
                return Err(crate::errors::RuntimeError::from(format!(
                    "Call: not a function (actual={})",
                    callee.type_name()
                )));
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
                call_args.push(receiver);
                call_args.extend_from_slice(&args);
                let result = (f.as_ref())(self, &call_args)?;
                self.stack.push(result);
            }
            Value::BuiltinObject(f, _props) => {
                let mut call_args: Vec<Value> = Vec::with_capacity(args.len() + 1);
                call_args.push(receiver);
                call_args.extend_from_slice(&args);
                let result = (f.as_ref())(self, &call_args)?;
                self.stack.push(result);
            }
            Value::Closure { ref proto, env, .. } => {
                let call_args = self.prepare_call_args(&args, proto)?;
                // Argument validation removed in JS-first mode.
                self.push_call_frame(proto, &env, call_args, receiver)?;
            }
            Value::Undefined if matches!(receiver, Value::Array(..)) => {
                // Special case: array methods with undefined callee
                if let Some(callback) = args.first().cloned() {
                    let items: Vec<Value> = match &receiver {
                        Value::Array(arr, _) => arr.borrow().clone(),
                        _ => Vec::new(),
                    };
                    for (i, item) in items.iter().enumerate() {
                        let idx = Value::Number(i as f64);
                        let _ = self.call_value_with_this(
                            &callback,
                            &Value::Undefined,
                            &[item.clone(), idx, receiver.clone()],
                        );
                    }
                    self.stack.push(Value::Undefined);
                } else {
                    self.dump_diag("CallWithThis failed: callee not callable", &callee);
                    return Err(crate::errors::RuntimeError::from(
                        "CallWithThis: not a function",
                    ));
                }
            }
            Value::Undefined => {
                // Calling undefined as a method - just return undefined
                // This happens when accessing a non-existent property
                self.stack.push(Value::Undefined);
            }
            Value::Null => {
                // Calling null as a method should throw TypeError
                return Err(crate::errors::RuntimeError::from(
                    "Cannot call null as a function",
                ));
            }
            _ => {
                // Debug: print what value type we got
                eprintln!("CallWithThis ERROR: callee={:?}, receiver={:?}", callee, receiver);
                self.dump_diag("CallWithThis failed: callee not callable", &callee);
                return Err(crate::errors::RuntimeError::from(
                    "CallWithThis: not a function",
                ));
            }
        }
        Ok(())
    }

    /// Host hook: define an own property with a descriptor.
    pub fn define_property_builtin(
        &mut self,
        args: &[Value],
    ) -> Result<Value, crate::errors::RuntimeError> {
        if args.len() < 3 {
            return Ok(Value::Undefined);
        }
        let target = &args[0];
        let key = self.to_property_key(&args[1])?;
        let desc = self.read_descriptor_from_value(&args[2])?;
        let ok = self.define_own_property(target, key, desc)?;
        Ok(Value::Bool(ok))
    }

    /// Host hook: define an own property from explicit arguments.
    pub fn define_property_value(
        &mut self,
        target: &Value,
        key: &Value,
        descriptor: &Value,
    ) -> Result<bool, crate::errors::RuntimeError> {
        let key = self.to_property_key(key)?;
        let desc = self.read_descriptor_from_value(descriptor)?;
        self.define_own_property(target, key, desc)
    }

    /// Host hook: invoke a callable with an explicit receiver.
    pub fn call_value_with_this(
        &mut self,
        callee: &Value,
        this_arg: &Value,
        args: &[Value],
    ) -> Result<Value, crate::errors::RuntimeError> {
        self.call_with_this(callee, this_arg, args)
    }

    pub fn eval_expression_source(
        &mut self,
        source: &str,
    ) -> Result<Value, crate::errors::RuntimeError> {
        let wrapped = format!("__tsblitz_eval_result = ({});", source);
        let session = crate::parser::Parser::parse_source(&wrapped)
            .map_err(|e| crate::errors::RuntimeError::from(format!("parse: {}", e)))?;
        let prog = session.program();
        let chunk = crate::compiler::compile_oxc(prog)
            .map_err(|e| crate::errors::RuntimeError::from(format!("compile: {}", e)))?;
        let mut child = Vm::new(Rc::clone(&self.globals));
        let _ = child.run(chunk)?;
        Ok(child
            .get_global("__tsblitz_eval_result")
            .unwrap_or(Value::Undefined))
    }

    /// Host hook: construct a callable with `new` semantics.
    pub fn construct_value(
        &mut self,
        callee: &Value,
        args: &[Value],
    ) -> Result<Value, crate::errors::RuntimeError> {
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
                let result = self.call_with_this(callee, &instance, args)?;
                Ok(match result {
                    Value::Object(_)
                    | Value::Array(..)
                    | Value::Closure { .. }
                    | Value::BuiltinObject(_, _) => result,
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
                 // For builtin constructors, pass user args directly. call_with_this will
                 // prepend the instance as 'this'. Builtin constructors receive:
                 // args[0] = the new instance (via call_with_this)
                 // args[1..] = user-provided constructor arguments
                 let result = self.call_with_this(callee, &instance, args)?;
                 Ok(match result {
                     Value::Object(_)
                     | Value::Array(..)
                     | Value::Closure { .. }
                     | Value::BuiltinObject(_, _) => result,
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
    pub fn get_property_host(
        &mut self,
        obj: &Value,
        key: &PropertyKey,
    ) -> Result<Value, crate::errors::RuntimeError> {
        self.get_property_value(obj, key)
    }

    /// Host hook: test whether a property exists on the receiver or its prototype chain.
    pub fn has_property_host(
        &self,
        obj: &Value,
        key: &PropertyKey,
    ) -> Result<bool, crate::errors::RuntimeError> {
        Ok(self.lookup_property_descriptor(obj, key, 0)?.is_some())
    }

    /// Host hook: delete an own property.
    pub fn delete_property_host(
        &mut self,
        obj: &Value,
        key: &PropertyKey,
    ) -> Result<bool, crate::errors::RuntimeError> {
        match obj {
            Value::Object(data) => {
                let mut d = data.borrow_mut();
                match d.properties.get(key) {
                    Some(PropertyDescriptor::Data { configurable, .. })
                    | Some(PropertyDescriptor::Accessor { configurable, .. })
                        if !*configurable =>
                    {
                        Ok(false)
                    }
                    _ => Ok(true),

                }
            }
            Value::Array(arr, props) => {
                if let PropertyKey::String(s) = key
                    && let Ok(i) = s.parse::<usize>()
                    && i < arr.borrow().len()
                {
                    arr.borrow_mut()[i] = Value::Undefined;
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
                    _ => Ok(true),

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
                    _ => Ok(true),

                }
            }
            _ => Ok(false),
        }
    }

    /// Host hook: set a property value on a target object-like value.
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
                let result = (f.as_ref())(self, &call_args)?;
                Ok(result)
            }
            Value::BuiltinObject(f, _props) => {
                let mut call_args: Vec<Value> = Vec::with_capacity(args.len() + 1);
                call_args.push(this_arg.clone());
                call_args.extend_from_slice(args);
                let result = (f.as_ref())(self, &call_args)?;
                Ok(result)
            }
            Value::Closure { proto, .. } => {
                let mut call_args: Vec<Value> = Vec::with_capacity(args.len());
                call_args.extend_from_slice(args);
                let call_args = self.prepare_call_args(&call_args, proto)?;
                self.validate_call_args(&call_args, proto)?;
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
                let closure_env = match callee {
                    Value::Closure { env, .. } => Rc::clone(env),
                    _ => unreachable!(),
                };
                self.push_call_frame(proto, &closure_env, call_args, this_arg.clone())?;
                let result = self.run_until_return(stack_base)?;
                // Restore caller state (run_until_return already truncated stack)
                self.chunk = caller_chunk;
                self.ip = caller_ip;
                self.locals = caller_locals;
                self.this_value = caller_this;
                self.current_closure_env = caller_env;
                self.current_slot_types = caller_slot_types;
                self.stack = saved_stack;
                Ok(result)
            }
            _ => {
                self.dump_diag("CallWithThis failed: callee not callable", callee);
                Err(crate::errors::RuntimeError::from(
                    "CallWithThis: not a function",
                ))
            }
        }
    }

    fn run_until_return(
        &mut self,
        stack_base: usize,
    ) -> Result<Value, crate::errors::RuntimeError> {
        loop {
            match self.step(true)? {
                StepOutcome::Continue => {}
                StepOutcome::Return(result) => {
                    self.stack.truncate(stack_base);
                    return Ok(result);
                }
            }
        }
    }

    fn step(&mut self, stop_on_return: bool) -> Result<StepOutcome, crate::errors::RuntimeError> {
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
            Op::True => self.stack.push(Value::Bool(true)),
            Op::False => self.stack.push(Value::Bool(false)),
            Op::GetLocal => {
                let slot = self.read_byte()? as usize;
                let v = self
                    .locals
                    .get(slot)
                    .map(|cell| cell.borrow().clone())
                    .ok_or("Bad local slot")?;
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
                    .get(&PropertyKey::String(name))
                    .and_then(|d| match d {
                        PropertyDescriptor::Data { value, .. } => Some(value.clone()),
                        _ => None,
                    })
                    .unwrap_or(Value::Undefined);
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
                // BigInt arithmetic: both operands must be BigInt
                if matches!(a, Value::BigInt(_)) || matches!(b, Value::BigInt(_)) {
                    if let (Value::BigInt(x), Value::BigInt(y)) = (a, b) {
                        self.stack.push(Value::BigInt(x + y));
                    } else {
                        return Err(crate::errors::RuntimeError::from(
                            "Add: cannot mix BigInt and other types",
                        ));
                    }
                } else {
                    // Ordinary addition: perform ToPrimitive with hint 'default'
                    let a_prim = self.to_primitive(&a, None)?;
                    let b_prim = self.to_primitive(&b, None)?;
                    // If either primitive is a string, perform string concatenation
                    if matches!(a_prim, Value::String(_)) || matches!(b_prim, Value::String(_)) {
                        let sa = self.to_string(&a_prim)?;
                        let sb = self.to_string(&b_prim)?;
                        self.stack.push(Value::String(format!("{}{}", sa, sb)));
                    } else {
                        let na = self.to_number_value(&a_prim)?;
                        let nb = self.to_number_value(&b_prim)?;
                        self.stack.push(Value::Number(na + nb));
                    }
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
            Op::Div => self.binary_num_bigint(
                |a, b| a / b,
                |_, _| {
                    Err(crate::errors::RuntimeError::from(
                        "Div: bigint not supported",
                    ))
                },
            )?,
            Op::Mod => self.binary_num_bigint(|a, b| a % b, |a, b| Ok(a % b))?,
            Op::Pow => {
                // Exponentiation: handle BigInt**BigInt and Number**Number semantics.
                // Pop right then left.
                let b = self.pop()?;
                let a = self.pop()?;
                // Helper: ToNumeric approximation returning either Number(f64) or BigInt(i128)
                enum Numeric {
                    Number(f64),
                    BigInt(i128),
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
                                Value::String(s) => {
                                    let parsed = s.parse::<f64>().unwrap_or(f64::NAN);
                                    Ok(Numeric::Number(parsed))
                                }
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
                        if exp < 0 {
                            let ctor = self.get_global("RangeError").unwrap_or(Value::Undefined);
                            let thrown = self
                                .construct_value(&ctor, &[])
                                .unwrap_or(Value::Object(Rc::new(RefCell::new(ObjectData::new()))));
                            match self.unwind_to_handler(thrown) {
                                Ok(()) => return Ok(StepOutcome::Continue),
                                Err(err) => return Err(err),
                            }
                        }
                        // Exponentiation by repeated squaring for i128 (may overflow)
                        let mut base = x;
                        let mut exp_mut = exp;
                        let mut result: i128 = 1;
                        while exp_mut > 0 {
                            if (exp_mut & 1) != 0 {
                                result = result.checked_mul(base).ok_or_else(|| {
                                    crate::errors::RuntimeError::from("Pow: bigint overflow")
                                })?;
                            }
                            exp_mut >>= 1;
                            if exp_mut > 0 {
                                base = base.checked_mul(base).ok_or_else(|| {
                                    crate::errors::RuntimeError::from("Pow: bigint overflow")
                                })?;
                            }
                        }
                        self.stack.push(Value::BigInt(result));
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
                self.stack.push(Value::Bool(values_equal(&a, &b)));
            }
            Op::Neq => {
                let b = self.pop()?;
                let a = self.pop()?;
                self.stack.push(Value::Bool(!values_equal(&a, &b)));
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
            Op::Return => {
                if self.frames.is_empty() {
                    let result = self.stack.pop().unwrap_or(Value::Undefined);
                    return Ok(StepOutcome::Return(result));
                }
                self.execute_return()?;
                if stop_on_return {
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
                let result = value.clone();
                self.set_property_tracked(&obj, &k, value)?;
                self.stack.push(result);
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
                self.unwind_to_handler(thrown)?;
            }
        }
        Ok(StepOutcome::Continue)
    }

    // -----------------------------------------------------------------------
    // ECMAScript coercions
    // -----------------------------------------------------------------------

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
                // Try @@toPrimitive if present.
                let to_prim_key =
                    PropertyKey::Symbol(crate::value::SymbolId::well_known_to_primitive());
                if let Ok(method) = get_property(v, &to_prim_key, 0)
                    && matches!(
                        method,
                        Value::Closure { .. } | Value::Builtin(_) | Value::BuiltinObject(_, _)
                    )
                    && let Ok(rv) = self.call_with_this(
                        &method,
                        v,
                        &[Value::String(hint.unwrap_or("default").to_string())],
                    )
                    && !matches!(
                        rv,
                        Value::Object(_)
                            | Value::Array(..)
                            | Value::Closure { .. }
                            | Value::Builtin(_)
                            | Value::BuiltinObject(_, _)
                    )
                {
                    return Ok(rv);
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
                        && let Ok(rv) = self.call_with_this(&method, v, &[])
                        && !matches!(
                            rv,
                            Value::Object(_)
                                | Value::Array(..)
                                | Value::Closure { .. }
                                | Value::Builtin(_)
                                | Value::BuiltinObject(_, _)
                        )
                    {
                        return Ok(rv);
                    }
                }
                // TODO: when Object.prototype.toString/valueOf are wired into
                // the runtime object model, this should throw TypeError instead.
                Ok(Value::String("[object Object]".to_string()))
            }
            _ => Ok(v.clone()),
        }
    }

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
            Value::Symbol(_, _) => Err(crate::errors::RuntimeError::from(
                "Cannot convert Symbol to string",
            )),
            _ => {
                let prim = self.to_primitive(v, Some("string"))?;
                self.to_string(&prim)
            }
        }
    }

    pub fn to_number_value(&mut self, v: &Value) -> Result<f64, crate::errors::RuntimeError> {
        match v {
            Value::Number(n) => Ok(*n),
            Value::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
            Value::String(s) => Ok(s.parse::<f64>().unwrap_or(f64::NAN)),
            Value::Undefined => Ok(f64::NAN),
            Value::Null => Ok(0.0),
            Value::BigInt(n) => Ok(*n as f64),
            Value::Symbol(_, _) => Err(crate::errors::RuntimeError::from(
                "Cannot convert Symbol to number",
            )),
            _ => {
                let prim = self.to_primitive(v, Some("number"))?;
                self.to_number_value(&prim)
            }
        }
    }

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
                    return Ok(PropertyKey::Symbol(crate::value::SymbolId::well_known(
                        *n as u64,
                    )));
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

    pub fn to_boolean(&self, v: &Value) -> bool {
        v.is_truthy()
    }

    /// Kernel helper: abstract equality comparison (==). Exposed so the
    /// soft layer can delegate equality semantics to the kernel during
    /// migration.
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

        // Object compared with String/Number: ToPrimitive on object
        if (matches!(x, Value::String(_)) || matches!(x, Value::Number(_)))
            && matches!(
                y,
                Value::Object(_)
                    | Value::Array(..)
                    | Value::Closure { .. }
                    | Value::Builtin(_)
                    | Value::BuiltinObject(_, _)
            )
        {
            let yprim = self.to_primitive(y, None)?;
            return self.abstract_equality(x, &yprim);
        }
        if (matches!(y, Value::String(_)) || matches!(y, Value::Number(_)))
            && matches!(
                x,
                Value::Object(_)
                    | Value::Array(..)
                    | Value::Closure { .. }
                    | Value::Builtin(_)
                    | Value::BuiltinObject(_, _)
            )
        {
            let xprim = self.to_primitive(x, None)?;
            return self.abstract_equality(&xprim, y);
        }

        Ok(false)
    }

    fn read_descriptor_from_value(
        &mut self,
        v: &Value,
    ) -> Result<PartialDescriptor, crate::errors::RuntimeError> {
        let mut desc = PartialDescriptor::default();
        if let Value::Object(data) = v {
            let d = data.borrow();
            if let Some(PropertyDescriptor::Data { value, .. }) =
                d.properties.get(&PropertyKey::String("value".to_string()))
            {
                desc.value = Some(value.clone());
            }
            if let Some(PropertyDescriptor::Data { value, .. }) = d
                .properties
                .get(&PropertyKey::String("writable".to_string()))
            {
                desc.writable = Some(self.to_boolean(value));
            }
            if let Some(PropertyDescriptor::Data { value, .. }) = d
                .properties
                .get(&PropertyKey::String("enumerable".to_string()))
            {
                desc.enumerable = Some(self.to_boolean(value));
            }
            if let Some(PropertyDescriptor::Data { value, .. }) = d
                .properties
                .get(&PropertyKey::String("configurable".to_string()))
            {
                desc.configurable = Some(self.to_boolean(value));
            }
            if let Some(PropertyDescriptor::Data { value, .. }) =
                d.properties.get(&PropertyKey::String("get".to_string()))
            {
                desc.get = Some(value.clone());
            }
            if let Some(PropertyDescriptor::Data { value, .. }) =
                d.properties.get(&PropertyKey::String("set".to_string()))
            {
                desc.set = Some(value.clone());
            }
        }
        Ok(desc)
    }

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

    fn get_property_value(
        &mut self,
        obj: &Value,
        key: &PropertyKey,
    ) -> Result<Value, crate::errors::RuntimeError> {
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
                    && let Some(v) = arr.borrow().get(i).cloned()
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
            _ => {}
        }
        Ok(None)
    }

    /// Pop the return value, restore the caller's frame, and push the result.
    fn execute_return(&mut self) -> Result<(), crate::errors::RuntimeError> {
        let frame = self.frames.pop().ok_or("Return without call frame")?;
        // Only consume a return value produced by the callee itself. If the
        // callee leaves no value above its stack base, the return value is
        // `undefined` and we must not pop caller operands.
        let result = if self.stack.len() > frame.stack_base {
            self.stack.pop().unwrap_or(Value::Undefined)
        } else {
            Value::Undefined
        };

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
        self.locals = frame.locals;
        self.this_value = frame.this_value;
        self.current_closure_env = frame.closure_env;

        // FIX: truncate the operand stack back to the caller's base, then
        // push the return value. Previously the caller's stack was lost.
        self.stack.truncate(frame.stack_base);
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
        proto: &crate::bytecode::FunctionProto,
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

        let rest_slice: Vec<Value> = if args.len() > fixed_count {
            args[fixed_count..].to_vec()
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
            if let Ok(for_each_val) = get_property(
                &props.borrow().prototype,
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
        Ok(fixed)
    }

    fn validate_call_args(
        &self,
        _call_args: &[Value],
        _proto: &crate::bytecode::FunctionProto,
    ) -> Result<(), crate::errors::RuntimeError> {
        // Argument type validation is intentionally disabled in JS-first mode.
        // TypeScript annotations are treated as comments and should not affect
        // runtime behaviour. Keep this function for tooling and future opt-in
        // checked modes.
        Ok(())
    }

    /// Save the caller's state and set up the callee.
    fn push_call_frame(
        &mut self,
        proto: &Rc<crate::bytecode::FunctionProto>,
        closure_env: &Rc<RefCell<Vec<Rc<RefCell<Value>>>>>,
        call_args: Vec<Value>,
        this_value: Value,
    ) -> Result<(), crate::errors::RuntimeError> {
        // FIX: record the current stack length as the base. We no longer
        // clone the entire operand stack into the frame.
        let stack_base = self.stack.len();

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
            locals: std::mem::replace(
                &mut self.locals,
                Vec::with_capacity(LOCALS_INITIAL_CAPACITY),
            ),
            this_value: std::mem::replace(&mut self.this_value, this_value),
            closure_env: self.current_closure_env.take(),
            slot_types: self.current_slot_types.take(),
            stack_base,
        });

        self.chunk = Rc::clone(&proto.chunk);
        self.ip = 0;
        self.current_closure_env = Some(Rc::clone(closure_env));
        self.current_slot_types = Some(proto.slot_types.clone());

        // Pre-allocate locals to at least `arity` slots, filling missing
        // arguments with `undefined` per JavaScript semantics.
        let args_for_arguments = call_args.clone();
        self.locals = std::iter::repeat_with(|| Rc::new(RefCell::new(Value::Undefined)))
            .take(proto.slot_types.len().max(proto.arity))
            .collect();
        for (i, value) in call_args.into_iter().enumerate().take(proto.arity) {
            *self.locals[i].borrow_mut() = value;
        }
        if let Some(slot) = proto.arguments_slot
            && slot < self.locals.len()
        {
            let arr_rc = Rc::new(RefCell::new(args_for_arguments));
            let props: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(ObjectData::new()));
            props.borrow_mut().properties.insert(
                PropertyKey::String("length".to_string()),
                PropertyDescriptor::data(Value::Number(arr_rc.borrow().len() as f64)),
            );
            self.locals[slot] = Rc::new(RefCell::new(Value::Array(arr_rc, props)));
        }

        Ok(())
    }

    fn build_array(&mut self) -> Result<(), crate::errors::RuntimeError> {
        let n = self.read_byte()? as usize;
        let mut elems: Vec<Value> = Vec::with_capacity(n);
        for _ in 0..n {
            elems.push(self.pop()?);
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

        self.stack.push(Value::Array(arr_rc, props));
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
        self.stack.push(Value::Object(Rc::new(RefCell::new(data))));
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

        if has_prototype_property {
            let proto_obj_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(ObjectData::new()));
            let proto_obj = Value::Object(Rc::clone(&proto_obj_map));
            props.borrow_mut().properties.insert(
                PropertyKey::String("prototype".to_string()),
                PropertyDescriptor::data(proto_obj),
            );
            proto_obj_map.borrow_mut().properties.insert(
                PropertyKey::String("constructor".to_string()),
                PropertyDescriptor::data(closure.clone()),
            );
        }

        self.stack.push(closure);
        Ok(())
    }

    /// Inline-cached property access with generation-based invalidation.
    fn get_property_ic(&mut self) -> Result<(), crate::errors::RuntimeError> {
        let slot = self.read_byte()? as usize;
        let key_val = self.pop()?;
        let obj = self.pop()?;
        let key = self.to_property_key(&key_val)?;

        let chunk_id = Rc::as_ptr(&self.chunk) as usize;

        if let Some(obj_ptr) = object_identity(&obj) {
            let current_gen = self.obj_generations.get(&obj_ptr).copied().unwrap_or(0);

            let hit = self
                .ic_caches
                .get(&chunk_id)
                .and_then(|vec| vec.get(slot))
                .and_then(|entry| entry.as_ref())
                .filter(|e| e.obj_ptr == obj_ptr && e.generation == current_gen && e.key == key)
                .map(|e| e.value.clone());

            if let Some(cached) = hit {
                self.stack.push(cached);
                return Ok(());
            }

            let v = self.get_property_value(&obj, &key)?;
            let cache_vec = self
                .ic_caches
                .entry(chunk_id)
                .or_insert_with(|| vec![None; IC_SLOTS_DEFAULT]);
            if slot >= cache_vec.len() {
                cache_vec.resize(slot + 1, None);
            }
            cache_vec[slot] = Some(IcEntry {
                obj_ptr,
                key: key.clone(),
                generation: current_gen,
                value: v.clone(),
            });
            self.stack.push(v);
        } else {
            let v = self.get_property_value(&obj, &key)?;
            self.stack.push(v);
        }

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

        // Bump generation for the mutated object.
        let ptr = match obj {
            Value::Object(m) => Some(Rc::as_ptr(m) as usize),
            Value::Array(a, _) => Some(Rc::as_ptr(a) as usize),
            Value::Closure { props, .. } => Some(Rc::as_ptr(props) as usize),
            Value::BuiltinObject(_, props) => Some(Rc::as_ptr(props) as usize),
            _ => None,
        };
        if let Some(p) = ptr {
            *self.obj_generations.entry(p).or_insert(0) += 1;
        }

        Ok(())
    }

    /// Unwind the call stack and operand stack to the nearest exception
    /// handler, or propagate the error if none exists.
    fn unwind_to_handler(&mut self, thrown: Value) -> Result<(), crate::errors::RuntimeError> {
        let handler = match self.handlers.pop() {
            Some(h) => h,
            None => {
                // No handler: print the thrown value for diagnostics then
                // propagate an error. Tests typically throw Test262Error
                // instances (objects), so surface their contents to help
                // debugging.
                eprintln!("[unwind] uncaught thrown value: {:?}", thrown);
                // Also print value string via trace helper when available.
                if crate::trace::enabled() {
                    crate::trace::value_string("[unwind] uncaught thrown (string) ", &thrown);
                }
                return Err(match thrown {
                    Value::String(s) => {
                        crate::errors::RuntimeError::from(format!("Uncaught: {}", s))
                    }
                    other => {
                        if let Ok(Value::String(msg)) =
                            self.get_property_value(&other, &PropertyKey::String("message".to_string()))
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
                self.locals = frame.locals;
                self.this_value = frame.this_value;
                self.current_closure_env = frame.closure_env;
                self.current_slot_types = frame.slot_types;
            }
        }

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
    fn binary_num<F>(&mut self, f: F) -> Result<(), crate::errors::RuntimeError>
    where
        F: FnOnce(f64, f64) -> f64,
    {
        let b = self.pop()?;
        let a = self.pop()?;
        let (x, y) = (to_number(&a)?, to_number(&b)?);
        self.stack.push(Value::Number(f(x, y)));
        Ok(())
    }

    #[inline(always)]
    fn binary_num_bigint<F, G>(&mut self, f: F, g: G) -> Result<(), crate::errors::RuntimeError>
    where
        F: FnOnce(f64, f64) -> f64,
        G: FnOnce(i128, i128) -> Result<i128, crate::errors::RuntimeError>,
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
        for (idx, v) in self.stack.iter().rev().take(8).enumerate() {
            crate::trace::value_string(&format!("[diag]   stack[{}]", idx), v);
        }

        let start = self.ip.saturating_sub(8);
        let end = usize::min(self.ip + 8, self.chunk.code.len());
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
                        let repr = match self.chunk.constants.get(arg as usize) {
                            Some(Value::String(s)) => {
                                format!("'{}'", s)
                            }
                            Some(v) => v.type_name().to_string(),
                            None => "<undef>".to_string(),
                        };
                        eprintln!("[diag] {:#06}  Const       {} ({})", j, arg, repr);
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
    }

    /// Instanceof helper: checks whether `value` has `constructor.prototype` in its prototype chain.
    pub fn instance_of(
        &self,
        value: &Value,
        constructor: &Value,
    ) -> Result<bool, crate::errors::RuntimeError> {
        // Only objects have prototype chains.
        match value {
            Value::Object(_)
            | Value::Array(..)
            | Value::Closure { .. }
            | Value::BuiltinObject(_, _) => {}
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

    /// 'in' operator helper. Returns true if property (after ToPropertyKey) is in object.
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
        Value::Array(arr, _) => Some(Rc::as_ptr(arr) as usize),
        Value::Closure { props, .. } => Some(Rc::as_ptr(props) as usize),
        Value::BuiltinObject(_, props) => Some(Rc::as_ptr(props) as usize),
        _ => None,
    }
}

fn to_number(v: &Value) -> Result<f64, crate::errors::RuntimeError> {
    match v {
        Value::Number(n) => Ok(*n),
        Value::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
        Value::String(s) => Ok(s.parse::<f64>().unwrap_or(f64::NAN)),
        Value::Undefined => Ok(f64::NAN),
        Value::Null => Ok(0.0),
        Value::BigInt(n) => Ok(*n as f64),
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
        Value::BigInt(x) => *x as f64,
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

// Helper methods on Vm for coercion that delegate to free functions but
// provide clearer naming when used in opcode handlers.
impl Vm {
    pub fn to_int32_value(&self, v: &Value) -> Result<i32, crate::errors::RuntimeError> {
        to_int32(v)
    }

    pub fn to_uint32_value(&self, v: &Value) -> Result<u32, crate::errors::RuntimeError> {
        to_uint32(v)
    }
}

fn value_to_type(v: &Value) -> Type {
    match v {
        Value::Undefined => Type::Builtin(BuiltinType::Undefined),
        Value::Number(_) => Type::Number,
        Value::String(_) => Type::String,
        Value::Bool(_) => Type::Boolean,
        Value::BigInt(_) => Type::Builtin(BuiltinType::BigInt),
        Value::Null => Type::Null,
        Value::Object(_) => Type::Object(HashMap::new()),
        Value::Array(_, _) => Type::Array(Box::new(Type::Void)),
        Value::Closure { .. } | Value::Builtin(_) | Value::BuiltinObject(_, _) => {
            Type::Function(vec![], Box::new(Type::Void))
        }
        Value::Symbol(_, _) => Type::Builtin(BuiltinType::Symbol),
    }
}

/// Check if `actual` is assignable to `expected` at runtime.
///
/// For `Type::Union` we now check each variant instead of blindly accepting.
fn runtime_assignable(expected: &Type, actual: &Value) -> bool {
    match expected {
        Type::Number => matches!(actual, Value::Number(_)),
        Type::String => matches!(actual, Value::String(_)),
        Type::Boolean => matches!(actual, Value::Bool(_)),
        Type::Null => matches!(actual, Value::Null),
        Type::Void => true,
        Type::Any => true,
        Type::Unknown => true,
        Type::Array(_) => matches!(actual, Value::Array(_, _)),
        Type::Object(_) => matches!(actual, Value::Object(_)),
        Type::Function(_, _) => matches!(
            actual,
            Value::Closure { .. } | Value::Builtin(_) | Value::BuiltinObject(_, _)
        ),
        Type::Union(variants) => variants.iter().any(|t| runtime_assignable(t, actual)),
        Type::Builtin(_) | Type::TypeReference { .. } => true,
    }
}

fn check_assign(expected: &Type, actual: &Value) -> Result<(), crate::errors::RuntimeError> {
    if runtime_assignable(expected, actual) {
        Ok(())
    } else {
        Err(crate::errors::RuntimeError::from(format!(
            "Runtime type error: expected {}, got {}",
            crate::typecheck::type_to_string(expected),
            crate::typecheck::type_to_string(&value_to_type(actual))
        )))
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
                && let Some(value) = arr.borrow().get(i).cloned()
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
                        a.resize(i + 1, Value::Undefined);
                    }
                    a[i] = value;
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
    Value::Object(namespace)
}

impl PartialOrd for Value {
    fn partial_cmp(&self, other: &Value) -> Option<std::cmp::Ordering> {
        match (self, other) {
            (Value::Number(a), Value::Number(b)) => a.partial_cmp(b),
            (Value::String(a), Value::String(b)) => a.partial_cmp(b),
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
        .unwrap();

        let stored = props
            .borrow()
            .properties
            .get(&PropertyKey::String("prototype".to_string()))
            .cloned();
        assert!(matches!(
            stored,
            Some(PropertyDescriptor::Data {
                value: Value::Number(11.0),
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
    fn array_max_u32_key_is_not_dense_index() {
        let arr_data = Rc::new(RefCell::new(vec![Value::Number(0.0)]));
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
        let arr_data = Rc::new(RefCell::new(vec![Value::Number(0.0)]));
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
        let chunk = compiler::compile(program).expect("compile ok");

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
        let chunk = compiler::compile(program).expect("compile ok");

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
        let chunk = compiler::compile(program).expect("compile ok");

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
        let chunk = compiler::compile(program).expect("compile ok");

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
        let chunk = compiler::compile(program).expect("compile ok");

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
}
