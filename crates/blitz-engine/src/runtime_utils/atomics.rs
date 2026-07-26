// SPDX-License-Identifier: MPL-2.0
use super::*;
use crate::ecmascript::semantics::atomics as semantics_atomics;
use crate::ecmascript::semantics::byte_views::{array_buffer_is_shared, byte_view_details};

use crate::gc;
use crate::value::{ArrayData, ObjectData, PropertyDescriptor, PropertyKey, Value};
use crate::{RuntimeError, Vm};
use num_bigint::BigInt as BigIntValue;
use num_traits::ToPrimitive;
use std::cell::RefCell;
use std::rc::Rc;

// Delegate pure algorithmic helpers to the semantics module. This keeps the
// runtime-specific code here small and avoids duplicating logic.
/// Gets the atomics value kind for the given TypedArray kind name.
fn atomics_value_kind(kind_name: &str) -> Option<AtomicsValueKind> {
  semantics_atomics::atomics_value_kind(kind_name)
}

/// Normalizes an integer value according to the bit width and signedness for atomics operations.
fn atomics_normalize_integer(
  value: i128,
  bits: u32,
  signed: bool,
) -> i128 {
  semantics_atomics::atomics_normalize_integer(value, bits, signed)
}

/// Normalizes a BigInt value according to its signedness for atomics operations.
fn atomics_normalize_bigint(
  value: BigIntValue,
  signed: bool,
) -> BigIntValue {
  semantics_atomics::atomics_normalize_bigint(value, signed)
}

/// Converts a value to an integer for atomics operations.
fn atomics_value_to_integer(
  vm: &mut Vm,
  value: &Value,
  bits: u32,
  signed: bool,
) -> Result<Value, RuntimeError> {
  let n = vm.to_number_value(value)?;
  let int = n.trunc().to_i128().unwrap_or_else(|| {
    if n.is_sign_negative() {
      i128::MIN
    } else {
      i128::MAX
    }
  });
  let int = atomics_normalize_integer(int, bits, signed);
  Ok(Value::Number(int.to_f64().unwrap_or(f64::NAN)))
}

/// Converts a value to a BigInt for atomics operations.
fn atomics_value_to_bigint(
  vm: &mut Vm,
  value: &Value,
  signed: bool,
) -> Result<Value, RuntimeError> {
  let bigint = match value {
    Value::BigInt(n) => n.clone(),
    Value::Number(n) => BigIntValue::from_f64(n.trunc())
      .ok_or_else(|| vm.throw_type_error("Atomics BigInt operations require BigInt values"))?,
    Value::String(s) => BigIntValue::parse_bytes(s.as_bytes(), 10)
      .ok_or_else(|| vm.throw_type_error("Atomics BigInt operations require BigInt values"))?,
    _ => {
      let prim = vm.to_primitive(value, Some("number"))?;
      match prim {
        Value::BigInt(n) => n,
        Value::Number(n) => BigIntValue::from_f64(n.trunc())
          .ok_or_else(|| vm.throw_type_error("Atomics BigInt operations require BigInt values"))?,
        _ => {
          return Err(vm.throw_type_error("Atomics BigInt operations require BigInt values"));
        }
      }
    }
  };
  Ok(Value::BigInt(atomics_normalize_bigint(bigint, signed)))
}

/// Extracts atomics view information from a TypedArray value.
pub(crate) fn atomics_view_info(
  vm: &mut Vm,
  view: &Value,
) -> Result<AtomicsViewInfo, RuntimeError> {
  let Value::Object(view_obj) = view else {
    return Err(vm.throw_type_error("Atomics requires a TypedArray"));
  };
  if let Some(details) = byte_view_details(view) {
    let Value::Object(buffer) = &details.buffer else {
      return Err(vm.throw_type_error("Atomics requires a TypedArray"));
    };
    let is_shared = array_buffer_is_shared(&details.buffer).unwrap_or(false);
    let value_kind = atomics_value_kind(&details.kind)
      .ok_or_else(|| vm.throw_type_error("Atomics requires a numeric or BigInt typed array"))?;
    return Ok(AtomicsViewInfo {
      view: details.object,
      data: details.data,
      buffer: Rc::clone(buffer),
      kind_name: details.kind,
      value_kind,
      length: details.length,
      is_shared,
    });
  }

  let (data, buffer, kind_name, length, is_shared) = {
    let view_data = view_obj.borrow();
    if matches!(
      view_data
        .properties
        .get(&PropertyKey::String("[[TypedArrayKind]]".to_string())),
      Some(PropertyDescriptor::Data {
        value: Value::String(kind),
        ..
      }) if matches!(kind.as_str(), "Uint8Array" | "Int8Array" | "Uint8ClampedArray")
    ) {
      return Err(vm.throw_type_error("Atomics requires a TypedArray"));
    }
    let data = match view_data
      .properties
      .get(&PropertyKey::String("[[TypedArrayData]]".to_string()))
    {
      Some(PropertyDescriptor::Data {
        value: Value::Array(arr, _),
        ..
      }) => Rc::clone(arr),
      _ => return Err(vm.throw_type_error("Atomics requires a TypedArray")),
    };
    let kind_name = match view_data
      .properties
      .get(&PropertyKey::String("[[TypedArrayKind]]".to_string()))
    {
      Some(PropertyDescriptor::Data {
        value: Value::String(kind),
        ..
      }) => kind.clone(),
      _ => return Err(vm.throw_type_error("Atomics requires a TypedArray")),
    };
    let length = match view_data
      .properties
      .get(&PropertyKey::String("[[TypedArrayLength]]".to_string()))
    {
      Some(PropertyDescriptor::Data {
        value: Value::Number(n),
        ..
      }) if n.is_finite() && *n >= 0.0 => n
        .trunc()
        .to_usize()
        .unwrap_or(data.borrow().len()),
      _ => data.borrow().len(),
    };
    let buffer = match view_data
      .properties
      .get(&PropertyKey::String("[[TypedArrayBuffer]]".to_string()))
    {
      Some(PropertyDescriptor::Data {
        value: Value::Object(buffer),
        ..
      }) => Rc::clone(buffer),
      _ => return Err(vm.throw_type_error("Atomics requires a TypedArray")),
    };
    let is_shared = {
      let buffer_data = buffer.borrow();
      matches!(
        buffer_data
          .properties
          .get(&PropertyKey::String("[[IsSharedArrayBuffer]]".to_string())),
        Some(PropertyDescriptor::Data {
          value: Value::Bool(true),
          ..
        })
      )
    };
    (data, buffer, kind_name, length, is_shared)
  };
  let value_kind = atomics_value_kind(&kind_name)
    .ok_or_else(|| vm.throw_type_error("Atomics requires a numeric or BigInt typed array"))?;
  Ok(AtomicsViewInfo {
    view: Rc::clone(view_obj),
    data,
    buffer,
    kind_name,
    value_kind,
    length,
    is_shared,
  })
}

/// Validates and normalizes an index for atomics operations on a view.
pub(crate) fn atomics_view_index(
  vm: &mut Vm,
  info: &AtomicsViewInfo,
  index_value: &Value,
) -> Result<usize, RuntimeError> {
  let index = to_index(vm, index_value)?;
  if index >= info.length {
    return Err(vm.throw_range_error("Atomics index out of range"));
  }
  Ok(index)
}

/// Reads a value from a TypedArray view at the given index.
fn atomics_read_view_slot(
  vm: &mut Vm,
  info: &AtomicsViewInfo,
  index: usize,
) -> Result<Value, RuntimeError> {
  let prop_key = PropertyKey::String(index.to_string());
  let value = vm.get_property_host(&Value::Object(Rc::clone(&info.view)), &prop_key)?;
  if !matches!(value, Value::Undefined) {
    return Ok(value);
  }
  Ok(
    info
      .data
      .borrow()
      .get(index)
      .cloned()
      .flatten()
      .unwrap_or(Value::Undefined),
  )
}

/// Writes a value to a TypedArray view at the given index.
fn atomics_write_view_slot(
  vm: &mut Vm,
  info: &AtomicsViewInfo,
  index: usize,
  value: Value,
) -> Result<(), RuntimeError> {
  let prop_key = Value::Number(usize_to_f64(index));
  let stored = value.clone();
  drop(vm.set_property_host(&Value::Object(Rc::clone(&info.view)), &prop_key, value)?);
  if let Some(slot) = info.data.borrow_mut().get_mut(index) {
    *slot = Some(stored);
  }
  Ok(())
}

/// Gets or creates the waiters array for an ArrayBuffer.
pub(crate) fn atomics_waiters_array(buffer: &Rc<RefCell<ObjectData>>) -> Rc<RefCell<ArrayData>> {
  let mut buffer_data = buffer.borrow_mut();
  if let Some(PropertyDescriptor::Data {
    value: Value::Array(arr, _),
    ..
  }) = buffer_data
    .properties
    .get(&PropertyKey::String("[[AtomicsWaiters]]".to_string()))
  {
    return Rc::clone(arr);
  }
  let arr = gc::track_array(Rc::new(RefCell::new(ArrayData::from(
    Vec::<Option<Value>>::new(),
  ))));
  buffer_data.properties.insert(
    PropertyKey::String("[[AtomicsWaiters]]".to_string()),
    PropertyDescriptor::ordinary_data(Value::Array(
      Rc::clone(&arr),
      gc::track_object(Rc::new(RefCell::new(ObjectData::new()))),
    )),
  );
  arr
}

/// Create a waiter record object for an Atomics.wait() operation with index and resolver.
pub(crate) fn atomics_make_waiter_record(
  index: usize,
  resolve: Option<Value>,
) -> Value {
  let mut data = ObjectData::new();
  data.properties.insert(
    PropertyKey::String("index".to_string()),
    PropertyDescriptor::ordinary_data(Value::Number(usize_to_f64(index))),
  );
  data.properties.insert(
    PropertyKey::String("resolve".to_string()),
    PropertyDescriptor::ordinary_data(resolve.unwrap_or(Value::Undefined)),
  );
  Value::Object(gc::track_object(Rc::new(RefCell::new(data))))
}

/// Create a result object for an Atomics.wait operation with async and value properties.
pub(crate) fn atomics_make_wait_result(
  async_flag: bool,
  value: Value,
) -> Value {
  let result = gc::track_object(Rc::new(RefCell::new(ObjectData::new())));
  result.borrow_mut().properties.insert(
    PropertyKey::String("async".to_string()),
    PropertyDescriptor::ordinary_data(Value::Bool(async_flag)),
  );
  result.borrow_mut().properties.insert(
    PropertyKey::String("value".to_string()),
    PropertyDescriptor::ordinary_data(value),
  );
  Value::Object(result)
}

/// Wake up waiting agents on a specific index of a SharedArrayBuffer.
///
/// # Errors
///
/// Returns a [`RuntimeError`] if the operation fails.
pub(crate) fn atomics_wake_waiters(
  vm: &mut Vm,
  info: &AtomicsViewInfo,
  index: usize,
  count: usize,
) -> Result<usize, RuntimeError> {
  let waiters = atomics_waiters_array(&info.buffer);
  let mut to_wake: Vec<Value> = Vec::new();
  {
    let mut waiters_mut = waiters.borrow_mut();
    let mut remaining: Vec<Option<Value>> = Vec::with_capacity(waiters_mut.len());
    for entry in waiters_mut.iter().cloned() {
      if to_wake.len() >= count {
        remaining.push(entry);
        continue;
      }
      let Some(waiter) = entry else {
        continue;
      };
      let record_index =
        match vm.get_property_host(&waiter, &PropertyKey::String("index".to_string())) {
          Ok(Value::Number(n)) if n.is_finite() && n >= 0.0 => match n.trunc().to_usize() {
            Some(index) => index,
            None => {
              remaining.push(Some(waiter));
              continue;
            }
          },
          _ => {
            remaining.push(Some(waiter));
            continue;
          }
        };
      if record_index != index {
        remaining.push(Some(waiter));
        continue;
      }
      to_wake.push(waiter);
    }
    *waiters_mut = ArrayData::from(remaining);
  }

  for waiter in &to_wake {
    if let Ok(resolve) = vm.get_property_host(waiter, &PropertyKey::String("resolve".to_string()))
      && !matches!(resolve, Value::Undefined)
    {
      drop(vm.call_value_with_this(
        &resolve,
        &Value::Undefined,
        &[Value::String("ok".to_string())],
      ));
    }
  }

  Ok(to_wake.len())
}

/// Register a waiter on a SharedArrayBuffer for Atomics.wait() operations.
///
/// # Errors
///
/// Returns a [`RuntimeError`] if the registration fails.
pub(crate) fn atomics_register_waiter(
  _vm: &mut Vm,
  info: &AtomicsViewInfo,
  index: usize,
  resolve: Option<Value>,
) -> Result<(), RuntimeError> {
  let waiters = atomics_waiters_array(&info.buffer);
  waiters
    .borrow_mut()
    .push(Some(atomics_make_waiter_record(index, resolve)));
  Ok(())
}

/// Coerce a value to the appropriate type for the typed array view (Number or BigInt).
///
/// # Errors
///
/// Returns a [`RuntimeError`] if type conversion fails.
pub(crate) fn atomics_coerce_view_value(
  vm: &mut Vm,
  info: &AtomicsViewInfo,
  value: &Value,
) -> Result<Value, RuntimeError> {
  match info.value_kind {
    AtomicsValueKind::Number { bits, signed } => atomics_value_to_integer(vm, value, bits, signed),
    AtomicsValueKind::BigInt { signed } => atomics_value_to_bigint(vm, value, signed),
  }
}

/// Read and normalize the current value at an index in the typed array.
///
/// # Errors
///
/// Returns a [`RuntimeError`] if the read or normalization fails.
pub(crate) fn atomics_normalized_current(
  vm: &mut Vm,
  info: &AtomicsViewInfo,
  index: usize,
) -> Result<Value, RuntimeError> {
  let current = atomics_read_view_slot(vm, info, index)?;
  atomics_coerce_view_value(vm, info, &current)
}

/// Read the current value at an index in the typed array, equivalent to `Atomics.load()`.
///
/// # Errors
///
/// Returns a [`RuntimeError`] if the read fails.
pub(crate) fn atomics_load_current(
  vm: &mut Vm,
  info: &AtomicsViewInfo,
  index: usize,
) -> Result<Value, RuntimeError> {
  atomics_normalized_current(vm, info, index)
}

/// Store a coerced value at an index in the typed array, equivalent to `Atomics.store()`.
///
/// # Errors
///
/// Returns a [`RuntimeError`] if the write fails.
pub(crate) fn atomics_store_current(
  vm: &mut Vm,
  info: &AtomicsViewInfo,
  index: usize,
  value: &Value,
) -> Result<Value, RuntimeError> {
  let stored = atomics_coerce_view_value(vm, info, value)?;
  atomics_write_view_slot(vm, info, index, stored.clone())?;
  Ok(stored)
}

/// Exchange the current value at an index with a new value, returning the old value.
///
/// # Errors
///
/// Returns a [`RuntimeError`] if the operation fails.
pub(crate) fn atomics_exchange_current(
  vm: &mut Vm,
  info: &AtomicsViewInfo,
  index: usize,
  value: &Value,
) -> Result<Value, RuntimeError> {
  let current = atomics_normalized_current(vm, info, index)?;
  let stored = atomics_coerce_view_value(vm, info, value)?;
  atomics_write_view_slot(vm, info, index, stored)?;
  Ok(current)
}

/// Perform a binary operation (add, sub, and, or, xor) at an index and return the old value.
///
/// # Errors
///
/// Returns a [`RuntimeError`] if the operation fails.
pub(crate) fn atomics_binary_update(
  vm: &mut Vm,
  info: &AtomicsViewInfo,
  index: usize,
  value: &Value,
  op: &str,
) -> Result<Value, RuntimeError> {
  let current = atomics_normalized_current(vm, info, index)?;
  let operand = atomics_coerce_view_value(vm, info, value)?;
  let result = match (info.value_kind, current.clone(), operand) {
    (
      AtomicsValueKind::Number { bits, signed },
      Value::Number(current_n),
      Value::Number(operand_n),
    ) => {
      let lhs = current_n.trunc().to_i128().unwrap_or_else(|| {
        if current_n.is_sign_negative() {
          i128::MIN
        } else {
          i128::MAX
        }
      });
      let rhs = operand_n.trunc().to_i128().unwrap_or_else(|| {
        if operand_n.is_sign_negative() {
          i128::MIN
        } else {
          i128::MAX
        }
      });
      let raw = match op {
        "add" => lhs + rhs,
        "sub" => lhs - rhs,
        "and" => lhs & rhs,
        "or" => lhs | rhs,
        "xor" => lhs ^ rhs,
        _ => lhs,
      };
      Value::Number(
        atomics_normalize_integer(raw, bits, signed)
          .to_f64()
          .unwrap_or(f64::NAN),
      )
    }
    (AtomicsValueKind::BigInt { signed }, Value::BigInt(current_n), Value::BigInt(operand_n)) => {
      let raw = match op {
        "add" => current_n + operand_n,
        "sub" => current_n - operand_n,
        "and" => current_n & operand_n,
        "or" => current_n | operand_n,
        "xor" => current_n ^ operand_n,
        _ => current_n,
      };
      Value::BigInt(atomics_normalize_bigint(raw, signed))
    }
    _ => return Err(vm.throw_type_error("Atomics type coercion failed")),
  };
  atomics_write_view_slot(vm, info, index, result)?;
  Ok(current)
}

/// Compare and exchange the value at an index, returning the old value.
///
/// # Errors
///
/// Returns a [`RuntimeError`] if the operation fails.
pub(crate) fn atomics_compare_exchange(
  vm: &mut Vm,
  info: &AtomicsViewInfo,
  index: usize,
  expected: &Value,
  replacement: &Value,
) -> Result<Value, RuntimeError> {
  let current = atomics_normalized_current(vm, info, index)?;
  let expected = atomics_coerce_view_value(vm, info, expected)?;
  if current == expected {
    let replacement = atomics_coerce_view_value(vm, info, replacement)?;
    atomics_write_view_slot(vm, info, index, replacement)?;
  }
  Ok(current)
}
