// SPDX-License-Identifier: MPL-2.0

use std::cell::RefCell;
use std::rc::Rc;

use num_bigint::BigInt as BigIntValue;

use super::*;
use crate::ecmascript::semantics::builtin_object;
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
  let typedarray_ctor_fn: BuiltinFn = Rc::new(move |_vm, _args| {
    Err(RuntimeError::from(
      "TypeError: TypedArray constructor is not directly callable",
    ))
  });
  let typedarray_ctor_val = Value::BuiltinObject(typedarray_ctor_fn, typedarray_ctor_map);
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
          let len = match vm
            .get_property_host(&receiver, &PropertyKey::String("length".to_string().into()))
          {
            Ok(Value::Number(n)) if n.is_finite() && n > 0.0 => n.trunc().to_usize().unwrap_or(0),
            _ => 0,
          };
          for i in 0..len {
            let key = Value::String(i.to_string().into());
            drop(vm.set_property_host(&receiver, &key, fill_value.clone())?);
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
        .filter(|(_, _, _, kind, _, _, _)| {
          matches!(
            kind.as_str(),
            "Uint8Array" | "Int8Array" | "Uint8ClampedArray"
          )
        })
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
        byte_offset + begin,
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

  let slice_builtin = {
    let arraybuffer_proto_map = Rc::clone(arraybuffer_proto_map);
    builtin_object(
      Rc::new(move |vm, args| {
        let receiver = args.first().cloned().unwrap_or(Value::Undefined);
        let (_, _, data, kind, length, byte_offset, prototype) = typed_view_details(&receiver)
          .filter(|(_, _, _, kind, _, _, _)| {
            matches!(
              kind.as_str(),
              "Uint8Array" | "Int8Array" | "Uint8ClampedArray"
            )
          })
          .ok_or_else(|| {
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
        let (buffer, new_data) = new_byte_buffer(&arraybuffer_proto_map, copy_length);
        for index in 0..copy_length {
          let value = data
            .borrow()
            .get(byte_offset + begin + index)
            .and_then(|slot| slot.clone())
            .unwrap_or(Value::Number(0.0));
          let byte = encode_byte(vm, &kind, &value)?;
          new_data.borrow_mut()[index] = Some(Value::Number(f64::from(byte)));
        }
        Ok(new_typed_array_view(
          prototype,
          buffer,
          new_data,
          &kind,
          copy_length,
          0,
        ))
      }),
      function_proto_map,
    )
  };
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
      let (_, _, _, _kind, target_length, _, _) = typed_view_details(&receiver)
        .filter(|(_, _, _, kind, _, _, _)| {
          matches!(
            kind.as_str(),
            "Uint8Array" | "Int8Array" | "Uint8ClampedArray"
          )
        })
        .ok_or_else(|| {
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

          if $shared {
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
              let byte = encode_byte(vm, &ctor_name, &value)?;
              if let Some(slot) = backing_data
                .borrow_mut()
                .get_mut(byte_offset + idx)
              {
                *slot = Some(Value::Number(f64::from(byte)));
              }
            }
            return Ok(new_byte_view(
              instance_proto,
              buffer_slot,
              backing_data,
              &ctor_name,
              length,
              byte_offset,
            ));
          }
          let typed_data_value = Value::Array(
            Rc::new(RefCell::new(ArrayData::from(vec![
              Some($zero_value.clone());
              length
            ]))),
            Rc::new(RefCell::new(ObjectData::new())),
          );
          let mut typed_array_data = ObjectData::with_prototype(instance_proto);
          typed_array_data.properties.insert(
            PropertyKey::String("[[TypedArrayData]]".to_string().into()),
            PropertyDescriptor::ordinary_data(typed_data_value),
          );
          typed_array_data.properties.insert(
            PropertyKey::String("[[TypedArrayKind]]".to_string().into()),
            PropertyDescriptor::ordinary_data(Value::String(ctor_name.clone().into())),
          );
          typed_array_data.properties.insert(
            PropertyKey::String("[[TypedArrayBytesPerElement]]".to_string().into()),
            PropertyDescriptor::ordinary_data(Value::Number(usize_to_f64($element_count))),
          );
          typed_array_data.properties.insert(
            PropertyKey::String("[[TypedArrayLength]]".to_string().into()),
            PropertyDescriptor::ordinary_data(Value::Number(usize_to_f64(length))),
          );
          typed_array_data.properties.insert(
            PropertyKey::String("[[TypedArrayByteLength]]".to_string().into()),
            PropertyDescriptor::ordinary_data(Value::Number(usize_to_f64(view_byte_length))),
          );
          typed_array_data.properties.insert(
            PropertyKey::String("[[TypedArrayByteOffset]]".to_string().into()),
            PropertyDescriptor::ordinary_data(Value::Number(usize_to_f64(byte_offset))),
          );
          typed_array_data.properties.insert(
            PropertyKey::String("[[TypedArrayBuffer]]".to_string().into()),
            PropertyDescriptor::ordinary_data(buffer_slot.clone()),
          );
          typed_array_data.properties.insert(
            PropertyKey::String("length".to_string().into()),
            PropertyDescriptor::ordinary_data(Value::Number(usize_to_f64(length))),
          );
          typed_array_data.properties.insert(
            PropertyKey::String("byteLength".to_string().into()),
            PropertyDescriptor::ordinary_data(Value::Number(usize_to_f64(view_byte_length))),
          );
          for (idx, value) in initial_values.into_iter().enumerate() {
            typed_array_data.properties.insert(
              PropertyKey::String(usize_to_decimal_string(idx).into()),
              PropertyDescriptor::ordinary_data(value),
            );
          }
          Ok(Value::Object(Rc::new(RefCell::new(typed_array_data))))
        })
      };
      let ctor_val = Value::BuiltinObject(ctor_fn.clone(), ctor_map.clone());
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
    Value::BigInt(BigIntValue::from(0u8)),
    false
  );
  let (biguint64array_ctor_val, _) = create_typed_array_ctor!(
    "BigUint64Array",
    8,
    Value::BigInt(BigIntValue::from(0u8)),
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
