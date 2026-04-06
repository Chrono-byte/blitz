use std::cell::RefCell;
use std::rc::Rc;

use num_traits::ToPrimitive;

use crate::runtime_utils::{
    builtin_object, char_code_at, char_index_to_byte_index, non_enum_data, set_builtin_length,
    set_builtin_name, usize_to_f64,
};
use crate::value::{BuiltinFn, ObjectData, PropertyDescriptor, PropertyKey, SymbolId, Value};
use crate::vm::Vm;

/// Installs the String, Boolean, and Symbol builtins into the globals object.
pub(crate) fn install_string_boolean_symbol_builtins(
    globals: &mut ObjectData,
    object_proto_map: &Rc<RefCell<ObjectData>>,
    function_proto_map: &Rc<RefCell<ObjectData>>,
) {
    use PropertyKey as PKey;

    let string_proto_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
        ObjectData::with_prototype(Value::Object(Rc::clone(object_proto_map))),
    ));
    let string_ctor_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
        ObjectData::with_prototype(Value::Object(Rc::clone(function_proto_map))),
    ));
    string_ctor_map.borrow_mut().properties.insert(
        PKey::String("prototype".to_string()),
        PropertyDescriptor::data(Value::Object(Rc::clone(&string_proto_map))),
    );

    let boolean_proto_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
        ObjectData::with_prototype(Value::Object(Rc::clone(object_proto_map))),
    ));
    boolean_proto_map.borrow_mut().properties.insert(
        PKey::String("[[PrimitiveValue]]".to_string()),
        PropertyDescriptor::data(Value::Bool(false)),
    );
    let boolean_ctor_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
        ObjectData::with_prototype(Value::Object(Rc::clone(function_proto_map))),
    ));
    boolean_ctor_map.borrow_mut().properties.insert(
        PKey::String("prototype".to_string()),
        PropertyDescriptor::Data {
            value: Value::Object(Rc::clone(&boolean_proto_map)),
            writable: false,
            enumerable: false,
            configurable: false,
        },
    );

    let symbol_proto_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
        ObjectData::with_prototype(Value::Object(Rc::clone(object_proto_map))),
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
        ObjectData::with_prototype(Value::Object(Rc::clone(function_proto_map))),
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
            function_proto_map,
        )),
    );
    symbol_proto_map.borrow_mut().properties.insert(
        PKey::String("valueOf".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(|_vm, args| Ok(args.first().cloned().unwrap_or(Value::Undefined))),
            function_proto_map,
        )),
    );
    for (name, sym) in [
        (
            "iterator",
            Value::Symbol(
                SymbolId::well_known_iterator(),
                Some("Symbol.iterator".to_string()),
            ),
        ),
        (
            "asyncIterator",
            Value::Symbol(
                SymbolId::well_known_async_iterator(),
                Some("Symbol.asyncIterator".to_string()),
            ),
        ),
        (
            "toPrimitive",
            Value::Symbol(
                SymbolId::well_known_to_primitive(),
                Some("Symbol.toPrimitive".to_string()),
            ),
        ),
        (
            "toStringTag",
            Value::Symbol(
                SymbolId::well_known_to_string_tag(),
                Some("Symbol.toStringTag".to_string()),
            ),
        ),
        (
            "species",
            Value::Symbol(
                SymbolId::well_known_species(),
                Some("Symbol.species".to_string()),
            ),
        ),
        (
            "hasInstance",
            Value::Symbol(
                SymbolId::well_known_has_instance(),
                Some("Symbol.hasInstance".to_string()),
            ),
        ),
        (
            "isConcatSpreadable",
            Value::Symbol(
                SymbolId::well_known_is_concat_spreadable(),
                Some("Symbol.isConcatSpreadable".to_string()),
            ),
        ),
        (
            "match",
            Value::Symbol(
                SymbolId::well_known_match(),
                Some("Symbol.match".to_string()),
            ),
        ),
        (
            "matchAll",
            Value::Symbol(
                SymbolId::well_known_match_all(),
                Some("Symbol.matchAll".to_string()),
            ),
        ),
        (
            "replace",
            Value::Symbol(
                SymbolId::well_known_replace(),
                Some("Symbol.replace".to_string()),
            ),
        ),
        (
            "search",
            Value::Symbol(
                SymbolId::well_known_search(),
                Some("Symbol.search".to_string()),
            ),
        ),
        (
            "split",
            Value::Symbol(
                SymbolId::well_known_split(),
                Some("Symbol.split".to_string()),
            ),
        ),
        (
            "unscopables",
            Value::Symbol(
                SymbolId::well_known_unscopables(),
                Some("Symbol.unscopables".to_string()),
            ),
        ),
    ] {
        symbol_ctor_map.borrow_mut().properties.insert(
            PKey::String(name.to_string()),
            PropertyDescriptor::data(sym),
        );
    }

    let string_proto_for_ctor = Rc::clone(&string_proto_map);
    let string_ctor_fn: BuiltinFn = Rc::new(move |vm, args| {
        if vm.current_new_target().is_some() {
            let value = args.get(1).cloned().unwrap_or(Value::Undefined);
            let s = vm.to_string(&value)?;
            let mut obj = ObjectData::new();
            obj.set_prototype_value(Value::Object(Rc::clone(&string_proto_for_ctor)));
            let obj_rc = Rc::new(RefCell::new(obj));
            obj_rc.borrow_mut().properties.insert(
                PKey::String("[[PrimitiveValue]]".to_string()),
                PropertyDescriptor::data(Value::String(s)),
            );
            return Ok(Value::Object(obj_rc));
        }

        let value_idx = if args.len() > 1 { 1 } else { 0 };
        let value = args.get(value_idx);
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
            other => Ok(Value::String(vm.to_string(other)?)),
        }
    });
    let string_ctor_val = Value::BuiltinObject(string_ctor_fn.clone(), Rc::clone(&string_ctor_map));
    string_proto_map.borrow_mut().properties.insert(
        PKey::String("constructor".to_string()),
        PropertyDescriptor::data(string_ctor_val.clone()),
    );
    string_proto_map.borrow_mut().properties.insert(
        PKey::String("valueOf".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(|vm, args| {
                let this = args.first().cloned().unwrap_or(Value::Undefined);
                let res =
                    match this {
                        Value::String(s) => Value::String(s),
                        Value::Object(obj) => {
                            if let Some(PropertyDescriptor::Data { value, .. }) = obj
                                .borrow()
                                .properties
                                .get(&PKey::String("[[PrimitiveValue]]".to_string()))
                            {
                                match value {
                                    Value::String(s) => Value::String(s.clone()),
                                    _ => return Err(vm.throw_type_error(
                                        "String.prototype.valueOf called on incompatible receiver",
                                    )),
                                }
                            } else {
                                return Err(vm.throw_type_error(
                                    "String.prototype.valueOf called on incompatible receiver",
                                ));
                            }
                        }
                        _ => {
                            return Err(vm.throw_type_error(
                                "String.prototype.valueOf called on incompatible receiver",
                            ));
                        }
                    };
                Ok(res)
            }),
            function_proto_map,
        )),
    );
    string_proto_map.borrow_mut().properties.insert(
        PKey::String("toString".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(|vm, args| {
                let this = args.first().cloned().unwrap_or(Value::Undefined);
                match this {
                    Value::String(s) => Ok(Value::String(s)),
                    Value::Object(obj) => {
                        if let Some(PropertyDescriptor::Data { value, .. }) = obj
                            .borrow()
                            .properties
                            .get(&PKey::String("[[PrimitiveValue]]".to_string()))
                            && let Value::String(s) = value
                        {
                            return Ok(Value::String(s.clone()));
                        }
                        Err(vm.throw_type_error(
                            "String.prototype.toString called on incompatible receiver",
                        ))
                    }
                    _ => Err(vm.throw_type_error(
                        "String.prototype.toString called on incompatible receiver",
                    )),
                }
            }),
            function_proto_map,
        )),
    );
    let string_index_of = builtin_object(
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
                    n.floor().to_usize().unwrap_or(len_chars).min(len_chars)
                }
            } else {
                0usize
            };
            if needle.is_empty() {
                return Ok(Value::Number(usize_to_f64(start)));
            }
            let start_byte = char_index_to_byte_index(&haystack, start);
            if let Some(rel_byte) = haystack[start_byte..].find(&needle) {
                let absolute_byte = start_byte + rel_byte;
                let absolute_char = haystack[..absolute_byte].chars().count();
                return Ok(Value::Number(usize_to_f64(absolute_char)));
            }
            Ok(Value::Number(-1.0))
        }),
        function_proto_map,
    );
    set_builtin_name(&string_index_of, "indexOf");
    set_builtin_length(&string_index_of, 2.0);
    string_proto_map.borrow_mut().properties.insert(
        PKey::String("indexOf".to_string()),
        non_enum_data(string_index_of),
    );

    let string_char_code_at = builtin_object(
        Rc::new(|vm, args| {
            let receiver = args.first().cloned().unwrap_or(Value::Undefined);
            let s = vm.to_string(&receiver)?;
            let pos = match args.get(1) {
                Some(v) => vm.to_number_value(v)?,
                None => 0.0,
            };
            let index = if pos.is_nan() || pos <= 0.0 {
                0usize
            } else if !pos.is_finite() {
                return Ok(Value::Number(f64::NAN));
            } else {
                pos.trunc().to_usize().unwrap_or(0)
            };
            match char_code_at(&s, index) {
                Some(code) => Ok(Value::Number(f64::from(code))),
                None => Ok(Value::Number(f64::NAN)),
            }
        }),
        function_proto_map,
    );
    set_builtin_name(&string_char_code_at, "charCodeAt");
    set_builtin_length(&string_char_code_at, 1.0);
    string_proto_map.borrow_mut().properties.insert(
        PKey::String("charCodeAt".to_string()),
        non_enum_data(string_char_code_at),
    );

    let string_slice = builtin_object(
        Rc::new(|vm, args| {
            let receiver = args.first().cloned().unwrap_or(Value::Undefined);
            let s = vm.to_string(&receiver)?;
            let len = s.chars().count().to_isize().unwrap_or(isize::MAX);
            let to_int = |vm: &mut Vm, v: &Value| -> Result<isize, crate::errors::RuntimeError> {
                let n = vm.to_number_value(v)?;
                if !n.is_finite() {
                    return Ok(0);
                }
                Ok(n.trunc().to_isize().unwrap_or(0))
            };
            let start_raw = match args.get(1) {
                Some(v) => to_int(vm, v)?,
                None => 0,
            };
            let from = if start_raw < 0 {
                (len + start_raw).max(0)
            } else {
                start_raw.min(len)
            };
            let end_raw_opt = args.get(2).filter(|v| !matches!(v, Value::Undefined));
            let end_raw = match end_raw_opt {
                Some(v) => to_int(vm, v)?,
                None => len,
            };
            let to = if end_raw < 0 {
                (len + end_raw).max(0)
            } else {
                end_raw.min(len)
            };
            if to <= from {
                return Ok(Value::String(String::new()));
            }
            let start_byte = char_index_to_byte_index(&s, usize::try_from(from).unwrap_or(0));
            let end_byte = char_index_to_byte_index(&s, usize::try_from(to).unwrap_or(0));
            Ok(Value::String(s[start_byte..end_byte].to_string()))
        }),
        function_proto_map,
    );
    set_builtin_name(&string_slice, "slice");
    set_builtin_length(&string_slice, 2.0);
    string_proto_map.borrow_mut().properties.insert(
        PKey::String("slice".to_string()),
        non_enum_data(string_slice),
    );

    let boolean_ctor_fn: BuiltinFn = Rc::new(|vm, args| {
        let value = if vm.current_new_target().is_some() {
            args.get(1).cloned().unwrap_or(Value::Undefined)
        } else {
            let value_idx = if args.len() > 1 { 1 } else { 0 };
            args.get(value_idx).cloned().unwrap_or(Value::Undefined)
        };
        let prim = Value::Bool(vm.to_boolean(&value));
        if vm.current_new_target().is_some()
            && let Some(Value::Object(obj)) = args.first()
        {
            obj.borrow_mut().properties.insert(
                PKey::String("[[PrimitiveValue]]".to_string()),
                PropertyDescriptor::data(prim),
            );
            return Ok(args.first().cloned().unwrap_or(Value::Undefined));
        }
        Ok(prim)
    });
    let boolean_ctor_val = Value::BuiltinObject(boolean_ctor_fn, Rc::clone(&boolean_ctor_map));
    set_builtin_length(&boolean_ctor_val, 1.0);
    set_builtin_name(&boolean_ctor_val, "Boolean");
    boolean_proto_map.borrow_mut().properties.insert(
        PKey::String("constructor".to_string()),
        PropertyDescriptor::data(boolean_ctor_val.clone()),
    );
    boolean_proto_map.borrow_mut().properties.insert(
        PKey::String("valueOf".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(|vm, args| {
                let this = args.first().cloned().unwrap_or(Value::Undefined);
                let res = match this {
                    Value::Bool(b) => Value::Bool(b),
                    Value::Object(obj) => {
                        if let Some(PropertyDescriptor::Data { value, .. }) = obj
                            .borrow()
                            .properties
                            .get(&PKey::String("[[PrimitiveValue]]".to_string()))
                        {
                            value.clone()
                        } else {
                            return Err(vm.throw_type_error(
                                "Boolean.prototype.valueOf called on incompatible receiver",
                            ));
                        }
                    }
                    _ => {
                        return Err(vm.throw_type_error(
                            "Boolean.prototype.valueOf called on incompatible receiver",
                        ));
                    }
                };
                Ok(res)
            }),
            function_proto_map,
        )),
    );
    boolean_proto_map.borrow_mut().properties.insert(
        PKey::String("toString".to_string()),
        PropertyDescriptor::data(builtin_object(
            Rc::new(|vm, args| {
                let this = args.first().cloned().unwrap_or(Value::Undefined);
                match this {
                    Value::Bool(b) => {
                        Ok(Value::String(if b { "true" } else { "false" }.to_string()))
                    }
                    Value::Object(obj) => {
                        if let Some(PropertyDescriptor::Data { value, .. }) = obj
                            .borrow()
                            .properties
                            .get(&PKey::String("[[PrimitiveValue]]".to_string()))
                            && let Value::Bool(b) = value
                        {
                            Ok(Value::String(if *b { "true" } else { "false" }.to_string()))
                        } else {
                            Err(vm.throw_type_error(
                                "Boolean.prototype.toString called on incompatible receiver",
                            ))
                        }
                    }
                    _ => Err(vm.throw_type_error(
                        "Boolean.prototype.toString called on incompatible receiver",
                    )),
                }
            }),
            function_proto_map,
        )),
    );

    set_builtin_name(&string_ctor_val, "String");
    set_builtin_name(&symbol_ctor_val, "Symbol");
    globals.properties.insert(
        PKey::String("String".to_string()),
        PropertyDescriptor::data(string_ctor_val),
    );
    globals.properties.insert(
        PKey::String("Boolean".to_string()),
        non_enum_data(boolean_ctor_val),
    );
    globals.properties.insert(
        PKey::String("Symbol".to_string()),
        PropertyDescriptor::data(symbol_ctor_val),
    );
}
