//! CLI: run TS files or REPL. Bootstrap soft layer (optional), then user code.

use std::cell::RefCell;
use std::collections::HashMap;
use std::env;
use std::path::Path;
use std::rc::Rc;

use tsblitz::bytecode;
use tsblitz::{Program, bundle_entry, compile, parser::Parser, typecheck, value::Value, vm::Vm};

/// Minimal native hook: print to stdout. The TS soft layer wraps this as console.log.
fn native_print(_vm: &mut Vm, args: &[Value]) -> Result<Value, String> {
    // Diagnostic wrapper: emit arg debug then print joined string
    if tsblitz::trace::enabled() {
        eprintln!("[diag] host.native_print called nargs={}", args.len());
        for (i, v) in args.iter().enumerate() {
            tsblitz::trace::value_string(&format!("host.native_print arg[{}]", i), v);
            eprintln!(
                "[diag] host.native_print debug arg[{}] type={}",
                i,
                v.type_name()
            );
        }
    }
    let s: String = args
        .iter()
        .map(|v| format!("{}", v))
        .collect::<Vec<_>>()
        .join(" ");
    if tsblitz::trace::enabled() {
        eprintln!(
            "[diag] host.native_print joined len={} repr={:?}",
            s.len(),
            s
        );
    }
    // Print without adding a newline (preserve previous behavior) but
    // explicitly flush stdout so short-lived processes still show output.
    use std::io::Write;
    print!("{}", s);
    let _ = std::io::stdout().flush();
    Ok(Value::Null)
}

// Host bindings are published directly into the VM globals (`globalThis`) so
// soft-layer code can access them as regular globals (e.g. `globalThis.__native_print`).

/// Runs source code through the full compilation pipeline
///
/// # Errors
///
/// Returns an error string if any stage of compilation fails
fn run_source(vm: &mut Vm, source: &str) -> Result<Value, String> {
    let program = Parser::parse_source(source).map_err(|e| format!("Parse error: {}", e))?;
    typecheck(&program).map_err(|e| format!("Type error: {}", e))?;
    let chunk = compile(&program).map_err(|e| format!("Compilation error: {}", e))?;
    vm.run(chunk).map_err(|e| format!("Runtime error: {}", e))
}

fn run_program(vm: &mut Vm, program: Program) -> Result<Value, String> {
    typecheck(&program).map_err(|e| format!("Type error in program: {}", e))?;
    let chunk = compile(&program).map_err(|e| format!("Compilation error in program: {}", e))?;
    vm.run(chunk)
        .map_err(|e| format!("Runtime error in program: {}", e))
}

/// Clear slot type annotations recursively for a Chunk graph.
///
/// The soft prelude is intentionally dynamic and may pass values that don't
/// match static parameter annotations. Before executing the prelude we clear
/// slot types in every FunctionProto across the compiled chunk graph so the
/// VM won't enforce runtime type checks for those functions.
fn clear_slot_types_in_chunk(chunk: bytecode::Chunk) -> bytecode::Chunk {
    use std::collections::HashMap;
    use std::rc::Rc;

    // Recursive helper that rebuilds chunks and their nested function chunks.
    fn rebuild(
        rc_chunk: &Rc<bytecode::Chunk>,
        visited: &mut HashMap<*const bytecode::Chunk, Rc<bytecode::Chunk>>,
    ) -> Rc<bytecode::Chunk> {
        let key = Rc::as_ptr(rc_chunk) as *const bytecode::Chunk;
        if let Some(existing) = visited.get(&key) {
            return Rc::clone(existing);
        }

        // Rebuild the chunk fields, replacing functions with cleared slot types.
        let mut new_chunk = bytecode::Chunk::new();
        new_chunk.code = rc_chunk.code.clone();
        new_chunk.constants = rc_chunk.constants.clone();
        new_chunk.lines = rc_chunk.lines.clone();
        // Clear global type annotations for the soft prelude so runtime won't
        // enforce static global types during bootstrap. Replace any annotated
        // type with `None` to disable runtime checks for globals.
        new_chunk.global_types = rc_chunk
            .global_types
            .keys()
            .map(|k| (k.clone(), None))
            .collect();

        // Process functions
        let mut new_funcs = Vec::new();
        for f in &rc_chunk.functions {
            // Rebuild nested chunk for this function
            let nested = rebuild(&f.chunk, visited);
            let mut nf = f.clone();
            // Replace all slot type annotations and return type with None
            nf.slot_types = nf.slot_types.iter().map(|_| None).collect();
            nf.return_type = None;
            nf.chunk = nested;
            new_funcs.push(nf);
        }
        new_chunk.functions = new_funcs;

        let rc_new = Rc::new(new_chunk);
        visited.insert(key, Rc::clone(&rc_new));
        rc_new
    }

    let rc = Rc::new(chunk);
    let mut visited: HashMap<*const bytecode::Chunk, Rc<bytecode::Chunk>> = HashMap::new();
    let rc_cleared = rebuild(&rc, &mut visited);
    // Return owned Chunk
    (*rc_cleared).clone()
}

// Textual import inlining was removed. The project now uses a proper
// bundler/module-graph loader that parses modules and inlines them at the
// AST level. Use `bundle_entry()` (from the `bundler` module) to build a
// combined `Program` for an entry file that resolves relative imports and
// inlines module bodies in dependency order.

fn main() -> Result<(), String> {
    // Public globals visible to user code (populated by the prelude)
    let globals = Rc::new(RefCell::new(HashMap::new()));

    // Provide a `globalThis` object that reflects the VM globals map so soft
    // layer code that writes `globalThis.x = ...` updates the VM globals.
    // We reuse the same Rc<RefCell<HashMap>> so property ops on the object and
    // Get/SetGlobal access the same backing map.
    let global_this = Value::Object(Rc::clone(&globals));
    globals
        .borrow_mut()
        .insert("globalThis".to_string(), global_this);

    // Publish native host bindings directly into globals so soft code can read
    // them as ordinary globals (e.g. `globalThis.__native_print`). This keeps
    // the runtime simpler and avoids an extra indirection.
    globals
        .borrow_mut()
        .insert("__native_print".to_string(), Value::Builtin(native_print));

    // Provide a raw native_new_object builtin that returns a VM-backed object
    // suitable for use as a prototype during bootstrap.
    fn native_new_object(_vm: &mut Vm, _args: &[Value]) -> Result<Value, String> {
        use std::cell::RefCell;
        use std::collections::HashMap;
        use std::rc::Rc;
        let v = Value::Object(Rc::new(RefCell::new(HashMap::new())));
        if tsblitz::trace::enabled() {
            tsblitz::trace::value_string("native_new_object ->", &v);
        }
        Ok(v)
    }

    // Minimal callable placeholder for `Object` constructor used only for
    // early bootstrap. It creates a plain object and links its `__proto__`
    // to the canonical `Object` prototype stored under the private key
    // `__Object_ctor_obj` (inserted below).
    fn native_object_ctor(vm: &mut Vm, _args: &[Value]) -> Result<Value, String> {
        use std::cell::RefCell;
        use std::collections::HashMap;
        use std::rc::Rc;
        // Create a fresh object instance.
        let new_obj = Value::Object(Rc::new(RefCell::new(HashMap::new())));
        // Try to link its __proto__ from the stored canonical constructor object.
        if let Some(ctor_obj) = vm.get_global("__Object_ctor_obj")
            && let Value::Object(map) = ctor_obj
            && let Some(proto) = map.borrow().get("prototype")
            && let Value::Object(inst_map) = &new_obj
        {
            inst_map
                .borrow_mut()
                .insert("__proto__".to_string(), proto.clone());
        }
        Ok(new_obj)
    }

    // Minimal callable placeholder for `Array` constructor used only for
    // early bootstrap. It creates a fresh array and links its `__proto__`
    // to the canonical `Array.prototype` stored under `__Array_ctor_obj`.
    fn native_array_ctor(vm: &mut Vm, _args: &[Value]) -> Result<Value, String> {
        use std::cell::RefCell;
        use std::collections::HashMap;
        use std::rc::Rc;
        // Create an empty dense array and empty props map.
        let new_arr = Value::Array(
            Rc::new(RefCell::new(Vec::new())),
            Rc::new(RefCell::new(HashMap::new())),
        );
        // Link props.__proto__ from stored ctor object if available.
        if let Some(ctor_obj) = vm.get_global("__Array_ctor_obj")
            && let Value::Object(map) = ctor_obj
            && let Some(proto) = map.borrow().get("prototype")
            && let Value::Array(_arr, props) = &new_arr
        {
            props
                .borrow_mut()
                .insert("__proto__".to_string(), proto.clone());
        }
        Ok(new_arr)
    }

    globals.borrow_mut().insert(
        "__native_new_object".to_string(),
        Value::Builtin(native_new_object),
    );

    // Provide a minimal host-backed `console` object so early bootstrap code
    // that expects `console.log` to be callable does not fail if the soft
    // prelude or host environment provides a non-callable value. We expose
    // simple builtins that forward to `native_print` so any call is treated
    // as a VM-callable builtin rather than a plain object property.
    {
        use std::cell::RefCell;
        use std::collections::HashMap;
        use std::rc::Rc;

        let mut console_map: HashMap<String, Value> = HashMap::new();
        // All console methods forward to the same native printer for now.
        console_map.insert("log".to_string(), Value::Builtin(native_print));
        console_map.insert("info".to_string(), Value::Builtin(native_print));
        console_map.insert("warn".to_string(), Value::Builtin(native_print));
        console_map.insert("error".to_string(), Value::Builtin(native_print));
        console_map.insert("debug".to_string(), Value::Builtin(native_print));

        globals.borrow_mut().insert(
            "console".to_string(),
            Value::Object(Rc::new(RefCell::new(console_map))),
        );
    }

    // Note: no host accessor indirection is published. Soft code should read
    // direct globals like `__native_print` and `__native_new_object`.

    // Enable soft-layer debugValue prints when tracing is desired. This sets
    // the JS-visible `__TSBLITZ_TRACE` flag so the soft `host.debugValue`
    // helper will emit additional diagnostic lines during bootstrap.
    globals
        .borrow_mut()
        .insert("__TSBLITZ_TRACE".to_string(), Value::Bool(true));

    // Note: we intentionally publish native host bindings into `globals` so
    // soft code can read them directly. This simplifies the runtime and
    // removes the `__host_get` indirection.

    // Load the soft layer.
    // Bundle the TypeScript soft layer (prelude + relative imports) into a single Program
    // using the bundler/loader and execute it so the soft layer can install globals
    // (console, Object, Array, ...).
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let prelude_path = manifest_dir.join("soft/prelude.ts");
    let prelude_program = bundle_entry(&prelude_path).map_err(|e| {
        format!(
            "Failed to load soft layer prelude '{}': {}",
            prelude_path.display(),
            e
        )
    })?;

    // Skip type checking for soft layer prelude (has dynamic patterns that typechecker doesn't handle yet)
    let chunk =
        compile(&prelude_program).map_err(|e| format!("Compilation error in soft layer: {}", e))?;
    // The soft prelude is written in TypeScript and contains dynamic patterns
    // and annotated helper types that the static compiler may record as
    // parameter types. The prelude is intended to run untyped at runtime,
    // so clear function slot type annotations here to avoid enforcing runtime
    // type checks during prelude initialization.
    // Clear slot types recursively across the chunk graph so the dynamic
    // soft prelude isn't blocked by runtime type checks that expect stricter
    // static types.
    let chunk = clear_slot_types_in_chunk(chunk);
    // One-time diagnostic: print compiled chunk constants and function prototypes
    // so we can map bytecode ip locations back to source-level functions during debugging.
    if tsblitz::trace::enabled() {
        eprintln!(
            "[trace] Prelude chunk: code_len={} constants={}",
            chunk.code.len(),
            chunk.constants.len()
        );
        eprintln!("[trace] Prelude functions (index -> name, arity, bytecode_len):");
        for (i, f) in chunk.functions.iter().enumerate() {
            eprintln!(
                "[trace]  fn[{}] name='{}' arity={} bytecode_len={}",
                i,
                f.name,
                f.arity,
                f.chunk.code.len()
            );
        }
        // Print annotated bytecode for each function: opcode, operand preview, and source line
        for (i, f) in chunk.functions.iter().enumerate() {
            eprintln!(
                "[trace] --- fn[{}] '{}' bytecode (len={}) ---",
                i,
                f.name,
                f.chunk.code.len()
            );
            let mut ip = 0usize;
            while ip < f.chunk.code.len() {
                let b = f.chunk.code[ip];
                let op = bytecode::Op::from_byte(b)
                    .map_or_else(|| format!("0x{:02x}", b), |o| format!("{:?}", o));
                // line number for this opcode if available
                let line = if ip < f.chunk.lines.len() {
                    f.chunk.lines[ip]
                } else {
                    0
                };
                let mut extra = String::new();
                match bytecode::Op::from_byte(b) {
                    Some(bytecode::Op::Const)
                    | Some(bytecode::Op::GetGlobal)
                    | Some(bytecode::Op::SetGlobal)
                    | Some(bytecode::Op::GetBuiltin)
                    | Some(bytecode::Op::MakeClosure)
                    | Some(bytecode::Op::GetPropertyIC)
                    | Some(bytecode::Op::Call) => {
                        if ip + 1 < f.chunk.code.len() {
                            let v = f.chunk.code[ip + 1] as usize;
                            extra = format!(" {}", v);
                        }
                    }
                    Some(bytecode::Op::Jump)
                    | Some(bytecode::Op::JumpIfFalse)
                    | Some(bytecode::Op::PushHandler) => {
                        if ip + 2 < f.chunk.code.len() {
                            let hi = f.chunk.code[ip + 1] as usize;
                            let lo = f.chunk.code[ip + 2] as usize;
                            let short = ((hi << 8) | lo) as i16;
                            extra = format!(" {}", short);
                        }
                    }
                    _ => {}
                }
                eprintln!(
                    "[trace] fn[{}] {:04} (line {})  {}{}",
                    i, ip, line, op, extra
                );
                ip += 1;
                match bytecode::Op::from_byte(b) {
                    Some(bytecode::Op::Const)
                    | Some(bytecode::Op::GetGlobal)
                    | Some(bytecode::Op::SetGlobal)
                    | Some(bytecode::Op::GetBuiltin)
                    | Some(bytecode::Op::MakeClosure)
                    | Some(bytecode::Op::GetPropertyIC)
                    | Some(bytecode::Op::Call) => ip += 1,
                    Some(bytecode::Op::Jump)
                    | Some(bytecode::Op::JumpIfFalse)
                    | Some(bytecode::Op::PushHandler) => ip += 2,
                    _ => {}
                }
            }
        }
        for (i, c) in chunk.constants.iter().enumerate() {
            match c {
                Value::String(s) => eprintln!("[trace] const[{}] (string) = {:?}", i, s),
                Value::Number(n) => eprintln!("[trace] const[{}] (number) = {}", i, n),
                Value::Bool(b) => eprintln!("[trace] const[{}] (bool) = {}", i, b),
                Value::Null => eprintln!("[trace] const[{}] = null", i),
                other => eprintln!("[trace] const[{}] (other) = {}", i, other.type_name()),
            }
        }
        // Very small disassembly to map ip -> opcode and operand (one-time)
        eprintln!("[trace] Prelude chunk disassembly (first 2000 bytes):");
        let mut ip = 0usize;
        while ip < chunk.code.len() {
            let b = chunk.code[ip];
            let op = bytecode::Op::from_byte(b)
                .map_or_else(|| format!("0x{:02x}", b), |o| format!("{:?}", o));
            // Prepare a short operand preview depending on opcode
            let mut extra = String::new();
            match bytecode::Op::from_byte(b) {
                Some(bytecode::Op::Const)
                | Some(bytecode::Op::GetGlobal)
                | Some(bytecode::Op::SetGlobal)
                | Some(bytecode::Op::GetBuiltin)
                | Some(bytecode::Op::MakeClosure)
                | Some(bytecode::Op::GetPropertyIC)
                | Some(bytecode::Op::Call) => {
                    if ip + 1 < chunk.code.len() {
                        let v = chunk.code[ip + 1] as usize;
                        extra = format!(" {}", v);
                    }
                }
                Some(bytecode::Op::Jump)
                | Some(bytecode::Op::JumpIfFalse)
                | Some(bytecode::Op::PushHandler) => {
                    if ip + 2 < chunk.code.len() {
                        let hi = chunk.code[ip + 1] as usize;
                        let lo = chunk.code[ip + 2] as usize;
                        let short = ((hi << 8) | lo) as i16;
                        extra = format!(" {}", short);
                    }
                }
                _ => {}
            }
            eprintln!("[trace] {:04}  {}{}", ip, op, extra);
            // Advance ip by opcode + operand length heuristically
            ip += 1;
            match bytecode::Op::from_byte(b) {
                Some(bytecode::Op::Const)
                | Some(bytecode::Op::GetGlobal)
                | Some(bytecode::Op::SetGlobal)
                | Some(bytecode::Op::GetBuiltin)
                | Some(bytecode::Op::MakeClosure)
                | Some(bytecode::Op::GetPropertyIC)
                | Some(bytecode::Op::Call) => ip += 1,
                Some(bytecode::Op::Jump)
                | Some(bytecode::Op::JumpIfFalse)
                | Some(bytecode::Op::PushHandler) => ip += 2,
                _ => {}
            }
            if ip > 2000 {
                break;
            }
        }
    }
    // Create a persistent VM for the REPL and reuse it for every input so
    // we don't recompile and recreate the VM on each line. The soft layer
    // (prelude) is executed into the same VM so its globals and any
    // runtime-installed state are available to the REPL.
    // Create VM and run prelude. blitzlib can call `__host_get(name)` during
    // bootstrap to capture host-provided values; raw host entries are never
    // placed into the globals table.
    let mut repl_vm = Vm::new(globals.clone());

    // Kernel bootstrap: create canonical native-backed constructors and
    // prototype objects so soft-layer bootstrap can assume these host-level
    // objects exist and safely assign properties on them. We keep these
    // minimal (non-callable) objects that expose a `prototype` slot and the
    // usual `__proto__` / `constructor` links. This prevents early soft-layer
    // code from encountering null or host-backed prototypes during setup.
    {
        use std::collections::HashMap;

        // Create prototype objects
        let object_prototype = Value::Object(Rc::new(RefCell::new(HashMap::new())));
        let function_prototype = Value::Object(Rc::new(RefCell::new(HashMap::new())));
        let array_prototype = Value::Object(Rc::new(RefCell::new(HashMap::new())));

        // Function.prototype.__proto__ -> Object.prototype
        if let Value::Object(fp_map) = &function_prototype {
            fp_map
                .borrow_mut()
                .insert("__proto__".to_string(), object_prototype.clone());
        }

        // Object.prototype.__proto__ -> null (explicitly leave as Null)

        // Create constructor objects and link their `prototype` slot.
        let object_ctor_map = Rc::new(RefCell::new(HashMap::new()));
        object_ctor_map
            .borrow_mut()
            .insert("prototype".to_string(), object_prototype.clone());
        // constructors inherit from Function.prototype
        object_ctor_map
            .borrow_mut()
            .insert("__proto__".to_string(), function_prototype.clone());
        let object_ctor = Value::Object(Rc::clone(&object_ctor_map));

        let function_ctor_map = Rc::new(RefCell::new(HashMap::new()));
        function_ctor_map
            .borrow_mut()
            .insert("prototype".to_string(), function_prototype.clone());
        // Function constructor's __proto__ is Function.prototype (self-referential)
        function_ctor_map
            .borrow_mut()
            .insert("__proto__".to_string(), function_prototype.clone());
        let function_ctor = Value::Object(Rc::clone(&function_ctor_map));

        let array_ctor_map = Rc::new(RefCell::new(HashMap::new()));
        array_ctor_map
            .borrow_mut()
            .insert("prototype".to_string(), array_prototype.clone());
        array_ctor_map
            .borrow_mut()
            .insert("__proto__".to_string(), function_prototype.clone());
        let array_ctor = Value::Object(Rc::clone(&array_ctor_map));

        // Link prototype.constructor -> ctor
        if let Value::Object(op_map) = &object_prototype {
            op_map
                .borrow_mut()
                .insert("constructor".to_string(), object_ctor.clone());
        }
        if let Value::Object(fp_map) = &function_prototype {
            fp_map
                .borrow_mut()
                .insert("constructor".to_string(), function_ctor.clone());
        }
        if let Value::Object(ap_map) = &array_prototype {
            ap_map
                .borrow_mut()
                .insert("constructor".to_string(), array_ctor.clone());
        }

        // Insert into globals so VM and soft layer see canonical entries. Do not
        // overwrite if the prelude or environment already set them earlier.
        let mut g = globals.borrow_mut();
        // Store raw ctor objects under private keys so the callable placeholders
        // below can use their `prototype` slots to link new instances.
        g.entry("__Object_ctor_obj".to_string())
            .or_insert(object_ctor);
        g.entry("Function".to_string()).or_insert(function_ctor);
        g.entry("__Array_ctor_obj".to_string())
            .or_insert(array_ctor);

        // Prefer to leave an explicitly provided `Object`/`Array` in place, but
        // ensure that a callable placeholder exists if the current entry isn't
        // callable (callable = Builtin or Closure). This avoids "Call: not a
        // function" during early bootstrap while keeping constructor prototype
        // objects available via the private keys above.
        // Insert placeholders only when the global is missing or not callable.
        match g.get("Object") {
            Some(v) => {
                if !matches!(v, Value::Builtin(_) | Value::Closure { .. }) {
                    g.insert("Object".to_string(), Value::Builtin(native_object_ctor));
                    if tsblitz::trace::enabled() {
                        eprintln!("[trace] Inserted callable placeholder for Object");
                    }
                }
            }
            None => {
                g.insert("Object".to_string(), Value::Builtin(native_object_ctor));
                if tsblitz::trace::enabled() {
                    eprintln!("[trace] Inserted callable placeholder for Object (missing)");
                }
            }
        }

        match g.get("Array") {
            Some(v) => {
                if !matches!(v, Value::Builtin(_) | Value::Closure { .. }) {
                    g.insert("Array".to_string(), Value::Builtin(native_array_ctor));
                    if tsblitz::trace::enabled() {
                        eprintln!("[trace] Inserted callable placeholder for Array");
                    }
                }
            }
            None => {
                g.insert("Array".to_string(), Value::Builtin(native_array_ctor));
                if tsblitz::trace::enabled() {
                    eprintln!("[trace] Inserted callable placeholder for Array (missing)");
                }
            }
        }
    }

    // Run prelude and capture errors to emit additional diagnostics useful when
    // host/soft bridging fails (e.g. Call: not a function). When the prelude
    // fails, print selected globals and their runtime types to stderr so the
    // developer can inspect what the soft layer saw at bootstrap time.
    if tsblitz::trace::enabled() {
        eprintln!("[diag pre-prelude] Snapshot of globals before running prelude:");
        for name in &[
            "console",
            "__native_print",
            "__native_new_object",
            "globalThis",
            "Object",
            "Array",
        ] {
            match globals.borrow().get(*name) {
                Some(v) => eprintln!(
                    "[diag pre-prelude] global '{}' type={}",
                    name,
                    v.type_name()
                ),
                None => eprintln!("[diag pre-prelude] global '{}' NOT present", name),
            }
        }
    }
    match repl_vm.run(chunk) {
        Ok(_) => {}
        Err(e) => {
            eprintln!("Runtime error in soft layer: {}", e);
            if tsblitz::trace::enabled() {
                eprintln!("[diag] Dumping selected globals for diagnosis:");
            }
            // Inspect common suspects
            for name in &[
                "console",
                "__native_print",
                "__native_new_object",
                "globalThis",
                "Object",
                "Array",
            ] {
                match repl_vm.get_global(name) {
                    Some(v) => {
                        eprintln!("[diag] global '{}' type={}", name, v.type_name());
                        if tsblitz::trace::enabled() {
                            if let Some(keys) = repl_vm.list_global_own_keys(name) {
                                eprintln!("[diag]   own keys = {:?}", keys);
                            }
                            if *name == "console"
                                && let Some(val) = repl_vm.get_global("console")
                            {
                                // Try to print the console object's own keys and whether
                                // it has a 'log' key; avoid calling private helpers.
                                if let Some(keys) = repl_vm.list_own_keys_of_value(&val) {
                                    eprintln!("[diag]   console own keys = {:?}", keys);
                                    eprintln!(
                                        "[diag]   console.has_log = {}",
                                        keys.iter().any(|k| k == "log")
                                    );
                                }
                            }
                        }
                    }
                    None => eprintln!("[diag] global '{}' NOT present", name),
                }
            }
            return Err(format!("Runtime error in soft layer: {}", e));
        }
    }

    // Ensure Array.prototype exists: some bootstrap paths in the soft prelude
    // may not have been able to attach a prototype object onto the soft
    // `Array` implementation. If `Array.prototype` is missing or not an
    // object, create a native-backed object and link it so array instances
    // created by the VM have a usable prototype for methods like `push`.
    if let Some(array_val) = repl_vm.get_global("Array")
        && let Value::Object(map_rc) = array_val
    {
        let mut map = map_rc.borrow_mut();
        if !map.contains_key("prototype") || matches!(map.get("prototype"), Some(Value::Null)) {
            // Create a native-backed empty object for prototype
            use std::cell::RefCell;
            use std::collections::HashMap;
            use std::rc::Rc;
            let proto = Value::Object(Rc::new(RefCell::new(HashMap::new())));
            map.insert("prototype".to_string(), proto);
        }
    }

    if tsblitz::trace::enabled() {
        eprintln!("[trace] Globals after prelude:");
        // Use the Vm API to inspect globals so we don't access private fields.
        if repl_vm.get_global("Array").is_some() {
            if let Some(keys) = repl_vm.list_global_own_keys("Array") {
                eprintln!("[trace] Array global own keys = {:?}", keys);
            } else {
                eprintln!("[trace] Array global present (own keys unavailable)");
            }
            if let Some(pkeys) = repl_vm.list_global_prototype_keys("Array") {
                eprintln!("[trace] Array.prototype own keys = {:?}", pkeys);
            } else {
                eprintln!("[trace] Array.prototype not found or not object");
            }
        }
        // Also print console and host symbols so we can diagnose missing bindings.
        if repl_vm.get_global("console").is_some() {
            if let Some(keys) = repl_vm.list_global_own_keys("console") {
                eprintln!("[trace] console global own keys = {:?}", keys);
            } else {
                eprintln!("[trace] console global present (own keys unavailable)");
            }
        } else {
            eprintln!("[trace] console global NOT present");
        }

        if let Some(v) = repl_vm.get_global("__host_get") {
            eprintln!("[trace] __host_get present type={}", v.type_name());
        } else {
            eprintln!("[trace] __host_get NOT present");
        }

        if let Some(v) = repl_vm.get_global("__native_print") {
            eprintln!("[trace] __native_print present type={}", v.type_name());
        } else {
            eprintln!("[trace] __native_print NOT present");
        }
    }

    let args: Vec<String> = env::args().collect();
    if args.len() > 1 {
        let path = Path::new(&args[1]);
        let program = bundle_entry(path)?;
        run_program(&mut repl_vm, program)?;
        return Ok(());
    }

    // REPL with line-editing and history
    println!("Blitz — TypeScript VM (Rust runtime). Type exit to quit.");
    let mut rl = rustyline::Editor::<(), rustyline::history::DefaultHistory>::new()
        .map_err(|e| format!("Readline init error: {}", e))?;
    let history_path = env::temp_dir().join("blitz_repl_history");
    let _ = rl.load_history(&history_path);
    loop {
        match rl.readline("> ") {
            Ok(input) => {
                let line = input.trim();
                if line.is_empty() {
                    continue;
                }
                let _ = rl.add_history_entry(line);
                let _ = rl.save_history(&history_path);
                if line.eq_ignore_ascii_case("exit") || line.eq_ignore_ascii_case("quit") {
                    break;
                }
                match run_source(&mut repl_vm, line) {
                    Ok(v) if !matches!(v, Value::Null) => println!("{}", v),
                    Ok(_) => {}
                    Err(e) => eprintln!("Error: {}", e),
                }
            }
            Err(rustyline::error::ReadlineError::Interrupted)
            | Err(rustyline::error::ReadlineError::Eof) => break,
            Err(e) => {
                eprintln!("Readline error: {}", e);
                break;
            }
        }
    }
    Ok(())
}
