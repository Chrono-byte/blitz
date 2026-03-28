//! Test the preload bootstrap that helps elevate us up to the level needed to run user code.

use std::cell::RefCell;
use std::rc::Rc;

use blitz::bytecode::Chunk;
use blitz::compile;
use blitz::module::ModuleLoader;
use blitz::value::{ObjectData, PropertyDescriptor, PropertyKey, Value};
use blitz::vm::Vm;

/// Clear slot types recursively for a Chunk graph.
///
/// This mirrors the helper used by `main.rs` so the soft prelude (which is
/// intentionally dynamic) won't be blocked by runtime slot-type checks.
fn clear_slot_types_in_chunk(chunk: Chunk) -> Chunk {
    use std::collections::HashMap;
    use std::rc::Rc;

    fn rebuild(rc_chunk: &Rc<Chunk>, visited: &mut HashMap<*const Chunk, Rc<Chunk>>) -> Rc<Chunk> {
        let key = Rc::as_ptr(rc_chunk) as *const Chunk;
        if let Some(existing) = visited.get(&key) {
            return Rc::clone(existing);
        }

        let mut new_chunk = Chunk::new(Rc::new(String::new()));
        new_chunk.code = rc_chunk.code.clone();
        new_chunk.constants = rc_chunk.constants.clone();
        new_chunk.lines = rc_chunk.lines.clone();
        // No global_types field in current Chunk layout; copy only the
        // existing basic fields and rebuild functions below.

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
    let mut visited: HashMap<*const Chunk, Rc<Chunk>> = HashMap::new();
    let rc_cleared = rebuild(&rc, &mut visited);
    // Return owned Chunk
    (*rc_cleared).clone()
}

#[test]
fn prelude_bootstrap_runs() {
    // Create a minimal VM globals map similar to main.rs bootstrap.
    let globals: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(ObjectData::new()));

    // Provide a global `globalThis` object
    let global_this = Value::Object(Rc::new(RefCell::new(ObjectData::new())));
    globals.borrow_mut().properties.insert(
        PropertyKey::String("globalThis".to_string()),
        PropertyDescriptor::data(global_this.clone()),
    );

    // Minimal builtin printer used by soft code during bootstrap.
    fn test_native_print(
        _vm: &mut Vm,
        _args: &[Value],
    ) -> Result<Value, blitz::errors::RuntimeError> {
        // Print a concise diagnostic to stdout to help debug bootstrap ordering.
        // Show the first arg if present otherwise print a marker.
        if !_args.is_empty() {
            println!("[host_print] {}", _args[0]);
        } else {
            println!("[host_print] <empty>");
        }
        Ok(Value::Null)
    }

    // Minimal native_new_object used during bootstrap.
    fn test_native_new_object(
        _vm: &mut Vm,
        _args: &[Value],
    ) -> Result<Value, blitz::errors::RuntimeError> {
        use std::cell::RefCell;
        use std::rc::Rc;
        let v = Value::Object(Rc::new(RefCell::new(ObjectData::new())));
        // No-op here; the test harness mirrors native helpers onto `globalThis`
        // outside of the native implementation so nested functions don't need
        // to capture outer variables.
        Ok(v)
    }

    // Publish host primitives into globals as main.rs does.
    // Construct builtins with explicit BuiltinFn typing to avoid inference issues
    let builtin_print: blitz::value::BuiltinFn = Rc::new(|vm, args| test_native_print(vm, args));
    let builtin_new_object: blitz::value::BuiltinFn =
        Rc::new(|vm, args| test_native_new_object(vm, args));

    globals.borrow_mut().properties.insert(
        PropertyKey::String("__native_print".to_string()),
        PropertyDescriptor::data(Value::Builtin(builtin_print.clone())),
    );
    globals.borrow_mut().properties.insert(
        PropertyKey::String("__native_new_object".to_string()),
        PropertyDescriptor::data(Value::Builtin(builtin_new_object.clone())),
    );
    // Also publish convenience host-level wrappers so the inlined prelude can
    // import them during bootstrap ordering variants in tests.
    globals.borrow_mut().properties.insert(
        PropertyKey::String("host_print".to_string()),
        PropertyDescriptor::data(Value::Builtin(builtin_print.clone())),
    );
    globals.borrow_mut().properties.insert(
        PropertyKey::String("native_print".to_string()),
        PropertyDescriptor::data(Value::Builtin(builtin_print.clone())),
    );
    let builtin_null: blitz::value::BuiltinFn = Rc::new(|_vm, _args| Ok(Value::Null));
    globals.borrow_mut().properties.insert(
        PropertyKey::String("debugValue".to_string()),
        PropertyDescriptor::data(Value::Builtin(builtin_null.clone())),
    );

    // Provide minimal kernel-native helpers the soft runtime expects.
    let builtin_typeof: blitz::value::BuiltinFn = Rc::new(|vm, args| {
        let v = args.get(0).cloned().unwrap_or(Value::Undefined);
        Ok(Value::String(vm.typeof_value(&v)))
    });
    let builtin_to_string: blitz::value::BuiltinFn = Rc::new(|vm, args| {
        let v = args.get(0).cloned().unwrap_or(Value::Undefined);
        let s = vm.to_string(&v)?;
        Ok(Value::String(s))
    });
    let builtin_to_number: blitz::value::BuiltinFn = Rc::new(|vm, args| {
        let v = args.get(0).cloned().unwrap_or(Value::Undefined);
        let n = vm.to_number_value(&v)?;
        Ok(Value::Number(n))
    });
    let builtin_to_boolean: blitz::value::BuiltinFn = Rc::new(|vm, args| {
        let v = args.get(0).cloned().unwrap_or(Value::Undefined);
        Ok(Value::Bool(vm.to_boolean(&v)))
    });
    let builtin_abstract_equality: blitz::value::BuiltinFn = Rc::new(|vm, args| {
        let a = args.get(0).cloned().unwrap_or(Value::Undefined);
        let b = args.get(1).cloned().unwrap_or(Value::Undefined);
        Ok(Value::Bool(vm.abstract_equality(&a, &b)?))
    });
    let builtin_instanceof: blitz::value::BuiltinFn = Rc::new(|vm, args| {
        let value = args.get(0).cloned().unwrap_or(Value::Undefined);
        let ctor = args.get(1).cloned().unwrap_or(Value::Undefined);
        Ok(Value::Bool(vm.instance_of(&value, &ctor)?))
    });
    let builtin_in_property: blitz::value::BuiltinFn = Rc::new(|vm, args| {
        let prop = args.get(0).cloned().unwrap_or(Value::Undefined);
        let obj = args.get(1).cloned().unwrap_or(Value::Undefined);
        Ok(Value::Bool(vm.in_property(&prop, &obj)?))
    });

    // Register these helpers as globals the soft prelude will look up.
    globals.borrow_mut().properties.insert(
        PropertyKey::String("__native_typeof".to_string()),
        PropertyDescriptor::data(Value::Builtin(builtin_typeof.clone())),
    );
    globals.borrow_mut().properties.insert(
        PropertyKey::String("__native_toString".to_string()),
        PropertyDescriptor::data(Value::Builtin(builtin_to_string.clone())),
    );
    globals.borrow_mut().properties.insert(
        PropertyKey::String("__native_toNumber".to_string()),
        PropertyDescriptor::data(Value::Builtin(builtin_to_number.clone())),
    );
    globals.borrow_mut().properties.insert(
        PropertyKey::String("__native_toBoolean".to_string()),
        PropertyDescriptor::data(Value::Builtin(builtin_to_boolean.clone())),
    );
    globals.borrow_mut().properties.insert(
        PropertyKey::String("__native_abstractEquality".to_string()),
        PropertyDescriptor::data(Value::Builtin(builtin_abstract_equality.clone())),
    );
    globals.borrow_mut().properties.insert(
        PropertyKey::String("__native_instanceof".to_string()),
        PropertyDescriptor::data(Value::Builtin(builtin_instanceof.clone())),
    );
    globals.borrow_mut().properties.insert(
        PropertyKey::String("__native_inProperty".to_string()),
        PropertyDescriptor::data(Value::Builtin(builtin_in_property.clone())),
    );

    // Also mirror into the `globalThis` object by writing a fresh object
    // value rather than capturing `global_this` in closures (captures are
    // disallowed in nested fn items). This mirrors how a real host would
    // publish runtime helpers onto the global object.
    if let Value::Object(obj_rc) = global_this.clone() {
        let mut obj = obj_rc.borrow_mut();
        obj.properties.insert(
            PropertyKey::String("__native_print".to_string()),
            PropertyDescriptor::data(Value::Builtin(builtin_print.clone())),
        );
        obj.properties.insert(
            PropertyKey::String("__native_new_object".to_string()),
            PropertyDescriptor::data(Value::Builtin(builtin_new_object.clone())),
        );
        obj.properties.insert(
            PropertyKey::String("host_print".to_string()),
            PropertyDescriptor::data(Value::Builtin(builtin_print.clone())),
        );
        obj.properties.insert(
            PropertyKey::String("native_print".to_string()),
            PropertyDescriptor::data(Value::Builtin(builtin_print.clone())),
        );
        obj.properties.insert(
            PropertyKey::String("debugValue".to_string()),
            PropertyDescriptor::data(Value::Builtin(builtin_null.clone())),
        );
    }

    // Provide a minimal `host` object used by some prelude paths
    // Create a host object and publish it both as a top-level global and as
    // a property on `globalThis` so soft code can find it through either
    // access pattern.
    let host_obj_rc = Rc::new(RefCell::new(ObjectData::new()));
    host_obj_rc.borrow_mut().properties.insert(
        PropertyKey::String("host_print".to_string()),
        PropertyDescriptor::data(Value::Builtin(builtin_print.clone())),
    );
    host_obj_rc.borrow_mut().properties.insert(
        PropertyKey::String("native_new_object".to_string()),
        PropertyDescriptor::data(Value::Builtin(builtin_new_object.clone())),
    );
    host_obj_rc.borrow_mut().properties.insert(
        PropertyKey::String("native_print".to_string()),
        PropertyDescriptor::data(Value::Builtin(builtin_print.clone())),
    );
    host_obj_rc.borrow_mut().properties.insert(
        PropertyKey::String("debugValue".to_string()),
        PropertyDescriptor::data(Value::Builtin(builtin_null.clone())),
    );
    globals.borrow_mut().properties.insert(
        PropertyKey::String("host".to_string()),
        PropertyDescriptor::data(Value::Object(Rc::clone(&host_obj_rc))),
    );
    if let Value::Object(obj_rc) = &global_this {
        obj_rc.borrow_mut().properties.insert(
            PropertyKey::String("host".to_string()),
            PropertyDescriptor::data(Value::Object(Rc::clone(&host_obj_rc))),
        );
    }

    // Provide a simple console object backed by the builtin printer.
    let console_obj_rc = Rc::new(RefCell::new(ObjectData::new()));
    console_obj_rc.borrow_mut().properties.insert(
        PropertyKey::String("log".to_string()),
        PropertyDescriptor::data(Value::Builtin(builtin_print.clone())),
    );
    console_obj_rc.borrow_mut().properties.insert(
        PropertyKey::String("info".to_string()),
        PropertyDescriptor::data(Value::Builtin(builtin_print.clone())),
    );
    console_obj_rc.borrow_mut().properties.insert(
        PropertyKey::String("warn".to_string()),
        PropertyDescriptor::data(Value::Builtin(builtin_print.clone())),
    );
    console_obj_rc.borrow_mut().properties.insert(
        PropertyKey::String("error".to_string()),
        PropertyDescriptor::data(Value::Builtin(builtin_print.clone())),
    );
    console_obj_rc.borrow_mut().properties.insert(
        PropertyKey::String("debug".to_string()),
        PropertyDescriptor::data(Value::Builtin(builtin_print.clone())),
    );
    globals.borrow_mut().properties.insert(
        PropertyKey::String("console".to_string()),
        PropertyDescriptor::data(Value::Object(Rc::clone(&console_obj_rc))),
    );
    if let Value::Object(obj_rc) = &global_this {
        obj_rc.borrow_mut().properties.insert(
            PropertyKey::String("console".to_string()),
            PropertyDescriptor::data(Value::Object(Rc::clone(&console_obj_rc))),
        );
    }

    // Disable soft-layer tracing for the test to keep output quiet.
    globals.borrow_mut().properties.insert(
        PropertyKey::String("__TSBLITZ_TRACE".to_string()),
        PropertyDescriptor::data(Value::Bool(false)),
    );

    // Create VM
    let mut vm = Vm::new(Rc::clone(&globals));

    // Bundle and compile the soft prelude
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let prelude_path = manifest_dir.join("soft/prelude.ts");
    // Use the VM module loader to construct (parse) the prelude module so the
    // module resolution logic is exercised in the same way as the running
    // process. We only need the ParserSession.program() for compilation here.
    let mut loader = ModuleLoader::new();
    let base = prelude_path.parent().unwrap_or(std::path::Path::new("."));
    let rec = loader
        .construct(
            base,
            &format!("./{}", prelude_path.file_name().unwrap().to_string_lossy()),
        )
        .expect("prelude construct ok");
    // Hold the borrow while calling compile so the Program reference remains valid.
    let rec_b = rec.borrow();
    let program = rec_b.session.program();
    let chunk = compile(program, rec_b.session.source()).expect("compile prelude ok");

    // Clear slot types (the prelude is intentionally dynamic)
    let chunk = clear_slot_types_in_chunk(chunk);

    // Run the prelude. This should not error with "Call: not a function" or other
    // bootstrap-time failures.
    let res = vm.run(chunk);
    assert!(
        res.is_ok(),
        "Prelude bootstrap should run without error, got: {:?}",
        res.err()
    );
}
