// SPDX-License-Identifier: MPL-2.0
//! Internal storage and branding for the byte-oriented ArrayBuffer views.
//!
//! The names in this module are VM slots, not ECMAScript properties. Keeping
//! them here prevents a script from manufacturing a typed array by assigning
//! properties with internal-looking names.

use std::cell::RefCell;
use std::rc::Rc;

use num_traits::ToPrimitive;

use crate::runtime_utils::canonical_index_key;
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
  pub(crate) prototype: Value,
}

pub(crate) struct ByteViewIndexInfo {
  pub(crate) data: Rc<RefCell<ArrayData>>,
  pub(crate) kind: String,
  pub(crate) byte_offset: usize,
  pub(crate) length: usize,
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
  let mut object = ObjectData::with_prototype(prototype);
  object.set_internal_slot(
    TYPED_ARRAY_DATA_SLOT,
    Value::Array(Rc::clone(&data), Rc::new(RefCell::new(ObjectData::new()))),
  );
  object.set_internal_slot(
    TYPED_ARRAY_KIND_SLOT,
    Value::String(kind.to_string().into()),
  );
  object.set_internal_slot(TYPED_ARRAY_BYTES_PER_ELEMENT_SLOT, Value::Number(1.0));
  object.set_internal_slot(TYPED_ARRAY_LENGTH_SLOT, Value::Number(length as f64));
  object.set_internal_slot(TYPED_ARRAY_BYTE_LENGTH_SLOT, Value::Number(length as f64));
  object.set_internal_slot(
    TYPED_ARRAY_BYTE_OFFSET_SLOT,
    Value::Number(byte_offset as f64),
  );
  object.set_internal_slot(TYPED_ARRAY_BUFFER_SLOT, buffer.clone());

  for (name, value) in [
    ("length", Value::Number(length as f64)),
    ("byteLength", Value::Number(length as f64)),
    ("byteOffset", Value::Number(byte_offset as f64)),
    ("buffer", buffer),
  ] {
    object.properties.insert(
      PropertyKey::String(name.to_string().into()),
      PropertyDescriptor::ordinary_data(value),
    );
  }
  Value::Object(Rc::new(RefCell::new(object)))
}

pub(crate) fn byte_view_details(value: &Value) -> Option<ByteViewDetails> {
  let Value::Object(object) = value else {
    return None;
  };
  let kind = match internal_slot(object, TYPED_ARRAY_KIND_SLOT)? {
    Value::String(kind)
      if matches!(
        kind.as_str_lossy(),
        "Uint8Array" | "Int8Array" | "Uint8ClampedArray"
      ) =>
    {
      kind
    }
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
  Some(ByteViewDetails {
    object: Rc::clone(object),
    buffer,
    data,
    kind: kind.to_string(),
    length,
    byte_offset,
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
  let byte = info
    .data
    .borrow()
    .get(info.byte_offset.checked_add(index)?)
    .and_then(|slot| slot.as_ref())
    .and_then(|value| match value {
      Value::Number(number) => number.to_u8(),
      _ => None,
    })
    .unwrap_or(0);
  Some(decode_byte(&info.kind, byte))
}

pub(crate) fn decode_byte(
  kind: &str,
  byte: u8,
) -> Value {
  if kind == "Int8Array" && byte >= 128 {
    Value::Number(f64::from(byte) - 256.0)
  } else {
    Value::Number(f64::from(byte))
  }
}

pub(crate) fn encode_byte(
  vm: &mut Vm,
  kind: &str,
  value: &Value,
) -> Result<u8, RuntimeError> {
  let number = vm.to_number_value(value)?;
  if number.is_nan() || number == 0.0 {
    return Ok(0);
  }
  if kind == "Uint8ClampedArray" {
    if number <= 0.0 {
      return Ok(0);
    }
    if number >= 255.0 {
      return Ok(255);
    }
    let floor = number.floor();
    let fraction = number - floor;
    return Ok(
      if fraction > 0.5 || (fraction == 0.5 && (floor as u8) % 2 == 1) {
        floor as u8 + 1
      } else {
        floor as u8
      },
    );
  }
  Ok(number.trunc().rem_euclid(256.0) as u8)
}

pub(crate) fn byte_view_index_from_key(key: &PropertyKey) -> Option<usize> {
  let PropertyKey::String(name) = key else {
    return None;
  };
  canonical_index_key(name)?.to_usize()
}
