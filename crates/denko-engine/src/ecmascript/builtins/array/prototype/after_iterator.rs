// SPDX-License-Identifier: MPL-2.0
//! Array prototype methods installed after iterator methods.

use super::*;

pub(in crate::ecmascript::builtins::array) fn install_after_iterator(
  array_proto_map: &Rc<RefCell<ObjectData>>,
  function_proto_map: &Rc<RefCell<ObjectData>>,
) {
  use PropertyKey as PKey;

  array_proto_map.borrow_mut().properties.insert(
    PKey::String("filter".to_string().into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new(|vm, args| {
        let receiver = vm.to_object_value(args.first().cloned().unwrap_or(Value::Undefined))?;
        let callback = args.get(1).cloned().unwrap_or(Value::Undefined);
        let this_arg = args.get(2).cloned().unwrap_or(Value::Undefined);
        if !Vm::is_callable_value(&callback) {
          return Err(vm.throw_type_error("Array.prototype.filter callback is not callable"));
        }

        let len = array_like_length(vm, &receiver)?;
        let mut out: Vec<Option<Value>> = Vec::new();

        for i in 0..len {
          let key = PKey::String(i.to_string().into());
          let has_prop = vm.has_property_host(&receiver, &key)?;
          if has_prop {
            let item = vm.get_property_host(&receiver, &key)?;
            let keep = vm.call_value_with_this(
              &callback,
              &this_arg,
              &[
                item.clone(),
                Value::Number(usize_to_f64(i)),
                receiver.clone(),
              ],
            )?;
            if keep.is_truthy() {
              out.push(Some(item));
            }
          }
        }

        let result = array_species_create(vm, &receiver, 0)?;
        for (index, value) in out.into_iter().flatten().enumerate() {
          create_data_property_or_throw(
            vm,
            &result,
            &Value::String(index.to_string().into()),
            value,
          )?;
        }
        Ok(result)
      }),
      function_proto_map,
    )),
  );
  array_proto_map.borrow_mut().properties.insert(
    PKey::String("find".to_string().into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new(move |vm, args| {
        let receiver = vm.to_object_value(args.first().cloned().unwrap_or(Value::Undefined))?;
        let callback = args.get(1).cloned().unwrap_or(Value::Undefined);
        let this_arg = args.get(2).cloned().unwrap_or(Value::Undefined);
        if !Vm::is_callable_value(&callback) {
          return Err(vm.throw_type_error("Array.prototype.find callback is not callable"));
        }

        let len = array_like_length(vm, &receiver)?;

        for i in 0..len {
          let key = PKey::String(i.to_string().into());
          let item = vm.get_property_host(&receiver, &key)?;
          let found = vm.call_value_with_this(
            &callback,
            &this_arg,
            &[
              item.clone(),
              Value::Number(usize_to_f64(i)),
              receiver.clone(),
            ],
          )?;
          if found.is_truthy() {
            return Ok(item);
          }
        }
        Ok(Value::Undefined)
      }),
      function_proto_map,
    )),
  );
  array_proto_map.borrow_mut().properties.insert(
    PKey::String("at".to_string().into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new(move |vm, args| {
        let receiver = args.first().cloned().unwrap_or(Value::Undefined);
        let len = array_like_length(vm, &receiver)?;
        let index = vm.to_number_value(args.get(1).unwrap_or(&Value::Undefined))?;
        if index.is_nan() {
          return Ok(Value::Undefined);
        }
        let idx = if index < 0.0 {
          let offset = index.trunc() as isize;
          len as isize + offset
        } else {
          index.trunc() as isize
        };
        if idx < 0 || idx as usize >= len {
          return Ok(Value::Undefined);
        }
        let key = PKey::String((idx as usize).to_string().into());
        if vm.has_property_host(&receiver, &key)? {
          Ok(vm.get_property_host(&receiver, &key)?)
        } else {
          Ok(Value::Undefined)
        }
      }),
      function_proto_map,
    )),
  );
  let array_includes = builtin_object(
    Rc::new(move |vm, args| {
      let receiver = args.first().cloned().unwrap_or(Value::Undefined);
      let search = args.get(1).cloned().unwrap_or(Value::Undefined);
      let len = array_like_length(vm, &receiver)?;
      if len == 0 {
        return Ok(Value::Bool(false));
      }
      let from_index = args.get(2).cloned().unwrap_or(Value::Number(0.0));
      let from_index = vm.to_number_value(&from_index)?;
      let mut k = match from_index {
        n if n.is_nan() || n == f64::NEG_INFINITY => 0usize,
        n if n == f64::INFINITY => return Ok(Value::Bool(false)),
        n if n >= 0.0 => n.trunc().to_usize().unwrap_or(len),
        n => len.saturating_sub(n.abs().trunc().to_usize().unwrap_or(len)),
      };
      while k < len {
        let key = PKey::String(k.to_string().into());
        let element = vm.get_property_host(&receiver, &key)?;
        if same_value_zero(&element, &search) {
          return Ok(Value::Bool(true));
        }
        k += 1;
      }
      Ok(Value::Bool(false))
    }),
    function_proto_map,
  );
  set_builtin_name(&array_includes, "includes");
  set_builtin_length(&array_includes, 1.0);
  array_proto_map.borrow_mut().properties.insert(
    PKey::String("includes".to_string().into()),
    PropertyDescriptor::builtin_data(array_includes),
  );
  array_proto_map.borrow_mut().properties.insert(
    PKey::String("indexOf".to_string().into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new(move |vm, args| {
        let receiver = args.first().cloned().unwrap_or(Value::Undefined);
        let search = args.get(1).cloned().unwrap_or(Value::Undefined);
        let len = array_like_length(vm, &receiver)?;
        let from_index = args.get(2).cloned().unwrap_or(Value::Number(0.0));
        let mut k = match vm.to_number_value(&from_index)? {
          n if n.is_nan() => 0isize,
          n if n >= 0.0 => n.trunc() as isize,
          n => len as isize + n.trunc() as isize,
        };
        if k < 0 {
          k = 0;
        }
        while (k as usize) < len {
          let key = PKey::String((k as usize).to_string().into());
          if vm.has_property_host(&receiver, &key)? {
            let element = vm.get_property_host(&receiver, &key)?;
            if strict_equal(&element, &search) {
              return Ok(Value::Number(usize_to_f64(k as usize)));
            }
          }
          k += 1;
        }
        Ok(Value::Number(-1.0))
      }),
      function_proto_map,
    )),
  );
  array_proto_map.borrow_mut().properties.insert(
    PKey::String("slice".to_string().into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new({
        move |vm, args| {
          let receiver = args.first().cloned().unwrap_or(Value::Undefined);
          let len = array_like_length(vm, &receiver)?;
          let start_raw = vm.to_number_value(args.get(1).unwrap_or(&Value::Number(0.0)))?;
          let start = if start_raw.is_nan() {
            0usize
          } else if start_raw < 0.0 {
            let back = start_raw.trunc().abs().to_usize().unwrap_or(len);
            len.saturating_sub(back)
          } else {
            start_raw
              .trunc()
              .to_usize()
              .unwrap_or(len)
              .min(len)
          };
          let end = match args.get(2) {
            None | Some(Value::Undefined) => len,
            Some(end_arg) => {
              let end_raw = vm.to_number_value(end_arg)?;
              if end_raw.is_nan() {
                0usize
              } else if end_raw < 0.0 {
                let back = end_raw.trunc().abs().to_usize().unwrap_or(len);
                len.saturating_sub(back)
              } else {
                end_raw.trunc().to_usize().unwrap_or(len).min(len)
              }
            }
          };
          let end = end.max(start);
          let slice_len = end.saturating_sub(start);
          if slice_len > u32::MAX as usize {
            return Err(vm.throw_range_error("Invalid array length"));
          }
          let result = array_species_create(vm, &receiver, slice_len)?;
          for i in start..end {
            let key = PKey::String(i.to_string().into());
            let out_key = Value::String((i - start).to_string().into());
            if vm.has_property_host(&receiver, &key)? {
              let item = vm.get_property_host(&receiver, &key)?;
              let out_key = vm.to_property_key(&out_key)?;
              if !vm.create_data_property_host(&result, out_key, item)? {
                return Err(vm.throw_type_error("Array.prototype.slice could not create property"));
              }
            } else {
              delete_property_or_throw(vm, &result, &PKey::String((i - start).to_string().into()))?;
            }
          }
          vm.set_property_host(
            &result,
            &Value::String("length".to_string().into()),
            Value::Number(usize_to_f64(slice_len)),
          )?;
          Ok(result)
        }
      }),
      function_proto_map,
    )),
  );
  array_proto_map.borrow_mut().properties.insert(
    PKey::String("reduce".to_string().into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new(move |vm, args| {
        let receiver = vm.to_object_value(args.first().cloned().unwrap_or(Value::Undefined))?;
        let callback = args.get(1).cloned().unwrap_or(Value::Undefined);
        if !Vm::is_callable_value(&callback) {
          return Err(vm.throw_type_error("Array.prototype.reduce callback is not callable"));
        }

        let len = array_like_length(vm, &receiver)?;

        let mut i = 0;
        let mut accumulator = if args.len() > 2 {
          args[2].clone()
        } else {
          let mut first = None;
          while i < len {
            let key = PKey::String(i.to_string().into());
            if vm.has_property_host(&receiver, &key)? {
              first = Some(vm.get_property_host(&receiver, &key)?);
              i += 1;
              break;
            }
            i += 1;
          }
          match first {
            Some(value) => value,
            None => {
              return Err(vm.throw_type_error("Reduce of empty array with no initial value"));
            }
          }
        };

        while i < len {
          let key = PKey::String(i.to_string().into());
          let has_prop = vm.has_property_host(&receiver, &key)?;
          if has_prop {
            let item = vm.get_property_host(&receiver, &key)?;
            accumulator = vm.call_value_with_this(
              &callback,
              &Value::Undefined,
              &[
                accumulator,
                item,
                Value::Number(usize_to_f64(i)),
                receiver.clone(),
              ],
            )?;
          }
          i += 1;
        }
        Ok(accumulator)
      }),
      function_proto_map,
    )),
  );
  array_proto_map.borrow_mut().properties.insert(
    PKey::String("flat".to_string().into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new(|vm, args| {
        fn is_array(
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
              is_array(vm, &target)
            }
            _ => Ok(false),
          }
        }

        fn flatten_into(
          vm: &mut Vm,
          source: &Value,
          length: usize,
          depth: f64,
          output: &mut Vec<Option<Value>>,
        ) -> Result<(), RuntimeError> {
          for index in 0..length {
            let key = PKey::String(index.to_string().into());
            if !vm.has_property_host(source, &key)? {
              continue;
            }
            let element = vm.get_property_host(source, &key)?;
            if depth > 0.0 && is_array(vm, &element)? {
              let element_length = array_like_length(vm, &element)?;
              flatten_into(vm, &element, element_length, depth - 1.0, output)?;
            } else {
              if output.len() >= 9_007_199_254_740_991usize {
                return Err(vm.throw_type_error("Array.prototype.flat result is too large"));
              }
              output.push(Some(element));
            }
          }
          Ok(())
        }

        let receiver = args.first().cloned().unwrap_or(Value::Undefined);
        let depth = match args.get(1) {
          None | Some(Value::Undefined) => 1.0,
          Some(value) => {
            let number = vm.to_number_value(value)?;
            if number.is_nan() || number <= 0.0 {
              0.0
            } else {
              number.trunc()
            }
          }
        };

        let len = array_like_length(vm, &receiver)?;
        let mut out: Vec<Option<Value>> = Vec::new();
        flatten_into(vm, &receiver, len, depth, &mut out)?;

        let result = array_species_create(vm, &receiver, 0)?;
        for (index, value) in out.into_iter().flatten().enumerate() {
          create_data_property_or_throw(
            vm,
            &result,
            &Value::String(index.to_string().into()),
            value,
          )?;
        }
        Ok(result)
      }),
      function_proto_map,
    )),
  );
  array_proto_map.borrow_mut().properties.insert(
    PKey::String("copyWithin".to_string().into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new(move |vm, args| {
        let receiver = args.first().cloned().unwrap_or(Value::Undefined);
        let len = array_like_length(vm, &receiver)?;
        let to = clamp_relative_index(
          to_integer_or_infinity(vm, args.get(1).unwrap_or(&Value::Undefined))?,
          len,
        );
        let from = clamp_relative_index(
          to_integer_or_infinity(vm, args.get(2).unwrap_or(&Value::Undefined))?,
          len,
        );
        let final_ = if let Some(end_arg) = args.get(3) {
          clamp_relative_index(to_integer_or_infinity(vm, end_arg)?, len)
        } else {
          len
        };
        let count = final_
          .saturating_sub(from)
          .min(len.saturating_sub(to));
        let mut snapshot: Vec<Option<Value>> = Vec::with_capacity(count);
        for i in 0..count {
          let key = PKey::String((from + i).to_string().into());
          if vm.has_property_host(&receiver, &key)? {
            snapshot.push(Some(vm.get_property_host(&receiver, &key)?));
          } else {
            snapshot.push(None);
          }
        }
        for (i, slot) in snapshot.into_iter().enumerate() {
          let key = PKey::String((to + i).to_string().into());
          let key_value = Value::String((to + i).to_string().into());
          if let Some(value) = slot {
            vm.set_property_host(&receiver, &key_value, value)?;
          } else {
            delete_property_or_throw(vm, &receiver, &key)?;
          }
        }
        Ok(receiver)
      }),
      function_proto_map,
    )),
  );
  array_proto_map.borrow_mut().properties.insert(
    PKey::String("every".to_string().into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new(move |vm, args| {
        let receiver = vm.to_object_value(args.first().cloned().unwrap_or(Value::Undefined))?;
        let len = array_like_length(vm, &receiver)?;
        let callback = args.get(1).cloned().unwrap_or(Value::Undefined);
        let this_arg = args.get(2).cloned().unwrap_or(Value::Undefined);
        if !Vm::is_callable_value(&callback) {
          return Err(vm.throw_type_error("Array.prototype.every callback is not callable"));
        }
        for i in 0..len {
          let key = PKey::String(i.to_string().into());
          if vm.has_property_host(&receiver, &key)? {
            let item = vm.get_property_host(&receiver, &key)?;
            let keep = vm.call_value_with_this(
              &callback,
              &this_arg,
              &[item, Value::Number(usize_to_f64(i)), receiver.clone()],
            )?;
            if !keep.is_truthy() {
              return Ok(Value::Bool(false));
            }
          }
        }
        Ok(Value::Bool(true))
      }),
      function_proto_map,
    )),
  );
  array_proto_map.borrow_mut().properties.insert(
    PKey::String("fill".to_string().into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new(move |vm, args| {
        let receiver = args.first().cloned().unwrap_or(Value::Undefined);
        let fill_value = args.get(1).cloned().unwrap_or(Value::Undefined);
        let len = array_like_length(vm, &receiver)?;
        let start = clamp_relative_index(
          to_integer_or_infinity(vm, args.get(2).unwrap_or(&Value::Undefined))?,
          len,
        );
        let end = if let Some(end_arg) = args.get(3) {
          clamp_relative_index(to_integer_or_infinity(vm, end_arg)?, len)
        } else {
          len
        };
        for i in start..end {
          let key = Value::String(i.to_string().into());
          vm.set_property_host(&receiver, &key, fill_value.clone())?;
        }
        Ok(receiver)
      }),
      function_proto_map,
    )),
  );
  array_proto_map.borrow_mut().properties.insert(
    PKey::String("findIndex".to_string().into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new(move |vm, args| {
        let receiver = vm.to_object_value(args.first().cloned().unwrap_or(Value::Undefined))?;
        let callback = args.get(1).cloned().unwrap_or(Value::Undefined);
        let this_arg = args.get(2).cloned().unwrap_or(Value::Undefined);
        if !Vm::is_callable_value(&callback) {
          return Err(vm.throw_type_error("Array.prototype.findIndex callback is not callable"));
        }
        let len = array_like_length(vm, &receiver)?;
        for i in 0..len {
          let key = PKey::String(i.to_string().into());
          let item = vm.get_property_host(&receiver, &key)?;
          let found = vm.call_value_with_this(
            &callback,
            &this_arg,
            &[
              item.clone(),
              Value::Number(usize_to_f64(i)),
              receiver.clone(),
            ],
          )?;
          if found.is_truthy() {
            return Ok(Value::Number(usize_to_f64(i)));
          }
        }
        Ok(Value::Number(-1.0))
      }),
      function_proto_map,
    )),
  );
  array_proto_map.borrow_mut().properties.insert(
    PKey::String("findLast".to_string().into()),
    PropertyDescriptor::builtin_data({
      let find_last_fn = builtin_object(
        Rc::new(move |vm, args| {
          let this = args.first().cloned().unwrap_or(Value::Undefined);
          let receiver = vm.to_object_value(this)?;
          let callback = args.get(1).cloned().unwrap_or(Value::Undefined);
          let len = array_like_length(vm, &receiver)?;
          if !Vm::is_callable_value(&callback) {
            return Err(vm.throw_type_error("Array.prototype.findLast callback is not callable"));
          }
          let this_arg = args.get(2).cloned().unwrap_or(Value::Undefined);
          let mut i = len;
          while i > 0 {
            i -= 1;
            let key = PKey::String(i.to_string().into());
            let item = vm.get_property_host(&receiver, &key)?;
            let found = vm.call_value_with_this(
              &callback,
              &this_arg,
              &[
                item.clone(),
                Value::Number(usize_to_f64(i)),
                receiver.clone(),
              ],
            )?;
            if found.is_truthy() {
              return Ok(item);
            }
          }
          Ok(Value::Undefined)
        }),
        function_proto_map,
      );
      set_builtin_name(&find_last_fn, "findLast");
      set_builtin_length(&find_last_fn, 1.0);
      find_last_fn
    }),
  );
  array_proto_map.borrow_mut().properties.insert(
    PKey::String("findLastIndex".to_string().into()),
    PropertyDescriptor::builtin_data({
      let find_last_index_fn = builtin_object(
        Rc::new(move |vm, args| {
          let this = args.first().cloned().unwrap_or(Value::Undefined);
          let receiver = vm.to_object_value(this)?;
          let callback = args.get(1).cloned().unwrap_or(Value::Undefined);
          let len = array_like_length(vm, &receiver)?;
          if !Vm::is_callable_value(&callback) {
            return Err(
              vm.throw_type_error("Array.prototype.findLastIndex callback is not callable"),
            );
          }
          let this_arg = args.get(2).cloned().unwrap_or(Value::Undefined);
          let mut i = len;
          while i > 0 {
            i -= 1;
            let key = PKey::String(i.to_string().into());
            let item = vm.get_property_host(&receiver, &key)?;
            let found = vm.call_value_with_this(
              &callback,
              &this_arg,
              &[item, Value::Number(usize_to_f64(i)), receiver.clone()],
            )?;
            if found.is_truthy() {
              return Ok(Value::Number(usize_to_f64(i)));
            }
          }
          Ok(Value::Number(-1.0))
        }),
        function_proto_map,
      );
      set_builtin_name(&find_last_index_fn, "findLastIndex");
      set_builtin_length(&find_last_index_fn, 1.0);
      find_last_index_fn
    }),
  );
  array_proto_map.borrow_mut().properties.insert(
    PKey::String("flatMap".to_string().into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new(|vm, args| {
        fn is_array(
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
              is_array(vm, &target)
            }
            _ => Ok(false),
          }
        }

        let receiver = args.first().cloned().unwrap_or(Value::Undefined);
        let callback = args.get(1).cloned().unwrap_or(Value::Undefined);
        if !callback.is_callable() {
          return Err(vm.throw_type_error("Array.prototype.flatMap callback is not callable"));
        }
        let this_arg = args.get(2).cloned().unwrap_or(Value::Undefined);
        let len = array_like_length(vm, &receiver)?;
        let mut out: Vec<Option<Value>> = Vec::new();
        for i in 0..len {
          let key = PKey::String(i.to_string().into());
          if vm.has_property_host(&receiver, &key)? {
            let item = vm.get_property_host(&receiver, &key)?;
            let mapped = vm.call_value_with_this(
              &callback,
              &this_arg,
              &[item, Value::Number(usize_to_f64(i)), receiver.clone()],
            )?;
            if is_array(vm, &mapped)? {
              let mapped_len = array_like_length(vm, &mapped)?;
              for mapped_index in 0..mapped_len {
                let mapped_key = PKey::String(mapped_index.to_string().into());
                if vm.has_property_host(&mapped, &mapped_key)? {
                  out.push(Some(vm.get_property_host(&mapped, &mapped_key)?));
                }
              }
            } else {
              out.push(Some(mapped));
            }
          }
        }
        let result = array_species_create(vm, &receiver, 0)?;
        for (index, value) in out.into_iter().flatten().enumerate() {
          create_data_property_or_throw(
            vm,
            &result,
            &Value::String(index.to_string().into()),
            value,
          )?;
        }
        Ok(result)
      }),
      function_proto_map,
    )),
  );
  array_proto_map.borrow_mut().properties.insert(
    PKey::String("pop".to_string().into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new(move |vm, args| {
        let receiver = args.first().cloned().unwrap_or(Value::Undefined);
        let len = array_like_length(vm, &receiver)?;
        if len == 0 {
          vm.set_property_host(
            &receiver,
            &Value::String("length".into()),
            Value::Number(0.0),
          )?;
          return Ok(Value::Undefined);
        }
        let index = len - 1;
        let key = PKey::String(index.to_string().into());
        let result = if vm.has_property_host(&receiver, &key)? {
          vm.get_property_host(&receiver, &key)?
        } else {
          Value::Undefined
        };
        delete_property_or_throw(vm, &receiver, &key)?;
        vm.set_property_host(
          &receiver,
          &Value::String("length".to_string().into()),
          Value::Number(usize_to_f64(index)),
        )?;
        Ok(result)
      }),
      function_proto_map,
    )),
  );
  array_proto_map.borrow_mut().properties.insert(
    PKey::String("reduceRight".to_string().into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new(move |vm, args| {
        let receiver = vm.to_object_value(args.first().cloned().unwrap_or(Value::Undefined))?;
        let callback = args.get(1).cloned().unwrap_or(Value::Undefined);
        if !Vm::is_callable_value(&callback) {
          return Err(vm.throw_type_error("Array.prototype.reduceRight callback is not callable"));
        }
        let len = array_like_length(vm, &receiver)?;
        let mut i = len;
        let mut accumulator = if args.len() > 2 {
          args[2].clone()
        } else {
          let mut found = None;
          while i > 0 {
            i -= 1;
            let key = PKey::String(i.to_string().into());
            if vm.has_property_host(&receiver, &key)? {
              found = Some(vm.get_property_host(&receiver, &key)?);
              break;
            }
          }
          match found {
            Some(value) => value,
            None => {
              return Err(vm.throw_type_error("Reduce of empty array with no initial value"));
            }
          }
        };

        while i > 0 {
          i -= 1;
          let key = PKey::String(i.to_string().into());
          if vm.has_property_host(&receiver, &key)? {
            let item = vm.get_property_host(&receiver, &key)?;
            accumulator = vm.call_value_with_this(
              &callback,
              &Value::Undefined,
              &[
                accumulator,
                item,
                Value::Number(usize_to_f64(i)),
                receiver.clone(),
              ],
            )?;
          }
        }
        Ok(accumulator)
      }),
      function_proto_map,
    )),
  );
  array_proto_map.borrow_mut().properties.insert(
    PKey::String("reverse".to_string().into()),
    PropertyDescriptor::builtin_data({
      let reverse_fn = builtin_object(
        Rc::new(move |vm, args| {
          let this = args.first().cloned().unwrap_or(Value::Undefined);
          let receiver = vm.to_object_value(this)?;
          let len = array_like_length(vm, &receiver)?;
          let half = len / 2;
          for lower in 0..half {
            let upper = len - 1 - lower;
            let lower_key = PKey::String(lower.to_string().into());
            let upper_key = PKey::String(upper.to_string().into());
            let lower_key_value = Value::String(lower.to_string().into());
            let upper_key_value = Value::String(upper.to_string().into());
            let lower_exists = vm.has_property_host(&receiver, &lower_key)?;
            let lower_value = if lower_exists {
              Some(vm.get_property_host(&receiver, &lower_key)?)
            } else {
              None
            };
            let upper_exists = vm.has_property_host(&receiver, &upper_key)?;
            let upper_value = if upper_exists {
              Some(vm.get_property_host(&receiver, &upper_key)?)
            } else {
              None
            };
            match (lower_value, upper_value) {
              (Some(low), Some(high)) => {
                vm.set_property_host(&receiver, &lower_key_value, high)?;
                vm.set_property_host(&receiver, &upper_key_value, low)?;
              }
              (Some(low), None) => {
                delete_property_or_throw(vm, &receiver, &lower_key)?;
                vm.set_property_host(&receiver, &upper_key_value, low)?;
              }
              (None, Some(high)) => {
                delete_property_or_throw(vm, &receiver, &upper_key)?;
                vm.set_property_host(&receiver, &lower_key_value, high)?;
              }
              (None, None) => {}
            }
          }
          Ok(receiver)
        }),
        function_proto_map,
      );
      set_builtin_name(&reverse_fn, "reverse");
      set_builtin_length(&reverse_fn, 0.0);
      reverse_fn
    }),
  );
  array_proto_map.borrow_mut().properties.insert(
    PKey::String("some".to_string().into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new(move |vm, args| {
        let receiver = vm.to_object_value(args.first().cloned().unwrap_or(Value::Undefined))?;
        let len = array_like_length(vm, &receiver)?;
        let callback = args.get(1).cloned().unwrap_or(Value::Undefined);
        let this_arg = args.get(2).cloned().unwrap_or(Value::Undefined);
        if !Vm::is_callable_value(&callback) {
          return Err(vm.throw_type_error("Array.prototype.some callback is not callable"));
        }
        for i in 0..len {
          let key = PKey::String(i.to_string().into());
          if vm.has_property_host(&receiver, &key)? {
            let item = vm.get_property_host(&receiver, &key)?;
            let keep = vm.call_value_with_this(
              &callback,
              &this_arg,
              &[item, Value::Number(usize_to_f64(i)), receiver.clone()],
            )?;
            if keep.is_truthy() {
              return Ok(Value::Bool(true));
            }
          }
        }
        Ok(Value::Bool(false))
      }),
      function_proto_map,
    )),
  );
  array_proto_map.borrow_mut().properties.insert(
    PKey::String("toLocaleString".to_string().into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new(move |vm, args| {
        let receiver = args.first().cloned().unwrap_or(Value::Undefined);
        let len = array_like_length(vm, &receiver)?;
        let locale_args = args.get(1..).unwrap_or(&[]);
        let mut out = js_string::JsStringBuilder::new();
        for i in 0..len {
          if i > 0 {
            out.push_code_unit(u16::from(b','));
          }
          let key = PKey::String(i.to_string().into());
          if !vm.has_property_host(&receiver, &key)? {
            continue;
          }
          let item = vm.get_property_host(&receiver, &key)?;
          match item {
            Value::Undefined | Value::Null => {}
            _ => {
              let to_locale =
                vm.get_property_host(&item, &PKey::String("toLocaleString".to_string().into()))?;
              if Vm::is_callable_value(&to_locale) {
                let rendered = vm.call_value_with_this(&to_locale, &item, locale_args)?;
                out.push_string(&vm.to_string(&rendered)?);
              } else {
                out.push_string(&vm.to_string(&item)?);
              }
            }
          }
        }
        Ok(Value::String(out.finish()))
      }),
      function_proto_map,
    )),
  );

  for (name, length) in [
    ("filter", 1.0),
    ("find", 1.0),
    ("at", 1.0),
    ("includes", 1.0),
    ("indexOf", 1.0),
    ("slice", 2.0),
    ("reduce", 1.0),
    ("copyWithin", 2.0),
    ("every", 1.0),
    ("fill", 1.0),
    ("findIndex", 1.0),
    ("findLast", 1.0),
    ("findLastIndex", 1.0),
    ("flat", 1.0),
    ("flatMap", 1.0),
    ("pop", 0.0),
    ("reduceRight", 1.0),
    ("reverse", 0.0),
    ("some", 1.0),
    ("toLocaleString", 0.0),
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

  let mut unscopables = ObjectData::with_prototype(Value::Null);
  for name in [
    "at",
    "copyWithin",
    "entries",
    "fill",
    "find",
    "findIndex",
    "findLast",
    "findLastIndex",
    "flat",
    "flatMap",
    "includes",
    "keys",
    "toReversed",
    "toSorted",
    "toSpliced",
    "values",
  ] {
    unscopables.properties.insert(
      PKey::String(name.into()),
      PropertyDescriptor::ordinary_data(Value::Bool(true)),
    );
  }
  array_proto_map.borrow_mut().properties.insert(
    PKey::Symbol(SymbolId::well_known_unscopables()),
    PropertyDescriptor::Data {
      value: Value::Object(Rc::new(RefCell::new(unscopables))),
      writable: false,
      enumerable: false,
      configurable: true,
    },
  );
}
