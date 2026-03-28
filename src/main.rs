//! CLI entry point for Blitz VM.
//!
//! The CLI provides two main interfaces:
//! - **File execution**: Run a JavaScript file directly
//! - **REPL**: Interactive read-eval-print loop with history
//!
//! # Bootstrapping
//!
//! On startup, the CLI:
//! 1. Creates a VM with native host bindings (`__native_print`, etc.)
//! 2. Loads and executes the soft layer prelude (`soft/prelude.ts`)
//! 3. The prelude installs standard library globals (console, Array, Object, etc.)
//! 4. Executes user code in the initialized VM
//!
//! # Host Bindings
//!
//! Native functions exposed to the soft layer:
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

use tsblitz::bytecode;
use tsblitz::compiler;
use tsblitz::{
    module::ModuleLoader, parser::Parser as TsParser, resolver::resolve_specifier, typecheck,
    value::Value, vm::Vm,
};

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

    // Print synchronously so output is visible before the VM exits.
    if tsblitz::trace::enabled() {
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
fn run_source(vm: &mut Vm, source: &str) -> Result<Value, tsblitz::errors::Error> {
    let session = TsParser::parse_source(source)?;
    let oast_prog = session.program();
    typecheck::check_oxc(oast_prog)?;
    // Use the temporary compile_oxc shim so we can avoid converting to local_ast here.
    let chunk = compiler::compile_oxc(oast_prog).map_err(tsblitz::errors::Error::from)?;
    vm.run(chunk).map_err(tsblitz::errors::Error::from)
}

fn run_program(
    vm: &mut Vm,
    program: &oxc_ast::ast::Program<'_>,
) -> Result<Value, tsblitz::errors::Error> {
    typecheck::check_oxc(program)?;
    let chunk = compiler::compile_oxc(program).map_err(tsblitz::errors::Error::from)?;
    vm.run(chunk).map_err(tsblitz::errors::Error::from)
}

// Slot/global type annotations were removed; use the runtime_utils helper to
// clear any lingering slot metadata when needed. The implementation lives in
// `src/runtime_utils.rs` and is intentionally a no-op in JS-first mode.

// Textual import inlining was removed. The project now uses a proper
// module-graph loader that parses modules and wires imports at runtime.
// Use `ModuleLoader` (see `module::ModuleLoader`) to construct/instantiate
// and evaluate entry modules; it handles parsing dependencies, compilation,
// and execution into a VM.

/// Command line arguments for the tsblitz executable.
#[derive(Parser, Debug)]
#[command(name = "blitz", about = "Blitz — TypeScript VM (Rust runtime)")]
struct Cli {
    /// TypeScript file to execute. If omitted the REPL will be started.
    #[arg(value_name = "FILE")]
    file: Option<PathBuf>,

    /// Start the REPL even when a file is not provided.
    #[arg(short, long, action)]
    repl: bool,
}

fn main() -> Result<(), tsblitz::errors::Error> {
    // Public globals visible to user code (populated by the prelude)
    let globals: Rc<RefCell<tsblitz::value::ObjectData>> =
        Rc::new(RefCell::new(tsblitz::value::ObjectData::new()));

    // Provide a `globalThis` object that reflects the VM globals map so soft
    // layer code that writes `globalThis.x = ...` updates the VM globals.
    // We reuse the same Rc<RefCell<HashMap>> so property ops on the object and
    // Get/SetGlobal access the same backing map.
    let global_this = Value::Object(Rc::clone(&globals));
    {
        let mut g = globals.borrow_mut();
        g.properties.insert(
            tsblitz::value::PropertyKey::String("globalThis".to_string()),
            tsblitz::value::PropertyDescriptor::data(global_this),
        );
    }

    // Publish native host bindings directly into globals so soft code can read
    // them as ordinary globals (e.g. `globalThis.__native_print`). This keeps
    // the runtime simpler and avoids an extra indirection.
    globals.borrow_mut().properties.insert(
        tsblitz::value::PropertyKey::String("__native_print".to_string()),
        tsblitz::value::PropertyDescriptor::data(Value::Builtin(Rc::new(|vm, args| {
            native_print(vm, args).map_err(tsblitz::errors::RuntimeError::from)
        }))),
    );

    // Minimal native hook for creating VM-backed objects.
    fn native_new_object(_vm: &mut Vm, _args: &[Value]) -> Result<Value, String> {
        use std::cell::RefCell;
        use std::rc::Rc;
        Ok(Value::Object(Rc::new(RefCell::new(
            tsblitz::value::ObjectData::new(),
        ))))
    }

    globals.borrow_mut().properties.insert(
        tsblitz::value::PropertyKey::String("__native_new_object".to_string()),
        tsblitz::value::PropertyDescriptor::data(Value::Builtin(Rc::new(|vm, args| {
            native_new_object(vm, args).map_err(tsblitz::errors::RuntimeError::from)
        }))),
    );

    globals.borrow_mut().properties.insert(
        tsblitz::value::PropertyKey::String("__define_property".to_string()),
        tsblitz::value::PropertyDescriptor::data(Value::Builtin(Rc::new(|vm, args| {
            vm.define_property_builtin(args)
        }))),
    );

    // Kernel-native coercion helpers: soft layer delegates ToString/ToNumber/ToBoolean
    // to the kernel during migration. These simple wrappers call into the VM
    // helpers implemented in `Vm` so soft runtime can use authoritative semantics
    // without reimplementing them in TypeScript.
    globals.borrow_mut().properties.insert(
        tsblitz::value::PropertyKey::String("__native_toString".to_string()),
        tsblitz::value::PropertyDescriptor::data(Value::Builtin(Rc::new(|vm, args| {
            let v = args.first().cloned().unwrap_or(Value::Undefined);
            vm.to_string(&v).map(Value::String)
        }))),
    );
    globals.borrow_mut().properties.insert(
        tsblitz::value::PropertyKey::String("__native_toNumber".to_string()),
        tsblitz::value::PropertyDescriptor::data(Value::Builtin(Rc::new(|vm, args| {
            let v = args.first().cloned().unwrap_or(Value::Undefined);
            vm.to_number_value(&v).map(Value::Number)
        }))),
    );
    globals.borrow_mut().properties.insert(
        tsblitz::value::PropertyKey::String("__native_toBoolean".to_string()),
        tsblitz::value::PropertyDescriptor::data(Value::Builtin(Rc::new(|vm, args| {
            let v = args.first().cloned().unwrap_or(Value::Undefined);
            Ok(Value::Bool(vm.to_boolean(&v)))
        }))),
    );

    globals.borrow_mut().properties.insert(
        tsblitz::value::PropertyKey::String("__native_abstractEquality".to_string()),
        tsblitz::value::PropertyDescriptor::data(Value::Builtin(Rc::new(|vm, args| {
            let a = args.first().cloned().unwrap_or(Value::Undefined);
            let b = args.get(1).cloned().unwrap_or(Value::Undefined);
            match vm.abstract_equality(&a, &b) {
                Ok(v) => Ok(Value::Bool(v)),
                Err(e) => Err(e),
            }
        }))),
    );

    globals.borrow_mut().properties.insert(
        tsblitz::value::PropertyKey::String("__native_instanceof".to_string()),
        tsblitz::value::PropertyDescriptor::data(Value::Builtin(Rc::new(|vm, args| {
            let value = args.first().cloned().unwrap_or(Value::Undefined);
            let ctor = args.get(1).cloned().unwrap_or(Value::Undefined);
            match vm.instance_of(&value, &ctor) {
                Ok(v) => Ok(Value::Bool(v)),
                Err(e) => Err(e),
            }
        }))),
    );

    globals.borrow_mut().properties.insert(
        tsblitz::value::PropertyKey::String("__native_inProperty".to_string()),
        tsblitz::value::PropertyDescriptor::data(Value::Builtin(Rc::new(|vm, args| {
            let prop = args.first().cloned().unwrap_or(Value::Undefined);
            let obj = args.get(1).cloned().unwrap_or(Value::Undefined);
            match vm.in_property(&prop, &obj) {
                Ok(v) => Ok(Value::Bool(v)),
                Err(e) => Err(e),
            }
        }))),
    );

    globals.borrow_mut().properties.insert(
        tsblitz::value::PropertyKey::String("__native_typeof".to_string()),
        tsblitz::value::PropertyDescriptor::data(Value::Builtin(Rc::new(|vm, args| {
            let v = args.first().cloned().unwrap_or(Value::Undefined);
            Ok(Value::String(vm.typeof_value(&v)))
        }))),
    );

    // Arithmetic and relational native helpers
    globals.borrow_mut().properties.insert(
        tsblitz::value::PropertyKey::String("__native_add".to_string()),
        tsblitz::value::PropertyDescriptor::data(Value::Builtin(Rc::new(|vm, args| {
            let a = args.first().cloned().unwrap_or(Value::Null);
            let b = args.get(1).cloned().unwrap_or(Value::Null);
            // BigInt handling
            if matches!(a, Value::BigInt(_)) || matches!(b, Value::BigInt(_)) {
                // Match on references to avoid moving `a`/`b` so they can be
                // borrowed again later in the closure.
                if let (Value::BigInt(x), Value::BigInt(y)) = (&a, &b) {
                    return Ok(Value::BigInt(*x + *y));
                } else {
                    return Err(tsblitz::errors::RuntimeError::from(
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
        tsblitz::value::PropertyKey::String("__native_subtract".to_string()),
        tsblitz::value::PropertyDescriptor::data(Value::Builtin(Rc::new(|vm, args| {
            let a = args.first().cloned().unwrap_or(Value::Null);
            let b = args.get(1).cloned().unwrap_or(Value::Null);
            if let (Value::BigInt(x), Value::BigInt(y)) = (&a, &b) {
                return Ok(Value::BigInt(*x - *y));
            }
            let na = vm.to_number_value(&a)?;
            let nb = vm.to_number_value(&b)?;
            Ok(Value::Number(na - nb))
        }))),
    );

    globals.borrow_mut().properties.insert(
        tsblitz::value::PropertyKey::String("__native_multiply".to_string()),
        tsblitz::value::PropertyDescriptor::data(Value::Builtin(Rc::new(|vm, args| {
            let a = args.first().cloned().unwrap_or(Value::Null);
            let b = args.get(1).cloned().unwrap_or(Value::Null);
            if let (Value::BigInt(x), Value::BigInt(y)) = (&a, &b) {
                return Ok(Value::BigInt(*x * *y));
            }
            let na = vm.to_number_value(&a)?;
            let nb = vm.to_number_value(&b)?;
            Ok(Value::Number(na * nb))
        }))),
    );

    globals.borrow_mut().properties.insert(
        tsblitz::value::PropertyKey::String("__native_divide".to_string()),
        tsblitz::value::PropertyDescriptor::data(Value::Builtin(Rc::new(|vm, args| {
            let a = args.first().cloned().unwrap_or(Value::Null);
            let b = args.get(1).cloned().unwrap_or(Value::Null);
            // BigInt division is not supported
            if matches!(a, Value::BigInt(_)) || matches!(b, Value::BigInt(_)) {
                return Err(tsblitz::errors::RuntimeError::from(
                    "Div: bigint not supported",
                ));
            }
            let na = vm.to_number_value(&a)?;
            let nb = vm.to_number_value(&b)?;
            Ok(Value::Number(na / nb))
        }))),
    );

    globals.borrow_mut().properties.insert(
        tsblitz::value::PropertyKey::String("__native_remainder".to_string()),
        tsblitz::value::PropertyDescriptor::data(Value::Builtin(Rc::new(|vm, args| {
            let a = args.first().cloned().unwrap_or(Value::Null);
            let b = args.get(1).cloned().unwrap_or(Value::Null);
            if let (Value::BigInt(x), Value::BigInt(y)) = (&a, &b) {
                return Ok(Value::BigInt(*x % *y));
            }
            let na = vm.to_number_value(&a)?;
            let nb = vm.to_number_value(&b)?;
            Ok(Value::Number(na % nb))
        }))),
    );

    globals.borrow_mut().properties.insert(
        tsblitz::value::PropertyKey::String("__native_exponentiate".to_string()),
        tsblitz::value::PropertyDescriptor::data(Value::Builtin(Rc::new(|vm, args| {
            let a = args.first().cloned().unwrap_or(Value::Null);
            let b = args.get(1).cloned().unwrap_or(Value::Null);
            if let (Value::BigInt(x), Value::BigInt(y)) = (&a, &b) {
                // BigInt exponentiation: use integer pow if y >= 0 and fits
                if *y < 0 {
                    return Err(tsblitz::errors::RuntimeError::from(
                        "Exponentiation with negative BigInt not supported",
                    ));
                }
                let mut res: i128 = 1;
                let mut base = *x;
                let mut exp = *y as u128;
                while exp > 0 {
                    if (exp & 1) == 1 {
                        res = res.checked_mul(base).ok_or_else(|| {
                            tsblitz::errors::RuntimeError::from("BigInt overflow in exponentiation")
                        })?;
                    }
                    exp >>= 1;
                    if exp > 0 {
                        base = base.checked_mul(base).ok_or_else(|| {
                            tsblitz::errors::RuntimeError::from("BigInt overflow in exponentiation")
                        })?;
                    }
                }
                return Ok(Value::BigInt(res));
            }
            let na = vm.to_number_value(&a)?;
            let nb = vm.to_number_value(&b)?;
            Ok(Value::Number(na.powf(nb)))
        }))),
    );

    // Relational
    globals.borrow_mut().properties.insert(
        tsblitz::value::PropertyKey::String("__native_lessThan".to_string()),
        tsblitz::value::PropertyDescriptor::data(Value::Builtin(Rc::new(|vm, args| {
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
        tsblitz::value::PropertyKey::String("__native_lessThanOrEqual".to_string()),
        tsblitz::value::PropertyDescriptor::data(Value::Builtin(Rc::new(|vm, args| {
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
        tsblitz::value::PropertyKey::String("__native_greaterThan".to_string()),
        tsblitz::value::PropertyDescriptor::data(Value::Builtin(Rc::new(|vm, args| {
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
        tsblitz::value::PropertyKey::String("__native_greaterThanOrEqual".to_string()),
        tsblitz::value::PropertyDescriptor::data(Value::Builtin(Rc::new(|vm, args| {
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
        tsblitz::value::PropertyKey::String("console".to_string()),
        tsblitz::value::PropertyDescriptor::data(Value::Object(Rc::new(RefCell::new({
            let mut data = tsblitz::value::ObjectData::new();
            data.properties.insert(
                tsblitz::value::PropertyKey::String("log".to_string()),
                tsblitz::value::PropertyDescriptor::data(Value::Builtin(Rc::new(|vm, args| {
                    native_print(vm, args).map_err(tsblitz::errors::RuntimeError::from)
                }))),
            );
            data
        })))),
    );

    tsblitz::runtime_utils::install_minimal_kernel_constructors(&globals);

    // Parse CLI early so clap can handle --help/--version before doing work.
    let cli = Cli::parse();

    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let prelude_path = manifest_dir.join("soft/prelude.ts");
    // Resolve the prelude path into a concrete file using oxc_resolver wrapper
    let prelude_resolved = resolve_specifier(
        prelude_path.parent().unwrap_or(Path::new(".")),
        &format!("./{}", prelude_path.file_name().unwrap().to_string_lossy()),
    )?;
    // Use ModuleLoader to construct/instantiate/evaluate the soft prelude which
    // handles parsing dependencies, compiling, clearing slot types, and running.
    let mut loader = ModuleLoader::new();
    loader.construct(
        prelude_resolved.parent().unwrap_or(Path::new(".")),
        &format!(
            "./{}",
            prelude_resolved.file_name().unwrap().to_string_lossy()
        ),
    )?;
    loader
        .instantiate_all()
        .map_err(tsblitz::errors::Error::from)?;
    // One-time diagnostic: compile the prelude module and print chunk info so
    // we can map bytecode ip locations back to source-level functions during
    // debugging. Compilation here is best-effort and only performed when
    // tracing is enabled to avoid extra work in normal runs.
    if tsblitz::trace::enabled() {
        // Prepare a one-off chunk compilation and disassembly for the prelude.
        if let Err(e) = (|| -> Result<(), tsblitz::errors::Error> {
            // The loader.map keys are canonicalized; ensure we look up the same
            // canonical path when fetching the prelude record.
            let prelude_key =
                std::fs::canonicalize(&prelude_resolved).unwrap_or(prelude_resolved.clone());
            let rec_rc = loader
                .map
                .get(&prelude_key)
                .ok_or_else(|| tsblitz::errors::BundlerError::from("Prelude module not found"))?
                .clone();

            // Hold the borrow across compilation so parser session data remains
            // valid for the duration of compile_oxc.
            let chunk = {
                let rec_b = rec_rc.borrow();
                let program = rec_b.session.program();
                let chunk = compiler::compile_oxc(program).map_err(tsblitz::errors::Error::from)?;
                // Use the runtime_utils helper to clear slot types for the prelude chunk.
                tsblitz::runtime_utils::clear_slot_types_in_chunk(chunk)
            };

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
                        | Some(bytecode::Op::GetBuiltin)
                        | Some(bytecode::Op::MakeClosure)
                        | Some(bytecode::Op::GetPropertyIC)
                        | Some(bytecode::Op::Call) => {
                            if ip + 1 < f.chunk.code.len() {
                                let v = f.chunk.code[ip + 1] as usize;
                                extra = format!(" {}", v);
                            }
                        }
                        Some(bytecode::Op::GetGlobal) | Some(bytecode::Op::SetGlobal) => {
                            if ip + 2 < f.chunk.code.len() {
                                let hi = f.chunk.code[ip + 1] as usize;
                                let lo = f.chunk.code[ip + 2] as usize;
                                let v = (hi << 8) | lo;
                                extra = format!(" {}", v);
                            }
                        }
                        Some(bytecode::Op::Jump)
                        | Some(bytecode::Op::JumpIfFalse)
                        | Some(bytecode::Op::PushHandler) => {
                            if ip + 2 < f.chunk.code.len() {
                                let short = i16::from_be_bytes([
                                    f.chunk.code[ip + 1],
                                    f.chunk.code[ip + 2],
                                ]);
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
                        | Some(bytecode::Op::GetBuiltin)
                        | Some(bytecode::Op::MakeClosure)
                        | Some(bytecode::Op::GetPropertyIC)
                        | Some(bytecode::Op::Call) => ip += 1,
                        Some(bytecode::Op::GetGlobal) | Some(bytecode::Op::SetGlobal) => ip += 2,
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
            // Very small disassembly to map ip -> opcode and operand (first 2000 bytes)
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
                    | Some(bytecode::Op::GetBuiltin)
                    | Some(bytecode::Op::MakeClosure)
                    | Some(bytecode::Op::GetPropertyIC)
                    | Some(bytecode::Op::Call) => {
                        if ip + 1 < chunk.code.len() {
                            let v = chunk.code[ip + 1] as usize;
                            extra = format!(" {}", v);
                        }
                    }
                    Some(bytecode::Op::GetGlobal) | Some(bytecode::Op::SetGlobal) => {
                        if ip + 2 < chunk.code.len() {
                            let hi = chunk.code[ip + 1] as usize;
                            let lo = chunk.code[ip + 2] as usize;
                            let v = (hi << 8) | lo;
                            extra = format!(" {}", v);
                        }
                    }
                    Some(bytecode::Op::Jump)
                    | Some(bytecode::Op::JumpIfFalse)
                    | Some(bytecode::Op::PushHandler) => {
                        if ip + 2 < chunk.code.len() {
                            let short =
                                i16::from_be_bytes([chunk.code[ip + 1], chunk.code[ip + 2]]);
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
                    | Some(bytecode::Op::GetBuiltin)
                    | Some(bytecode::Op::MakeClosure)
                    | Some(bytecode::Op::GetPropertyIC)
                    | Some(bytecode::Op::Call) => ip += 1,
                    Some(bytecode::Op::GetGlobal) | Some(bytecode::Op::SetGlobal) => ip += 2,
                    Some(bytecode::Op::Jump)
                    | Some(bytecode::Op::JumpIfFalse)
                    | Some(bytecode::Op::PushHandler) => ip += 2,
                    _ => {}
                }
                if ip > 2000 {
                    break;
                }
            }

            Ok(())
        })() {
            eprintln!("[trace] Prelude trace preparation failed: {}", e)
        }
    }
    // Create a persistent VM for the REPL and reuse it for every input so
    // we don't recompile and recreate the VM on each line. The soft layer
    // (prelude) is executed into the same VM so its globals and any
    // runtime-installed state are available to the REPL.
    // Create VM and run prelude. blitzlib can call `__host_get(name)` during
    // bootstrap to capture host-provided values; raw host entries are never
    // placed into the globals table.
    let mut repl_vm = Vm::new(globals);

    // Run prelude. The soft layer provides all base objects (Object, Array, console, etc.).
    // Evaluate prelude via ModuleLoader into the repl_vm
    if let Err(e) = loader
        .evaluate_entry(&prelude_resolved, &mut repl_vm)
        .map_err(tsblitz::errors::Error::from)
    {
        eprintln!("Runtime error in soft layer: {}", e);
        return Err(tsblitz::errors::RuntimeError::from(format!(
            "Runtime error in soft layer: {}",
            e
        ))
        .into());
    }

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
            .map_err(tsblitz::errors::Error::from)?;
        loader
            .evaluate_entry(&path, &mut repl_vm)
            .map_err(tsblitz::errors::Error::from)?;
        return Ok(());
    }

    // Start REPL (explicit flag or no file provided)
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
