use std::cell::RefCell;
use std::rc::Rc;

use crate::Vm;
use crate::ecmascript::semantics::usize_to_f64;
use crate::runtime_utils::canonical_index_key;
use crate::value::{ArrayData, ObjectData, PropertyDescriptor, PropertyKey, Value};
use crate::{RuntimeError, gc};
use num_traits::ToPrimitive;

pub(crate) fn own_property_descriptor(
    obj: &Value,
    key: &PropertyKey,
) -> Option<PropertyDescriptor> {
    match obj {
        Value::Object(data) => data.borrow().properties.get(key).cloned(),
        Value::Proxy(proxy) => {
            let proxy = proxy.borrow();
            if proxy.is_revoked() {
                None
            } else {
                own_property_descriptor(&proxy.target, key)
            }
        }
        Value::Array(arr, props) => {
            let props_ref = props.borrow();
            if let Some(desc) = props_ref.properties.get(key).cloned() {
                return Some(desc);
            }
            if let PropertyKey::String(s) = key
                && let Some(idx) = canonical_index_key(s)
                && let Some(idx) = idx.to_usize()
                && idx < array_length(&props_ref)
                && let Some(Some(value)) = arr.borrow().get(idx).cloned()
            {
                return Some(PropertyDescriptor::Data {
                    value,
                    writable: true,
                    enumerable: true,
                    configurable: true,
                });
            }
            None
        }
        Value::Closure { props, .. } | Value::BuiltinObject(_, props) => {
            props.borrow().properties.get(key).cloned()
        }
        _ => None,
    }
}

fn array_length(props: &ObjectData) -> usize {
    props
        .properties
        .get(&PropertyKey::String("length".to_string()))
        .and_then(|desc| match desc {
            PropertyDescriptor::Data {
                value: Value::Number(n),
                ..
            } if n.is_finite() && *n >= 0.0 => Some(*n as usize),
            _ => None,
        })
        .unwrap_or(0)
}

pub(crate) fn own_enumerable_keys(obj: &Value) -> Vec<PropertyKey> {
    fn enumerable_prop_keys(props: &RefCell<ObjectData>, skip_length: bool) -> Vec<PropertyKey> {
        let mut index_keys: Vec<u64> = Vec::new();
        let mut string_keys: Vec<String> = Vec::new();
        let mut symbol_keys: Vec<PropertyKey> = Vec::new();
        for (key, desc) in &props.borrow().properties {
            let enumerable = match desc {
                PropertyDescriptor::Data { enumerable, .. }
                | PropertyDescriptor::Accessor { enumerable, .. } => *enumerable,
            };
            if !enumerable {
                continue;
            }
            match key {
                PropertyKey::String(name) => {
                    if skip_length && name == "length" {
                        continue;
                    }
                    if let Some(idx) = canonical_index_key(name) {
                        index_keys.push(idx);
                    } else {
                        string_keys.push(name.clone());
                    }
                }
                PropertyKey::Symbol(_) => symbol_keys.push(key.clone()),
            }
        }
        index_keys.sort_unstable();
        index_keys.dedup();
        let mut out: Vec<PropertyKey> = index_keys
            .into_iter()
            .map(|idx| PropertyKey::String(decimal_u64_string(idx)))
            .collect();
        out.extend(string_keys.into_iter().map(PropertyKey::String));
        out.extend(symbol_keys);
        out
    }

    let mut keys = Vec::new();
    match obj {
        Value::Object(data) => keys.extend(enumerable_prop_keys(data, false)),
        Value::Proxy(proxy) => {
            let proxy = proxy.borrow();
            if !proxy.is_revoked() {
                keys.extend(own_enumerable_keys(&proxy.target));
            }
        }
        Value::Array(arr, props) => {
            for (idx, slot) in arr.borrow().iter().enumerate() {
                if slot.is_some() {
                    keys.push(PropertyKey::String(usize_to_decimal_string(idx)));
                }
            }
            keys.extend(enumerable_prop_keys(props, true));
        }
        Value::Closure { .. } | Value::BuiltinObject(..) | Value::Builtin(_) => {
            let p = match obj {
                Value::Closure { props, .. } => Some(props),
                Value::BuiltinObject(_, props) => Some(props),
                _ => None,
            };
            if let Some(props) = p {
                keys.extend(enumerable_prop_keys(props, false));
            }
        }
        _ => {}
    }
    keys
}

pub(crate) fn non_enum_data(value: Value) -> PropertyDescriptor {
    PropertyDescriptor::Data {
        value,
        writable: true,
        enumerable: false,
        configurable: true,
    }
}

/// 22.1.3.34.1 ThisStringValue ( value )
pub(crate) fn this_string_value(vm: &mut Vm, value: &Value) -> Result<String, RuntimeError> {
    match value {
        Value::String(s) => Ok(s.clone()),
        Value::Object(obj) => {
            if let Some(PropertyDescriptor::Data { value, .. }) = obj
                .borrow()
                .properties
                .get(&PropertyKey::String("[[PrimitiveValue]]".to_string()))
                && let Value::String(s) = value
            {
                return Ok(s.clone());
            }
            Err(vm.throw_type_error("String method called on incompatible receiver"))
        }
        _ => Err(vm.throw_type_error("String method called on incompatible receiver")),
    }
}

/// 20.3.3.3.1 ThisBooleanValue ( value )
pub(crate) fn this_boolean_value(vm: &mut Vm, value: &Value) -> Result<bool, RuntimeError> {
    match value {
        Value::Bool(b) => Ok(*b),
        Value::Object(obj) => {
            if let Some(PropertyDescriptor::Data { value, .. }) = obj
                .borrow()
                .properties
                .get(&PropertyKey::String("[[PrimitiveValue]]".to_string()))
                && let Value::Bool(b) = value
            {
                return Ok(*b);
            }
            Err(vm.throw_type_error("Boolean method called on incompatible receiver"))
        }
        _ => Err(vm.throw_type_error("Boolean method called on incompatible receiver")),
    }
}

pub(crate) fn require_object_coercible(vm: &mut Vm, value: &Value) -> Result<(), RuntimeError> {
    match value {
        Value::Undefined | Value::Null => {
            Err(vm.throw_type_error("Cannot convert undefined or null to object"))
        }
        _ => Ok(()),
    }
}

pub(crate) fn descriptor_value_from_descriptor(
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
    Value::Object(gc::track_object(Rc::new(RefCell::new(obj))))
}

/// Returns the own property keys of an object.
pub(crate) fn own_property_keys(obj: &Value) -> Vec<PropertyKey> {
    let mut keys = Vec::new();
    match obj {
        Value::Object(data) => {
            for key in data.borrow().properties.keys() {
                keys.push(key.clone());
            }
        }
        Value::Proxy(proxy) => {
            let proxy = proxy.borrow();
            if !proxy.is_revoked() {
                keys.extend(own_property_keys(&proxy.target));
            }
        }
        Value::Array(arr, props) => {
            for idx in 0..arr.borrow().len() {
                keys.push(PropertyKey::String(usize_to_decimal_string(idx)));
            }
            for key in props.borrow().properties.keys() {
                keys.push(key.clone());
            }
        }
        Value::Closure { props, .. } | Value::BuiltinObject(_, props) => {
            for key in props.borrow().properties.keys() {
                keys.push(key.clone());
            }
        }
        _ => {}
    }
    keys
}

pub(crate) fn property_key_to_value(key: &PropertyKey) -> Value {
    match key {
        PropertyKey::String(name) => Value::String(name.clone()),
        PropertyKey::Symbol(id) => Value::Symbol(id.clone(), None),
    }
}

fn decimal_u64_string(value: u64) -> String {
    let mut buf = [0u8; 20];
    let mut n = value;
    let mut i = buf.len();
    loop {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    let mut out = String::with_capacity(buf.len() - i);
    out.extend(buf[i..].iter().map(|b| char::from(*b)));
    out
}

pub(crate) fn u64_to_decimal_string(value: u64) -> String {
    decimal_u64_string(value)
}

pub(crate) fn decimal_u64_eq_str(value: u64, s: &str) -> bool {
    let mut buf = [0u8; 20];
    let mut n = value;
    let mut i = buf.len();
    loop {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    let digits = &buf[i..];
    digits.len() == s.len() && digits.iter().zip(s.as_bytes()).all(|(a, b)| a == b)
}

pub(crate) fn usize_to_decimal_string(value: usize) -> String {
    decimal_u64_string(value as u64)
}

pub(crate) fn array_from_values(values: Vec<Value>) -> Value {
    let arr_rc = Rc::new(RefCell::new(ArrayData::from(
        values.into_iter().map(Some).collect::<Vec<_>>(),
    )));
    let props = gc::track_object(Rc::new(RefCell::new(ObjectData::new())));
    props.borrow_mut().properties.insert(
        PropertyKey::String("length".to_string()),
        array_length_descriptor(arr_rc.borrow().len()),
    );
    Value::Array(arr_rc, props)
}

/// Create the standard array `length` descriptor.
pub(crate) fn array_length_descriptor(length: usize) -> PropertyDescriptor {
    PropertyDescriptor::Data {
        value: Value::Number(usize_to_f64(length)),
        writable: true,
        enumerable: false,
        configurable: false,
    }
}
