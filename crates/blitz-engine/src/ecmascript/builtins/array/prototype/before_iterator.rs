// SPDX-License-Identifier: MPL-2.0
//! Array prototype methods installed before iterator methods.

use super::*;

pub(in crate::ecmascript::builtins::array) fn install_before_iterator(
  object_proto_map: &Rc<RefCell<ObjectData>>,
  function_proto_map: &Rc<RefCell<ObjectData>>,
) -> Rc<RefCell<ObjectData>> {
  use PropertyKey as PKey;

  let array_proto_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(ObjectData::with_prototype(
    Value::Object(Rc::clone(&object_proto_map)),
  )));
  let for_each_fn = builtin_object(
    Rc::new(|vm, args| {
      let this = args.first().cloned().unwrap_or(Value::Undefined);
      let callback = args.get(1).cloned().unwrap_or(Value::Undefined);
      let this_for_call = this.clone();
      if let Value::Array(arr, _) = &this {
        // Snapshot initial length per ECMAScript semantics and avoid
        // holding a RefCell borrow across callback invocation.
        let len = arr.borrow().len();
        for i in 0..len {
          let item = arr.borrow().get(i).cloned().flatten();
          if let Some(item) = item {
            let idx = Value::Number(usize_to_f64(i));
            vm.call_value_with_this(
              &callback,
              &this_for_call,
              &[item, idx, this_for_call.clone()],
            )?;
          }
        }
      }
      Ok(Value::Undefined)
    }),
    &function_proto_map,
  );
  array_proto_map.borrow_mut().properties.insert(
    PKey::String("forEach".to_string().into()),
    PropertyDescriptor::builtin_data(for_each_fn),
  );
  let array_proto_for_map = Rc::clone(&array_proto_map);
  array_proto_map.borrow_mut().properties.insert(
    PKey::String("map".to_string().into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new(move |vm, args| {
        let receiver = args.first().cloned().unwrap_or(Value::Undefined);
        let callback = args.get(1).cloned().unwrap_or(Value::Undefined);
        let this_arg = args.get(2).cloned().unwrap_or(Value::Undefined);

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

        let arr_rc = Rc::new(RefCell::new(ArrayData::from(out)));
        let props: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(ObjectData::new()));
        props
          .borrow_mut()
          .set_prototype_value(Value::Object(Rc::clone(&array_proto_for_map)));
        props.borrow_mut().properties.insert(
          PKey::String("length".to_string().into()),
          array_length_descriptor(arr_rc.borrow().len()),
        );
        Ok(Value::Array(arr_rc, props))
      }),
      &function_proto_map,
    )),
  );
  array_proto_map.borrow_mut().properties.insert(
    PKey::String("join".to_string().into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new(|vm, args| {
        let this = args.first().cloned().unwrap_or(Value::Undefined);
        let sep = match args.get(1) {
          Some(Value::Undefined) | None => js_string::JsString::from(","),
          Some(v) => vm.to_string(v)?,
        };
        match this {
          Value::Array(arr, _) => {
            let mut out = js_string::JsStringBuilder::new();
            let mut first = true;
            for item_slot in arr.borrow().iter() {
              if !first {
                out.push_string(&sep);
              }
              first = false;
              if let Some(item) = item_slot {
                match item {
                  Value::Undefined | Value::Null => {}
                  v => out.push_string(&vm.to_string(v)?),
                }
              }
            }
            Ok(Value::String(out.finish()))
          }
          _ => Ok(Value::String("".to_string().into())),
        }
      }),
      &function_proto_map,
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
      &function_proto_map,
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
              let _ = vm.delete_property_host(&this, &PKey::String(idx.to_string().into()))?;
            }
          }
          Ok(this)
        }),
        &function_proto_map,
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
      &function_proto_map,
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
          let has_compare = matches!(
            compare_fn,
            Value::Builtin(_) | Value::BuiltinObject(_, _) | Value::Closure { .. }
          );
          let len = array_like_length(vm, &receiver)?;
          let mut values: Vec<Value> = Vec::new();
          let mut undefined_count = 0usize;
          let mut hole_count = 0usize;
          for i in 0..len {
            let key = PKey::String(i.to_string().into());
            if vm.has_property_host(&receiver, &key)? {
              let item = vm.get_property_host(&receiver, &key)?;
              match item {
                Value::Undefined => undefined_count += 1,
                v => values.push(v),
              }
            } else {
              hole_count += 1;
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
                cmp_num.is_finite() && cmp_num > 0.0
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
          let mut out: Vec<Option<Value>> = values.into_iter().map(Some).collect();
          out.extend((0..hole_count).map(|_| None));
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
      &function_proto_map,
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
        &function_proto_map,
      );
      set_builtin_name(&builtin, "with");
      set_builtin_length(&builtin, 2.0);
      builtin
    }),
  );
  array_proto_map.borrow_mut().properties.insert(
    PKey::String("push".to_string().into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new(|_vm, args| {
        let this = args.first().cloned().unwrap_or(Value::Undefined);
        let (items, props) = match this {
          Value::Array(arr, props) => (arr, props),
          _ => return Ok(Value::Number(0.0)),
        };
        let mut vec = items.borrow_mut();
        for v in args.iter().skip(1).cloned() {
          vec.push(Some(v));
        }
        let new_len = usize_to_f64(vec.len());
        drop(vec);
        props.borrow_mut().properties.insert(
          PKey::String("length".to_string().into()),
          PropertyDescriptor::ordinary_data(Value::Number(new_len)),
        );
        Ok(Value::Number(new_len))
      }),
      &function_proto_map,
    )),
  );
  array_proto_map
    .borrow_mut()
    .properties
    .insert(PKey::String("splice".to_string().into()), {
      let splice_fn = builtin_object(
        Rc::new({
          let array_proto_map = Rc::clone(&array_proto_map);
          move |vm, args| {
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
            let (keys, snapshot) = collect_indexed_snapshot(vm, &this, len)?;
            let mut removed: Vec<Option<Value>> = Vec::with_capacity(actual_delete_count);
            for i in 0..actual_delete_count {
              removed.push(snapshot.get(&(start + i)).cloned());
            }
            let removed_arr = Rc::new(RefCell::new(ArrayData::from(removed)));
            let removed_props = Rc::new(RefCell::new(ObjectData::new()));
            removed_props
              .borrow_mut()
              .set_prototype_value(Value::Object(Rc::clone(&array_proto_map)));
            removed_props.borrow_mut().properties.insert(
              PKey::String("length".to_string().into()),
              array_length_descriptor(removed_arr.borrow().len()),
            );

            let delta = insert_count as isize - actual_delete_count as isize;
            let mut written_targets: HashSet<usize> = HashSet::new();
            for idx in &keys {
              let target = if *idx < start {
                Some(*idx)
              } else if *idx >= start + actual_delete_count {
                idx.checked_add_signed(delta)
              } else {
                None
              };
              if let Some(target_idx) = target
                && let Some(value) = snapshot.get(idx).cloned()
              {
                vm.set_property_host(&this, &Value::String(target_idx.to_string().into()), value)?;
                written_targets.insert(target_idx);
              }
            }
            for (offset, value) in insert_items.into_iter().enumerate() {
              let idx = start + offset;
              vm.set_property_host(&this, &Value::String(idx.to_string().into()), value)?;
              written_targets.insert(idx);
            }
            for idx in keys {
              if idx >= start && !written_targets.contains(&idx) {
                let _ = vm.delete_property_host(&this, &PKey::String(idx.to_string().into()))?;
              }
            }
            if !array_length_is_writable(&this) && new_len != len {
              return Err(vm.throw_type_error("Array length is not writable"));
            }
            vm.set_property_host(
              &this,
              &Value::String("length".to_string().into()),
              Value::Number(usize_to_f64(new_len)),
            )?;
            Ok(Value::Array(removed_arr, removed_props))
          }
        }),
        &function_proto_map,
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
            return Ok(Value::Undefined);
          }
          let result = vm.get_property_host(&this, &PKey::String("0".to_string().into()))?;
          let (keys, snapshot) = collect_indexed_snapshot(vm, &this, len)?;
          let source_keys: HashSet<usize> = keys.iter().copied().collect();
          let mut written_targets: HashSet<usize> = HashSet::new();

          for idx in keys.iter().copied() {
            if idx == 0 {
              continue;
            }
            if let Some(value) = snapshot.get(&idx).cloned() {
              vm.set_property_host(&this, &Value::String((idx - 1).to_string().into()), value)?;
              written_targets.insert(idx - 1);
            }
          }
          for idx in keys {
            if !source_keys.contains(&(idx + 1)) && !written_targets.contains(&idx) {
              let _ = vm.delete_property_host(&this, &PKey::String(idx.to_string().into()))?;
            }
          }
          if !array_length_is_writable(&this) {
            return Err(vm.throw_type_error("Array length is not writable"));
          }
          vm.set_property_host(
            &this,
            &Value::String("length".to_string().into()),
            Value::Number(usize_to_f64(len - 1)),
          )?;
          Ok(result)
        }),
        &function_proto_map,
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
          let mut keys: Vec<usize> = vm
            .own_property_keys_value(&this)?
            .into_iter()
            .filter_map(|key| match key {
              PropertyKey::String(s) => s.parse::<usize>().ok(),
              PropertyKey::Symbol(_) => None,
            })
            .filter(|idx| *idx < len)
            .collect();
          keys.sort_unstable_by(|a, b| b.cmp(a));
          for idx in keys {
            let from_key = PKey::String(idx.to_string().into());
            let to_key = Value::String((idx + insert_count).to_string().into());
            let from_present = vm.has_property_host(&this, &from_key)?;
            if from_present {
              let value = vm.get_property_host(&this, &from_key)?;
              vm.set_property_host(&this, &to_key, value)?;
            } else {
              let _ = vm.delete_property_host(
                &this,
                &PKey::String((idx + insert_count).to_string().into()),
              )?;
            }
          }
          for (idx, value) in insert_items.into_iter().enumerate() {
            vm.set_property_host(&this, &Value::String(idx.to_string().into()), value)?;
          }
          if !array_length_is_writable(&this) {
            return Err(vm.throw_type_error("Array length is not writable"));
          }
          vm.set_property_host(
            &this,
            &Value::String("length".to_string().into()),
            Value::Number(usize_to_f64(new_len)),
          )?;
          Ok(Value::Number(usize_to_f64(new_len)))
        }),
        &function_proto_map,
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
      &function_proto_map,
    )),
  );
  array_proto_map.borrow_mut().properties.insert(
    PKey::String("concat".to_string().into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new({
        let array_proto_map = Rc::clone(&array_proto_map);
        move |_vm, args| {
          let receiver = args.first().cloned().unwrap_or(Value::Undefined);
          let mut out: Vec<Option<Value>> = Vec::new();
          let mut append = |value: Value| -> Result<(), RuntimeError> {
            match value {
              Value::Array(arr, _) => {
                out.extend(arr.borrow().iter().cloned());
              }
              other => out.push(Some(other)),
            }
            Ok(())
          };
          append(receiver)?;
          for arg in args.iter().skip(1).cloned() {
            append(arg)?;
          }

          let arr_rc = Rc::new(RefCell::new(ArrayData::from(out)));
          let props: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(ObjectData::new()));
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
      &function_proto_map,
    )),
  );

  array_proto_map
}
