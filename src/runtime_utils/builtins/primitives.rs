//! Primitive wrapper builtins currently migrated from runtime_utils.
//!
//! Number
//! BigInt
//! Boolean

use std::cell::RefCell;
use std::rc::Rc;

use num_bigint::BigInt as BigIntValue;

use crate::runtime_utils::builtin_object;
use crate::value::{BuiltinFn, ObjectData, PropertyDescriptor, PropertyKey, Value};

/// Fundamental primitive wrapper builtins currently migrated from runtime_utils.
pub(crate) fn install_bigint_and_number_builtins(
    globals: &mut ObjectData,
    object_proto_map: &Rc<RefCell<ObjectData>>,
    function_proto_map: &Rc<RefCell<ObjectData>>,
) {
    use PropertyKey as PKey;

    let bigint_proto_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
        ObjectData::with_prototype(Value::Object(Rc::clone(object_proto_map))),
    ));
    let bigint_ctor_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
        ObjectData::with_prototype(Value::Object(Rc::clone(function_proto_map))),
    ));
    bigint_ctor_map.borrow_mut().properties.insert(
        PKey::String("prototype".to_string()),
        PropertyDescriptor::data(Value::Object(Rc::clone(&bigint_proto_map))),
    );
    let bigint_proto_for_ctor = Rc::clone(&bigint_proto_map);
    let bigint_ctor_fn: BuiltinFn = Rc::new(move |vm, args| {
        let value_idx = if args.len() > 1 { 1 } else { 0 };
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
    let bigint_ctor_val = Value::BuiltinObject(bigint_ctor_fn, Rc::clone(&bigint_ctor_map));
    bigint_proto_map.borrow_mut().properties.insert(
        PKey::String("constructor".to_string()),
        PropertyDescriptor::data(bigint_ctor_val.clone()),
    );
    bigint_proto_map.borrow_mut().properties.insert(
        PKey::String("valueOf".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(|vm, args| {
                let this = args.first().cloned().unwrap_or(Value::Undefined);
                match this {
                    Value::BigInt(n) => Ok(Value::BigInt(n)),
                    Value::Object(obj) => {
                        if let Some(PropertyDescriptor::Data { value, .. }) = obj
                            .borrow()
                            .properties
                            .get(&PKey::String("[[PrimitiveValue]]".to_string()))
                            && let Value::BigInt(n) = value
                        {
                            return Ok(Value::BigInt(n.clone()));
                        }
                        Err(vm.throw_type_error(
                            "BigInt.prototype.valueOf called on incompatible receiver",
                        ))
                    }
                    _ => Err(vm.throw_type_error(
                        "BigInt.prototype.valueOf called on incompatible receiver",
                    )),
                }
            }),
            function_proto_map,
        )),
    );
    bigint_proto_map.borrow_mut().properties.insert(
        PKey::String("toString".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(|vm, args| {
                let this = args.first().cloned().unwrap_or(Value::Undefined);
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
                        Err(vm.throw_type_error(
                            "BigInt.prototype.toString called on incompatible receiver",
                        ))
                    }
                    _ => Err(vm.throw_type_error(
                        "BigInt.prototype.toString called on incompatible receiver",
                    )),
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
    let number_ctor_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
        ObjectData::with_prototype(Value::Object(Rc::clone(function_proto_map))),
    ));
    number_ctor_map.borrow_mut().properties.insert(
        PKey::String("prototype".to_string()),
        PropertyDescriptor::data(Value::Object(Rc::clone(&number_proto_map))),
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
        let value_idx = if args.len() > 1 { 1 } else { 0 };
        let value = args.get(value_idx).cloned().unwrap_or(Value::Number(0.0));
        let n = vm.to_number_value(&value)?;
        Ok(Value::Number(n))
    });
    let number_ctor_val = Value::BuiltinObject(number_ctor_fn, Rc::clone(&number_ctor_map));
    number_proto_map.borrow_mut().properties.insert(
        PKey::String("constructor".to_string()),
        PropertyDescriptor::data(number_ctor_val.clone()),
    );
    number_proto_map.borrow_mut().properties.insert(
        PKey::String("valueOf".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(|vm, args| {
                let this = args.first().cloned().unwrap_or(Value::Undefined);
                let res = match this {
                    Value::Number(n) => Value::Number(n),
                    Value::Object(obj) => {
                        if let Some(PropertyDescriptor::Data { value, .. }) = obj
                            .borrow()
                            .properties
                            .get(&PKey::String("[[PrimitiveValue]]".to_string()))
                        {
                            value.clone()
                        } else {
                            return Err(vm.throw_type_error(
                                "Number.prototype.valueOf called on incompatible receiver",
                            ));
                        }
                    }
                    _ => {
                        return Err(vm.throw_type_error(
                            "Number.prototype.valueOf called on incompatible receiver",
                        ));
                    }
                };
                if crate::trace::enabled() {
                    crate::trace::value_string(
                        "[trace] Number.prototype.valueOf receiver",
                        &args.first().cloned().unwrap_or(Value::Undefined),
                    );
                    crate::trace::value_string("[trace] Number.prototype.valueOf result", &res);
                }
                Ok(res)
            }),
            function_proto_map,
        )),
    );
    number_proto_map.borrow_mut().properties.insert(
        PKey::String("toString".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(|vm, args| {
                let this = args.first().cloned().unwrap_or(Value::Undefined);
                match this {
                    Value::Number(n) => Ok(Value::String(n.to_string())),
                    Value::Object(obj) => {
                        if let Some(PropertyDescriptor::Data { value, .. }) = obj
                            .borrow()
                            .properties
                            .get(&PKey::String("[[PrimitiveValue]]".to_string()))
                            && let Value::Number(n) = value
                        {
                            return Ok(Value::String(n.to_string()));
                        }
                        Err(vm.throw_type_error(
                            "Number.prototype.toString called on incompatible receiver",
                        ))
                    }
                    _ => Err(vm.throw_type_error(
                        "Number.prototype.toString called on incompatible receiver",
                    )),
                }
            }),
            function_proto_map,
        )),
    );
    number_ctor_map.borrow_mut().properties.insert(
        PKey::String("NaN".to_string()),
        PropertyDescriptor::data(Value::Number(f64::NAN)),
    );
    number_ctor_map.borrow_mut().properties.insert(
        PKey::String("POSITIVE_INFINITY".to_string()),
        PropertyDescriptor::data(Value::Number(f64::INFINITY)),
    );
    number_ctor_map.borrow_mut().properties.insert(
        PKey::String("NEGATIVE_INFINITY".to_string()),
        PropertyDescriptor::data(Value::Number(f64::NEG_INFINITY)),
    );
    number_ctor_map.borrow_mut().properties.insert(
        PKey::String("MAX_VALUE".to_string()),
        PropertyDescriptor::data(Value::Number(f64::MAX)),
    );
    number_ctor_map.borrow_mut().properties.insert(
        PKey::String("MIN_VALUE".to_string()),
        PropertyDescriptor::data(Value::Number(f64::MIN_POSITIVE)),
    );
    number_ctor_map.borrow_mut().properties.insert(
        PKey::String("isNaN".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(|_vm, args| {
                let value_idx = if args.len() > 1 { 1 } else { 0 };
                let is_nan = matches!(args.get(value_idx), Some(Value::Number(n)) if n.is_nan());
                Ok(Value::Bool(is_nan))
            }),
            function_proto_map,
        )),
    );
    globals.properties.insert(
        PKey::String("Number".to_string()),
        PropertyDescriptor::data(number_ctor_val),
    );
}
