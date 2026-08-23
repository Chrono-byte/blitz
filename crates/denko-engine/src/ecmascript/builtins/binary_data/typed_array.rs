// SPDX-License-Identifier: MPL-2.0

use std::cell::RefCell;
use std::rc::Rc;

use super::*;
use crate::ecmascript::semantics::builtin_object;
use crate::runtime_utils::{same_value_zero, strict_equal};
use crate::value::BuiltinFn;

pub(crate) struct TypedArrayIntrinsics {
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

fn typed_array_length(
  vm: &mut Vm,
  receiver: &Value,
) -> Result<usize, RuntimeError> {
  typed_view_details(receiver)
    .map(|(_, _, _, _, length, _, _)| length)
    .ok_or_else(|| vm.throw_type_error("TypedArray method called on incompatible receiver"))
}

fn typed_array_create_same(
  vm: &mut Vm,
  receiver: &Value,
  length: usize,
) -> Result<Value, RuntimeError> {
  let (_, _, _, source_kind, _, _, _) = typed_view_details(receiver)
    .ok_or_else(|| vm.throw_type_error("TypedArray method called on incompatible receiver"))?;
  let default_constructor = vm
    .get_global(&source_kind)
    .ok_or_else(|| vm.throw_type_error("TypedArray constructor is unavailable"))?;
  let constructor = vm.get_property_host(receiver, &PropertyKey::String("constructor".into()))?;
  let constructor = match constructor {
    Value::Undefined => default_constructor,
    constructor if constructor.is_object() => {
      let species = vm.get_property_host(
        &constructor,
        &PropertyKey::Symbol(SymbolId::well_known_species()),
      )?;
      match species {
        Value::Undefined | Value::Null => default_constructor,
        species => species,
      }
    }
    _ => return Err(vm.throw_type_error("TypedArray constructor is not an object")),
  };
  if !constructor.is_constructable() {
    return Err(vm.throw_type_error("TypedArray constructor is not constructable"));
  }
  let result = vm.construct_value(&constructor, &[Value::Number(usize_to_f64(length))])?;
  let Some((_, _, _, result_kind, result_length, _, _)) = typed_view_details(&result) else {
    return Err(vm.throw_type_error("TypedArray constructor returned an incompatible value"));
  };
  if source_kind.starts_with("Big") != result_kind.starts_with("Big") || result_length < length {
    return Err(vm.throw_type_error("TypedArray species created an incompatible value"));
  }
  Ok(result)
}

fn typed_array_relative_start(
  vm: &mut Vm,
  value: &Value,
  length: usize,
) -> Result<usize, RuntimeError> {
  typed_array_relative_index(vm, value, length)
}

fn typed_element(
  vm: &mut Vm,
  receiver: &Value,
  index: usize,
) -> Result<Value, RuntimeError> {
  vm.get_property_host(receiver, &PropertyKey::String(index.to_string().into()))
}

fn set_typed_element(
  vm: &mut Vm,
  receiver: &Value,
  index: usize,
  value: Value,
) -> Result<(), RuntimeError> {
  vm.set_property_host(receiver, &Value::String(index.to_string().into()), value)?;
  Ok(())
}

fn install_typed_method(
  prototype: &Rc<RefCell<ObjectData>>,
  function_prototype: &Rc<RefCell<ObjectData>>,
  name: &str,
  length: f64,
  function: BuiltinFn,
) {
  let builtin = builtin_object(function, function_prototype);
  crate::ecmascript::semantics::set_builtin_name(&builtin, name);
  crate::ecmascript::semantics::set_builtin_length(&builtin, length);
  prototype.borrow_mut().properties.insert(
    PropertyKey::String(name.into()),
    PropertyDescriptor::builtin_data(builtin),
  );
}

pub(crate) fn install_typed_arrays(
  object_proto_map: &Rc<RefCell<ObjectData>>,
  function_proto_map: &Rc<RefCell<ObjectData>>,
  arraybuffer_proto_map: &Rc<RefCell<ObjectData>>,
  array_values_builtin: Value,
  array_keys_builtin: Value,
  array_entries_builtin: Value,
) -> TypedArrayIntrinsics {
  let typedarray_proto_map = Rc::new(RefCell::new(ObjectData::with_prototype(Value::Object(
    Rc::clone(object_proto_map),
  ))));
  let typedarray_ctor_map = Rc::new(RefCell::new(ObjectData::with_prototype(Value::Object(
    Rc::clone(function_proto_map),
  ))));
  typedarray_ctor_map
    .borrow_mut()
    .properties
    .insert(
      PropertyKey::String("prototype".to_string().into()),
      PropertyDescriptor::builtin_prototype(Value::Object(Rc::clone(&typedarray_proto_map))),
    );
  typedarray_ctor_map
    .borrow_mut()
    .properties
    .insert(
      PropertyKey::String("name".to_string().into()),
      PropertyDescriptor::builtin_metadata(Value::String("TypedArray".to_string().into())),
    );
  let species_getter = builtin_object(
    Rc::new(|_vm, args| Ok(args.first().cloned().unwrap_or(Value::Undefined))),
    function_proto_map,
  );
  crate::ecmascript::semantics::set_builtin_name(&species_getter, "get [Symbol.species]");
  crate::ecmascript::semantics::set_builtin_length(&species_getter, 0.0);
  typedarray_ctor_map
    .borrow_mut()
    .properties
    .insert(
      PropertyKey::Symbol(SymbolId::well_known_species()),
      PropertyDescriptor::Accessor {
        get: Some(species_getter),
        set: None,
        enumerable: false,
        configurable: true,
      },
    );
  let typedarray_ctor_fn: BuiltinFn = Rc::new(move |_vm, _args| {
    Err(RuntimeError::from(
      "TypeError: TypedArray constructor is not directly callable",
    ))
  });
  let typedarray_ctor_val = Value::BuiltinObject(typedarray_ctor_fn, typedarray_ctor_map);
  crate::ecmascript::semantics::set_builtin_length(&typedarray_ctor_val, 0.0);
  typedarray_proto_map
    .borrow_mut()
    .properties
    .insert(
      PropertyKey::String("constructor".to_string().into()),
      PropertyDescriptor::builtin_data(typedarray_ctor_val.clone()),
    );
  typedarray_proto_map
    .borrow_mut()
    .properties
    .insert(
      PropertyKey::String("values".to_string().into()),
      PropertyDescriptor::builtin_data(array_values_builtin.clone()),
    );
  typedarray_proto_map
    .borrow_mut()
    .properties
    .insert(
      PropertyKey::String("keys".to_string().into()),
      PropertyDescriptor::builtin_data(array_keys_builtin),
    );
  typedarray_proto_map
    .borrow_mut()
    .properties
    .insert(
      PropertyKey::String("entries".to_string().into()),
      PropertyDescriptor::builtin_data(array_entries_builtin),
    );
  for name in ["buffer", "byteLength", "byteOffset", "length"] {
    let getter_name = name.to_string();
    let getter = builtin_object(
      Rc::new(move |vm, args| {
        let receiver = args.first().cloned().unwrap_or(Value::Undefined);
        let (_, buffer, _, kind, length, byte_offset, _) = typed_view_details(&receiver)
          .ok_or_else(|| {
            vm.throw_type_error("TypedArray accessor called on incompatible receiver")
          })?;
        Ok(match getter_name.as_str() {
          "buffer" => buffer,
          "byteLength" => Value::Number(usize_to_f64(
            length
              * crate::ecmascript::semantics::byte_views::typed_array_bytes_per_element(&kind)
                .unwrap_or(1),
          )),
          "byteOffset" => Value::Number(usize_to_f64(byte_offset)),
          _ => Value::Number(usize_to_f64(length)),
        })
      }),
      function_proto_map,
    );
    crate::ecmascript::semantics::set_builtin_name(&getter, &format!("get {name}"));
    crate::ecmascript::semantics::set_builtin_length(&getter, 0.0);
    typedarray_proto_map
      .borrow_mut()
      .properties
      .insert(
        PropertyKey::String(name.into()),
        PropertyDescriptor::Accessor {
          get: Some(getter),
          set: None,
          enumerable: false,
          configurable: true,
        },
      );
  }
  typedarray_proto_map
    .borrow_mut()
    .properties
    .insert(
      PropertyKey::Symbol(SymbolId::well_known_iterator()),
      PropertyDescriptor::builtin_data(array_values_builtin),
    );
  typedarray_proto_map
    .borrow_mut()
    .properties
    .insert(
      PropertyKey::String("fill".to_string().into()),
      PropertyDescriptor::builtin_data(builtin_object(
        Rc::new(|vm, args| {
          let receiver = args.first().cloned().unwrap_or(Value::Undefined);
          let fill_value = args.get(1).cloned().unwrap_or(Value::Undefined);
          let length = typed_array_length(vm, &receiver)?;
          let start =
            typed_array_relative_start(vm, args.get(2).unwrap_or(&Value::Undefined), length)?;
          let end = args.get(3).map_or(Ok(length), |value| {
            typed_array_relative_start(vm, value, length)
          })?;
          for index in start..end.max(start) {
            set_typed_element(vm, &receiver, index, fill_value.clone())?;
          }
          Ok(receiver)
        }),
        function_proto_map,
      )),
    );

  let subarray_builtin = builtin_object(
    Rc::new(move |vm, args| {
      let receiver = args.first().cloned().unwrap_or(Value::Undefined);
      let (_, buffer, data, kind, length, byte_offset, prototype) = typed_view_details(&receiver)
        .ok_or_else(|| {
        vm.throw_type_error("TypedArray.prototype.subarray called on incompatible receiver")
      })?;
      let begin_value = args.get(1).cloned().unwrap_or(Value::Number(0.0));
      let begin = typed_array_relative_index(vm, &begin_value, length)?;
      let end_value = args
        .get(2)
        .cloned()
        .unwrap_or(Value::Number(usize_to_f64(length)));
      let end = typed_array_relative_index(vm, &end_value, length)?;
      let final_end = end.max(begin);
      Ok(new_typed_array_view(
        prototype,
        buffer,
        data,
        &kind,
        final_end.saturating_sub(begin),
        byte_offset
          + begin
            * crate::ecmascript::semantics::byte_views::typed_array_bytes_per_element(&kind)
              .unwrap_or(1),
      ))
    }),
    function_proto_map,
  );
  typedarray_proto_map
    .borrow_mut()
    .properties
    .insert(
      PropertyKey::String("subarray".to_string().into()),
      PropertyDescriptor::builtin_data(subarray_builtin),
    );

  let slice_builtin = builtin_object(
    Rc::new(move |vm, args| {
      let receiver = args.first().cloned().unwrap_or(Value::Undefined);
      let (_, _, _, _kind, length, _, _) = typed_view_details(&receiver).ok_or_else(|| {
        vm.throw_type_error("TypedArray.prototype.slice called on incompatible receiver")
      })?;
      let begin_value = args.get(1).cloned().unwrap_or(Value::Number(0.0));
      let begin = typed_array_relative_index(vm, &begin_value, length)?;
      let end_value = args
        .get(2)
        .cloned()
        .unwrap_or(Value::Number(usize_to_f64(length)));
      let end = typed_array_relative_index(vm, &end_value, length)?;
      let copy_length = end.max(begin).saturating_sub(begin);
      let result = typed_array_create_same(vm, &receiver, copy_length)?;
      for index in 0..copy_length {
        let value = typed_element(vm, &receiver, begin + index)?;
        set_typed_element(vm, &result, index, value)?;
      }
      Ok(result)
    }),
    function_proto_map,
  );
  typedarray_proto_map
    .borrow_mut()
    .properties
    .insert(
      PropertyKey::String("slice".to_string().into()),
      PropertyDescriptor::builtin_data(slice_builtin),
    );

  let set_builtin = builtin_object(
    Rc::new(|vm, args| {
      let receiver = args.first().cloned().unwrap_or(Value::Undefined);
      let (_, _, _, _kind, target_length, _, _) =
        typed_view_details(&receiver).ok_or_else(|| {
          vm.throw_type_error("TypedArray.prototype.set called on incompatible receiver")
        })?;
      let source = args.get(1).cloned().unwrap_or(Value::Undefined);
      let offset_value = args.get(2).cloned().unwrap_or(Value::Number(0.0));
      let offset = to_index(vm, &offset_value)?;
      let source_length =
        vm.get_property_host(&source, &PropertyKey::String("length".to_string().into()))?;
      let source_length = to_length(vm, &source_length)?;
      if offset.saturating_add(source_length) > target_length {
        return Err(vm.throw_range_error("Source is too large"));
      }
      let mut values = Vec::with_capacity(source_length);
      for index in 0..source_length {
        values.push(vm.get_property_host(
          &source,
          &PropertyKey::String(usize_to_decimal_string(index).into()),
        )?);
      }
      for (index, value) in values.into_iter().enumerate() {
        vm.set_property_host(
          &receiver,
          &Value::String(usize_to_decimal_string(offset + index).into()),
          value,
        )?;
      }
      Ok(Value::Undefined)
    }),
    function_proto_map,
  );
  typedarray_proto_map
    .borrow_mut()
    .properties
    .insert(
      PropertyKey::String("set".to_string().into()),
      PropertyDescriptor::builtin_data(set_builtin),
    );

  install_typed_method(
    &typedarray_proto_map,
    function_proto_map,
    "at",
    1.0,
    Rc::new(|vm, args| {
      let receiver = args.first().cloned().unwrap_or(Value::Undefined);
      let length = typed_array_length(vm, &receiver)?;
      let index = vm.to_number_value(args.get(1).unwrap_or(&Value::Undefined))?;
      if index.is_nan() || !index.is_finite() {
        return Ok(Value::Undefined);
      }
      let index = index.trunc();
      let index = if index < 0.0 {
        length as isize + index as isize
      } else {
        index as isize
      };
      if index < 0 || index as usize >= length {
        Ok(Value::Undefined)
      } else {
        typed_element(vm, &receiver, index as usize)
      }
    }),
  );
  install_typed_method(
    &typedarray_proto_map,
    function_proto_map,
    "toReversed",
    0.0,
    Rc::new(|vm, args| {
      let receiver = args.first().cloned().unwrap_or(Value::Undefined);
      let length = typed_array_length(vm, &receiver)?;
      let result = typed_array_create_same(vm, &receiver, length)?;
      for index in 0..length {
        let value = typed_element(vm, &receiver, length - 1 - index)?;
        set_typed_element(vm, &result, index, value)?;
      }
      Ok(result)
    }),
  );
  install_typed_method(
    &typedarray_proto_map,
    function_proto_map,
    "with",
    2.0,
    Rc::new(|vm, args| {
      let receiver = args.first().cloned().unwrap_or(Value::Undefined);
      let length = typed_array_length(vm, &receiver)?;
      let index = vm.to_number_value(args.get(1).unwrap_or(&Value::Undefined))?;
      let index = if index.is_nan() {
        0
      } else if index < 0.0 {
        length as isize + index.trunc() as isize
      } else {
        index.trunc() as isize
      };
      if index < 0 || index as usize >= length {
        return Err(vm.throw_range_error("TypedArray.prototype.with index out of range"));
      }
      let replacement = args.get(2).cloned().unwrap_or(Value::Undefined);
      let result = typed_array_create_same(vm, &receiver, length)?;
      for offset in 0..length {
        let value = if offset == index as usize {
          replacement.clone()
        } else {
          typed_element(vm, &receiver, offset)?
        };
        set_typed_element(vm, &result, offset, value)?;
      }
      Ok(result)
    }),
  );
  install_typed_method(
    &typedarray_proto_map,
    function_proto_map,
    "toSorted",
    1.0,
    Rc::new(|vm, args| {
      let receiver = args.first().cloned().unwrap_or(Value::Undefined);
      let length = typed_array_length(vm, &receiver)?;
      let result = typed_array_create_same(vm, &receiver, length)?;
      for index in 0..length {
        let value = typed_element(vm, &receiver, index)?;
        set_typed_element(vm, &result, index, value)?;
      }
      let sort = vm.get_property_host(&result, &PropertyKey::String("sort".into()))?;
      vm.call_value_with_this(&sort, &result, args.get(1..).unwrap_or(&[]))
    }),
  );
  install_typed_method(
    &typedarray_proto_map,
    function_proto_map,
    "copyWithin",
    2.0,
    Rc::new(|vm, args| {
      let receiver = args.first().cloned().unwrap_or(Value::Undefined);
      let length = typed_array_length(vm, &receiver)?;
      let to = typed_array_relative_start(vm, args.get(1).unwrap_or(&Value::Undefined), length)?;
      let from = typed_array_relative_start(vm, args.get(2).unwrap_or(&Value::Undefined), length)?;
      let end = args.get(3).map_or(Ok(length), |value| {
        typed_array_relative_start(vm, value, length)
      })?;
      let count = length
        .saturating_sub(to)
        .min(end.saturating_sub(from));
      let values = (0..count)
        .map(|index| typed_element(vm, &receiver, from + index))
        .collect::<Result<Vec<_>, _>>()?;
      for (index, value) in values.into_iter().enumerate() {
        set_typed_element(vm, &receiver, to + index, value)?;
      }
      Ok(receiver)
    }),
  );
  for (name, mode) in [("every", 0_u8), ("some", 1), ("find", 2), ("findIndex", 3)] {
    install_typed_method(
      &typedarray_proto_map,
      function_proto_map,
      name,
      1.0,
      Rc::new(move |vm, args| {
        let receiver = args.first().cloned().unwrap_or(Value::Undefined);
        let callback = args.get(1).cloned().unwrap_or(Value::Undefined);
        if !Vm::is_callable_value(&callback) {
          return Err(vm.throw_type_error("TypedArray callback is not callable"));
        }
        let this_arg = args.get(2).cloned().unwrap_or(Value::Undefined);
        let length = typed_array_length(vm, &receiver)?;
        for index in 0..length {
          let value = typed_element(vm, &receiver, index)?;
          let matched = vm
            .call_value_with_this(
              &callback,
              &this_arg,
              &[
                value.clone(),
                Value::Number(usize_to_f64(index)),
                receiver.clone(),
              ],
            )?
            .is_truthy();
          match mode {
            0 if !matched => return Ok(Value::Bool(false)),
            1 if matched => return Ok(Value::Bool(true)),
            2 if matched => return Ok(value),
            3 if matched => return Ok(Value::Number(usize_to_f64(index))),
            _ => {}
          }
        }
        Ok(match mode {
          0 => Value::Bool(true),
          1 => Value::Bool(false),
          2 => Value::Undefined,
          _ => Value::Number(-1.0),
        })
      }),
    );
  }
  for (name, find_index) in [("findLast", false), ("findLastIndex", true)] {
    install_typed_method(
      &typedarray_proto_map,
      function_proto_map,
      name,
      1.0,
      Rc::new(move |vm, args| {
        let receiver = args.first().cloned().unwrap_or(Value::Undefined);
        let callback = args.get(1).cloned().unwrap_or(Value::Undefined);
        if !Vm::is_callable_value(&callback) {
          return Err(vm.throw_type_error("TypedArray callback is not callable"));
        }
        let this_arg = args.get(2).cloned().unwrap_or(Value::Undefined);
        let length = typed_array_length(vm, &receiver)?;
        for index in (0..length).rev() {
          let value = typed_element(vm, &receiver, index)?;
          if vm
            .call_value_with_this(
              &callback,
              &this_arg,
              &[
                value.clone(),
                Value::Number(usize_to_f64(index)),
                receiver.clone(),
              ],
            )?
            .is_truthy()
          {
            return Ok(if find_index {
              Value::Number(usize_to_f64(index))
            } else {
              value
            });
          }
        }
        Ok(if find_index {
          Value::Number(-1.0)
        } else {
          Value::Undefined
        })
      }),
    );
  }
  install_typed_method(
    &typedarray_proto_map,
    function_proto_map,
    "forEach",
    1.0,
    Rc::new(|vm, args| {
      let receiver = args.first().cloned().unwrap_or(Value::Undefined);
      let callback = args.get(1).cloned().unwrap_or(Value::Undefined);
      if !Vm::is_callable_value(&callback) {
        return Err(vm.throw_type_error("TypedArray callback is not callable"));
      }
      let this_arg = args.get(2).cloned().unwrap_or(Value::Undefined);
      let length = typed_array_length(vm, &receiver)?;
      for index in 0..length {
        let value = typed_element(vm, &receiver, index)?;
        drop(vm.call_value_with_this(
          &callback,
          &this_arg,
          &[value, Value::Number(usize_to_f64(index)), receiver.clone()],
        )?);
      }
      Ok(Value::Undefined)
    }),
  );
  for (name, mode) in [("includes", 0_u8), ("indexOf", 1), ("lastIndexOf", 2)] {
    install_typed_method(
      &typedarray_proto_map,
      function_proto_map,
      name,
      1.0,
      Rc::new(move |vm, args| {
        let receiver = args.first().cloned().unwrap_or(Value::Undefined);
        let search = args.get(1).cloned().unwrap_or(Value::Undefined);
        let length = typed_array_length(vm, &receiver)?;
        if length == 0 {
          return Ok(if mode == 0 {
            Value::Bool(false)
          } else {
            Value::Number(-1.0)
          });
        }
        let from = args.get(2).map_or(
          Ok::<usize, RuntimeError>(if mode == 2 {
            length.saturating_sub(1)
          } else {
            0
          }),
          |value| {
            let number = vm.to_number_value(value)?;
            if mode == 2 {
              if number.is_nan() {
                Ok::<usize, RuntimeError>(length.saturating_sub(1))
              } else if number < 0.0 {
                Ok::<usize, RuntimeError>(length.saturating_sub((-number).trunc() as usize))
              } else {
                Ok::<usize, RuntimeError>((number.trunc() as usize).min(length.saturating_sub(1)))
              }
            } else if number.is_nan() || number <= 0.0 {
              Ok::<usize, RuntimeError>(0)
            } else {
              Ok::<usize, RuntimeError>((number.trunc() as usize).min(length))
            }
          },
        )?;
        let indices: Box<dyn Iterator<Item = usize>> = if mode == 2 {
          Box::new((0..=from).rev())
        } else {
          Box::new(from..length)
        };
        for index in indices {
          let value = typed_element(vm, &receiver, index)?;
          let equal = if mode == 0 {
            same_value_zero(&value, &search)
          } else {
            strict_equal(&value, &search)
          };
          if equal {
            return Ok(if mode == 0 {
              Value::Bool(true)
            } else {
              Value::Number(usize_to_f64(index))
            });
          }
        }
        Ok(if mode == 0 {
          Value::Bool(false)
        } else {
          Value::Number(-1.0)
        })
      }),
    );
  }
  install_typed_method(
    &typedarray_proto_map,
    function_proto_map,
    "join",
    1.0,
    Rc::new(|vm, args| {
      let receiver = args.first().cloned().unwrap_or(Value::Undefined);
      let separator = match args.get(1) {
        None | Some(Value::Undefined) => ",".into(),
        Some(value) => vm.to_string(value)?,
      };
      let length = typed_array_length(vm, &receiver)?;
      let mut output = js_string::JsStringBuilder::new();
      for index in 0..length {
        if index > 0 {
          output.push_string(&separator);
        }
        let value = typed_element(vm, &receiver, index)?;
        output.push_string(&vm.to_string(&value)?);
      }
      Ok(Value::String(output.finish()))
    }),
  );
  install_typed_method(
    &typedarray_proto_map,
    function_proto_map,
    "toString",
    0.0,
    Rc::new(|vm, args| {
      let receiver = args.first().cloned().unwrap_or(Value::Undefined);
      let join = vm.get_property_host(&receiver, &PropertyKey::String("join".into()))?;
      vm.call_value_with_this(&join, &receiver, &[])
    }),
  );
  install_typed_method(
    &typedarray_proto_map,
    function_proto_map,
    "toLocaleString",
    0.0,
    Rc::new(|vm, args| {
      let receiver = args.first().cloned().unwrap_or(Value::Undefined);
      let length = typed_array_length(vm, &receiver)?;
      let mut output = js_string::JsStringBuilder::new();
      for index in 0..length {
        if index > 0 {
          output.push_code_unit(u16::from(b','));
        }
        let value = typed_element(vm, &receiver, index)?;
        let method = vm.get_property_host(&value, &PropertyKey::String("toLocaleString".into()))?;
        let rendered = if Vm::is_callable_value(&method) {
          vm.call_value_with_this(&method, &value, args.get(1..).unwrap_or(&[]))?
        } else {
          value
        };
        output.push_string(&vm.to_string(&rendered)?);
      }
      Ok(Value::String(output.finish()))
    }),
  );
  for (name, filter) in [("map", false), ("filter", true)] {
    install_typed_method(
      &typedarray_proto_map,
      function_proto_map,
      name,
      1.0,
      Rc::new(move |vm, args| {
        let receiver = args.first().cloned().unwrap_or(Value::Undefined);
        let callback = args.get(1).cloned().unwrap_or(Value::Undefined);
        if !Vm::is_callable_value(&callback) {
          return Err(vm.throw_type_error("TypedArray callback is not callable"));
        }
        let this_arg = args.get(2).cloned().unwrap_or(Value::Undefined);
        let length = typed_array_length(vm, &receiver)?;
        let mut values = Vec::with_capacity(length);
        for index in 0..length {
          let value = typed_element(vm, &receiver, index)?;
          let mapped = vm.call_value_with_this(
            &callback,
            &this_arg,
            &[
              value.clone(),
              Value::Number(usize_to_f64(index)),
              receiver.clone(),
            ],
          )?;
          if !filter || mapped.is_truthy() {
            values.push(if filter { value } else { mapped });
          }
        }
        let result = typed_array_create_same(vm, &receiver, values.len())?;
        for (index, value) in values.into_iter().enumerate() {
          set_typed_element(vm, &result, index, value)?;
        }
        Ok(result)
      }),
    );
  }
  for (name, right) in [("reduce", false), ("reduceRight", true)] {
    install_typed_method(
      &typedarray_proto_map,
      function_proto_map,
      name,
      1.0,
      Rc::new(move |vm, args| {
        let receiver = args.first().cloned().unwrap_or(Value::Undefined);
        let callback = args.get(1).cloned().unwrap_or(Value::Undefined);
        if !Vm::is_callable_value(&callback) {
          return Err(vm.throw_type_error("TypedArray callback is not callable"));
        }
        let length = typed_array_length(vm, &receiver)?;
        let indices = if right {
          (0..length).rev().collect::<Vec<_>>()
        } else {
          (0..length).collect::<Vec<_>>()
        };
        let mut iter = indices.into_iter();
        let mut accumulator = match args.get(2) {
          Some(value) => value.clone(),
          None => {
            let Some(index) = iter.next() else {
              return Err(vm.throw_type_error("Reduce of empty typed array with no initial value"));
            };
            typed_element(vm, &receiver, index)?
          }
        };
        for index in iter {
          let value = typed_element(vm, &receiver, index)?;
          accumulator = vm.call_value_with_this(
            &callback,
            &Value::Undefined,
            &[
              accumulator,
              value,
              Value::Number(usize_to_f64(index)),
              receiver.clone(),
            ],
          )?;
        }
        Ok(accumulator)
      }),
    );
  }
  install_typed_method(
    &typedarray_proto_map,
    function_proto_map,
    "reverse",
    0.0,
    Rc::new(|vm, args| {
      let receiver = args.first().cloned().unwrap_or(Value::Undefined);
      let length = typed_array_length(vm, &receiver)?;
      for index in 0..length / 2 {
        let other = length - 1 - index;
        let a = typed_element(vm, &receiver, index)?;
        let b = typed_element(vm, &receiver, other)?;
        set_typed_element(vm, &receiver, index, b)?;
        set_typed_element(vm, &receiver, other, a)?;
      }
      Ok(receiver)
    }),
  );
  install_typed_method(
    &typedarray_proto_map,
    function_proto_map,
    "sort",
    1.0,
    Rc::new(|vm, args| {
      let receiver = args.first().cloned().unwrap_or(Value::Undefined);
      let compare = args.get(1).cloned().unwrap_or(Value::Undefined);
      if !matches!(compare, Value::Undefined) && !Vm::is_callable_value(&compare) {
        return Err(vm.throw_type_error("TypedArray sort comparefn is not callable"));
      }
      let length = typed_array_length(vm, &receiver)?;
      let mut values = (0..length)
        .map(|index| typed_element(vm, &receiver, index))
        .collect::<Result<Vec<_>, _>>()?;
      for left in 0..values.len() {
        for right in left + 1..values.len() {
          let order = if matches!(compare, Value::Undefined) {
            let a = vm.to_number_value(&values[left])?;
            let b = vm.to_number_value(&values[right])?;
            a.is_nan() || (!b.is_nan() && a <= b)
          } else {
            let comparison = vm.call_value_with_this(
              &compare,
              &Value::Undefined,
              &[values[left].clone(), values[right].clone()],
            )?;
            vm.to_number_value(&comparison)? <= 0.0
          };
          if !order {
            values.swap(left, right);
          }
        }
      }
      for (index, value) in values.into_iter().enumerate() {
        set_typed_element(vm, &receiver, index, value)?;
      }
      Ok(receiver)
    }),
  );

  macro_rules! create_typed_array_ctor {
    ($name:expr, $element_count:expr, $zero_value:expr, $shared:expr) => {{
      let proto_map = Rc::new(RefCell::new(ObjectData::with_prototype(Value::Object(
        typedarray_proto_map.clone(),
      ))));
      let ctor_map = Rc::new(RefCell::new(ObjectData::with_prototype(
        typedarray_ctor_val.clone(),
      )));
      let arraybuffer_proto_rc = Rc::clone(arraybuffer_proto_map);
      let ctor_fn: BuiltinFn = {
        let proto_map_rc = proto_map.clone();
        let ctor_name = $name.to_string();
        Rc::new(move |vm, args| {
          let off = static_builtin_arg_offset(vm, args, $name);
          let init = args.get(off).cloned().unwrap_or(Value::Undefined);
          let byte_offset_arg = args
            .get(off + 1)
            .cloned()
            .unwrap_or(Value::Undefined);
          let length_arg = args
            .get(off + 2)
            .cloned()
            .unwrap_or(Value::Undefined);
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
            .unwrap_or_else(|| Value::Object(proto_map_rc.clone()));
          let (length, initial_values, byte_offset, view_byte_length, mut buffer_slot): (
            usize,
            Vec<Value>,
            usize,
            usize,
            Value,
          ) = match &init {
            Value::Number(_) => {
              let length = to_length(vm, &init)?;
              let view_byte_length = length * $element_count;
              (length, Vec::new(), 0, view_byte_length, Value::Undefined)
            }
            Value::String(s) => {
              let length = s.parse::<usize>().unwrap_or(0);
              let view_byte_length = length * $element_count;
              (length, Vec::new(), 0, view_byte_length, Value::Undefined)
            }
            Value::Array(arr, _) => {
              let initial_values = arr
                .borrow()
                .iter()
                .map(|slot| slot.clone().unwrap_or(Value::Undefined))
                .collect::<Vec<Value>>();
              let length = initial_values.len();
              let view_byte_length = length * $element_count;
              (
                length,
                initial_values,
                0,
                view_byte_length,
                Value::Undefined,
              )
            }
            Value::Object(_) => {
              let buffer_value = init.clone();
              if is_array_buffer(&buffer_value) {
                let backing = array_buffer_backing(&buffer_value)
                  .ok_or_else(|| vm.throw_range_error("Invalid typed array length"))?;
                let buffer_byte_length = backing.borrow().len();
                let byte_offset = if matches!(byte_offset_arg, Value::Undefined) {
                  0
                } else {
                  to_index(vm, &byte_offset_arg)?
                };
                if !is_byte_offset_aligned(byte_offset, $element_count) {
                  return Err(vm.throw_range_error("Invalid typed array offset"));
                }
                let length_from_spec = if matches!(length_arg, Value::Undefined) {
                  if buffer_byte_length < byte_offset {
                    return Err(vm.throw_range_error("Invalid typed array length"));
                  }
                  (buffer_byte_length - byte_offset) / $element_count
                } else {
                  to_index(vm, &length_arg)?
                };
                let expected_byte_length = length_from_spec * $element_count;
                if byte_offset + expected_byte_length > buffer_byte_length {
                  return Err(vm.throw_range_error("Invalid typed array length"));
                }
                (
                  length_from_spec,
                  Vec::new(),
                  byte_offset,
                  expected_byte_length,
                  buffer_value,
                )
              } else {
                let length_value = vm.get_property_host(
                  &buffer_value,
                  &PropertyKey::String("length".to_string().into()),
                )?;
                let length = to_length(vm, &length_value)?;
                let mut initial_values = Vec::with_capacity(length);
                for idx in 0..length {
                  initial_values.push(vm.get_property_host(
                    &buffer_value,
                    &PropertyKey::String(usize_to_decimal_string(idx).into()),
                  )?);
                }
                (
                  length,
                  initial_values,
                  0,
                  length * $element_count,
                  Value::Undefined,
                )
              }
            }
            _ => (0, Vec::new(), 0, 0, Value::Undefined),
          };

          let backing_data = if is_array_buffer(&buffer_slot) {
            array_buffer_backing(&buffer_slot)
              .ok_or_else(|| vm.throw_type_error("Invalid ArrayBuffer backing store"))?
          } else {
            let (buffer, data) = new_array_buffer(
              Value::Object(Rc::clone(&arraybuffer_proto_rc)),
              view_byte_length,
              false,
            );
            buffer_slot = buffer;
            data
          };
          for (idx, value) in initial_values.into_iter().enumerate() {
            let bytes = crate::ecmascript::semantics::byte_views::encode_typed_element(
              vm, &ctor_name, &value,
            )?;
            let start = byte_offset + idx * $element_count;
            let mut data = backing_data.borrow_mut();
            for (offset, byte) in bytes.into_iter().enumerate() {
              data[start + offset] = Some(Value::Number(f64::from(byte)));
            }
          }
          Ok(new_byte_view(
            instance_proto,
            buffer_slot,
            backing_data,
            &ctor_name,
            length,
            byte_offset,
          ))
        })
      };
      let ctor_val = Value::BuiltinObject(ctor_fn.clone(), ctor_map.clone());
      crate::ecmascript::semantics::set_builtin_length(&ctor_val, 3.0);
      ctor_map.borrow_mut().properties.insert(
        PropertyKey::String("prototype".to_string().into()),
        PropertyDescriptor::builtin_prototype(Value::Object(proto_map.clone())),
      );
      ctor_map.borrow_mut().properties.insert(
        PropertyKey::String("name".to_string().into()),
        PropertyDescriptor::builtin_metadata(Value::String($name.to_string().into())),
      );
      ctor_map.borrow_mut().properties.insert(
        PropertyKey::String("BYTES_PER_ELEMENT".to_string().into()),
        PropertyDescriptor::immutable_data(Value::Number(usize_to_f64($element_count))),
      );
      proto_map.borrow_mut().properties.insert(
        PropertyKey::String("constructor".to_string().into()),
        PropertyDescriptor::builtin_data(ctor_val.clone()),
      );
      proto_map.borrow_mut().properties.insert(
        PropertyKey::String("BYTES_PER_ELEMENT".to_string().into()),
        PropertyDescriptor::immutable_data(Value::Number(usize_to_f64($element_count))),
      );
      for static_name in ["from", "of"] {
        let fallback_constructor = ctor_val.clone();
        let builtin = builtin_object(
          Rc::new(move |vm, args| {
            let off = static_builtin_arg_offset(vm, args, "TypedArray");
            let constructor = if vm.this_value().is_constructable() {
              vm.this_value()
            } else {
              fallback_constructor.clone()
            };
            let values = if static_name == "of" {
              args.iter().skip(off).cloned().collect::<Vec<_>>()
            } else {
              let source =
                vm.to_object_value(args.get(off).cloned().unwrap_or(Value::Undefined))?;
              let map_fn = args
                .get(off + 1)
                .cloned()
                .unwrap_or(Value::Undefined);
              if !matches!(map_fn, Value::Undefined) && !Vm::is_callable_value(&map_fn) {
                return Err(vm.throw_type_error("TypedArray.from mapfn is not callable"));
              }
              let this_arg = args
                .get(off + 2)
                .cloned()
                .unwrap_or(Value::Undefined);
              let iterator_method = crate::runtime_utils::get_method(
                vm,
                &source,
                &PropertyKey::Symbol(SymbolId::well_known_iterator()),
              )?;
              if let Some(iterator_method) = iterator_method {
                let (iterator, next) =
                  crate::runtime_utils::get_iterator_record(vm, &source, &iterator_method)?;
                let mut values = Vec::new();
                loop {
                  let value = match crate::runtime_utils::iterator_step(vm, &iterator, &next) {
                    Ok(Some(value)) => value,
                    Ok(None) => break,
                    Err(error) => {
                      drop(crate::runtime_utils::iterator_close(vm, &iterator));
                      return Err(error);
                    }
                  };
                  let value = if matches!(map_fn, Value::Undefined) {
                    value
                  } else {
                    match vm.call_value_with_this(
                      &map_fn,
                      &this_arg,
                      &[value, Value::Number(usize_to_f64(values.len()))],
                    ) {
                      Ok(value) => value,
                      Err(error) => {
                        drop(crate::runtime_utils::iterator_close(vm, &iterator));
                        return Err(error);
                      }
                    }
                  };
                  values.push(value);
                }
                values
              } else {
                let length =
                  vm.get_property_host(&source, &PropertyKey::String("length".into()))?;
                let length = to_length(vm, &length)?;
                let mut values = Vec::with_capacity(length);
                for index in 0..length {
                  let value =
                    vm.get_property_host(&source, &PropertyKey::String(index.to_string().into()))?;
                  values.push(if matches!(map_fn, Value::Undefined) {
                    value
                  } else {
                    vm.call_value_with_this(
                      &map_fn,
                      &this_arg,
                      &[value, Value::Number(usize_to_f64(index))],
                    )?
                  });
                }
                values
              }
            };
            let result =
              vm.construct_value(&constructor, &[Value::Number(usize_to_f64(values.len()))])?;
            if typed_view_details(&result).is_none() {
              return Err(
                vm.throw_type_error("TypedArray constructor returned an incompatible value"),
              );
            }
            for (index, value) in values.into_iter().enumerate() {
              set_typed_element(vm, &result, index, value)?;
            }
            Ok(result)
          }),
          function_proto_map,
        );
        crate::ecmascript::semantics::set_builtin_name(&builtin, static_name);
        crate::ecmascript::semantics::set_builtin_length(
          &builtin,
          if static_name == "from" { 1.0 } else { 0.0 },
        );
        ctor_map.borrow_mut().properties.insert(
          PropertyKey::String(static_name.into()),
          PropertyDescriptor::builtin_data(builtin),
        );
      }
      (ctor_val, ctor_map)
    }};
  }

  let (uint8array_ctor_val, _) =
    create_typed_array_ctor!("Uint8Array", 1, Value::Number(0.0), true);
  let (int8array_ctor_val, _) = create_typed_array_ctor!("Int8Array", 1, Value::Number(0.0), true);
  let (uint16array_ctor_val, _) =
    create_typed_array_ctor!("Uint16Array", 2, Value::Number(0.0), false);
  let (int16array_ctor_val, _) =
    create_typed_array_ctor!("Int16Array", 2, Value::Number(0.0), false);
  let (uint32array_ctor_val, _) =
    create_typed_array_ctor!("Uint32Array", 4, Value::Number(0.0), false);
  let (int32array_ctor_val, _) =
    create_typed_array_ctor!("Int32Array", 4, Value::Number(0.0), false);
  let (float32array_ctor_val, _) =
    create_typed_array_ctor!("Float32Array", 4, Value::Number(0.0), false);
  let (float64array_ctor_val, _) =
    create_typed_array_ctor!("Float64Array", 8, Value::Number(0.0), false);
  let (bigint64array_ctor_val, _) = create_typed_array_ctor!(
    "BigInt64Array",
    8,
    Value::BigInt(num_bigint::BigInt::from(0u8)),
    false
  );
  let (biguint64array_ctor_val, _) = create_typed_array_ctor!(
    "BigUint64Array",
    8,
    Value::BigInt(num_bigint::BigInt::from(0u8)),
    false
  );
  let (uint8clampedarray_ctor_val, _) =
    create_typed_array_ctor!("Uint8ClampedArray", 1, Value::Number(0.0), true);

  TypedArrayIntrinsics {
    typedarray_ctor_val,
    uint8array_ctor_val,
    int8array_ctor_val,
    uint16array_ctor_val,
    int16array_ctor_val,
    uint32array_ctor_val,
    int32array_ctor_val,
    float32array_ctor_val,
    float64array_ctor_val,
    bigint64array_ctor_val,
    biguint64array_ctor_val,
    uint8clampedarray_ctor_val,
    typedarray_proto_map,
  }
}
