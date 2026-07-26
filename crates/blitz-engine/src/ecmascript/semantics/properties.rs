// SPDX-License-Identifier: MPL-2.0
use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use crate::Vm;
use crate::ecmascript::semantics::byte_views::{
  byte_view_details, byte_view_index_from_key, byte_view_index_value,
};
use crate::ecmascript::semantics::usize_to_f64;
use crate::runtime_utils::canonical_index_key;
use crate::value::{ArrayData, ObjectData, PropertyDescriptor, PropertyKey, Value};
use crate::{RuntimeError, gc};
use num_traits::ToPrimitive;

/// Gets the own property descriptor for a key on an object.
pub(crate) fn own_property_descriptor(
  obj: &Value,
  key: &PropertyKey,
) -> Option<PropertyDescriptor> {
  match obj {
    Value::Object(data) => {
      if let Some(index) = byte_view_index_from_key(key)
        && let Some(value) = byte_view_index_value(&Value::Object(Rc::clone(data)), index)
      {
        return Some(PropertyDescriptor::Data {
          value,
          writable: true,
          enumerable: true,
          configurable: true,
        });
      }
      data.borrow().properties.get(key).cloned()
    }
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

/// Creates a non-enumerable data property descriptor.
pub(crate) fn non_enum_data(value: Value) -> PropertyDescriptor {
  PropertyDescriptor::Data {
    value,
    writable: true,
    enumerable: false,
    configurable: true,
  }
}

/// 22.1.3.34.1 ThisStringValue ( value )
pub(crate) fn this_string_value(
  vm: &mut Vm,
  value: &Value,
) -> Result<String, RuntimeError> {
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
pub(crate) fn this_boolean_value(
  vm: &mut Vm,
  value: &Value,
) -> Result<bool, RuntimeError> {
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

/// Validates that a value can be coerced to an object.
pub(crate) fn require_object_coercible(
  vm: &mut Vm,
  value: &Value,
) -> Result<(), RuntimeError> {
  match value {
    Value::Undefined | Value::Null => {
      Err(vm.throw_type_error("Cannot convert undefined or null to object"))
    }
    _ => Ok(()),
  }
}

/// Converts an internal property descriptor into an ordinary object descriptor.
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
      if let Some(details) = byte_view_details(&Value::Object(Rc::clone(data))) {
        for index in 0..details.length {
          keys.push(PropertyKey::String(usize_to_decimal_string(index)));
        }
      }
      keys.extend(data.borrow().properties.keys().cloned());
    }
    Value::Proxy(proxy) => {
      let proxy = proxy.borrow();
      if !proxy.is_revoked() {
        keys.extend(own_property_keys(&proxy.target));
      }
    }
    Value::Array(arr, props) => {
      for (idx, slot) in arr.borrow().iter().enumerate() {
        if slot.is_some() {
          keys.push(PropertyKey::String(usize_to_decimal_string(idx)));
        }
      }
      keys.extend(props.borrow().properties.keys().cloned());
    }
    Value::Closure { props, .. } | Value::BuiltinObject(_, props) => {
      keys.extend(props.borrow().properties.keys().cloned());
    }
    _ => {}
  }
  order_property_keys(keys)
}

fn order_property_keys(keys: Vec<PropertyKey>) -> Vec<PropertyKey> {
  let mut indices = Vec::new();
  let mut strings = Vec::new();
  let mut symbols = Vec::new();
  let mut seen = HashSet::new();

  for key in keys {
    if !seen.insert(key.clone()) {
      continue;
    }
    match &key {
      PropertyKey::String(name) => match array_index_from_string(name) {
        Some(index) => indices.push((index, key)),
        None => strings.push(key),
      },
      PropertyKey::Symbol(_) => symbols.push(key),
    }
  }

  indices.sort_unstable_by_key(|(index, _)| *index);
  indices
    .into_iter()
    .map(|(_, key)| key)
    .chain(strings)
    .chain(symbols)
    .collect()
}

fn array_index_from_string(name: &str) -> Option<u32> {
  let index = canonical_index_key(name)?;
  let index = u32::try_from(index).ok()?;
  (index != u32::MAX).then_some(index)
}

/// Converts a PropertyKey to its corresponding Value.
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

/// Converts a u64 to a decimal string representation.
pub(crate) fn u64_to_decimal_string(value: u64) -> String {
  decimal_u64_string(value)
}

/// Checks if a u64 equals the value represented by a decimal string.
pub(crate) fn decimal_u64_eq_str(
  value: u64,
  s: &str,
) -> bool {
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
  digits.len() == s.len()
    && digits
      .iter()
      .zip(s.as_bytes())
      .all(|(a, b)| a == b)
}

/// Converts a usize to a decimal string representation.
pub(crate) fn usize_to_decimal_string(value: usize) -> String {
  decimal_u64_string(value as u64)
}

/// Creates an array Value from a vector of values.
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
