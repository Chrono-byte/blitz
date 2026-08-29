// SPDX-License-Identifier: MPL-2.0
//! Function constructor and `Function.prototype` builtins.

use std::cell::RefCell;
use std::rc::Rc;

use crate::RuntimeError;
use crate::compiler;
use crate::ecmascript::semantics::builtin_object;
use crate::ecmascript::semantics::create_list_from_array_like;
use crate::parser::Parser;
use crate::runtime_utils::{set_builtin_length, set_builtin_name, syntax_error};
use crate::value::{BuiltinFn, ObjectData, PropertyDescriptor, PropertyKey, SymbolId, Value};
use crate::vm::Vm;

fn function_call(
  vm: &mut Vm,
  args: &[Value],
) -> Result<Value, RuntimeError> {
  let callee = args.first().cloned().unwrap_or(Value::Undefined);
  let this_arg = args.get(1).cloned().unwrap_or(Value::Undefined);
  let rest = args.get(2..).unwrap_or(&[]);
  vm.call_value_with_this(&callee, &this_arg, rest)
}

fn function_apply(
  vm: &mut Vm,
  args: &[Value],
) -> Result<Value, RuntimeError> {
  let callee = args.first().cloned().unwrap_or(Value::Undefined);
  let this_arg = args.get(1).cloned().unwrap_or(Value::Undefined);
  if !Vm::is_callable_value(&callee) {
    return Err(vm.throw_type_error("Function.prototype.apply called on non-callable"));
  }

  let call_args = match args.get(2) {
    None | Some(Value::Undefined) | Some(Value::Null) => Vec::new(),
    Some(arguments_list) => create_list_from_array_like(vm, arguments_list)?,
  };
  vm.call_value_with_this(&callee, &this_arg, &call_args)
}

fn function_to_string(
  vm: &mut Vm,
  args: &[Value],
) -> Result<Value, RuntimeError> {
  let this = args.first().cloned().unwrap_or(Value::Undefined);
  let source = if Vm::is_callable_value(&this) {
    vm.function_to_string(&this)?
  } else {
    return Err(vm.throw_type_error("Function.prototype.toString called on incompatible receiver"));
  };
  Ok(Value::String(source.into()))
}

/// The Function constructor installed into the global object.
pub(crate) struct FunctionIntrinsics {
  pub(crate) constructor: Value,
}

pub(crate) fn create_generator_function_constructor(
  globals: &Rc<RefCell<ObjectData>>,
  function_constructor: &Value,
  generator_function_prototype: &Rc<RefCell<ObjectData>>,
) -> Value {
  let constructor_properties = Rc::new(RefCell::new(ObjectData::with_prototype(
    function_constructor.clone(),
  )));
  constructor_properties
    .borrow_mut()
    .properties
    .insert(
      PropertyKey::prototype(),
      PropertyDescriptor::builtin_prototype(Value::Object(Rc::clone(generator_function_prototype))),
    );

  let constructor_globals = Rc::clone(globals);
  let constructor = Value::BuiltinObject(
    Rc::new(move |vm, args| {
      let off = usize::from(vm.pending_constructor_new_target().is_some());
      let user_args = args.get(off..).unwrap_or_default();
      let body = if let Some(last) = user_args.last() {
        vm.to_string(last)?
      } else {
        js_string::JsString::empty()
      };
      let mut parameters = Vec::with_capacity(user_args.len().saturating_sub(1));
      for argument in user_args
        .iter()
        .take(user_args.len().saturating_sub(1))
      {
        parameters.push(vm.to_string(argument)?);
      }
      let source = crate::js!(
        format(script),
        "(function* anonymous({}\n) {{\n{}\n}})",
        parameters
          .iter()
          .map(js_string::JsString::as_str_lossy)
          .collect::<Vec<_>>()
          .join(","),
        body
      );
      let code = crate::typecheck::with_strict_override(None, || {
        let session = Parser::parse_source(&source)
          .map_err(|error| syntax_error(vm, format!("parse: {error}")))?;
        compiler::compile_repl_session(&session)
          .map_err(|error| syntax_error(vm, format!("compile: {error}")))
      })?;
      let mut child = Vm::new(Rc::clone(&constructor_globals));
      let result = child.run(code)?;
      if let Value::Closure { props, .. } = &result {
        props.borrow_mut().set_internal_slot(
          "[[RealmGlobals]]",
          Value::Object(Rc::clone(&constructor_globals)),
        );
      }
      Ok(result)
    }),
    constructor_properties,
  );
  set_builtin_name(&constructor, "GeneratorFunction");
  set_builtin_length(&constructor, 1.0);
  generator_function_prototype
    .borrow_mut()
    .properties
    .insert(
      PropertyKey::String("constructor".into()),
      PropertyDescriptor::Data {
        value: constructor.clone(),
        writable: false,
        enumerable: false,
        configurable: true,
      },
    );
  constructor
}

pub(crate) fn create_async_function_constructor(
  globals: &Rc<RefCell<ObjectData>>,
  function_constructor: &Value,
  async_function_prototype: &Rc<RefCell<ObjectData>>,
) -> Value {
  let constructor_properties = Rc::new(RefCell::new(ObjectData::with_prototype(
    function_constructor.clone(),
  )));
  constructor_properties
    .borrow_mut()
    .properties
    .insert(
      PropertyKey::prototype(),
      PropertyDescriptor::builtin_prototype(Value::Object(Rc::clone(async_function_prototype))),
    );

  let constructor_globals = Rc::clone(globals);
  let constructor = Value::BuiltinObject(
    Rc::new(move |vm, args| {
      let off = usize::from(vm.pending_constructor_new_target().is_some());
      let user_args = args.get(off..).unwrap_or_default();
      let body = if let Some(last) = user_args.last() {
        vm.to_string(last)?
      } else {
        js_string::JsString::empty()
      };
      let mut parameters = Vec::with_capacity(user_args.len().saturating_sub(1));
      for argument in user_args
        .iter()
        .take(user_args.len().saturating_sub(1))
      {
        parameters.push(vm.to_string(argument)?);
      }
      let source = crate::js!(
        format(script),
        "(async function anonymous({}\n) {{\n{}\n}})",
        parameters
          .iter()
          .map(js_string::JsString::as_str_lossy)
          .collect::<Vec<_>>()
          .join(","),
        body
      );
      let code = crate::typecheck::with_strict_override(None, || {
        let session = Parser::parse_source(&source)
          .map_err(|error| syntax_error(vm, format!("parse: {error}")))?;
        compiler::compile_repl_session(&session)
          .map_err(|error| syntax_error(vm, format!("compile: {error}")))
      })?;
      let mut child = Vm::new(Rc::clone(&constructor_globals));
      let result = child.run(code)?;
      if let Value::Closure { props, .. } = &result {
        props.borrow_mut().set_internal_slot(
          "[[RealmGlobals]]",
          Value::Object(Rc::clone(&constructor_globals)),
        );
      }
      Ok(result)
    }),
    constructor_properties,
  );
  set_builtin_name(&constructor, "AsyncFunction");
  set_builtin_length(&constructor, 1.0);
  async_function_prototype
    .borrow_mut()
    .properties
    .insert(
      PropertyKey::String("constructor".into()),
      PropertyDescriptor::Data {
        value: constructor.clone(),
        writable: false,
        enumerable: false,
        configurable: true,
      },
    );
  constructor
}

/// Creates the Function constructor and installs the properties of
/// `Function.prototype` on the preallocated foundation objects.
///
/// The foundation allocator owns the cyclic Function constructor/prototype
/// graph. Keeping those allocations there ensures that all builtin functions
/// created here retain the original prototype identities.
pub(crate) fn create_function_intrinsics(
  globals: &Rc<RefCell<ObjectData>>,
  function_constructor_properties: &Rc<RefCell<ObjectData>>,
  function_prototype: &Rc<RefCell<ObjectData>>,
) -> FunctionIntrinsics {
  use PropertyKey as PKey;

  let function_ctor_globals = Rc::clone(globals);
  let function_ctor_fn: BuiltinFn = Rc::new(move |vm, args| {
    let off = if vm.pending_constructor_new_target().is_some() {
      1
    } else {
      0
    };
    let user_args = args.get(off..).unwrap_or(&[]);
    let body_src = if let Some(last) = user_args.last() {
      vm.to_string(last)?
    } else {
      js_string::JsString::empty()
    };
    let param_count = user_args.len().saturating_sub(1);
    let mut params = Vec::with_capacity(param_count);
    for argument in user_args.iter().take(param_count) {
      params.push(vm.to_string(argument)?);
    }
    let source = crate::js!(
      format(script),
      "(function __tsblitz_dynamic({}) {{\n{}\n}})",
      params
        .iter()
        .map(js_string::JsString::as_str_lossy)
        .collect::<Vec<_>>()
        .join(","),
      body_src
    );
    let code = crate::typecheck::with_strict_override(None, || {
      let session =
        Parser::parse_source(&source).map_err(|e| syntax_error(vm, format!("parse: {}", e)))?;
      compiler::compile_repl_session(&session)
        .map_err(|e| syntax_error(vm, format!("compile: {}", e)))
    })?;
    let mut child = Vm::new(Rc::clone(&function_ctor_globals));
    let result = child.run(code)?;
    set_builtin_name(&result, "anonymous");
    match &result {
      Value::Closure { props, .. } | Value::BuiltinObject(_, props) => {
        props.borrow_mut().set_internal_slot(
          "[[RealmGlobals]]",
          Value::Object(Rc::clone(&function_ctor_globals)),
        );
      }
      _ => {}
    }
    Ok(result)
  });
  let constructor =
    Value::BuiltinObject(function_ctor_fn, Rc::clone(function_constructor_properties));

  let function_call = builtin_object(Rc::new(function_call), function_prototype);
  function_prototype.borrow_mut().properties.insert(
    PKey::String("constructor".to_string().into()),
    PropertyDescriptor::builtin_data(constructor.clone()),
  );
  let has_instance = builtin_object(
    Rc::new(|vm, args| {
      let this = args.first().cloned().unwrap_or(Value::Undefined);
      let value = args.get(1).cloned().unwrap_or(Value::Undefined);
      Ok(Value::Bool(vm.ordinary_has_instance(&this, &value)?))
    }),
    function_prototype,
  );
  set_builtin_name(&has_instance, "[Symbol.hasInstance]");
  set_builtin_length(&has_instance, 1.0);
  function_prototype.borrow_mut().properties.insert(
    PKey::Symbol(SymbolId::well_known_has_instance()),
    PropertyDescriptor::immutable_data(has_instance),
  );
  function_prototype.borrow_mut().properties.insert(
    PKey::String("call".to_string().into()),
    PropertyDescriptor::builtin_data(function_call),
  );
  function_prototype.borrow_mut().properties.insert(
    PKey::String("apply".to_string().into()),
    PropertyDescriptor::builtin_data(builtin_object(Rc::new(function_apply), function_prototype)),
  );
  function_prototype.borrow_mut().properties.insert(
    PKey::String("bind".to_string().into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new({
        let function_prototype = Rc::clone(function_prototype);
        move |vm, args| {
          let target = args.first().cloned().unwrap_or(Value::Undefined);
          let bound_this = args.get(1).cloned().unwrap_or(Value::Undefined);
          let bound_args: Vec<Value> = args.iter().skip(2).cloned().collect();
          if !Vm::is_callable_value(&target) {
            return Err(vm.throw_type_error("Function.prototype.bind called on non-callable"));
          }
          if crate::trace::enabled() {
            eprintln!(
              "[trace] bind create target={:?} bound_this={:?} bound_args={:?}",
              target, bound_this, bound_args
            );
          }
          let bound_target = target.clone();
          let bound_this_arg = bound_this;
          let prefix_args = bound_args.clone();
          let bound_fn = builtin_object(
            Rc::new(move |vm, call_args| {
              if crate::trace::enabled() {
                eprintln!(
                  "[trace] bound fn call target={:?} this={:?} args={:?}",
                  bound_target, bound_this_arg, call_args
                );
              }
              let mut combined = prefix_args.clone();
              if vm.builtin_this_arg_prepended() {
                combined.extend(call_args.iter().skip(1).cloned());
              } else {
                combined.extend(call_args.iter().cloned());
              }
              vm.call_value_with_this(&bound_target, &bound_this_arg, &combined)
            }),
            &function_prototype,
          );
          let target_length =
            match crate::ecmascript::semantics::properties::own_property_descriptor(
              &target,
              &PKey::String("length".into()),
            ) {
              Some(PropertyDescriptor::Data { value, .. }) => value,
              Some(PropertyDescriptor::Accessor {
                get: Some(getter), ..
              }) => vm.call_value_with_this(&getter, &target, &[])?,
              Some(PropertyDescriptor::Accessor { get: None, .. }) => Value::Undefined,
              None => Value::Undefined,
            };
          let bound_length = match target_length {
            Value::Number(length) if length.is_finite() && length > 0.0 => {
              (length.floor() - bound_args.len() as f64).max(0.0)
            }
            Value::Number(length) if length.is_sign_positive() && length.is_infinite() => length,
            _ => 0.0,
          };
          let target_name = vm.get_property_host(&target, &PKey::String("name".into()))?;
          let target_name = match target_name {
            Value::String(name) => name.as_str_lossy().to_owned(),
            _ => String::new(),
          };
          set_builtin_length(&bound_fn, bound_length);
          set_builtin_name(&bound_fn, &format!("bound {target_name}"));
          if let Value::BuiltinObject(_, props) = &bound_fn {
            let mut props = props.borrow_mut();
            props.set_internal_slot("[[BoundTargetFunction]]", target);
            props.set_internal_slot(
              "[[BoundArguments]]",
              crate::runtime_utils::array_from_values(bound_args),
            );
          }
          Ok(bound_fn)
        }
      }),
      function_prototype,
    )),
  );

  function_prototype.borrow_mut().properties.insert(
    PKey::String("toString".to_string().into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new(function_to_string),
      function_prototype,
    )),
  );

  FunctionIntrinsics { constructor }
}
