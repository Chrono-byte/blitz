// SPDX-License-Identifier: MPL-2.0

mod array_buffer;
mod data_view;
mod shared_array_buffer;
mod typed_array;

use std::cell::RefCell;
use std::rc::Rc;

use crate::Vm;
use crate::ecmascript::semantics::{
  array_buffer_backing, byte_view_details, byte_view_metadata, is_array_buffer,
  is_byte_offset_aligned, new_array_buffer, new_byte_view, usize_to_decimal_string, usize_to_f64,
};
use crate::errors::RuntimeError;
use crate::runtime_utils::{static_builtin_arg_offset, to_index, to_length};
use crate::value::{ArrayData, ObjectData, PropertyDescriptor, PropertyKey, SymbolId, Value};

pub(crate) use array_buffer::install_array_buffer;
pub(crate) use data_view::{install_data_view, is_data_view};
pub(crate) use shared_array_buffer::install_shared_array_buffer;
pub(crate) use typed_array::install_typed_arrays;

pub(crate) struct BinaryDataIntrinsics {
  pub(crate) arraybuffer_ctor_val: Value,
  pub(crate) arraybuffer_proto_map: Rc<RefCell<ObjectData>>,
  pub(crate) sharedarraybuffer_ctor_val: Value,
  pub(crate) sharedarraybuffer_proto_map: Rc<RefCell<ObjectData>>,
  pub(crate) dataview_ctor_val: Value,
  pub(crate) dataview_proto_map: Rc<RefCell<ObjectData>>,
  pub(crate) typedarray_ctor_val: Value,
  pub(crate) uint8array_ctor_val: Value,
  pub(crate) int8array_ctor_val: Value,
  pub(crate) uint16array_ctor_val: Value,
  pub(crate) int16array_ctor_val: Value,
  pub(crate) uint32array_ctor_val: Value,
  pub(crate) int32array_ctor_val: Value,
  pub(crate) float32array_ctor_val: Value,
  pub(crate) float64array_ctor_val: Value,
  pub(crate) bigint64array_ctor_val: Value,
  pub(crate) biguint64array_ctor_val: Value,
  pub(crate) uint8clampedarray_ctor_val: Value,
  pub(crate) typedarray_proto_map: Rc<RefCell<ObjectData>>,
}

pub(crate) fn install_binary_data_intrinsics(
  object_proto_map: &Rc<RefCell<ObjectData>>,
  function_proto_map: &Rc<RefCell<ObjectData>>,
  array_values_builtin: Value,
  array_keys_builtin: Value,
  array_entries_builtin: Value,
  array_to_string_builtin: Value,
) -> BinaryDataIntrinsics {
  let (arraybuffer_ctor_val, arraybuffer_proto_map) =
    install_array_buffer(object_proto_map, function_proto_map);
  let (sharedarraybuffer_ctor_val, sharedarraybuffer_proto_map) =
    install_shared_array_buffer(object_proto_map, function_proto_map);
  let typed_arrays = install_typed_arrays(
    object_proto_map,
    function_proto_map,
    &arraybuffer_proto_map,
    array_values_builtin,
    array_keys_builtin,
    array_entries_builtin,
    array_to_string_builtin,
  );
  let (dataview_ctor_val, dataview_proto_map) =
    install_data_view(object_proto_map, function_proto_map);

  BinaryDataIntrinsics {
    arraybuffer_ctor_val,
    arraybuffer_proto_map,
    sharedarraybuffer_ctor_val,
    sharedarraybuffer_proto_map,
    dataview_ctor_val,
    dataview_proto_map,
    typedarray_ctor_val: typed_arrays.typedarray_ctor_val,
    uint8array_ctor_val: typed_arrays.uint8array_ctor_val,
    int8array_ctor_val: typed_arrays.int8array_ctor_val,
    uint16array_ctor_val: typed_arrays.uint16array_ctor_val,
    int16array_ctor_val: typed_arrays.int16array_ctor_val,
    uint32array_ctor_val: typed_arrays.uint32array_ctor_val,
    int32array_ctor_val: typed_arrays.int32array_ctor_val,
    float32array_ctor_val: typed_arrays.float32array_ctor_val,
    float64array_ctor_val: typed_arrays.float64array_ctor_val,
    bigint64array_ctor_val: typed_arrays.bigint64array_ctor_val,
    biguint64array_ctor_val: typed_arrays.biguint64array_ctor_val,
    uint8clampedarray_ctor_val: typed_arrays.uint8clampedarray_ctor_val,
    typedarray_proto_map: typed_arrays.typedarray_proto_map,
  }
}

#[allow(clippy::type_complexity)]
fn typed_view_details(
  value: &Value
) -> Option<(
  Rc<RefCell<ObjectData>>,
  Value,
  Rc<RefCell<ArrayData>>,
  String,
  usize,
  usize,
  Value,
)> {
  let details = byte_view_details(value)?;
  Some((
    details.object,
    details.buffer,
    details.data,
    details.kind.to_string(),
    details.length,
    details.byte_offset,
    details.prototype,
  ))
}

fn typed_array_relative_index(
  vm: &mut Vm,
  value: &Value,
  length: usize,
) -> Result<usize, RuntimeError> {
  let number = vm.to_number_value(value)?;
  if number.is_nan() || number == f64::NEG_INFINITY {
    return Ok(0);
  }
  if number == f64::INFINITY {
    return Ok(length);
  }
  let integer = number.trunc();
  if integer < 0.0 {
    return Ok(length.saturating_sub((-integer) as usize));
  }
  Ok(integer.min(length as f64) as usize)
}
