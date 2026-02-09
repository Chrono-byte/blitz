//! CLI entry point for tsblitz VM.
//!
//! The CLI provides two main interfaces:
//! - **File execution**: Run a TypeScript file directly
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
//! # Run a TypeScript file
//! cargo run -- path/to/file.ts
//!
//! # Start REPL
//! cargo run -- --repl
//!
//! # Enable tracing
//! TSBLITZ_TRACE=1 cargo run -- file.ts
//! ```

use clap::Parser;
use std::cell::RefCell;
use std::collections::HashMap;
use std::env;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use tsblitz::bytecode;
use tsblitz::compiler;
use tsblitz::{
    bundle_entry, parser::Parser as TsParser, resolver::resolve_specifier, typecheck, value::Value,
    vm::Vm,
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

/// Clear slot type annotations recursively for a Chunk graph.
///
/// The soft prelude is intentionally dynamic and may pass values that don't
/// match static parameter annotations. Before executing the prelude we clear
/// slot types in every FunctionProto across the compiled chunk graph so the
/// VM won't enforce runtime type checks for those functions.
fn clear_slot_types_in_chunk(chunk: bytecode::Chunk) -> bytecode::Chunk {
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

/// Command line arguments for the tsblitz executable.
#[derive(Parser, Debug)]
#[command(name = "tsblitz", about = "Blitz — TypeScript VM (Rust runtime)")]
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
            vm.define_property_builtin(args)}))),
    );

    // Kernel-native coercion helpers: soft layer delegates ToString/ToNumber/ToBoolean
    // to the kernel during migration. These simple wrappers call into the VM
    // helpers implemented in `Vm` so soft runtime can use authoritative semantics
    // without reimplementing them in TypeScript.
    globals.borrow_mut().properties.insert(
        tsblitz::value::PropertyKey::String("__native_toString".to_string()),
        tsblitz::value::PropertyDescriptor::data(Value::Builtin(Rc::new(|vm, args| {
            let v = args.first().cloned().unwrap_or(Value::Undefined);
            vm.to_string(&v)
                .map(Value::String)}))),
    );
    globals.borrow_mut().properties.insert(
        tsblitz::value::PropertyKey::String("__native_toNumber".to_string()),
        tsblitz::value::PropertyDescriptor::data(Value::Builtin(Rc::new(|vm, args| {
            let v = args.first().cloned().unwrap_or(Value::Undefined);
            vm.to_number_value(&v)
                .map(Value::Number)}))),
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

    // Publish minimal kernel-owned constructors and their prototype objects so
    // the soft prelude can assume canonical constructors exist. We expose them
    // as callable builtin-backed objects so they behave like functions (callable)
    // and also expose a property bag for `.prototype` where std installers can
    // attach methods.

    // Helper: create an object-backed constructor value where the object stores
    // the `prototype` own property and the value is callable through the new
    // `BuiltinObject` variant.
    // Object
    let object_proto_map: Rc<RefCell<tsblitz::value::ObjectData>> =
        Rc::new(RefCell::new(tsblitz::value::ObjectData::new()));
    let object_ctor_map: Rc<RefCell<tsblitz::value::ObjectData>> =
        Rc::new(RefCell::new(tsblitz::value::ObjectData::new()));
    // ctor.prototype -> proto
    object_ctor_map.borrow_mut().properties.insert(
        tsblitz::value::PropertyKey::String("prototype".to_string()),
        tsblitz::value::PropertyDescriptor::data(Value::Object(Rc::clone(&object_proto_map))),
    );
    // proto.constructor -> ctor (temporary placeholder, will be filled after ctor value exists)
    // We'll create a callable builtin object below and then set proto.constructor -> ctor

    // Array
    let array_proto_map: Rc<RefCell<tsblitz::value::ObjectData>> =
        Rc::new(RefCell::new(tsblitz::value::ObjectData::new()));
    let array_ctor_map: Rc<RefCell<tsblitz::value::ObjectData>> =
        Rc::new(RefCell::new(tsblitz::value::ObjectData::new()));
    array_ctor_map.borrow_mut().properties.insert(
        tsblitz::value::PropertyKey::String("prototype".to_string()),
        tsblitz::value::PropertyDescriptor::data(Value::Object(Rc::clone(&array_proto_map))),
    );

    // String
    let string_proto_map: Rc<RefCell<tsblitz::value::ObjectData>> =
        Rc::new(RefCell::new(tsblitz::value::ObjectData::new()));
    let string_ctor_map: Rc<RefCell<tsblitz::value::ObjectData>> =
        Rc::new(RefCell::new(tsblitz::value::ObjectData::new()));
    string_ctor_map.borrow_mut().properties.insert(
        tsblitz::value::PropertyKey::String("prototype".to_string()),
        tsblitz::value::PropertyDescriptor::data(Value::Object(Rc::clone(&string_proto_map))),
    );

    // Build callable constructor objects by using the new BuiltinObject variant.
    // Each builtin captures a reference to its own property bag so it can read
    // the `.prototype` value when creating instances.
    use tsblitz::value::BuiltinFn;
    use tsblitz::value::PropertyKey as PKey;

    // Object constructor: returns a fresh object whose prototype is ctor.prototype
    let object_ctor_props = Rc::clone(&object_ctor_map);
    let object_ctor_fn: BuiltinFn = Rc::new(move |_vm, _args| {
        // read .prototype from constructor props
        let proto = object_ctor_props
            .borrow()
            .properties
            .get(&PKey::String("prototype".to_string()))
            .and_then(|d| match d {
                tsblitz::value::PropertyDescriptor::Data { value, .. } => Some(value.clone()),
                _ => None,
            })
            .unwrap_or(Value::Null);
        let mut inst = tsblitz::value::ObjectData::new();
        inst.prototype = proto;
        Ok(Value::Object(Rc::new(RefCell::new(inst))))
    });

    let object_ctor_val = Value::BuiltinObject(object_ctor_fn.clone(), Rc::clone(&object_ctor_map));
    // now set proto.constructor -> ctor
    object_proto_map.borrow_mut().properties.insert(
        tsblitz::value::PropertyKey::String("constructor".to_string()),
        tsblitz::value::PropertyDescriptor::data(object_ctor_val),
    );
    // Install Object.prototype.toString and valueOf as builtin functions so
    // runtime coercions have sane defaults.
    object_proto_map.borrow_mut().properties.insert(
        tsblitz::value::PropertyKey::String("toString".to_string()),
        tsblitz::value::PropertyDescriptor::data(Value::Builtin(Rc::new(|_vm, args| {
            // `this` is passed as first arg for builtins
            let this = args.first().cloned().unwrap_or(Value::Null);
            let tag = this.object_tag();
            Ok(Value::String(format!("[object {}]", tag)))
        }))),
    );
    object_proto_map.borrow_mut().properties.insert(
        tsblitz::value::PropertyKey::String("valueOf".to_string()),
        tsblitz::value::PropertyDescriptor::data(Value::Builtin(Rc::new(|_vm, args| {
            let this = args.first().cloned().unwrap_or(Value::Null);
            Ok(this)
        }))),
    );
    globals.borrow_mut().properties.insert(
        tsblitz::value::PropertyKey::String("Object".to_string()),
        tsblitz::value::PropertyDescriptor::data(Value::BuiltinObject(
            object_ctor_fn.clone(),
            Rc::clone(&object_ctor_map),
        )),
    );

    // Array constructor: build a Value::Array from arguments
    let array_ctor_props = Rc::clone(&array_ctor_map);
    let array_ctor_fn: BuiltinFn = Rc::new(move |_vm, args| {
        let elems: Vec<Value> = args.to_vec();
        let arr_rc = Rc::new(RefCell::new(elems));
        // prepare property bag with length and prototype
        let props: Rc<RefCell<tsblitz::value::ObjectData>> =
            Rc::new(RefCell::new(tsblitz::value::ObjectData::new()));
        props.borrow_mut().properties.insert(
            PKey::String("length".to_string()),
            tsblitz::value::PropertyDescriptor::data(Value::Number(arr_rc.borrow().len() as f64)),
        );
        // attach prototype from constructor props if present
        if let Some(pval) = array_ctor_props
            .borrow()
            .properties
            .get(&PKey::String("prototype".to_string()))
            .and_then(|d| match d {
                tsblitz::value::PropertyDescriptor::Data { value, .. } => Some(value.clone()),
                _ => None,
            })
        {
            props.borrow_mut().prototype = pval;
        }
        Ok(Value::Array(arr_rc, props))
    });

    let array_ctor_val = Value::BuiltinObject(array_ctor_fn.clone(), Rc::clone(&array_ctor_map));
    array_proto_map.borrow_mut().properties.insert(
        tsblitz::value::PropertyKey::String("constructor".to_string()),
        tsblitz::value::PropertyDescriptor::data(array_ctor_val),
    );
    globals.borrow_mut().properties.insert(
        tsblitz::value::PropertyKey::String("Array".to_string()),
        tsblitz::value::PropertyDescriptor::data(Value::BuiltinObject(
            array_ctor_fn.clone(),
            Rc::clone(&array_ctor_map),
        )),
    );

    // String constructor: coerce argument to string primitive
    let _string_ctor_props = Rc::clone(&string_ctor_map);
    let string_ctor_fn: BuiltinFn = Rc::new(move |_vm, args| {
        if args.is_empty() {
            return Ok(Value::String("".to_string()));
        }
        // Use basic ToString for primitives; for objects return primitive '[object Object]'
        match &args[0] {
            Value::String(s) => Ok(Value::String(s.clone())),
            Value::Number(n) => Ok(Value::String(n.to_string())),
            Value::Bool(b) => Ok(Value::String(
                (if *b { "true" } else { "false" }).to_string(),
            )),
            Value::Null => Ok(Value::String("null".to_string())),
            Value::Undefined => Ok(Value::String("undefined".to_string())),
            Value::BigInt(n) => Ok(Value::String(n.to_string())),
            _ => Ok(Value::String("[object Object]".to_string())),
        }
    });

    let string_ctor_val = Value::BuiltinObject(string_ctor_fn.clone(), Rc::clone(&string_ctor_map));
    string_proto_map.borrow_mut().properties.insert(
        tsblitz::value::PropertyKey::String("constructor".to_string()),
        tsblitz::value::PropertyDescriptor::data(string_ctor_val),
    );
    globals.borrow_mut().properties.insert(
        tsblitz::value::PropertyKey::String("String".to_string()),
        tsblitz::value::PropertyDescriptor::data(Value::BuiltinObject(
            string_ctor_fn.clone(),
            Rc::clone(&string_ctor_map),
        )),
    );

    // Parse CLI early so clap can handle --help/--version before doing work.
    let cli = Cli::parse();

    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let prelude_path = manifest_dir.join("soft/prelude.ts");
    // Resolve the prelude path into a concrete file using oxc_resolver wrapper
    let prelude_resolved = resolve_specifier(
        prelude_path.parent().unwrap_or(Path::new(".")),
        &format!("./{}", prelude_path.file_name().unwrap().to_string_lossy()),
    )?;
    let prelude_session = bundle_entry(&prelude_resolved)?;
    let prelude_program = prelude_session.program();

    // Skip type checking for soft layer prelude (has dynamic patterns that typechecker doesn't handle yet)
    let chunk = compiler::compile_oxc(prelude_program).map_err(tsblitz::errors::Error::from)?;
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
            match globals
                .borrow()
                .properties
                .get(&tsblitz::value::PropertyKey::String((*name).to_string()))
                .and_then(|d| match d {
                    tsblitz::value::PropertyDescriptor::Data { value, .. } => Some(value),
                    _ => None,
                }) {
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

    // If a file was provided, run it and exit. Otherwise start REPL.
    if let Some(path) = cli.file {
        let session = bundle_entry(&path)?;
        let program = session.program();
        run_program(&mut repl_vm, program)?;
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
