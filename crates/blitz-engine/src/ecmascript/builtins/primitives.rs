//! Primitive wrapper builtins for Number and BigInt.

use std::cell::RefCell;
use std::rc::Rc;

use num_bigint::BigInt as BigIntValue;

use crate::runtime_utils::{builtin_object, js_number_to_string, non_enum_data, strict_equal};
use crate::value::{BuiltinFn, ObjectData, PropertyDescriptor, PropertyKey, Value};

/// Installs the Number and BigInt constructors and their prototype methods.
#[allow(clippy::too_many_lines)]
pub(crate) fn install_bigint_and_number_builtins(
  globals: &mut ObjectData,
  object_proto_map: &Rc<RefCell<ObjectData>>,
  function_proto_map: &Rc<RefCell<ObjectData>>,
) {
  use PropertyKey as PKey;

  let bigint_proto_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
    ObjectData::with_prototype(Value::Object(Rc::clone(object_proto_map))),
  ));
  let bigint_ctor_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(ObjectData::with_prototype(
    Value::Object(Rc::clone(function_proto_map)),
  )));
  bigint_ctor_map.borrow_mut().properties.insert(
    PKey::String("prototype".to_string()),
    PropertyDescriptor::data(Value::Object(Rc::clone(&bigint_proto_map))),
  );
  let bigint_proto_for_ctor = Rc::clone(&bigint_proto_map);
  let bigint_ctor_fn: BuiltinFn = Rc::new(move |vm, args| {
    let value_idx = if vm.current_new_target().is_some() || args.len() > 1 {
      1
    } else {
      0
    };
    let value = args
      .get(value_idx)
      .cloned()
      .unwrap_or(Value::BigInt(BigIntValue::from(0u8)));
    let bigint = match value {
      Value::BigInt(n) => n,
      Value::Bool(b) => BigIntValue::from(if b { 1 } else { 0 }),
      Value::Number(n) => {
        if !n.is_finite() || n.fract() != 0.0 {
          return Err(vm.throw_range_error("Cannot convert Number to BigInt"));
        }
        BigIntValue::from(n as i64)
      }
      Value::String(s) => s
        .parse::<BigIntValue>()
        .map_err(|_| vm.throw_type_error("Cannot convert string to BigInt"))?,
      Value::Object(obj) => {
        if let Some(PropertyDescriptor::Data { value, .. }) = obj
          .borrow()
          .properties
          .get(&PKey::String("[[PrimitiveValue]]".to_string()))
          && let Value::BigInt(n) = value
        {
          n.clone()
        } else {
          return Err(vm.throw_type_error("Cannot convert object to BigInt"));
        }
      }
      _ => return Err(vm.throw_type_error("Cannot convert value to BigInt")),
    };

    if vm.current_new_target().is_some() {
      let mut obj = ObjectData::new();
      obj.set_prototype_value(Value::Object(Rc::clone(&bigint_proto_for_ctor)));
      let obj_rc = Rc::new(RefCell::new(obj));
      obj_rc.borrow_mut().properties.insert(
        PKey::String("[[PrimitiveValue]]".to_string()),
        PropertyDescriptor::data(Value::BigInt(bigint)),
      );
      Ok(Value::Object(obj_rc))
    } else {
      Ok(Value::BigInt(bigint))
    }
  });
  let bigint_ctor_val = Value::BuiltinObject(bigint_ctor_fn, bigint_ctor_map);
  bigint_proto_map.borrow_mut().properties.insert(
    PKey::String("constructor".to_string()),
    PropertyDescriptor::data(bigint_ctor_val.clone()),
  );
  bigint_proto_map.borrow_mut().properties.insert(
    PKey::String("valueOf".to_string()),
    PropertyDescriptor::data(builtin_object(
      Rc::new(|vm, args| {
        let undefined = Value::Undefined;
        let this = args.first().unwrap_or(&undefined);
        match this {
          Value::BigInt(n) => Ok(Value::BigInt(n.clone())),
          Value::Object(obj) => {
            if let Some(PropertyDescriptor::Data { value, .. }) = obj
              .borrow()
              .properties
              .get(&PKey::String("[[PrimitiveValue]]".to_string()))
              && let Value::BigInt(n) = value
            {
              return Ok(Value::BigInt(n.clone()));
            }
            Err(vm.throw_type_error("BigInt.prototype.valueOf called on incompatible receiver"))
          }
          _ => Err(vm.throw_type_error("BigInt.prototype.valueOf called on incompatible receiver")),
        }
      }),
      function_proto_map,
    )),
  );
  bigint_proto_map.borrow_mut().properties.insert(
    PKey::String("toString".to_string()),
    PropertyDescriptor::data(builtin_object(
      Rc::new(|vm, args| {
        let undefined = Value::Undefined;
        let this = args.first().unwrap_or(&undefined);
        match this {
          Value::BigInt(n) => Ok(Value::String(n.to_string())),
          Value::Object(obj) => {
            if let Some(PropertyDescriptor::Data { value, .. }) = obj
              .borrow()
              .properties
              .get(&PKey::String("[[PrimitiveValue]]".to_string()))
              && let Value::BigInt(n) = value
            {
              return Ok(Value::String(n.to_string()));
            }
            Err(vm.throw_type_error("BigInt.prototype.toString called on incompatible receiver"))
          }
          _ => {
            Err(vm.throw_type_error("BigInt.prototype.toString called on incompatible receiver"))
          }
        }
      }),
      function_proto_map,
    )),
  );
  globals.properties.insert(
    PKey::String("BigInt".to_string()),
    PropertyDescriptor::data(bigint_ctor_val),
  );

  let number_proto_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
    ObjectData::with_prototype(Value::Object(Rc::clone(object_proto_map))),
  ));
  let number_ctor_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(ObjectData::with_prototype(
    Value::Object(Rc::clone(function_proto_map)),
  )));
  number_ctor_map.borrow_mut().properties.insert(
    PKey::String("prototype".to_string()),
    non_enum_data(Value::Object(Rc::clone(&number_proto_map))),
  );
  let number_proto_for_ctor = Rc::clone(&number_proto_map);
  let number_ctor_fn: BuiltinFn = Rc::new(move |vm, args| {
    if vm.current_new_target().is_some() {
      let value = args.get(1).cloned().unwrap_or(Value::Number(0.0));
      let n = vm.to_number_value(&value)?;
      let mut obj = ObjectData::new();
      obj.set_prototype_value(Value::Object(Rc::clone(&number_proto_for_ctor)));
      let obj_rc = Rc::new(RefCell::new(obj));
      obj_rc.borrow_mut().properties.insert(
        PKey::String("[[PrimitiveValue]]".to_string()),
        PropertyDescriptor::data(Value::Number(n)),
      );
      return Ok(Value::Object(obj_rc));
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
    PKey::String("constructor".to_string()),
    non_enum_data(number_ctor_val.clone()),
  );
  number_proto_map.borrow_mut().properties.insert(
    PKey::String("valueOf".to_string()),
    non_enum_data(builtin_object(
      Rc::new(|vm, args| {
        let undefined = Value::Undefined;
        let this = args.first().unwrap_or(&undefined);
        let res = match this {
          Value::Number(n) => Value::Number(*n),
          Value::Object(obj) => {
            if let Some(PropertyDescriptor::Data { value, .. }) = obj
              .borrow()
              .properties
              .get(&PKey::String("[[PrimitiveValue]]".to_string()))
            {
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
    PKey::String("toString".to_string()),
    non_enum_data(builtin_object(
      Rc::new(|vm, args| {
        let undefined = Value::Undefined;
        let this = args.first().unwrap_or(&undefined);
        match this {
          Value::Number(n) => Ok(Value::String(js_number_to_string(*n))),
          Value::Object(obj) => {
            if let Some(PropertyDescriptor::Data { value, .. }) = obj
              .borrow()
              .properties
              .get(&PKey::String("[[PrimitiveValue]]".to_string()))
              && let Value::Number(n) = value
            {
              return Ok(Value::String(js_number_to_string(*n)));
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
  number_ctor_map.borrow_mut().properties.insert(
    PKey::String("NaN".to_string()),
    PropertyDescriptor::Data {
      value: Value::Number(f64::NAN),
      writable: false,
      enumerable: false,
      configurable: false,
    },
  );
  number_ctor_map.borrow_mut().properties.insert(
    PKey::String("POSITIVE_INFINITY".to_string()),
    PropertyDescriptor::Data {
      value: Value::Number(f64::INFINITY),
      writable: false,
      enumerable: false,
      configurable: false,
    },
  );
  number_ctor_map.borrow_mut().properties.insert(
    PKey::String("NEGATIVE_INFINITY".to_string()),
    PropertyDescriptor::Data {
      value: Value::Number(f64::NEG_INFINITY),
      writable: false,
      enumerable: false,
      configurable: false,
    },
  );
  number_ctor_map.borrow_mut().properties.insert(
    PKey::String("MAX_VALUE".to_string()),
    PropertyDescriptor::Data {
      value: Value::Number(f64::MAX),
      writable: false,
      enumerable: false,
      configurable: false,
    },
  );
  number_ctor_map.borrow_mut().properties.insert(
    PKey::String("MIN_VALUE".to_string()),
    PropertyDescriptor::Data {
      value: Value::Number(f64::MIN_POSITIVE),
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
      PKey::String(name.to_string()),
      PropertyDescriptor::Data {
        value: Value::Number(value),
        writable: false,
        enumerable: false,
        configurable: false,
      },
    );
  }
  let number_predicates: [(&str, fn(f64) -> bool); 4] = [
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
      PKey::String(name.to_string()),
      non_enum_data(builtin_object(
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
    PKey::String("Number".to_string()),
    PropertyDescriptor::data(number_ctor_val),
  );
}
