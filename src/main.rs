//! CLI entry point for Blitz VM.
//!
//! The CLI provides two main interfaces:
//! - **File execution**: Run a JavaScript file directly
//! - **REPL**: Interactive read-eval-print loop with history
//!
//! # Bootstrapping
//!
//! On startup, the CLI creates a VM with native host bindings and executes
//! user code directly in the initialized VM.
//!
//! # Host Bindings
//!
//! Native functions exposed to the runtime:
//! - `__native_print` - Print to stdout
//! - `__native_new_object` - Create VM-backed objects
//!
//! # Usage
//!
//! ```bash
//! # Run a JavaScript file
//! cargo run -- path/to/file.js
//!
//! # Start REPL
//! cargo run -- --repl
//!
//! # Enable tracing
//! TSBLITZ_TRACE=1 cargo run -- file.ts
//! ```

use clap::Parser;
use std::cell::RefCell;
use std::env;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use blitz::compiler;
use blitz::{
    module::ModuleLoader, parser::Parser as TsParser, runtime_utils, value::Value, vm::Vm,
};
use num_traits::{Signed, ToPrimitive};

/// Minimal native hook: print to stdout. The TS soft layer wraps this as console.log.
fn native_print(_vm: &mut Vm, args: &[Value]) -> Result<Value, String> {
    // Diagnostic wrapper: emit arg debug then print joined string
    if blitz::trace::enabled() {
        eprintln!("[diag] host.native_print called nargs={}", args.len());
        for (i, v) in args.iter().enumerate() {
            blitz::trace::value_string(&format!("host.native_print arg[{}]", i), v);
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

    // Print synchronously so output is visible before the VM exits.
    if blitz::trace::enabled() {
        eprintln!(
            "[diag] host.native_print write len={} repr={:?}",
            s.len(),
            s
        );
    }

    eprintln!("{}", s);

    Ok(Value::Null)
}

// Host bindings are published directly into the VM globals (`globalThis`) so
// soft-layer code can access them as regular globals (e.g. `globalThis.__native_print`).

/// Runs source code through the full compilation pipeline
///
/// # Errors
///
/// Returns an error string if any stage of compilation fails
fn run_source(vm: &mut Vm, source: &str) -> Result<Value, blitz::errors::Error> {
    let session = TsParser::parse_source(source)?;
    let oast_prog = session.program();

    // Use the temporary compile_oxc shim so we can avoid converting to local_ast here.
    let chunk = compiler::compile_oxc(oast_prog, source).map_err(blitz::errors::Error::from)?;
    vm.run(chunk).map_err(blitz::errors::Error::from)
}

/// Command line arguments for the Blitz executable.
#[derive(Parser, Debug)]
#[command(name = "blitz", about = "Blitz — TypeScript VM (Rust runtime)")]
struct Cli {
    /// TypeScript file to execute. If omitted the REPL will be started.
    #[arg(value_name = "FILE")]
    file: Option<PathBuf>,

    /// Start the REPL even when a file is not provided.
    #[arg(short, long, action)]
    repl: bool,

    /// Run the TypeScript typechecker before execution.
    #[arg(long, action)]
    typecheck: bool,
}

fn main() -> Result<(), blitz::errors::Error> {
    // Public globals visible to user code (populated by the prelude)
    let globals: Rc<RefCell<blitz::value::ObjectData>> =
        Rc::new(RefCell::new(blitz::value::ObjectData::new()));

    // Provide a `globalThis` object that reflects the VM globals map so soft
    // layer code that writes `globalThis.x = ...` updates the VM globals.
    // We reuse the same Rc<RefCell<HashMap>> so property ops on the object and
    // Get/SetGlobal access the same backing map.
    let global_this = Value::Object(Rc::clone(&globals));
    {
        let mut g = globals.borrow_mut();
        g.properties.insert(
            blitz::value::PropertyKey::String("globalThis".to_string()),
            blitz::value::PropertyDescriptor::data(global_this),
        );
    }

    // Publish native host bindings directly into globals so soft code can read
    // them as ordinary globals (e.g. `globalThis.__native_print`). This keeps
    // the runtime simpler and avoids an extra indirection.
    globals.borrow_mut().properties.insert(
        blitz::value::PropertyKey::String("__native_print".to_string()),
        blitz::value::PropertyDescriptor::data(Value::Builtin(Rc::new(|vm, args| {
            native_print(vm, args).map_err(blitz::errors::RuntimeError::from)
        }))),
    );

    // Minimal native hook for creating VM-backed objects.
    fn native_new_object(_vm: &mut Vm, _args: &[Value]) -> Result<Value, String> {
        use std::cell::RefCell;
        use std::rc::Rc;
        Ok(Value::Object(Rc::new(RefCell::new(
            blitz::value::ObjectData::new(),
        ))))
    }

    globals.borrow_mut().properties.insert(
        blitz::value::PropertyKey::String("__native_new_object".to_string()),
        blitz::value::PropertyDescriptor::data(Value::Builtin(Rc::new(|vm, args| {
            native_new_object(vm, args).map_err(blitz::errors::RuntimeError::from)
        }))),
    );

    globals.borrow_mut().properties.insert(
        blitz::value::PropertyKey::String("__define_property".to_string()),
        blitz::value::PropertyDescriptor::data(Value::Builtin(Rc::new(|vm, args| {
            vm.define_property_builtin(args)
        }))),
    );
    globals.borrow_mut().properties.insert(
        blitz::value::PropertyKey::String("__native_getOwnPropertyDescriptor".to_string()),
        blitz::value::PropertyDescriptor::data(Value::Builtin(Rc::new(|vm, args| {
            let target = args.first().cloned().unwrap_or(Value::Undefined);
            let key_value = args.get(1).cloned().unwrap_or(Value::Undefined);
            let key = vm.to_property_key(&key_value)?;
            Ok(runtime_utils::own_property_descriptor_value(&target, &key)
                .unwrap_or(Value::Undefined))
        }))),
    );

    // Kernel-native coercion helpers: soft layer delegates ToString/ToNumber/ToBoolean
    // to the kernel during migration. These simple wrappers call into the VM
    // helpers implemented in `Vm` so soft runtime can use authoritative semantics
    // without reimplementing them in TypeScript.
    globals.borrow_mut().properties.insert(
        blitz::value::PropertyKey::String("__native_toString".to_string()),
        blitz::value::PropertyDescriptor::data(Value::Builtin(Rc::new(|vm, args| {
            let v = args.first().cloned().unwrap_or(Value::Undefined);
            vm.to_string(&v).map(Value::String)
        }))),
    );
    globals.borrow_mut().properties.insert(
        blitz::value::PropertyKey::String("__native_toNumber".to_string()),
        blitz::value::PropertyDescriptor::data(Value::Builtin(Rc::new(|vm, args| {
            let v = args.first().cloned().unwrap_or(Value::Undefined);
            vm.to_number_value(&v).map(Value::Number)
        }))),
    );
    globals.borrow_mut().properties.insert(
        blitz::value::PropertyKey::String("__native_toBoolean".to_string()),
        blitz::value::PropertyDescriptor::data(Value::Builtin(Rc::new(|vm, args| {
            let v = args.first().cloned().unwrap_or(Value::Undefined);
            Ok(Value::Bool(vm.to_boolean(&v)))
        }))),
    );

    globals.borrow_mut().properties.insert(
        blitz::value::PropertyKey::String("__native_abstractEquality".to_string()),
        blitz::value::PropertyDescriptor::data(Value::Builtin(Rc::new(|vm, args| {
            let a = args.first().cloned().unwrap_or(Value::Undefined);
            let b = args.get(1).cloned().unwrap_or(Value::Undefined);
            match vm.abstract_equality(&a, &b) {
                Ok(v) => Ok(Value::Bool(v)),
                Err(e) => Err(e),
            }
        }))),
    );

    globals.borrow_mut().properties.insert(
        blitz::value::PropertyKey::String("__native_instanceof".to_string()),
        blitz::value::PropertyDescriptor::data(Value::Builtin(Rc::new(|vm, args| {
            let value = args.first().cloned().unwrap_or(Value::Undefined);
            let ctor = args.get(1).cloned().unwrap_or(Value::Undefined);
            match vm.instance_of(&value, &ctor) {
                Ok(v) => Ok(Value::Bool(v)),
                Err(e) => Err(e),
            }
        }))),
    );

    globals.borrow_mut().properties.insert(
        blitz::value::PropertyKey::String("__native_inProperty".to_string()),
        blitz::value::PropertyDescriptor::data(Value::Builtin(Rc::new(|vm, args| {
            let prop = args.first().cloned().unwrap_or(Value::Undefined);
            let obj = args.get(1).cloned().unwrap_or(Value::Undefined);
            match vm.in_property(&prop, &obj) {
                Ok(v) => Ok(Value::Bool(v)),
                Err(e) => Err(e),
            }
        }))),
    );

    globals.borrow_mut().properties.insert(
        blitz::value::PropertyKey::String("__native_typeof".to_string()),
        blitz::value::PropertyDescriptor::data(Value::Builtin(Rc::new(|vm, args| {
            let v = args.first().cloned().unwrap_or(Value::Undefined);
            Ok(Value::String(vm.typeof_value(&v)))
        }))),
    );

    // Arithmetic and relational native helpers
    globals.borrow_mut().properties.insert(
        blitz::value::PropertyKey::String("__native_add".to_string()),
        blitz::value::PropertyDescriptor::data(Value::Builtin(Rc::new(|vm, args| {
            let a = args.first().cloned().unwrap_or(Value::Null);
            let b = args.get(1).cloned().unwrap_or(Value::Null);
            // BigInt handling
            if matches!(a, Value::BigInt(_)) || matches!(b, Value::BigInt(_)) {
                // Match on references to avoid moving `a`/`b` so they can be
                // borrowed again later in the closure.
                if let (Value::BigInt(x), Value::BigInt(y)) = (&a, &b) {
                    return Ok(Value::BigInt(x.clone() + y.clone()));
                } else {
                    return Err(blitz::errors::RuntimeError::from(
                        "Add: cannot mix BigInt and other types",
                    ));
                }
            }
            // Ordinary addition: ToPrimitive default
            let a_prim = vm.to_primitive(&a, None)?;
            let b_prim = vm.to_primitive(&b, None)?;
            if matches!(a_prim, Value::String(_)) || matches!(b_prim, Value::String(_)) {
                let sa = vm.to_string(&a_prim)?;
                let sb = vm.to_string(&b_prim)?;
                return Ok(Value::String(format!("{}{}", sa, sb)));
            }
            let na = vm.to_number_value(&a_prim)?;
            let nb = vm.to_number_value(&b_prim)?;
            Ok(Value::Number(na + nb))
        }))),
    );

    globals.borrow_mut().properties.insert(
        blitz::value::PropertyKey::String("__native_subtract".to_string()),
        blitz::value::PropertyDescriptor::data(Value::Builtin(Rc::new(|vm, args| {
            let a = args.first().cloned().unwrap_or(Value::Null);
            let b = args.get(1).cloned().unwrap_or(Value::Null);
            if let (Value::BigInt(x), Value::BigInt(y)) = (&a, &b) {
                return Ok(Value::BigInt(x.clone() - y.clone()));
            }
            let na = vm.to_number_value(&a)?;
            let nb = vm.to_number_value(&b)?;
            Ok(Value::Number(na - nb))
        }))),
    );

    globals.borrow_mut().properties.insert(
        blitz::value::PropertyKey::String("__native_multiply".to_string()),
        blitz::value::PropertyDescriptor::data(Value::Builtin(Rc::new(|vm, args| {
            let a = args.first().cloned().unwrap_or(Value::Null);
            let b = args.get(1).cloned().unwrap_or(Value::Null);
            if let (Value::BigInt(x), Value::BigInt(y)) = (&a, &b) {
                return Ok(Value::BigInt(x.clone() * y.clone()));
            }
            let na = vm.to_number_value(&a)?;
            let nb = vm.to_number_value(&b)?;
            Ok(Value::Number(na * nb))
        }))),
    );

    globals.borrow_mut().properties.insert(
        blitz::value::PropertyKey::String("__native_divide".to_string()),
        blitz::value::PropertyDescriptor::data(Value::Builtin(Rc::new(|vm, args| {
            let a = args.first().cloned().unwrap_or(Value::Null);
            let b = args.get(1).cloned().unwrap_or(Value::Null);
            // BigInt division is not supported
            if matches!(a, Value::BigInt(_)) || matches!(b, Value::BigInt(_)) {
                return Err(blitz::errors::RuntimeError::from(
                    "Div: bigint not supported",
                ));
            }
            let na = vm.to_number_value(&a)?;
            let nb = vm.to_number_value(&b)?;
            Ok(Value::Number(na / nb))
        }))),
    );

    globals.borrow_mut().properties.insert(
        blitz::value::PropertyKey::String("__native_remainder".to_string()),
        blitz::value::PropertyDescriptor::data(Value::Builtin(Rc::new(|vm, args| {
            let a = args.first().cloned().unwrap_or(Value::Null);
            let b = args.get(1).cloned().unwrap_or(Value::Null);
            if let (Value::BigInt(x), Value::BigInt(y)) = (&a, &b) {
                return Ok(Value::BigInt(x.clone() % y.clone()));
            }
            let na = vm.to_number_value(&a)?;
            let nb = vm.to_number_value(&b)?;
            Ok(Value::Number(na % nb))
        }))),
    );

    globals.borrow_mut().properties.insert(
        blitz::value::PropertyKey::String("__native_exponentiate".to_string()),
        blitz::value::PropertyDescriptor::data(Value::Builtin(Rc::new(|vm, args| {
            let a = args.first().cloned().unwrap_or(Value::Null);
            let b = args.get(1).cloned().unwrap_or(Value::Null);
            if let (Value::BigInt(x), Value::BigInt(y)) = (&a, &b) {
                // BigInt exponentiation: use integer pow if y >= 0 and fits
                if y.is_negative() {
                    return Err(blitz::errors::RuntimeError::from(
                        "Exponentiation with negative BigInt not supported",
                    ));
                }
                let exp = y.to_u32().ok_or_else(|| {
                    blitz::errors::RuntimeError::from("Exponentiation with huge BigInt exponent")
                })?;
                return Ok(Value::BigInt(x.clone().pow(exp)));
            }
            let na = vm.to_number_value(&a)?;
            let nb = vm.to_number_value(&b)?;
            Ok(Value::Number(na.powf(nb)))
        }))),
    );

    // Relational
    globals.borrow_mut().properties.insert(
        blitz::value::PropertyKey::String("__native_lessThan".to_string()),
        blitz::value::PropertyDescriptor::data(Value::Builtin(Rc::new(|vm, args| {
            let a = args.first().cloned().unwrap_or(Value::Null);
            let b = args.get(1).cloned().unwrap_or(Value::Null);
            let ap = vm.to_primitive(&a, None)?;
            let bp = vm.to_primitive(&b, None)?;
            if let (Value::String(sa), Value::String(sb)) = (ap.clone(), bp.clone()) {
                return Ok(Value::Bool(sa < sb));
            }
            let na = vm.to_number_value(&ap)?;
            let nb = vm.to_number_value(&bp)?;
            Ok(Value::Bool(na < nb))
        }))),
    );

    globals.borrow_mut().properties.insert(
        blitz::value::PropertyKey::String("__native_lessThanOrEqual".to_string()),
        blitz::value::PropertyDescriptor::data(Value::Builtin(Rc::new(|vm, args| {
            let a = args.first().cloned().unwrap_or(Value::Null);
            let b = args.get(1).cloned().unwrap_or(Value::Null);
            let ap = vm.to_primitive(&a, None)?;
            let bp = vm.to_primitive(&b, None)?;
            if let (Value::String(sa), Value::String(sb)) = (ap.clone(), bp.clone()) {
                return Ok(Value::Bool(sa <= sb));
            }
            let na = vm.to_number_value(&ap)?;
            let nb = vm.to_number_value(&bp)?;
            Ok(Value::Bool(na <= nb))
        }))),
    );

    globals.borrow_mut().properties.insert(
        blitz::value::PropertyKey::String("__native_greaterThan".to_string()),
        blitz::value::PropertyDescriptor::data(Value::Builtin(Rc::new(|vm, args| {
            let a = args.first().cloned().unwrap_or(Value::Null);
            let b = args.get(1).cloned().unwrap_or(Value::Null);
            let ap = vm.to_primitive(&a, None)?;
            let bp = vm.to_primitive(&b, None)?;
            if let (Value::String(sa), Value::String(sb)) = (ap.clone(), bp.clone()) {
                return Ok(Value::Bool(sa > sb));
            }
            let na = vm.to_number_value(&ap)?;
            let nb = vm.to_number_value(&bp)?;
            Ok(Value::Bool(na > nb))
        }))),
    );

    globals.borrow_mut().properties.insert(
        blitz::value::PropertyKey::String("__native_greaterThanOrEqual".to_string()),
        blitz::value::PropertyDescriptor::data(Value::Builtin(Rc::new(|vm, args| {
            let a = args.first().cloned().unwrap_or(Value::Null);
            let b = args.get(1).cloned().unwrap_or(Value::Null);
            let ap = vm.to_primitive(&a, None)?;
            let bp = vm.to_primitive(&b, None)?;
            if let (Value::String(sa), Value::String(sb)) = (ap.clone(), bp.clone()) {
                return Ok(Value::Bool(sa >= sb));
            }
            let na = vm.to_number_value(&ap)?;
            let nb = vm.to_number_value(&bp)?;
            Ok(Value::Bool(na >= nb))
        }))),
    );

    // Provide a minimal builtin console.log for early bootstrap and REPL use.
    globals.borrow_mut().properties.insert(
        blitz::value::PropertyKey::String("console".to_string()),
        blitz::value::PropertyDescriptor::data(Value::Object(Rc::new(RefCell::new({
            let mut data = blitz::value::ObjectData::new();
            data.properties.insert(
                blitz::value::PropertyKey::String("log".to_string()),
                blitz::value::PropertyDescriptor::data(Value::Builtin(Rc::new(|vm, args| {
                    native_print(vm, args).map_err(blitz::errors::RuntimeError::from)
                }))),
            );
            data
        })))),
    );

    runtime_utils::install_minimal_kernel_constructors(&globals);

    // Parse CLI early so clap can handle --help/--version before doing work.
    let cli = Cli::parse();

    // Create a persistent VM for the REPL and reuse it for every input so
    // we don't recompile and recreate the VM on each line.
    // Create VM and run prelude. blitzlib can call `__host_get(name)` during
    // bootstrap to capture host-provided values; raw host entries are never
    // placed into the globals table.
    let mut repl_vm = Vm::new(globals);

    // If a file was provided, run it and exit. Otherwise start REPL.
    if let Some(path) = cli.file {
        // Use ModuleLoader to load and run the entry file (and its deps)
        let mut loader = ModuleLoader::new();
        loader.construct(
            path.parent().unwrap_or(Path::new(".")),
            &format!("./{}", path.file_name().unwrap().to_string_lossy()),
        )?;
        loader
            .instantiate_all()
            .map_err(blitz::errors::Error::from)?;
        loader
            .evaluate_entry(&path, &mut repl_vm, cli.typecheck)
            .map_err(blitz::errors::Error::from)?;
        return Ok(());
    }

    // Start REPL (explicit flag or no file provided)
    println!("Blitz — TypeScript VM (Rust runtime). Type exit to quit.");
    let mut rl = rustyline::Editor::<(), rustyline::history::DefaultHistory>::new()
        .map_err(|e| blitz::errors::RuntimeError::from(format!("Readline init error: {}", e)))?;
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
