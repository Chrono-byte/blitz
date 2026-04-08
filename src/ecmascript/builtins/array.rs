use std::cell::RefCell;
use std::env;
use std::rc::Rc;

use num_traits::ToPrimitive;

use crate::runtime_utils::{
    builtin_object, canonical_index_key, collect_iterable_values, set_builtin_length,
    static_builtin_arg_offset, strict_equal, to_length, u64_to_decimal_string, usize_to_f64,
};
use crate::value::{BuiltinFn, ObjectData, PropertyDescriptor, PropertyKey, SymbolId, Value};
use crate::vm::Vm;

/// 23.1 Array Objects
pub(crate) fn install_array_constructor_and_late_prototype_methods(
    array_ctor_map: &Rc<RefCell<ObjectData>>,
    array_proto_map: &Rc<RefCell<ObjectData>>,
    function_proto_map: &Rc<RefCell<ObjectData>>,
) -> Value {
    use PropertyKey as PKey;

    let array_ctor_props = Rc::clone(array_ctor_map);
    let array_ctor_fn: BuiltinFn = Rc::new(move |vm, args| {
        let off = static_builtin_arg_offset(vm, args, "Array");
        let elems: Vec<Option<Value>> = args.iter().skip(off).map(|v| Some(v.clone())).collect();
        let arr_rc = Rc::new(RefCell::new(elems));
        let props: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(ObjectData::new()));
        props.borrow_mut().properties.insert(
            PKey::String("length".to_string()),
            PropertyDescriptor::data(Value::Number(usize_to_f64(arr_rc.borrow().len()))),
        );
        if let Some(pval) = array_ctor_props
            .borrow()
            .properties
            .get(&PKey::String("prototype".to_string()))
            .and_then(|d| match d {
                PropertyDescriptor::Data { value, .. } => Some(value.clone()),
                _ => None,
            })
        {
            props.borrow_mut().set_prototype_value(pval);
        }
        Ok(Value::Array(arr_rc, props))
    });
    let array_ctor_val = Value::BuiltinObject(array_ctor_fn.clone(), Rc::clone(array_ctor_map));
    array_ctor_map.borrow_mut().properties.insert(
        PKey::String("isArray".to_string()),
        PropertyDescriptor::data({
            let builtin = builtin_object(
                Rc::new(|_vm, args| Ok(Value::Bool(matches!(args.get(1), Some(Value::Array(..)))))),
                function_proto_map,
            );
            set_builtin_length(&builtin, 1.0);
            builtin
        }),
    );
    array_ctor_map.borrow_mut().properties.insert(
        PKey::String("from".to_string()),
        PropertyDescriptor::data({
            let array_proto_for_from = Rc::clone(array_proto_map);
            let function_proto_map = Rc::clone(function_proto_map);
            let builtin = builtin_object(
                Rc::new(move |vm, args| {
                    let off = static_builtin_arg_offset(vm, args, "Array");
                    let undefined = Value::Undefined;
                    let source = args.get(off).unwrap_or(&undefined);
                    let map_fn = args.get(off + 1).unwrap_or(&undefined);
                    let has_map_fn = matches!(
                        map_fn,
                        Value::Builtin(_) | Value::BuiltinObject(_, _) | Value::Closure { .. }
                    );

                    let len = match source {
                        Value::Array(arr, _) => arr.borrow().len(),
                        Value::String(s) => s.chars().count(),
                        _ => {
                            let len_v =
                                vm.get_property_host(source, &PKey::String("length".to_string()))?;
                            to_length(vm, &len_v)?
                        }
                    };

                    let mut out: Vec<Option<Value>> = Vec::with_capacity(len);
                    for i in 0..len {
                        let mut v = match source {
                            Value::Array(arr, _) => arr
                                .borrow()
                                .get(i)
                                .cloned()
                                .flatten()
                                .unwrap_or(Value::Undefined),
                            Value::String(s) => {
                                let ch = s.chars().nth(i).unwrap_or('\0');
                                Value::String(ch.to_string())
                            }
                            _ => vm.get_property_host(source, &PKey::String(i.to_string()))?,
                        };
                        if has_map_fn {
                            v = vm.call_value_with_this(
                                map_fn,
                                &Value::Undefined,
                                &[v, Value::Number(usize_to_f64(i))],
                            )?;
                        }
                        out.push(Some(v));
                    }

                    let arr_rc = Rc::new(RefCell::new(out));
                    let props = Rc::new(RefCell::new(ObjectData::new()));
                    props
                        .borrow_mut()
                        .set_prototype_value(Value::Object(Rc::clone(&array_proto_for_from)));
                    props.borrow_mut().properties.insert(
                        PKey::String("length".to_string()),
                        PropertyDescriptor::data(Value::Number(usize_to_f64(
                            arr_rc.borrow().len(),
                        ))),
                    );
                    Ok(Value::Array(arr_rc, props))
                }),
                &function_proto_map,
            );
            set_builtin_length(&builtin, 1.0);
            builtin
        }),
    );
    array_ctor_map.borrow_mut().properties.insert(
        PKey::String("fromAsync".to_string()),
        PropertyDescriptor::data({
            let array_proto_for_from_async = Rc::clone(array_proto_map);
            let function_proto_map = Rc::clone(function_proto_map);
            let builtin = builtin_object(
                Rc::new(move |vm, args| {
                    let off = static_builtin_arg_offset(vm, args, "Array");
                    let undefined = Value::Undefined;
                    let source = args.get(off).unwrap_or(&undefined);
                    let map_fn = args.get(off + 1).unwrap_or(&undefined);
                    let this_arg = args.get(off + 2).unwrap_or(&undefined);
                    if crate::trace::enabled() {
                        eprintln!(
                            "[trace] Array.fromAsync start source={} map_fn={} this_arg={}",
                            source.type_name(),
                            map_fn.type_name(),
                            this_arg.type_name()
                        );
                    }

                    let promise_ctor = match vm.get_global("Promise") {
                        Some(value) => value,
                        None => return Err(vm.throw_type_error("Promise constructor missing")),
                    };
                    let promise_resolve = match vm
                        .get_property_host(&promise_ctor, &PKey::String("resolve".to_string()))
                    {
                        Ok(value) => value,
                        Err(err) => return Err(err),
                    };
                    let promise_reject = match vm
                        .get_property_host(&promise_ctor, &PKey::String("reject".to_string()))
                    {
                        Ok(value) => value,
                        Err(err) => return Err(err),
                    };
                    let constructor_candidate = vm.this_value();
                    let use_constructor = matches!(
                        constructor_candidate,
                        Value::Closure { ref proto, .. } if proto.is_constructor
                    );

                    let outcome: Result<Value, crate::errors::RuntimeError> = (|| {
                        if !matches!(map_fn, Value::Undefined) && !Vm::is_callable_value(map_fn) {
                            return Err(
                                vm.throw_type_error("Array.fromAsync mapfn is not callable")
                            );
                        }

                        let items = collect_iterable_values(vm, source)?;
                        if crate::trace::enabled() {
                            eprintln!("[trace] Array.fromAsync collected {} items", items.len());
                        }
                        let len = items.len();
                        let created = if use_constructor {
                            let ctor_len = if is_iterable_source(vm, source)? {
                                Vec::new()
                            } else {
                                vec![Value::Number(usize_to_f64(len))]
                            };
                            vm.construct_value(&constructor_candidate, &ctor_len)?
                        } else {
                            let out: Vec<Option<Value>> = vec![None; len];
                            let props = Rc::new(RefCell::new(ObjectData::new()));
                            props
                                .borrow_mut()
                                .set_prototype_value(Value::Object(Rc::clone(
                                    &array_proto_for_from_async,
                                )));
                            props.borrow_mut().properties.insert(
                                PKey::String("length".to_string()),
                                PropertyDescriptor::data(Value::Number(usize_to_f64(len))),
                            );
                            Value::Array(Rc::new(RefCell::new(out)), props)
                        };

                        for (i, mut value) in items.into_iter().enumerate() {
                            if !matches!(map_fn, Value::Undefined) {
                                value = vm.call_value_with_this(
                                    map_fn,
                                    this_arg,
                                    &[value, Value::Number(usize_to_f64(i))],
                                )?;
                            }
                            let key = Value::String(i.to_string());
                            let mut desc_data = ObjectData::new();
                            desc_data.properties.insert(
                                PKey::String("value".to_string()),
                                PropertyDescriptor::data(value),
                            );
                            desc_data.properties.insert(
                                PKey::String("writable".to_string()),
                                PropertyDescriptor::data(Value::Bool(true)),
                            );
                            desc_data.properties.insert(
                                PKey::String("enumerable".to_string()),
                                PropertyDescriptor::data(Value::Bool(true)),
                            );
                            desc_data.properties.insert(
                                PKey::String("configurable".to_string()),
                                PropertyDescriptor::data(Value::Bool(true)),
                            );
                            let desc = Value::Object(Rc::new(RefCell::new(desc_data)));
                            if !vm.define_property_value(&created, &key, &desc)? {
                                return Err(
                                    vm.throw_type_error("Array.fromAsync failed to define element")
                                );
                            }
                        }

                        let len_key = Value::String("length".to_string());
                        vm.set_property_host(&created, &len_key, Value::Number(usize_to_f64(len)))?;
                        Ok(created)
                    })(
                    );

                    match outcome {
                        Ok(value) => {
                            if crate::trace::enabled() {
                                eprintln!(
                                    "[trace] Array.fromAsync success value type={}",
                                    value.type_name()
                                );
                            }
                            vm.call_value_with_this(&promise_resolve, &promise_ctor, &[value])
                        }
                        Err(err) => {
                            if crate::trace::enabled() {
                                eprintln!("[trace] Array.fromAsync error {}", err);
                            }
                            let reason = vm
                                .last_uncaught_exception
                                .take()
                                .unwrap_or_else(|| Value::String(format!("{}", err)));
                            vm.call_value_with_this(&promise_reject, &promise_ctor, &[reason])
                        }
                    }
                }),
                &function_proto_map,
            );
            set_builtin_length(&builtin, 1.0);
            builtin
        }),
    );
    array_proto_map.borrow_mut().properties.insert(
        PKey::String("constructor".to_string()),
        PropertyDescriptor::data(array_ctor_val.clone()),
    );
    array_proto_map.borrow_mut().properties.insert(
        PKey::String("lastIndexOf".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(|vm, args| {
                let undefined = Value::Undefined;
                let receiver = args.first().unwrap_or(&undefined);
                let search = args.get(1).unwrap_or(&undefined);
                let length = vm.get_property_host(receiver, &PKey::String("length".to_string()))?;
                let len_num = vm.to_number_value(&length)?;
                if !len_num.is_finite() || len_num <= 0.0 {
                    return Ok(Value::Number(-1.0));
                }
                let len = len_num.floor().to_i64().unwrap_or(i64::MAX);
                let mut k = if let Some(from_index) = args.get(2) {
                    let n = vm.to_number_value(from_index)?;
                    if n.is_nan() {
                        len - 1
                    } else if n >= 0.0 {
                        n.floor().to_i64().unwrap_or(len - 1).min(len - 1)
                    } else {
                        let idx = len + n.floor().to_i64().unwrap_or(0);
                        if idx < 0 {
                            return Ok(Value::Number(-1.0));
                        }
                        idx
                    }
                } else {
                    len - 1
                };

                if let Value::Array(arr, _) = receiver {
                    let dense_len = arr.borrow().len().to_i64().unwrap_or(i64::MAX);
                    let mut idx = k.min(dense_len.saturating_sub(1));
                    while idx >= 0 {
                        if let Some(Some(element)) = arr.borrow().get(idx as usize)
                            && strict_equal(element, search)
                        {
                            return Ok(Value::Number(idx.to_f64().unwrap_or(f64::NAN)));
                        }
                        idx -= 1;
                    }
                }

                if len > 1_000_000 {
                    let mut candidates: Vec<u64> = Vec::new();
                    match &receiver {
                        Value::Object(obj) => {
                            let k_u64 = k.to_u64().unwrap_or(u64::MAX);
                            for key in obj.borrow().properties.keys() {
                                if let PKey::String(s) = key
                                    && let Some(idx) = canonical_index_key(s)
                                    && idx <= k_u64
                                {
                                    candidates.push(idx);
                                }
                            }
                        }
                        Value::Array(arr, props) => {
                            let k_u64 = k.to_u64().unwrap_or(u64::MAX);
                            let dense_len = arr.borrow().len().to_u64().unwrap_or(u64::MAX);
                            for idx in 0..dense_len.min(k_u64.saturating_add(1)) {
                                candidates.push(idx);
                            }
                            for key in props.borrow().properties.keys() {
                                if let PKey::String(s) = key
                                    && let Some(idx) = canonical_index_key(s)
                                    && idx <= k_u64
                                {
                                    candidates.push(idx);
                                }
                            }
                        }
                        Value::Closure { props, .. } | Value::BuiltinObject(_, props) => {
                            let k_u64 = k.to_u64().unwrap_or(u64::MAX);
                            for key in props.borrow().properties.keys() {
                                if let PKey::String(s) = key
                                    && let Some(idx) = canonical_index_key(s)
                                    && idx <= k_u64
                                {
                                    candidates.push(idx);
                                }
                            }
                        }
                        _ => {}
                    }

                    candidates.sort_unstable();
                    candidates.dedup();
                    for idx in candidates.into_iter().rev() {
                        let key = PKey::String(u64_to_decimal_string(idx));
                        let element = vm.get_property_host(receiver, &key)?;
                        if strict_equal(&element, search) {
                            return Ok(Value::Number(idx.to_f64().unwrap_or(f64::NAN)));
                        }
                    }
                    return Ok(Value::Number(-1.0));
                }

                while k >= 0 {
                    let key = PKey::String(u64_to_decimal_string(k as u64));
                    if vm.has_property_host(receiver, &key)? {
                        let element = vm.get_property_host(receiver, &key)?;
                        if env::var("TSBLITZ_TRACE").ok().as_deref() == Some("1") {
                            eprintln!(
                                "[trace] lastIndexOf k={} element={} search={}",
                                k, element, search
                            );
                        }
                        if strict_equal(&element, search) {
                            return Ok(Value::Number(k.to_f64().unwrap_or(f64::NAN)));
                        }
                    }
                    k -= 1;
                }
                Ok(Value::Number(-1.0))
            }),
            function_proto_map,
        )),
    );
    array_proto_map.borrow_mut().properties.insert(
        PKey::String("join".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(|vm, args| {
                let undefined = Value::Undefined;
                let receiver = args.first().unwrap_or(&undefined);
                let sep = match args.get(1) {
                    Some(Value::Undefined) | None => ",".to_string(),
                    Some(v) => vm.to_string(v)?,
                };
                let Value::Array(arr, _) = receiver else {
                    return Ok(Value::String(String::new()));
                };
                let mut out = String::new();
                for (idx, item) in arr.borrow().iter().enumerate() {
                    if idx > 0 {
                        out.push_str(&sep);
                    }
                    match item {
                        Some(Value::Undefined) | Some(Value::Null) | None => {}
                        Some(v) => out.push_str(&vm.to_string(v)?),
                    }
                }
                Ok(Value::String(out))
            }),
            function_proto_map,
        )),
    );

    array_ctor_val
}

fn is_iterable_source(vm: &mut Vm, source: &Value) -> Result<bool, crate::errors::RuntimeError> {
    match source {
        Value::Array(..) => Ok(true),
        other => {
            let iterator_method =
                vm.get_property_host(other, &PropertyKey::Symbol(SymbolId::well_known_iterator()))?;
            Ok(Vm::is_callable_value(&iterator_method))
        }
    }
}
