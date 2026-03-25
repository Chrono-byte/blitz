use std::cell::RefCell;
use std::collections::HashMap;
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
                && let Some(value) = arr.borrow().get(idx as usize).cloned()
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
            for idx in 0..arr.borrow().len() {
                keys.push(PropertyKey::String(idx.to_string()));
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
            let items: Vec<Value> = match &this {
                Value::Array(arr, _) => arr.borrow().clone(),
                _ => Vec::new(),
            };
            for (i, item) in items.iter().enumerate() {
                let idx = Value::Number(i as f64);
                let _ = vm.call_value_with_this(&callback, &this_for_call, &[item.clone(), idx, this_for_call.clone()]);
            }
            Ok(Value::Undefined)
        }),
        &function_proto_map,
    );
    array_proto_map.borrow_mut().properties.insert(
        PKey::String("forEach".to_string()),
        PropertyDescriptor::data(for_each_fn.clone()),
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
                let items: Vec<Value> = match this {
                    Value::Array(arr, _) => arr.borrow().clone(),
                    _ => Vec::new(),
                };
                let mut out = String::new();
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push_str(&sep);
                    }
                    match item {
                        Value::Undefined | Value::Null => {}
                        v => out.push_str(&vm.to_string(v)?),
                    }
                }
                Ok(Value::String(out))
            }),
            &function_proto_map,
        )),
    );
    array_proto_map.borrow_mut().properties.insert(
        PKey::String("push".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(|_vm, args| {
                let this = args.first().cloned().unwrap_or(Value::Undefined);
                let mut items = match this {
                    Value::Array(arr, _) => arr,
                    _ => return Ok(Value::Number(0.0)),
                };
                let mut vec = items.borrow_mut();
                for v in args.iter().skip(1).cloned() {
                    vec.push(v);
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
    let function_ctor_fn: BuiltinFn =
        Rc::new(move |_vm, _args| Ok(make_host_function_instance(&function_ctor_proto)));
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
                    Some(Value::Array(arr, _)) => arr.borrow().clone(),
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
                let function_call_val = function_call_val.clone();
                move |_vm, args| {
                    let target = args.first().cloned().unwrap_or(Value::Undefined);
                    let bound_this = args.get(1).cloned().unwrap_or(Value::Undefined);
                    let bound_args: Vec<Value> = args.iter().skip(2).cloned().collect();
                    if crate::trace::enabled() {
                        eprintln!("[trace] bind create target={:?} bound_this={:?} bound_args={:?}", target, bound_this, bound_args);
                    }
                    let bound_target = target;
                    let bound_this_arg = bound_this;
                    let prefix_args = bound_args;
                    Ok(builtin_object(
                        Rc::new(move |vm, call_args| {
                    if crate::trace::enabled() {
                        eprintln!("[trace] bound fn call target={:?} this={:?} args={:?}", bound_target, bound_this_arg, call_args);
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
    let symbol_ctor_fn: BuiltinFn = Rc::new(move |_vm, args| {
        let desc = match args.get(1) {
            Some(Value::String(s)) => Some(s.clone()),
            Some(Value::Undefined) | None => None,
            Some(v) => Some(v.type_name().to_string()),
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
    symbol_ctor_map.borrow_mut().properties.insert(
        PKey::String("iterator".to_string()),
        PropertyDescriptor::data(Value::Symbol(SymbolId::well_known_iterator(), Some("Symbol.iterator".to_string()))),
    );
    symbol_ctor_map.borrow_mut().properties.insert(
        PKey::String("asyncIterator".to_string()),
        PropertyDescriptor::data(Value::Symbol(SymbolId::well_known_async_iterator(), Some("Symbol.asyncIterator".to_string()))),
    );
    symbol_ctor_map.borrow_mut().properties.insert(
        PKey::String("toPrimitive".to_string()),
        PropertyDescriptor::data(Value::Symbol(SymbolId::well_known_to_primitive(), Some("Symbol.toPrimitive".to_string()))),
    );
    symbol_ctor_map.borrow_mut().properties.insert(
        PKey::String("toStringTag".to_string()),
        PropertyDescriptor::data(Value::Symbol(SymbolId::well_known_to_string_tag(), Some("Symbol.toStringTag".to_string()))),
    );
    symbol_ctor_map.borrow_mut().properties.insert(
        PKey::String("species".to_string()),
        PropertyDescriptor::data(Value::Symbol(SymbolId::well_known_species(), Some("Symbol.species".to_string()))),
    );
    symbol_ctor_map.borrow_mut().properties.insert(
        PKey::String("hasInstance".to_string()),
        PropertyDescriptor::data(Value::Symbol(SymbolId::well_known_has_instance(), Some("Symbol.hasInstance".to_string()))),
    );
    symbol_ctor_map.borrow_mut().properties.insert(
        PKey::String("isConcatSpreadable".to_string()),
        PropertyDescriptor::data(Value::Symbol(SymbolId::well_known_is_concat_spreadable(), Some("Symbol.isConcatSpreadable".to_string()))),
    );
    symbol_ctor_map.borrow_mut().properties.insert(
        PKey::String("match".to_string()),
        PropertyDescriptor::data(Value::Symbol(SymbolId::well_known_match(), Some("Symbol.match".to_string()))),
    );
    symbol_ctor_map.borrow_mut().properties.insert(
        PKey::String("matchAll".to_string()),
        PropertyDescriptor::data(Value::Symbol(SymbolId::well_known_match_all(), Some("Symbol.matchAll".to_string()))),
    );
    symbol_ctor_map.borrow_mut().properties.insert(
        PKey::String("replace".to_string()),
        PropertyDescriptor::data(Value::Symbol(SymbolId::well_known_replace(), Some("Symbol.replace".to_string()))),
    );
    symbol_ctor_map.borrow_mut().properties.insert(
        PKey::String("search".to_string()),
        PropertyDescriptor::data(Value::Symbol(SymbolId::well_known_search(), Some("Symbol.search".to_string()))),
    );
    symbol_ctor_map.borrow_mut().properties.insert(
        PKey::String("split".to_string()),
        PropertyDescriptor::data(Value::Symbol(SymbolId::well_known_split(), Some("Symbol.split".to_string()))),
    );
    symbol_ctor_map.borrow_mut().properties.insert(
        PKey::String("unscopables".to_string()),
        PropertyDescriptor::data(Value::Symbol(SymbolId::well_known_unscopables(), Some("Symbol.unscopables".to_string()))),
    );
    object_proto_map.borrow_mut().properties.insert(
        PKey::String("constructor".to_string()),
        PropertyDescriptor::data(object_ctor_val.clone()),
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
                    let proto = args.get(1).cloned().unwrap_or(Value::Undefined);
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
                    if let Some(props) = args.get(2)
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
            Rc::new(|_vm, args| {
                let left = args.get(1).cloned().unwrap_or(Value::Undefined);
                let right = args.get(2).cloned().unwrap_or(Value::Undefined);
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
                    let target = args.get(1).cloned().unwrap_or(Value::Undefined);
                    let key_value = args.get(2).cloned().unwrap_or(Value::Undefined);
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
        PKey::String("getOwnPropertyNames".to_string()),
        PropertyDescriptor::data({
            let builtin = builtin_object(
                Rc::new(|_vm, args| {
                    let target = args.get(1).cloned().unwrap_or(Value::Undefined);
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
                        names.into_iter().map(Value::String).collect::<Vec<_>>(),
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
                if args.len() < 4 {
                    return Ok(Value::Undefined);
                }
                let target = args[1].clone();
                if crate::trace::enabled() {
                    eprintln!("[trace] Object.defineProperty target={:?} key={:?} desc={:?}", target, args.get(2), args.get(3));
                }
                let ok = vm.define_property_value(&target, &args[2], &args[3])?;
                if ok {
                    Ok(target)
                } else {
                    Err(crate::errors::RuntimeError::from(
                        "Object.defineProperty failed",
                    ))
                }
            }),
            &function_proto_map,
        )),
    );
    object_ctor_map.borrow_mut().properties.insert(
        PKey::String("getPrototypeOf".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(|_vm, args| {
                let target = args.get(1).cloned().unwrap_or(Value::Undefined);
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
        let elems: Vec<Value> = args.to_vec();
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
        if args.is_empty() {
            return Ok(Value::String(String::new()));
        }
        match &args[0] {
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

    let error_ctor_props = Rc::clone(&error_ctor_map);
    let error_ctor_fn: BuiltinFn = Rc::new(move |_vm, args| {
        let proto = error_ctor_props
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
        if let Some(message) = args.get(1).cloned() {
            inst.properties.insert(
                PKey::String("message".to_string()),
                PropertyDescriptor::data(message),
            );
        }
        Ok(Value::Object(Rc::new(RefCell::new(inst))))
    });
    let error_ctor_val = Value::BuiltinObject(error_ctor_fn.clone(), Rc::clone(&error_ctor_map));
    error_proto_map.borrow_mut().properties.insert(
        PKey::String("constructor".to_string()),
        PropertyDescriptor::data(error_ctor_val.clone()),
    );
    error_proto_map.borrow_mut().properties.insert(
        PKey::String("name".to_string()),
        PropertyDescriptor::data(Value::String("Error".to_string())),
    );

    let aggregate_error_ctor_props = Rc::clone(&aggregate_error_ctor_map);
    let aggregate_error_ctor_fn: BuiltinFn = Rc::new(move |_vm, args| {
        let proto = aggregate_error_ctor_props
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
        if let Some(message) = args.get(2).cloned() {
            inst.properties.insert(
                PKey::String("message".to_string()),
                PropertyDescriptor::data(message),
            );
        }
        Ok(Value::Object(Rc::new(RefCell::new(inst))))
    });
    let aggregate_error_ctor_val = Value::BuiltinObject(
        aggregate_error_ctor_fn.clone(),
        Rc::clone(&aggregate_error_ctor_map),
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
        let ctor_props_for_fn = Rc::clone(&ctor_props);
        let ctor_fn: BuiltinFn = Rc::new(move |_vm, args| {
            let proto = ctor_props_for_fn
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
            if let Some(message) = args.get(1).cloned() {
                inst.properties.insert(
                    PKey::String("message".to_string()),
                    PropertyDescriptor::data(message),
                );
            }
            Ok(Value::Object(Rc::new(RefCell::new(inst))))
        });
        let ctor_val = Value::BuiltinObject(ctor_fn, Rc::clone(&ctor_props));
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
                let new_target = args.get(3).cloned().unwrap_or_else(|| target.clone());
                let proto = vm.get_property_host(
                    &new_target,
                    &PropertyKey::String("prototype".to_string()),
                )?;
                let fallback_proto = match &new_target {
                    Value::Object(obj) => obj.borrow().prototype.clone(),
                    Value::Array(_, props) => props.borrow().prototype.clone(),
                    Value::Closure { props, .. } => props.borrow().prototype.clone(),
                    Value::BuiltinObject(_, props) => props.borrow().prototype.clone(),
                    _ => Value::Null,
                };
                let instance_proto = match proto {
                    Value::Object(_)
                    | Value::Array(..)
                    | Value::Closure { .. }
                    | Value::BuiltinObject(_, _) => proto,
                    _ => fallback_proto,
                };
                let props = Rc::new(RefCell::new(ObjectData::new()));
                props.borrow_mut().prototype = instance_proto;
                let callable =
                    Value::BuiltinObject(Rc::new(|_, _| Ok(Value::Undefined)), Rc::clone(&props));
                let fn_proto_obj = Rc::new(RefCell::new(ObjectData::new()));
                props.borrow_mut().properties.insert(
                    PropertyKey::String("prototype".to_string()),
                    PropertyDescriptor::data(Value::Object(Rc::clone(&fn_proto_obj))),
                );
                fn_proto_obj.borrow_mut().properties.insert(
                    PropertyKey::String("constructor".to_string()),
                    PropertyDescriptor::data(callable.clone()),
                );
                Ok(callable)
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
                Value::Object(_) | Value::Array(..) => "object",
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
    g.properties.insert(
        PKey::String("Reflect".to_string()),
        PropertyDescriptor::data(Value::Object(reflect)),
    );

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
            let target = args.get(1).cloned().unwrap_or(Value::Undefined);
            let key = args.get(2).cloned().unwrap_or(Value::Undefined);
            if crate::trace::enabled() {
                eprintln!("[trace] __native_deleteProperty target={:?} key={:?}", target, key);
            }
            let key = vm.to_property_key(&key)?;
            Ok(Value::Bool(vm.delete_property_host(&target, &key)?))
        })),
    );

    // Mirror a selection of helpers onto globalThis if not present there.
    if let Some(PropertyDescriptor::Data { value: gv, .. }) =
        g.properties.get(&global_this_key).cloned()
        && let Value::Object(obj_rc) = gv
    {
        let mut obj = obj_rc.borrow_mut();
        let mut mirror = |name: &str| {
            let k = PropertyKey::String(name.to_string());
            if !obj.properties.contains_key(&k)
                && let Some(PropertyDescriptor::Data { value, .. }) =
                    g.properties.get(&PKey::String(name.to_string())).cloned()
            {
                obj.properties.insert(k, PropertyDescriptor::data(value));
            }
        };
        for n in [
            "__native_typeof",
            "__native_toString",
            "__native_toNumber",
            "__native_toBoolean",
            "__native_abstractEquality",
            "__native_instanceof",
            "__native_in",
            "__native_construct",
        ] {
            mirror(n);
        }
    }
}
