use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Mutex;

use crate::runtime_utils::{
    atomics_binary_update, atomics_coerce_view_value, atomics_compare_exchange,
    atomics_exchange_current, atomics_load_current, atomics_make_wait_result,
    atomics_make_waiter_record, atomics_normalized_current, atomics_register_waiter,
    atomics_store_current, atomics_view_index, atomics_view_info, atomics_waiters_array,
    atomics_wake_waiters, builtin_object, set_builtin_length, set_builtin_name, usize_to_f64,
};
use crate::value::{BuiltinFn, ObjectData, PropertyDescriptor, PropertyKey, Value};
use num_traits::ToPrimitive;

// Pure algorithmic helpers for Atomics. These were previously defined in
// runtime_utils/atomics.rs but are language semantics and belong with the
// other ECMAScript algorithms. Keep them minimal and host-independent.
pub(crate) fn atomics_value_kind(
    kind_name: &str,
) -> Option<crate::runtime_utils::AtomicsValueKind> {
    use crate::runtime_utils::AtomicsValueKind;
    match kind_name {
        "Int8Array" => Some(AtomicsValueKind::Number {
            bits: 8,
            signed: true,
        }),
        "Uint8Array" => Some(AtomicsValueKind::Number {
            bits: 8,
            signed: false,
        }),
        "Int16Array" => Some(AtomicsValueKind::Number {
            bits: 16,
            signed: true,
        }),
        "Uint16Array" => Some(AtomicsValueKind::Number {
            bits: 16,
            signed: false,
        }),
        "Int32Array" => Some(AtomicsValueKind::Number {
            bits: 32,
            signed: true,
        }),
        "Uint32Array" => Some(AtomicsValueKind::Number {
            bits: 32,
            signed: false,
        }),
        "BigInt64Array" => Some(AtomicsValueKind::BigInt { signed: true }),
        "BigUint64Array" => Some(AtomicsValueKind::BigInt { signed: false }),
        _ => None,
    }
}

pub(crate) fn atomics_wait_kind(kind_name: &str) -> bool {
    matches!(kind_name, "Int32Array" | "BigInt64Array")
}

pub(crate) fn atomics_normalize_integer(value: i128, bits: u32, signed: bool) -> i128 {
    let modulus = 1i128 << bits;
    let mut v = value % modulus;
    if v < 0 {
        v += modulus;
    }
    if signed {
        let sign_bit = 1i128 << (bits - 1);
        if v >= sign_bit {
            v -= modulus;
        }
    }
    v
}

pub(crate) fn atomics_normalize_bigint(
    value: num_bigint::BigInt,
    signed: bool,
) -> num_bigint::BigInt {
    use num_bigint::{BigInt as BigIntValue, Sign};
    let modulus = BigIntValue::from(1u8) << 64u32;
    let half = BigIntValue::from(1u8) << 63u32;
    let mut v = value % &modulus;
    if v.sign() == Sign::Minus {
        v += &modulus;
    }
    if signed && v >= half {
        v -= &modulus;
    }
    v
}

pub(crate) fn build_atomics_object(
    object_proto_map: &Rc<RefCell<ObjectData>>,
    function_proto_map: &Rc<RefCell<ObjectData>>,
) -> Value {
    let atomics_props: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(ObjectData::with_prototype(
        Value::Object(Rc::clone(object_proto_map)),
    )));
    let insert_atomics_method = |name: &str, length: f64, f: BuiltinFn| {
        let func_val = builtin_object(f, function_proto_map);
        set_builtin_name(&func_val, name);
        set_builtin_length(&func_val, length);
        atomics_props.borrow_mut().properties.insert(
            PropertyKey::String(name.to_string()),
            PropertyDescriptor::data(func_val),
        );
    };
    insert_atomics_method(
        "load",
        2.0,
        Rc::new(|vm, args| {
            let receiver = args.first().cloned().unwrap_or(Value::Undefined);
            let info = atomics_view_info(vm, &receiver)?;
            let index = atomics_view_index(vm, &info, args.get(1).unwrap_or(&Value::Undefined))?;
            atomics_load_current(vm, &info, index)
        }),
    );
    insert_atomics_method(
        "store",
        3.0,
        Rc::new(|vm, args| {
            let receiver = args.first().cloned().unwrap_or(Value::Undefined);
            let info = atomics_view_info(vm, &receiver)?;
            let index = atomics_view_index(vm, &info, args.get(1).unwrap_or(&Value::Undefined))?;
            let value = args.get(2).cloned().unwrap_or(Value::Undefined);
            atomics_store_current(vm, &info, index, &value)
        }),
    );
    for (name, op) in [
        ("add", "add"),
        ("sub", "sub"),
        ("and", "and"),
        ("or", "or"),
        ("xor", "xor"),
    ] {
        insert_atomics_method(
            name,
            3.0,
            Rc::new(move |vm, args| {
                let receiver = args.first().cloned().unwrap_or(Value::Undefined);
                let info = atomics_view_info(vm, &receiver)?;
                let index =
                    atomics_view_index(vm, &info, args.get(1).unwrap_or(&Value::Undefined))?;
                let value = args.get(2).cloned().unwrap_or(Value::Undefined);
                atomics_binary_update(vm, &info, index, &value, op)
            }),
        );
    }
    insert_atomics_method(
        "exchange",
        3.0,
        Rc::new(|vm, args| {
            let receiver = args.first().cloned().unwrap_or(Value::Undefined);
            let info = atomics_view_info(vm, &receiver)?;
            let index = atomics_view_index(vm, &info, args.get(1).unwrap_or(&Value::Undefined))?;
            let value = args.get(2).cloned().unwrap_or(Value::Undefined);
            atomics_exchange_current(vm, &info, index, &value)
        }),
    );
    insert_atomics_method(
        "compareExchange",
        4.0,
        Rc::new(|vm, args| {
            let receiver = args.first().cloned().unwrap_or(Value::Undefined);
            let info = atomics_view_info(vm, &receiver)?;
            let index = atomics_view_index(vm, &info, args.get(1).unwrap_or(&Value::Undefined))?;
            let expected = args.get(2).cloned().unwrap_or(Value::Undefined);
            let replacement = args.get(3).cloned().unwrap_or(Value::Undefined);
            atomics_compare_exchange(vm, &info, index, &expected, &replacement)
        }),
    );
    insert_atomics_method(
        "isLockFree",
        1.0,
        Rc::new(|vm, args| {
            let size = args.get(1).cloned().unwrap_or(Value::Undefined);
            let n = match vm.to_number_value(&size) {
                Ok(n) if n.is_finite() && n >= 0.0 => n.trunc().to_usize().unwrap_or(0),
                _ => 0,
            };
            Ok(Value::Bool(matches!(n, 1 | 2 | 4 | 8)))
        }),
    );
    insert_atomics_method(
        "notify",
        3.0,
        Rc::new(|vm, args| {
            let receiver = args.first().cloned().unwrap_or(Value::Undefined);
            let info = atomics_view_info(vm, &receiver)?;
            if !info.is_shared {
                return Err(vm.throw_type_error(
                    "Atomics.notify requires a SharedArrayBuffer-backed typed array",
                ));
            }
            if !atomics_wait_kind(&info.kind_name) {
                return Err(
                    vm.throw_type_error("Atomics.notify requires an Int32Array or BigInt64Array")
                );
            }
            let index = atomics_view_index(vm, &info, args.get(1).unwrap_or(&Value::Undefined))?;
            let count = match args.get(2) {
                None | Some(Value::Undefined) => usize::MAX,
                Some(value) => {
                    let n = vm.to_number_value(value)?;
                    if !n.is_finite() {
                        usize::MAX
                    } else if n <= 0.0 {
                        0
                    } else {
                        n.trunc().to_usize().unwrap_or(usize::MAX)
                    }
                }
            };
            let woke = atomics_wake_waiters(vm, &info, index, count)?;
            Ok(Value::Number(usize_to_f64(woke)))
        }),
    );
    insert_atomics_method(
        "wait",
        4.0,
        Rc::new(|vm, args| {
            let receiver = args.first().cloned().unwrap_or(Value::Undefined);
            let info = atomics_view_info(vm, &receiver)?;
            if !info.is_shared {
                return Err(vm.throw_type_error(
                    "Atomics.wait requires a SharedArrayBuffer-backed typed array",
                ));
            }
            if !atomics_wait_kind(&info.kind_name) {
                return Err(
                    vm.throw_type_error("Atomics.wait requires an Int32Array or BigInt64Array")
                );
            }
            let index = atomics_view_index(vm, &info, args.get(1).unwrap_or(&Value::Undefined))?;
            let expected = args.get(2).cloned().unwrap_or(Value::Undefined);
            let timeout = match args.get(3) {
                None | Some(Value::Undefined) => f64::INFINITY,
                Some(value) => {
                    let n = vm.to_number_value(value)?;
                    if n.is_nan() || n < 0.0 {
                        0.0
                    } else {
                        n
                    }
                }
            };
            let current = atomics_normalized_current(vm, &info, index)?;
            let expected = atomics_coerce_view_value(vm, &info, &expected)?;
            if current != expected {
                return Ok(Value::String("not-equal".to_string()));
            }
            if timeout == 0.0 {
                return Ok(Value::String("timed-out".to_string()));
            }
            atomics_register_waiter(vm, &info, index, None)?;
            Ok(Value::String("ok".to_string()))
        }),
    );
    insert_atomics_method(
        "waitAsync",
        4.0,
        Rc::new(|vm, args| {
            let receiver = args.first().cloned().unwrap_or(Value::Undefined);
            let info = atomics_view_info(vm, &receiver)?;
            if !info.is_shared {
                return Err(vm.throw_type_error(
                    "Atomics.waitAsync requires a SharedArrayBuffer-backed typed array",
                ));
            }
            if !atomics_wait_kind(&info.kind_name) {
                return Err(vm.throw_type_error(
                    "Atomics.waitAsync requires an Int32Array or BigInt64Array",
                ));
            }
            let index = atomics_view_index(vm, &info, args.get(1).unwrap_or(&Value::Undefined))?;
            let expected = args.get(2).cloned().unwrap_or(Value::Undefined);
            let timeout = match args.get(3) {
                None | Some(Value::Undefined) => f64::INFINITY,
                Some(value) => {
                    let n = vm.to_number_value(value)?;
                    if n.is_nan() || n < 0.0 {
                        0.0
                    } else {
                        n
                    }
                }
            };
            let current = atomics_normalized_current(vm, &info, index)?;
            let expected = atomics_coerce_view_value(vm, &info, &expected)?;
            if current != expected {
                return Ok(atomics_make_wait_result(
                    false,
                    Value::String("not-equal".to_string()),
                ));
            }
            if timeout == 0.0 {
                return Ok(atomics_make_wait_result(
                    false,
                    Value::String("timed-out".to_string()),
                ));
            }

            let resolve_slot: Rc<Mutex<Option<Value>>> = Rc::new(Mutex::new(None));
            let resolve_slot_capture = Rc::clone(&resolve_slot);
            let promise_ctor = vm
                .get_global("Promise")
                .ok_or_else(|| vm.throw_type_error("Promise constructor missing"))?;
            let executor = Value::Builtin(Rc::new(move |_vm, executor_args| {
                let off = if executor_args.len() > 2 { 1 } else { 0 };
                *resolve_slot_capture.lock().unwrap() = executor_args.get(off).cloned();
                Ok(Value::Undefined)
            }));
            let promise = vm.construct_value(&promise_ctor, &[executor])?;
            let resolve_fn = resolve_slot
                .lock()
                .unwrap()
                .clone()
                .ok_or_else(|| vm.throw_type_error("Atomics.waitAsync resolve missing"))?;
            atomics_waiters_array(&info.buffer)
                .borrow_mut()
                .push(Some(atomics_make_waiter_record(index, Some(resolve_fn))));
            Ok(atomics_make_wait_result(true, promise))
        }),
    );
    Value::Object(atomics_props)
}
