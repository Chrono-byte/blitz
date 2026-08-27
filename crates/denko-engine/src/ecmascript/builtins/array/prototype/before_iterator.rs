// SPDX-License-Identifier: MPL-2.0
//! Array prototype methods installed before iterator methods.

use super::*;

pub(in crate::ecmascript::builtins::array) fn install_before_iterator(
  object_proto_map: &Rc<RefCell<ObjectData>>,
  function_proto_map: &Rc<RefCell<ObjectData>>,
) -> Rc<RefCell<ObjectData>> {
  use PropertyKey as PKey;

  let array_proto_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(ObjectData::with_prototype(
    Value::Object(Rc::clone(object_proto_map)),
  )));
  array_proto_map.borrow_mut().properties.insert(
    PKey::String("length".into()),
    PropertyDescriptor::Data {
      value: Value::Number(0.0),
      writable: true,
      enumerable: false,
      configurable: false,
    },
  );
  let for_each_fn = builtin_object(
    Rc::new(|vm, args| {
      let receiver = vm.to_object_value(args.first().cloned().unwrap_or(Value::Undefined))?;
      let callback = args.get(1).cloned().unwrap_or(Value::Undefined);
      if !Vm::is_callable_value(&callback) {
        return Err(vm.throw_type_error("Array.prototype.forEach callback is not callable"));
      }
      let this_arg = args.get(2).cloned().unwrap_or(Value::Undefined);
      let len = array_like_length(vm, &receiver)?;
      for i in 0..len {
        let key = PKey::String(i.to_string().into());
        if vm.has_property_host(&receiver, &key)? {
          let item = vm.get_property_host(&receiver, &key)?;
          vm.call_value_with_this(
            &callback,
            &this_arg,
            &[item, Value::Number(usize_to_f64(i)), receiver.clone()],
          )?;
        }
      }
      Ok(Value::Undefined)
    }),
    function_proto_map,
  );
  array_proto_map.borrow_mut().properties.insert(
    PKey::String("forEach".to_string().into()),
    PropertyDescriptor::builtin_data(for_each_fn),
  );
  array_proto_map.borrow_mut().properties.insert(
    PKey::String("map".to_string().into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new(|vm, args| {
        let receiver = vm.to_object_value(args.first().cloned().unwrap_or(Value::Undefined))?;
        let callback = args.get(1).cloned().unwrap_or(Value::Undefined);
        let this_arg = args.get(2).cloned().unwrap_or(Value::Undefined);
        if !Vm::is_callable_value(&callback) {
          return Err(vm.throw_type_error("Array.prototype.map callback is not callable"));
        }

        let len = array_like_length(vm, &receiver)?;
        if len > u32::MAX as usize {
          return Err(vm.throw_range_error("Invalid array length"));
        }
        let mut out: Vec<Option<Value>> = Vec::with_capacity(len);
        for i in 0..len {
          let key = PKey::String(i.to_string().into());
          // Check whether the property exists, including inherited properties.
          let has_prop = vm.has_property_host(&receiver, &key)?;
          if has_prop {
            let item = vm.get_property_host(&receiver, &key)?;
            let mapped = vm.call_value_with_this(
              &callback,
              &this_arg,
              &[item, Value::Number(usize_to_f64(i)), receiver.clone()],
            )?;
            out.push(Some(mapped));
          } else {
            // Preserve hole
            out.push(None);
          }
        }

        let result = array_species_create(vm, &receiver, len)?;
        for (index, value) in out.into_iter().enumerate() {
          if let Some(value) = value {
            create_data_property_or_throw(
              vm,
              &result,
              &Value::String(index.to_string().into()),
              value,
            )?;
          }
        }
        vm.set_property_host(
          &result,
          &Value::String("length".into()),
          Value::Number(usize_to_f64(len)),
        )?;
        Ok(result)
      }),
      function_proto_map,
    )),
  );
  array_proto_map.borrow_mut().properties.insert(
    PKey::String("join".to_string().into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new(|vm, args| {
        let receiver = vm.to_object_value(args.first().cloned().unwrap_or(Value::Undefined))?;
        let sep = match args.get(1) {
          Some(Value::Undefined) | None => js_string::JsString::from(","),
          Some(v) => vm.to_string(v)?,
        };
        let len = array_like_length(vm, &receiver)?;
        let mut out = js_string::JsStringBuilder::new();
        for index in 0..len {
          if index > 0 {
            out.push_string(&sep);
          }
          let item = vm.get_property_host(&receiver, &PKey::String(index.to_string().into()))?;
          match item {
            Value::Undefined | Value::Null => {}
            value => out.push_string(&vm.to_string(&value)?),
          }
        }
        Ok(Value::String(out.finish()))
      }),
      function_proto_map,
    )),
  );
  array_proto_map.borrow_mut().properties.insert(
    PKey::String("toString".to_string().into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new(|vm, args| {
        let this = args.first().cloned().unwrap_or(Value::Undefined);
        let join = vm.get_property_host(&this, &PKey::String("join".to_string().into()))?;
        vm.call_value_with_this(&join, &this, &[])
      }),
      function_proto_map,
    )),
  );
  array_proto_map
    .borrow_mut()
    .properties
    .insert(PKey::String("sort".to_string().into()), {
      let sort_fn = builtin_object(
        Rc::new(|vm, args| {
          let this = args.first().cloned().unwrap_or(Value::Undefined);
          let compare_fn = args.get(1).cloned().unwrap_or(Value::Undefined);
          let compare_is_undefined = matches!(compare_fn, Value::Undefined);
          let compare_is_callable = compare_is_undefined
            || matches!(
              compare_fn,
              Value::Builtin(_) | Value::BuiltinObject(_, _) | Value::Closure { .. }
            );
          if !compare_is_callable {
            return Err(vm.throw_type_error("Array.prototype.sort comparefn is not callable"));
          }

          let len = array_like_length(vm, &this)?;
          let (keys, snapshot) = collect_indexed_snapshot(vm, &this, len)?;
          let mut values: Vec<Value> = Vec::new();
          let mut undefined_count = 0usize;
          for idx in &keys {
            match snapshot
              .get(idx)
              .cloned()
              .unwrap_or(Value::Undefined)
            {
              Value::Undefined => undefined_count += 1,
              v => values.push(v),
            }
          }

          let compare_pair =
            |vm: &mut Vm, a: &Value, b: &Value| -> Result<Ordering, RuntimeError> {
              if compare_is_undefined {
                let sa = vm.to_string(a)?;
                let sb = vm.to_string(b)?;
                return Ok(sa.cmp(&sb));
              }
              let cmp =
                vm.call_value_with_this(&compare_fn, &Value::Undefined, &[a.clone(), b.clone()])?;
              let cmp_num = vm.to_number_value(&cmp)?;
              Ok(if cmp_num.is_nan() || cmp_num == 0.0 {
                Ordering::Equal
              } else if cmp_num < 0.0 {
                Ordering::Less
              } else {
                Ordering::Greater
              })
            };
          let n = values.len();
          for i in 0..n {
            for j in (i + 1)..n {
              if compare_pair(vm, &values[i], &values[j])? == Ordering::Greater {
                values.swap(i, j);
              }
            }
          }

          let cutoff = values.len() + undefined_count;
          let mut next_index = 0usize;
          for value in values {
            vm.set_property_host(&this, &Value::String(next_index.to_string().into()), value)?;
            next_index += 1;
          }
          for _ in 0..undefined_count {
            vm.set_property_host(
              &this,
              &Value::String(next_index.to_string().into()),
              Value::Undefined,
            )?;
            next_index += 1;
          }
          for idx in keys {
            if idx >= cutoff {
              delete_property_or_throw(vm, &this, &PKey::String(idx.to_string().into()))?;
            }
          }
          Ok(this)
        }),
        function_proto_map,
      );
      set_builtin_length(&sort_fn, 1.0);
      PropertyDescriptor::builtin_data(sort_fn)
    });
  array_proto_map.borrow_mut().properties.insert(
    PKey::String("toReversed".to_string().into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new({
        let array_proto_map = Rc::clone(&array_proto_map);
        move |vm, args| {
          let receiver = args.first().cloned().unwrap_or(Value::Undefined);
          let len = array_like_length(vm, &receiver)?;
          let mut out: Vec<Option<Value>> = Vec::with_capacity(len);
          for k in 0..len {
            let from = len - 1 - k;
            let key = PKey::String(from.to_string().into());
            if vm.has_property_host(&receiver, &key)? {
              out.push(Some(vm.get_property_host(&receiver, &key)?));
            } else {
              out.push(None);
            }
          }
          let arr_rc = Rc::new(RefCell::new(ArrayData::from(out)));
          let props = Rc::new(RefCell::new(ObjectData::new()));
          props
            .borrow_mut()
            .set_prototype_value(Value::Object(Rc::clone(&array_proto_map)));
          props.borrow_mut().properties.insert(
            PKey::String("length".to_string().into()),
            array_length_descriptor(arr_rc.borrow().len()),
          );
          Ok(Value::Array(arr_rc, props))
        }
      }),
      function_proto_map,
    )),
  );
  array_proto_map.borrow_mut().properties.insert(
    PKey::String("toSorted".to_string().into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new({
        let array_proto_map = Rc::clone(&array_proto_map);
        move |vm, args| {
          let receiver = args.first().cloned().unwrap_or(Value::Undefined);
          let compare_fn = args.get(1).cloned().unwrap_or(Value::Undefined);
          let has_compare = !matches!(compare_fn, Value::Undefined);
          if has_compare && !Vm::is_callable_value(&compare_fn) {
            return Err(vm.throw_type_error("Array.prototype.toSorted comparator is not callable"));
          }
          let len = array_like_length(vm, &receiver)?;
          if len > u32::MAX as usize {
            return Err(vm.throw_range_error("Invalid array length"));
          }
          let mut values: Vec<Value> = Vec::new();
          let mut undefined_count = 0usize;
          for i in 0..len {
            let key = PKey::String(i.to_string().into());
            if vm.has_property_host(&receiver, &key)? {
              let item = vm.get_property_host(&receiver, &key)?;
              match item {
                Value::Undefined => undefined_count += 1,
                v => values.push(v),
              }
            } else {
              undefined_count += 1;
            }
          }
          let n = values.len();
          for i in 0..n {
            for j in (i + 1)..n {
              let swap = if has_compare {
                let cmp = vm.call_value_with_this(
                  &compare_fn,
                  &Value::Undefined,
                  &[values[i].clone(), values[j].clone()],
                )?;
                let cmp_num = vm.to_number_value(&cmp)?;
                cmp_num > 0.0
              } else {
                let ai = vm.to_string(&values[i])?;
                let aj = vm.to_string(&values[j])?;
                ai > aj
              };
              if swap {
                values.swap(i, j);
              }
            }
          }
          while undefined_count > 0 {
            values.push(Value::Undefined);
            undefined_count -= 1;
          }
          let out: Vec<Option<Value>> = values.into_iter().map(Some).collect();
          let arr_rc = Rc::new(RefCell::new(ArrayData::from(out)));
          let props = Rc::new(RefCell::new(ObjectData::new()));
          props
            .borrow_mut()
            .set_prototype_value(Value::Object(Rc::clone(&array_proto_map)));
          props.borrow_mut().properties.insert(
            PKey::String("length".to_string().into()),
            array_length_descriptor(arr_rc.borrow().len()),
          );
          Ok(Value::Array(arr_rc, props))
        }
      }),
      function_proto_map,
    )),
  );
  array_proto_map.borrow_mut().properties.insert(
    PKey::String("with".to_string().into()),
    PropertyDescriptor::builtin_data({
      let builtin = builtin_object(
        Rc::new({
          let array_proto_map = Rc::clone(&array_proto_map);
          move |vm, args| {
            let receiver = args.first().cloned().unwrap_or(Value::Undefined);
            let len = array_like_length(vm, &receiver)?;
            let relative_index =
              to_integer_or_infinity(vm, args.get(1).unwrap_or(&Value::Undefined))?;
            let actual_index = if relative_index < 0 {
              len as isize + relative_index
            } else {
              relative_index
            };
            if actual_index < 0 || actual_index as usize >= len {
              return Err(vm.throw_range_error("Array.prototype.with index out of range"));
            }
            let actual_index = actual_index as usize;
            let replacement = args.get(2).cloned().unwrap_or(Value::Undefined);
            let mut out: Vec<Option<Value>> = Vec::with_capacity(len);
            for i in 0..len {
              if i == actual_index {
                out.push(Some(replacement.clone()));
                continue;
              }
              let key = PKey::String(i.to_string().into());
              let value = if matches!(receiver, Value::Array(..))
                && own_property_descriptor(&receiver, &key).is_none()
              {
                vm.get_property_host(&Value::Object(Rc::clone(&array_proto_map)), &key)?
              } else {
                vm.get_property_host(&receiver, &key)?
              };
              out.push(Some(value));
            }
            let arr_rc = Rc::new(RefCell::new(ArrayData::from(out)));
            let props = Rc::new(RefCell::new(ObjectData::new()));
            props
              .borrow_mut()
              .set_prototype_value(Value::Object(Rc::clone(&array_proto_map)));
            props.borrow_mut().properties.insert(
              PKey::String("length".to_string().into()),
              array_length_descriptor(arr_rc.borrow().len()),
            );
            Ok(Value::Array(arr_rc, props))
          }
        }),
        function_proto_map,
      );
      set_builtin_name(&builtin, "with");
      set_builtin_length(&builtin, 2.0);
      builtin
    }),
  );
  let push = builtin_object(
    Rc::new(|vm, args| {
      let receiver = vm.to_object_value(args.first().cloned().unwrap_or(Value::Undefined))?;
      let length = array_like_length(vm, &receiver)?;
      let item_count = args.len().saturating_sub(1);
      let new_length = length
        .checked_add(item_count)
        .filter(|length| *length <= 9_007_199_254_740_991usize)
        .ok_or_else(|| vm.throw_type_error("Array.prototype.push result is too large"))?;
      if !array_length_is_writable(&receiver) && item_count > 0 {
        return Err(vm.throw_type_error("Array length is not writable"));
      }
      for (offset, item) in args.iter().skip(1).cloned().enumerate() {
        vm.set_property_host(
          &receiver,
          &Value::String((length + offset).to_string().into()),
          item,
        )?;
      }
      vm.set_property_host(
        &receiver,
        &Value::String("length".to_string().into()),
        Value::Number(usize_to_f64(new_length)),
      )?;
      Ok(Value::Number(usize_to_f64(new_length)))
    }),
    function_proto_map,
  );
  if let Value::BuiltinObject(_, properties) = &push {
    properties
      .borrow_mut()
      .set_internal_slot("[[DenkoArrayPush]]", Value::Bool(true));
  }
  array_proto_map.borrow_mut().properties.insert(
    PKey::String("push".to_string().into()),
    PropertyDescriptor::builtin_data(push),
  );
  array_proto_map
    .borrow_mut()
    .properties
    .insert(PKey::String("splice".to_string().into()), {
      let splice_fn = builtin_object(
        Rc::new(move |vm, args| {
          let this = args.first().cloned().unwrap_or(Value::Undefined);
          let len = array_like_length(vm, &this)?;
          let arg_count = args.len().saturating_sub(1);
          let start_raw = vm.to_number_value(args.get(1).unwrap_or(&Value::Number(0.0)))?;
          let start = if start_raw < 0.0 {
            let back = start_raw.abs().trunc().to_usize().unwrap_or(len);
            len.saturating_sub(back)
          } else {
            start_raw
              .trunc()
              .to_usize()
              .unwrap_or(len)
              .min(len)
          };
          let actual_delete_count = if arg_count == 0 {
            0
          } else if arg_count == 1 {
            len.saturating_sub(start)
          } else {
            let delete_count = vm
              .to_number_value(
                args
                  .get(2)
                  .expect("args.get(2) should exist when arg_count >= 2"),
              )?
              .max(0.0)
              .trunc()
              .to_usize()
              .unwrap_or(0);
            delete_count.min(len.saturating_sub(start))
          };
          if actual_delete_count > u32::MAX as usize {
            return Err(vm.throw_range_error("Invalid array length"));
          }
          let insert_items: Vec<Value> = args.iter().skip(3).cloned().collect();
          let insert_count = insert_items.len();
          let new_len = len
            .saturating_sub(actual_delete_count)
            .saturating_add(insert_count);
          if new_len > u32::MAX as usize {
            return Err(vm.throw_range_error("Invalid array length"));
          }
          if new_len > 9_007_199_254_740_991usize {
            return Err(vm.throw_type_error("Array length exceeds supported size"));
          }
          let removed = array_species_create(vm, &this, actual_delete_count)?;
          for index in 0..actual_delete_count {
            let from = start + index;
            let from_key = PKey::String(from.to_string().into());
            if vm.has_property_host(&this, &from_key)? {
              let value = vm.get_property_host(&this, &from_key)?;
              create_data_property_or_throw(
                vm,
                &removed,
                &Value::String(index.to_string().into()),
                value,
              )?;
            }
          }
          vm.set_property_host(
            &removed,
            &Value::String("length".into()),
            Value::Number(usize_to_f64(actual_delete_count)),
          )?;

          if insert_count < actual_delete_count {
            for index in start..len.saturating_sub(actual_delete_count) {
              let from = index + actual_delete_count;
              let to = index + insert_count;
              let from_key = PKey::String(from.to_string().into());
              if vm.has_property_host(&this, &from_key)? {
                let value = vm.get_property_host(&this, &from_key)?;
                vm.set_property_host(&this, &Value::String(to.to_string().into()), value)?;
              } else {
                delete_property_or_throw(vm, &this, &PKey::String(to.to_string().into()))?;
              }
            }
            for index in (new_len..len).rev() {
              delete_property_or_throw(vm, &this, &PKey::String(index.to_string().into()))?;
            }
          } else if insert_count > actual_delete_count {
            for index in (start..len.saturating_sub(actual_delete_count)).rev() {
              let from = index + actual_delete_count;
              let to = index + insert_count;
              let from_key = PKey::String(from.to_string().into());
              if vm.has_property_host(&this, &from_key)? {
                let value = vm.get_property_host(&this, &from_key)?;
                vm.set_property_host(&this, &Value::String(to.to_string().into()), value)?;
              } else {
                delete_property_or_throw(vm, &this, &PKey::String(to.to_string().into()))?;
              }
            }
          }
          for (offset, value) in insert_items.into_iter().enumerate() {
            let idx = start + offset;
            vm.set_property_host(&this, &Value::String(idx.to_string().into()), value)?;
          }
          vm.set_property_host(
            &this,
            &Value::String("length".to_string().into()),
            Value::Number(usize_to_f64(new_len)),
          )?;
          Ok(removed)
        }),
        function_proto_map,
      );
      set_builtin_length(&splice_fn, 2.0);
      PropertyDescriptor::builtin_data(splice_fn)
    });
  array_proto_map
    .borrow_mut()
    .properties
    .insert(PKey::String("shift".to_string().into()), {
      let shift_fn = builtin_object(
        Rc::new(move |vm, args| {
          let this = args.first().cloned().unwrap_or(Value::Undefined);
          let len = array_like_length(vm, &this)?;
          if len == 0 {
            vm.set_property_host(&this, &Value::String("length".into()), Value::Number(0.0))?;
            return Ok(Value::Undefined);
          }
          let result = vm.get_property_host(&this, &PKey::String("0".to_string().into()))?;
          for from in 1..len {
            let from_key = PKey::String(from.to_string().into());
            let to_key = PKey::String((from - 1).to_string().into());
            if vm.has_property_host(&this, &from_key)? {
              let value = vm.get_property_host(&this, &from_key)?;
              vm.set_property_host(&this, &Value::String((from - 1).to_string().into()), value)?;
            } else {
              delete_property_or_throw(vm, &this, &to_key)?;
            }
          }
          delete_property_or_throw(vm, &this, &PKey::String((len - 1).to_string().into()))?;
          vm.set_property_host(
            &this,
            &Value::String("length".to_string().into()),
            Value::Number(usize_to_f64(len - 1)),
          )?;
          Ok(result)
        }),
        function_proto_map,
      );
      set_builtin_length(&shift_fn, 0.0);
      PropertyDescriptor::builtin_data(shift_fn)
    });
  array_proto_map
    .borrow_mut()
    .properties
    .insert(PKey::String("unshift".to_string().into()), {
      let unshift_fn = builtin_object(
        Rc::new(move |vm, args| {
          let this = args.first().cloned().unwrap_or(Value::Undefined);
          let len = array_like_length(vm, &this)?;
          let insert_items: Vec<Value> = args.iter().skip(1).cloned().collect();
          let insert_count = insert_items.len();
          if insert_count == 0 {
            return Ok(Value::Number(usize_to_f64(len)));
          }
          let new_len = len
            .checked_add(insert_count)
            .ok_or_else(|| vm.throw_type_error("Array length exceeds supported size"))?;
          if new_len > 9_007_199_254_740_991usize {
            return Err(vm.throw_type_error("Array length exceeds supported size"));
          }
          for from in (0..len).rev() {
            let from_key = PKey::String(from.to_string().into());
            let to = from + insert_count;
            if vm.has_property_host(&this, &from_key)? {
              let value = vm.get_property_host(&this, &from_key)?;
              vm.set_property_host(&this, &Value::String(to.to_string().into()), value)?;
            } else {
              delete_property_or_throw(vm, &this, &PKey::String(to.to_string().into()))?;
            }
          }
          for (idx, value) in insert_items.into_iter().enumerate() {
            vm.set_property_host(&this, &Value::String(idx.to_string().into()), value)?;
          }
          vm.set_property_host(
            &this,
            &Value::String("length".to_string().into()),
            Value::Number(usize_to_f64(new_len)),
          )?;
          Ok(Value::Number(usize_to_f64(new_len)))
        }),
        function_proto_map,
      );
      set_builtin_length(&unshift_fn, 0.0);
      PropertyDescriptor::builtin_data(unshift_fn)
    });
  array_proto_map.borrow_mut().properties.insert(
    PKey::String("toSpliced".to_string().into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new({
        let array_proto_map = Rc::clone(&array_proto_map);
        move |vm, args| {
          let receiver = args.first().cloned().unwrap_or(Value::Undefined);
          let len = array_like_length(vm, &receiver)?;
          let max_safe_len = 9_007_199_254_740_991u128;
          let len = if len == 0 {
            0usize
          } else {
            len.min(max_safe_len as usize)
          };
          let insert_count = args.len().saturating_sub(3);
          let start_raw = vm.to_number_value(args.get(1).unwrap_or(&Value::Number(0.0)))?;
          let start = if start_raw < 0.0 {
            let back = start_raw.abs().trunc().to_usize().unwrap_or(len);
            len.saturating_sub(back)
          } else {
            start_raw
              .trunc()
              .to_usize()
              .unwrap_or(len)
              .min(len)
          };
          let delete_count = if let Some(dc) = args.get(2) {
            vm.to_number_value(dc)?
              .max(0.0)
              .trunc()
              .to_usize()
              .unwrap_or(0)
          } else {
            len.saturating_sub(start)
          };
          let actual_delete_count = delete_count.min(len.saturating_sub(start));
          let new_len = (len as u128)
            .checked_sub(actual_delete_count as u128)
            .and_then(|n| n.checked_add(insert_count as u128))
            .ok_or_else(|| vm.throw_type_error("Array length exceeds supported size"))?;
          if new_len > max_safe_len {
            return Err(vm.throw_type_error("Array length exceeds supported size"));
          }
          if new_len > 4_294_967_295u128 {
            return Err(vm.throw_range_error("Invalid array length"));
          }
          let mut out: Vec<Option<Value>> = Vec::new();
          for i in 0..start {
            let key = PKey::String(i.to_string().into());
            if vm.has_property_host(&receiver, &key)? {
              out.push(Some(vm.get_property_host(&receiver, &key)?));
            } else {
              out.push(None);
            }
          }
          out.extend(args.iter().skip(3).cloned().map(Some));
          for i in start.saturating_add(actual_delete_count)..len {
            let key = PKey::String(i.to_string().into());
            if vm.has_property_host(&receiver, &key)? {
              out.push(Some(vm.get_property_host(&receiver, &key)?));
            } else {
              out.push(None);
            }
          }
          let arr_rc = Rc::new(RefCell::new(ArrayData::from(out)));
          let props = Rc::new(RefCell::new(ObjectData::new()));
          props
            .borrow_mut()
            .set_prototype_value(Value::Object(Rc::clone(&array_proto_map)));
          props.borrow_mut().properties.insert(
            PKey::String("length".to_string().into()),
            array_length_descriptor(arr_rc.borrow().len()),
          );
          Ok(Value::Array(arr_rc, props))
        }
      }),
      function_proto_map,
    )),
  );
  array_proto_map.borrow_mut().properties.insert(
    PKey::String("concat".to_string().into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new(move |vm, args| {
        fn is_array_or_throw(
          vm: &mut Vm,
          value: &Value,
        ) -> Result<bool, RuntimeError> {
          match value {
            Value::Array(..) => Ok(true),
            Value::Proxy(proxy) => {
              let target = {
                let proxy = proxy.borrow();
                if proxy.is_revoked() {
                  return Err(vm.throw_type_error("Cannot inspect a revoked Proxy"));
                }
                proxy.target.clone()
              };
              is_array_or_throw(vm, &target)
            }
            _ => Ok(false),
          }
        }

        let receiver = vm.to_object_value(args.first().cloned().unwrap_or(Value::Undefined))?;
        let result = array_species_create(vm, &receiver, 0)?;
        let mut next_index = 0usize;
        for value in std::iter::once(receiver.clone()).chain(args.iter().skip(1).cloned()) {
          let spreadable = if value.is_object() {
            let spreadable = vm.get_property_host(
              &value,
              &PKey::Symbol(SymbolId::well_known_is_concat_spreadable()),
            )?;
            if matches!(spreadable, Value::Undefined) {
              is_array_or_throw(vm, &value)?
            } else {
              vm.to_boolean(&spreadable)
            }
          } else {
            false
          };
          if spreadable {
            let length = array_like_length(vm, &value)?;
            if next_index
              .checked_add(length)
              .is_none_or(|length| length > 9_007_199_254_740_991usize)
            {
              return Err(vm.throw_type_error("Array.prototype.concat result is too large"));
            }
            for source_index in 0..length {
              let source_key = PKey::String(source_index.to_string().into());
              if vm.has_property_host(&value, &source_key)? {
                let element = vm.get_property_host(&value, &source_key)?;
                create_data_property_or_throw(
                  vm,
                  &result,
                  &Value::String(next_index.to_string().into()),
                  element,
                )?;
              }
              next_index += 1;
            }
          } else {
            if next_index >= 9_007_199_254_740_991usize {
              return Err(vm.throw_type_error("Array.prototype.concat result is too large"));
            }
            create_data_property_or_throw(
              vm,
              &result,
              &Value::String(next_index.to_string().into()),
              value,
            )?;
            next_index += 1;
          }
        }
        vm.set_property_host(
          &result,
          &Value::String("length".into()),
          Value::Number(usize_to_f64(next_index)),
        )?;
        Ok(result)
      }),
      function_proto_map,
    )),
  );

  for (name, length) in [
    ("forEach", 1.0),
    ("map", 1.0),
    ("join", 1.0),
    ("toString", 0.0),
    ("sort", 1.0),
    ("toReversed", 0.0),
    ("toSorted", 1.0),
    ("with", 2.0),
    ("push", 1.0),
    ("splice", 2.0),
    ("shift", 0.0),
    ("unshift", 1.0),
    ("toSpliced", 2.0),
    ("concat", 1.0),
  ] {
    let builtin = array_proto_map
      .borrow()
      .properties
      .get(&PKey::String(name.into()))
      .and_then(|descriptor| match descriptor {
        PropertyDescriptor::Data { value, .. } => Some(value.clone()),
        _ => None,
      });
    if let Some(builtin) = builtin {
      set_builtin_name(&builtin, name);
      set_builtin_length(&builtin, length);
    }
  }
  array_proto_map
}
