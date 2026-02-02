//! Stack-based bytecode interpreter. Types are enforced at runtime.

use crate::ast::Type;
use crate::bytecode::{Chunk, Op};
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
    /// Inline-cache storage per chunk keyed by chunk pointer. Each entry is a
    /// vector of optional IC entries indexed by the one-byte IC slot used in
    /// the bytecode. Stored in a RefCell so the VM can mutate caches while
    /// sharing immutable Chunk Rc pointers.
    ic_caches: RefCell<HashMap<usize, Vec<Option<IcEntry>>>>,
    /// Exception handler stack. Each handler records the chunk where the
    /// handler lives, the handler ip within that chunk, the saved stack and
    /// locals lengths and the frame count at time of installation. This lets
    /// the VM unwind across call frames when an exception is thrown.
    handlers: Vec<HandlerRecord>,
}

struct Frame {
    chunk: Rc<Chunk>,
    ip: usize,
    locals: Vec<Value>,
    slot_types: Option<Vec<Option<Type>>>,
    return_type: Option<Type>,
    /// Snapshot of the value stack at the time the frame was created. When
    /// unwinding we restore this stack so the caller sees the same values it
    /// had before the call.
    stack: Vec<Value>,
}

struct HandlerRecord {
    chunk: Rc<Chunk>,
    handler_ip: usize,
    saved_stack_len: usize,
    saved_locals_len: usize,
    frame_count: usize,
}

/// Simple inline cache entry storing the object identity pointer and the
/// cached property value.
#[derive(Clone)]
struct IcEntry {
    obj_ptr: usize,
    value: Value,
}

impl Vm {
    /// Create a new virtual machine.
    ///
    /// # Arguments
    /// * `globals` - Global variables.
    ///
    /// # Returns
    /// A new virtual machine.
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

    /// Debug helper: get a global value by name (cloned). Public for diagnostics.
    pub fn get_global(&self, name: &str) -> Option<Value> {
        self.globals.borrow().get(name).cloned()
    }

    /// Return own (non-prototypal) keys for a global value if it is an object
    /// or a function object (closure props). This helper is diagnostic only
    /// and intentionally shallow to avoid recursing into nested values which
    /// can cause deep/cyclic traversals during debug printing.
    pub fn list_global_own_keys(&self, name: &str) -> Option<Vec<String>> {
        let g = self.globals.borrow();
        let v = g.get(name)?;
        self.list_own_keys_of_value(v)
    }

    /// Return own keys for a value when applicable. Returns None for values
    /// that are not objects/closures.
    pub fn list_own_keys_of_value(&self, v: &Value) -> Option<Vec<String>> {
        match v {
            Value::Object(map) => Some(map.borrow().keys().cloned().collect()),
            Value::Closure { props, .. } => Some(props.borrow().keys().cloned().collect()),
            _ => None,
        }
    }

    /// If the global named `name` exposes a `prototype` property that is an
    /// object, return the prototype object's own keys. Useful for checking
    /// `Array.prototype` after the soft prelude ran.
    pub fn list_global_prototype_keys(&self, name: &str) -> Option<Vec<String>> {
        let g = self.globals.borrow();
        let v = g.get(name)?;
        match get_property(v, "prototype") {
            Ok(pv) => self.list_own_keys_of_value(&pv),
            Err(_) => None,
        }
    }

    /// Shallow equality check for object-like runtime values. Returns true if
    /// both values share the same underlying Rc pointer (object/array/props).
    /// This helps confirm prototype identity without printing full object
    /// contents.
    pub fn values_same_object(&self, a: &Value, b: &Value) -> bool {
        match (a, b) {
            (Value::Object(am), Value::Object(bm)) => Rc::ptr_eq(am, bm),
            // Array equality compares the underlying vector pointer for identity.
            (Value::Array(ar, _), Value::Array(br, _)) => Rc::ptr_eq(ar, br),
            (Value::Closure { props: ap, .. }, Value::Closure { props: bp, .. }) => {
                Rc::ptr_eq(ap, bp)
            }
            _ => false,
        }
    }

    /// Reset mutable runtime state while preserving the globals Rc.
    /// `keep_globals` lists global names that must not be removed from the
    /// global table (e.g. soft-prelude installed APIs and host builtins).
    /// This is a generic VM capability for reusing a VM across multiple
    /// runs/tests without reallocating host-visible globals.
    pub fn reset(&mut self, keep_globals: &std::collections::HashSet<String>) {
        self.stack.clear();
        self.locals.clear();
        self.frames.clear();
        self.root_chunk = None;
        self.current_slot_types = None;
        self.ip = 0;
        // reset to an empty chunk so next `run` starts clean
        self.chunk = Rc::new(Chunk::new());
        // Remove globals not in the keep list
        let mut g = self.globals.borrow_mut();
        g.retain(|k, _| keep_globals.contains(k));
    }

    /// Run a chunk of bytecode.
    ///
    /// # Errors
    /// Returns an error if the bytecode is invalid or if the execution fails.
    pub fn run(&mut self, chunk: Chunk) -> Result<Value, String> {
        let chunk_rc = Rc::new(chunk);
        self.root_chunk = Some(Rc::clone(&chunk_rc));
        self.chunk = chunk_rc;
        self.ip = 0;
        self.locals.clear();
        self.frames.clear();
        self.current_slot_types = None;

        loop {
            let op_byte = self.read_byte_checked()?;
            let op = Op::from_byte(op_byte).ok_or(format!(
                "Invalid opcode 0x{:02x} at ip {}",
                op_byte,
                self.ip.saturating_sub(1)
            ))?;
            match op {
                Op::Const => {
                    let idx = self.read_byte_checked()? as usize;
                    let v = self
                        .chunk
                        .constants
                        .get(idx)
                        .cloned()
                        .ok_or("Bad constant")?;
                    crate::trace::value_string("vm Const push", &v);
                    self.stack.push(v);
                }
                Op::Null => self.stack.push(Value::Null),
                Op::True => self.stack.push(Value::Bool(true)),
                Op::False => self.stack.push(Value::Bool(false)),
                Op::GetLocal => {
                    let slot = self.read_byte_checked()? as usize;
                    let v = self.locals.get(slot).cloned().ok_or("Bad local slot")?;
                    self.stack.push(v);
                }
                Op::SetLocal => {
                    let slot = self.read_byte_checked()? as usize;
                    let v = self.pop()?;
                    if let Some(Some(expected)) = self
                        .current_slot_types
                        .as_ref()
                        .and_then(|types| types.get(slot))
                        && let Err(e) = check_assign(expected, &v)
                    {
                        if crate::trace::enabled() {
                            crate::trace::value_string(
                                &format!("SetLocal slot={} failed", slot),
                                &v,
                            );
                        }
                        return Err(e);
                    }
                    while self.locals.len() <= slot {
                        self.locals.push(Value::Null);
                    }
                    self.locals[slot] = v;
                }
                Op::GetGlobal => {
                    let idx = self.read_byte_checked()? as usize;
                    let name = match self.chunk.constants.get(idx) {
                        Some(Value::String(s)) => s.clone(),
                        _ => return Err("GetGlobal: constant not a string".into()),
                    };
                    let g = self.globals.borrow();
                    let v = g.get(&name).cloned().unwrap_or(Value::Null);
                    if crate::trace::enabled() {
                        eprintln!("[trace] GetGlobal: '{}' -> {}", name, v.type_name());
                    }
                    self.stack.push(v);
                }
                Op::SetGlobal => {
                    let idx = self.read_byte_checked()? as usize;
                    let name = match self.chunk.constants.get(idx) {
                        Some(Value::String(s)) => s.clone(),
                        _ => return Err("SetGlobal: constant not a string".into()),
                    };
                    let v = self.pop()?;
                    if let Some(Some(expected)) = self
                        .root_chunk
                        .as_ref()
                        .and_then(|root| root.global_types.get(&name))
                        && let Err(e) = check_assign(expected, &v)
                    {
                        if crate::trace::enabled() {
                            crate::trace::value_string(&format!("SetGlobal '{}' failed", name), &v);
                        }
                        return Err(e);
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
                        // Use Value's Display implementation for concatenation but avoid
                        // exposing internal structure; `Value::Display` formats safely.
                        Ok(Value::String(format!("{}{}", s, v)))
                    }
                    _ => Err("Add: number or string expected"),
                })?,
                Op::AddNumber => {
                    // Fast-path numeric addition when both operands are numbers.
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
                Op::Call => {
                    let nargs = self.read_byte_checked()? as usize;
                    if crate::trace::enabled() {
                        eprintln!(
                            "[trace] Op::Call ip={} nargs={} stack_len={}",
                            self.ip,
                            nargs,
                            self.stack.len()
                        );
                    }
                    let mut args: Vec<Value> = Vec::with_capacity(nargs);
                    for _ in 0..nargs {
                        args.push(self.pop()?);
                    }
                    args.reverse();
                    let callee = self.pop()?;
                    if crate::trace::enabled() {
                        for (i, v) in args.iter().enumerate() {
                            crate::trace::value_string(&format!("Call arg[{}]", i), v);
                        }
                        crate::trace::value_string("Call callee", &callee);
                    }
                    match callee {
                        Value::Builtin(f) => {
                            // Builtins receive a mutable reference to the VM and may
                            // perform VM-level operations; they return Result.
                            let result = f(self, &args)?;
                            self.stack.push(result);
                        }
                        Value::Closure { proto, .. } => {
                            // Handle rest parameters: if the function prototype declares a
                            // rest parameter (`has_rest`), pack any extra arguments into an
                            // array placed into the last parameter slot.
                            let mut call_args = args;
                            if proto.has_rest {
                                let ar = proto.arity;
                                let mut fixed: Vec<Value> = Vec::new();
                                if ar > 0 {
                                    for i in 0..(ar - 1) {
                                        fixed
                                            .push(call_args.get(i).cloned().unwrap_or(Value::Null));
                                    }
                                }
                                let rest_slice = if call_args.len() > (ar.saturating_sub(1)) {
                                    call_args.split_off(ar.saturating_sub(1))
                                } else {
                                    Vec::new()
                                };
                                // Build a dense array representation (Vec + props map)
                                // so rest parameter arrays have fast numeric access while
                                // preserving prototype linkage and named properties.
                                let elems = rest_slice;
                                // elements are already in proper order from split_off
                                let arr_rc = Rc::new(RefCell::new(elems));
                                let props_map = Rc::new(RefCell::new(HashMap::new()));
                                // length property
                                props_map.borrow_mut().insert(
                                    "length".to_string(),
                                    Value::Number(arr_rc.borrow().len() as f64),
                                );
                                // try to link to Array.prototype if available
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

                            for (i, (arg, expected_opt)) in call_args
                                .iter()
                                .zip(proto.slot_types.iter().take(proto.arity))
                                .enumerate()
                            {
                                if let Some(expected) = expected_opt {
                                    if *expected == Type::Void {
                                        continue;
                                    }
                                    if let Err(e) = check_assign(expected, arg) {
                                        // Diagnostic: indicate which function and parameter failed
                                        if crate::trace::enabled() {
                                            eprintln!(
                                                "[trace] argument type mismatch in function '{}' param={} expected={} actual={}",
                                                proto.name,
                                                i + 1,
                                                crate::typecheck::type_to_string(expected),
                                                crate::typecheck::type_to_string(&value_to_type(
                                                    arg
                                                )),
                                            );
                                            crate::trace::value_string("arg value", arg);
                                        }
                                        return Err(format!("Argument {}: {}", i + 1, e));
                                    }
                                }
                            }
                            // Capture a snapshot of the current value stack so we can
                            // restore it when unwinding from the callee.
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
                            // Extend with env for closed-over vars if we add that later
                        }
                        _ => {
                            if crate::trace::enabled() {
                                eprintln!(
                                    "[trace] Call: not a function at ip={} stack_len={}",
                                    self.ip,
                                    self.stack.len()
                                );
                                for (i, v) in self.stack.iter().enumerate() {
                                    crate::trace::value_string(&format!("stack[{}]", i), v);
                                }
                            }
                            return Err("Call: not a function".into());
                        }
                    }
                }
                Op::Return => {
                    let result = match self.pop() {
                        Ok(v) => v,
                        Err(_) => Value::Null,
                    };
                    if let Some(frame) = self.frames.pop() {
                        if let Some(ref expected) = frame.return_type {
                            check_assign(expected, &result)
                                .map_err(|e| format!("Return value: {}", e))?;
                        }
                        self.current_slot_types = frame.slot_types;
                        self.chunk = frame.chunk;
                        self.ip = frame.ip;
                        self.locals = frame.locals;
                        self.stack.push(result);
                    } else {
                        return Ok(result);
                    }
                }
                Op::Jump => {
                    let offset = self.read_short_signed()?;
                    let new_ip = (self.ip as isize) + offset;
                    if new_ip < 0 || (new_ip as usize) >= self.chunk.code.len() {
                        return Err(format!(
                            "Jump target out of bounds (ip={} offset={})",
                            self.ip, offset
                        ));
                    }
                    self.ip = new_ip as usize;
                }
                Op::JumpIfFalse => {
                    let offset = self.read_short_signed()?;
                    let v = self.pop()?;
                    if !v.is_truthy() {
                        let new_ip = (self.ip as isize) + offset;
                        if new_ip < 0 || (new_ip as usize) >= self.chunk.code.len() {
                            return Err(format!(
                                "JumpIfFalse target out of bounds (ip={} offset={})",
                                self.ip, offset
                            ));
                        }
                        self.ip = new_ip as usize;
                    }
                }
                Op::BuildArray => {
                    let n = self.read_byte_checked()? as usize;
                    // Build a dense array representation: a Vec for indexed elements
                    // plus a property bag for non-index properties and prototype
                    // linkage. This preserves semantics while allowing fast numeric
                    // access.
                    let mut elems: Vec<Value> = Vec::with_capacity(n);
                    for _ in 0..n {
                        elems.push(self.pop()?);
                    }
                    elems.reverse();

                    let arr_rc = Rc::new(RefCell::new(elems));
                    let props = Rc::new(RefCell::new(HashMap::new()));
                    // length property
                    props
                        .borrow_mut()
                        .insert("length".to_string(), Value::Number(n as f64));

                    // Try to link to Array.prototype if present in globals.
                    if let Some(array_val) = self.globals.borrow().get("Array") {
                        // Diagnostics: inspect global Array and its prototype keys
                        if crate::trace::enabled() {
                            if let Some(keys) = self.list_global_own_keys("Array") {
                                eprintln!("[trace] BuildArray: global Array own keys = {:?}", keys);
                            }
                            if let Some(pkeys) = self.list_global_prototype_keys("Array") {
                                eprintln!(
                                    "[trace] BuildArray: Array.prototype own keys = {:?}",
                                    pkeys
                                );
                            } else {
                                eprintln!(
                                    "[trace] BuildArray: Array.prototype not found or not object"
                                )
                            }
                        }

                        if let Ok(proto_val) = get_property(array_val, "prototype") {
                            if crate::trace::enabled() {
                                match get_property(&proto_val, "map") {
                                    Ok(mv) => crate::trace::value_string(
                                        "BuildArray: prototype.map ->",
                                        &mv,
                                    ),
                                    Err(e) => eprintln!(
                                        "[trace] BuildArray: prototype.map lookup err: {}",
                                        e
                                    ),
                                }
                            }
                            if !matches!(proto_val, Value::Null) {
                                // Link prototype into the array's props bag so prototype
                                // lookups delegate correctly.
                                props
                                    .borrow_mut()
                                    .insert("__proto__".to_string(), proto_val.clone());
                                if crate::trace::enabled()
                                    && let Some(global_proto_keys) =
                                        self.list_global_prototype_keys("Array")
                                {
                                    eprintln!(
                                        "[trace] BuildArray: confirmed Array.prototype keys len={}",
                                        global_proto_keys.len()
                                    );
                                }
                            }
                        }
                    }

                    self.stack.push(Value::Array(arr_rc, props));
                }
                Op::BuildObject => {
                    let n = self.read_byte_checked()? as usize;
                    let mut map = HashMap::new();
                    for _ in 0..n {
                        let value = self.pop()?;
                        let key = match self.pop()? {
                            Value::String(s) => s,
                            _ => return Err("BuildObject: key must be string".into()),
                        };
                        map.insert(key, value);
                    }
                    self.stack.push(Value::Object(Rc::new(RefCell::new(map))));
                }
                Op::GetProperty => {
                    if crate::trace::enabled() {
                        eprintln!(
                            "[trace] Op::GetProperty ip={} stack_len={}",
                            self.ip,
                            self.stack.len()
                        );
                        if let Some(v) = self.stack.last() {
                            crate::trace::value_string("GetProperty: key (peek)", v);
                        }
                        if self.stack.len() >= 2 {
                            crate::trace::value_string(
                                "GetProperty: obj (peek)",
                                &self.stack[self.stack.len() - 2],
                            );
                        }
                    }
                    let key = self.pop()?;
                    let obj = self.pop()?;
                    if crate::trace::enabled() {
                        crate::trace::value_string("GetProperty: popped key", &key);
                        crate::trace::value_string("GetProperty: popped obj", &obj);
                    }
                    let k = property_key(&key)?;
                    if crate::trace::enabled() {
                        eprintln!("[trace] GetProperty: resolved key='{}'", k);
                    }
                    let v = get_property(&obj, &k)?;
                    if crate::trace::enabled() {
                        crate::trace::value_string("GetProperty: result", &v);
                    }
                    self.stack.push(v);
                }
                Op::GetPropertyIC => {
                    // Operand: one-byte IC slot index (0..255)
                    let slot = self.read_byte_checked()? as usize;
                    // Pop key then object (key is expected to be a constant string for our current compiler emission)
                    let key_val = self.pop()?;
                    let obj = self.pop()?;
                    let key = property_key(&key_val)?;

                    // Determine chunk id (use pointer of chunk Rc)
                    let chunk_id = Rc::as_ptr(&self.chunk) as usize;
                    // Ensure there's a cache vector for this chunk
                    let mut caches = self.ic_caches.borrow_mut();
                    let cache_vec = caches.entry(chunk_id).or_insert_with(|| vec![None; 256]);

                    // Inspect object identity and attempt cache hit
                    match &obj {
                        Value::Object(map_rc) => {
                            let obj_ptr = Rc::as_ptr(map_rc) as usize;
                            if let Some(Some(entry)) = cache_vec.get(slot).cloned()
                                && entry.obj_ptr == obj_ptr
                            {
                                if crate::trace::enabled() {
                                    crate::trace::value_string(
                                        "GetPropertyIC: cache hit",
                                        &entry.value,
                                    );
                                }
                                self.stack.push(entry.value.clone());
                                continue;
                            }
                            // Cache miss: perform full lookup and update cache
                            let v = get_property(&obj, &key)?;
                            cache_vec[slot] = Some(IcEntry {
                                obj_ptr,
                                value: v.clone(),
                            });
                            if crate::trace::enabled() {
                                crate::trace::value_string("GetPropertyIC: cache miss result", &v);
                            }
                            self.stack.push(v);
                        }
                        // Arrays and closures/prototypes can fallback to general get_property
                        _ => {
                            let v = get_property(&obj, &key)?;
                            self.stack.push(v);
                        }
                    }
                }
                Op::SetProperty => {
                    let value = self.pop()?;
                    let key = self.pop()?;
                    let obj = self.pop()?;
                    if crate::trace::enabled() {
                        crate::trace::value_string("SetProperty: obj", &obj);
                        crate::trace::value_string("SetProperty: key", &key);
                        crate::trace::value_string("SetProperty: value", &value);
                    }
                    let k = property_key(&key)?;
                    set_property(&obj, &k, value)?;
                }
                Op::MakeClosure => {
                    let idx = self.read_byte_checked()? as usize;
                    let proto = self
                        .chunk
                        .functions
                        .get(idx)
                        .cloned()
                        .ok_or("Bad function index")?;
                    let env = Rc::new(RefCell::new(Vec::new()));

                    // Create a mutable property bag for the function and a default prototype object.
                    let proto_obj_map = Rc::new(RefCell::new(HashMap::new()));
                    let proto_obj = Value::Object(Rc::clone(&proto_obj_map));

                    let props = Rc::new(RefCell::new(HashMap::new()));
                    props
                        .borrow_mut()
                        .insert("prototype".to_string(), proto_obj.clone());

                    let closure = Value::Closure {
                        proto: Rc::new(proto),
                        env,
                        props: Rc::clone(&props),
                    };

                    // Link prototype.constructor back to the function object.
                    proto_obj_map
                        .borrow_mut()
                        .insert("constructor".to_string(), closure.clone());

                    self.stack.push(closure);
                }
                Op::GetBuiltin => {
                    let idx = self.read_byte_checked()? as usize;
                    let name = match self.chunk.constants.get(idx) {
                        Some(Value::String(s)) => s.clone(),
                        _ => return Err("GetBuiltin: constant not a string".into()),
                    };
                    let g = self.globals.borrow();
                    let v = g
                        .get(&name)
                        .cloned()
                        .ok_or(format!("GetBuiltin: '{}' not found", name))?;
                    match v {
                        Value::Builtin(_) => self.stack.push(v),
                        _ => return Err(format!("GetBuiltin: '{}' is not a builtin", name)),
                    }
                }
                Op::PushHandler => {
                    // Operand is a signed 16-bit offset to handler start
                    let offset = self.read_short_signed()?;
                    let handler_ip = (self.ip as isize + offset) as usize;
                    // record current chunk, stack and locals lengths and frame count
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
                    // Pop thrown value and perform unwinding to the nearest handler
                    let thrown = self.pop()?;
                    let mut handled = false;
                    // Try to find a handler by popping handler records until one succeeds
                    while let Some(h) = self.handlers.pop() {
                        // Unwind call frames until we reach the frame count recorded
                        while self.frames.len() > h.frame_count {
                            if let Some(frame) = self.frames.pop() {
                                // Drop the callee frame and restore caller context
                                // Also restore the value stack to the caller's stack length
                                while self.stack.len() > frame.stack.len() {
                                    self.stack.pop();
                                }
                                self.current_slot_types = frame.slot_types;
                                self.chunk = frame.chunk;
                                self.ip = frame.ip;
                                self.locals = frame.locals;
                            }
                        }

                        // Switch to the handler's chunk if necessary
                        if !Rc::ptr_eq(&self.chunk, &h.chunk) {
                            self.chunk = Rc::clone(&h.chunk);
                        }

                        // Restore stack and locals lengths recorded at handler installation
                        while self.stack.len() > h.saved_stack_len {
                            self.stack.pop();
                        }
                        while self.locals.len() > h.saved_locals_len {
                            self.locals.pop();
                        }

                        // Push the thrown value as the catch parameter
                        self.stack.push(thrown.clone());

                        // Jump to handler
                        self.ip = h.handler_ip;
                        handled = true;
                        // Resume execution at handler
                        break;
                    }

                    if !handled {
                        // No handler found: propagate as runtime error
                        return Err(match thrown {
                            Value::String(s) => format!("Uncaught: {}", s),
                            _ => "Uncaught exception".to_string(),
                        });
                    }
                }
            }
        }
    }

    fn read_byte(&mut self) -> u8 {
        // Use usize for `ip` and index directly. Return 0 if out of bounds.
        let b = self.chunk.code.get(self.ip).copied().unwrap_or(0);
        self.ip = self.ip.saturating_add(1);
        b
    }

    fn read_byte_checked(&mut self) -> Result<u8, String> {
        if self.ip >= self.chunk.code.len() {
            return Err(format!("Unexpected end of bytecode at ip {}", self.ip));
        }
        let b = self.chunk.code[self.ip];
        self.ip = self.ip.saturating_add(1);
        Ok(b)
    }

    fn pop(&mut self) -> Result<Value, String> {
        self.stack
            .pop()
            .ok_or_else(|| "Stack underflow".to_string())
    }

    fn read_short_signed(&mut self) -> Result<isize, String> {
        // Read two bytes and interpret as signed 16-bit big-endian value.
        let hi = self.read_byte_checked()? as u16;
        let lo = self.read_byte_checked()? as u16;
        let u = (hi << 8) | lo;
        Ok((u as i16) as isize)
    }

    /// Raise a JavaScript-level exception value and attempt to unwind to the
    /// nearest installed handler. If a handler is found the VM context is
    /// adjusted and execution will resume at the handler; if no handler is
    /// present this returns Err so the runtime run() call can propagate an
    /// uncaught exception.
    fn raise_exception(&mut self, thrown: Value) -> Result<(), String> {
        let mut handled = false;
        // Try to find a handler by popping handler records until one succeeds
        while let Some(h) = self.handlers.pop() {
            // Unwind call frames until we reach the frame count recorded
            while self.frames.len() > h.frame_count {
                if let Some(frame) = self.frames.pop() {
                    // Drop the callee frame and restore caller context
                    // Also restore the value stack to the caller's stack length
                    while self.stack.len() > frame.stack.len() {
                        self.stack.pop();
                    }
                    self.current_slot_types = frame.slot_types;
                    self.chunk = frame.chunk;
                    self.ip = frame.ip;
                    self.locals = frame.locals;
                }
            }

            // Switch to the handler's chunk if necessary
            if !Rc::ptr_eq(&self.chunk, &h.chunk) {
                self.chunk = Rc::clone(&h.chunk);
            }

            // Restore stack and locals lengths recorded at handler installation
            while self.stack.len() > h.saved_stack_len {
                self.stack.pop();
            }
            while self.locals.len() > h.saved_locals_len {
                self.locals.pop();
            }

            // Push the thrown value as the catch parameter
            self.stack.push(thrown.clone());

            // Jump to handler
            self.ip = h.handler_ip;
            handled = true;
            // Resume execution at handler
            break;
        }

        if !handled {
            // No handler found: propagate as runtime error
            return Err(match thrown {
                Value::String(s) => format!("Uncaught: {}", s),
                _ => "Uncaught exception".to_string(),
            });
        }

        Ok(())
    }

    fn binary_num<F>(&mut self, f: F) -> Result<(), String>
    where
        F: FnOnce(f64, f64) -> f64,
    {
        let b = self.stack.pop().ok_or("Stack underflow")?;
        let a = self.stack.pop().ok_or("Stack underflow")?;
        let (x, y) = (to_number(&a)?, to_number(&b)?);
        self.stack.push(Value::Number(f(x, y)));
        Ok(())
    }

    fn binary_num_or_str<F>(&mut self, f: F) -> Result<(), String>
    where
        F: FnOnce(Value, Value) -> Result<Value, &'static str>,
    {
        let b = self.stack.pop().ok_or("Stack underflow")?;
        let a = self.stack.pop().ok_or("Stack underflow")?;
        let v = f(a, b).map_err(ToString::to_string)?;
        self.stack.push(v);
        Ok(())
    }

    fn binary_cmp<F>(&mut self, f: F) -> Result<(), String>
    where
        F: FnOnce(Value, Value) -> Value,
    {
        let b = self.stack.pop().ok_or("Stack underflow")?;
        let a = self.stack.pop().ok_or("Stack underflow")?;
        self.stack.push(f(a, b));
        Ok(())
    }
}

fn to_number(v: &Value) -> Result<f64, String> {
    match v {
        Value::Number(n) => Ok(*n),
        _ => Err("Expected number".into()),
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
        // At runtime, `void` is treated permissively: values may be provided
        // where `void` is annotated (commonly used for variadic/placeholder
        // annotations in library code). Treat `void` as accepting any value.
        Type::Void => true,
        Type::Array(_) => matches!(actual, Value::Array(_, _)),
        Type::Object(_) => matches!(actual, Value::Object(_)),
        Type::Function(_, _) => matches!(actual, Value::Closure { .. } | Value::Builtin(_)),
    }
}

fn check_assign(expected: &Type, actual: &Value) -> Result<(), String> {
    if runtime_assignable(expected, actual) {
        Ok(())
    } else {
        // Emit a shallow diagnostic to help find where the type mismatch is
        if crate::trace::enabled() {
            eprintln!(
                "[trace] runtime type check failed: expected {} got {}",
                crate::typecheck::type_to_string(expected),
                crate::typecheck::type_to_string(&value_to_type(actual))
            );
            crate::trace::value_string("runtime check actual", actual);
            // Print a Rust backtrace to help locate the VM opcode / callsite
            // that triggered this check. This is diagnostic-only.
            if let Ok(bt) = std::panic::catch_unwind(std::backtrace::Backtrace::capture) {
                eprintln!("[trace] backtrace: {:?}", bt);
            }
        }
        Err(runtime_type_error(
            &crate::typecheck::type_to_string(expected),
            &crate::typecheck::type_to_string(&value_to_type(actual)),
        ))
    }
}

/// All runtime type failures use this prefix so they're clearly distinguishable.
fn runtime_type_error(expected: &str, got: &str) -> String {
    format!("Runtime type error: expected {}, got {}", expected, got)
}

fn property_key(v: &Value) -> Result<String, String> {
    match v {
        Value::String(s) => Ok(s.clone()),
        Value::Number(n) => Ok(n.to_string()),
        _ => Err("Property key must be string or number".into()),
    }
}

fn get_property(obj: &Value, key: &str) -> Result<Value, String> {
    match obj {
        Value::Object(map) => {
            let m = map.borrow();
            if let Some(v) = m.get(key) {
                return Ok(v.clone());
            }
            // Prototype chain: if the object has a `__proto__` slot that is
            // non-null, delegate property lookup to it. This lets soft-layer
            // prototypes (Array.prototype, Object.prototype) provide methods.
            if let Some(proto) = m.get("__proto__")
                && !matches!(proto, Value::Null)
            {
                return get_property(proto, key);
            }
            Ok(Value::Null)
        }
        Value::Array(arr, props) => {
            // Numeric index access
            if let Ok(i) = key.parse::<usize>() {
                return Ok(arr.borrow().get(i).cloned().unwrap_or(Value::Null));
            }
            // Non-index property: check array's props (length, __proto__, etc.)
            let p = props.borrow();
            if let Some(v) = p.get(key) {
                return Ok(v.clone());
            }
            // If props contain a __proto__ link, delegate to it
            if let Some(proto) = p.get("__proto__")
                && !matches!(proto, Value::Null)
            {
                return get_property(proto, key);
            }
            Ok(Value::Null)
        }
        Value::Closure { props, .. } => Ok(props.borrow().get(key).cloned().unwrap_or(Value::Null)),
        _ => Err("GetProperty: not an object".into()),
    }
}

fn set_property(obj: &Value, key: &str, value: Value) -> Result<(), String> {
    match obj {
        Value::Object(map) => {
            map.borrow_mut().insert(key.to_string(), value);
            Ok(())
        }
        Value::Array(arr, props) => {
            // Numeric index assignment
            if let Ok(i) = key.parse::<usize>() {
                let mut a = arr.borrow_mut();
                while a.len() <= i {
                    a.push(Value::Null);
                }
                a[i] = value;
                return Ok(());
            }
            // Non-index property: set on props map
            props.borrow_mut().insert(key.to_string(), value);
            Ok(())
        }
        Value::Closure { props, .. } => {
            props.borrow_mut().insert(key.to_string(), value);
            Ok(())
        }
        _ => {
            if crate::trace::enabled() {
                eprintln!(
                    "[trace] SetProperty: received non-object value type='{}' value='{}'",
                    obj.type_name(),
                    obj
                );
            }
            Err(format!(
                "SetProperty: not an object (actual={})",
                obj.type_name()
            ))
        }
    }
}

// PartialOrd for Value for Lt/Le/Gt/Ge
impl PartialOrd for Value {
    fn partial_cmp(&self, other: &Value) -> Option<std::cmp::Ordering> {
        match (self, other) {
            (Value::Number(a), Value::Number(b)) => a.partial_cmp(b),
            (Value::String(a), Value::String(b)) => a.partial_cmp(b),
            _ => None,
        }
    }
}
