// SPDX-License-Identifier: MPL-2.0
//! Primitive wrapper builtins for Number and BigInt.

use std::cell::RefCell;
use std::rc::Rc;

use num_bigint::BigInt as BigIntValue;
use num_traits::{FromPrimitive, ToPrimitive};

use crate::ecmascript::semantics::{set_builtin_length, set_builtin_name};
use crate::runtime_utils::{
  builtin_object, global_parse_float, global_parse_int, js_number_to_string,
  static_builtin_arg_offset, strict_equal, syntax_error,
};
use crate::value::{BuiltinFn, ObjectData, PropertyDescriptor, PropertyKey, Value};

fn this_number_value(
  vm: &mut crate::Vm,
  args: &[Value],
  method: &str,
) -> Result<f64, crate::RuntimeError> {
  let this = args.first().unwrap_or(&Value::Undefined);
  match this {
    Value::Number(value) => Ok(*value),
    Value::Object(object) => {
      if let Some(Value::Number(value)) = object.borrow().internal_slot("[[NumberData]]") {
        Ok(*value)
      } else {
        Err(vm.throw_type_error(&format!(
          "Number.prototype.{method} called on incompatible receiver"
        )))
      }
    }
    _ => Err(vm.throw_type_error(&format!(
      "Number.prototype.{method} called on incompatible receiver"
    ))),
  }
}

fn number_format_digits(
  vm: &mut crate::Vm,
  args: &[Value],
  default: usize,
  minimum: usize,
  method: &str,
) -> Result<usize, crate::RuntimeError> {
  let Some(value) = args.get(1) else {
    return Ok(default);
  };
  let value = vm.to_number_value(value)?;
  let digits = if value.is_nan() || value == 0.0 {
    0.0
  } else if value.is_infinite() {
    return Err(vm.throw_range_error(&format!("{method} digits argument must be finite")));
  } else {
    value.trunc()
  };
  if digits < minimum as f64 || digits > 100.0 {
    return Err(vm.throw_range_error(&format!(
      "{method} digits argument must be between {minimum} and 100"
    )));
  }
  Ok(digits as usize)
}

fn normalized_exponential(value: f64) -> String {
  let sign = if value.is_sign_negative() { "-" } else { "" };
  let source = js_number_to_string(value.abs());
  let (coefficient, explicit_exponent) = source
    .split_once('e')
    .map_or((source.as_str(), 0_i32), |(coefficient, exponent)| {
      (coefficient, exponent.parse::<i32>().unwrap_or(0))
    });
  let decimal = coefficient.find('.').unwrap_or(coefficient.len());
  let digits = coefficient.replace('.', "");
  let leading = digits.len() - digits.trim_start_matches('0').len();
  let significant = digits
    .trim_start_matches('0')
    .trim_end_matches('0');
  let exponent = explicit_exponent + decimal as i32 - leading as i32 - 1;
  if significant.len() == 1 {
    format!("{sign}{significant}e{exponent:+}")
  } else {
    format!(
      "{sign}{}.{}e{exponent:+}",
      &significant[..1],
      &significant[1..]
    )
  }
}

fn format_exponential(
  value: f64,
  fraction_digits: Option<usize>,
) -> String {
  if !value.is_finite() {
    return js_number_to_string(value);
  }
  if value == 0.0 {
    return match fraction_digits {
      Some(digits) if digits > 0 => format!("0.{}e+0", "0".repeat(digits)),
      _ => "0e+0".to_string(),
    };
  }
  let Some(fraction_digits) = fraction_digits else {
    return normalized_exponential(value);
  };
  let formatted = format!("{value:.fraction_digits$e}");
  let (coefficient, exponent) = formatted
    .split_once('e')
    .expect("Rust exponential formatting");
  let exponent = exponent.parse::<i32>().unwrap_or(0);
  format!("{coefficient}e{exponent:+}")
}

fn format_fixed(
  value: f64,
  fraction_digits: usize,
) -> String {
  let sign = if value.is_sign_negative() { "-" } else { "" };
  let scale = 10_f64.powi(fraction_digits as i32);
  let scaled = value.abs() * scale;
  if !scaled.is_finite() {
    return format!("{sign}{:.fraction_digits$}", value.abs());
  }
  let mut digits = format!("{:.0}", scaled.round());
  if fraction_digits == 0 {
    return format!("{sign}{digits}");
  }
  while digits.len() <= fraction_digits {
    digits.insert(0, '0');
  }
  let point = digits.len() - fraction_digits;
  format!("{sign}{}.{}", &digits[..point], &digits[point..])
}

fn install_number_method(
  prototype: &Rc<RefCell<ObjectData>>,
  function_prototype: &Rc<RefCell<ObjectData>>,
  name: &str,
  length: f64,
  function: BuiltinFn,
) {
  let builtin = builtin_object(function, function_prototype);
  set_builtin_name(&builtin, name);
  set_builtin_length(&builtin, length);
  prototype.borrow_mut().properties.insert(
    PropertyKey::String(name.into()),
    PropertyDescriptor::builtin_data(builtin),
  );
}

fn parse_bigint_string(source: &str) -> Option<BigIntValue> {
  let source = source.trim();
  if source.is_empty() {
    return Some(BigIntValue::from(0_u8));
  }
  let (negative, unsigned) = match source.as_bytes().first() {
    Some(b'+') => (false, &source[1..]),
    Some(b'-') => (true, &source[1..]),
    _ => (false, source),
  };
  let (radix, digits) = if let Some(digits) = unsigned
    .strip_prefix("0x")
    .or_else(|| unsigned.strip_prefix("0X"))
  {
    (16, digits)
  } else if let Some(digits) = unsigned
    .strip_prefix("0o")
    .or_else(|| unsigned.strip_prefix("0O"))
  {
    (8, digits)
  } else if let Some(digits) = unsigned
    .strip_prefix("0b")
    .or_else(|| unsigned.strip_prefix("0B"))
  {
    (2, digits)
  } else {
    (10, unsigned)
  };
  if digits.is_empty() {
    return None;
  }
  let value = BigIntValue::parse_bytes(digits.as_bytes(), radix)?;
  Some(if negative { -value } else { value })
}

pub(crate) fn to_bigint(
  vm: &mut crate::Vm,
  value: &Value,
) -> Result<BigIntValue, crate::RuntimeError> {
  match value {
    Value::BigInt(value) => Ok(value.clone()),
    Value::Bool(value) => Ok(BigIntValue::from(u8::from(*value))),
    Value::Number(value) if value.is_finite() && value.fract() == 0.0 => {
      BigIntValue::from_f64(*value)
        .ok_or_else(|| vm.throw_range_error("Cannot convert Number to BigInt"))
    }
    Value::Number(_) => Err(vm.throw_range_error("Cannot convert Number to BigInt")),
    Value::String(value) => parse_bigint_string(value.as_str_lossy())
      .ok_or_else(|| syntax_error(vm, "Cannot convert string to BigInt".to_string())),
    Value::Object(_)
    | Value::Array(..)
    | Value::Proxy(_)
    | Value::Closure { .. }
    | Value::BuiltinObject(_, _)
    | Value::Promise(..) => {
      let primitive = vm.to_primitive(value, Some("number"))?;
      to_bigint(vm, &primitive)
    }
    _ => Err(vm.throw_type_error("Cannot convert value to BigInt")),
  }
}

fn bigint_bits(
  vm: &mut crate::Vm,
  value: &Value,
) -> Result<usize, crate::RuntimeError> {
  let number = vm.to_number_value(value)?;
  if !number.is_finite()
    || number < 0.0
    || number.fract() != 0.0
    || number > 9_007_199_254_740_991.0
  {
    return Err(vm.throw_range_error("BigInt bit width is outside the valid index range"));
  }
  number
    .to_usize()
    .ok_or_else(|| vm.throw_range_error("BigInt bit width is not supported on this target"))
}

fn bigint_as_uint_n(
  bits: usize,
  value: BigIntValue,
) -> BigIntValue {
  if bits == 0 {
    return BigIntValue::from(0_u8);
  }
  let modulus = BigIntValue::from(1_u8) << bits;
  ((value % &modulus) + &modulus) % modulus
}

fn this_bigint_value(
  vm: &mut crate::Vm,
  args: &[Value],
  method: &str,
) -> Result<BigIntValue, crate::RuntimeError> {
  let this = args.first().unwrap_or(&Value::Undefined);
  match this {
    Value::BigInt(value) => Ok(value.clone()),
    Value::Object(object) => object
      .borrow()
      .properties
      .get(&PropertyKey::String("[[PrimitiveValue]]".into()))
      .and_then(|descriptor| match descriptor {
        PropertyDescriptor::Data {
          value: Value::BigInt(value),
          ..
        } => Some(value.clone()),
        _ => None,
      })
      .ok_or_else(|| {
        vm.throw_type_error(&format!(
          "BigInt.prototype.{method} called on incompatible receiver"
        ))
      }),
    _ => Err(vm.throw_type_error(&format!(
      "BigInt.prototype.{method} called on incompatible receiver"
    ))),
  }
}

pub(crate) struct NumericIntrinsics {
  pub(crate) bigint_constructor: Value,
  pub(crate) bigint_prototype: Value,
  pub(crate) number_constructor: Value,
  pub(crate) number_prototype: Value,
}

/// Installs the Number and BigInt constructors and their prototype methods.
#[allow(clippy::too_many_lines)]
pub(crate) fn install_bigint_and_number_builtins(
  globals: &mut ObjectData,
  object_proto_map: &Rc<RefCell<ObjectData>>,
  function_proto_map: &Rc<RefCell<ObjectData>>,
) -> NumericIntrinsics {
  use PropertyKey as PKey;

  let bigint_proto_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
    ObjectData::with_prototype(Value::Object(Rc::clone(object_proto_map))),
  ));
  let bigint_ctor_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(ObjectData::with_prototype(
    Value::Object(Rc::clone(function_proto_map)),
  )));
  bigint_ctor_map.borrow_mut().properties.insert(
    PKey::String("prototype".to_string().into()),
    PropertyDescriptor::builtin_prototype(Value::Object(Rc::clone(&bigint_proto_map))),
  );
  let bigint_ctor_fn: BuiltinFn = Rc::new(move |vm, args| {
    if vm.pending_constructor_new_target().is_some() {
      return Err(vm.throw_type_error("BigInt is not a constructor"));
    }
    let offset = static_builtin_arg_offset(vm, args, "BigInt");
    let value = args.get(offset).unwrap_or(&Value::Undefined);
    Ok(Value::BigInt(to_bigint(vm, value)?))
  });
  let bigint_ctor_val = Value::BuiltinObject(bigint_ctor_fn, Rc::clone(&bigint_ctor_map));
  set_builtin_name(&bigint_ctor_val, "BigInt");
  set_builtin_length(&bigint_ctor_val, 1.0);
  bigint_proto_map.borrow_mut().properties.insert(
    PKey::String("constructor".to_string().into()),
    PropertyDescriptor::builtin_data(bigint_ctor_val.clone()),
  );
  install_number_method(
    &bigint_proto_map,
    function_proto_map,
    "valueOf",
    0.0,
    Rc::new(|vm, args| Ok(Value::BigInt(this_bigint_value(vm, args, "valueOf")?))),
  );
  install_number_method(
    &bigint_proto_map,
    function_proto_map,
    "toString",
    0.0,
    Rc::new(|vm, args| {
      let value = this_bigint_value(vm, args, "toString")?;
      let radix = match args.get(1) {
        None | Some(Value::Undefined) => 10,
        Some(radix) => {
          let radix = vm.to_number_value(radix)?;
          if !radix.is_finite() || radix.trunc() < 2.0 || radix.trunc() > 36.0 {
            return Err(vm.throw_range_error("BigInt radix must be between 2 and 36"));
          }
          radix.trunc() as u32
        }
      };
      Ok(Value::String(value.to_str_radix(radix).into()))
    }),
  );
  install_number_method(
    &bigint_proto_map,
    function_proto_map,
    "toLocaleString",
    0.0,
    Rc::new(|vm, args| {
      Ok(Value::String(
        this_bigint_value(vm, args, "toLocaleString")?
          .to_string()
          .into(),
      ))
    }),
  );
  for (name, signed) in [("asUintN", false), ("asIntN", true)] {
    install_number_method(
      &bigint_ctor_map,
      function_proto_map,
      name,
      2.0,
      Rc::new(move |vm, args| {
        let offset = static_builtin_arg_offset(vm, args, "BigInt");
        let bits = bigint_bits(vm, args.get(offset).unwrap_or(&Value::Undefined))?;
        let value = to_bigint(vm, args.get(offset + 1).unwrap_or(&Value::Undefined))?;
        let unsigned = bigint_as_uint_n(bits, value);
        if !signed || bits == 0 {
          return Ok(Value::BigInt(unsigned));
        }
        let sign_bit = BigIntValue::from(1_u8) << (bits - 1);
        if unsigned >= sign_bit {
          Ok(Value::BigInt(unsigned - (BigIntValue::from(1_u8) << bits)))
        } else {
          Ok(Value::BigInt(unsigned))
        }
      }),
    );
  }
  globals.properties.insert(
    PKey::String("BigInt".to_string().into()),
    PropertyDescriptor::builtin_data(bigint_ctor_val.clone()),
  );

  let number_proto_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
    ObjectData::with_prototype(Value::Object(Rc::clone(object_proto_map))),
  ));
  number_proto_map
    .borrow_mut()
    .set_internal_slot("[[NumberData]]", Value::Number(0.0));
  let number_ctor_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(ObjectData::with_prototype(
    Value::Object(Rc::clone(function_proto_map)),
  )));
  number_ctor_map.borrow_mut().properties.insert(
    PKey::String("prototype".to_string().into()),
    PropertyDescriptor::builtin_data(Value::Object(Rc::clone(&number_proto_map))),
  );
  let number_proto_for_ctor = Rc::clone(&number_proto_map);
  let number_ctor_fn: BuiltinFn = Rc::new(move |vm, args| {
    if vm.pending_constructor_new_target().is_some() {
      let value = args.get(1).cloned().unwrap_or(Value::Number(0.0));
      let n = vm.to_number_value(&value)?;
      let mut obj = ObjectData::new();
      obj.set_prototype_value(Value::Object(Rc::clone(&number_proto_for_ctor)));
      obj.set_internal_slot("[[NumberData]]", Value::Number(n));
      return Ok(Value::Object(Rc::new(RefCell::new(obj))));
    }
    let value_idx = if !matches!(vm.this_value(), Value::Undefined)
      && args.len() == 1
      && args
        .first()
        .is_some_and(|first| strict_equal(first, &vm.this_value()))
    {
      usize::MAX
    } else if args.len() > 1
      && args
        .first()
        .is_some_and(|first| strict_equal(first, &vm.this_value()))
    {
      1
    } else {
      0
    };
    if value_idx == usize::MAX {
      return Ok(Value::Number(0.0));
    }
    let value = args
      .get(value_idx)
      .cloned()
      .unwrap_or(Value::Number(0.0));
    let n = vm.to_number_value(&value)?;
    Ok(Value::Number(n))
  });
  let number_ctor_val = Value::BuiltinObject(number_ctor_fn, Rc::clone(&number_ctor_map));
  number_proto_map.borrow_mut().properties.insert(
    PKey::String("constructor".to_string().into()),
    PropertyDescriptor::builtin_data(number_ctor_val.clone()),
  );
  number_proto_map.borrow_mut().properties.insert(
    PKey::String("valueOf".to_string().into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new(|vm, args| {
        let undefined = Value::Undefined;
        let this = args.first().unwrap_or(&undefined);
        let res = match this {
          Value::Number(n) => Value::Number(*n),
          Value::Object(obj) => {
            if let Some(value) = obj.borrow().internal_slot("[[NumberData]]") {
              value.clone()
            } else {
              return Err(
                vm.throw_type_error("Number.prototype.valueOf called on incompatible receiver"),
              );
            }
          }
          _ => {
            return Err(
              vm.throw_type_error("Number.prototype.valueOf called on incompatible receiver"),
            );
          }
        };
        if crate::trace::enabled() {
          crate::trace::value_string("[trace] Number.prototype.valueOf receiver", this);
          crate::trace::value_string("[trace] Number.prototype.valueOf result", &res);
        }
        Ok(res)
      }),
      function_proto_map,
    )),
  );
  number_proto_map.borrow_mut().properties.insert(
    PKey::String("toString".to_string().into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new(|vm, args| {
        let undefined = Value::Undefined;
        let this = args.first().unwrap_or(&undefined);
        match this {
          Value::Number(n) => Ok(Value::String(js_number_to_string(*n).into())),
          Value::Object(obj) => {
            if let Some(Value::Number(n)) = obj.borrow().internal_slot("[[NumberData]]") {
              return Ok(Value::String(js_number_to_string(*n).into()));
            }
            Err(vm.throw_type_error("Number.prototype.toString called on incompatible receiver"))
          }
          _ => {
            Err(vm.throw_type_error("Number.prototype.toString called on incompatible receiver"))
          }
        }
      }),
      function_proto_map,
    )),
  );
  install_number_method(
    &number_proto_map,
    function_proto_map,
    "toFixed",
    1.0,
    Rc::new(|vm, args| {
      let value = this_number_value(vm, args, "toFixed")?;
      let digits = number_format_digits(vm, args, 0, 0, "toFixed")?;
      if !value.is_finite() {
        return Ok(Value::String(js_number_to_string(value).into()));
      }
      if value.abs() >= 1e21 {
        return Ok(Value::String(js_number_to_string(value).into()));
      }
      let value = if value == 0.0 { 0.0 } else { value };
      Ok(Value::String(format_fixed(value, digits).into()))
    }),
  );
  install_number_method(
    &number_proto_map,
    function_proto_map,
    "toExponential",
    1.0,
    Rc::new(|vm, args| {
      let value = this_number_value(vm, args, "toExponential")?;
      let digits = match args.get(1) {
        None | Some(Value::Undefined) => None,
        Some(_) => Some(number_format_digits(vm, args, 0, 0, "toExponential")?),
      };
      Ok(Value::String(format_exponential(value, digits).into()))
    }),
  );
  install_number_method(
    &number_proto_map,
    function_proto_map,
    "toPrecision",
    1.0,
    Rc::new(|vm, args| {
      let value = this_number_value(vm, args, "toPrecision")?;
      let Some(precision) = (match args.get(1) {
        None | Some(Value::Undefined) => None,
        Some(_) => Some(number_format_digits(vm, args, 0, 1, "toPrecision")?),
      }) else {
        return Ok(Value::String(js_number_to_string(value).into()));
      };
      if !value.is_finite() {
        return Ok(Value::String(js_number_to_string(value).into()));
      }
      if value == 0.0 {
        let fractional = "0".repeat(precision.saturating_sub(1));
        let formatted = if fractional.is_empty() {
          "0".to_string()
        } else {
          format!("0.{fractional}")
        };
        return Ok(Value::String(formatted.into()));
      }
      let exponent = value.abs().log10().floor() as i32;
      let formatted = if exponent >= precision as i32 || exponent < -6 {
        format_exponential(value, Some(precision - 1))
      } else {
        let fractional = (precision as i32 - exponent - 1).max(0) as usize;
        format!("{value:.fractional$}")
      };
      Ok(Value::String(formatted.into()))
    }),
  );
  install_number_method(
    &number_proto_map,
    function_proto_map,
    "toLocaleString",
    0.0,
    Rc::new(|vm, args| {
      let value = this_number_value(vm, args, "toLocaleString")?;
      // Intl is not yet installed by the raw engine, so the locale-independent
      // fallback is the only observable formatting available here.
      Ok(Value::String(js_number_to_string(value).into()))
    }),
  );
  install_number_method(
    &number_ctor_map,
    function_proto_map,
    "parseInt",
    1.0,
    Rc::new(|vm, args| {
      let offset = static_builtin_arg_offset(vm, args, "Number.parseInt");
      global_parse_int(vm, &args[offset..])
    }),
  );
  install_number_method(
    &number_ctor_map,
    function_proto_map,
    "parseFloat",
    1.0,
    Rc::new(|vm, args| {
      let offset = static_builtin_arg_offset(vm, args, "Number.parseFloat");
      global_parse_float(vm, &args[offset..])
    }),
  );
  number_ctor_map.borrow_mut().properties.insert(
    PKey::String("NaN".to_string().into()),
    PropertyDescriptor::Data {
      value: Value::Number(f64::NAN),
      writable: false,
      enumerable: false,
      configurable: false,
    },
  );
  number_ctor_map.borrow_mut().properties.insert(
    PKey::String("POSITIVE_INFINITY".to_string().into()),
    PropertyDescriptor::Data {
      value: Value::Number(f64::INFINITY),
      writable: false,
      enumerable: false,
      configurable: false,
    },
  );
  number_ctor_map.borrow_mut().properties.insert(
    PKey::String("NEGATIVE_INFINITY".to_string().into()),
    PropertyDescriptor::Data {
      value: Value::Number(f64::NEG_INFINITY),
      writable: false,
      enumerable: false,
      configurable: false,
    },
  );
  number_ctor_map.borrow_mut().properties.insert(
    PKey::String("MAX_VALUE".to_string().into()),
    PropertyDescriptor::Data {
      value: Value::Number(f64::MAX),
      writable: false,
      enumerable: false,
      configurable: false,
    },
  );
  number_ctor_map.borrow_mut().properties.insert(
    PKey::String("MIN_VALUE".to_string().into()),
    PropertyDescriptor::Data {
      value: Value::Number(f64::MIN_POSITIVE),
      writable: false,
      enumerable: false,
      configurable: false,
    },
  );
  number_ctor_map.borrow_mut().properties.insert(
    PKey::String("EPSILON".to_string().into()),
    PropertyDescriptor::Data {
      value: Value::Number(f64::EPSILON),
      writable: false,
      enumerable: false,
      configurable: false,
    },
  );
  for (name, value) in [
    ("MAX_SAFE_INTEGER", 9_007_199_254_740_991.0),
    ("MIN_SAFE_INTEGER", -9_007_199_254_740_991.0),
  ] {
    number_ctor_map.borrow_mut().properties.insert(
      PKey::String(name.to_string().into()),
      PropertyDescriptor::Data {
        value: Value::Number(value),
        writable: false,
        enumerable: false,
        configurable: false,
      },
    );
  }
  type NumberPredicate = fn(f64) -> bool;
  let number_predicates: [(&str, NumberPredicate); 4] = [
    ("isNaN", f64::is_nan),
    ("isFinite", f64::is_finite),
    ("isInteger", |value: f64| {
      value.is_finite() && value.fract() == 0.0
    }),
    ("isSafeInteger", |value: f64| {
      value.is_finite() && value.fract() == 0.0 && value.abs() <= 9_007_199_254_740_991.0
    }),
  ];
  for (name, predicate) in number_predicates {
    number_ctor_map.borrow_mut().properties.insert(
      PKey::String(name.to_string().into()),
      PropertyDescriptor::builtin_data(builtin_object(
        Rc::new(move |_vm, args| {
          let value_idx = usize::from(args.len() > 1);
          let result =
            matches!(args.get(value_idx), Some(Value::Number(value)) if predicate(*value));
          Ok(Value::Bool(result))
        }),
        function_proto_map,
      )),
    );
  }
  globals.properties.insert(
    PKey::String("Number".to_string().into()),
    PropertyDescriptor::builtin_data(number_ctor_val.clone()),
  );

  NumericIntrinsics {
    bigint_constructor: bigint_ctor_val,
    bigint_prototype: Value::Object(bigint_proto_map),
    number_constructor: number_ctor_val,
    number_prototype: Value::Object(number_proto_map),
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::compiler;
  use crate::parser::Parser;
  use crate::runtime_utils;

  fn run(source: &str) -> crate::Vm {
    let session = Parser::parse_source(source).expect("parse numeric primitive test");
    let chunk = compiler::compile_session(&session).expect("compile numeric primitive test");
    let globals = Rc::new(RefCell::new(ObjectData::new()));
    globals.borrow_mut().properties.insert(
      PropertyKey::String("globalThis".into()),
      PropertyDescriptor::ordinary_data(Value::Object(Rc::clone(&globals))),
    );
    runtime_utils::install_minimal_kernel_constructors(&globals);
    let mut vm = crate::Vm::new(globals);
    vm.run(chunk).expect("run numeric primitive test");
    vm
  }

  #[test]
  fn bigint_constructor_width_and_radix_contracts() {
    let vm = run(crate::js!(
      r#"
        let constructThrows = false;
        let invalidThrows = false;
        try { new BigInt(1); } catch (error) { constructThrows = error instanceof TypeError; }
        try { BigInt("nope"); } catch (error) { invalidThrows = error instanceof SyntaxError; }
        globalThis.result = [
          BigInt("0xff").toString(16),
          BigInt.asUintN(8, -1n).toString(),
          BigInt.asIntN(8, 255n).toString(),
          BigInt(true).toString(),
          constructThrows,
          invalidThrows,
        ].join("|");
      "#
    ));
    assert_eq!(
      vm.get_global("result"),
      Some(Value::String("ff|255|-1|1|true|true".into()))
    );
  }
}
