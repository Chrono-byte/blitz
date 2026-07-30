// SPDX-License-Identifier: MPL-2.0

use std::cell::RefCell;
use std::rc::Rc;

use num_bigint::BigInt;
use num_traits::ToPrimitive;

use crate::ecmascript::semantics::{
  array_buffer_backing, is_array_buffer, set_builtin_length, set_builtin_name, usize_to_f64,
};
use crate::runtime_utils::{static_builtin_arg_offset, to_index};
use crate::value::{ArrayData, BuiltinFn, ObjectData, PropertyDescriptor, PropertyKey, Value};

const DATA_VIEW_BUFFER: &str = "[[DataViewBuffer]]";
const DATA_VIEW_BYTE_OFFSET: &str = "[[DataViewByteOffset]]";
const DATA_VIEW_BYTE_LENGTH: &str = "[[DataViewByteLength]]";

pub(crate) fn is_data_view(value: &Value) -> bool {
  let Value::Object(object) = value else {
    return false;
  };
  object
    .borrow()
    .internal_slot(DATA_VIEW_BUFFER)
    .is_some()
}

fn data_view_details(
  vm: &mut crate::Vm,
  receiver: &Value,
  byte_offset: &Value,
  size: usize,
) -> Result<(Rc<RefCell<ArrayData>>, usize), crate::RuntimeError> {
  let Value::Object(object) = receiver else {
    return Err(vm.throw_type_error("DataView method called on incompatible receiver"));
  };
  let object = object.borrow();
  let Some(buffer) = object.internal_slot(DATA_VIEW_BUFFER) else {
    return Err(vm.throw_type_error("DataView method called on incompatible receiver"));
  };
  let base = match object.internal_slot(DATA_VIEW_BYTE_OFFSET) {
    Some(Value::Number(value)) => value.to_usize(),
    _ => None,
  }
  .ok_or_else(|| vm.throw_type_error("DataView method called on incompatible receiver"))?;
  let length = match object.internal_slot(DATA_VIEW_BYTE_LENGTH) {
    Some(Value::Number(value)) => value.to_usize(),
    _ => None,
  }
  .ok_or_else(|| vm.throw_type_error("DataView method called on incompatible receiver"))?;
  let data = array_buffer_backing(buffer)
    .ok_or_else(|| vm.throw_type_error("Cannot perform DataView operation on a detached buffer"))?;
  let offset = to_index(vm, byte_offset)?;
  if offset
    .checked_add(size)
    .is_none_or(|end| end > length)
  {
    return Err(vm.throw_range_error("DataView access is outside the view"));
  }
  Ok((data, base + offset))
}

fn read_bytes(
  data: &Rc<RefCell<ArrayData>>,
  offset: usize,
  size: usize,
) -> Vec<u8> {
  data.borrow().clone_elements()[offset..offset + size]
    .iter()
    .map(|value| match value {
      Some(Value::Number(value)) => value.to_u8().unwrap_or(0),
      _ => 0,
    })
    .collect()
}

fn write_bytes(
  data: &Rc<RefCell<ArrayData>>,
  offset: usize,
  bytes: &[u8],
) {
  let mut values = data.borrow().clone_elements();
  for (index, byte) in bytes.iter().enumerate() {
    values[offset + index] = Some(Value::Number(f64::from(*byte)));
  }
  *data.borrow_mut() = ArrayData::from(values);
}

fn little_endian(args: &[Value]) -> bool {
  args.get(2).is_some_and(Value::is_truthy)
}

fn install_accessor(
  prototype: &Rc<RefCell<ObjectData>>,
  function_prototype: &Rc<RefCell<ObjectData>>,
  name: &str,
  getter: BuiltinFn,
) {
  prototype.borrow_mut().properties.insert(
    PropertyKey::String(name.into()),
    PropertyDescriptor::Accessor {
      get: Some(crate::ecmascript::semantics::builtin_object(
        getter,
        function_prototype,
      )),
      set: None,
      enumerable: false,
      configurable: true,
    },
  );
}

fn install_getter(
  prototype: &Rc<RefCell<ObjectData>>,
  function_prototype: &Rc<RefCell<ObjectData>>,
  name: &str,
  size: usize,
  decode: fn(&[u8], bool) -> Value,
) {
  let builtin = crate::ecmascript::semantics::builtin_object(
    Rc::new(move |vm, args| {
      let receiver = args.first().cloned().unwrap_or(Value::Undefined);
      let offset = args.get(1).cloned().unwrap_or(Value::Undefined);
      let (data, offset) = data_view_details(vm, &receiver, &offset, size)?;
      Ok(decode(
        &read_bytes(&data, offset, size),
        little_endian(args),
      ))
    }),
    function_prototype,
  );
  set_builtin_name(&builtin, name);
  set_builtin_length(&builtin, 1.0);
  prototype.borrow_mut().properties.insert(
    PropertyKey::String(name.into()),
    PropertyDescriptor::builtin_data(builtin),
  );
}

fn install_setter(
  prototype: &Rc<RefCell<ObjectData>>,
  function_prototype: &Rc<RefCell<ObjectData>>,
  name: &str,
  size: usize,
  encode: fn(&mut crate::Vm, &Value, bool) -> Result<Vec<u8>, crate::RuntimeError>,
) {
  let builtin = crate::ecmascript::semantics::builtin_object(
    Rc::new(move |vm, args| {
      let receiver = args.first().cloned().unwrap_or(Value::Undefined);
      let offset = args.get(1).cloned().unwrap_or(Value::Undefined);
      let value = args.get(2).cloned().unwrap_or(Value::Undefined);
      let little_endian = args.get(3).is_some_and(Value::is_truthy);
      let (data, offset) = data_view_details(vm, &receiver, &offset, size)?;
      let bytes = encode(vm, &value, little_endian)?;
      write_bytes(&data, offset, &bytes);
      Ok(Value::Undefined)
    }),
    function_prototype,
  );
  set_builtin_name(&builtin, name);
  set_builtin_length(&builtin, 2.0);
  prototype.borrow_mut().properties.insert(
    PropertyKey::String(name.into()),
    PropertyDescriptor::builtin_data(builtin),
  );
}

fn unsigned(
  bytes: &[u8],
  little: bool,
) -> u64 {
  let bytes = if little {
    bytes.iter().rev().copied().collect()
  } else {
    bytes.to_vec()
  };
  bytes
    .into_iter()
    .fold(0_u64, |value, byte| (value << 8) | u64::from(byte))
}

fn signed(
  bytes: &[u8],
  little: bool,
) -> i64 {
  let unsigned = unsigned(bytes, little);
  let bits = bytes.len() * 8;
  if unsigned & (1_u64 << (bits - 1)) != 0 {
    (unsigned as i128 - (1_i128 << bits)) as i64
  } else {
    unsigned as i64
  }
}

fn integer_bytes(
  value: i128,
  size: usize,
  little: bool,
) -> Vec<u8> {
  let mut bytes = (0..size)
    .map(|index| ((value >> (index * 8)) & 0xff) as u8)
    .collect::<Vec<_>>();
  if !little {
    bytes.reverse();
  }
  bytes
}

fn number_bytes(
  vm: &mut crate::Vm,
  value: &Value,
  size: usize,
  little: bool,
) -> Result<Vec<u8>, crate::RuntimeError> {
  let value = vm.to_number_value(value)?;
  Ok(integer_bytes(value.trunc() as i128, size, little))
}

fn bigint_bytes(
  vm: &mut crate::Vm,
  value: &Value,
  size: usize,
  little: bool,
) -> Result<Vec<u8>, crate::RuntimeError> {
  let Value::BigInt(value) = value else {
    return Err(vm.throw_type_error("DataView BigInt methods require a BigInt value"));
  };
  let modulus = BigInt::from(1_u8) << (size * 8);
  let normalized = ((value % &modulus) + &modulus) % &modulus;
  let mut bytes = normalized.to_u64().unwrap_or(0).to_le_bytes()[..size].to_vec();
  if !little {
    bytes.reverse();
  }
  Ok(bytes)
}

fn get_u8(
  bytes: &[u8],
  _: bool,
) -> Value {
  Value::Number(f64::from(bytes[0]))
}
fn get_i8(
  bytes: &[u8],
  _: bool,
) -> Value {
  Value::Number(f64::from(bytes[0] as i8))
}
fn get_u16(
  bytes: &[u8],
  little: bool,
) -> Value {
  Value::Number(unsigned(bytes, little) as f64)
}
fn get_i16(
  bytes: &[u8],
  little: bool,
) -> Value {
  Value::Number(signed(bytes, little) as f64)
}
fn get_u32(
  bytes: &[u8],
  little: bool,
) -> Value {
  Value::Number(unsigned(bytes, little) as f64)
}
fn get_i32(
  bytes: &[u8],
  little: bool,
) -> Value {
  Value::Number(signed(bytes, little) as f64)
}
fn get_f32(
  bytes: &[u8],
  little: bool,
) -> Value {
  let bits = unsigned(bytes, little) as u32;
  Value::Number(f64::from(f32::from_bits(bits)))
}
fn get_f64(
  bytes: &[u8],
  little: bool,
) -> Value {
  Value::Number(f64::from_bits(unsigned(bytes, little)))
}
fn get_bu64(
  bytes: &[u8],
  little: bool,
) -> Value {
  Value::BigInt(BigInt::from(unsigned(bytes, little)))
}
fn get_bi64(
  bytes: &[u8],
  little: bool,
) -> Value {
  Value::BigInt(BigInt::from(signed(bytes, little)))
}
fn set_u8(
  vm: &mut crate::Vm,
  value: &Value,
  _: bool,
) -> Result<Vec<u8>, crate::RuntimeError> {
  number_bytes(vm, value, 1, true)
}
fn set_i8(
  vm: &mut crate::Vm,
  value: &Value,
  _: bool,
) -> Result<Vec<u8>, crate::RuntimeError> {
  number_bytes(vm, value, 1, true)
}
fn set_u16(
  vm: &mut crate::Vm,
  value: &Value,
  little: bool,
) -> Result<Vec<u8>, crate::RuntimeError> {
  number_bytes(vm, value, 2, little)
}
fn set_i16(
  vm: &mut crate::Vm,
  value: &Value,
  little: bool,
) -> Result<Vec<u8>, crate::RuntimeError> {
  number_bytes(vm, value, 2, little)
}
fn set_u32(
  vm: &mut crate::Vm,
  value: &Value,
  little: bool,
) -> Result<Vec<u8>, crate::RuntimeError> {
  number_bytes(vm, value, 4, little)
}
fn set_i32(
  vm: &mut crate::Vm,
  value: &Value,
  little: bool,
) -> Result<Vec<u8>, crate::RuntimeError> {
  number_bytes(vm, value, 4, little)
}
fn set_f32(
  vm: &mut crate::Vm,
  value: &Value,
  little: bool,
) -> Result<Vec<u8>, crate::RuntimeError> {
  Ok(integer_bytes(
    f32::to_bits(vm.to_number_value(value)? as f32) as i128,
    4,
    little,
  ))
}
fn set_f64(
  vm: &mut crate::Vm,
  value: &Value,
  little: bool,
) -> Result<Vec<u8>, crate::RuntimeError> {
  Ok(integer_bytes(
    f64::to_bits(vm.to_number_value(value)?) as i128,
    8,
    little,
  ))
}
fn set_bi64(
  vm: &mut crate::Vm,
  value: &Value,
  little: bool,
) -> Result<Vec<u8>, crate::RuntimeError> {
  bigint_bytes(vm, value, 8, little)
}
fn set_bu64(
  vm: &mut crate::Vm,
  value: &Value,
  little: bool,
) -> Result<Vec<u8>, crate::RuntimeError> {
  bigint_bytes(vm, value, 8, little)
}

pub(crate) fn install_data_view(
  object_proto_map: &Rc<RefCell<ObjectData>>,
  function_proto_map: &Rc<RefCell<ObjectData>>,
) -> (Value, Rc<RefCell<ObjectData>>) {
  let dataview_proto_map = Rc::new(RefCell::new(ObjectData::with_prototype(Value::Object(
    Rc::clone(object_proto_map),
  ))));
  let dataview_ctor_map = Rc::new(RefCell::new(ObjectData::with_prototype(Value::Object(
    Rc::clone(function_proto_map),
  ))));
  let dataview_ctor_fn: BuiltinFn = {
    let proto_map_rc = Rc::clone(&dataview_proto_map);
    Rc::new(move |vm, args| {
      let off = static_builtin_arg_offset(vm, args, "DataView");
      let buffer = args.get(off).cloned().unwrap_or(Value::Undefined);
      if !is_array_buffer(&buffer) {
        return Err(
          vm.throw_type_error("DataView constructor requires ArrayBuffer or SharedArrayBuffer"),
        );
      }
      let buffer_byte_length = array_buffer_backing(&buffer)
        .ok_or_else(|| vm.throw_range_error("Invalid DataView length"))?
        .borrow()
        .len();
      let byte_offset_arg = args
        .get(off + 1)
        .cloned()
        .unwrap_or(Value::Undefined);
      let byte_offset = if matches!(byte_offset_arg, Value::Undefined) {
        0
      } else {
        to_index(vm, &byte_offset_arg)?
      };
      let byte_length_arg = args
        .get(off + 2)
        .cloned()
        .unwrap_or(Value::Undefined);
      let byte_length = if matches!(byte_length_arg, Value::Undefined) {
        buffer_byte_length
          .checked_sub(byte_offset)
          .ok_or_else(|| vm.throw_range_error("Invalid DataView length"))?
      } else {
        to_index(vm, &byte_length_arg)?
      };
      if byte_offset + byte_length > buffer_byte_length {
        return Err(vm.throw_range_error("Invalid DataView length"));
      }
      let mut dataview_data = ObjectData::with_prototype(Value::Object(Rc::clone(&proto_map_rc)));
      dataview_data.set_internal_slot(DATA_VIEW_BUFFER, buffer);
      dataview_data.set_internal_slot(
        DATA_VIEW_BYTE_OFFSET,
        Value::Number(usize_to_f64(byte_offset)),
      );
      dataview_data.set_internal_slot(
        DATA_VIEW_BYTE_LENGTH,
        Value::Number(usize_to_f64(byte_length)),
      );
      Ok(Value::Object(Rc::new(RefCell::new(dataview_data))))
    })
  };
  let dataview_ctor_val =
    Value::BuiltinObject(Rc::clone(&dataview_ctor_fn), Rc::clone(&dataview_ctor_map));
  dataview_ctor_map.borrow_mut().properties.insert(
    PropertyKey::String("prototype".to_string().into()),
    PropertyDescriptor::builtin_prototype(Value::Object(Rc::clone(&dataview_proto_map))),
  );
  set_builtin_name(&dataview_ctor_val, "DataView");
  set_builtin_length(&dataview_ctor_val, 1.0);
  dataview_proto_map.borrow_mut().properties.insert(
    PropertyKey::String("constructor".to_string().into()),
    PropertyDescriptor::builtin_data(dataview_ctor_val.clone()),
  );
  for name in ["buffer", "byteLength", "byteOffset"] {
    let field = name.to_string();
    install_accessor(
      &dataview_proto_map,
      function_proto_map,
      name,
      Rc::new(move |vm, args| {
        let receiver = args.first().cloned().unwrap_or(Value::Undefined);
        let Value::Object(object) = receiver else {
          return Err(vm.throw_type_error("DataView accessor called on incompatible receiver"));
        };
        let object = object.borrow();
        let Some(buffer) = object.internal_slot(DATA_VIEW_BUFFER) else {
          return Err(vm.throw_type_error("DataView accessor called on incompatible receiver"));
        };
        if array_buffer_backing(buffer).is_none() {
          return Err(
            vm.throw_type_error("Cannot perform DataView operation on a detached buffer"),
          );
        }
        match field.as_str() {
          "buffer" => Ok(buffer.clone()),
          "byteLength" => Ok(
            object
              .internal_slot(DATA_VIEW_BYTE_LENGTH)
              .cloned()
              .unwrap_or(Value::Undefined),
          ),
          _ => Ok(
            object
              .internal_slot(DATA_VIEW_BYTE_OFFSET)
              .cloned()
              .unwrap_or(Value::Undefined),
          ),
        }
      }),
    );
  }
  for (name, size, decode) in [
    ("getInt8", 1, get_i8 as fn(&[u8], bool) -> Value),
    ("getUint8", 1, get_u8),
    ("getInt16", 2, get_i16),
    ("getUint16", 2, get_u16),
    ("getInt32", 4, get_i32),
    ("getUint32", 4, get_u32),
    ("getFloat32", 4, get_f32),
    ("getFloat64", 8, get_f64),
    ("getBigInt64", 8, get_bi64),
    ("getBigUint64", 8, get_bu64),
  ] {
    install_getter(&dataview_proto_map, function_proto_map, name, size, decode);
  }
  for (name, size, encode) in [
    (
      "setInt8",
      1,
      set_i8 as fn(&mut crate::Vm, &Value, bool) -> Result<Vec<u8>, crate::RuntimeError>,
    ),
    ("setUint8", 1, set_u8),
    ("setInt16", 2, set_i16),
    ("setUint16", 2, set_u16),
    ("setInt32", 4, set_i32),
    ("setUint32", 4, set_u32),
    ("setFloat32", 4, set_f32),
    ("setFloat64", 8, set_f64),
    ("setBigInt64", 8, set_bi64),
    ("setBigUint64", 8, set_bu64),
  ] {
    install_setter(&dataview_proto_map, function_proto_map, name, size, encode);
  }

  (dataview_ctor_val, dataview_proto_map)
}
