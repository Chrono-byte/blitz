// SPDX-License-Identifier: MPL-2.0

use std::cell::RefCell;
use std::rc::Rc;

use crate::ecmascript::semantics::new_array_buffer;
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

  (arraybuffer_ctor_val, arraybuffer_proto_map)
}
