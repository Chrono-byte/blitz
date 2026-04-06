use std::cell::RefCell;
use std::rc::Rc;

use num_traits::ToPrimitive;

use crate::runtime_utils::{
    builtin_object, char_code_at, char_index_to_byte_index, non_enum_data,
    require_object_coercible, set_builtin_length, set_builtin_name, static_builtin_arg_offset,
    this_boolean_value, this_string_value, to_length, usize_to_f64,
};
use crate::value::{BuiltinFn, ObjectData, PropertyDescriptor, PropertyKey, SymbolId, Value};
use crate::vm::Vm;

fn utf16_encode_code_point(cp: u32) -> String {
    if cp <= 0xFFFF {
        char::from_u32(cp).unwrap_or('\u{FFFD}').to_string()
    } else {
        let cp = cp - 0x1_0000;
        let high = 0xD800 + ((cp >> 10) as u16);
        let low = 0xDC00 + ((cp & 0x3FF) as u16);
        String::from_utf16_lossy(&[high, low])
    }
}

fn string_constructor_to_string(
    vm: &mut Vm,
    value: &Value,
) -> Result<String, crate::errors::RuntimeError> {
    match value {
        Value::Symbol(_, desc) if vm.current_new_target().is_some() => {
            Err(vm.throw_type_error("Cannot convert a Symbol value to a string"))
        }
        Value::Symbol(_, desc) => Ok(match desc {
            Some(d) => format!("Symbol({})", d),
            None => "Symbol()".to_string(),
        }),
        Value::String(_)
        | Value::Number(_)
        | Value::Bool(_)
        | Value::Null
        | Value::Undefined
        | Value::BigInt(_) => vm.to_string(value),
        _ => {
            let prim = vm.to_primitive(value, Some("string"))?;
            if matches!(
                prim,
                Value::Object(_)
                    | Value::Array(..)
                    | Value::Closure { .. }
                    | Value::Builtin(_)
                    | Value::BuiltinObject(_, _)
                    | Value::Promise(_)
            ) {
                return Err(vm.throw_type_error("Cannot convert object to string"));
            }
            vm.to_string(&prim)
        }
    }
}

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
        let value_idx = if vm.current_new_target().is_some() || args.len() > 1 {
            1
        } else {
            0
        };
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
    set_builtin_length(&symbol_ctor_val, 0.0);
    symbol_proto_map.borrow_mut().properties.insert(
        PKey::String("constructor".to_string()),
        PropertyDescriptor::data(symbol_ctor_val.clone()),
    );
    let symbol_to_string = builtin_object(
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
    );
    set_builtin_name(&symbol_to_string, "toString");
    set_builtin_length(&symbol_to_string, 0.0);
    symbol_proto_map.borrow_mut().properties.insert(
        PKey::String("toString".to_string()),
        PropertyDescriptor::data(symbol_to_string),
    );
    let symbol_value_of = builtin_object(
        Rc::new(|_vm, args| Ok(args.first().cloned().unwrap_or(Value::Undefined))),
        function_proto_map,
    );
    set_builtin_name(&symbol_value_of, "valueOf");
    set_builtin_length(&symbol_value_of, 0.0);
    symbol_proto_map.borrow_mut().properties.insert(
        PKey::String("valueOf".to_string()),
        PropertyDescriptor::data(symbol_value_of),
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
        let value_idx = if vm.current_new_target().is_some() || args.len() > 1 {
            1
        } else {
            0
        };
        let s = match args.get(value_idx) {
            None => String::new(),
            Some(v) => string_constructor_to_string(vm, v)?,
        };
        if vm.current_new_target().is_some()
            && let Some(Value::Object(obj_rc)) = args.first()
        {
            let mut obj = obj_rc.borrow_mut();
            obj.set_prototype_value(Value::Object(Rc::clone(&string_proto_for_ctor)));
            obj.properties.insert(
                PKey::String("[[PrimitiveValue]]".to_string()),
                PropertyDescriptor::data(Value::String(s.clone())),
            );
            obj.properties.insert(
                PKey::String("length".to_string()),
                PropertyDescriptor::Data {
                    value: Value::Number(usize_to_f64(s.encode_utf16().count())),
                    writable: false,
                    enumerable: false,
                    configurable: false,
                },
            );
            for (idx, ch) in s.chars().enumerate() {
                obj.properties.insert(
                    PKey::String(idx.to_string()),
                    PropertyDescriptor::Data {
                        value: Value::String(ch.to_string()),
                        writable: false,
                        enumerable: true,
                        configurable: false,
                    },
                );
            }
            return Ok(args.first().cloned().unwrap_or(Value::Undefined));
        }
        Ok(Value::String(s))
    });
    let string_ctor_val = Value::BuiltinObject(string_ctor_fn.clone(), Rc::clone(&string_ctor_map));
    set_builtin_length(&string_ctor_val, 1.0);
    set_builtin_name(&string_ctor_val, "String");
    string_ctor_map.borrow_mut().properties.insert(
        PKey::String("fromCharCode".to_string()),
        non_enum_data({
            let from_char_code = builtin_object(
                Rc::new(|vm, args| {
                    let off = static_builtin_arg_offset(vm, args, "String");
                    let mut out = String::new();
                    for arg in args.iter().skip(off) {
                        let n = vm.to_number_value(arg)?;
                        let int = if n.is_nan() || !n.is_finite() {
                            0u32
                        } else {
                            n.trunc() as i64 as u32
                        };
                        let unit = (int & 0xFFFF) as u16;
                        out.push_str(&String::from_utf16_lossy(&[unit]));
                    }
                    Ok(Value::String(out))
                }),
                function_proto_map,
            );
            set_builtin_name(&from_char_code, "fromCharCode");
            set_builtin_length(&from_char_code, 1.0);
            from_char_code
        }),
    );
    string_ctor_map.borrow_mut().properties.insert(
        PKey::String("fromCodePoint".to_string()),
        non_enum_data({
            let from_code_point = builtin_object(
                Rc::new(|vm, args| {
                    let off = static_builtin_arg_offset(vm, args, "String");
                    let mut out = String::new();
                    for arg in args.iter().skip(off) {
                        let n = vm.to_number_value(arg)?;
                        if !n.is_finite()
                            || n.fract() != 0.0
                            || !(0.0..=0x10FFFF as f64).contains(&n)
                        {
                            return Err(vm.throw_range_error("Invalid code point"));
                        }
                        out.push_str(&utf16_encode_code_point(n as u32));
                    }
                    Ok(Value::String(out))
                }),
                function_proto_map,
            );
            set_builtin_name(&from_code_point, "fromCodePoint");
            set_builtin_length(&from_code_point, 1.0);
            from_code_point
        }),
    );
    string_ctor_map.borrow_mut().properties.insert(
        PKey::String("raw".to_string()),
        non_enum_data({
            let raw_fn = builtin_object(
                Rc::new(|vm, args| {
                    let off = static_builtin_arg_offset(vm, args, "String");
                    let template = args.get(off).cloned().unwrap_or(Value::Undefined);
                    let template_obj = vm.to_object_value(template)?;
                    let raw =
                        vm.get_property_host(&template_obj, &PKey::String("raw".to_string()))?;
                    let raw_obj = if matches!(raw, Value::Undefined)
                        && matches!(template_obj, Value::Array(..))
                    {
                        template_obj.clone()
                    } else {
                        vm.to_object_value(raw)?
                    };
                    let len_value =
                        vm.get_property_host(&raw_obj, &PKey::String("length".to_string()))?;
                    let literal_count = to_length(vm, &len_value)?;
                    if literal_count == 0 {
                        return Ok(Value::String(String::new()));
                    }
                    let substitutions = args.get(off + 1..).unwrap_or(&[]);
                    let mut out = String::new();
                    for i in 0..literal_count {
                        let next = vm.get_property_host(&raw_obj, &PKey::String(i.to_string()))?;
                        out.push_str(&vm.to_string(&next)?);
                        if i + 1 == literal_count {
                            break;
                        }
                        let subst = substitutions.get(i).cloned();
                        if let Some(subst) = subst {
                            out.push_str(&vm.to_string(&subst)?);
                        }
                    }
                    Ok(Value::String(out))
                }),
                function_proto_map,
            );
            set_builtin_name(&raw_fn, "raw");
            set_builtin_length(&raw_fn, 1.0);
            raw_fn
        }),
    );
    string_proto_map.borrow_mut().properties.insert(
        PKey::String("constructor".to_string()),
        PropertyDescriptor::data(string_ctor_val.clone()),
    );
    let string_value_of = builtin_object(
        Rc::new(|vm, args| {
            let this = args.first().cloned().unwrap_or(Value::Undefined);
            Ok(Value::String(this_string_value(vm, &this)?))
        }),
        function_proto_map,
    );
    set_builtin_name(&string_value_of, "valueOf");
    set_builtin_length(&string_value_of, 0.0);
    string_proto_map.borrow_mut().properties.insert(
        PKey::String("valueOf".to_string()),
        PropertyDescriptor::data(string_value_of),
    );
    let string_to_string = builtin_object(
        Rc::new(|vm, args| {
            let this = args.first().cloned().unwrap_or(Value::Undefined);
            Ok(Value::String(this_string_value(vm, &this)?))
        }),
        function_proto_map,
    );
    set_builtin_name(&string_to_string, "toString");
    set_builtin_length(&string_to_string, 0.0);
    string_proto_map.borrow_mut().properties.insert(
        PKey::String("toString".to_string()),
        PropertyDescriptor::data(string_to_string),
    );
    let string_index_of = builtin_object(
        Rc::new(|vm, args| {
            let receiver = args.first().cloned().unwrap_or(Value::Undefined);
            require_object_coercible(vm, &receiver)?;
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
    set_builtin_length(&string_index_of, 1.0);
    string_proto_map.borrow_mut().properties.insert(
        PKey::String("indexOf".to_string()),
        non_enum_data(string_index_of),
    );

    let string_char_code_at = builtin_object(
        Rc::new(|vm, args| {
            let receiver = args.first().cloned().unwrap_or(Value::Undefined);
            require_object_coercible(vm, &receiver)?;
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
            require_object_coercible(vm, &receiver)?;
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
        let value_idx = if vm.current_new_target().is_some() || args.len() > 1 {
            1
        } else {
            0
        };
        let value = args.get(value_idx).cloned().unwrap_or(Value::Undefined);
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
    let boolean_value_of = builtin_object(
        Rc::new(|vm, args| {
            let this = args.first().cloned().unwrap_or(Value::Undefined);
            Ok(Value::Bool(this_boolean_value(vm, &this)?))
        }),
        function_proto_map,
    );
    set_builtin_name(&boolean_value_of, "valueOf");
    set_builtin_length(&boolean_value_of, 0.0);
    boolean_proto_map.borrow_mut().properties.insert(
        PKey::String("valueOf".to_string()),
        PropertyDescriptor::data(boolean_value_of),
    );
    let boolean_to_string = builtin_object(
        Rc::new(|vm, args| {
            let this = args.first().cloned().unwrap_or(Value::Undefined);
            let b = this_boolean_value(vm, &this)?;
            Ok(Value::String(if b { "true" } else { "false" }.to_string()))
        }),
        function_proto_map,
    );
    set_builtin_name(&boolean_to_string, "toString");
    set_builtin_length(&boolean_to_string, 0.0);
    boolean_proto_map.borrow_mut().properties.insert(
        PKey::String("toString".to_string()),
        PropertyDescriptor::data(boolean_to_string),
    );

    set_builtin_name(&string_ctor_val, "String");
    set_builtin_name(&symbol_ctor_val, "Symbol");
    globals.properties.insert(
        PKey::String("String".to_string()),
        non_enum_data(string_ctor_val),
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
