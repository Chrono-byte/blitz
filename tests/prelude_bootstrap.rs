//! Test the preload bootstrap that helps elevate us up to the level needed to run user code.

use std::cell::RefCell;
use std::rc::Rc;

use tsblitz::bytecode::Chunk;
use tsblitz::value::{ObjectData, PropertyDescriptor, PropertyKey, Value};
use tsblitz::vm::Vm;
use tsblitz::{bundle_entry, compile};

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

        let mut new_chunk = Chunk::new();
        new_chunk.code = rc_chunk.code.clone();
        new_chunk.constants = rc_chunk.constants.clone();
        new_chunk.lines = rc_chunk.lines.clone();
        // Clear global type annotations so runtime won't enforce them during bootstrap.
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
        PropertyDescriptor::data(global_this),
    );

    // Minimal builtin printer used by soft code during bootstrap.
    fn test_native_print(
        _vm: &mut Vm,
        _args: &[Value],
    ) -> Result<Value, tsblitz::errors::RuntimeError> {
        // Intentionally no-op for tests.
        Ok(Value::Null)
    }

    // Minimal native_new_object used during bootstrap.
    fn test_native_new_object(
        _vm: &mut Vm,
        _args: &[Value],
    ) -> Result<Value, tsblitz::errors::RuntimeError> {
        use std::cell::RefCell;
        use std::rc::Rc;
        let v = Value::Object(Rc::new(RefCell::new(ObjectData::new())));
        Ok(v)
    }

    // Publish host primitives into globals as main.rs does.
    globals.borrow_mut().properties.insert(
        PropertyKey::String("__native_print".to_string()),
        PropertyDescriptor::data(Value::Builtin(test_native_print)),
    );
    globals.borrow_mut().properties.insert(
        PropertyKey::String("__native_new_object".to_string()),
        PropertyDescriptor::data(Value::Builtin(test_native_new_object)),
    );
    // Also publish convenience host-level wrappers so the bundled prelude can
    // import them during bootstrap ordering variants in tests.
    globals.borrow_mut().properties.insert(
        PropertyKey::String("host_print".to_string()),
        PropertyDescriptor::data(Value::Builtin(test_native_print)),
    );
    globals.borrow_mut().properties.insert(
        PropertyKey::String("native_print".to_string()),
        PropertyDescriptor::data(Value::Builtin(test_native_print)),
    );
    globals.borrow_mut().properties.insert(
        PropertyKey::String("debugValue".to_string()),
        PropertyDescriptor::data(Value::Builtin(|_vm, _args| Ok(Value::Null))),
    );

    // Provide a minimal `host` object used by some prelude paths
    let mut host_obj = ObjectData::new();
    host_obj.properties.insert(
        PropertyKey::String("host_print".to_string()),
        PropertyDescriptor::data(Value::Builtin(test_native_print)),
    );
    host_obj.properties.insert(
        PropertyKey::String("native_new_object".to_string()),
        PropertyDescriptor::data(Value::Builtin(test_native_new_object)),
    );
    host_obj.properties.insert(
        PropertyKey::String("native_print".to_string()),
        PropertyDescriptor::data(Value::Builtin(test_native_print)),
    );
    host_obj.properties.insert(
        PropertyKey::String("debugValue".to_string()),
        PropertyDescriptor::data(Value::Builtin(|_vm, _args| Ok(Value::Null))),
    );
    globals.borrow_mut().properties.insert(
        PropertyKey::String("host".to_string()),
        PropertyDescriptor::data(Value::Object(Rc::new(RefCell::new(host_obj)))),
    );

    // Provide a simple console object backed by the builtin printer.
    let mut console_obj = ObjectData::new();
    console_obj.properties.insert(
        PropertyKey::String("log".to_string()),
        PropertyDescriptor::data(Value::Builtin(test_native_print)),
    );
    console_obj.properties.insert(
        PropertyKey::String("info".to_string()),
        PropertyDescriptor::data(Value::Builtin(test_native_print)),
    );
    console_obj.properties.insert(
        PropertyKey::String("warn".to_string()),
        PropertyDescriptor::data(Value::Builtin(test_native_print)),
    );
    console_obj.properties.insert(
        PropertyKey::String("error".to_string()),
        PropertyDescriptor::data(Value::Builtin(test_native_print)),
    );
    console_obj.properties.insert(
        PropertyKey::String("debug".to_string()),
        PropertyDescriptor::data(Value::Builtin(test_native_print)),
    );
    globals.borrow_mut().properties.insert(
        PropertyKey::String("console".to_string()),
        PropertyDescriptor::data(Value::Object(Rc::new(RefCell::new(console_obj)))),
    );

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
    let session = bundle_entry(&prelude_path).expect("bundle prelude ok");
    let program = session.program();
    let chunk = compile(program).expect("compile prelude ok");

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
