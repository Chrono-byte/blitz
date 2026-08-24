// SPDX-License-Identifier: MPL-2.0
//! Internal storage and branding for the byte-oriented ArrayBuffer views.
//!
//! The names in this module are VM slots, not ECMAScript properties. Keeping
//! them here prevents a script from manufacturing a typed array by assigning
//! properties with internal-looking names.

use std::cell::RefCell;
use std::rc::Rc;

use num_bigint::{BigInt, Sign};
use num_traits::ToPrimitive;

use crate::value::{
  ArrayData, ObjectData, PropertyDescriptor, PropertyKey, Uint8ArrayByteView, Value,
};
use crate::{RuntimeError, Vm};

pub(crate) const ARRAY_BUFFER_DATA_SLOT: &str = "[[ArrayBufferData]]";
pub(crate) const ARRAY_BUFFER_SHARED_SLOT: &str = "[[IsSharedArrayBuffer]]";
pub(crate) const TYPED_ARRAY_DATA_SLOT: &str = "[[TypedArrayData]]";
pub(crate) const TYPED_ARRAY_KIND_SLOT: &str = "[[TypedArrayKind]]";
pub(crate) const TYPED_ARRAY_BYTES_PER_ELEMENT_SLOT: &str = "[[TypedArrayBytesPerElement]]";
pub(crate) const TYPED_ARRAY_LENGTH_SLOT: &str = "[[TypedArrayLength]]";
pub(crate) const TYPED_ARRAY_BYTE_LENGTH_SLOT: &str = "[[TypedArrayByteLength]]";
pub(crate) const TYPED_ARRAY_BYTE_OFFSET_SLOT: &str = "[[TypedArrayByteOffset]]";
pub(crate) const TYPED_ARRAY_BUFFER_SLOT: &str = "[[TypedArrayBuffer]]";

pub(crate) struct ByteViewDetails {
  pub(crate) object: Rc<RefCell<ObjectData>>,
  pub(crate) buffer: Value,
  pub(crate) data: Rc<RefCell<ArrayData>>,
  pub(crate) kind: String,
  pub(crate) length: usize,
  pub(crate) byte_offset: usize,
  pub(crate) bytes_per_element: usize,
  pub(crate) prototype: Value,
}

pub(crate) struct ByteViewIndexInfo {
  pub(crate) data: Rc<RefCell<ArrayData>>,
  pub(crate) kind: String,
  pub(crate) byte_offset: usize,
  pub(crate) length: usize,
  pub(crate) bytes_per_element: usize,
}

fn internal_slot(
  object: &Rc<RefCell<ObjectData>>,
  name: &'static str,
) -> Option<Value> {
  object.borrow().internal_slot(name).cloned()
}

pub(crate) fn array_buffer_backing(value: &Value) -> Option<Rc<RefCell<ArrayData>>> {
  let Value::Object(object) = value else {
    return None;
  };
  match internal_slot(object, ARRAY_BUFFER_DATA_SLOT)? {
    Value::Array(data, _) => Some(data),
    Value::Null => None,
    _ => None,
  }
}

pub(crate) fn is_array_buffer(value: &Value) -> bool {
  let Value::Object(object) = value else {
    return false;
  };
  let object = object.borrow();
  object
    .internal_slot(ARRAY_BUFFER_DATA_SLOT)
    .is_some()
    && matches!(
      object.internal_slot(ARRAY_BUFFER_SHARED_SLOT),
      Some(Value::Bool(_))
    )
}

pub(crate) fn array_buffer_is_shared(value: &Value) -> Option<bool> {
  let Value::Object(object) = value else {
    return None;
  };
  match internal_slot(object, ARRAY_BUFFER_SHARED_SLOT)? {
    Value::Bool(shared) => Some(shared),
    _ => None,
  }
}

pub(crate) fn new_array_buffer(
  prototype: Value,
  length: usize,
  shared: bool,
) -> (Value, Rc<RefCell<ArrayData>>) {
  let data = Rc::new(RefCell::new(ArrayData::from(vec![
    Some(Value::Number(0.0));
    length
  ])));
  let mut object = ObjectData::with_prototype(prototype);
  object.set_internal_slot(
    ARRAY_BUFFER_DATA_SLOT,
    Value::Array(Rc::clone(&data), Rc::new(RefCell::new(ObjectData::new()))),
  );
  object.set_internal_slot(ARRAY_BUFFER_SHARED_SLOT, Value::Bool(shared));
  object.properties.insert(
    PropertyKey::String("byteLength".to_string().into()),
    PropertyDescriptor::ordinary_data(Value::Number(length as f64)),
  );
  (Value::Object(Rc::new(RefCell::new(object))), data)
}

pub(crate) fn new_byte_view(
  prototype: Value,
  buffer: Value,
  data: Rc<RefCell<ArrayData>>,
  kind: &str,
  length: usize,
  byte_offset: usize,
) -> Value {
  let bytes_per_element = typed_array_bytes_per_element(kind).expect("known typed array kind");
  let mut object = ObjectData::with_prototype(prototype);
  object.set_internal_slot(
    TYPED_ARRAY_DATA_SLOT,
    Value::Array(Rc::clone(&data), Rc::new(RefCell::new(ObjectData::new()))),
  );
  object.set_internal_slot(
    TYPED_ARRAY_KIND_SLOT,
    Value::String(kind.to_string().into()),
  );
  object.set_internal_slot(
    TYPED_ARRAY_BYTES_PER_ELEMENT_SLOT,
    Value::Number(bytes_per_element as f64),
  );
  object.set_internal_slot(TYPED_ARRAY_LENGTH_SLOT, Value::Number(length as f64));
  object.set_internal_slot(
    TYPED_ARRAY_BYTE_LENGTH_SLOT,
    Value::Number((length * bytes_per_element) as f64),
  );
  object.set_internal_slot(
    TYPED_ARRAY_BYTE_OFFSET_SLOT,
    Value::Number(byte_offset as f64),
  );
  object.set_internal_slot(TYPED_ARRAY_BUFFER_SLOT, buffer);

  Value::Object(Rc::new(RefCell::new(object)))
}

pub(crate) fn byte_view_metadata(value: &Value) -> Option<(Value, String, usize, usize)> {
  let Value::Object(object) = value else {
    return None;
  };
  let kind = match internal_slot(object, TYPED_ARRAY_KIND_SLOT)? {
    Value::String(kind) if typed_array_bytes_per_element(kind.as_str_lossy()).is_some() => {
      kind.to_string()
    }
    _ => return None,
  };
  let buffer = internal_slot(object, TYPED_ARRAY_BUFFER_SLOT)?;
  let length = match internal_slot(object, TYPED_ARRAY_LENGTH_SLOT)? {
    Value::Number(length) => length.to_usize()?,
    _ => return None,
  };
  let byte_offset = match internal_slot(object, TYPED_ARRAY_BYTE_OFFSET_SLOT)? {
    Value::Number(offset) => offset.to_usize()?,
    _ => return None,
  };
  Some((buffer, kind, length, byte_offset))
}

pub(crate) fn byte_view_details(value: &Value) -> Option<ByteViewDetails> {
  let Value::Object(object) = value else {
    return None;
  };
  let kind = match internal_slot(object, TYPED_ARRAY_KIND_SLOT)? {
    Value::String(kind) if typed_array_bytes_per_element(kind.as_str_lossy()).is_some() => kind,
    _ => return None,
  };
  let buffer = internal_slot(object, TYPED_ARRAY_BUFFER_SLOT)?;
  let data = array_buffer_backing(&buffer)?;
  if let Value::Array(view_data, _) = internal_slot(object, TYPED_ARRAY_DATA_SLOT)?
    && !Rc::ptr_eq(&view_data, &data)
  {
    return None;
  }
  let length = match internal_slot(object, TYPED_ARRAY_LENGTH_SLOT)? {
    Value::Number(length) => length.to_usize()?,
    _ => return None,
  };
  let byte_offset = match internal_slot(object, TYPED_ARRAY_BYTE_OFFSET_SLOT)? {
    Value::Number(offset) => offset.to_usize()?,
    _ => return None,
  };
  let bytes_per_element = match internal_slot(object, TYPED_ARRAY_BYTES_PER_ELEMENT_SLOT)? {
    Value::Number(bytes) => bytes.to_usize()?,
    _ => return None,
  };
  if bytes_per_element != typed_array_bytes_per_element(kind.as_str_lossy())? {
    return None;
  }
  Some(ByteViewDetails {
    object: Rc::clone(object),
    buffer,
    data,
    kind: kind.to_string(),
    length,
    byte_offset,
    bytes_per_element,
    prototype: object.borrow().get_prototype(),
  })
}

impl Vm {
  /// Return opaque host access to a genuine attached `Uint8Array` view.
  ///
  /// `Int8Array`, `Uint8ClampedArray`, detached views, proxies, and objects
  /// that merely spoof internal-looking properties are rejected.
  #[must_use]
  pub fn uint8_array_byte_view(
    &self,
    value: &Value,
  ) -> Option<Uint8ArrayByteView> {
    let details = byte_view_details(value)?;
    if details.kind != "Uint8Array" {
      return None;
    }
    Some(Uint8ArrayByteView::new(
      details.buffer,
      details.data,
      details.byte_offset,
      details.length,
    ))
  }
}

pub(crate) fn byte_view_index_info(value: &Value) -> Option<ByteViewIndexInfo> {
  let details = byte_view_details(value)?;
  Some(ByteViewIndexInfo {
    data: details.data,
    kind: details.kind,
    byte_offset: details.byte_offset,
    length: details.length,
    bytes_per_element: details.bytes_per_element,
  })
}

pub(crate) fn byte_view_index_value(
  value: &Value,
  index: usize,
) -> Option<Value> {
  let info = byte_view_index_info(value)?;
  if index >= info.length {
    return None;
  }
  let start = info
    .byte_offset
    .checked_add(index.checked_mul(info.bytes_per_element)?)?;
  let data = info.data.borrow();
  let bytes = (0..info.bytes_per_element)
    .map(|offset| {
      data
        .get(start + offset)
        .and_then(|slot| slot.as_ref())
        .and_then(|value| match value {
          Value::Number(number) => number.to_u8(),
          _ => None,
        })
        .unwrap_or(0)
    })
    .collect::<Vec<_>>();
  decode_typed_element(&info.kind, &bytes)
}

pub(crate) fn write_byte_view_index(
  vm: &mut Vm,
  info: &ByteViewIndexInfo,
  index: usize,
  value: &Value,
) -> Result<(), RuntimeError> {
  if index >= info.length {
    return Ok(());
  }
  let bytes = encode_typed_element(vm, &info.kind, value)?;
  let start = info.byte_offset + index * info.bytes_per_element;
  let mut data = info.data.borrow_mut();
  for (offset, byte) in bytes.into_iter().enumerate() {
    if let Some(slot) = data.get_mut(start + offset) {
      *slot = Some(Value::Number(f64::from(byte)));
    }
  }
  Ok(())
}

pub(crate) fn typed_array_bytes_per_element(kind: &str) -> Option<usize> {
  match kind {
    "Uint8Array" | "Int8Array" | "Uint8ClampedArray" => Some(1),
    "Uint16Array" | "Int16Array" => Some(2),
    "Uint32Array" | "Int32Array" | "Float32Array" => Some(4),
    "Float64Array" | "BigInt64Array" | "BigUint64Array" => Some(8),
    _ => None,
  }
}

pub(crate) fn decode_typed_element(
  kind: &str,
  bytes: &[u8],
) -> Option<Value> {
  let value = match kind {
    "Uint8Array" => Value::Number(f64::from(*bytes.first()?)),
    "Int8Array" => Value::Number(f64::from(*bytes.first()? as i8)),
    "Uint8ClampedArray" => Value::Number(f64::from(*bytes.first()?)),
    "Uint16Array" => Value::Number(f64::from(u16::from_le_bytes(bytes.try_into().ok()?))),
    "Int16Array" => Value::Number(f64::from(i16::from_le_bytes(bytes.try_into().ok()?))),
    "Uint32Array" => Value::Number(f64::from(u32::from_le_bytes(bytes.try_into().ok()?))),
    "Int32Array" => Value::Number(f64::from(i32::from_le_bytes(bytes.try_into().ok()?))),
    "Float32Array" => Value::Number(f64::from(f32::from_le_bytes(bytes.try_into().ok()?))),
    "Float64Array" => Value::Number(f64::from_le_bytes(bytes.try_into().ok()?)),
    "BigInt64Array" => Value::BigInt(BigInt::from(i64::from_le_bytes(bytes.try_into().ok()?))),
    "BigUint64Array" => Value::BigInt(BigInt::from(u64::from_le_bytes(bytes.try_into().ok()?))),
    _ => return None,
  };
  Some(value)
}

pub(crate) fn encode_typed_element(
  vm: &mut Vm,
  kind: &str,
  value: &Value,
) -> Result<Vec<u8>, RuntimeError> {
  if matches!(kind, "BigInt64Array" | "BigUint64Array") {
    let value = crate::ecmascript::builtins::primitives::to_bigint(vm, value)?;
    let modulus = BigInt::from(1u8) << 64;
    let mut bits: BigInt = value % &modulus;
    if bits.sign() == Sign::Minus {
      bits += &modulus;
    }
    let bits = bits
      .to_u64()
      .expect("BigInt modulo 2^64 is representable");
    return Ok(match kind {
      "BigInt64Array" | "BigUint64Array" => bits.to_le_bytes().to_vec(),
      _ => unreachable!(),
    });
  }
  let number = vm.to_number_value(value)?;
  let bytes = match kind {
    "Uint8Array" => vec![number.trunc().rem_euclid(256.0) as u8],
    "Int8Array" => vec![number.trunc().rem_euclid(256.0) as u8],
    "Uint8ClampedArray" => vec![encode_uint8_clamped(number)],
    "Uint16Array" => (number.trunc().rem_euclid(65_536.0) as u16)
      .to_le_bytes()
      .to_vec(),
    "Int16Array" => (number.trunc().rem_euclid(65_536.0) as u16)
      .to_le_bytes()
      .to_vec(),
    "Uint32Array" => (number.trunc().rem_euclid(4_294_967_296.0) as u32)
      .to_le_bytes()
      .to_vec(),
    "Int32Array" => (number.trunc().rem_euclid(4_294_967_296.0) as u32)
      .to_le_bytes()
      .to_vec(),
    "Float32Array" => (number as f32).to_le_bytes().to_vec(),
    "Float64Array" => number.to_le_bytes().to_vec(),
    _ => return Err(vm.throw_type_error("Unknown typed array kind")),
  };
  Ok(bytes)
}

fn encode_uint8_clamped(number: f64) -> u8 {
  if number.is_nan() || number <= 0.0 {
    return 0;
  }
  if number >= 255.0 {
    return 255;
  }
  let floor = number.floor();
  let fraction = number - floor;
  if fraction > 0.5 || (fraction == 0.5 && (floor as u8) % 2 == 1) {
    floor as u8 + 1
  } else {
    floor as u8
  }
}

pub(crate) fn is_byte_view(value: &Value) -> bool {
  let Value::Object(object) = value else {
    return false;
  };
  matches!(
    internal_slot(object, TYPED_ARRAY_KIND_SLOT),
    Some(Value::String(kind)) if typed_array_bytes_per_element(kind.as_str_lossy()).is_some()
  )
}

pub(crate) fn canonical_numeric_index_from_key(key: &PropertyKey) -> Option<f64> {
  let PropertyKey::String(name) = key else {
    return None;
  };
  let name = name.as_str_lossy();
  if name == "-0" {
    return Some(-0.0);
  }
  let number = match name {
    "NaN" => f64::NAN,
    "Infinity" => f64::INFINITY,
    "-Infinity" => f64::NEG_INFINITY,
    source => source.parse::<f64>().ok()?,
  };
  (crate::runtime_utils::js_number_to_string(number) == name).then_some(number)
}

pub(crate) fn byte_view_index_from_key(key: &PropertyKey) -> Option<usize> {
  let number = canonical_numeric_index_from_key(key)?;
  if !number.is_finite() || number.is_sign_negative() || number.fract() != 0.0 {
    return None;
  }
  number.to_usize()
}
