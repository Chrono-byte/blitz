// SPDX-License-Identifier: MPL-2.0
//! Error constructors and prototype objects.

use std::cell::RefCell;
use std::rc::Rc;

use crate::ecmascript::builtins::collections::consume_collection_iterable;
use crate::runtime_utils::{
  array_from_values, builtin_object, set_builtin_length, set_builtin_name,
  static_builtin_arg_offset, strict_equal,
};
use crate::value::{BuiltinFn, ObjectData, PropertyDescriptor, PropertyKey, Value};

/// Constructors and prototype objects for the native Error family.
pub(crate) struct ErrorIntrinsics {
  pub(crate) error_constructor: Value,
  pub(crate) aggregate_error_constructor: Value,
  pub(crate) eval_error_constructor: Value,
  pub(crate) range_error_constructor: Value,
  pub(crate) reference_error_constructor: Value,
  pub(crate) syntax_error_constructor: Value,
  pub(crate) type_error_constructor: Value,
  pub(crate) uri_error_constructor: Value,
  pub(crate) error_prototype: Value,
}

fn string_key(value: &str) -> PropertyKey {
  PropertyKey::String(value.into())
}

fn prototype_value(prototype: &Rc<RefCell<ObjectData>>) -> Value {
  Value::Object(Rc::clone(prototype))
}

fn install_error_data(
  vm: &mut crate::Vm,
  instance: &mut ObjectData,
  message: Option<&Value>,
  options: Option<&Value>,
) -> Result<Option<js_string::JsString>, crate::RuntimeError> {
  let message = match message {
    Some(Value::Undefined) | None => None,
    Some(message) => {
      let message = vm.to_string(message)?;
      instance.properties.insert(
        string_key("message"),
        PropertyDescriptor::builtin_data(Value::String(message.clone())),
      );
      Some(message)
    }
  };
  if let Some(options) = options
    && options.is_object()
    && vm.has_property_host(options, &string_key("cause"))?
  {
    let cause = vm.get_property_host(options, &string_key("cause"))?;
    instance
      .properties
      .insert(string_key("cause"), PropertyDescriptor::builtin_data(cause));
  }
  Ok(message)
}

fn create_named_error_constructor(
  name: &'static str,
  prototype: &Rc<RefCell<ObjectData>>,
  function_prototype: &Rc<RefCell<ObjectData>>,
) -> Value {
  let constructor_properties = Rc::new(RefCell::new(ObjectData::with_prototype(Value::Object(
    Rc::clone(function_prototype),
  ))));
  constructor_properties
    .borrow_mut()
    .properties
    .insert(
      string_key("prototype"),
      PropertyDescriptor::builtin_prototype(prototype_value(prototype)),
    );
  let constructor_name = name.to_owned();
  let instance_name = name.to_owned();
  let constructor_function: BuiltinFn = Rc::new(move |vm, args| {
    let constructor = vm
      .current_new_target()
      .or_else(|| vm.get_global(&constructor_name))
      .unwrap_or(Value::Undefined);
    let prototype = vm
      .get_property_host(&constructor, &string_key("prototype"))
      .unwrap_or(Value::Null);
    let mut instance = ObjectData::new();
    instance.set_prototype_value(prototype);
    let offset = static_builtin_arg_offset(vm, args, name);
    let message = install_error_data(vm, &mut instance, args.get(offset), args.get(offset + 1))?;
    let stack = message.map_or_else(
      || instance_name.clone(),
      |message| format!("{instance_name}: {message}"),
    );
    instance.set_internal_slot("[[ErrorData]]", Value::String(stack.into()));
    Ok(Value::Object(Rc::new(RefCell::new(instance))))
  });
  let constructor = Value::BuiltinObject(constructor_function, Rc::clone(&constructor_properties));
  set_builtin_name(&constructor, name);
  set_builtin_length(&constructor, 1.0);
  prototype.borrow_mut().properties.insert(
    string_key("constructor"),
    PropertyDescriptor::builtin_data(constructor.clone()),
  );
  prototype.borrow_mut().properties.insert(
    string_key("name"),
    PropertyDescriptor::builtin_data(Value::String(name.into())),
  );
  prototype.borrow_mut().properties.insert(
    string_key("message"),
    PropertyDescriptor::builtin_data(Value::String("".into())),
  );
  constructor
}

/// Creates Error, AggregateError, and the native Error subclasses.
pub(crate) fn create_error_intrinsics(
  object_prototype: &Rc<RefCell<ObjectData>>,
  function_prototype: &Rc<RefCell<ObjectData>>,
) -> ErrorIntrinsics {
  let error_prototype_map = Rc::new(RefCell::new(ObjectData::with_prototype(Value::Object(
    Rc::clone(object_prototype),
  ))));
  let error_constructor_properties = Rc::new(RefCell::new(ObjectData::with_prototype(
    Value::Object(Rc::clone(function_prototype)),
  )));
  error_constructor_properties
    .borrow_mut()
    .properties
    .insert(
      string_key("prototype"),
      PropertyDescriptor::builtin_prototype(prototype_value(&error_prototype_map)),
    );
  let aggregate_error_prototype = Rc::new(RefCell::new(ObjectData::with_prototype(
    prototype_value(&error_prototype_map),
  )));
  let aggregate_error_constructor_properties = Rc::new(RefCell::new(ObjectData::with_prototype(
    Value::Object(Rc::clone(function_prototype)),
  )));
  aggregate_error_constructor_properties
    .borrow_mut()
    .properties
    .insert(
      string_key("prototype"),
      PropertyDescriptor::builtin_prototype(prototype_value(&aggregate_error_prototype)),
    );

  let error_constructor_function: BuiltinFn = Rc::new(move |vm, args| {
    let constructor = vm
      .current_new_target()
      .or_else(|| vm.get_global("Error"))
      .unwrap_or(Value::Undefined);
    let prototype = vm
      .get_property_host(&constructor, &string_key("prototype"))
      .unwrap_or(Value::Null);
    let mut instance = ObjectData::new();
    instance.set_prototype_value(prototype);
    let offset = static_builtin_arg_offset(vm, args, "Error");
    let message = install_error_data(vm, &mut instance, args.get(offset), args.get(offset + 1))?;
    let stack = message.map_or_else(|| "Error".to_owned(), |message| format!("Error: {message}"));
    instance.set_internal_slot("[[ErrorData]]", Value::String(stack.into()));
    Ok(Value::Object(Rc::new(RefCell::new(instance))))
  });
  let error_constructor = Value::BuiltinObject(
    error_constructor_function,
    Rc::clone(&error_constructor_properties),
  );
  set_builtin_name(&error_constructor, "Error");
  set_builtin_length(&error_constructor, 1.0);
  error_prototype_map
    .borrow_mut()
    .properties
    .insert(
      string_key("constructor"),
      PropertyDescriptor::builtin_data(error_constructor.clone()),
    );
  error_prototype_map
    .borrow_mut()
    .properties
    .insert(
      string_key("name"),
      PropertyDescriptor::builtin_data(Value::String("Error".into())),
    );
  error_prototype_map
    .borrow_mut()
    .properties
    .insert(
      string_key("message"),
      PropertyDescriptor::builtin_data(Value::String("".into())),
    );
  let stack_getter = builtin_object(
    Rc::new(|vm, args| {
      let receiver = args.first().cloned().unwrap_or(Value::Undefined);
      let Value::Object(object) = receiver else {
        if !receiver.is_object() {
          return Err(vm.throw_type_error("Error.prototype.stack getter requires an object"));
        }
        return Ok(Value::Undefined);
      };
      Ok(
        object
          .borrow()
          .internal_slot("[[ErrorData]]")
          .cloned()
          .unwrap_or(Value::Undefined),
      )
    }),
    function_prototype,
  );
  set_builtin_name(&stack_getter, "get stack");
  set_builtin_length(&stack_getter, 0.0);
  let stack_home = prototype_value(&error_prototype_map);
  let stack_setter = builtin_object(
    Rc::new(move |vm, args| {
      let receiver = args.first().cloned().unwrap_or(Value::Undefined);
      if !receiver.is_object() {
        return Err(vm.throw_type_error("Error.prototype.stack setter requires an object"));
      }
      let value = args.get(1).cloned().unwrap_or(Value::Undefined);
      if !matches!(value, Value::String(_)) {
        return Err(vm.throw_type_error("Error stack must be a string"));
      }
      if strict_equal(&receiver, &stack_home) {
        return Err(vm.throw_type_error("Cannot set stack on Error.prototype"));
      }
      let object_constructor = vm
        .get_global("Object")
        .unwrap_or(Value::Undefined);
      let get_descriptor = vm.get_property_host(
        &object_constructor,
        &PropertyKey::String("getOwnPropertyDescriptor".into()),
      )?;
      let current = vm.call_value_with_this(
        &get_descriptor,
        &object_constructor,
        &[receiver.clone(), Value::String("stack".into())],
      )?;
      if !matches!(current, Value::Undefined) {
        vm.set_property_host(&receiver, &Value::String("stack".into()), value)?;
        return Ok(Value::Undefined);
      }
      let mut descriptor = ObjectData::new();
      for (name, field) in [
        ("value", value),
        ("writable", Value::Bool(true)),
        ("enumerable", Value::Bool(true)),
        ("configurable", Value::Bool(true)),
      ] {
        descriptor
          .properties
          .insert(string_key(name), PropertyDescriptor::ordinary_data(field));
      }
      let descriptor = Value::Object(Rc::new(RefCell::new(descriptor)));
      if !vm.define_property_value(&receiver, &Value::String("stack".into()), &descriptor)? {
        return Err(vm.throw_type_error("Cannot define stack property"));
      }
      Ok(Value::Undefined)
    }),
    function_prototype,
  );
  set_builtin_name(&stack_setter, "set stack");
  set_builtin_length(&stack_setter, 1.0);
  error_prototype_map
    .borrow_mut()
    .properties
    .insert(
      string_key("stack"),
      PropertyDescriptor::Accessor {
        get: Some(stack_getter),
        set: Some(stack_setter),
        enumerable: false,
        configurable: true,
      },
    );

  let error_to_string = builtin_object(
    Rc::new(|vm, args| {
      let this = args.first().cloned().unwrap_or(Value::Undefined);
      if !this.is_object() {
        return Err(vm.throw_type_error("Error.prototype.toString requires an object"));
      }
      let name = vm.get_property_host(&this, &string_key("name"))?;
      let name = if matches!(name, Value::Undefined) {
        "Error".into()
      } else {
        vm.to_string(&name)?
      };
      let message = vm.get_property_host(&this, &string_key("message"))?;
      let message = if matches!(message, Value::Undefined) {
        "".into()
      } else {
        vm.to_string(&message)?
      };
      if name.is_empty() {
        return Ok(Value::String(message));
      }
      if message.is_empty() {
        return Ok(Value::String(name));
      }
      Ok(Value::String(name.concat(&": ".into()).concat(&message)))
    }),
    function_prototype,
  );
  set_builtin_name(&error_to_string, "toString");
  set_builtin_length(&error_to_string, 0.0);
  error_prototype_map
    .borrow_mut()
    .properties
    .insert(
      string_key("toString"),
      PropertyDescriptor::builtin_data(error_to_string),
    );

  let aggregate_error_constructor_function: BuiltinFn = Rc::new(move |vm, args| {
    let constructor = vm
      .current_new_target()
      .or_else(|| vm.get_global("AggregateError"))
      .unwrap_or(Value::Undefined);
    let prototype = vm
      .get_property_host(&constructor, &string_key("prototype"))
      .unwrap_or(Value::Null);
    let mut instance = ObjectData::new();
    instance.set_prototype_value(prototype);
    let offset = static_builtin_arg_offset(vm, args, "AggregateError");
    let message = install_error_data(
      vm,
      &mut instance,
      args.get(offset + 1),
      args.get(offset + 2),
    )?;
    let iterable = args
      .get(offset)
      .cloned()
      .unwrap_or(Value::Undefined);
    let mut errors = Vec::new();
    consume_collection_iterable(vm, &iterable, |_vm, error| {
      errors.push(error);
      Ok(())
    })?;
    instance.properties.insert(
      string_key("errors"),
      PropertyDescriptor::builtin_data(array_from_values(errors)),
    );
    let stack = message.map_or_else(
      || "AggregateError".to_owned(),
      |message| format!("AggregateError: {message}"),
    );
    instance.set_internal_slot("[[ErrorData]]", Value::String(stack.into()));
    Ok(Value::Object(Rc::new(RefCell::new(instance))))
  });
  let aggregate_error_constructor = Value::BuiltinObject(
    aggregate_error_constructor_function,
    Rc::clone(&aggregate_error_constructor_properties),
  );
  set_builtin_name(&aggregate_error_constructor, "AggregateError");
  set_builtin_length(&aggregate_error_constructor, 2.0);
  aggregate_error_prototype
    .borrow_mut()
    .properties
    .insert(
      string_key("constructor"),
      PropertyDescriptor::builtin_data(aggregate_error_constructor.clone()),
    );
  aggregate_error_prototype
    .borrow_mut()
    .properties
    .insert(
      string_key("name"),
      PropertyDescriptor::builtin_data(Value::String("AggregateError".into())),
    );
  aggregate_error_prototype
    .borrow_mut()
    .properties
    .insert(
      string_key("message"),
      PropertyDescriptor::builtin_data(Value::String("".into())),
    );

  let make_prototype = || {
    Rc::new(RefCell::new(ObjectData::with_prototype(prototype_value(
      &error_prototype_map,
    ))))
  };
  let eval_error_constructor =
    create_named_error_constructor("EvalError", &make_prototype(), function_prototype);
  let range_error_constructor =
    create_named_error_constructor("RangeError", &make_prototype(), function_prototype);
  let reference_error_constructor =
    create_named_error_constructor("ReferenceError", &make_prototype(), function_prototype);
  let syntax_error_constructor =
    create_named_error_constructor("SyntaxError", &make_prototype(), function_prototype);
  let type_error_constructor =
    create_named_error_constructor("TypeError", &make_prototype(), function_prototype);
  let uri_error_constructor =
    create_named_error_constructor("URIError", &make_prototype(), function_prototype);

  ErrorIntrinsics {
    error_constructor,
    aggregate_error_constructor,
    eval_error_constructor,
    range_error_constructor,
    reference_error_constructor,
    syntax_error_constructor,
    type_error_constructor,
    uri_error_constructor,
    error_prototype: prototype_value(&error_prototype_map),
  }
}
