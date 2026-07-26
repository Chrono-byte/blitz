// SPDX-License-Identifier: MPL-2.0
use super::*;
use crate::gc;

/// Creates a builtin object value with the given function and prototype chain.
pub(crate) fn builtin_object(
  f: BuiltinFn,
  function_proto: &Rc<RefCell<ObjectData>>,
) -> Value {
  let props = gc::track_object(Rc::new(RefCell::new(ObjectData::new())));
  props
    .borrow_mut()
    .set_prototype_value(Value::Object(Rc::clone(function_proto)));
  Value::BuiltinObject(f, props)
}

/// Sets the `length` property on a builtin function object.
pub(crate) fn set_builtin_length(
  target: &Value,
  length: f64,
) {
  let desc = PropertyDescriptor::builtin_metadata(Value::Number(length));
  match target {
    Value::Object(props)
    | Value::Array(_, props)
    | Value::Closure { props, .. }
    | Value::BuiltinObject(_, props) => {
      props
        .borrow_mut()
        .properties
        .insert(PropertyKey::String("length".to_string()), desc);
    }
    _ => {}
  }
}

/// Sets the `name` property on a builtin function object.
pub(crate) fn set_builtin_name(
  target: &Value,
  name: &str,
) {
  let desc = PropertyDescriptor::builtin_metadata(Value::String(name.to_string()));
  match target {
    Value::Object(props)
    | Value::Array(_, props)
    | Value::Closure { props, .. }
    | Value::BuiltinObject(_, props) => {
      props
        .borrow_mut()
        .properties
        .insert(PropertyKey::String("name".to_string()), desc);
    }
    _ => {}
  }
}
