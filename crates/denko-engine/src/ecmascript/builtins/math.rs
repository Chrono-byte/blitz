// SPDX-License-Identifier: MPL-2.0
//! Math object and its numeric operations.

use std::cell::RefCell;
use std::rc::Rc;

use crate::ecmascript::semantics::{
  builtin_object, number_exponentiate, set_builtin_length, set_builtin_name,
};
use crate::value::{BuiltinFn, ObjectData, PropertyDescriptor, PropertyKey, SymbolId, Value};

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

fn math_round(value: f64) -> f64 {
  if value.is_nan() || value.is_infinite() || value == 0.0 {
    return value;
  }
  if (-0.5..0.0).contains(&value) {
    return -0.0;
  }
  (value + 0.5).floor()
}

#[cfg(test)]
fn math_min(values: impl Iterator<Item = f64>) -> f64 {
  let mut result = f64::INFINITY;
  for value in values {
    if value.is_nan() {
      return f64::NAN;
    }
    if value < result || (value == 0.0 && result == 0.0 && value.is_sign_negative()) {
      result = value;
    }
  }
  result
}

#[cfg(test)]
fn math_max(values: impl Iterator<Item = f64>) -> f64 {
  let mut result = f64::NEG_INFINITY;
  for value in values {
    if value.is_nan() {
      return f64::NAN;
    }
    if value > result || (value == 0.0 && result == 0.0 && value.is_sign_positive()) {
      result = value;
    }
  }
  result
}

fn math_hypot(values: &[f64]) -> f64 {
  if values.iter().any(|value| value.is_infinite()) {
    return f64::INFINITY;
  }
  if values.iter().any(|value| value.is_nan()) {
    return f64::NAN;
  }
  let largest = values
    .iter()
    .map(|value| value.abs())
    .fold(0.0, f64::max);
  if largest == 0.0 {
    return 0.0;
  }
  largest
    * values
      .iter()
      .map(|value| (value / largest).powi(2))
      .sum::<f64>()
      .sqrt()
}

/// Creates the Math namespace object in the specified property order.
pub(crate) fn create_math_object(
  object_prototype: &Rc<RefCell<ObjectData>>,
  function_prototype: &Rc<RefCell<ObjectData>>,
) -> Rc<RefCell<ObjectData>> {
  let object = Rc::new(RefCell::new(ObjectData::with_prototype(Value::Object(
    Rc::clone(object_prototype),
  ))));
  object.borrow_mut().properties.insert(
    PropertyKey::Symbol(SymbolId::well_known_to_string_tag()),
    PropertyDescriptor::Data {
      value: Value::String("Math".into()),
      writable: false,
      enumerable: false,
      configurable: true,
    },
  );
  install(
    &object,
    function_prototype,
    "abs",
    1.0,
    Rc::new(|vm, args| {
      Ok(Value::Number(
        vm.to_number_value(args.get(1).unwrap_or(&Value::Undefined))?
          .abs(),
      ))
    }),
  );
  for (name, operation) in [
    ("acos", f64::acos as fn(f64) -> f64),
    ("acosh", f64::acosh),
    ("asin", f64::asin),
    ("asinh", f64::asinh),
    ("atan", f64::atan),
    ("atanh", f64::atanh),
    ("cbrt", f64::cbrt),
    ("ceil", f64::ceil),
    ("cos", f64::cos),
    ("cosh", f64::cosh),
    ("exp", f64::exp),
    ("expm1", f64::exp_m1),
    ("floor", f64::floor),
    ("log", f64::ln),
    ("log10", f64::log10),
    ("log1p", f64::ln_1p),
    ("log2", f64::log2),
    ("sin", f64::sin),
    ("sinh", f64::sinh),
    ("sqrt", f64::sqrt),
    ("tan", f64::tan),
    ("tanh", f64::tanh),
    ("trunc", f64::trunc),
  ] {
    install(
      &object,
      function_prototype,
      name,
      1.0,
      Rc::new(move |vm, args| {
        Ok(Value::Number(operation(vm.to_number_value(
          args.get(1).unwrap_or(&Value::Undefined),
        )?)))
      }),
    );
  }
  install(
    &object,
    function_prototype,
    "atan2",
    2.0,
    Rc::new(|vm, args| {
      let y = vm.to_number_value(args.get(1).unwrap_or(&Value::Undefined))?;
      let x = vm.to_number_value(args.get(2).unwrap_or(&Value::Undefined))?;
      Ok(Value::Number(y.atan2(x)))
    }),
  );
  install(
    &object,
    function_prototype,
    "clz32",
    1.0,
    Rc::new(|vm, args| {
      let value = vm.to_uint32_value(args.get(1).unwrap_or(&Value::Undefined))?;
      Ok(Value::Number(f64::from(value.leading_zeros())))
    }),
  );
  install(
    &object,
    function_prototype,
    "fround",
    1.0,
    Rc::new(|vm, args| {
      let value = vm.to_number_value(args.get(1).unwrap_or(&Value::Undefined))?;
      Ok(Value::Number(f64::from(value as f32)))
    }),
  );
  install(
    &object,
    function_prototype,
    "hypot",
    2.0,
    Rc::new(|vm, args| {
      let values = args
        .iter()
        .skip(1)
        .map(|value| vm.to_number_value(value))
        .collect::<Result<Vec<_>, _>>()?;
      Ok(Value::Number(math_hypot(&values)))
    }),
  );
  install(
    &object,
    function_prototype,
    "imul",
    2.0,
    Rc::new(|vm, args| {
      let left = vm.to_int32_value(args.get(1).unwrap_or(&Value::Undefined))?;
      let right = vm.to_int32_value(args.get(2).unwrap_or(&Value::Undefined))?;
      Ok(Value::Number(f64::from(left.wrapping_mul(right))))
    }),
  );
  install(
    &object,
    function_prototype,
    "max",
    2.0,
    Rc::new(|vm, args| {
      let mut result = f64::NEG_INFINITY;
      for argument in args.iter().skip(1) {
        let value = vm.to_number_value(argument)?;
        if value.is_nan() {
          return Ok(Value::Number(f64::NAN));
        }
        if value > result || (value == 0.0 && result == 0.0 && value.is_sign_positive()) {
          result = value;
        }
      }
      Ok(Value::Number(result))
    }),
  );
  install(
    &object,
    function_prototype,
    "min",
    2.0,
    Rc::new(|vm, args| {
      let mut result = f64::INFINITY;
      for argument in args.iter().skip(1) {
        let value = vm.to_number_value(argument)?;
        if value.is_nan() {
          return Ok(Value::Number(f64::NAN));
        }
        if value < result || (value == 0.0 && result == 0.0 && value.is_sign_negative()) {
          result = value;
        }
      }
      Ok(Value::Number(result))
    }),
  );
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
    "round",
    1.0,
    Rc::new(|vm, args| {
      Ok(Value::Number(math_round(vm.to_number_value(
        args.get(1).unwrap_or(&Value::Undefined),
      )?)))
    }),
  );
  install(
    &object,
    function_prototype,
    "sign",
    1.0,
    Rc::new(|vm, args| {
      let value = vm.to_number_value(args.get(1).unwrap_or(&Value::Undefined))?;
      Ok(Value::Number(if value.is_nan() || value == 0.0 {
        value
      } else if value.is_sign_negative() {
        -1.0
      } else {
        1.0
      }))
    }),
  );
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

#[cfg(test)]
mod tests {
  use super::{math_hypot, math_max, math_min, math_round};

  #[test]
  fn round_preserves_negative_zero() {
    assert!(math_round(-0.0).is_sign_negative());
    assert!(math_round(-0.1).is_sign_negative());
    assert_eq!(math_round(-0.5), 0.0);
  }

  #[test]
  fn min_and_max_select_the_specified_zero_sign() {
    assert!(math_min([0.0, -0.0].into_iter()).is_sign_negative());
    assert!(math_max([-0.0, 0.0].into_iter()).is_sign_positive());
  }

  #[test]
  fn hypot_scales_before_squaring() {
    assert_eq!(math_hypot(&[3.0, 4.0]), 5.0);
    assert!(math_hypot(&[f64::INFINITY, f64::NAN]).is_infinite());
  }
}
