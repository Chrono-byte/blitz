// SPDX-License-Identifier: MPL-2.0
//! Math object and its numeric operations.

use std::cell::RefCell;
use std::rc::Rc;

use crate::ecmascript::semantics::{
  builtin_object, number_exponentiate, set_builtin_length, set_builtin_name,
};
use crate::value::{BuiltinFn, ObjectData, PropertyDescriptor, PropertyKey, Value};

fn install(
  object: &Rc<RefCell<ObjectData>>,
  function_prototype: &Rc<RefCell<ObjectData>>,
  name: &str,
  length: f64,
  function: BuiltinFn,
) {
  let builtin = builtin_object(function, function_prototype);
  set_builtin_name(&builtin, name);
  set_builtin_length(&builtin, length);
  object.borrow_mut().properties.insert(
    PropertyKey::String(name.into()),
    PropertyDescriptor::builtin_data(builtin),
  );
}

/// Creates the Math namespace object in the specified property order.
pub(crate) fn create_math_object(
  function_prototype: &Rc<RefCell<ObjectData>>
) -> Rc<RefCell<ObjectData>> {
  let object = Rc::new(RefCell::new(ObjectData::new()));
  install(
    &object,
    function_prototype,
    "pow",
    2.0,
    Rc::new(|vm, args| {
      let x = vm.to_number_value(args.get(1).unwrap_or(&Value::Undefined))?;
      let y = vm.to_number_value(args.get(2).unwrap_or(&Value::Undefined))?;
      Ok(Value::Number(number_exponentiate(x, y)))
    }),
  );
  for (name, operation) in [
    ("abs", f64::abs as fn(f64) -> f64),
    ("floor", f64::floor),
    ("round", |value: f64| (value + 0.5).floor()),
  ] {
    install(
      &object,
      function_prototype,
      name,
      1.0,
      Rc::new(move |vm, args| {
        let value = vm.to_number_value(args.get(1).unwrap_or(&Value::Undefined))?;
        Ok(Value::Number(operation(value)))
      }),
    );
  }
  install(
    &object,
    function_prototype,
    "random",
    0.0,
    Rc::new(|_vm, _args| Ok(Value::Number(fastrand::f64()))),
  );
  install(
    &object,
    function_prototype,
    "imul",
    2.0,
    Rc::new(|vm, args| {
      let left = vm.to_number_value(args.get(1).unwrap_or(&Value::Undefined))? as i32;
      let right = vm.to_number_value(args.get(2).unwrap_or(&Value::Undefined))? as i32;
      Ok(Value::Number(f64::from(left.wrapping_mul(right))))
    }),
  );
  for (name, identity, operation) in [
    ("min", f64::INFINITY, f64::min as fn(f64, f64) -> f64),
    ("max", f64::NEG_INFINITY, f64::max),
  ] {
    install(
      &object,
      function_prototype,
      name,
      2.0,
      Rc::new(move |vm, args| {
        let mut result = identity;
        for value in args.iter().skip(1) {
          result = operation(result, vm.to_number_value(value)?);
        }
        Ok(Value::Number(result))
      }),
    );
  }
  for (name, value) in [
    ("E", std::f64::consts::E),
    ("LN10", std::f64::consts::LN_10),
    ("LN2", std::f64::consts::LN_2),
    ("LOG10E", std::f64::consts::LOG10_E),
    ("LOG2E", std::f64::consts::LOG2_E),
    ("PI", std::f64::consts::PI),
    ("SQRT1_2", std::f64::consts::FRAC_1_SQRT_2),
    ("SQRT2", std::f64::consts::SQRT_2),
  ] {
    object.borrow_mut().properties.insert(
      PropertyKey::String(name.into()),
      PropertyDescriptor::Data {
        value: Value::Number(value),
        writable: false,
        enumerable: false,
        configurable: false,
      },
    );
  }
  object
}
