// SPDX-License-Identifier: MPL-2.0
//! Array builtin functions and prototype methods

use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::HashMap;
use std::env;
use std::rc::Rc;

use js_string::{JsString, JsStringBuilder};
use num_traits::ToPrimitive;

use crate::RuntimeError;
use crate::runtime_utils::own_property_descriptor;
use crate::runtime_utils::{
  array_from_values, array_length_descriptor, builtin_object, canonical_index_key,
  collect_iterable_values, reject_pending_promise, resolve_promise_with_value, same_value_zero,
  set_builtin_length, set_builtin_name, static_builtin_arg_offset, strict_equal,
  to_integer_or_infinity, to_length, u64_to_decimal_string, usize_to_f64,
};
use crate::value::{
  ArrayData, BuiltinFn, ObjectData, PromiseState, PropertyDescriptor, PropertyKey, SymbolId, Value,
};
use crate::vm::Vm;

pub(super) fn array_like_length(
  vm: &mut Vm,
  receiver: &Value,
) -> Result<usize, RuntimeError> {
  if let Value::Array(_, props) = receiver {
    let props_ref = props.borrow();
    if let Some(PropertyDescriptor::Data {
      value: Value::Number(length),
      ..
    }) = props_ref
      .properties
      .get(&PropertyKey::String("length".into()))
    {
      return array_like_length_from_number(*length);
    }
  }

  let length = vm.get_property_host(receiver, &PropertyKey::String("length".into()))?;
  array_like_length_from_number(vm.to_number_value(&length)?)
}

fn array_like_length_from_number(length: f64) -> Result<usize, RuntimeError> {
  const MAX_SAFE_LENGTH: u128 = 9_007_199_254_740_991;

  if length.is_nan() || length <= 0.0 {
    return Ok(0);
  }
  if !length.is_finite() {
    return Ok(MAX_SAFE_LENGTH as usize);
  }
  Ok(
    length
      .trunc()
      .to_u128()
      .unwrap_or(MAX_SAFE_LENGTH)
      .min(MAX_SAFE_LENGTH) as usize,
  )
}

pub(super) fn collect_indexed_snapshot(
  vm: &mut Vm,
  receiver: &Value,
  length: usize,
) -> Result<(Vec<usize>, HashMap<usize, Value>), RuntimeError> {
  let mut keys = Vec::new();
  let mut values = HashMap::new();
  for key in vm.own_property_keys_value(receiver)? {
    let PropertyKey::String(key) = key else {
      continue;
    };
    let Ok(index) = key.parse::<usize>() else {
      continue;
    };
    if index >= length || values.contains_key(&index) {
      continue;
    }
    let value = vm.get_property_host(receiver, &PropertyKey::String(key))?;
    keys.push(index);
    values.insert(index, value);
  }
  keys.sort_unstable();
  Ok((keys, values))
}

pub(super) fn array_length_is_writable(receiver: &Value) -> bool {
  match receiver {
    Value::Array(_, properties) => properties
      .borrow()
      .properties
      .get(&PropertyKey::String("length".into()))
      .is_none_or(|descriptor| {
        matches!(descriptor, PropertyDescriptor::Data { writable: true, .. })
      }),
    _ => true,
  }
}

pub(super) fn clamp_relative_index(
  index: isize,
  length: usize,
) -> usize {
  if index < 0 {
    length.saturating_sub(index.checked_abs().unwrap_or(isize::MAX) as usize)
  } else {
    (index as usize).min(length)
  }
}

pub(super) fn create_data_property_or_throw(
  vm: &mut Vm,
  target: &Value,
  key: &Value,
  value: Value,
) -> Result<(), crate::errors::RuntimeError> {
  let dense_index = match (target, key) {
    (Value::Array(_, _), Value::String(key)) => {
      canonical_index_key(key).and_then(|index| usize::try_from(index).ok())
    }
    _ => None,
  };
  if let (Value::Array(_, properties), Some(index)) = (target, dense_index) {
    let properties = properties.borrow();
    if let Some(PropertyDescriptor::Data {
      value: Value::Number(length),
      writable: false,
      ..
    }) = properties
      .properties
      .get(&PropertyKey::String("length".to_string().into()))
      && usize_to_f64(index) >= *length
    {
      return Err(vm.throw_type_error("Array.from cannot extend an array with readonly length"));
    }
  }

  let mut descriptor = ObjectData::new();
  descriptor.properties.insert(
    PropertyKey::String("value".to_string().into()),
    PropertyDescriptor::ordinary_data(value.clone()),
  );
  descriptor.properties.insert(
    PropertyKey::String("writable".to_string().into()),
    PropertyDescriptor::ordinary_data(Value::Bool(true)),
  );
  descriptor.properties.insert(
    PropertyKey::String("enumerable".to_string().into()),
    PropertyDescriptor::ordinary_data(Value::Bool(true)),
  );
  descriptor.properties.insert(
    PropertyKey::String("configurable".to_string().into()),
    PropertyDescriptor::ordinary_data(Value::Bool(true)),
  );
  let descriptor = Value::Object(Rc::new(RefCell::new(descriptor)));
  if !vm.define_property_value(target, key, &descriptor)? {
    return Err(vm.throw_type_error("Array.from failed to define element"));
  }

  // Dense arrays use their element storage as the canonical representation.
  // `define_property_value` performs all extensibility and descriptor checks;
  // normalize its successful numeric definition so later array algorithms do
  // not observe a stale shadow property alongside the dense element.
  if let (Value::Array(elements, properties), Value::String(key), Some(index)) =
    (target, key, dense_index)
  {
    let mut elements = elements.borrow_mut();
    if index >= elements.len() {
      elements.resize(index.saturating_add(1), None);
    }
    elements[index] = Some(value);
    let length = elements.len();
    drop(elements);
    let mut properties = properties.borrow_mut();
    properties
      .properties
      .shift_remove(&PropertyKey::String(key.clone()));
    let length_key = PropertyKey::String("length".to_string().into());
    let length_descriptor = match properties.properties.get(&length_key).cloned() {
      Some(PropertyDescriptor::Data {
        writable,
        enumerable,
        configurable,
        ..
      }) => PropertyDescriptor::Data {
        value: Value::Number(usize_to_f64(length)),
        writable,
        enumerable,
        configurable,
      },
      _ => array_length_descriptor(length),
    };
    properties
      .properties
      .insert(length_key, length_descriptor);
  }
  Ok(())
}

pub(super) fn delete_property_or_throw(
  vm: &mut Vm,
  target: &Value,
  key: &PropertyKey,
) -> Result<(), RuntimeError> {
  if vm.delete_property_host(target, key)? {
    Ok(())
  } else {
    Err(vm.throw_type_error("Cannot delete array element"))
  }
}

pub(crate) fn is_array_value(
  vm: &mut Vm,
  value: &Value,
) -> Result<bool, RuntimeError> {
  match value {
    Value::Array(..) => Ok(true),
    Value::Proxy(proxy) => {
      let target = {
        let proxy = proxy.borrow();
        if proxy.is_revoked() {
          return Err(vm.throw_type_error("Cannot inspect a revoked Proxy"));
        }
        proxy.target.clone()
      };
      is_array_value(vm, &target)
    }
    _ => Ok(false),
  }
}

/// Implements the `ArraySpeciesCreate` constructor selection shared by
/// array-producing prototype methods.
pub(super) fn array_species_create(
  vm: &mut Vm,
  receiver: &Value,
  length: usize,
) -> Result<Value, RuntimeError> {
  let default_constructor = vm.get_global("Array").unwrap_or(Value::Undefined);
  let constructor = if is_array_value(vm, receiver)? {
    vm.get_property_host(receiver, &PropertyKey::String("constructor".into()))?
  } else {
    Value::Undefined
  };
  let constructor = match constructor {
    Value::Undefined => default_constructor,
    value
      if !matches!(
        value,
        Value::Object(..)
          | Value::Array(..)
          | Value::Closure { .. }
          | Value::BuiltinObject(_, _)
          | Value::Proxy(_)
      ) =>
    {
      return Err(vm.throw_type_error("Array constructor property is not an object"));
    }
    value => {
      let species =
        vm.get_property_host(&value, &PropertyKey::Symbol(SymbolId::well_known_species()))?;
      match species {
        Value::Undefined | Value::Null => default_constructor,
        value if value.is_constructable() => value,
        _ => return Err(vm.throw_type_error("constructor[Symbol.species] is not a constructor")),
      }
    }
  };
  vm.construct_value(&constructor, &[Value::Number(usize_to_f64(length))])
}

mod constructor;
mod iterator;
mod prototype;

/// The Array constructor, prototype, and iterator intrinsics.
pub(crate) struct ArrayIntrinsics {
  pub(crate) constructor: Value,
  pub(crate) prototype: Rc<RefCell<ObjectData>>,
  pub(crate) iterator_prototype: Rc<RefCell<ObjectData>>,
  pub(crate) values: Value,
  pub(crate) keys: Value,
  pub(crate) entries: Value,
}

/// Installs the complete Array intrinsic family in observable property order.
pub(crate) fn install_array_intrinsics(
  object_prototype: &Rc<RefCell<ObjectData>>,
  function_prototype: &Rc<RefCell<ObjectData>>,
) -> ArrayIntrinsics {
  let prototype = prototype::install_before_iterator(object_prototype, function_prototype);
  let iterators = iterator::install(&prototype, object_prototype, function_prototype);
  prototype::install_after_iterator(&prototype, function_prototype);
  let constructor = constructor::install(&prototype, function_prototype);

  ArrayIntrinsics {
    constructor,
    prototype,
    iterator_prototype: iterators.prototype,
    values: iterators.values,
    keys: iterators.keys,
    entries: iterators.entries,
  }
}
