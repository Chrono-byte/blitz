//! Stack-based bytecode interpreter with runtime type enforcement.
//!
//! The VM executes compiled bytecode in a stack-based environment. It provides:
//! - Stack and local variable management
//! - Function call frames and closures
//! - Property access with inline caching
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
use crate::oxc_typecheck::Type;
use crate::value::Value;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

/// Virtual machine for executing bytecode.
pub struct Vm {
    /// Chunk being executed.
    chunk: Rc<Chunk>,
    /// Instruction pointer within the chunk.
    ip: usize,
    /// Stack of values.
    stack: Vec<Value>,
    /// Local variables.
    locals: Vec<Value>,
    /// Global variables.
    globals: Rc<RefCell<HashMap<String, Value>>>,
    /// Stack of frames.
    frames: Vec<Frame>,
    /// Root chunk.
    root_chunk: Option<Rc<Chunk>>,
    /// Slot types for current function (None at top level).
    current_slot_types: Option<Vec<Option<Type>>>,
    /// Inline-cache storage per chunk keyed by chunk pointer.
    ic_caches: RefCell<HashMap<usize, Vec<Option<IcEntry>>>>,
    /// Exception handler stack.
    handlers: Vec<HandlerRecord>,
}

struct Frame {
    chunk: Rc<Chunk>,
    ip: usize,
    locals: Vec<Value>,
    slot_types: Option<Vec<Option<Type>>>,
    return_type: Option<Type>,
    stack: Vec<Value>,
}

struct HandlerRecord {
    chunk: Rc<Chunk>,
    handler_ip: usize,
    saved_stack_len: usize,
    saved_locals_len: usize,
    frame_count: usize,
}

#[derive(Clone)]
struct IcEntry {
    obj_ptr: usize,
    value: Value,
}

impl Vm {
    /// Create a new virtual machine.
    pub fn new(globals: Rc<RefCell<HashMap<String, Value>>>) -> Self {
        Self {
            chunk: Rc::new(Chunk::new()),
            ip: 0,
            stack: Vec::new(),
            locals: Vec::new(),
            globals,
            frames: Vec::new(),
            root_chunk: None,
            current_slot_types: None,
            ic_caches: RefCell::new(HashMap::new()),
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
        self.list_own_keys_of_value(v)
    }

    /// Return own keys for a value when applicable.
    pub fn list_own_keys_of_value(&self, v: &Value) -> Option<Vec<String>> {
        match v {
            Value::Object(map) => Some(map.borrow().keys().cloned().collect()),
            Value::Closure { props, .. } => Some(props.borrow().keys().cloned().collect()),
            _ => None,
        }
    }

    /// Return prototype keys for a global's prototype property.
    pub fn list_global_prototype_keys(&self, name: &str) -> Option<Vec<String>> {
        let g = self.globals.borrow();
        let v = g.get(name)?;
        match get_property(v, "prototype") {
            Ok(pv) => self.list_own_keys_of_value(&pv),
            Err(_) => None,
        }
    }

    /// Shallow equality check for object-like runtime values.
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

    /// Reset mutable runtime state while preserving globals.
    pub fn reset(&mut self, keep_globals: &std::collections::HashSet<String>) {
        self.stack.clear();
        self.locals.clear();
        self.frames.clear();
        self.root_chunk = None;
        self.current_slot_types = None;
        self.ip = 0;
        self.chunk = Rc::new(Chunk::new());
        let mut g = self.globals.borrow_mut();
        g.retain(|k, _| keep_globals.contains(k));
    }

    /// Run a chunk of bytecode.
    pub fn run(&mut self, chunk: Chunk) -> Result<Value, crate::errors::RuntimeError> {
        let chunk_rc = Rc::new(chunk);
        self.root_chunk = Some(Rc::clone(&chunk_rc));
        self.chunk = chunk_rc;
        self.ip = 0;
        self.locals.clear();
        self.frames.clear();
        self.current_slot_types = None;

        loop {
            let op_byte = self.read_byte_checked()?;
            let op = Op::from_byte(op_byte).ok_or_else(|| {
                crate::errors::RuntimeError::from(format!(
                    "Invalid opcode 0x{:02x} at ip {}",
                    op_byte,
                    self.ip.saturating_sub(1)
                ))
            })?;

            match op {
                Op::Const => {
                    let idx = self.read_byte_checked()? as usize;
                    let v = self
                        .chunk
                        .constants
                        .get(idx)
                        .cloned()
                        .ok_or_else(|| crate::errors::RuntimeError::from("Bad constant"))?;
                    crate::trace::value_string("vm Const push", &v);
                    // Emit a stack snapshot before the push, push the constant into
                    // the runtime stack, then emit a snapshot after the push so the
                    // before/after snapshots show the actual effect of the Const opcode.
                    if crate::trace::enabled() {
                        self.trace_stack_snapshot("stack snapshot before Const");
                    }
                    self.stack.push(v);
                    if crate::trace::enabled() {
                        self.trace_stack_snapshot("stack snapshot after Const");
                    }
                }
                Op::Null => self.stack.push(Value::Null),
                Op::True => self.stack.push(Value::Bool(true)),
                Op::False => self.stack.push(Value::Bool(false)),
                Op::GetLocal => {
                    let slot = self.read_byte_checked()? as usize;
                    let v = self
                        .locals
                        .get(slot)
                        .cloned()
                        .ok_or_else(|| crate::errors::RuntimeError::from("Bad local slot"))?;
                    self.stack.push(v);
                }
                Op::SetLocal => {
                    let slot = self.read_byte_checked()? as usize;
                    let v = self.pop()?;
                    if let Some(Some(expected)) = self
                        .current_slot_types
                        .as_ref()
                        .and_then(|types| types.get(slot))
                    {
                        check_assign(expected, &v)?;
                    }
                    while self.locals.len() <= slot {
                        self.locals.push(Value::Null);
                    }
                    self.locals[slot] = v;
                }
                Op::GetGlobal => {
                    let idx = self.read_byte_checked()? as usize;
                    let name = self.get_string_constant(idx)?;
                    let g = self.globals.borrow();
                    let v = g.get(&name).cloned().unwrap_or(Value::Null);
                    if crate::trace::enabled() {
                        eprintln!("[trace] GetGlobal: '{}' -> {}", name, v.type_name());
                    }
                    self.stack.push(v);
                }
                Op::SetGlobal => {
                    let idx = self.read_byte_checked()? as usize;
                    let name = self.get_string_constant(idx)?;
                    let v = self.pop()?;
                    if let Some(Some(expected)) = self
                        .root_chunk
                        .as_ref()
                        .and_then(|root| root.global_types.get(&name))
                    {
                        check_assign(expected, &v)?;
                    }
                    self.globals.borrow_mut().insert(name, v);
                }
                Op::Pop => {
                    self.pop()?;
                }
                Op::Dup => {
                    let v = self.stack.last().cloned().ok_or("Stack underflow")?;
                    self.stack.push(v);
                }
                Op::Add => self.binary_num_or_str(|a, b| match (a, b) {
                    (Value::Number(x), Value::Number(y)) => Ok(Value::Number(x + y)),
                    (Value::String(s), v) | (v, Value::String(s)) => {
                        Ok(Value::String(format!("{}{}", s, v)))
                    }
                    _ => Err("Add: number or string expected"),
                })?,
                Op::AddNumber => {
                    let b = self.stack.pop().ok_or("Stack underflow")?;
                    let a = self.stack.pop().ok_or("Stack underflow")?;
                    match (a, b) {
                        (Value::Number(x), Value::Number(y)) => {
                            self.stack.push(Value::Number(x + y))
                        }
                        _ => return Err("AddNumber: number expected".into()),
                    }
                }
                Op::Sub => self.binary_num(|a, b| a - b)?,
                Op::Mul => self.binary_num(|a, b| a * b)?,
                Op::Div => self.binary_num(|a, b| a / b)?,
                Op::Mod => self.binary_num(|a, b| a % b)?,
                Op::Eq => self.binary_cmp(|a, b| Value::Bool(a == b))?,
                Op::Neq => self.binary_cmp(|a, b| Value::Bool(a != b))?,
                Op::Lt => self.binary_cmp(|a, b| Value::Bool(a < b))?,
                Op::Le => self.binary_cmp(|a, b| Value::Bool(a <= b))?,
                Op::Gt => self.binary_cmp(|a, b| Value::Bool(a > b))?,
                Op::Ge => self.binary_cmp(|a, b| Value::Bool(a >= b))?,
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
                Op::Call => self.execute_call()?,
                Op::Return => {
                    // If there is no call frame left, treat top-level Return as program completion.
                    if self.frames.is_empty() {
                        let result = match self.pop() {
                            Ok(v) => v,
                            Err(_) => Value::Null,
                        };
                        return Ok(result);
                    } else {
                        self.execute_return()?;
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
                Op::BuildObject => {
                    let n = self.read_byte_checked()? as usize;
                    let mut map = HashMap::new();
                    for _ in 0..n {
                        let value = self.pop()?;
                        // Pop the key value and validate it is a string. If it's not,
                        // emit richer diagnostics to help track down the source.
                        let key_val = self.pop()?;
                        let key = match key_val {
                            Value::String(s) => s,
                            other => {
                                if crate::trace::enabled() {
                                    eprintln!(
                                        "[diag] BuildObject failed: key must be string (actual={})",
                                        other.type_name()
                                    );
                                    crate::trace::value_string(
                                        "[diag] BuildObject non-string key",
                                        &other,
                                    );
                                    eprintln!("[diag] Stack snapshot (top -> bottom):");
                                    let mut idx = 0;
                                    for v in self.stack.iter().rev().take(8) {
                                        crate::trace::value_string(
                                            &format!("[diag]   stack[{}]", idx),
                                            v,
                                        );
                                        idx += 1;
                                    }

                                    // Disassembly window to help locate the offending bytecode.
                                    // Show a small window before and after the current ip.
                                    let start = self.ip.saturating_sub(8);
                                    let end = usize::min(self.ip + 8, self.chunk.code.len());
                                    eprintln!("[diag] disasm window ({}..{}):", start, end);

                                    let mut j = start;
                                    while j < end {
                                        let b = self.chunk.code[j];
                                        if let Some(op) = Op::from_byte(b) {
                                            match op {
                                                Op::Const => {
                                                    let arg = self
                                                        .chunk
                                                        .code
                                                        .get(j + 1)
                                                        .cloned()
                                                        .unwrap_or(0);
                                                    let arg_repr = match self
                                                        .chunk
                                                        .constants
                                                        .get(arg as usize)
                                                    {
                                                        Some(Value::String(s)) => {
                                                            format!("'{}'", s)
                                                        }
                                                        Some(v) => v.type_name().to_string(),
                                                        None => "<undef>".to_string(),
                                                    };
                                                    eprintln!(
                                                        "[diag] {:#06}  Const       {} ({})",
                                                        j, arg, arg_repr
                                                    );
                                                    j += 2;
                                                }
                                                Op::GetLocal
                                                | Op::SetLocal
                                                | Op::Call
                                                | Op::MakeClosure
                                                | Op::GetPropertyIC
                                                | Op::GetBuiltin => {
                                                    let arg = self
                                                        .chunk
                                                        .code
                                                        .get(j + 1)
                                                        .cloned()
                                                        .unwrap_or(0);
                                                    eprintln!(
                                                        "[diag] {:#06}  {:12?} {}",
                                                        j, op, arg
                                                    );
                                                    j += 2;
                                                }
                                                Op::GetGlobal | Op::SetGlobal => {
                                                    let arg = self
                                                        .chunk
                                                        .code
                                                        .get(j + 1)
                                                        .cloned()
                                                        .unwrap_or(0);
                                                    let name = match self
                                                        .chunk
                                                        .constants
                                                        .get(arg as usize)
                                                    {
                                                        Some(Value::String(s)) => s.clone(),
                                                        _ => "<non-string constant>".to_string(),
                                                    };
                                                    eprintln!(
                                                        "[diag] {:#06}  {:12?} {} ({})",
                                                        j, op, arg, name
                                                    );
                                                    j += 2;
                                                }
                                                Op::Jump | Op::JumpIfFalse | Op::PushHandler => {
                                                    let hi = self
                                                        .chunk
                                                        .code
                                                        .get(j + 1)
                                                        .cloned()
                                                        .unwrap_or(0)
                                                        as u16;
                                                    let lo = self
                                                        .chunk
                                                        .code
                                                        .get(j + 2)
                                                        .cloned()
                                                        .unwrap_or(0)
                                                        as u16;
                                                    let val = ((hi << 8) | lo) as i16;
                                                    eprintln!(
                                                        "[diag] {:#06}  {:12?} {}",
                                                        j, op, val
                                                    );
                                                    j += 3;
                                                }
                                                Op::BuildArray | Op::BuildObject => {
                                                    let count = self
                                                        .chunk
                                                        .code
                                                        .get(j + 1)
                                                        .cloned()
                                                        .unwrap_or(0);
                                                    eprintln!(
                                                        "[diag] {:#06}  {:12?} {}",
                                                        j, op, count
                                                    );
                                                    j += 2;
                                                }
                                                _ => {
                                                    eprintln!("[diag] {:#06}  {:?}", j, op);
                                                    j += 1;
                                                }
                                            }
                                        } else {
                                            eprintln!("[diag] {:#06}  ???  0x{:02x}", j, b);
                                            j += 1;
                                        }
                                    }
                                }
                                return Err(crate::errors::RuntimeError::from(format!(
                                    "BuildObject: key must be string (actual={})",
                                    other.type_name()
                                )));
                            }
                        };
                        map.insert(key, value);
                    }
                    self.stack.push(Value::Object(Rc::new(RefCell::new(map))));
                }
                Op::GetProperty => {
                    let key = self.pop()?;
                    let obj = self.pop()?;
                    let k = property_key(&key)?;
                    let v = get_property(&obj, &k)?;
                    self.stack.push(v);
                }
                Op::GetPropertyIC => self.get_property_ic()?,
                Op::SetProperty => {
                    let value = self.pop()?;
                    let key = self.pop()?;
                    let obj = self.pop()?;
                    let k = property_key(&key)?;
                    set_property(&obj, &k, value)?;
                }
                Op::MakeClosure => {
                    let idx = self.read_byte_checked()? as usize;
                    let proto =
                        self.chunk.functions.get(idx).cloned().ok_or_else(|| {
                            crate::errors::RuntimeError::from("Bad function index")
                        })?;

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
                }
                Op::GetBuiltin => {
                    let idx = self.read_byte_checked()? as usize;
                    let name = self.get_string_constant(idx)?;
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

    fn execute_call(&mut self) -> Result<(), crate::errors::RuntimeError> {
        let nargs = self.read_byte_checked()? as usize;
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
                if crate::trace::enabled() {
                    eprintln!(
                        "[diag] Call failed: callee not callable (actual={})",
                        callee.type_name()
                    );
                    crate::trace::value_string("[diag] Call callee value", &callee);
                    eprintln!("[diag] Stack snapshot (top -> bottom):");
                    let mut idx = 0;
                    for v in self.stack.iter().rev().take(8) {
                        crate::trace::value_string(&format!("[diag]   stack[{}]", idx), v);
                        idx += 1;
                    }

                    // Disassembly window to help locate the offending bytecode.
                    let start = self.ip.saturating_sub(8);
                    let end = std::cmp::min(self.ip + 8, self.chunk.code.len());
                    eprintln!("[diag] disasm window ({}..{}):", start, end);

                    let mut j = start;
                    while j < end {
                        let b = self.chunk.code[j];
                        if let Some(op) = Op::from_byte(b) {
                            match op {
                                Op::Const => {
                                    let arg = self.chunk.code.get(j + 1).cloned().unwrap_or(0);
                                    let arg_repr = match self.chunk.constants.get(arg as usize) {
                                        Some(Value::String(s)) => format!("'{}'", s),
                                        Some(v) => v.type_name().to_string(),
                                        None => "<undef>".to_string(),
                                    };
                                    eprintln!(
                                        "[diag] {:#06}  Const       {} ({})",
                                        j, arg, arg_repr
                                    );
                                    j += 2;
                                }
                                Op::GetLocal
                                | Op::SetLocal
                                | Op::Call
                                | Op::MakeClosure
                                | Op::GetPropertyIC
                                | Op::GetBuiltin => {
                                    let arg = self.chunk.code.get(j + 1).cloned().unwrap_or(0);
                                    eprintln!("[diag] {:#06}  {:12?} {}", j, op, arg);
                                    j += 2;
                                }
                                Op::GetGlobal | Op::SetGlobal => {
                                    let arg = self.chunk.code.get(j + 1).cloned().unwrap_or(0);
                                    let name = match self.chunk.constants.get(arg as usize) {
                                        Some(Value::String(s)) => s.clone(),
                                        _ => "<non-string constant>".to_string(),
                                    };
                                    eprintln!("[diag] {:#06}  {:12?} {} ({})", j, op, arg, name);
                                    j += 2;
                                }
                                Op::Jump | Op::JumpIfFalse | Op::PushHandler => {
                                    let hi =
                                        self.chunk.code.get(j + 1).cloned().unwrap_or(0) as u16;
                                    let lo =
                                        self.chunk.code.get(j + 2).cloned().unwrap_or(0) as u16;
                                    let val = ((hi << 8) | lo) as i16;
                                    eprintln!("[diag] {:#06}  {:12?} {}", j, op, val);
                                    j += 3;
                                }
                                Op::BuildArray | Op::BuildObject => {
                                    let count = self.chunk.code.get(j + 1).cloned().unwrap_or(0);
                                    eprintln!("[diag] {:#06}  {:12?} {}", j, op, count);
                                    j += 2;
                                }
                                _ => {
                                    eprintln!("[diag] {:#06}  {:?}", j, op);
                                    j += 1;
                                }
                            }
                        } else {
                            eprintln!("[diag] {:#06}  ???  0x{:02x}", j, b);
                            j += 1;
                        }
                    }
                }
                return Err(crate::errors::RuntimeError::from(format!(
                    "Call: not a function (actual={})",
                    callee.type_name()
                )));
            }
        }

        Ok(())
    }

    fn execute_return(&mut self) -> Result<(), crate::errors::RuntimeError> {
        let result = match self.pop() {
            Ok(v) => v,
            Err(_) => Value::Null,
        };

        if let Some(frame) = self.frames.pop() {
            if let Some(ref expected) = frame.return_type {
                check_assign(expected, &result)?;
            }
            self.current_slot_types = frame.slot_types;
            self.chunk = frame.chunk;
            self.ip = frame.ip;
            self.locals = frame.locals;
            self.stack.push(result);
        } else {
            return Err(crate::errors::RuntimeError::from(
                "Return without call frame",
            ));
        }

        Ok(())
    }

    fn prepare_call_args(
        &self,
        args: &[Value],
        proto: &crate::bytecode::FunctionProto,
    ) -> Result<Vec<Value>, crate::errors::RuntimeError> {
        let mut call_args = args.to_vec();

        if proto.has_rest {
            let ar = proto.arity;
            let mut fixed = Vec::new();
            if ar > 0 {
                for i in 0..(ar - 1) {
                    fixed.push(call_args.get(i).cloned().unwrap_or(Value::Null));
                }
            }

            let rest_slice = if call_args.len() > ar.saturating_sub(1) {
                call_args.split_off(ar.saturating_sub(1))
            } else {
                Vec::new()
            };

            let arr_rc = Rc::new(RefCell::new(rest_slice));
            let props_map = Rc::new(RefCell::new(HashMap::new()));
            props_map.borrow_mut().insert(
                "length".to_string(),
                Value::Number(arr_rc.borrow().len() as f64),
            );

            if let Some(array_val) = self.globals.borrow().get("Array")
                && let Ok(proto_val) = get_property(array_val, "prototype")
                && !matches!(proto_val, Value::Null)
            {
                props_map
                    .borrow_mut()
                    .insert("__proto__".to_string(), proto_val);
            }

            fixed.push(Value::Array(arr_rc, props_map));
            call_args = fixed;
        }

        Ok(call_args)
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

    fn push_call_frame(
        &mut self,
        proto: &Rc<crate::bytecode::FunctionProto>,
        call_args: Vec<Value>,
    ) -> Result<(), crate::errors::RuntimeError> {
        self.frames.push(Frame {
            chunk: Rc::clone(&self.chunk),
            ip: self.ip,
            locals: std::mem::take(&mut self.locals),
            slot_types: self.current_slot_types.clone(),
            return_type: proto.return_type.clone(),
            stack: self.stack.clone(),
        });

        self.chunk = Rc::clone(&proto.chunk);
        self.ip = 0;
        self.current_slot_types = Some(proto.slot_types.clone());
        self.locals = call_args
            .into_iter()
            .chain(std::iter::repeat(Value::Null))
            .take(proto.arity)
            .collect();

        Ok(())
    }

    fn build_array(&mut self) -> Result<(), crate::errors::RuntimeError> {
        let n = self.read_byte_checked()? as usize;
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
            && let Ok(proto_val) = get_property(array_val, "prototype")
            && !matches!(proto_val, Value::Null)
        {
            props
                .borrow_mut()
                .insert("__proto__".to_string(), proto_val);
        }

        self.stack.push(Value::Array(arr_rc, props));
        Ok(())
    }

    fn get_property_ic(&mut self) -> Result<(), crate::errors::RuntimeError> {
        let slot = self.read_byte_checked()? as usize;
        let key_val = self.pop()?;
        let obj = self.pop()?;
        let key = property_key(&key_val)?;

        let chunk_id = Rc::as_ptr(&self.chunk) as usize;
        let mut caches = self.ic_caches.borrow_mut();
        let cache_vec = caches.entry(chunk_id).or_insert_with(|| vec![None; 256]);

        match &obj {
            Value::Object(map_rc) => {
                let obj_ptr = Rc::as_ptr(map_rc) as usize;
                if let Some(Some(entry)) = cache_vec.get(slot).cloned()
                    && entry.obj_ptr == obj_ptr
                {
                    self.stack.push(entry.value);
                    return Ok(());
                }

                let v = get_property(&obj, &key)?;
                cache_vec[slot] = Some(IcEntry {
                    obj_ptr,
                    value: v.clone(),
                });
                self.stack.push(v);
            }
            _ => {
                let v = get_property(&obj, &key)?;
                self.stack.push(v);
            }
        }

        Ok(())
    }

    fn jump(&mut self, offset: isize) -> Result<(), crate::errors::RuntimeError> {
        let new_ip = (self.ip as isize) + offset;
        if new_ip < 0 || (new_ip as usize) >= self.chunk.code.len() {
            return Err(crate::errors::RuntimeError::from(format!(
                "Jump target out of bounds (ip={} offset={})",
                self.ip, offset
            )));
        }
        self.ip = new_ip as usize;
        Ok(())
    }

    fn unwind_to_handler(&mut self, thrown: Value) -> Result<(), crate::errors::RuntimeError> {
        if let Some(h) = self.handlers.pop() {
            while self.frames.len() > h.frame_count {
                if let Some(frame) = self.frames.pop() {
                    while self.stack.len() > frame.stack.len() {
                        self.stack.pop();
                    }
                    self.current_slot_types = frame.slot_types;
                    self.chunk = frame.chunk;
                    self.ip = frame.ip;
                    self.locals = frame.locals;
                }
            }

            if !Rc::ptr_eq(&self.chunk, &h.chunk) {
                self.chunk = Rc::clone(&h.chunk);
            }

            while self.stack.len() > h.saved_stack_len {
                self.stack.pop();
            }
            while self.locals.len() > h.saved_locals_len {
                self.locals.pop();
            }

            self.stack.push(thrown.clone());
            self.ip = h.handler_ip;
            Ok(())
        } else {
            Err(match thrown {
                Value::String(s) => crate::errors::RuntimeError::from(format!("Uncaught: {}", s)),
                _ => crate::errors::RuntimeError::from("Uncaught exception"),
            })
        }
    }

    fn get_string_constant(&self, idx: usize) -> Result<String, crate::errors::RuntimeError> {
        match self.chunk.constants.get(idx) {
            Some(Value::String(s)) => Ok(s.clone()),
            _ => Err(crate::errors::RuntimeError::from("Constant not a string")),
        }
    }

    #[allow(unused)]
    fn trace_push(&self, label: &str, v: &Value) {
        if crate::trace::enabled() {
            self.trace_stack_snapshot(&format!("stack snapshot before {}", label));
            self.trace_stack_snapshot(&format!("stack snapshot after {}", label));
        }
    }

    fn trace_stack_snapshot(&self, label: &str) {
        if crate::trace::enabled() {
            eprintln!("[trace] {} len={}", label, self.stack.len());
            let start = if self.stack.len() > 10 {
                self.stack.len() - 10
            } else {
                0
            };
            for (i, v) in self.stack.iter().enumerate().skip(start) {
                crate::trace::value_string(&format!("{} [{}]", label, i), v);
            }
            if self.stack.len() > 10 {
                eprintln!("[trace] ... (showing last 10)");
            }
        }
    }

    fn pop(&mut self) -> Result<Value, crate::errors::RuntimeError> {
        self.stack
            .pop()
            .ok_or_else(|| crate::errors::RuntimeError::from("Stack underflow"))
    }

    fn read_byte_checked(&mut self) -> Result<u8, crate::errors::RuntimeError> {
        if self.ip >= self.chunk.code.len() {
            return Err(crate::errors::RuntimeError::from(format!(
                "Unexpected end of bytecode at ip {}",
                self.ip
            )));
        }
        let b = self.chunk.code[self.ip];
        self.ip = self.ip.saturating_add(1);
        Ok(b)
    }

    fn read_short_signed(&mut self) -> Result<isize, crate::errors::RuntimeError> {
        let hi = self.read_byte_checked()? as u16;
        let lo = self.read_byte_checked()? as u16;
        let u = (hi << 8) | lo;
        Ok((u as i16) as isize)
    }

    fn binary_num<F>(&mut self, f: F) -> Result<(), crate::errors::RuntimeError>
    where
        F: FnOnce(f64, f64) -> f64,
    {
        let b = self
            .stack
            .pop()
            .ok_or_else(|| crate::errors::RuntimeError::from("Stack underflow"))?;
        let a = self
            .stack
            .pop()
            .ok_or_else(|| crate::errors::RuntimeError::from("Stack underflow"))?;
        let (x, y) = (to_number(&a)?, to_number(&b)?);
        self.stack.push(Value::Number(f(x, y)));
        Ok(())
    }

    fn binary_num_or_str<F>(&mut self, f: F) -> Result<(), crate::errors::RuntimeError>
    where
        F: FnOnce(Value, Value) -> Result<Value, &'static str>,
    {
        let b = self
            .stack
            .pop()
            .ok_or_else(|| crate::errors::RuntimeError::from("Stack underflow"))?;
        let a = self
            .stack
            .pop()
            .ok_or_else(|| crate::errors::RuntimeError::from("Stack underflow"))?;
        let v = f(a, b).map_err(crate::errors::RuntimeError::from)?;
        self.stack.push(v);
        Ok(())
    }

    fn binary_cmp<F>(&mut self, f: F) -> Result<(), crate::errors::RuntimeError>
    where
        F: FnOnce(Value, Value) -> Value,
    {
        let b = self
            .stack
            .pop()
            .ok_or_else(|| crate::errors::RuntimeError::from("Stack underflow"))?;
        let a = self
            .stack
            .pop()
            .ok_or_else(|| crate::errors::RuntimeError::from("Stack underflow"))?;
        self.stack.push(f(a, b));
        Ok(())
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

fn runtime_assignable(expected: &Type, actual: &Value) -> bool {
    match expected {
        Type::Number => matches!(actual, Value::Number(_)),
        Type::String => matches!(actual, Value::String(_)),
        Type::Boolean => matches!(actual, Value::Bool(_)),
        Type::Null => matches!(actual, Value::Null),
        Type::Void => true,
        Type::Array(_) => matches!(actual, Value::Array(_, _)),
        Type::Object(_) => matches!(actual, Value::Object(_)),
        Type::Function(_, _) => matches!(actual, Value::Closure { .. } | Value::Builtin(_)),
        // Accept other type forms at runtime conservatively.
        Type::Any
        | Type::Unknown
        | Type::Builtin(_)
        | Type::TypeReference { .. }
        | Type::Union(_) => true,
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
        _ => Err(crate::errors::RuntimeError::from(
            "Property key must be string or number",
        )),
    }
}

fn get_property(obj: &Value, key: &str) -> Result<Value, crate::errors::RuntimeError> {
    match obj {
        Value::Object(map) => {
            let m = map.borrow();
            if let Some(v) = m.get(key) {
                return Ok(v.clone());
            }
            if let Some(proto) = m.get("__proto__")
                && !matches!(proto, Value::Null)
            {
                return get_property(proto, key);
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
                return get_property(proto, key);
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
                while a.len() <= i {
                    a.push(Value::Null);
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
        _ => Err(format!("SetProperty: not an object (actual={})", obj.type_name()).into()),
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
