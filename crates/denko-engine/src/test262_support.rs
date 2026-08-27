// SPDX-License-Identifier: MPL-2.0
use crate::compiler;
use crate::ecmascript::semantics::byte_views::ARRAY_BUFFER_DATA_SLOT;
use crate::parser::Parser;
use crate::runtime_utils;
use crate::value::{
  BuiltinFn, NativeFn, ObjectData, PropertyDescriptor, PropertyKey, Value, define_native_global,
};
use crate::vm::Vm;
use std::cell::RefCell;
use std::rc::Rc;

/// State captured from Test262 async completion markers printed via `$DONE`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AsyncCompletion {
  /// Async test finished successfully.
  Complete,
  /// Async test finished with a failure payload.
  Failure(String),
}

fn print_args(
  vm: &mut Vm,
  args: &[Value],
) -> String {
  let value = args.first().cloned().unwrap_or(Value::Undefined);
  match vm.to_string(&value) {
    Ok(s) => s.to_string(),
    Err(_) => format!("{}", value),
  }
}

fn builtin_eval_script(
  vm: &mut Vm,
  globals: &Rc<RefCell<ObjectData>>,
  args: &[Value],
) -> Result<Value, String> {
  if args.is_empty() {
    return Ok(Value::Null);
  }
  let src = match &args[0] {
    Value::String(s) => s.clone(),
    _ => return Ok(Value::Null),
  };

  let chunk = crate::typecheck::with_strict_override(None, || {
    let src = src.as_str_lossy();
    let session = Parser::parse_source(src).map_err(|e| format!("evalScript parse: {}", e))?;
    compiler::compile_repl_session(&session).map_err(|e| format!("evalScript compile: {}", e))
  })?;
  vm.run_nested_in_realm(chunk, Rc::clone(globals))
    .map_err(|failure| format!("evalScript runtime: {}", failure.error))
}

fn make_test262_object(
  globals: &Rc<RefCell<ObjectData>>,
  print_native: &NativeFn,
) -> Value {
  let mut data = ObjectData::new();
  let function_proto = globals
    .borrow()
    .properties
    .get(&PropertyKey::String("Function".to_string().into()))
    .and_then(|desc| match desc {
      PropertyDescriptor::Data { value, .. } => Some(value.clone()),
      _ => None,
    })
    .and_then(|function_ctor| match function_ctor {
      Value::Object(obj) => obj
        .borrow()
        .properties
        .get(&PropertyKey::String("prototype".to_string().into()))
        .and_then(|desc| match desc {
          PropertyDescriptor::Data { value, .. } => Some(value.clone()),
          _ => None,
        }),
      Value::BuiltinObject(_, props) => props
        .borrow()
        .properties
        .get(&PropertyKey::String("prototype".to_string().into()))
        .and_then(|desc| match desc {
          PropertyDescriptor::Data { value, .. } => Some(value.clone()),
          _ => None,
        }),
      _ => None,
    })
    .unwrap_or(Value::Null);

  let host_builtin = |f: BuiltinFn| {
    let props = Rc::new(RefCell::new(ObjectData::with_prototype(
      function_proto.clone(),
    )));
    Value::BuiltinObject(f, props)
  };

  let eval_globals = Rc::clone(globals);
  data.properties.insert(
    PropertyKey::String("evalScript".to_string().into()),
    PropertyDescriptor::ordinary_data(host_builtin(Rc::new(move |vm, args| {
      let offset = usize::from(vm.builtin_this_arg_prepended());
      builtin_eval_script(vm, &eval_globals, &args[offset..])
        .map_err(crate::errors::RuntimeError::from)
    }))),
  );
  data.properties.insert(
    PropertyKey::String("IsHTMLDDA".to_string().into()),
    PropertyDescriptor::ordinary_data(Value::Object(Rc::new(RefCell::new(ObjectData::new())))),
  );
  data.properties.insert(
    PropertyKey::String("detachArrayBuffer".to_string().into()),
    PropertyDescriptor::ordinary_data(host_builtin(Rc::new(|vm, args| {
      let offset = usize::from(vm.builtin_this_arg_prepended());
      if let Some(Value::Object(obj)) = args.get(offset) {
        let mut obj_data = obj.borrow_mut();
        obj_data.set_internal_slot(ARRAY_BUFFER_DATA_SLOT, Value::Null);
        obj_data.properties.insert(
          PropertyKey::String("byteLength".to_string().into()),
          PropertyDescriptor::ordinary_data(Value::Number(0.0)),
        );
      }
      Ok(Value::Undefined)
    }))),
  );
  data.properties.insert(
    PropertyKey::String("gc".to_string().into()),
    PropertyDescriptor::ordinary_data(host_builtin(Rc::new(|vm, _args| {
      let _ = vm.collect_garbage_prototype();
      Ok(Value::Undefined)
    }))),
  );
  data.properties.insert(
    PropertyKey::String("AbstractModuleSource".to_string().into()),
    PropertyDescriptor::ordinary_data(Value::Undefined),
  );

  let noop_agent_method: BuiltinFn = Rc::new(|_: &mut Vm, _: &[Value]| Ok(Value::Undefined));
  let agent_obj = Rc::new(RefCell::new(ObjectData::new()));
  for name in [
    "getReport",
    "sleep",
    "broadcast",
    "receiveBroadcast",
    "report",
  ] {
    agent_obj.borrow_mut().properties.insert(
      PropertyKey::String(name.to_string().into()),
      PropertyDescriptor::ordinary_data(host_builtin(noop_agent_method.clone())),
    );
  }
  data.properties.insert(
    PropertyKey::String("agent".to_string().into()),
    PropertyDescriptor::ordinary_data(Value::Object(agent_obj)),
  );

  let print_native = Rc::clone(print_native);
  data.properties.insert(
    PropertyKey::String("createRealm".to_string().into()),
    PropertyDescriptor::ordinary_data(host_builtin(Rc::new(move |vm, _args| {
      let realm_global = Rc::new(RefCell::new(ObjectData::new()));
      let realm_root = vm.create_host_root(Value::Object(Rc::clone(&realm_global)));
      realm_global.borrow_mut().properties.insert(
        PropertyKey::String("globalThis".to_string().into()),
        PropertyDescriptor::ordinary_data(Value::Object(realm_global.clone())),
      );
      define_native_global(&realm_global, "print", print_native.clone());
      runtime_utils::install_minimal_kernel_constructors(&realm_global);
      let new_262 = make_test262_object(&realm_global, &print_native);
      realm_global.borrow_mut().properties.insert(
        PropertyKey::String("$262".to_string().into()),
        PropertyDescriptor::ordinary_data(new_262.clone()),
      );
      if let Value::Object(obj) = &new_262 {
        obj.borrow_mut().properties.insert(
          PropertyKey::String("global".to_string().into()),
          PropertyDescriptor::ordinary_data(Value::Object(realm_global)),
        );
      }
      vm.release_host_root(realm_root)?;
      Ok(new_262)
    }))),
  );

  data.properties.insert(
    PropertyKey::String("global".to_string().into()),
    PropertyDescriptor::ordinary_data(Value::Object(Rc::clone(globals))),
  );

  Value::Object(Rc::new(RefCell::new(data)))
}

/// Parses the special async completion markers emitted by `doneprintHandle.js`.
pub fn async_completion_marker(s: &str) -> Option<AsyncCompletion> {
  if s == "Test262:AsyncTestComplete" {
    Some(AsyncCompletion::Complete)
  } else {
    s.strip_prefix("Test262:AsyncTestFailure:")
      .map(|msg| AsyncCompletion::Failure(msg.to_string()))
  }
}

/// Build a global object configured with the basic Test262 host hooks using
/// the provided host `print` implementation.
pub fn make_test262_globals_with_print(print_native: NativeFn) -> Rc<RefCell<ObjectData>> {
  let globals: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(ObjectData::new()));
  globals.borrow_mut().properties.insert(
    PropertyKey::String("globalThis".to_string().into()),
    PropertyDescriptor::ordinary_data(Value::Object(globals.clone())),
  );

  define_native_global(&globals, "print", print_native.clone());
  globals.borrow_mut().properties.insert(
    PropertyKey::String("NaN".to_string().into()),
    PropertyDescriptor::immutable_data(Value::Number(f64::NAN)),
  );
  globals.borrow_mut().properties.insert(
    PropertyKey::String("Infinity".to_string().into()),
    PropertyDescriptor::immutable_data(Value::Number(f64::INFINITY)),
  );

  runtime_utils::install_minimal_kernel_constructors(&globals);
  let test262_obj = make_test262_object(&globals, &print_native);
  globals.borrow_mut().properties.insert(
    PropertyKey::String("$262".to_string().into()),
    PropertyDescriptor::ordinary_data(test262_obj),
  );
  globals
}

/// Build a global object configured with the basic Test262 host hooks.
pub fn make_test262_globals() -> Rc<RefCell<ObjectData>> {
  let print_native: NativeFn = Rc::new(|vm, _this, args| {
    println!("{}", print_args(vm, args));
    Ok(Value::Null)
  });
  make_test262_globals_with_print(print_native)
}
