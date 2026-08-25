// SPDX-License-Identifier: MPL-2.0
use super::*;
use crate::gc;

/// Installs standard built-in properties on one intrinsic object.
///
/// Keeping descriptor attributes and function metadata here makes family
/// installers describe the intrinsic graph instead of repeating its plumbing.
pub(crate) struct IntrinsicInstaller<'a> {
  target: &'a Rc<RefCell<ObjectData>>,
  function_prototype: &'a Rc<RefCell<ObjectData>>,
}

impl<'a> IntrinsicInstaller<'a> {
  pub(crate) fn new(
    target: &'a Rc<RefCell<ObjectData>>,
    function_prototype: &'a Rc<RefCell<ObjectData>>,
  ) -> Self {
    Self {
      target,
      function_prototype,
    }
  }

  pub(crate) fn data(
    &self,
    key: PropertyKey,
    value: Value,
  ) {
    self
      .target
      .borrow_mut()
      .properties
      .insert(key, PropertyDescriptor::builtin_data(value));
  }

  pub(crate) fn method<F>(
    &self,
    key: &str,
    length: u8,
    implementation: F,
  ) -> Value
  where
    F: Fn(&mut Vm, &[Value]) -> Result<Value, RuntimeError> + 'static,
  {
    self.named_method(PropertyKey::String(key.into()), key, length, implementation)
  }

  pub(crate) fn named_method<F>(
    &self,
    key: PropertyKey,
    name: &str,
    length: u8,
    implementation: F,
  ) -> Value
  where
    F: Fn(&mut Vm, &[Value]) -> Result<Value, RuntimeError> + 'static,
  {
    let function = builtin_object(Rc::new(implementation), self.function_prototype);
    set_builtin_name(&function, name);
    set_builtin_length(&function, f64::from(length));
    self.data(key, function.clone());
    function
  }
}

/// Creates a builtin object value with the given function and prototype chain.
pub(crate) fn builtin_object(
  f: BuiltinFn,
  function_proto: &Rc<RefCell<ObjectData>>,
) -> Value {
  let props = gc::track_object(Rc::new(RefCell::new(ObjectData::with_property_capacity(2))));
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
        .insert(PropertyKey::builtin_length(), desc);
    }
    _ => {}
  }
}

/// Sets the `name` property on a builtin function object.
pub(crate) fn set_builtin_name(
  target: &Value,
  name: &str,
) {
  let desc = PropertyDescriptor::builtin_metadata(Value::String(name.to_string().into()));
  match target {
    Value::Object(props)
    | Value::Array(_, props)
    | Value::Closure { props, .. }
    | Value::BuiltinObject(_, props) => {
      props
        .borrow_mut()
        .properties
        .insert(PropertyKey::builtin_name(), desc);
    }
    _ => {}
  }
}
