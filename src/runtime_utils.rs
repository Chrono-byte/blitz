//! Utility functions for runtime operations.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::env;
use std::rc::Rc;

use crate::bytecode;
use crate::compiler;
use crate::parser::Parser;
use crate::value::SymbolId;
use crate::value::{BuiltinFn, ObjectData, PropertyDescriptor, PropertyKey, Value};

/// Clear slot type annotations recursively for a Chunk graph.
///
/// The soft prelude is intentionally dynamic and may pass values that don't
/// match static parameter annotations. Before executing the prelude we clear
/// slot types in every FunctionProto across the compiled chunk graph so the
/// VM won't enforce runtime type checks for those functions.
pub fn clear_slot_types_in_chunk(chunk: bytecode::Chunk) -> bytecode::Chunk {
    // Recursive helper that rebuilds chunks and their nested function chunks.
    fn rebuild(
        rc_chunk: &Rc<bytecode::Chunk>,
        visited: &mut HashMap<*const bytecode::Chunk, Rc<bytecode::Chunk>>,
    ) -> Rc<bytecode::Chunk> {
        let key = Rc::as_ptr(rc_chunk) as *const bytecode::Chunk;
        if let Some(existing) = visited.get(&key) {
            return Rc::clone(existing);
        }

        // Rebuild the chunk fields, replacing functions with cleared slot types.
        let mut new_chunk = bytecode::Chunk::new();
        new_chunk.code = rc_chunk.code.clone();
        new_chunk.constants = rc_chunk.constants.clone();
        new_chunk.lines = rc_chunk.lines.clone();
        // Slot/global type annotations removed in JS-first mode; nothing to do.

        // Process functions
        let mut new_funcs = Vec::new();
        for f in &rc_chunk.functions {
            // Rebuild nested chunk for this function
            let nested = rebuild(&f.chunk, visited);
            let mut nf = f.clone();
            // Replace all slot type annotations and return type with None
            nf.slot_types = nf.slot_types.iter().map(|_| None).collect();
            nf.return_type = None;
            nf.chunk = nested;
            new_funcs.push(nf);
        }
        new_chunk.functions = new_funcs;

        let rc_new = Rc::new(new_chunk);
        visited.insert(key, Rc::clone(&rc_new));
        rc_new
    }

    let rc = Rc::new(chunk);
    let mut visited: HashMap<*const bytecode::Chunk, Rc<bytecode::Chunk>> = HashMap::new();
    let rc_cleared = rebuild(&rc, &mut visited);
    Rc::try_unwrap(rc_cleared).unwrap_or_else(|rc| (*rc).clone())
}

fn strict_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Undefined, Value::Undefined) => true,
        (Value::Null, Value::Null) => true,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::String(x), Value::String(y)) => x == y,
        (Value::Number(x), Value::Number(y)) => x == y,
        (Value::BigInt(x), Value::BigInt(y)) => x == y,
        (Value::Symbol(x, _), Value::Symbol(y, _)) => x == y,
        (Value::Object(ax), Value::Object(bx)) => Rc::ptr_eq(ax, bx),
        (Value::Array(ax, _), Value::Array(bx, _)) => Rc::ptr_eq(ax, bx),
        (Value::Closure { props: ap, .. }, Value::Closure { props: bp, .. }) => Rc::ptr_eq(ap, bp),
        (Value::BuiltinObject(_, ap), Value::BuiltinObject(_, bp)) => Rc::ptr_eq(ap, bp),
        (Value::Builtin(af), Value::Builtin(bf)) => Rc::ptr_eq(af, bf),
        _ => false,
    }
}

fn static_builtin_arg_offset(vm: &crate::vm::Vm, args: &[Value], global_name: &str) -> usize {
    if let (Some(first), Some(global)) = (args.first(), vm.get_global(global_name))
        && strict_equal(first, &global)
    {
        return 1;
    }
    0
}

fn builtin_object(f: BuiltinFn, function_proto: &Rc<RefCell<ObjectData>>) -> Value {
    let props = Rc::new(RefCell::new(ObjectData::new()));
    props.borrow_mut().prototype = Value::Object(Rc::clone(function_proto));
    Value::BuiltinObject(f, props)
}

fn make_host_function_instance(function_proto: &Rc<RefCell<ObjectData>>) -> Value {
    let props = Rc::new(RefCell::new(ObjectData::new()));
    props.borrow_mut().prototype = Value::Object(Rc::clone(function_proto));
    let callable = Value::BuiltinObject(Rc::new(|_, _| Ok(Value::Undefined)), Rc::clone(&props));
    let proto_obj = Rc::new(RefCell::new(ObjectData::new()));
    props.borrow_mut().properties.insert(
        PropertyKey::String("prototype".to_string()),
        PropertyDescriptor::data(Value::Object(Rc::clone(&proto_obj))),
    );
    proto_obj.borrow_mut().properties.insert(
        PropertyKey::String("constructor".to_string()),
        PropertyDescriptor::data(callable.clone()),
    );
    callable
}

fn set_builtin_length(target: &Value, length: f64) {
    let desc = PropertyDescriptor::Data {
        value: Value::Number(length),
        writable: false,
        enumerable: false,
        configurable: true,
    };
    match target {
        Value::Object(obj) => {
            obj.borrow_mut()
                .properties
                .insert(PropertyKey::String("length".to_string()), desc);
        }
        Value::Array(_, props) | Value::Closure { props, .. } | Value::BuiltinObject(_, props) => {
            props
                .borrow_mut()
                .properties
                .insert(PropertyKey::String("length".to_string()), desc);
        }
        _ => {}
    }
}

fn own_property_descriptor(obj: &Value, key: &PropertyKey) -> Option<PropertyDescriptor> {
    match obj {
        Value::Object(data) => data.borrow().properties.get(key).cloned(),
        Value::Array(arr, props) => {
            if let PropertyKey::String(s) = key
                && let Some(idx) = canonical_index_key(s)
                && let Some(Some(value)) = arr.borrow().get(idx as usize).cloned()
            {
                return Some(PropertyDescriptor::Data {
                    value,
                    writable: true,
                    enumerable: true,
                    configurable: true,
                });
            }
            props.borrow().properties.get(key).cloned()
        }
        Value::Closure { props, .. } | Value::BuiltinObject(_, props) => {
            props.borrow().properties.get(key).cloned()
        }
        _ => None,
    }
}

fn own_enumerable_keys(obj: &Value) -> Vec<PropertyKey> {
    let mut keys = Vec::new();
    match obj {
        Value::Object(data) => {
            for (key, desc) in &data.borrow().properties {
                let enumerable = match desc {
                    PropertyDescriptor::Data { enumerable, .. }
                    | PropertyDescriptor::Accessor { enumerable, .. } => *enumerable,
                };
                if enumerable {
                    keys.push(key.clone());
                }
            }
        }
        Value::Array(arr, props) => {
            for (idx, slot) in arr.borrow().iter().enumerate() {
                if slot.is_some() {
                    keys.push(PropertyKey::String(idx.to_string()));
                }
            }
            for (key, desc) in &props.borrow().properties {
                let enumerable = match desc {
                    PropertyDescriptor::Data { enumerable, .. }
                    | PropertyDescriptor::Accessor { enumerable, .. } => *enumerable,
                };
                if enumerable {
                    keys.push(key.clone());
                }
            }
        }
        Value::Closure { props, .. } | Value::BuiltinObject(_, props) => {
            for (key, desc) in &props.borrow().properties {
                let enumerable = match desc {
                    PropertyDescriptor::Data { enumerable, .. }
                    | PropertyDescriptor::Accessor { enumerable, .. } => *enumerable,
                };
                if enumerable {
                    keys.push(key.clone());
                }
            }
        }
        _ => {}
    }
    keys
}

fn descriptor_value_from_descriptor(
    object_proto: &Rc<RefCell<ObjectData>>,
    desc: &PropertyDescriptor,
) -> Value {
    let mut obj = ObjectData::with_prototype(Value::Object(Rc::clone(object_proto)));
    match desc {
        PropertyDescriptor::Data {
            value,
            writable,
            enumerable,
            configurable,
        } => {
            obj.properties.insert(
                PropertyKey::String("value".to_string()),
                PropertyDescriptor::data(value.clone()),
            );
            obj.properties.insert(
                PropertyKey::String("writable".to_string()),
                PropertyDescriptor::data(Value::Bool(*writable)),
            );
            obj.properties.insert(
                PropertyKey::String("enumerable".to_string()),
                PropertyDescriptor::data(Value::Bool(*enumerable)),
            );
            obj.properties.insert(
                PropertyKey::String("configurable".to_string()),
                PropertyDescriptor::data(Value::Bool(*configurable)),
            );
        }
        PropertyDescriptor::Accessor {
            get,
            set,
            enumerable,
            configurable,
        } => {
            obj.properties.insert(
                PropertyKey::String("get".to_string()),
                PropertyDescriptor::data(get.clone().unwrap_or(Value::Undefined)),
            );
            obj.properties.insert(
                PropertyKey::String("set".to_string()),
                PropertyDescriptor::data(set.clone().unwrap_or(Value::Undefined)),
            );
            obj.properties.insert(
                PropertyKey::String("enumerable".to_string()),
                PropertyDescriptor::data(Value::Bool(*enumerable)),
            );
            obj.properties.insert(
                PropertyKey::String("configurable".to_string()),
                PropertyDescriptor::data(Value::Bool(*configurable)),
            );
        }
    }
    Value::Object(Rc::new(RefCell::new(obj)))
}

fn own_property_keys(obj: &Value) -> Vec<PropertyKey> {
    let mut keys = Vec::new();
    let mut seen = HashSet::new();
    match obj {
        Value::Object(data) => {
            for key in data.borrow().properties.keys() {
                if seen.insert(key.clone()) {
                    keys.push(key.clone());
                }
            }
        }
        Value::Array(arr, props) => {
            for (idx, slot) in arr.borrow().iter().enumerate() {
                if slot.is_some() {
                    let key = PropertyKey::String(idx.to_string());
                    if seen.insert(key.clone()) {
                        keys.push(key);
                    }
                }
            }
            for key in props.borrow().properties.keys() {
                if seen.insert(key.clone()) {
                    keys.push(key.clone());
                }
            }
        }
        Value::Closure { props, .. } | Value::BuiltinObject(_, props) => {
            for key in props.borrow().properties.keys() {
                if seen.insert(key.clone()) {
                    keys.push(key.clone());
                }
            }
        }
        _ => {}
    }
    keys
}

fn property_key_to_value(key: &PropertyKey) -> Value {
    match key {
        PropertyKey::String(name) => Value::String(name.clone()),
        PropertyKey::Symbol(id) => Value::Symbol(id.clone(), None),
    }
}

fn array_from_values(values: Vec<Value>) -> Value {
    let arr_rc = Rc::new(RefCell::new(
        values.into_iter().map(Some).collect::<Vec<_>>(),
    ));
    let props = Rc::new(RefCell::new(ObjectData::new()));
    props.borrow_mut().properties.insert(
        PropertyKey::String("length".to_string()),
        PropertyDescriptor::data(Value::Number(arr_rc.borrow().len() as f64)),
    );
    Value::Array(arr_rc, props)
}

fn canonical_index_key(s: &str) -> Option<u64> {
    if s.is_empty() {
        return None;
    }
    if s != "0" && s.starts_with('0') {
        return None;
    }
    let idx = s.parse::<u64>().ok()?;
    if idx.to_string() != s {
        return None;
    }
    Some(idx)
}

pub fn install_minimal_kernel_constructors(globals: &Rc<RefCell<ObjectData>>) {
    use PropertyKey as PKey;

    let object_proto_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(ObjectData::new()));
    let function_proto_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
        ObjectData::with_prototype(Value::Object(Rc::clone(&object_proto_map))),
    ));
    let function_ctor_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
        ObjectData::with_prototype(Value::Object(Rc::clone(&function_proto_map))),
    ));
    function_ctor_map.borrow_mut().properties.insert(
        PKey::String("prototype".to_string()),
        PropertyDescriptor::data(Value::Object(Rc::clone(&function_proto_map))),
    );

    let object_ctor_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
        ObjectData::with_prototype(Value::Object(Rc::clone(&function_proto_map))),
    ));
    object_ctor_map.borrow_mut().properties.insert(
        PKey::String("prototype".to_string()),
        PropertyDescriptor::data(Value::Object(Rc::clone(&object_proto_map))),
    );

    let array_proto_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
        ObjectData::with_prototype(Value::Object(Rc::clone(&object_proto_map))),
    ));
    let for_each_fn = builtin_object(
        Rc::new(|vm, args| {
            let this = args.first().cloned().unwrap_or(Value::Undefined);
            let callback = args.get(1).cloned().unwrap_or(Value::Undefined);
            let this_for_call = this.clone();
            if let Value::Array(arr, _) = &this {
                for (i, item_slot) in arr.borrow().iter().enumerate() {
                    if let Some(item) = item_slot {
                        let idx = Value::Number(i as f64);
                        let _ = vm.call_value_with_this(
                            &callback,
                            &this_for_call,
                            &[item.clone(), idx, this_for_call.clone()],
                        );
                    }
                }
            }
            Ok(Value::Undefined)
        }),
        &function_proto_map,
    );
    array_proto_map.borrow_mut().properties.insert(
        PKey::String("forEach".to_string()),
        PropertyDescriptor::data(for_each_fn),
    );
    let array_proto_for_map = Rc::clone(&array_proto_map);
    array_proto_map.borrow_mut().properties.insert(
        PKey::String("map".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(move |vm, args| {
                let receiver = args.first().cloned().unwrap_or(Value::Undefined);
                let callback = args.get(1).cloned().unwrap_or(Value::Undefined);
                let this_arg = args.get(2).cloned().unwrap_or(Value::Undefined);

                let len_val =
                    vm.get_property_host(&receiver, &PKey::String("length".to_string()))?;
                let len = vm.to_number_value(&len_val)?.max(0.0) as usize;
                let mut out: Vec<Option<Value>> = Vec::with_capacity(len);
                for i in 0..len {
                    let key = PKey::String(i.to_string());
                    // Check if property exists (hasOwnProperty check)
                    let has_prop = vm.has_property_host(&receiver, &key)?;
                    if has_prop {
                        let item = vm.get_property_host(&receiver, &key)?;
                        let mapped = vm.call_value_with_this(
                            &callback,
                            &this_arg,
                            &[item, Value::Number(i as f64), receiver.clone()],
                        )?;
                        out.push(Some(mapped));
                    } else {
                        // Preserve hole
                        out.push(None);
                    }
                }

                let arr_rc = Rc::new(RefCell::new(out));
                let props: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(ObjectData::new()));
                props.borrow_mut().prototype = Value::Object(Rc::clone(&array_proto_for_map));
                props.borrow_mut().properties.insert(
                    PKey::String("length".to_string()),
                    PropertyDescriptor::data(Value::Number(arr_rc.borrow().len() as f64)),
                );
                Ok(Value::Array(arr_rc, props))
            }),
            &function_proto_map,
        )),
    );
    array_proto_map.borrow_mut().properties.insert(
        PKey::String("join".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(|vm, args| {
                let this = args.first().cloned().unwrap_or(Value::Undefined);
                let sep = match args.get(1) {
                    Some(Value::Undefined) | None => ",".to_string(),
                    Some(v) => vm.to_string(v)?,
                };
                match this {
                    Value::Array(arr, _) => {
                        let mut out = String::new();
                        let mut first = true;
                        for item_slot in arr.borrow().iter() {
                            if !first {
                                out.push_str(&sep);
                            }
                            first = false;
                            if let Some(item) = item_slot {
                                match item {
                                    Value::Undefined | Value::Null => {}
                                    v => out.push_str(&vm.to_string(v)?),
                                }
                            }
                        }
                        Ok(Value::String(out))
                    }
                    _ => Ok(Value::String("".to_string())),
                }
            }),
            &function_proto_map,
        )),
    );
    array_proto_map.borrow_mut().properties.insert(
        PKey::String("push".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(|_vm, args| {
                let this = args.first().cloned().unwrap_or(Value::Undefined);
                let items = match this {
                    Value::Array(arr, _) => arr,
                    _ => return Ok(Value::Number(0.0)),
                };
                let mut vec = items.borrow_mut();
                for v in args.iter().skip(1).cloned() {
                    vec.push(Some(v));
                }
                Ok(Value::Number(vec.len() as f64))
            }),
            &function_proto_map,
        )),
    );
    let array_iterator_proto: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
        ObjectData::with_prototype(Value::Object(Rc::clone(&object_proto_map))),
    ));
    array_iterator_proto.borrow_mut().properties.insert(
        PKey::String("next".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(|_vm, _args| {
                let result = Rc::new(RefCell::new(ObjectData::new()));
                result.borrow_mut().properties.insert(
                    PKey::String("done".to_string()),
                    PropertyDescriptor::data(Value::Bool(true)),
                );
                result.borrow_mut().properties.insert(
                    PKey::String("value".to_string()),
                    PropertyDescriptor::data(Value::Undefined),
                );
                Ok(Value::Object(result))
            }),
            &function_proto_map,
        )),
    );
    array_proto_map.borrow_mut().properties.insert(
        PKey::Symbol(SymbolId::well_known_iterator()),
        PropertyDescriptor::data(builtin_object(
            Rc::new({
                let array_iterator_proto = Rc::clone(&array_iterator_proto);
                move |_vm, args| {
                    let this = args.first().cloned().unwrap_or(Value::Undefined);
                    let iter = Rc::new(RefCell::new(ObjectData::with_prototype(Value::Object(
                        Rc::clone(&array_iterator_proto),
                    ))));
                    iter.borrow_mut().properties.insert(
                        PKey::String("[[IteratedObject]]".to_string()),
                        PropertyDescriptor::data(this),
                    );
                    Ok(Value::Object(iter))
                }
            }),
            &function_proto_map,
        )),
    );
    // Array.prototype.filter
    let array_proto_for_filter = Rc::clone(&array_proto_map);
    array_proto_map.borrow_mut().properties.insert(
        PKey::String("filter".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(move |vm, args| {
                let receiver = args.first().cloned().unwrap_or(Value::Undefined);
                let callback = args.get(1).cloned().unwrap_or(Value::Undefined);
                let this_arg = args.get(2).cloned().unwrap_or(Value::Undefined);

                let len_val =
                    vm.get_property_host(&receiver, &PKey::String("length".to_string()))?;
                let len = vm.to_number_value(&len_val)?.max(0.0) as usize;
                let mut out: Vec<Option<Value>> = Vec::new();

                for i in 0..len {
                    let key = PKey::String(i.to_string());
                    let has_prop = vm.has_property_host(&receiver, &key)?;
                    if has_prop {
                        let item = vm.get_property_host(&receiver, &key)?;
                        let keep = vm.call_value_with_this(
                            &callback,
                            &this_arg,
                            &[item.clone(), Value::Number(i as f64), receiver.clone()],
                        )?;
                        if keep.is_truthy() {
                            out.push(Some(item));
                        }
                    }
                }

                let arr_rc = Rc::new(RefCell::new(out));
                let props: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(ObjectData::new()));
                props.borrow_mut().prototype = Value::Object(Rc::clone(&array_proto_for_filter));
                props.borrow_mut().properties.insert(
                    PKey::String("length".to_string()),
                    PropertyDescriptor::data(Value::Number(arr_rc.borrow().len() as f64)),
                );
                Ok(Value::Array(arr_rc, props))
            }),
            &function_proto_map,
        )),
    );
    // Array.prototype.find
    array_proto_map.borrow_mut().properties.insert(
        PKey::String("find".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(move |vm, args| {
                let receiver = args.first().cloned().unwrap_or(Value::Undefined);
                let callback = args.get(1).cloned().unwrap_or(Value::Undefined);
                let this_arg = args.get(2).cloned().unwrap_or(Value::Undefined);

                let len_val =
                    vm.get_property_host(&receiver, &PKey::String("length".to_string()))?;
                let len = vm.to_number_value(&len_val)?.max(0.0) as usize;

                for i in 0..len {
                    let key = PKey::String(i.to_string());
                    let has_prop = vm.has_property_host(&receiver, &key)?;
                    if has_prop {
                        let item = vm.get_property_host(&receiver, &key)?;
                        let found = vm.call_value_with_this(
                            &callback,
                            &this_arg,
                            &[item.clone(), Value::Number(i as f64), receiver.clone()],
                        )?;
                        if found.is_truthy() {
                            return Ok(item);
                        }
                    }
                }
                Ok(Value::Undefined)
            }),
            &function_proto_map,
        )),
    );
    // Array.prototype.reduce
    array_proto_map.borrow_mut().properties.insert(
        PKey::String("reduce".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(move |vm, args| {
                let receiver = args.first().cloned().unwrap_or(Value::Undefined);
                let callback = args.get(1).cloned().unwrap_or(Value::Undefined);

                let len_val =
                    vm.get_property_host(&receiver, &PKey::String("length".to_string()))?;
                let len = vm.to_number_value(&len_val)?.max(0.0) as usize;

                let mut i = 0;
                let mut accumulator = if args.len() > 2 {
                    args[2].clone()
                } else {
                    // Find first defined element
                    while i < len {
                        let key = PKey::String(i.to_string());
                        let has_prop = vm.has_property_host(&receiver, &key)?;
                        if has_prop {
                            return Ok(Value::Undefined); // Need initial value
                        }
                        i += 1;
                    }
                    return Ok(Value::Undefined);
                };

                while i < len {
                    let key = PKey::String(i.to_string());
                    let has_prop = vm.has_property_host(&receiver, &key)?;
                    if has_prop {
                        let item = vm.get_property_host(&receiver, &key)?;
                        accumulator = vm.call_value_with_this(
                            &callback,
                            &Value::Undefined,
                            &[accumulator, item, Value::Number(i as f64), receiver.clone()],
                        )?;
                    }
                    i += 1;
                }
                Ok(accumulator)
            }),
            &function_proto_map,
        )),
    );
    // Array.prototype.flat
    let array_proto_for_flat = Rc::clone(&array_proto_map);
    array_proto_map.borrow_mut().properties.insert(
        PKey::String("flat".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(move |vm, args| {
                let receiver = args.first().cloned().unwrap_or(Value::Undefined);
                let depth = if let Some(d) = args.get(1) {
                    if matches!(d, Value::Undefined) {
                        1
                    } else {
                        vm.to_number_value(d)?.max(0.0) as usize
                    }
                } else {
                    1
                };

                let len_val =
                    vm.get_property_host(&receiver, &PKey::String("length".to_string()))?;
                let len = vm.to_number_value(&len_val)?.max(0.0) as usize;
                let mut out: Vec<Option<Value>> = Vec::new();

                for i in 0..len {
                    let key = PKey::String(i.to_string());
                    let has_prop = vm.has_property_host(&receiver, &key)?;
                    if has_prop {
                        let item = vm.get_property_host(&receiver, &key)?;
                        if depth > 0 {
                            // Check if item is an array
                            if matches!(item, Value::Array(_, _)) {
                                let item_len_val = vm.get_property_host(
                                    &item,
                                    &PKey::String("length".to_string()),
                                )?;
                                let item_len = vm.to_number_value(&item_len_val)?.max(0.0) as usize;
                                for j in 0..item_len {
                                    let j_key = PKey::String(j.to_string());
                                    let has_j = vm.has_property_host(&item, &j_key)?;
                                    if has_j {
                                        let sub_item = vm.get_property_host(&item, &j_key)?;
                                        // Recursive flat for depth > 1 would need more work
                                        out.push(Some(sub_item));
                                    }
                                }
                            } else {
                                out.push(Some(item));
                            }
                        } else {
                            out.push(Some(item));
                        }
                    }
                }

                let arr_rc = Rc::new(RefCell::new(out));
                let props: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(ObjectData::new()));
                props.borrow_mut().prototype = Value::Object(Rc::clone(&array_proto_for_flat));
                props.borrow_mut().properties.insert(
                    PKey::String("length".to_string()),
                    PropertyDescriptor::data(Value::Number(arr_rc.borrow().len() as f64)),
                );
                Ok(Value::Array(arr_rc, props))
            }),
            &function_proto_map,
        )),
    );
    let array_ctor_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
        ObjectData::with_prototype(Value::Object(Rc::clone(&function_proto_map))),
    ));
    array_ctor_map.borrow_mut().properties.insert(
        PKey::String("prototype".to_string()),
        PropertyDescriptor::data(Value::Object(Rc::clone(&array_proto_map))),
    );

    let string_proto_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
        ObjectData::with_prototype(Value::Object(Rc::clone(&object_proto_map))),
    ));
    let string_ctor_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
        ObjectData::with_prototype(Value::Object(Rc::clone(&function_proto_map))),
    ));
    string_ctor_map.borrow_mut().properties.insert(
        PKey::String("prototype".to_string()),
        PropertyDescriptor::data(Value::Object(Rc::clone(&string_proto_map))),
    );

    let error_proto_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
        ObjectData::with_prototype(Value::Object(Rc::clone(&object_proto_map))),
    ));
    let error_ctor_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
        ObjectData::with_prototype(Value::Object(Rc::clone(&function_proto_map))),
    ));
    error_ctor_map.borrow_mut().properties.insert(
        PKey::String("prototype".to_string()),
        PropertyDescriptor::data(Value::Object(Rc::clone(&error_proto_map))),
    );

    let aggregate_error_proto_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
        ObjectData::with_prototype(Value::Object(Rc::clone(&error_proto_map))),
    ));
    let aggregate_error_ctor_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
        ObjectData::with_prototype(Value::Object(Rc::clone(&function_proto_map))),
    ));
    aggregate_error_ctor_map.borrow_mut().properties.insert(
        PKey::String("prototype".to_string()),
        PropertyDescriptor::data(Value::Object(Rc::clone(&aggregate_error_proto_map))),
    );

    let function_ctor_proto = Rc::clone(&function_proto_map);
    let function_ctor_fn: BuiltinFn = Rc::new(move |_vm, args| {
        let body = args
            .last()
            .cloned()
            .unwrap_or(Value::String("return undefined".to_string()));
        let body_src = match body {
            Value::String(s) => s,
            _ => "return undefined".to_string(),
        };
        let body_src = body_src.trim().to_string();
        if crate::trace::enabled() {
            eprintln!("[trace] Function ctor body='{}'", body_src);
        }
        let call_target: BuiltinFn = Rc::new(move |vm, _call_args| {
            let src = body_src.trim();
            if src == "return this" || src == "return this;" {
                return Ok(vm.get_global("globalThis").unwrap_or(Value::Undefined));
            }
            vm.eval_expression_source(src)
        });
        Ok(Value::BuiltinObject(
            call_target,
            Rc::new(RefCell::new(ObjectData::with_prototype(Value::Object(
                Rc::clone(&function_ctor_proto),
            )))),
        ))
    });
    let function_ctor_val =
        Value::BuiltinObject(function_ctor_fn.clone(), Rc::clone(&function_ctor_map));
    let function_call_val = builtin_object(
        Rc::new(|vm, args| {
            let callee = args.first().cloned().unwrap_or(Value::Undefined);
            let this_arg = args.get(1).cloned().unwrap_or(Value::Undefined);
            let rest = args.get(2..).unwrap_or(&[]);
            vm.call_value_with_this(&callee, &this_arg, rest)
        }),
        &function_proto_map,
    );
    function_proto_map.borrow_mut().properties.insert(
        PKey::String("constructor".to_string()),
        PropertyDescriptor::data(function_ctor_val.clone()),
    );
    function_proto_map.borrow_mut().properties.insert(
        PKey::String("call".to_string()),
        PropertyDescriptor::data(function_call_val.clone()),
    );
    function_proto_map.borrow_mut().properties.insert(
        PKey::String("apply".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(|vm, args| {
                let callee = args.first().cloned().unwrap_or(Value::Undefined);
                let this_arg = args.get(1).cloned().unwrap_or(Value::Undefined);
                let call_args: Vec<Value> = match args.get(2) {
                    None | Some(Value::Undefined) | Some(Value::Null) => Vec::new(),
                    Some(Value::Array(arr, _)) => {
                        arr.borrow().iter().filter_map(Clone::clone).collect()
                    }
                    Some(_) => Vec::new(),
                };
                vm.call_value_with_this(&callee, &this_arg, &call_args)
            }),
            &function_proto_map,
        )),
    );
    function_proto_map.borrow_mut().properties.insert(
        PKey::String("bind".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new({
                let function_proto_map = Rc::clone(&function_proto_map);
                let _function_call_val = function_call_val;
                move |_vm, args| {
                    let target = args.first().cloned().unwrap_or(Value::Undefined);
                    let bound_this = args.get(1).cloned().unwrap_or(Value::Undefined);
                    let bound_args: Vec<Value> = args.iter().skip(2).cloned().collect();
                    if crate::trace::enabled() {
                        eprintln!(
                            "[trace] bind create target={:?} bound_this={:?} bound_args={:?}",
                            target, bound_this, bound_args
                        );
                    }
                    let bound_target = target;
                    let bound_this_arg = bound_this;
                    let prefix_args = bound_args;
                    Ok(builtin_object(
                        Rc::new(move |vm, call_args| {
                            if crate::trace::enabled() {
                                eprintln!(
                                    "[trace] bound fn call target={:?} this={:?} args={:?}",
                                    bound_target, bound_this_arg, call_args
                                );
                            }
                            let mut combined = prefix_args.clone();
                            combined.extend(call_args.iter().cloned());
                            vm.call_value_with_this(&bound_target, &bound_this_arg, &combined)
                        }),
                        &function_proto_map,
                    ))
                }
            }),
            &function_proto_map,
        )),
    );

    let object_ctor_props = Rc::clone(&object_ctor_map);
    let object_ctor_fn: BuiltinFn = Rc::new(move |_vm, _args| {
        let proto = object_ctor_props
            .borrow()
            .properties
            .get(&PKey::String("prototype".to_string()))
            .and_then(|d| match d {
                PropertyDescriptor::Data { value, .. } => Some(value.clone()),
                _ => None,
            })
            .unwrap_or(Value::Null);
        let mut inst = ObjectData::new();
        inst.prototype = proto;
        Ok(Value::Object(Rc::new(RefCell::new(inst))))
    });
    let object_ctor_val = Value::BuiltinObject(object_ctor_fn.clone(), Rc::clone(&object_ctor_map));

    let symbol_proto_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
        ObjectData::with_prototype(Value::Object(Rc::clone(&object_proto_map))),
    ));
    let symbol_ctor_fn: BuiltinFn = Rc::new(move |vm, args| {
        let value_idx = if args.len() > 1 { 1 } else { 0 };
        let desc = match args.get(value_idx) {
            Some(Value::Undefined) | None => None,
            Some(Value::String(s)) => Some(s.clone()),
            Some(v) => Some(vm.to_string(v)?),
        };
        Ok(Value::Symbol(SymbolId::new(), desc))
    });
    let symbol_ctor_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
        ObjectData::with_prototype(Value::Object(Rc::clone(&function_proto_map))),
    ));
    symbol_ctor_map.borrow_mut().properties.insert(
        PKey::String("prototype".to_string()),
        PropertyDescriptor::data(Value::Object(Rc::clone(&symbol_proto_map))),
    );
    let symbol_ctor_val = Value::BuiltinObject(symbol_ctor_fn.clone(), Rc::clone(&symbol_ctor_map));
    symbol_proto_map.borrow_mut().properties.insert(
        PKey::String("constructor".to_string()),
        PropertyDescriptor::data(symbol_ctor_val.clone()),
    );
    symbol_proto_map.borrow_mut().properties.insert(
        PKey::String("toString".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(|_vm, args| {
                let this = args.first().cloned().unwrap_or(Value::Undefined);
                match this {
                    Value::Symbol(_, desc) => Ok(Value::String(match desc {
                        Some(d) => format!("Symbol({})", d),
                        None => "Symbol()".to_string(),
                    })),
                    _ => Ok(Value::String("[object Symbol]".to_string())),
                }
            }),
            &function_proto_map,
        )),
    );
    symbol_proto_map.borrow_mut().properties.insert(
        PKey::String("valueOf".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(|_vm, args| Ok(args.first().cloned().unwrap_or(Value::Undefined))),
            &function_proto_map,
        )),
    );
    symbol_ctor_map.borrow_mut().properties.insert(
        PKey::String("iterator".to_string()),
        PropertyDescriptor::data(Value::Symbol(
            SymbolId::well_known_iterator(),
            Some("Symbol.iterator".to_string()),
        )),
    );
    symbol_ctor_map.borrow_mut().properties.insert(
        PKey::String("asyncIterator".to_string()),
        PropertyDescriptor::data(Value::Symbol(
            SymbolId::well_known_async_iterator(),
            Some("Symbol.asyncIterator".to_string()),
        )),
    );
    symbol_ctor_map.borrow_mut().properties.insert(
        PKey::String("toPrimitive".to_string()),
        PropertyDescriptor::data(Value::Symbol(
            SymbolId::well_known_to_primitive(),
            Some("Symbol.toPrimitive".to_string()),
        )),
    );
    symbol_ctor_map.borrow_mut().properties.insert(
        PKey::String("toStringTag".to_string()),
        PropertyDescriptor::data(Value::Symbol(
            SymbolId::well_known_to_string_tag(),
            Some("Symbol.toStringTag".to_string()),
        )),
    );
    symbol_ctor_map.borrow_mut().properties.insert(
        PKey::String("species".to_string()),
        PropertyDescriptor::data(Value::Symbol(
            SymbolId::well_known_species(),
            Some("Symbol.species".to_string()),
        )),
    );
    symbol_ctor_map.borrow_mut().properties.insert(
        PKey::String("hasInstance".to_string()),
        PropertyDescriptor::data(Value::Symbol(
            SymbolId::well_known_has_instance(),
            Some("Symbol.hasInstance".to_string()),
        )),
    );
    symbol_ctor_map.borrow_mut().properties.insert(
        PKey::String("isConcatSpreadable".to_string()),
        PropertyDescriptor::data(Value::Symbol(
            SymbolId::well_known_is_concat_spreadable(),
            Some("Symbol.isConcatSpreadable".to_string()),
        )),
    );
    symbol_ctor_map.borrow_mut().properties.insert(
        PKey::String("match".to_string()),
        PropertyDescriptor::data(Value::Symbol(
            SymbolId::well_known_match(),
            Some("Symbol.match".to_string()),
        )),
    );
    symbol_ctor_map.borrow_mut().properties.insert(
        PKey::String("matchAll".to_string()),
        PropertyDescriptor::data(Value::Symbol(
            SymbolId::well_known_match_all(),
            Some("Symbol.matchAll".to_string()),
        )),
    );
    symbol_ctor_map.borrow_mut().properties.insert(
        PKey::String("replace".to_string()),
        PropertyDescriptor::data(Value::Symbol(
            SymbolId::well_known_replace(),
            Some("Symbol.replace".to_string()),
        )),
    );
    symbol_ctor_map.borrow_mut().properties.insert(
        PKey::String("search".to_string()),
        PropertyDescriptor::data(Value::Symbol(
            SymbolId::well_known_search(),
            Some("Symbol.search".to_string()),
        )),
    );
    symbol_ctor_map.borrow_mut().properties.insert(
        PKey::String("split".to_string()),
        PropertyDescriptor::data(Value::Symbol(
            SymbolId::well_known_split(),
            Some("Symbol.split".to_string()),
        )),
    );
    symbol_ctor_map.borrow_mut().properties.insert(
        PKey::String("unscopables".to_string()),
        PropertyDescriptor::data(Value::Symbol(
            SymbolId::well_known_unscopables(),
            Some("Symbol.unscopables".to_string()),
        )),
    );
    object_proto_map.borrow_mut().properties.insert(
        PKey::String("constructor".to_string()),
        PropertyDescriptor::data(object_ctor_val.clone()),
    );

    // Promise constructor
    let promise_proto_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
        ObjectData::with_prototype(Value::Object(Rc::clone(&object_proto_map))),
    ));
    let promise_ctor_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
        ObjectData::with_prototype(Value::Object(Rc::clone(&function_proto_map))),
    ));
    let promise_ctor_fn: BuiltinFn = {
        let _promise_proto_map_rc = Rc::clone(&promise_proto_map);
        Rc::new(move |vm, args| {
            // Promise constructor takes an executor function
            let executor = args.first().cloned().unwrap_or(Value::Undefined);

            // Create a new pending promise
            use crate::value::PromiseState;
            let promise = Rc::new(RefCell::new(PromiseState::Pending {
                on_fulfilled: Vec::new(),
                on_rejected: Vec::new(),
            }));

            // Create resolve function
            let promise_for_resolve = Rc::clone(&promise);
            let resolve_fn = Value::Builtin(Rc::new(move |vm, args| {
                let value = args.first().cloned().unwrap_or(Value::Undefined);
                let mut state = promise_for_resolve.borrow_mut();
                match &*state {
                    PromiseState::Pending { on_fulfilled, .. } => {
                        // Transition to fulfilled state
                        let callbacks = on_fulfilled.clone();
                        let value_for_return = value.clone();
                        *state = PromiseState::Fulfilled(value);
                        drop(state);

                        // Call all registered fulfillment callbacks synchronously
                        // (In a real implementation, these would be queued as microtasks)
                        for callback in callbacks {
                            let _ = vm.call_value_with_this(
                                &callback,
                                &Value::Undefined,
                                &[value_for_return.clone()],
                            );
                        }
                    }
                    _ => {
                        // Already settled, do nothing
                    }
                }
                Ok(Value::Undefined)
            }));

            // Create reject function
            let promise_for_reject = Rc::clone(&promise);
            let reject_fn = Value::Builtin(Rc::new(move |vm, args| {
                let reason = args.first().cloned().unwrap_or(Value::Undefined);
                let mut state = promise_for_reject.borrow_mut();
                match &*state {
                    PromiseState::Pending { on_rejected, .. } => {
                        // Transition to rejected state
                        let callbacks = on_rejected.clone();
                        let reason_for_return = reason.clone();
                        *state = PromiseState::Rejected(reason);
                        drop(state);

                        // Call all registered rejection callbacks synchronously
                        // (In a real implementation, these would be queued as microtasks)
                        for callback in callbacks {
                            let _ = vm.call_value_with_this(
                                &callback,
                                &Value::Undefined,
                                &[reason_for_return.clone()],
                            );
                        }
                    }
                    _ => {
                        // Already settled, do nothing
                    }
                }
                Ok(Value::Undefined)
            }));

            // Call the executor with resolve and reject functions
            let _ = vm.call_value_with_this(&executor, &Value::Undefined, &[resolve_fn, reject_fn]);

            // Return the promise
            Ok(Value::Promise(promise))
        })
    };
    promise_ctor_map.borrow_mut().properties.insert(
        PKey::String("prototype".to_string()),
        PropertyDescriptor::data(Value::Object(Rc::clone(&promise_proto_map))),
    );
    let promise_ctor_val =
        Value::BuiltinObject(promise_ctor_fn.clone(), Rc::clone(&promise_ctor_map));
    promise_proto_map.borrow_mut().properties.insert(
        PKey::String("constructor".to_string()),
        PropertyDescriptor::data(promise_ctor_val.clone()),
    );

    // Promise.prototype.then()
    promise_proto_map.borrow_mut().properties.insert(
        PKey::String("then".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(|vm, args| {
                let this = args.first().cloned().unwrap_or(Value::Undefined);
                let on_fulfilled = args.get(1).cloned().unwrap_or(Value::Undefined);
                let on_rejected = args.get(2).cloned().unwrap_or(Value::Undefined);

                if let Value::Promise(promise_rc) = this {
                    use crate::value::PromiseState;

                    // Create a new promise for chaining
                    let result_promise = Rc::new(RefCell::new(PromiseState::Pending {
                        on_fulfilled: Vec::new(),
                        on_rejected: Vec::new(),
                    }));

                    let mut state = promise_rc.borrow_mut();
                    match &*state {
                        PromiseState::Pending {
                            on_fulfilled: existing_fulfilled,
                            on_rejected: existing_rejected,
                        } => {
                            // Create wrapper callbacks that will resolve/reject the new promise
                            let result_promise_for_fulfill = Rc::clone(&result_promise);
                            let on_fulfilled_fn = Value::Builtin(Rc::new(move |_vm, args| {
                                let value = args.first().cloned().unwrap_or(Value::Undefined);
                                let mut new_state = result_promise_for_fulfill.borrow_mut();
                                *new_state = PromiseState::Fulfilled(value);
                                Ok(Value::Undefined)
                            }));

                            let result_promise_for_reject = Rc::clone(&result_promise);
                            let on_rejected_fn = Value::Builtin(Rc::new(move |_vm, args| {
                                let reason = args.first().cloned().unwrap_or(Value::Undefined);
                                let mut new_state = result_promise_for_reject.borrow_mut();
                                *new_state = PromiseState::Rejected(reason);
                                Ok(Value::Undefined)
                            }));

                            // Register the wrappers with the original promise
                            // (This would need to be stored on the Pending promise state)
                            let mut new_fulfilled = existing_fulfilled.clone();
                            new_fulfilled.push(on_fulfilled_fn);
                            let mut new_rejected = existing_rejected.clone();
                            new_rejected.push(on_rejected_fn);

                            *state = PromiseState::Pending {
                                on_fulfilled: new_fulfilled,
                                on_rejected: new_rejected,
                            };
                        }
                        PromiseState::Fulfilled(value) => {
                            // Call on_fulfilled immediately if provided
                            if !matches!(on_fulfilled, Value::Undefined) {
                                let _ = vm.call_value_with_this(
                                    &on_fulfilled,
                                    &Value::Undefined,
                                    &[value.clone()],
                                );
                            }
                            *result_promise.borrow_mut() = PromiseState::Fulfilled(value.clone());
                        }
                        PromiseState::Rejected(reason) => {
                            // Call on_rejected immediately if provided
                            if !matches!(on_rejected, Value::Undefined) {
                                let _ = vm.call_value_with_this(
                                    &on_rejected,
                                    &Value::Undefined,
                                    &[reason.clone()],
                                );
                            }
                            *result_promise.borrow_mut() = PromiseState::Rejected(reason.clone());
                        }
                    }

                    Ok(Value::Promise(result_promise))
                } else {
                    // Not a promise, return undefined
                    Ok(Value::Undefined)
                }
            }),
            &function_proto_map,
        )),
    );

    // Promise.resolve() static method
    promise_ctor_map.borrow_mut().properties.insert(
        PKey::String("resolve".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(|_vm, args| {
                use crate::value::PromiseState;
                let value = args.first().cloned().unwrap_or(Value::Undefined);
                let promise = Rc::new(RefCell::new(PromiseState::Fulfilled(value)));
                Ok(Value::Promise(promise))
            }),
            &function_proto_map,
        )),
    );

    // Promise.reject() static method
    promise_ctor_map.borrow_mut().properties.insert(
        PKey::String("reject".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(|_vm, args| {
                use crate::value::PromiseState;
                let reason = args.first().cloned().unwrap_or(Value::Undefined);
                let promise = Rc::new(RefCell::new(PromiseState::Rejected(reason)));
                Ok(Value::Promise(promise))
            }),
            &function_proto_map,
        )),
    );

    // Reflect.construct
    let reflect_obj = Rc::new(RefCell::new(ObjectData::with_prototype(Value::Object(
        Rc::clone(&object_proto_map),
    ))));
    reflect_obj.borrow_mut().properties.insert(
        PKey::String("construct".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(|vm, args| {
                let off = static_builtin_arg_offset(vm, args, "Reflect");
                let target = args.get(off).cloned().unwrap_or(Value::Undefined);
                let arg_list = args.get(off + 1).cloned().unwrap_or(Value::Undefined);
                let new_target = args.get(off + 2).cloned().unwrap_or_else(|| target.clone());

                let ctor = if matches!(new_target, Value::Undefined) {
                    &target
                } else {
                    &new_target
                };

                if !target.is_constructable() {
                    return Err(crate::errors::RuntimeError::from(
                        "Reflect.construct: target is not a constructor",
                    ));
                }
                if !ctor.is_constructable() {
                    return Err(crate::errors::RuntimeError::from(
                        "Reflect.construct: newTarget is not a constructor",
                    ));
                }

                let args_vec: Vec<Value> = match arg_list {
                    Value::Array(arr, _) => arr.borrow().iter().filter_map(Clone::clone).collect(),
                    Value::Undefined | Value::Null => Vec::new(),
                    _ => Vec::new(),
                };

                let result = vm.construct_value(&target, &args_vec)?;
                if matches!(new_target, Value::Undefined) {
                    return Ok(result);
                }

                let new_target_proto = vm
                    .get_property_host(&new_target, &PropertyKey::String("prototype".to_string()))
                    .unwrap_or(Value::Undefined);
                let desired_proto = match new_target_proto {
                    Value::Object(_)
                    | Value::Array(..)
                    | Value::Closure { .. }
                    | Value::BuiltinObject(_, _) => Some(new_target_proto),
                    _ => None,
                };

                if let Some(proto_value) = desired_proto
                    && let Value::Object(obj) = &result
                {
                    obj.borrow_mut().prototype = proto_value;
                }

                Ok(result)
            }),
            &function_proto_map,
        )),
    );

    // Date constructor

    let date_proto_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
        ObjectData::with_prototype(Value::Object(Rc::clone(&object_proto_map))),
    ));
    let date_ctor_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(ObjectData::with_prototype(
        Value::Object(Rc::clone(&function_proto_map)),
    )));
    let date_ctor_fn: BuiltinFn = {
        let date_proto_map_rc = Rc::clone(&date_proto_map);
        Rc::new(move |_vm, args| {
            use std::time::{SystemTime, UNIX_EPOCH};

            // Helper to convert value to number, returning NaN if not a number
            fn to_num(v: &Value) -> f64 {
                match v {
                    Value::Number(n) => *n,
                    Value::Undefined => f64::NAN,
                    Value::Null => 0.0,
                    Value::Bool(b) => {
                        if *b {
                            1.0
                        } else {
                            0.0
                        }
                    }
                    Value::String(s) => s.parse::<f64>().unwrap_or(f64::NAN),
                    _ => f64::NAN,
                }
            }

            // Simplified Unix timestamp calculation for date components
            fn date_to_timestamp(
                year: i32,
                month: i32,
                day: i32,
                hour: i32,
                min: i32,
                sec: i32,
                ms: i32,
            ) -> f64 {
                // month is 1-indexed (1=Jan, 12=Dec)
                // Simplified - assumes proleptic Gregorian calendar
                let is_leap = |y: i32| (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;

                let mut total_days = 0i64;

                // Add days for complete years before this year
                for y in 1970..year {
                    total_days += if is_leap(y) { 366 } else { 365 };
                }

                // Add days for complete months in this year
                let days_in_month = [0, 31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
                for m in 1..month {
                    let mut m_days = days_in_month[m.min(12) as usize] as i64;
                    if m == 2 && is_leap(year) {
                        m_days += 1;
                    }
                    total_days += m_days;
                }

                // Add days in this month (day is 1-indexed)
                total_days += (day - 1) as i64;

                // Convert to milliseconds
                let total_secs =
                    total_days * 86400 + hour as i64 * 3600 + min as i64 * 60 + sec as i64;
                total_secs as f64 * 1000.0 + ms as f64
            }

            let timestamp = match args.len() {
                1 => {
                    // Only receiver, no user arguments: current time
                    match SystemTime::now().duration_since(UNIX_EPOCH) {
                        Ok(duration) => duration.as_millis() as f64,
                        Err(_) => 0.0,
                    }
                }
                2 => {
                    // Receiver + single argument: timestamp or date string
                    if let Some(arg) = args.get(1) {
                        to_num(arg)
                    } else {
                        0.0
                    }
                }
                _ => {
                    // Multiple user arguments: year, month [, day, hour, minute, second, millisecond]
                    let year = to_num(args.get(1).unwrap_or(&Value::Undefined)) as i32;
                    let month = to_num(args.get(2).unwrap_or(&Value::Undefined)) as i32; // 0-indexed
                    let day = to_num(args.get(3).unwrap_or(&Value::Number(1.0))) as i32;
                    let hour = to_num(args.get(4).unwrap_or(&Value::Number(0.0))) as i32;
                    let min = to_num(args.get(5).unwrap_or(&Value::Number(0.0))) as i32;
                    let sec = to_num(args.get(6).unwrap_or(&Value::Number(0.0))) as i32;
                    let ms = to_num(args.get(7).unwrap_or(&Value::Number(0.0))) as i32;

                    date_to_timestamp(year, month + 1, day, hour, min, sec, ms)
                }
            };

            let mut date_obj_data =
                ObjectData::with_prototype(Value::Object(Rc::clone(&date_proto_map_rc)));
            date_obj_data.properties.insert(
                PropertyKey::String("[[PrimitiveValue]]".to_string()),
                PropertyDescriptor::data(Value::Number(timestamp)),
            );
            let date_obj = Rc::new(RefCell::new(date_obj_data));
            Ok(Value::Object(date_obj))
        })
    };
    let date_ctor_val = Value::BuiltinObject(date_ctor_fn.clone(), Rc::clone(&date_ctor_map));
    date_ctor_map.borrow_mut().properties.insert(
        PKey::String("prototype".to_string()),
        PropertyDescriptor::data(Value::Object(Rc::clone(&date_proto_map))),
    );
    date_proto_map.borrow_mut().properties.insert(
        PKey::String("constructor".to_string()),
        PropertyDescriptor::data(date_ctor_val.clone()),
    );
    date_proto_map.borrow_mut().properties.insert(
        PKey::String("valueOf".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(|_vm, args| {
                let this = args.first().cloned().unwrap_or(Value::Undefined);
                if let Value::Object(obj) = this
                    && let Some(PropertyDescriptor::Data { value, .. }) = obj
                        .borrow()
                        .properties
                        .get(&PropertyKey::String("[[PrimitiveValue]]".to_string()))
                {
                    return Ok(value.clone());
                }
                Ok(Value::Number(f64::NAN))
            }),
            &function_proto_map,
        )),
    );
    date_proto_map.borrow_mut().properties.insert(
        PKey::String("getTime".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(|_vm, args| {
                let this = args.first().cloned().unwrap_or(Value::Undefined);
                if let Value::Object(obj) = this
                    && let Some(PropertyDescriptor::Data { value, .. }) = obj
                        .borrow()
                        .properties
                        .get(&PropertyKey::String("[[PrimitiveValue]]".to_string()))
                {
                    return Ok(value.clone());
                }
                Ok(Value::Number(f64::NAN))
            }),
            &function_proto_map,
        )),
    );
    date_proto_map.borrow_mut().properties.insert(
        PKey::String("getTimezoneOffset".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(|_vm, _args| {
                // Return UTC offset in minutes (simplified - assuming UTC for now)
                Ok(Value::Number(0.0))
            }),
            &function_proto_map,
        )),
    );
    date_proto_map.borrow_mut().properties.insert(
        PKey::String("toISOString".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(|_vm, args| {
                let this = args.first().cloned().unwrap_or(Value::Undefined);
                if let Value::Object(obj) = this
                    && let Some(PropertyDescriptor::Data { value, .. }) = obj
                        .borrow()
                        .properties
                        .get(&PropertyKey::String("[[PrimitiveValue]]".to_string()))
                    && let Value::Number(n) = value
                {
                    // Convert timestamp to ISO string (simplified)
                    let millis = *n as i64;
                    let secs = millis / 1000;
                    let ms = millis % 1000;

                    // Calculate date components (simplified)
                    let year = 1970 + (secs / (365 * 24 * 3600)) as i32;
                    let day_of_year = ((secs % (365 * 24 * 3600)) / (24 * 3600)) as i32;
                    let hour = ((secs % (24 * 3600)) / 3600) as i32;
                    let minute = ((secs % 3600) / 60) as i32;
                    let second = (secs % 60) as i32;

                    return Ok(Value::String(format!(
                        "{:04}-01-{:02}T{:02}:{:02}:{:02}.{:03}Z",
                        year,
                        day_of_year.min(31).max(1),
                        hour,
                        minute,
                        second,
                        ms
                    )));
                }
                Ok(Value::String("Invalid Date".to_string()))
            }),
            &function_proto_map,
        )),
    );

    // ArrayBuffer constructor
    let arraybuffer_proto_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
        ObjectData::with_prototype(Value::Object(Rc::clone(&object_proto_map))),
    ));
    let arraybuffer_ctor_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
        ObjectData::with_prototype(Value::Object(Rc::clone(&function_proto_map))),
    ));
    let arraybuffer_ctor_fn: BuiltinFn = {
        let proto_map_rc = Rc::clone(&arraybuffer_proto_map);
        Rc::new(move |_vm, args| {
            // Get length argument
            let length = match args.get(1) {
                Some(Value::Number(n)) => (*n as usize).max(0),
                Some(Value::Undefined) => 0,
                Some(v) => {
                    // Try to convert to number
                    if let Value::String(s) = v {
                        s.parse::<usize>().unwrap_or(0)
                    } else {
                        0
                    }
                }
                None => 0,
            };

            let mut arraybuffer_obj_data =
                ObjectData::with_prototype(Value::Object(Rc::clone(&proto_map_rc)));
            arraybuffer_obj_data.properties.insert(
                PropertyKey::String("[[ArrayBufferData]]".to_string()),
                PropertyDescriptor::data(Value::Array(
                    Rc::new(RefCell::new(vec![Some(Value::Number(0.0)); length])),
                    Rc::new(RefCell::new(ObjectData::new())),
                )),
            );
            arraybuffer_obj_data.properties.insert(
                PropertyKey::String("byteLength".to_string()),
                PropertyDescriptor::data(Value::Number(length as f64)),
            );
            let arraybuffer_obj = Rc::new(RefCell::new(arraybuffer_obj_data));
            Ok(Value::Object(arraybuffer_obj))
        })
    };
    let arraybuffer_ctor_val = Value::BuiltinObject(
        arraybuffer_ctor_fn.clone(),
        Rc::clone(&arraybuffer_ctor_map),
    );
    arraybuffer_ctor_map.borrow_mut().properties.insert(
        PKey::String("prototype".to_string()),
        PropertyDescriptor::data(Value::Object(Rc::clone(&arraybuffer_proto_map))),
    );
    arraybuffer_proto_map.borrow_mut().properties.insert(
        PKey::String("constructor".to_string()),
        PropertyDescriptor::data(arraybuffer_ctor_val.clone()),
    );

    // Typed Array constructors
    macro_rules! create_typed_array_ctor {
        ($name:expr, $element_count:expr) => {{
            let proto_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
                ObjectData::with_prototype(Value::Object(Rc::clone(&object_proto_map))),
            ));
            let ctor_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
                ObjectData::with_prototype(Value::Object(Rc::clone(&function_proto_map))),
            ));
            let ctor_fn: BuiltinFn = {
                let proto_map_rc = Rc::clone(&proto_map);
                Rc::new(move |_vm, args| {
                    let length = match args.get(1) {
                        Some(Value::Number(n)) => (*n as usize).max(0),
                        Some(Value::Undefined) => 0,
                        Some(v) => {
                            if let Value::String(s) = v {
                                s.parse::<usize>().unwrap_or(0)
                            } else {
                                0
                            }
                        }
                        None => 0,
                    };

                    let mut typed_array_data =
                        ObjectData::with_prototype(Value::Object(Rc::clone(&proto_map_rc)));
                    typed_array_data.properties.insert(
                        PropertyKey::String("[[TypedArrayData]]".to_string()),
                        PropertyDescriptor::data(Value::Array(
                            Rc::new(RefCell::new(vec![Some(Value::Number(0.0)); length])),
                            Rc::new(RefCell::new(ObjectData::new())),
                        )),
                    );
                    typed_array_data.properties.insert(
                        PropertyKey::String("length".to_string()),
                        PropertyDescriptor::data(Value::Number(length as f64)),
                    );
                    typed_array_data.properties.insert(
                        PropertyKey::String("byteLength".to_string()),
                        PropertyDescriptor::data(Value::Number((length * $element_count) as f64)),
                    );
                    let typed_array_obj = Rc::new(RefCell::new(typed_array_data));
                    Ok(Value::Object(typed_array_obj))
                })
            };
            let ctor_val = Value::BuiltinObject(ctor_fn.clone(), Rc::clone(&ctor_map));
            ctor_map.borrow_mut().properties.insert(
                PKey::String("prototype".to_string()),
                PropertyDescriptor::data(Value::Object(Rc::clone(&proto_map))),
            );
            proto_map.borrow_mut().properties.insert(
                PKey::String("constructor".to_string()),
                PropertyDescriptor::data(ctor_val.clone()),
            );
            ctor_val
        }};
    }

    let uint8array_ctor_val = create_typed_array_ctor!("Uint8Array", 1);
    let int8array_ctor_val = create_typed_array_ctor!("Int8Array", 1);
    let uint16array_ctor_val = create_typed_array_ctor!("Uint16Array", 2);
    let int16array_ctor_val = create_typed_array_ctor!("Int16Array", 2);
    let uint32array_ctor_val = create_typed_array_ctor!("Uint32Array", 4);
    let int32array_ctor_val = create_typed_array_ctor!("Int32Array", 4);
    let float32array_ctor_val = create_typed_array_ctor!("Float32Array", 4);
    let float64array_ctor_val = create_typed_array_ctor!("Float64Array", 8);
    let uint8clampedarray_ctor_val = create_typed_array_ctor!("Uint8ClampedArray", 1);

    let map_proto_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(ObjectData::with_prototype(
        Value::Object(Rc::clone(&object_proto_map)),
    )));
    let map_ctor_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(ObjectData::with_prototype(
        Value::Object(Rc::clone(&function_proto_map)),
    )));
    let map_ctor_fn: BuiltinFn = Rc::new(move |vm, args| {
        let receiver = args.first().cloned().unwrap_or(Value::Undefined);
        if matches!(
            receiver,
            Value::Object(_)
                | Value::Array(..)
                | Value::Closure { .. }
                | Value::BuiltinObject(_, _)
        ) {
            let arr_rc = Rc::new(RefCell::new(Vec::<Option<Value>>::new()));
            let arr_props = Rc::new(RefCell::new(ObjectData::new()));
            arr_props.borrow_mut().properties.insert(
                PropertyKey::String("length".to_string()),
                PropertyDescriptor::data(Value::Number(0.0)),
            );
            let _ = vm.set_property_host(
                &receiver,
                &Value::String("__map_entries".to_string()),
                Value::Array(arr_rc, arr_props),
            )?;
            let _ = vm.set_property_host(
                &receiver,
                &Value::String("size".to_string()),
                Value::Number(0.0),
            )?;
            return Ok(receiver);
        }
        Ok(Value::Object(Rc::new(RefCell::new(ObjectData::new()))))
    });
    let map_ctor_val = Value::BuiltinObject(map_ctor_fn.clone(), Rc::clone(&map_ctor_map));
    map_ctor_map.borrow_mut().properties.insert(
        PKey::String("prototype".to_string()),
        PropertyDescriptor::data(Value::Object(Rc::clone(&map_proto_map))),
    );
    map_proto_map.borrow_mut().properties.insert(
        PKey::String("constructor".to_string()),
        PropertyDescriptor::data(map_ctor_val.clone()),
    );
    map_proto_map.borrow_mut().properties.insert(
        PKey::String("get".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(|vm, args| {
                let this = args.first().cloned().unwrap_or(Value::Undefined);
                let key = args.get(1).cloned().unwrap_or(Value::Undefined);
                let entries =
                    vm.get_property_host(&this, &PropertyKey::String("__map_entries".to_string()))?;
                if let Value::Array(arr, _) = entries {
                    for item in arr.borrow().iter() {
                        if let Some(Value::Array(pair, _)) = item {
                            let p = pair.borrow();
                            if p.len() >= 2
                                && strict_equal(p[0].as_ref().unwrap_or(&Value::Undefined), &key)
                            {
                                return Ok(p[1].as_ref().unwrap_or(&Value::Undefined).clone());
                            }
                        }
                    }
                }
                Ok(Value::Undefined)
            }),
            &function_proto_map,
        )),
    );
    map_proto_map.borrow_mut().properties.insert(
        PKey::String("has".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(|vm, args| {
                let this = args.first().cloned().unwrap_or(Value::Undefined);
                let key = args.get(1).cloned().unwrap_or(Value::Undefined);
                let entries =
                    vm.get_property_host(&this, &PropertyKey::String("__map_entries".to_string()))?;
                if let Value::Array(arr, _) = entries {
                    for item in arr.borrow().iter() {
                        if let Some(Value::Array(pair, _)) = item {
                            let p = pair.borrow();
                            if !p.is_empty()
                                && strict_equal(p[0].as_ref().unwrap_or(&Value::Undefined), &key)
                            {
                                return Ok(Value::Bool(true));
                            }
                        }
                    }
                }
                Ok(Value::Bool(false))
            }),
            &function_proto_map,
        )),
    );
    map_proto_map.borrow_mut().properties.insert(
        PKey::String("set".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(|vm, args| {
                let this = args.first().cloned().unwrap_or(Value::Undefined);
                let key = args.get(1).cloned().unwrap_or(Value::Undefined);
                let value = args.get(2).cloned().unwrap_or(Value::Undefined);
                let entries_key = PropertyKey::String("__map_entries".to_string());
                let entries = vm.get_property_host(&this, &entries_key)?;
                if let Value::Array(arr, props) = entries {
                    let mut updated = false;
                    for item in arr.borrow_mut().iter_mut() {
                        if let Some(Value::Array(pair, _)) = item {
                            let mut p = pair.borrow_mut();
                            if !p.is_empty()
                                && strict_equal(p[0].as_ref().unwrap_or(&Value::Undefined), &key)
                            {
                                if p.len() < 2 {
                                    p.push(Some(value.clone()));
                                } else {
                                    p[1] = Some(value.clone());
                                }
                                updated = true;
                                break;
                            }
                        }
                    }
                    if !updated {
                        let pair_rc = Rc::new(RefCell::new(vec![Some(key), Some(value)]));
                        let pair_props = Rc::new(RefCell::new(ObjectData::new()));
                        pair_props.borrow_mut().properties.insert(
                            PropertyKey::String("length".to_string()),
                            PropertyDescriptor::data(Value::Number(2.0)),
                        );
                        arr.borrow_mut()
                            .push(Some(Value::Array(pair_rc, pair_props)));
                        props.borrow_mut().properties.insert(
                            PropertyKey::String("length".to_string()),
                            PropertyDescriptor::data(Value::Number(arr.borrow().len() as f64)),
                        );
                    }
                    let _ = vm.set_property_host(
                        &this,
                        &Value::String("size".to_string()),
                        Value::Number(arr.borrow().len() as f64),
                    )?;
                }
                Ok(this)
            }),
            &function_proto_map,
        )),
    );
    object_proto_map.borrow_mut().properties.insert(
        PKey::String("toString".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(|_vm, args| {
                let this = args.first().cloned().unwrap_or(Value::Null);
                Ok(Value::String(format!("[object {}]", this.object_tag())))
            }),
            &function_proto_map,
        )),
    );
    object_proto_map.borrow_mut().properties.insert(
        PKey::String("valueOf".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(|_vm, args| Ok(args.first().cloned().unwrap_or(Value::Null))),
            &function_proto_map,
        )),
    );
    object_proto_map.borrow_mut().properties.insert(
        PKey::String("hasOwnProperty".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(|vm, args| {
                let this = args.first().cloned().unwrap_or(Value::Undefined);
                let key_value = args.get(1).cloned().unwrap_or(Value::Undefined);
                let key = vm.to_property_key(&key_value)?;
                if crate::trace::enabled() {
                    eprintln!("[trace] hasOwnProperty this={:?} key={:?}", this, key_value);
                }
                let has_own = match this {
                    Value::Object(obj) => obj.borrow().properties.contains_key(&key),
                    Value::Array(_, props)
                    | Value::Closure { props, .. }
                    | Value::BuiltinObject(_, props) => {
                        props.borrow().properties.contains_key(&key)
                    }
                    _ => false,
                };
                Ok(Value::Bool(has_own))
            }),
            &function_proto_map,
        )),
    );
    object_proto_map.borrow_mut().properties.insert(
        PKey::String("propertyIsEnumerable".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(|vm, args| {
                let this = args.first().cloned().unwrap_or(Value::Undefined);
                let key_value = args.get(1).cloned().unwrap_or(Value::Undefined);
                let key = vm.to_property_key(&key_value)?;
                let enumerable =
                    own_property_descriptor(&this, &key).is_some_and(|desc| match desc {
                        PropertyDescriptor::Data { enumerable, .. }
                        | PropertyDescriptor::Accessor { enumerable, .. } => enumerable,
                    });
                Ok(Value::Bool(enumerable))
            }),
            &function_proto_map,
        )),
    );
    object_ctor_map.borrow_mut().properties.insert(
        PKey::String("create".to_string()),
        PropertyDescriptor::data({
            let object_proto_map = Rc::clone(&object_proto_map);
            let builtin = builtin_object(
                Rc::new(move |vm, args| {
                    let off = static_builtin_arg_offset(vm, args, "Object");
                    let proto = args.get(off).cloned().unwrap_or(Value::Undefined);
                    let obj = match proto {
                        Value::Null => ObjectData::with_prototype(Value::Null),
                        Value::Object(_)
                        | Value::Array(..)
                        | Value::Closure { .. }
                        | Value::BuiltinObject(_, _) => ObjectData::with_prototype(proto),
                        _ => {
                            return Err(crate::errors::RuntimeError::from(
                                "Object.create: prototype must be object or null",
                            ));
                        }
                    };
                    let created = Value::Object(Rc::new(RefCell::new(obj)));
                    if let Some(props) = args.get(off + 1)
                        && !matches!(props, Value::Undefined)
                    {
                        for key in own_enumerable_keys(props) {
                            let descriptor = vm.get_property_host(props, &key)?;
                            let key_value = match &key {
                                PropertyKey::String(s) => Value::String(s.clone()),
                                PropertyKey::Symbol(sym) => Value::Symbol(sym.clone(), None),
                            };
                            let ok = vm.define_property_value(&created, &key_value, &descriptor)?;
                            if !ok {
                                return Err(crate::errors::RuntimeError::from(
                                    "Object.create: define property failed",
                                ));
                            }
                        }
                    }
                    let _ = &object_proto_map;
                    Ok(created)
                }),
                &function_proto_map,
            );
            set_builtin_length(&builtin, 2.0);
            builtin
        }),
    );
    object_ctor_map.borrow_mut().properties.insert(
        PKey::String("is".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(|vm, args| {
                let off = static_builtin_arg_offset(vm, args, "Object");
                let left = args.get(off).cloned().unwrap_or(Value::Undefined);
                let right = args.get(off + 1).cloned().unwrap_or(Value::Undefined);
                Ok(Value::Bool(crate::vm::same_value(&left, &right)))
            }),
            &function_proto_map,
        )),
    );
    object_ctor_map.borrow_mut().properties.insert(
        PKey::String("getOwnPropertyDescriptor".to_string()),
        PropertyDescriptor::data({
            let object_proto_map = Rc::clone(&object_proto_map);
            let builtin = builtin_object(
                Rc::new(move |vm, args| {
                    let off = static_builtin_arg_offset(vm, args, "Object");
                    let target = args.get(off).cloned().unwrap_or(Value::Undefined);
                    let key_value = args.get(off + 1).cloned().unwrap_or(Value::Undefined);
                    let key = vm.to_property_key(&key_value)?;
                    let Some(desc) = own_property_descriptor(&target, &key) else {
                        return Ok(Value::Undefined);
                    };
                    Ok(descriptor_value_from_descriptor(&object_proto_map, &desc))
                }),
                &function_proto_map,
            );
            set_builtin_length(&builtin, 2.0);
            builtin
        }),
    );

    object_ctor_map.borrow_mut().properties.insert(
        PKey::String("getOwnPropertySymbols".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(|_vm, args| {
                let target = args.get(1).cloned().unwrap_or(Value::Undefined);
                let symbols = own_property_keys(&target)
                    .into_iter()
                    .filter_map(|key| {
                        if let PropertyKey::Symbol(id) = key {
                            Some(Value::Symbol(id, None))
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>();
                Ok(array_from_values(symbols))
            }),
            &function_proto_map,
        )),
    );

    object_ctor_map.borrow_mut().properties.insert(
        PKey::String("getOwnPropertyDescriptors".to_string()),
        PropertyDescriptor::data({
            let object_proto_map = Rc::clone(&object_proto_map);
            builtin_object(
                Rc::new(move |vm, args| {
                    let off = static_builtin_arg_offset(vm, args, "Object");
                    let target = args.get(off).cloned().unwrap_or(Value::Undefined);
                    let mut result = ObjectData::new();
                    for key in own_property_keys(&target) {
                        if let Some(desc) = own_property_descriptor(&target, &key) {
                            let descriptor_value =
                                descriptor_value_from_descriptor(&object_proto_map, &desc);
                            result
                                .properties
                                .insert(key.clone(), PropertyDescriptor::data(descriptor_value));
                        }
                    }
                    Ok(Value::Object(Rc::new(RefCell::new(result))))
                }),
                &function_proto_map,
            )
        }),
    );
    object_ctor_map.borrow_mut().properties.insert(
        PKey::String("getOwnPropertyNames".to_string()),
        PropertyDescriptor::data({
            let builtin = builtin_object(
                Rc::new(|vm, args| {
                    let off = static_builtin_arg_offset(vm, args, "Object");
                    let target = args.get(off).cloned().unwrap_or(Value::Undefined);
                    let mut names: Vec<String> = Vec::new();
                    match &target {
                        Value::Object(obj) => {
                            for key in obj.borrow().properties.keys() {
                                if let PropertyKey::String(name) = key {
                                    names.push(name.clone());
                                }
                            }
                        }
                        Value::Array(arr, props) => {
                            for idx in 0..arr.borrow().len() {
                                names.push(idx.to_string());
                            }
                            for key in props.borrow().properties.keys() {
                                if let PropertyKey::String(name) = key {
                                    names.push(name.clone());
                                }
                            }
                        }
                        Value::Closure { props, .. } | Value::BuiltinObject(_, props) => {
                            for key in props.borrow().properties.keys() {
                                if let PropertyKey::String(name) = key {
                                    names.push(name.clone());
                                }
                            }
                        }
                        _ => {}
                    }
                    names.sort();
                    names.dedup();
                    let arr_rc = Rc::new(RefCell::new(
                        names
                            .into_iter()
                            .map(|s| Some(Value::String(s)))
                            .collect::<Vec<_>>(),
                    ));
                    let props = Rc::new(RefCell::new(ObjectData::new()));
                    props.borrow_mut().properties.insert(
                        PropertyKey::String("length".to_string()),
                        PropertyDescriptor::data(Value::Number(arr_rc.borrow().len() as f64)),
                    );
                    Ok(Value::Array(arr_rc, props))
                }),
                &function_proto_map,
            );
            set_builtin_length(&builtin, 1.0);
            builtin
        }),
    );
    object_ctor_map.borrow_mut().properties.insert(
        PKey::String("defineProperty".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(|vm, args| {
                let off = static_builtin_arg_offset(vm, args, "Object");
                if args.len() < off + 3 {
                    return Ok(Value::Undefined);
                }
                let target = args[off].clone();
                if crate::trace::enabled() {
                    eprintln!(
                        "[trace] Object.defineProperty target={:?} key={:?} desc={:?}",
                        target,
                        args.get(off + 1),
                        args.get(off + 2)
                    );
                }
                let ok = vm.define_property_value(&target, &args[off + 1], &args[off + 2])?;
                if !ok {
                    return Err(vm.throw_type_error("Object.defineProperty failed"));
                }
                Ok(target)
            }),
            &function_proto_map,
        )),
    );

    object_ctor_map.borrow_mut().properties.insert(
        PKey::String("defineProperties".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(|vm, args| {
                let off = static_builtin_arg_offset(vm, args, "Object");
                let target = args.get(off).cloned().unwrap_or(Value::Undefined);
                let props = args.get(off + 1).cloned().unwrap_or(Value::Undefined);
                if !matches!(
                    props,
                    Value::Object(_)
                        | Value::Array(..)
                        | Value::Closure { .. }
                        | Value::BuiltinObject(_, _)
                ) {
                    return Err(vm.throw_type_error("Object.defineProperties called on non-object"));
                }
                for key in own_property_keys(&props) {
                    let descriptor = vm.get_property_host(&props, &key)?;
                    if matches!(descriptor, Value::Undefined) {
                        continue;
                    }
                    let key_value = property_key_to_value(&key);
                    let ok = vm.define_property_value(&target, &key_value, &descriptor)?;
                    if !ok {
                        return Err(vm.throw_type_error("Object.defineProperties failed"));
                    }
                }
                Ok(target)
            }),
            &function_proto_map,
        )),
    );
    object_ctor_map.borrow_mut().properties.insert(
        PKey::String("getPrototypeOf".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(|vm, args| {
                let off = static_builtin_arg_offset(vm, args, "Object");
                let target = args.get(off).cloned().unwrap_or(Value::Undefined);
                let proto = match target {
                    Value::Object(obj) => obj.borrow().prototype.clone(),
                    Value::Array(_, props) => props.borrow().prototype.clone(),
                    Value::Closure { props, .. } => props.borrow().prototype.clone(),
                    Value::BuiltinObject(_, props) => props.borrow().prototype.clone(),
                    _ => Value::Undefined,
                };
                Ok(proto)
            }),
            &function_proto_map,
        )),
    );
    object_ctor_map.borrow_mut().properties.insert(
        PKey::String("isExtensible".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(|_vm, args| {
                let target = args.get(1).cloned().unwrap_or(Value::Undefined);
                let extensible = match target {
                    Value::Object(obj) => obj.borrow().extensible,
                    Value::Array(_, props)
                    | Value::Closure { props, .. }
                    | Value::BuiltinObject(_, props) => props.borrow().extensible,
                    _ => false,
                };
                Ok(Value::Bool(extensible))
            }),
            &function_proto_map,
        )),
    );

    let array_ctor_props = Rc::clone(&array_ctor_map);
    let array_ctor_fn: BuiltinFn = Rc::new(move |_vm, args| {
        let elems: Vec<Option<Value>> = args.iter().map(|v| Some(v.clone())).collect();
        let arr_rc = Rc::new(RefCell::new(elems));
        let props: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(ObjectData::new()));
        props.borrow_mut().properties.insert(
            PKey::String("length".to_string()),
            PropertyDescriptor::data(Value::Number(arr_rc.borrow().len() as f64)),
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
            props.borrow_mut().prototype = pval;
        }
        Ok(Value::Array(arr_rc, props))
    });
    let array_ctor_val = Value::BuiltinObject(array_ctor_fn.clone(), Rc::clone(&array_ctor_map));
    array_ctor_map.borrow_mut().properties.insert(
        PKey::String("isArray".to_string()),
        PropertyDescriptor::data({
            let builtin = builtin_object(
                Rc::new(|_vm, args| {
                    let value = args.get(1).cloned().unwrap_or(Value::Undefined);
                    Ok(Value::Bool(matches!(value, Value::Array(..))))
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
                let receiver = args.first().cloned().unwrap_or(Value::Undefined);
                let search = args.get(1).cloned().unwrap_or(Value::Undefined);
                let length =
                    vm.get_property_host(&receiver, &PKey::String("length".to_string()))?;
                let len_num = vm.to_number_value(&length)?;
                if !len_num.is_finite() || len_num <= 0.0 {
                    return Ok(Value::Number(-1.0));
                }
                let len = len_num.floor() as i64;
                let mut k = if let Some(from_index) = args.get(2) {
                    let n = vm.to_number_value(from_index)?;
                    if n.is_nan() {
                        len - 1
                    } else if n >= 0.0 {
                        (n.floor() as i64).min(len - 1)
                    } else {
                        let idx = len + n.floor() as i64;
                        if idx < 0 {
                            return Ok(Value::Number(-1.0));
                        }
                        idx
                    }
                } else {
                    len - 1
                };

                if len > 1_000_000 {
                    let mut candidates: Vec<u64> = Vec::new();
                    match &receiver {
                        Value::Object(obj) => {
                            for key in obj.borrow().properties.keys() {
                                if let PKey::String(s) = key
                                    && let Some(idx) = canonical_index_key(s)
                                    && idx <= k as u64
                                {
                                    candidates.push(idx);
                                }
                            }
                        }
                        Value::Array(arr, props) => {
                            let dense_len = arr.borrow().len() as u64;
                            for idx in 0..dense_len.min((k as u64).saturating_add(1)) {
                                candidates.push(idx);
                            }
                            for key in props.borrow().properties.keys() {
                                if let PKey::String(s) = key
                                    && let Some(idx) = canonical_index_key(s)
                                    && idx <= k as u64
                                {
                                    candidates.push(idx);
                                }
                            }
                        }
                        Value::Closure { props, .. } | Value::BuiltinObject(_, props) => {
                            for key in props.borrow().properties.keys() {
                                if let PKey::String(s) = key
                                    && let Some(idx) = canonical_index_key(s)
                                    && idx <= k as u64
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
                        let key = PKey::String(idx.to_string());
                        let element = vm.get_property_host(&receiver, &key)?;
                        if strict_equal(&element, &search) {
                            return Ok(Value::Number(idx as f64));
                        }
                    }
                    return Ok(Value::Number(-1.0));
                }

                while k >= 0 {
                    let key = PKey::String(k.to_string());
                    if vm.has_property_host(&receiver, &key)? {
                        let element = vm.get_property_host(&receiver, &key)?;
                        if env::var("TSBLITZ_TRACE").ok().as_deref() == Some("1") {
                            eprintln!(
                                "[trace] lastIndexOf k={} element={} search={}",
                                k, element, search
                            );
                        }
                        if strict_equal(&element, &search) {
                            return Ok(Value::Number(k as f64));
                        }
                    }
                    k -= 1;
                }
                Ok(Value::Number(-1.0))
            }),
            &function_proto_map,
        )),
    );

    let string_ctor_fn: BuiltinFn = Rc::new(move |_vm, args| {
        let value = args.first();
        if value.is_none() {
            return Ok(Value::String(String::new()));
        }
        match value.expect("checked is_some") {
            Value::String(s) => Ok(Value::String(s.clone())),
            Value::Number(n) => Ok(Value::String(n.to_string())),
            Value::Bool(b) => Ok(Value::String(
                (if *b { "true" } else { "false" }).to_string(),
            )),
            Value::Null => Ok(Value::String("null".to_string())),
            Value::Undefined => Ok(Value::String("undefined".to_string())),
            Value::BigInt(n) => Ok(Value::String(n.to_string())),
            Value::Symbol(_, desc) => Ok(Value::String(match desc {
                Some(d) => format!("Symbol({})", d),
                None => "Symbol()".to_string(),
            })),
            _ => Ok(Value::String("[object Object]".to_string())),
        }
    });
    let string_ctor_val = Value::BuiltinObject(string_ctor_fn.clone(), Rc::clone(&string_ctor_map));
    string_proto_map.borrow_mut().properties.insert(
        PKey::String("constructor".to_string()),
        PropertyDescriptor::data(string_ctor_val.clone()),
    );
    string_proto_map.borrow_mut().properties.insert(
        PKey::String("indexOf".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(|vm, args| {
                let receiver = args.first().cloned().unwrap_or(Value::Undefined);
                let haystack = vm.to_string(&receiver)?;
                let needle = vm.to_string(args.get(1).unwrap_or(&Value::Undefined))?;
                let len_chars = haystack.chars().count();
                let start = if let Some(pos) = args.get(2) {
                    let n = vm.to_number_value(pos)?;
                    if n.is_nan() || n <= 0.0 {
                        0usize
                    } else {
                        (n.floor() as usize).min(len_chars)
                    }
                } else {
                    0usize
                };
                if needle.is_empty() {
                    return Ok(Value::Number(start as f64));
                }
                let start_byte = if start == 0 {
                    0usize
                } else {
                    haystack
                        .char_indices()
                        .nth(start)
                        .map_or(haystack.len(), |(idx, _)| idx)
                };
                if let Some(rel_byte) = haystack[start_byte..].find(&needle) {
                    let absolute_byte = start_byte + rel_byte;
                    let absolute_char = haystack[..absolute_byte].chars().count();
                    return Ok(Value::Number(absolute_char as f64));
                }
                Ok(Value::Number(-1.0))
            }),
            &function_proto_map,
        )),
    );

    let error_ctor_fn: BuiltinFn = Rc::new(move |vm, args| {
        let ctor = args
            .last()
            .cloned()
            .filter(|v| {
                matches!(
                    v,
                    Value::BuiltinObject(_, _) | Value::Closure { .. } | Value::Builtin(_)
                )
            })
            .or_else(|| vm.get_global("Error"))
            .unwrap_or(Value::Undefined);
        let proto = vm
            .get_property_host(&ctor, &PKey::String("prototype".to_string()))
            .unwrap_or(Value::Null);
        let mut inst = ObjectData::new();
        inst.prototype = proto;
        if let Some(message) = args.get(1).cloned() {
            inst.properties.insert(
                PKey::String("message".to_string()),
                PropertyDescriptor::data(message),
            );
        }
        inst.properties.insert(
            PKey::String("constructor".to_string()),
            PropertyDescriptor::data(ctor),
        );
        Ok(Value::Object(Rc::new(RefCell::new(inst))))
    });
    let error_ctor_val = Value::BuiltinObject(error_ctor_fn.clone(), Rc::clone(&error_ctor_map));
    error_ctor_map.borrow_mut().properties.insert(
        PKey::String("name".to_string()),
        PropertyDescriptor::data(Value::String("Error".to_string())),
    );
    error_proto_map.borrow_mut().properties.insert(
        PKey::String("constructor".to_string()),
        PropertyDescriptor::data(error_ctor_val.clone()),
    );
    error_proto_map.borrow_mut().properties.insert(
        PKey::String("name".to_string()),
        PropertyDescriptor::data(Value::String("Error".to_string())),
    );

    let aggregate_error_ctor_fn: BuiltinFn = Rc::new(move |vm, args| {
        let ctor = args
            .last()
            .cloned()
            .filter(|v| {
                matches!(
                    v,
                    Value::BuiltinObject(_, _) | Value::Closure { .. } | Value::Builtin(_)
                )
            })
            .or_else(|| vm.get_global("AggregateError"))
            .unwrap_or(Value::Undefined);
        let proto = vm
            .get_property_host(&ctor, &PKey::String("prototype".to_string()))
            .unwrap_or(Value::Null);
        let mut inst = ObjectData::new();
        inst.prototype = proto;
        if let Some(message) = args.get(2).cloned() {
            inst.properties.insert(
                PKey::String("message".to_string()),
                PropertyDescriptor::data(message),
            );
        }
        inst.properties.insert(
            PKey::String("constructor".to_string()),
            PropertyDescriptor::data(ctor),
        );
        Ok(Value::Object(Rc::new(RefCell::new(inst))))
    });
    let aggregate_error_ctor_val = Value::BuiltinObject(
        aggregate_error_ctor_fn.clone(),
        Rc::clone(&aggregate_error_ctor_map),
    );
    aggregate_error_ctor_map.borrow_mut().properties.insert(
        PKey::String("name".to_string()),
        PropertyDescriptor::data(Value::String("AggregateError".to_string())),
    );
    aggregate_error_proto_map.borrow_mut().properties.insert(
        PKey::String("constructor".to_string()),
        PropertyDescriptor::data(aggregate_error_ctor_val.clone()),
    );
    aggregate_error_proto_map.borrow_mut().properties.insert(
        PKey::String("name".to_string()),
        PropertyDescriptor::data(Value::String("AggregateError".to_string())),
    );

    let make_error_ctor = |name: &str, proto_map: &Rc<RefCell<ObjectData>>| {
        let ctor_props = Rc::new(RefCell::new(ObjectData::with_prototype(Value::Object(
            Rc::clone(&function_proto_map),
        ))));
        ctor_props.borrow_mut().properties.insert(
            PKey::String("prototype".to_string()),
            PropertyDescriptor::data(Value::Object(Rc::clone(proto_map))),
        );
        let ctor_name = name.to_string();
        let ctor_fn: BuiltinFn = Rc::new(move |vm, args| {
            let ctor = args
                .last()
                .cloned()
                .filter(|v| {
                    matches!(
                        v,
                        Value::BuiltinObject(_, _) | Value::Closure { .. } | Value::Builtin(_)
                    )
                })
                .or_else(|| vm.get_global(&ctor_name))
                .unwrap_or(Value::Undefined);
            let proto = vm
                .get_property_host(&ctor, &PKey::String("prototype".to_string()))
                .unwrap_or(Value::Null);
            let mut inst = ObjectData::new();
            inst.prototype = proto;
            if let Some(message) = args.get(1).cloned() {
                inst.properties.insert(
                    PKey::String("message".to_string()),
                    PropertyDescriptor::data(message),
                );
            }
            inst.properties.insert(
                PKey::String("constructor".to_string()),
                PropertyDescriptor::data(ctor),
            );
            Ok(Value::Object(Rc::new(RefCell::new(inst))))
        });
        let ctor_val = Value::BuiltinObject(ctor_fn, Rc::clone(&ctor_props));
        ctor_props.borrow_mut().properties.insert(
            PKey::String("name".to_string()),
            PropertyDescriptor::data(Value::String(name.to_string())),
        );
        proto_map.borrow_mut().properties.insert(
            PKey::String("constructor".to_string()),
            PropertyDescriptor::data(ctor_val.clone()),
        );
        proto_map.borrow_mut().properties.insert(
            PKey::String("name".to_string()),
            PropertyDescriptor::data(Value::String(name.to_string())),
        );
        (name.to_string(), ctor_val)
    };

    let eval_error_proto_map = Rc::new(RefCell::new(ObjectData::with_prototype(Value::Object(
        Rc::clone(&error_proto_map),
    ))));
    let range_error_proto_map = Rc::new(RefCell::new(ObjectData::with_prototype(Value::Object(
        Rc::clone(&error_proto_map),
    ))));
    let reference_error_proto_map = Rc::new(RefCell::new(ObjectData::with_prototype(
        Value::Object(Rc::clone(&error_proto_map)),
    )));
    let syntax_error_proto_map = Rc::new(RefCell::new(ObjectData::with_prototype(Value::Object(
        Rc::clone(&error_proto_map),
    ))));
    let type_error_proto_map = Rc::new(RefCell::new(ObjectData::with_prototype(Value::Object(
        Rc::clone(&error_proto_map),
    ))));
    let uri_error_proto_map = Rc::new(RefCell::new(ObjectData::with_prototype(Value::Object(
        Rc::clone(&error_proto_map),
    ))));

    let extra_error_ctors = vec![
        make_error_ctor("EvalError", &eval_error_proto_map),
        make_error_ctor("RangeError", &range_error_proto_map),
        make_error_ctor("ReferenceError", &reference_error_proto_map),
        make_error_ctor("SyntaxError", &syntax_error_proto_map),
        make_error_ctor("TypeError", &type_error_proto_map),
        make_error_ctor("URIError", &uri_error_proto_map),
    ];

    let math = Rc::new(RefCell::new(ObjectData::new()));
    math.borrow_mut().properties.insert(
        PKey::String("pow".to_string()),
        PropertyDescriptor::data({
            let builtin = builtin_object(
                Rc::new(|vm, args| {
                    let x = vm.to_number_value(args.get(1).unwrap_or(&Value::Undefined))?;
                    let y = vm.to_number_value(args.get(2).unwrap_or(&Value::Undefined))?;
                    Ok(Value::Number(x.powf(y)))
                }),
                &function_proto_map,
            );
            set_builtin_length(&builtin, 2.0);
            builtin
        }),
    );

    let reflect = Rc::new(RefCell::new(ObjectData::new()));
    reflect.borrow_mut().properties.insert(
        PKey::String("construct".to_string()),
        PropertyDescriptor::data(Value::BuiltinObject(
            Rc::new(|vm, args| {
                let target = args.get(1).cloned().unwrap_or(Value::Undefined);
                let arguments_list = args.get(2).cloned().unwrap_or(Value::Undefined);
                let new_target = args.get(3).cloned().unwrap_or_else(|| target.clone());

                if !target.is_constructable() {
                    let err =
                        vm.create_intrinsic_error("TypeError", "target is not a constructor")?;
                    return Err(vm.throw_exception(err));
                }
                if !new_target.is_constructable() {
                    let err =
                        vm.create_intrinsic_error("TypeError", "newTarget is not a constructor")?;
                    return Err(vm.throw_exception(err));
                }

                let mut args_vec = Vec::new();
                match &arguments_list {
                    Value::Array(arr, _) => {
                        for item in arr.borrow().iter().cloned() {
                            if let Some(v) = item {
                                args_vec.push(v);
                            } else {
                                args_vec.push(Value::Undefined);
                            }
                        }
                    }
                    other => {
                        let len_val = vm
                            .get_property_host(other, &PropertyKey::String("length".to_string()))?;
                        let len = vm.to_number_value(&len_val)?.max(0.0) as usize;
                        for idx in 0..len {
                            let item =
                                vm.get_property_host(other, &PropertyKey::String(idx.to_string()))?;
                            args_vec.push(item);
                        }
                    }
                }

                vm.construct_value(&target, &args_vec)
            }),
            Rc::new(RefCell::new(ObjectData::with_prototype(Value::Object(
                Rc::clone(&function_proto_map),
            )))),
        )),
    );
    reflect.borrow_mut().properties.insert(
        PKey::String("ownKeys".to_string()),
        PropertyDescriptor::data(Value::BuiltinObject(
            Rc::new(|_vm, args| {
                let target = args.first().cloned().unwrap_or(Value::Undefined);
                let keys = own_property_keys(&target)
                    .into_iter()
                    .map(|key| property_key_to_value(&key))
                    .collect::<Vec<_>>();
                Ok(array_from_values(keys))
            }),
            Rc::new(RefCell::new(ObjectData::with_prototype(Value::Object(
                Rc::clone(&function_proto_map),
            )))),
        )),
    );

    let mut g = globals.borrow_mut();
    // Ensure a `globalThis` object exists. If host provided one leave it
    // intact; otherwise create a fresh object and publish it.
    let global_this_key = PKey::String("globalThis".to_string());
    if !g.properties.contains_key(&global_this_key) {
        g.properties.insert(
            global_this_key.clone(),
            PropertyDescriptor::data(Value::Object(Rc::new(RefCell::new(ObjectData::new())))),
        );
    }
    // ECMAScript global constants (only insert if not already present)
    g.properties
        .entry(PKey::String("NaN".to_string()))
        .or_insert(PropertyDescriptor::data(Value::Number(f64::NAN)));
    g.properties
        .entry(PKey::String("Infinity".to_string()))
        .or_insert(PropertyDescriptor::data(Value::Number(f64::INFINITY)));
    g.properties.insert(
        PKey::String("eval".to_string()),
        PropertyDescriptor::data(Value::Builtin(Rc::new(move |vm, args| {
            let src = match args.first() {
                Some(Value::String(s)) => s.clone(),
                Some(other) => return Ok(other.clone()),
                None => return Ok(Value::Undefined),
            };
            let session = Parser::parse_source(&src)
                .map_err(|e| crate::errors::RuntimeError::from(format!("eval parse: {}", e)))?;
            let prog = session.program();
            let chunk = compiler::compile_oxc(prog)
                .map_err(|e| crate::errors::RuntimeError::from(format!("eval compile: {}", e)))?;
            let mut eval_vm = crate::vm::Vm::new(vm.globals_rc());
            eval_vm.run(chunk)
        }))),
    );
    g.properties.insert(
        PKey::String("Reflect".to_string()),
        PropertyDescriptor::data(Value::Object(reflect_obj)),
    );
    // Add kernel helpers if missing, prefer host-provided values when present.
    let mut insert_if_missing = |name: &str, val: Value| {
        let k = PKey::String(name.to_string());
        g.properties
            .entry(k)
            .or_insert_with(|| PropertyDescriptor::data(val));
    };

    insert_if_missing(
        "__native_in",
        Value::Builtin(Rc::new(move |vm, args| {
            let key = args.first().cloned().unwrap_or(Value::Undefined);
            let target = args.get(1).cloned().unwrap_or(Value::Undefined);
            let key = vm.to_property_key(&key)?;
            Ok(Value::Bool(vm.has_property_host(&target, &key)?))
        })),
    );

    insert_if_missing(
        "__native_construct",
        Value::Builtin(Rc::new(move |vm, args| {
            let target = args.first().cloned().unwrap_or(Value::Undefined);
            let ctor_args: Vec<Value> = args.iter().skip(1).cloned().collect();
            vm.construct_value(&target, &ctor_args)
        })),
    );

    insert_if_missing(
        "__native_call_spread",
        Value::Builtin(Rc::new(move |vm, args| {
            let callee = args.first().cloned().unwrap_or(Value::Undefined);
            let this_arg = args.get(1).cloned().unwrap_or(Value::Undefined);
            let values = args.get(2).cloned().unwrap_or(Value::Undefined);
            let mask = args.get(3).cloned().unwrap_or(Value::Undefined);

            let values_vec = match values {
                Value::Array(arr, _) => arr.borrow().clone(),
                _ => Vec::new(),
            };
            let mask_vec = match mask {
                Value::Array(arr, _) => arr.borrow().clone(),
                _ => Vec::new(),
            };

            let mut flat: Vec<Option<Value>> = Vec::new();
            for (i, value) in values_vec.into_iter().enumerate() {
                let spread = mask_vec
                    .get(i)
                    .and_then(|v| v.as_ref())
                    .is_some_and(|v| vm.to_boolean(v));
                if !spread {
                    flat.push(value);
                    continue;
                }

                match value {
                    Some(Value::Array(arr, _)) => {
                        flat.extend(arr.borrow().iter().cloned());
                    }
                    other => {
                        let len_val = vm.get_property_host(
                            other.as_ref().unwrap_or(&Value::Undefined),
                            &PropertyKey::String("length".to_string()),
                        )?;
                        let len = vm.to_number_value(&len_val)?.max(0.0) as usize;
                        for idx in 0..len {
                            let item = vm.get_property_host(
                                other.as_ref().unwrap_or(&Value::Undefined),
                                &PropertyKey::String(idx.to_string()),
                            )?;
                            flat.push(Some(item));
                        }
                    }
                }
            }

            vm.call_value_with_this(
                &callee,
                &this_arg,
                &flat.iter().filter_map(Clone::clone).collect::<Vec<_>>(),
            )
        })),
    );

    insert_if_missing(
        "__native_typeof",
        Value::Builtin(Rc::new(move |_vm, args| {
            let value = args.first().cloned().unwrap_or(Value::Undefined);
            let ty = match value {
                Value::Undefined => "undefined",
                Value::Null => "object",
                Value::Bool(_) => "boolean",
                Value::Number(_) => "number",
                Value::String(_) => "string",
                Value::BigInt(_) => "bigint",
                Value::Symbol(_, _) => "symbol",
                Value::Builtin(_) | Value::BuiltinObject(_, _) | Value::Closure { .. } => {
                    "function"
                }
                Value::Object(_) | Value::Array(..) | Value::Promise(_) => "object",
            };
            Ok(Value::String(ty.to_string()))
        })),
    );
    g.properties.insert(
        PKey::String("Function".to_string()),
        PropertyDescriptor::data(function_ctor_val),
    );
    g.properties.insert(
        PKey::String("Object".to_string()),
        PropertyDescriptor::data(object_ctor_val),
    );
    g.properties.insert(
        PKey::String("Array".to_string()),
        PropertyDescriptor::data(array_ctor_val),
    );
    g.properties.insert(
        PKey::String("String".to_string()),
        PropertyDescriptor::data(string_ctor_val),
    );
    g.properties.insert(
        PKey::String("Symbol".to_string()),
        PropertyDescriptor::data(symbol_ctor_val),
    );
    g.properties.insert(
        PKey::String("Promise".to_string()),
        PropertyDescriptor::data(promise_ctor_val),
    );
    g.properties.insert(
        PKey::String("Date".to_string()),
        PropertyDescriptor::data(date_ctor_val),
    );
    g.properties.insert(
        PKey::String("ArrayBuffer".to_string()),
        PropertyDescriptor::data(arraybuffer_ctor_val),
    );
    g.properties.insert(
        PKey::String("Uint8Array".to_string()),
        PropertyDescriptor::data(uint8array_ctor_val),
    );
    g.properties.insert(
        PKey::String("Int8Array".to_string()),
        PropertyDescriptor::data(int8array_ctor_val),
    );
    g.properties.insert(
        PKey::String("Uint16Array".to_string()),
        PropertyDescriptor::data(uint16array_ctor_val),
    );
    g.properties.insert(
        PKey::String("Int16Array".to_string()),
        PropertyDescriptor::data(int16array_ctor_val),
    );
    g.properties.insert(
        PKey::String("Uint32Array".to_string()),
        PropertyDescriptor::data(uint32array_ctor_val),
    );
    g.properties.insert(
        PKey::String("Int32Array".to_string()),
        PropertyDescriptor::data(int32array_ctor_val),
    );
    g.properties.insert(
        PKey::String("Float32Array".to_string()),
        PropertyDescriptor::data(float32array_ctor_val),
    );
    g.properties.insert(
        PKey::String("Float64Array".to_string()),
        PropertyDescriptor::data(float64array_ctor_val),
    );
    g.properties.insert(
        PKey::String("Uint8ClampedArray".to_string()),
        PropertyDescriptor::data(uint8clampedarray_ctor_val),
    );
    g.properties.insert(
        PKey::String("Map".to_string()),
        PropertyDescriptor::data(map_ctor_val),
    );

    g.properties.insert(
        PKey::String("Error".to_string()),
        PropertyDescriptor::data(error_ctor_val),
    );
    g.properties.insert(
        PKey::String("AggregateError".to_string()),
        PropertyDescriptor::data(aggregate_error_ctor_val),
    );
    for (name, ctor) in extra_error_ctors {
        g.properties
            .insert(PKey::String(name), PropertyDescriptor::data(ctor));
    }
    g.properties.insert(
        PKey::String("Math".to_string()),
        PropertyDescriptor::data(Value::Object(math)),
    );

    // Number constructor with static properties
    let number_obj = Rc::new(RefCell::new(ObjectData::new()));
    number_obj.borrow_mut().properties.insert(
        PKey::String("NaN".to_string()),
        PropertyDescriptor::data(Value::Number(f64::NAN)),
    );
    number_obj.borrow_mut().properties.insert(
        PKey::String("POSITIVE_INFINITY".to_string()),
        PropertyDescriptor::data(Value::Number(f64::INFINITY)),
    );
    number_obj.borrow_mut().properties.insert(
        PKey::String("NEGATIVE_INFINITY".to_string()),
        PropertyDescriptor::data(Value::Number(f64::NEG_INFINITY)),
    );
    number_obj.borrow_mut().properties.insert(
        PKey::String("MAX_VALUE".to_string()),
        PropertyDescriptor::data(Value::Number(f64::MAX)),
    );
    number_obj.borrow_mut().properties.insert(
        PKey::String("MIN_VALUE".to_string()),
        PropertyDescriptor::data(Value::Number(f64::MIN_POSITIVE)),
    );
    g.properties.insert(
        PKey::String("Number".to_string()),
        PropertyDescriptor::data(Value::Object(number_obj)),
    );

    let _reflect = Rc::new(RefCell::new(ObjectData::new()));

    // Ensure a few additional helpers the soft prelude expects. Do not
    // override host-provided values.
    let mut insert_if_missing_global = |name: &str, v: Value| {
        let k = PKey::String(name.to_string());
        g.properties
            .entry(k)
            .or_insert_with(|| PropertyDescriptor::data(v));
    };

    insert_if_missing_global(
        "__native_toString",
        Value::Builtin(Rc::new(|vm, args| {
            let v = args.first().cloned().unwrap_or(Value::Undefined);
            let s = vm.to_string(&v)?;
            Ok(Value::String(s))
        })),
    );
    insert_if_missing_global(
        "__native_toNumber",
        Value::Builtin(Rc::new(|vm, args| {
            let v = args.first().cloned().unwrap_or(Value::Undefined);
            let n = vm.to_number_value(&v)?;
            Ok(Value::Number(n))
        })),
    );
    insert_if_missing_global(
        "__native_toBoolean",
        Value::Builtin(Rc::new(|vm, args| {
            let v = args.first().cloned().unwrap_or(Value::Undefined);
            Ok(Value::Bool(vm.to_boolean(&v)))
        })),
    );
    insert_if_missing_global(
        "__native_abstractEquality",
        Value::Builtin(Rc::new(|vm, args| {
            let a = args.first().cloned().unwrap_or(Value::Undefined);
            let b = args.get(1).cloned().unwrap_or(Value::Undefined);
            Ok(Value::Bool(vm.abstract_equality(&a, &b)?))
        })),
    );
    insert_if_missing_global(
        "__native_instanceof",
        Value::Builtin(Rc::new(|vm, args| {
            let value = args.first().cloned().unwrap_or(Value::Undefined);
            let ctor = args.get(1).cloned().unwrap_or(Value::Undefined);
            Ok(Value::Bool(vm.instance_of(&value, &ctor)?))
        })),
    );
    insert_if_missing_global(
        "__native_inProperty",
        Value::Builtin(Rc::new(|vm, args| {
            let prop = args.first().cloned().unwrap_or(Value::Undefined);
            let obj = args.get(1).cloned().unwrap_or(Value::Undefined);
            Ok(Value::Bool(vm.in_property(&prop, &obj)?))
        })),
    );

    insert_if_missing_global(
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

    insert_if_missing_global(
        "__native_forInKeys",
        Value::Builtin(Rc::new(|vm, args| {
            let mut current = args.first().cloned().unwrap_or(Value::Undefined);
            if matches!(current, Value::Undefined | Value::Null) {
                return Err(crate::errors::RuntimeError::from(
                    "Cannot convert undefined or null to object",
                ));
            }

            let mut out: Vec<Option<Value>> = Vec::new();
            let mut seen: HashSet<String> = HashSet::new();
            let mut depth = 0usize;
            while depth < 128 {
                for key in own_enumerable_keys(&current) {
                    if let PropertyKey::String(name) = key
                        && seen.insert(name.clone())
                    {
                        out.push(Some(Value::String(name)));
                    }
                }

                let next = match &current {
                    Value::Object(obj) => obj.borrow().prototype.clone(),
                    Value::Array(_, props)
                    | Value::Closure { props, .. }
                    | Value::BuiltinObject(_, props) => props.borrow().prototype.clone(),
                    Value::String(_) => vm
                        .get_global("String")
                        .and_then(|ctor| {
                            vm.get_property_host(
                                &ctor,
                                &PropertyKey::String("prototype".to_string()),
                            )
                            .ok()
                        })
                        .unwrap_or(Value::Null),
                    Value::Symbol(_, _) => vm
                        .get_global("Symbol")
                        .and_then(|ctor| {
                            vm.get_property_host(
                                &ctor,
                                &PropertyKey::String("prototype".to_string()),
                            )
                            .ok()
                        })
                        .unwrap_or(Value::Null),
                    _ => Value::Null,
                };
                if matches!(next, Value::Null | Value::Undefined) {
                    break;
                }
                current = next;
                depth += 1;
            }

            let arr_rc = Rc::new(RefCell::new(out));
            let props = Rc::new(RefCell::new(ObjectData::new()));
            props.borrow_mut().properties.insert(
                PropertyKey::String("length".to_string()),
                PropertyDescriptor::data(Value::Number(arr_rc.borrow().len() as f64)),
            );
            Ok(Value::Array(arr_rc, props))
        })),
    );

    insert_if_missing_global(
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

    insert_if_missing_global(
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
                "<<" => {
                    let l = vm.to_int32_value(&lhs)?;
                    let r = vm.to_uint32_value(&rhs)? & 0x1f;
                    Value::Number(l.wrapping_shl(r) as f64)
                }
                ">>" => {
                    let l = vm.to_int32_value(&lhs)?;
                    let r = vm.to_uint32_value(&rhs)? & 0x1f;
                    Value::Number(l.wrapping_shr(r) as f64)
                }
                ">>>" => {
                    let l = vm.to_uint32_value(&lhs)?;
                    let r = vm.to_uint32_value(&rhs)? & 0x1f;
                    Value::Number(l.wrapping_shr(r) as f64)
                }
                _ => {
                    return Err(crate::errors::RuntimeError::from(
                        "unsupported compound assignment operator",
                    ));
                }
            };

            let _ = vm.set_property_host(&target, &key_value, out.clone())?;
            Ok(out)
        })),
    );

    // Mirror a selection of helpers onto globalThis if not present there.
    // Special handling needed when globalThis is the same object as globals
    let names = [
        "__native_typeof",
        "__native_toString",
        "__native_toNumber",
        "__native_toBoolean",
        "__native_abstractEquality",
        "__native_instanceof",
        "__native_in",
        "__native_construct",
    ];

    // Get the globalThis value and check if it points back to globals
    if let Some(PropertyDescriptor::Data { value: gv, .. }) =
        g.properties.get(&global_this_key).cloned()
        && let Value::Object(obj_rc) = gv
    {
        // Check if obj_rc is the same as globals by comparing Rc pointers
        let is_globals = Rc::ptr_eq(&obj_rc, globals);

        if is_globals {
            // If globalThis IS globals, we need to be careful with borrows
            // Drop the mutable borrow of g first
            drop(g);

            // Now borrow g mutably again to insert the missing properties
            let g = globals.borrow_mut();
            for name in &names {
                let k = PropertyKey::String(name.to_string());
                if !g.properties.contains_key(&k) {
                    // Get the value from globals itself - but we need to avoid double-borrow
                    // This is handled by cloning what we need first
                    // Actually, since both keys are in the same map (g), we can just skip
                    // if the key already exists (which we checked above)
                }
            }
        } else {
            // obj_rc is a separate object, safe to borrow both
            drop(g); // Drop mutable borrow

            let g = globals.borrow(); // Borrow immutably
            let mut obj = obj_rc.borrow_mut();
            for name in &names {
                let k = PropertyKey::String(name.to_string());
                if !obj.properties.contains_key(&k)
                    && let Some(PropertyDescriptor::Data { value, .. }) =
                        g.properties.get(&PKey::String(name.to_string())).cloned()
                {
                    obj.properties.insert(k, PropertyDescriptor::data(value));
                }
            }
            // g and obj borrows are dropped here
        }
    }
}
