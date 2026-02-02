//! CLI: run TS files or REPL. Bootstrap soft layer (optional), then user code.

use std::cell::RefCell;
use std::collections::HashMap;
use std::env;
use std::path::Path;
use std::rc::Rc;
use std::thread_local;

use tsblitz::bytecode;
use tsblitz::{Program, bundle_entry, compile, parser::Parser, typecheck, value::Value, vm::Vm};

/// Minimal native hook: print to stdout. The TS soft layer wraps this as console.log.
fn native_print(_vm: &mut Vm, args: &[Value]) -> Result<Value, String> {
    if tsblitz::trace::enabled() {
        for (i, v) in args.iter().enumerate() {
            tsblitz::trace::value_string(&format!("native_print arg[{}]", i), v);
        }
    }
    let s: String = args
        .iter()
        .map(|v| format!("{}", v))
        .collect::<Vec<_>>()
        .join(" ");
    if tsblitz::trace::enabled() {
        eprintln!("[trace] native_print joined len={} repr={:?}", s.len(), s);
    }
    println!("{}", s);
    Ok(Value::Null)
}

thread_local! {
    // Per-thread host table reference. We store an Rc to the real table so
    // the builtin `host_get` can access host-provided Values without making
    // the table a Sync static.
    static HOST_TABLE_TLS: RefCell<Option<Rc<RefCell<HashMap<String, Value>>>>> = const { RefCell::new(None) };
}

/// Accessor: return a host-provided Value by name from the private host table.
fn host_get(_vm: &mut Vm, args: &[Value]) -> Result<Value, String> {
    if args.is_empty() {
        return Err("host_get: name required".into());
    }
    match &args[0] {
        Value::String(s) => HOST_TABLE_TLS.with(|cell| {
            let opt = cell.borrow();
            let table_rc = opt
                .as_ref()
                .ok_or("host table not initialized".to_string())?;
            let tbl = table_rc.borrow();
            let res = tbl.get(s).cloned();
            if tsblitz::trace::enabled() {
                if let Some(v) = &res {
                    tsblitz::trace::value_string(&format!("host_get('{}') ->", s), v);
                } else {
                    eprintln!("[trace] host_get('{}') -> None", s);
                }
            }
            res.ok_or(format!("host_get: '{}' not found", s))
        }),
        _ => Err("host_get: name must be string".into()),
    }
}

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

    // Kernel <-> blitzlib private host table: native hooks live here and are
    // not exposed to user code. Populate the global OnceLock so `host_get`
    // can access entries; never place raw host functions directly into the
    // VM globals table.
    let host_map_rc: Rc<RefCell<HashMap<String, Value>>> = Rc::new(RefCell::new(HashMap::new()));
    // Populate host_map_rc with native entries
    host_map_rc
        .borrow_mut()
        .insert("__native_print".to_string(), Value::Builtin(native_print));

    // Note: host-provided primitives (like __native_print) live in the
    // private host_map_rc table. They are intentionally not placed directly
    // into the VM globals to avoid leaking raw host functions into user code.

    // Native helper to create a raw VM object (HashMap-backed) from Rust.
    // This bypasses any soft-layer constructor and is useful during bootstrap
    // when the soft Object API may not be fully initialized or when we want
    // a guaranteed object Value to use as a prototype.
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
    host_map_rc.borrow_mut().insert(
        "__native_new_object".to_string(),
        Value::Builtin(native_new_object),
    );

    // Initialize TLS host table so `host_get` can access host-provided values.
    HOST_TABLE_TLS.with(|cell| {
        *cell.borrow_mut() = Some(Rc::clone(&host_map_rc));
    });

    // Insert a single, minimal accessor into globals so blitzlib can call it
    // during bootstrap and capture host references into its private bindings.
    globals
        .borrow_mut()
        .insert("__host_get".to_string(), Value::Builtin(host_get));

    // Do not expose host shims (host_print / __native_print) directly in
    // the globals table — soft layer code should obtain host helpers via
    // __host_get to preserve the separation between host and VM.

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
        g.entry("Object".to_string()).or_insert(object_ctor);
        g.entry("Function".to_string()).or_insert(function_ctor);
        g.entry("Array".to_string()).or_insert(array_ctor);
    }

    repl_vm
        .run(chunk)
        .map_err(|e| format!("Runtime error in soft layer: {}", e))?;

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

    // Do not inject an ad-hoc callable `String` builtin here. The soft
    // prelude should use __host_get to resolve host values or rely on the
    // runtime to provide canonical constructors when available.

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
    println!("tsblitz — TypeScript VM (Rust runtime). Type exit to quit.");
    let mut rl = rustyline::Editor::<(), rustyline::history::DefaultHistory>::new()
        .map_err(|e| format!("Readline init error: {}", e))?;
    let history_path = env::temp_dir().join("tsblitz_repl_history");
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
