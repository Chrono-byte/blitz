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

    // Join the arguments into a single string to send to host stdout.
    let s: String = args
        .iter()
        .map(|v| format!("{}", v))
        .collect::<Vec<_>>()
        .join(" ");

    // Preserve existing trace logging when enabled.
    if tsblitz::trace::enabled() {
        eprintln!(
            "[diag] host.native_print joined len={} repr={:?}",
            s.len(),
            s
        );
    }

    // Additional unconditional diagnostic: write a single-line debug to stderr so
    // we can see what the runtime attempted to print even when trace is not enabled.
    // This helps diagnose cases where stdout output appears missing.
    eprintln!("[diag] host.native_print -> {:?}", s);

    // Print without adding a newline (preserve previous behavior) but
    // explicitly flush stdout so short-lived processes still show output.
    use std::io::Write;
    print!("{}", s);
    let _ = std::io::stdout().flush();
    // Also flush stderr to ensure the diagnostic line is visible immediately.
    let _ = std::io::stderr().flush();

    Ok(Value::Null)
}

// Host bindings are published directly into the VM globals (`globalThis`) so
// soft-layer code can access them as regular globals (e.g. `globalThis.__native_print`).

/// Runs source code through the full compilation pipeline
///
/// # Errors
///
/// Returns an error string if any stage of compilation fails
fn run_source(vm: &mut Vm, source: &str) -> Result<Value, tsblitz::errors::Error> {
    let session = Parser::parse_source(source).map_err(tsblitz::errors::ParserError::from)?;
    let program = session.program();
    typecheck(program).map_err(tsblitz::errors::TypeCheckerError::from)?;
    let chunk = compile(program).map_err(tsblitz::errors::CompilerError::from)?;
    vm.run(chunk).map_err(tsblitz::errors::Error::from)
}

fn run_program(vm: &mut Vm, program: Program) -> Result<Value, tsblitz::errors::Error> {
    typecheck(&program).map_err(tsblitz::errors::TypeCheckerError::from)?;
    let chunk = compile(&program).map_err(tsblitz::errors::CompilerError::from)?;
    vm.run(chunk).map_err(tsblitz::errors::Error::from)
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
    Rc::try_unwrap(rc_cleared).unwrap_or_else(|rc| (*rc).clone())
}

// Textual import inlining was removed. The project now uses a proper
// bundler/module-graph loader that parses modules and inlines them at the
// AST level. Use `bundle_entry()` (from the `bundler` module) to build a
// combined `Program` for an entry file that resolves relative imports and
// inlines module bodies in dependency order.

fn main() -> Result<(), tsblitz::errors::Error> {
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
    globals.borrow_mut().insert(
        "__native_print".to_string(),
        Value::Builtin(|vm, args| {
            native_print(vm, args).map_err(tsblitz::errors::RuntimeError::from)
        }),
    );

    // Minimal native hook for creating VM-backed objects.
    fn native_new_object(_vm: &mut Vm, _args: &[Value]) -> Result<Value, String> {
        use std::cell::RefCell;
        use std::collections::HashMap;
        use std::rc::Rc;
        Ok(Value::Object(Rc::new(RefCell::new(HashMap::new()))))
    }

    globals.borrow_mut().insert(
        "__native_new_object".to_string(),
        Value::Builtin(|vm, args| {
            native_new_object(vm, args).map_err(tsblitz::errors::RuntimeError::from)
        }),
    );

    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let prelude_path = manifest_dir.join("soft/prelude.ts");
    let prelude_session =
        bundle_entry(&prelude_path).map_err(tsblitz::errors::BundlerError::from)?;

    // Skip type checking for soft layer prelude (has dynamic patterns that typechecker doesn't handle yet)
    let chunk = compile(prelude_session.program()).map_err(tsblitz::errors::CompilerError::from)?;
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

    // Run prelude. The soft layer provides all base objects (Object, Array, console, etc.).
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
            return Err(tsblitz::errors::RuntimeError::from(format!(
                "Runtime error in soft layer: {}",
                e
            ))
            .into());
        }
    }

    let args: Vec<String> = env::args().collect();
    if args.len() > 1 {
        let path = Path::new(&args[1]);
        let program = bundle_entry(path).map_err(tsblitz::errors::BundlerError::from)?;
        run_program(&mut repl_vm, program)?;
        return Ok(());
    }

    // REPL with line-editing and history
    println!("Blitz — TypeScript VM (Rust runtime). Type exit to quit.");
    let mut rl = rustyline::Editor::<(), rustyline::history::DefaultHistory>::new()
        .map_err(|e| tsblitz::errors::RuntimeError::from(format!("Readline init error: {}", e)))?;
    let history_path = env::temp_dir().join("blitz_repl_history");
    let _ = rl.load_history(&history_path);
    loop {
        match rl.readline("> ") {
            Ok(input) => {
                let line: &str = input.trim();
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
