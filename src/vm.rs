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
//! use std::collections::HashMap;
//! use std::rc::Rc;
//!
//! let globals = Rc::new(RefCell::new(HashMap::new()));
//! let mut vm = Vm::new(globals);
//! // vm.run(chunk)?;
//! ```

use crate::bytecode::{Chunk, Op};
use crate::typecheck::Type;
use crate::value::Value;
use std::cell::RefCell;
use std::collections::HashMap;
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

// ---------------------------------------------------------------------------
// Inline-cache entry
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct IcEntry {
    /// Raw `Rc` pointer used as an object identity token.
    obj_ptr: usize,
    /// Object generation at the time the entry was cached. If the object
    /// has been mutated since, the generation will have advanced and this
    /// entry is stale.
    generation: u64,
    /// Cached property value.
    value: Value,
}

// ---------------------------------------------------------------------------
// Call frame
// ---------------------------------------------------------------------------

struct Frame {
    chunk: Rc<Chunk>,
    ip: usize,
    locals: Vec<Value>,
    slot_types: Option<Vec<Option<Type>>>,
    return_type: Option<Type>,
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
    locals: Vec<Value>,
    /// Global variables.
    globals: Rc<RefCell<HashMap<String, Value>>>,
    /// Call-frame stack.
    frames: Vec<Frame>,
    /// Root chunk (kept alive for global type metadata).
    root_chunk: Option<Rc<Chunk>>,
    /// Slot-type annotations for the current function (None at top level).
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
}

impl Vm {
    // -----------------------------------------------------------------------
    // Construction & public helpers
    // -----------------------------------------------------------------------

    /// Create a new virtual machine.
    pub fn new(globals: Rc<RefCell<HashMap<String, Value>>>) -> Self {
        Self {
            chunk: Rc::new(Chunk::new()),
            ip: 0,
            stack: Vec::with_capacity(STACK_INITIAL_CAPACITY),
            locals: Vec::with_capacity(LOCALS_INITIAL_CAPACITY),
            globals,
            frames: Vec::with_capacity(FRAMES_INITIAL_CAPACITY),
            root_chunk: None,
            current_slot_types: None,
            ic_caches: HashMap::new(),
            obj_generations: HashMap::new(),
            bg_runtime: get_global_bg_runtime(),
            handlers: Vec::new(),
        }
    }

    /// Debug helper: get a global value by name (cloned).
    pub fn get_global(&self, name: &str) -> Option<Value> {
        self.globals.borrow().get(name).cloned()
    }

    /// Return own keys for a global value if it is an object or closure.
    pub fn list_global_own_keys(&self, name: &str) -> Option<Vec<String>> {
        let g = self.globals.borrow();
        let v = g.get(name)?;
        list_own_keys_of_value(v)
    }

    /// Return prototype keys for a global's prototype property.
    pub fn list_global_prototype_keys(&self, name: &str) -> Option<Vec<String>> {
        let g = self.globals.borrow();
        let v = g.get(name)?;
        match get_property(v, "prototype", 0) {
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
    pub fn reset(&mut self, keep_globals: &std::collections::HashSet<String>) {
        self.stack.clear();
        self.locals.clear();
        self.frames.clear();
        self.handlers.clear();
        self.root_chunk = None;
        self.current_slot_types = None;
        self.ip = 0;
        self.chunk = Rc::new(Chunk::new());
        self.ic_caches.clear();
        self.obj_generations.clear();
        let mut g = self.globals.borrow_mut();
        g.retain(|k, _| keep_globals.contains(k));
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
        self.locals.clear();
        self.frames.clear();
        self.handlers.clear();
        self.current_slot_types = None;

        loop {
            // In debug builds every byte read is bounds-checked.  In release
            // builds we can elide the check because the compiler/verifier
            // guarantees the bytecode stream is well-formed.
            let op_byte = self.read_byte()?;
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
                // -----------------------------------------------------------
                // Constants & literals
                // -----------------------------------------------------------
                Op::Const => {
                    let idx = self.read_byte()? as usize;
                    let v = self.constant(idx)?;
                    self.stack.push(v);
                }
                Op::Null => self.stack.push(Value::Null),
                Op::True => self.stack.push(Value::Bool(true)),
                Op::False => self.stack.push(Value::Bool(false)),

                // -----------------------------------------------------------
                // Local variables
                // -----------------------------------------------------------
                Op::GetLocal => {
                    let slot = self.read_byte()? as usize;
                    let v = self.locals.get(slot).cloned().ok_or("Bad local slot")?;
                    self.stack.push(v);
                }
                Op::SetLocal => {
                    let slot = self.read_byte()? as usize;
                    let v = self.pop()?;
                    if let Some(Some(expected)) =
                        self.current_slot_types.as_ref().and_then(|t| t.get(slot))
                    {
                        check_assign(expected, &v)?;
                    }
                    ensure_slot(&mut self.locals, slot);
                    self.locals[slot] = v;
                }

                // -----------------------------------------------------------
                // Global variables
                // -----------------------------------------------------------
                Op::GetGlobal => {
                    let idx = self.read_byte()? as usize;
                    let name = self.string_constant(idx)?;
                    let v = self
                        .globals
                        .borrow()
                        .get(&name)
                        .cloned()
                        .unwrap_or(Value::Null);
                    self.stack.push(v);
                }
                Op::SetGlobal => {
                    let idx = self.read_byte()? as usize;
                    let name = self.string_constant(idx)?;
                    let v = self.pop()?;
                    if let Some(Some(expected)) = self
                        .root_chunk
                        .as_ref()
                        .and_then(|r| r.global_types.get(&name))
                    {
                        check_assign(expected, &v)?;
                    }
                    self.globals.borrow_mut().insert(name, v);
                }

                // -----------------------------------------------------------
                // Stack manipulation
                // -----------------------------------------------------------
                Op::Pop => {
                    self.pop()?;
                }
                Op::Dup => {
                    let v = self.stack.last().cloned().ok_or_else(|| {
                        if crate::trace::enabled() {
                            eprintln!("[diag] Stack underflow (Dup)");
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
                                            let arg =
                                                self.chunk.code.get(j + 1).cloned().unwrap_or(0);
                                            if let Some(c) = self.chunk.constants.get(arg as usize)
                                            {
                                                eprintln!("[diag]   {}: {:?} {:?}", j, op, c);
                                            } else {
                                                eprintln!("[diag]   {}: {:?} {}", j, op, arg);
                                            }
                                        }
                                        Op::Jump | Op::JumpIfFalse | Op::PushHandler => {
                                            let hi =
                                                self.chunk.code.get(j + 1).cloned().unwrap_or(0);
                                            let lo =
                                                self.chunk.code.get(j + 2).cloned().unwrap_or(0);
                                            let offset = ((hi as i16) << 8) | (lo as i16);
                                            eprintln!("[diag]   {}: {:?} {}", j, op, offset);
                                        }
                                        Op::SetLocal
                                        | Op::GetLocal
                                        | Op::SetGlobal
                                        | Op::GetGlobal
                                        | Op::Call
                                        | Op::GetBuiltin
                                        | Op::GetProperty
                                        | Op::SetProperty
                                        | Op::GetPropertyIC
                                        | Op::MakeClosure => {
                                            let arg =
                                                self.chunk.code.get(j + 1).cloned().unwrap_or(0);
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
                        "Stack underflow"
                    })?;
                    self.stack.push(v);
                }

                // -----------------------------------------------------------
                // Arithmetic
                // -----------------------------------------------------------
                Op::Add => {
                    let b = self.pop()?;
                    let a = self.pop()?;
                    let result = match (a, b) {
                        (Value::Number(x), Value::Number(y)) => Value::Number(x + y),
                        (Value::String(s), v) => Value::String(format!("{}{}", s, v)),
                        (v, Value::String(s)) => Value::String(format!("{}{}", v, s)),
                        _ => return Err("Add: number or string expected".into()),
                    };
                    self.stack.push(result);
                }
                Op::AddNumber => {
                    let b = self.pop()?;
                    let a = self.pop()?;
                    match (a, b) {
                        (Value::Number(x), Value::Number(y)) => {
                            self.stack.push(Value::Number(x + y));
                        }
                        _ => return Err("AddNumber: number expected".into()),
                    }
                }
                Op::Sub => self.binary_num(|a, b| a - b)?,
                Op::Mul => self.binary_num(|a, b| a * b)?,
                Op::Div => self.binary_num(|a, b| a / b)?,
                Op::Mod => self.binary_num(|a, b| a % b)?,

                // -----------------------------------------------------------
                // Comparison -- uses JS-style reference equality for objects
                // -----------------------------------------------------------
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

                // -----------------------------------------------------------
                // Logical
                // -----------------------------------------------------------
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

                // -----------------------------------------------------------
                // Unary
                // -----------------------------------------------------------
                Op::Neg => {
                    let v = self.pop()?;
                    match v {
                        Value::Number(n) => self.stack.push(Value::Number(-n)),
                        _ => return Err("Neg: number expected".into()),
                    }
                }
                Op::Not => {
                    let v = self.pop()?;
                    self.stack.push(Value::Bool(!v.is_truthy()));
                }
                Op::Typeof => {
                    let v = self.pop()?;
                    self.stack.push(Value::String(v.type_name().to_string()));
                }

                // -----------------------------------------------------------
                // Calls & returns
                // -----------------------------------------------------------
                Op::Call => self.execute_call()?,
                Op::Return => {
                    if self.frames.is_empty() {
                        let result = self.pop().unwrap_or(Value::Null);
                        return Ok(result);
                    }
                    self.execute_return()?;
                }

                // -----------------------------------------------------------
                // Control flow
                // -----------------------------------------------------------
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

                // -----------------------------------------------------------
                // Composite construction
                // -----------------------------------------------------------
                Op::BuildArray => self.build_array()?,
                Op::BuildObject => self.build_object()?,

                // -----------------------------------------------------------
                // Property access
                // -----------------------------------------------------------
                Op::GetProperty => {
                    let key = self.pop()?;
                    let obj = self.pop()?;
                    let k = property_key(&key)?;
                    let v = get_property(&obj, &k, 0)?;
                    self.stack.push(v);
                }
                Op::GetPropertyIC => self.get_property_ic()?,
                Op::SetProperty => {
                    let value = self.pop()?;
                    let key = self.pop()?;
                    let obj = self.pop()?;
                    let k = property_key(&key)?;
                    self.set_property_tracked(&obj, &k, value)?;
                }

                // -----------------------------------------------------------
                // Closures
                // -----------------------------------------------------------
                Op::MakeClosure => self.make_closure()?,

                // -----------------------------------------------------------
                // Builtins
                // -----------------------------------------------------------
                Op::GetBuiltin => {
                    let idx = self.read_byte()? as usize;
                    let name = self.string_constant(idx)?;
                    let g = self.globals.borrow();
                    let v = g.get(&name).cloned().ok_or_else(|| {
                        crate::errors::RuntimeError::from(format!(
                            "GetBuiltin: '{}' not found",
                            name
                        ))
                    })?;
                    match v {
                        Value::Builtin(_) => self.stack.push(v),
                        _ => {
                            return Err(crate::errors::RuntimeError::from(format!(
                                "GetBuiltin: '{}' is not a builtin",
                                name
                            )));
                        }
                    }
                }

                // -----------------------------------------------------------
                // Exception handling
                // -----------------------------------------------------------
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
                }
                Op::PopHandler => {
                    self.handlers.pop();
                }
                Op::Throw => {
                    let thrown = self.pop()?;
                    self.unwind_to_handler(thrown)?;
                }
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
                let result = f(self, &args)?;
                self.stack.push(result);
            }
            Value::Closure { proto, .. } => {
                let call_args = self.prepare_call_args(&args, &proto)?;
                self.validate_call_args(&call_args, &proto)?;
                self.push_call_frame(&proto, call_args)?;
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

    /// Pop the return value, restore the caller's frame, and push the result.
    fn execute_return(&mut self) -> Result<(), crate::errors::RuntimeError> {
        let result = self.pop().unwrap_or(Value::Null);

        let frame = self.frames.pop().ok_or("Return without call frame")?;

        // Runtime return-type check.
        if let Some(ref expected) = frame.return_type {
            check_assign(expected, &result)?;
        }

        // Restore caller state.
        self.current_slot_types = frame.slot_types;
        self.chunk = frame.chunk;
        self.ip = frame.ip;
        self.locals = frame.locals;

        // FIX: truncate the operand stack back to the caller's base, then
        // push the return value. Previously the caller's stack was lost.
        self.stack.truncate(frame.stack_base);
        self.stack.push(result);

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
            fixed.push(args.get(i).cloned().unwrap_or(Value::Null));
        }

        let rest_slice: Vec<Value> = if args.len() > fixed_count {
            args[fixed_count..].to_vec()
        } else {
            Vec::new()
        };

        let arr_rc = Rc::new(RefCell::new(rest_slice));
        let props = Rc::new(RefCell::new(HashMap::new()));
        props.borrow_mut().insert(
            "length".to_string(),
            Value::Number(arr_rc.borrow().len() as f64),
        );

        if let Some(array_val) = self.globals.borrow().get("Array")
            && let Ok(proto_val) = get_property(array_val, "prototype", 0)
            && !matches!(proto_val, Value::Null)
        {
            props
                .borrow_mut()
                .insert("__proto__".to_string(), proto_val);
        }

        fixed.push(Value::Array(arr_rc, props));
        Ok(fixed)
    }

    fn validate_call_args(
        &self,
        call_args: &[Value],
        proto: &crate::bytecode::FunctionProto,
    ) -> Result<(), crate::errors::RuntimeError> {
        for (i, (arg, expected_opt)) in call_args
            .iter()
            .zip(proto.slot_types.iter().take(proto.arity))
            .enumerate()
        {
            if let Some(expected) = expected_opt {
                if *expected == Type::Void {
                    continue;
                }
                check_assign(expected, arg).map_err(|e| {
                    crate::errors::RuntimeError::from(format!("Argument {}: {}", i + 1, e))
                })?;
            }
        }
        Ok(())
    }

    /// Save the caller's state and set up the callee.
    fn push_call_frame(
        &mut self,
        proto: &Rc<crate::bytecode::FunctionProto>,
        call_args: Vec<Value>,
    ) -> Result<(), crate::errors::RuntimeError> {
        // FIX: record the current stack length as the base. We no longer
        // clone the entire operand stack into the frame.
        let stack_base = self.stack.len();

        self.frames.push(Frame {
            chunk: Rc::clone(&self.chunk),
            ip: self.ip,
            locals: std::mem::replace(
                &mut self.locals,
                Vec::with_capacity(LOCALS_INITIAL_CAPACITY),
            ),
            slot_types: self.current_slot_types.take(),
            return_type: proto.return_type.clone(),
            stack_base,
        });

        self.chunk = Rc::clone(&proto.chunk);
        self.ip = 0;
        self.current_slot_types = Some(proto.slot_types.clone());

        // Pre-allocate locals to at least `arity` slots, filling missing
        // arguments with Null.
        self.locals = call_args
            .into_iter()
            .chain(std::iter::repeat(Value::Null))
            .take(proto.arity)
            .collect();

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
        let props = Rc::new(RefCell::new(HashMap::new()));
        props
            .borrow_mut()
            .insert("length".to_string(), Value::Number(n as f64));

        if let Some(array_val) = self.globals.borrow().get("Array")
            && let Ok(proto_val) = get_property(array_val, "prototype", 0)
            && !matches!(proto_val, Value::Null)
        {
            props
                .borrow_mut()
                .insert("__proto__".to_string(), proto_val);
        }

        self.stack.push(Value::Array(arr_rc, props));
        Ok(())
    }

    fn build_object(&mut self) -> Result<(), crate::errors::RuntimeError> {
        let n = self.read_byte()? as usize;
        let mut map = HashMap::with_capacity(n);
        for _ in 0..n {
            let value = self.pop()?;
            let key_val = self.pop()?;
            let key = match key_val {
                Value::String(s) => s,
                other => {
                    self.dump_diag("BuildObject: key must be string", &other);
                    return Err(crate::errors::RuntimeError::from(format!(
                        "BuildObject: key must be string (actual={})",
                        other.type_name()
                    )));
                }
            };
            map.insert(key, value);
        }
        self.stack.push(Value::Object(Rc::new(RefCell::new(map))));
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

        let proto_obj_map = Rc::new(RefCell::new(HashMap::new()));
        let proto_obj = Value::Object(Rc::clone(&proto_obj_map));
        let props = Rc::new(RefCell::new(HashMap::new()));
        props
            .borrow_mut()
            .insert("prototype".to_string(), proto_obj);

        let closure = Value::Closure {
            proto: Rc::new(proto),
            env: Rc::new(RefCell::new(Vec::new())),
            props: Rc::clone(&props),
        };

        proto_obj_map
            .borrow_mut()
            .insert("constructor".to_string(), closure.clone());

        self.stack.push(closure);
        Ok(())
    }

    /// Inline-cached property access with generation-based invalidation.
    fn get_property_ic(&mut self) -> Result<(), crate::errors::RuntimeError> {
        let slot = self.read_byte()? as usize;
        let key_val = self.pop()?;
        let obj = self.pop()?;
        let key = property_key(&key_val)?;

        let chunk_id = Rc::as_ptr(&self.chunk) as usize;

        // Attempt a cache hit for Object values.
        if let Value::Object(ref map_rc) = obj {
            let obj_ptr = Rc::as_ptr(map_rc) as usize;
            let current_gen = self.obj_generations.get(&obj_ptr).copied().unwrap_or(0);

            // Look up existing cache entry.
            let hit = self
                .ic_caches
                .get(&chunk_id)
                .and_then(|vec| vec.get(slot))
                .and_then(|entry| entry.as_ref())
                .filter(|e| e.obj_ptr == obj_ptr && e.generation == current_gen)
                .map(|e| e.value.clone());

            if let Some(cached) = hit {
                self.stack.push(cached);
                return Ok(());
            }

            // Cache miss: perform the full lookup.
            let v = get_property(&obj, &key, 0)?;

            // Store into the IC.
            let cache_vec = self
                .ic_caches
                .entry(chunk_id)
                .or_insert_with(|| vec![None; IC_SLOTS_DEFAULT]);
            if slot >= cache_vec.len() {
                cache_vec.resize(slot + 1, None);
            }
            cache_vec[slot] = Some(IcEntry {
                obj_ptr,
                generation: current_gen,
                value: v.clone(),
            });
            self.stack.push(v);
        } else {
            // Non-object path: no caching.
            let v = get_property(&obj, &key, 0)?;
            self.stack.push(v);
        }

        Ok(())
    }

    /// `SetProperty` wrapper that bumps the generation counter so that stale
    /// IC entries are invalidated on next access.
    fn set_property_tracked(
        &mut self,
        obj: &Value,
        key: &str,
        value: Value,
    ) -> Result<(), crate::errors::RuntimeError> {
        set_property(obj, key, value)?;

        // Bump generation for the mutated object.
        let ptr = match obj {
            Value::Object(m) => Some(Rc::as_ptr(m) as usize),
            Value::Array(a, _) => Some(Rc::as_ptr(a) as usize),
            Value::Closure { props, .. } => Some(Rc::as_ptr(props) as usize),
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
                return Err(match thrown {
                    Value::String(s) => {
                        crate::errors::RuntimeError::from(format!("Uncaught: {}", s))
                    }
                    _ => crate::errors::RuntimeError::from("Uncaught exception"),
                });
            }
        };

        // FIX: Pop frames back to the handler's level without
        // sequentially restoring intermediate state. Only the handler's
        // saved snapshot matters.
        while self.frames.len() > handler.frame_count {
            if let Some(frame) = self.frames.pop() {
                // Restore the outermost caller's locals so we land in
                // the correct local-variable scope.
                self.locals = frame.locals;
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
                                let arg = self.chunk.code.get(j + 1).cloned().unwrap_or(0);
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
                            | Op::SetGlobal
                            | Op::GetGlobal
                            | Op::Call
                            | Op::GetBuiltin
                            | Op::GetProperty
                            | Op::SetProperty
                            | Op::GetPropertyIC
                            | Op::MakeClosure => {
                                let arg = self.chunk.code.get(j + 1).cloned().unwrap_or(0);
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
                        let arg = self.chunk.code.get(j + 1).cloned().unwrap_or(0);
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
                        let arg = self.chunk.code.get(j + 1).cloned().unwrap_or(0);
                        let name = match self.chunk.constants.get(arg as usize) {
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
}

// ---------------------------------------------------------------------------
// Free functions
// ---------------------------------------------------------------------------

/// Return the bytecode width of an opcode (opcode byte + operand bytes).
fn opcode_width(op: Op) -> usize {
    match op {
        Op::Const
        | Op::GetLocal
        | Op::SetLocal
        | Op::GetGlobal
        | Op::SetGlobal
        | Op::Call
        | Op::MakeClosure
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
        Value::Object(map) => Some(map.borrow().keys().cloned().collect()),
        Value::Closure { props, .. } => Some(props.borrow().keys().cloned().collect()),
        _ => None,
    }
}

/// Grow a locals vector so that `slot` is a valid index.
#[inline(always)]
fn ensure_slot(locals: &mut Vec<Value>, slot: usize) {
    if slot >= locals.len() {
        locals.resize(slot + 1, Value::Null);
    }
}

fn to_number(v: &Value) -> Result<f64, crate::errors::RuntimeError> {
    match v {
        Value::Number(n) => Ok(*n),
        _ => Err(crate::errors::RuntimeError::from("Expected number")),
    }
}

fn value_to_type(v: &Value) -> Type {
    match v {
        Value::Number(_) => Type::Number,
        Value::String(_) => Type::String,
        Value::Bool(_) => Type::Boolean,
        Value::Null => Type::Null,
        Value::Object(_) => Type::Object(HashMap::new()),
        Value::Array(_, _) => Type::Array(Box::new(Type::Void)),
        Value::Closure { .. } | Value::Builtin(_) => Type::Function(vec![], Box::new(Type::Void)),
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
        Type::Function(_, _) => {
            matches!(actual, Value::Closure { .. } | Value::Builtin(_))
        }
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

fn property_key(v: &Value) -> Result<String, crate::errors::RuntimeError> {
    match v {
        Value::String(s) => Ok(s.clone()),
        Value::Number(n) => Ok(n.to_string()),
        Value::Bool(b) => Ok(if *b {
            "true".to_string()
        } else {
            "false".to_string()
        }),
        Value::Null => Ok("null".to_string()),
        Value::Object(_) | Value::Array(..) | Value::Closure { .. } | Value::Builtin(_) => {
            // Minimal ToPropertyKey: fall back to stringification.
            // Full Symbol and ToPrimitive semantics are not yet implemented.
            Ok(format!("{}", v))
        }
    }
}

/// Reference-equality semantics for objects/arrays/closures, structural
/// equality for primitives. Matches JavaScript `===` behaviour.
fn values_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => {
            // IEEE 754: NaN !== NaN, +0 === -0
            x == y
        }
        (Value::String(x), Value::String(y)) => x == y,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Null, Value::Null) => true,
        // Reference equality for heap objects.
        (Value::Object(am), Value::Object(bm)) => Rc::ptr_eq(am, bm),
        (Value::Array(ar, _), Value::Array(br, _)) => Rc::ptr_eq(ar, br),
        (Value::Closure { props: ap, .. }, Value::Closure { props: bp, .. }) => Rc::ptr_eq(ap, bp),
        _ => false,
    }
}

/// Walk the prototype chain with cycle detection.
fn get_property(
    obj: &Value,
    key: &str,
    depth: usize,
) -> Result<Value, crate::errors::RuntimeError> {
    if depth > MAX_PROTOTYPE_DEPTH {
        return Err(crate::errors::RuntimeError::from(
            "Prototype chain cycle detected or too deep",
        ));
    }

    match obj {
        Value::Object(map) => {
            let m = map.borrow();
            if let Some(v) = m.get(key) {
                return Ok(v.clone());
            }
            if let Some(proto) = m.get("__proto__")
                && !matches!(proto, Value::Null)
            {
                return get_property(proto, key, depth + 1);
            }
            Ok(Value::Null)
        }
        Value::Array(arr, props) => {
            if let Ok(i) = key.parse::<usize>() {
                return Ok(arr.borrow().get(i).cloned().unwrap_or(Value::Null));
            }
            let p = props.borrow();
            if let Some(v) = p.get(key) {
                return Ok(v.clone());
            }
            if let Some(proto) = p.get("__proto__")
                && !matches!(proto, Value::Null)
            {
                return get_property(proto, key, depth + 1);
            }
            Ok(Value::Null)
        }
        Value::Closure { props, .. } => Ok(props.borrow().get(key).cloned().unwrap_or(Value::Null)),
        _ => Ok(Value::Null),
    }
}

fn set_property(obj: &Value, key: &str, value: Value) -> Result<(), crate::errors::RuntimeError> {
    match obj {
        Value::Object(map) => {
            map.borrow_mut().insert(key.to_string(), value);
            Ok(())
        }
        Value::Array(arr, props) => {
            if let Ok(i) = key.parse::<usize>() {
                let mut a = arr.borrow_mut();
                if i >= a.len() {
                    a.resize(i + 1, Value::Null);
                }
                a[i] = value;
                return Ok(());
            }
            props.borrow_mut().insert(key.to_string(), value);
            Ok(())
        }
        Value::Closure { props, .. } => {
            props.borrow_mut().insert(key.to_string(), value);
            Ok(())
        }
        _ => {
            // Non-object property assignments are no-ops in sloppy mode.
            Ok(())
        }
    }
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
