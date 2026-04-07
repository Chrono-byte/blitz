use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use crate::runtime_utils::{
    dynamic_import_value, global_parse_float, global_parse_int, own_enumerable_keys,
    static_builtin_arg_offset, usize_to_f64,
};
use crate::value::{ObjectData, PropertyDescriptor, PropertyKey, Value};

pub(crate) fn install_kernel_helpers(
    globals: &mut ObjectData,
    _function_proto_map: &Rc<RefCell<ObjectData>>,
) {
    use PropertyKey as PKey;

    let mut insert_if_missing = |name: &str, val: Value| {
        let key = PKey::String(name.to_string());
        globals
            .properties
            .entry(key)
            .or_insert_with(|| PropertyDescriptor::data(val));
    };

    insert_if_missing(
        "__native_in",
        Value::Builtin(Rc::new(move |vm, args| {
            let off = static_builtin_arg_offset(vm, args, "");
            let key = args.get(off).cloned().unwrap_or(Value::Undefined);
            let target = args.get(off + 1).cloned().unwrap_or(Value::Undefined);
            if !target.is_object() {
                return Err(vm.throw_type_error(&format!(
                    "Cannot use 'in' operator to search for '{:?}' in {}",
                    key,
                    target.type_name()
                )));
            }
            let key = vm.to_property_key(&key)?;
            Ok(Value::Bool(vm.has_property_host(&target, &key)?))
        })),
    );

    insert_if_missing(
        "__native_construct",
        Value::Builtin(Rc::new(move |vm, args| {
            let off = static_builtin_arg_offset(vm, args, "");
            let target = args.get(off).cloned().unwrap_or(Value::Undefined);
            let ctor_args: Vec<Value> = args.iter().skip(off + 1).cloned().collect();
            vm.construct_value(&target, &ctor_args)
        })),
    );

    insert_if_missing(
        "__native_dynamic_import",
        Value::Builtin(Rc::new(move |vm, args| {
            let off = static_builtin_arg_offset(vm, args, "");
            let specifier = args.get(off).cloned().unwrap_or(Value::Undefined);
            let options = args.get(off + 1).cloned().unwrap_or(Value::Undefined);
            dynamic_import_value(vm, &[specifier, options])
        })),
    );

    insert_if_missing(
        "__native_toString",
        Value::Builtin(Rc::new(|vm, args| {
            let v = args.first().cloned().unwrap_or(Value::Undefined);
            let s = vm.to_string(&v)?;
            Ok(Value::String(s))
        })),
    );

    insert_if_missing(
        "__native_toNumber",
        Value::Builtin(Rc::new(|vm, args| {
            let v = args.first().cloned().unwrap_or(Value::Undefined);
            let n = vm.to_number_value(&v)?;
            Ok(Value::Number(n))
        })),
    );

    insert_if_missing(
        "isNaN",
        Value::Builtin(Rc::new(|vm, args| {
            let v = args.first().cloned().unwrap_or(Value::Undefined);
            let n = vm.to_number_value(&v)?;
            Ok(Value::Bool(n.is_nan()))
        })),
    );

    insert_if_missing(
        "isFinite",
        Value::Builtin(Rc::new(|vm, args| {
            let v = args.first().cloned().unwrap_or(Value::Undefined);
            let n = vm.to_number_value(&v)?;
            Ok(Value::Bool(n.is_finite()))
        })),
    );

    insert_if_missing("parseInt", Value::Builtin(Rc::new(global_parse_int)));

    insert_if_missing("parseFloat", Value::Builtin(Rc::new(global_parse_float)));

    insert_if_missing(
        "__native_toBoolean",
        Value::Builtin(Rc::new(|vm, args| {
            let v = args.first().cloned().unwrap_or(Value::Undefined);
            Ok(Value::Bool(vm.to_boolean(&v)))
        })),
    );

    insert_if_missing(
        "__native_abstractEquality",
        Value::Builtin(Rc::new(|vm, args| {
            let a = args.first().cloned().unwrap_or(Value::Undefined);
            let b = args.get(1).cloned().unwrap_or(Value::Undefined);
            Ok(Value::Bool(vm.abstract_equality(&a, &b)?))
        })),
    );

    insert_if_missing(
        "__native_inProperty",
        Value::Builtin(Rc::new(|vm, args| {
            let prop = args.first().cloned().unwrap_or(Value::Undefined);
            let obj = args.get(1).cloned().unwrap_or(Value::Undefined);
            Ok(Value::Bool(vm.in_property(&prop, &obj)?))
        })),
    );

    insert_if_missing(
        "__native_deleteProperty",
        Value::Builtin(Rc::new(|vm, args| {
            let (target, key) = if args.len() >= 3 {
                (
                    args.get(1).cloned().unwrap_or(Value::Undefined),
                    args.get(2).cloned().unwrap_or(Value::Undefined),
                )
            } else {
                (
                    args.first().cloned().unwrap_or(Value::Undefined),
                    args.get(1).cloned().unwrap_or(Value::Undefined),
                )
            };
            if crate::trace::enabled() {
                eprintln!(
                    "[trace] __native_deleteProperty target={:?} key={:?}",
                    target, key
                );
            }
            let key = vm.to_property_key(&key)?;
            Ok(Value::Bool(vm.delete_property_host(&target, &key)?))
        })),
    );

    insert_if_missing(
        "__native_forInKeys",
        Value::Builtin(Rc::new(|vm, args| {
            let current = args.first().cloned().unwrap_or(Value::Undefined);
            if matches!(current, Value::Undefined | Value::Null) {
                return Err(vm.throw_type_error("Cannot convert undefined or null to object"));
            }

            let mut out: Vec<Option<Value>> = Vec::new();
            let mut seen: HashSet<String> = HashSet::new();
            for key in own_enumerable_keys(&current) {
                if let PropertyKey::String(name) = key
                    && seen.insert(name.clone())
                {
                    out.push(Some(Value::String(name)));
                }
            }

            let arr_rc = Rc::new(RefCell::new(out));
            let props = Rc::new(RefCell::new(ObjectData::new()));
            props.borrow_mut().properties.insert(
                PropertyKey::String("length".to_string()),
                PropertyDescriptor::data(Value::Number(usize_to_f64(arr_rc.borrow().len()))),
            );
            Ok(Value::Array(arr_rc, props))
        })),
    );

    insert_if_missing(
        "__native_updateProperty",
        Value::Builtin(Rc::new(|vm, args| {
            let off = if args.len() >= 5 { 1 } else { 0 };
            let target = args.get(off).cloned().unwrap_or(Value::Undefined);
            let key_value = args.get(off + 1).cloned().unwrap_or(Value::Undefined);
            let delta =
                vm.to_number_value(&args.get(off + 2).cloned().unwrap_or(Value::Number(1.0)))?;
            let prefix = vm.to_boolean(&args.get(off + 3).cloned().unwrap_or(Value::Bool(true)));

            let key = vm.to_property_key(&key_value)?;
            let current = vm.get_property_host(&target, &key)?;
            let n = vm.to_number_value(&current)?;
            let updated = Value::Number(n + delta);
            let _ = vm.set_property_host(&target, &key_value, updated.clone())?;
            if prefix { Ok(updated) } else { Ok(current) }
        })),
    );

    insert_if_missing(
        "__native_compoundAssign",
        Value::Builtin(Rc::new(|vm, args| {
            let off = if args.len() >= 6 { 1 } else { 0 };
            let target = args.get(off).cloned().unwrap_or(Value::Undefined);
            let key_value = args.get(off + 1).cloned().unwrap_or(Value::Undefined);
            let rhs = args.get(off + 2).cloned().unwrap_or(Value::Undefined);
            let op = vm.to_string(
                &args
                    .get(off + 3)
                    .cloned()
                    .unwrap_or(Value::String("+".to_string())),
            )?;

            let key = vm.to_property_key(&key_value)?;
            let lhs = vm.get_property_host(&target, &key)?;

            let out = match op.as_str() {
                "+" => {
                    if matches!(lhs, Value::String(_))
                        || matches!(rhs, Value::String(_))
                        || matches!(lhs, Value::Symbol(_, _))
                        || matches!(rhs, Value::Symbol(_, _))
                    {
                        Value::String(format!("{}{}", vm.to_string(&lhs)?, vm.to_string(&rhs)?))
                    } else {
                        Value::Number(vm.to_number_value(&lhs)? + vm.to_number_value(&rhs)?)
                    }
                }
                "-" => Value::Number(vm.to_number_value(&lhs)? - vm.to_number_value(&rhs)?),
                "*" => Value::Number(vm.to_number_value(&lhs)? * vm.to_number_value(&rhs)?),
                "/" => Value::Number(vm.to_number_value(&lhs)? / vm.to_number_value(&rhs)?),
                "%" => Value::Number(vm.to_number_value(&lhs)? % vm.to_number_value(&rhs)?),
                "**" => Value::Number(vm.to_number_value(&lhs)?.powf(vm.to_number_value(&rhs)?)),
                "&" => {
                    let l = vm.to_int32_value(&lhs)?;
                    let r = vm.to_int32_value(&rhs)?;
                    Value::Number((l & r) as f64)
                }
                "|" => {
                    let l = vm.to_int32_value(&lhs)?;
                    let r = vm.to_int32_value(&rhs)?;
                    Value::Number((l | r) as f64)
                }
                "^" => {
                    let l = vm.to_int32_value(&lhs)?;
                    let r = vm.to_int32_value(&rhs)?;
                    Value::Number((l ^ r) as f64)
                }
                _ => Value::Undefined,
            };

            let _ = vm.set_property_host(&target, &key_value, out.clone())?;
            Ok(out)
        })),
    );
}
