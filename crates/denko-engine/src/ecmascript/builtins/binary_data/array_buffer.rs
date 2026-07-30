// SPDX-License-Identifier: MPL-2.0

use std::cell::RefCell;
use std::rc::Rc;

use super::is_data_view;
use crate::ecmascript::semantics::{
  array_buffer_backing, array_buffer_is_shared, byte_view_details, is_array_buffer,
  new_array_buffer, set_builtin_length, set_builtin_name,
};
use crate::runtime_utils::to_length;
use crate::value::{BuiltinFn, ObjectData, PropertyDescriptor, PropertyKey, Value};

pub(crate) fn install_array_buffer(
  object_proto_map: &Rc<RefCell<ObjectData>>,
  function_proto_map: &Rc<RefCell<ObjectData>>,
) -> (Value, Rc<RefCell<ObjectData>>) {
  let arraybuffer_proto_map = Rc::new(RefCell::new(ObjectData::with_prototype(Value::Object(
    Rc::clone(object_proto_map),
  ))));
  let arraybuffer_ctor_map = Rc::new(RefCell::new(ObjectData::with_prototype(Value::Object(
    Rc::clone(function_proto_map),
  ))));
  let arraybuffer_ctor_fn: BuiltinFn = {
    let proto_map_rc = Rc::clone(&arraybuffer_proto_map);
    Rc::new(move |vm, args| {
      let instance_proto = vm
        .current_new_target()
        .and_then(|new_target| {
          vm.get_property_host(
            &new_target,
            &PropertyKey::String("prototype".to_string().into()),
          )
          .ok()
        })
        .and_then(|proto| match proto {
          Value::Object(_)
          | Value::Array(..)
          | Value::Closure { .. }
          | Value::BuiltinObject(_, _) => Some(proto),
          _ => None,
        })
        .unwrap_or_else(|| Value::Object(Rc::clone(&proto_map_rc)));
      let length_value = args.get(1).cloned().unwrap_or(Value::Undefined);
      let length = to_length(vm, &length_value)?;

      let (buffer, _) = new_array_buffer(instance_proto, length, false);
      Ok(buffer)
    })
  };
  let arraybuffer_ctor_val = Value::BuiltinObject(
    Rc::clone(&arraybuffer_ctor_fn),
    Rc::clone(&arraybuffer_ctor_map),
  );
  arraybuffer_ctor_map
    .borrow_mut()
    .properties
    .insert(
      PropertyKey::String("prototype".to_string().into()),
      PropertyDescriptor::builtin_prototype(Value::Object(Rc::clone(&arraybuffer_proto_map))),
    );
  arraybuffer_proto_map
    .borrow_mut()
    .properties
    .insert(
      PropertyKey::String("constructor".to_string().into()),
      PropertyDescriptor::builtin_data(arraybuffer_ctor_val.clone()),
    );
  set_builtin_name(&arraybuffer_ctor_val, "ArrayBuffer");
  set_builtin_length(&arraybuffer_ctor_val, 1.0);
  let slice_proto = Rc::clone(&arraybuffer_proto_map);
  let slice = crate::ecmascript::semantics::builtin_object(
    Rc::new(move |vm, args| {
      let receiver = args.first().cloned().unwrap_or(Value::Undefined);
      if !is_array_buffer(&receiver) || array_buffer_is_shared(&receiver) != Some(false) {
        return Err(
          vm.throw_type_error("ArrayBuffer.prototype.slice called on incompatible receiver"),
        );
      }
      let source = array_buffer_backing(&receiver)
        .ok_or_else(|| vm.throw_type_error("Cannot slice a detached ArrayBuffer"))?;
      let length = source.borrow().len();
      let relative = |vm: &mut crate::Vm,
                      value: Option<&Value>,
                      default: usize|
       -> Result<usize, crate::RuntimeError> {
        let Some(value) = value else {
          return Ok(default);
        };
        let number = vm.to_number_value(value)?;
        if number.is_nan() {
          return Ok(0);
        }
        if number < 0.0 {
          return Ok(length.saturating_sub((-number).trunc() as usize));
        }
        Ok(number.trunc().clamp(0.0, length as f64) as usize)
      };
      let start = relative(vm, args.get(1), 0)?;
      let end = relative(vm, args.get(2), length)?;
      let start = start.min(end);
      let (result, backing) =
        new_array_buffer(Value::Object(Rc::clone(&slice_proto)), end - start, false);
      let values = source.borrow().clone_elements();
      *backing.borrow_mut() = crate::value::ArrayData::from(values[start..end].to_vec());
      Ok(result)
    }),
    function_proto_map,
  );
  set_builtin_name(&slice, "slice");
  set_builtin_length(&slice, 2.0);
  arraybuffer_proto_map
    .borrow_mut()
    .properties
    .insert(
      PropertyKey::String("slice".to_string().into()),
      PropertyDescriptor::builtin_data(slice),
    );
  arraybuffer_ctor_map
    .borrow_mut()
    .properties
    .insert(
      PropertyKey::String("isView".to_string().into()),
      PropertyDescriptor::builtin_data(crate::ecmascript::semantics::builtin_object(
        Rc::new(|_vm, args| {
          let value = args
            .get(1)
            .or_else(|| args.first())
            .unwrap_or(&Value::Undefined);
          Ok(Value::Bool(
            byte_view_details(value).is_some() || is_data_view(value),
          ))
        }),
        function_proto_map,
      )),
    );

  (arraybuffer_ctor_val, arraybuffer_proto_map)
}
