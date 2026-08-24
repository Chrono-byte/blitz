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
      let instance_proto = if let Some(new_target) = vm.pending_constructor_new_target() {
        let prototype = vm.get_property_host(
          &new_target,
          &PropertyKey::String("prototype".to_string().into()),
        )?;
        match prototype {
          Value::Object(_)
          | Value::Array(..)
          | Value::Closure { .. }
          | Value::BuiltinObject(_, _)
          | Value::Proxy(_) => prototype,
          _ => Value::Object(Rc::clone(&proto_map_rc)),
        }
      } else {
        Value::Object(Rc::clone(&proto_map_rc))
      };
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
  let species_getter = crate::ecmascript::semantics::builtin_object(
    Rc::new(|_vm, args| Ok(args.first().cloned().unwrap_or(Value::Undefined))),
    function_proto_map,
  );
  set_builtin_name(&species_getter, "get [Symbol.species]");
  set_builtin_length(&species_getter, 0.0);
  arraybuffer_ctor_map
    .borrow_mut()
    .properties
    .insert(
      PropertyKey::Symbol(crate::value::SymbolId::well_known_species()),
      PropertyDescriptor::Accessor {
        get: Some(species_getter),
        set: None,
        enumerable: false,
        configurable: true,
      },
    );
  let byte_length_getter = crate::ecmascript::semantics::builtin_object(
    Rc::new(|vm, args| {
      let receiver = args.first().cloned().unwrap_or(Value::Undefined);
      if !is_array_buffer(&receiver) || array_buffer_is_shared(&receiver) != Some(false) {
        return Err(
          vm.throw_type_error("ArrayBuffer byteLength getter called on incompatible receiver"),
        );
      }
      let backing = array_buffer_backing(&receiver)
        .ok_or_else(|| vm.throw_type_error("Cannot get byteLength of a detached ArrayBuffer"))?;
      let length = backing.borrow().len();
      Ok(Value::Number(length as f64))
    }),
    function_proto_map,
  );
  set_builtin_name(&byte_length_getter, "get byteLength");
  set_builtin_length(&byte_length_getter, 0.0);
  arraybuffer_proto_map
    .borrow_mut()
    .properties
    .insert(
      PropertyKey::String("byteLength".into()),
      PropertyDescriptor::Accessor {
        get: Some(byte_length_getter),
        set: None,
        enumerable: false,
        configurable: true,
      },
    );
  arraybuffer_proto_map
    .borrow_mut()
    .properties
    .insert(
      PropertyKey::Symbol(crate::value::SymbolId::well_known_to_string_tag()),
      PropertyDescriptor::Data {
        value: Value::String("ArrayBuffer".into()),
        writable: false,
        enumerable: false,
        configurable: true,
      },
    );

  let default_slice_constructor = arraybuffer_ctor_val.clone();
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
      let new_length = end - start;

      let constructor =
        vm.get_property_host(&receiver, &PropertyKey::String("constructor".into()))?;
      let constructor = match constructor {
        Value::Undefined => default_slice_constructor.clone(),
        constructor if constructor.is_object() => {
          let species = vm.get_property_host(
            &constructor,
            &PropertyKey::Symbol(crate::value::SymbolId::well_known_species()),
          )?;
          match species {
            Value::Undefined | Value::Null => default_slice_constructor.clone(),
            species => species,
          }
        }
        _ => return Err(vm.throw_type_error("ArrayBuffer constructor is not an object")),
      };
      if !constructor.is_constructable() {
        return Err(vm.throw_type_error("ArrayBuffer species is not constructable"));
      }
      let result = vm.construct_value(&constructor, &[Value::Number(new_length as f64)])?;
      if !is_array_buffer(&result) || array_buffer_is_shared(&result) != Some(false) {
        return Err(vm.throw_type_error("ArrayBuffer species returned an incompatible value"));
      }
      if crate::runtime_utils::strict_equal(&result, &receiver) {
        return Err(vm.throw_type_error("ArrayBuffer species returned the source buffer"));
      }
      let result_backing = array_buffer_backing(&result)
        .ok_or_else(|| vm.throw_type_error("ArrayBuffer species returned a detached buffer"))?;
      if result_backing.borrow().len() < new_length {
        return Err(vm.throw_type_error("ArrayBuffer species returned a buffer that is too small"));
      }
      let source = array_buffer_backing(&receiver)
        .ok_or_else(|| vm.throw_type_error("ArrayBuffer was detached during slice"))?;
      let values = source.borrow().clone_elements();
      let mut result_data = result_backing.borrow_mut();
      let mut result_values = result_data.clone_elements();
      for (index, value) in values[start..end].iter().cloned().enumerate() {
        result_values[index] = value;
      }
      *result_data = crate::value::ArrayData::new(result_values);
      drop(result_data);
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
