// SPDX-License-Identifier: MPL-2.0
//! Error constructors and prototype objects.

use std::cell::RefCell;
use std::rc::Rc;

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
    if let Some(message) = args.get(1).cloned() {
      instance.properties.insert(
        string_key("message"),
        PropertyDescriptor::builtin_data(message),
      );
    }
    instance.properties.insert(
      string_key("name"),
      PropertyDescriptor::builtin_data(Value::String(instance_name.clone().into())),
    );
    Ok(Value::Object(Rc::new(RefCell::new(instance))))
  });
  let constructor = Value::BuiltinObject(constructor_function, Rc::clone(&constructor_properties));
  constructor_properties
    .borrow_mut()
    .properties
    .insert(
      string_key("name"),
      PropertyDescriptor::builtin_data(Value::String(name.into())),
    );
  prototype.borrow_mut().properties.insert(
    string_key("constructor"),
    PropertyDescriptor::builtin_data(constructor.clone()),
  );
  prototype.borrow_mut().properties.insert(
    string_key("name"),
    PropertyDescriptor::builtin_data(Value::String(name.into())),
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
    let message = args.get(1).cloned();
    if let Some(message) = message.clone() {
      instance.properties.insert(
        string_key("message"),
        PropertyDescriptor::builtin_data(message),
      );
    }
    let stack = message.map_or_else(
      || Ok("Error".to_owned()),
      |message| {
        vm.to_string(&message)
          .map(|message| format!("Error: {message}"))
      },
    )?;
    instance.properties.insert(
      string_key("stack"),
      PropertyDescriptor::builtin_data(Value::String(stack.into())),
    );
    instance.properties.insert(
      string_key("constructor"),
      PropertyDescriptor::builtin_data(constructor),
    );
    Ok(Value::Object(Rc::new(RefCell::new(instance))))
  });
  let error_constructor = Value::BuiltinObject(
    error_constructor_function,
    Rc::clone(&error_constructor_properties),
  );
  error_constructor_properties
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
    if let Some(message) = args.get(2).cloned() {
      instance.properties.insert(
        string_key("message"),
        PropertyDescriptor::builtin_data(message),
      );
    }
    instance.properties.insert(
      string_key("name"),
      PropertyDescriptor::builtin_data(Value::String("AggregateError".into())),
    );
    instance.properties.insert(
      string_key("constructor"),
      PropertyDescriptor::builtin_data(constructor),
    );
    Ok(Value::Object(Rc::new(RefCell::new(instance))))
  });
  let aggregate_error_constructor = Value::BuiltinObject(
    aggregate_error_constructor_function,
    Rc::clone(&aggregate_error_constructor_properties),
  );
  aggregate_error_constructor_properties
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
