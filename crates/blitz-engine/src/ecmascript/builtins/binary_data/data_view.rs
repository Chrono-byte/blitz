// SPDX-License-Identifier: MPL-2.0

use std::cell::RefCell;
use std::rc::Rc;

use crate::ecmascript::semantics::{array_buffer_backing, is_array_buffer, usize_to_f64};
use crate::runtime_utils::{static_builtin_arg_offset, to_index};
use crate::value::{BuiltinFn, ObjectData, PropertyDescriptor, PropertyKey, Value};

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
      let buffer = args
        .get(off + 1)
        .cloned()
        .unwrap_or(Value::Undefined);
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
        .get(off + 2)
        .cloned()
        .unwrap_or(Value::Undefined);
      let byte_offset = if matches!(byte_offset_arg, Value::Undefined) {
        0
      } else {
        to_index(vm, &byte_offset_arg)?
      };
      let byte_length_arg = args
        .get(off + 3)
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
      dataview_data.properties.insert(
        PropertyKey::String("[[DataViewBuffer]]".to_string().into()),
        PropertyDescriptor::ordinary_data(buffer.clone()),
      );
      dataview_data.properties.insert(
        PropertyKey::String("[[DataViewByteOffset]]".to_string().into()),
        PropertyDescriptor::ordinary_data(Value::Number(usize_to_f64(byte_offset))),
      );
      dataview_data.properties.insert(
        PropertyKey::String("[[DataViewByteLength]]".to_string().into()),
        PropertyDescriptor::ordinary_data(Value::Number(usize_to_f64(byte_length))),
      );
      dataview_data.properties.insert(
        PropertyKey::String("buffer".to_string().into()),
        PropertyDescriptor::ordinary_data(buffer),
      );
      dataview_data.properties.insert(
        PropertyKey::String("byteLength".to_string().into()),
        PropertyDescriptor::ordinary_data(Value::Number(usize_to_f64(byte_length))),
      );
      dataview_data.properties.insert(
        PropertyKey::String("byteOffset".to_string().into()),
        PropertyDescriptor::ordinary_data(Value::Number(usize_to_f64(byte_offset))),
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
  dataview_proto_map.borrow_mut().properties.insert(
    PropertyKey::String("constructor".to_string().into()),
    PropertyDescriptor::builtin_data(dataview_ctor_val.clone()),
  );

  (dataview_ctor_val, dataview_proto_map)
}
