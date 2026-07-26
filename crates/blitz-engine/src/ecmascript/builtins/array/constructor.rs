// SPDX-License-Identifier: MPL-2.0
//! Array constructor and static methods.

use super::*;

pub(super) fn install(
  array_proto_map: &Rc<RefCell<ObjectData>>,
  function_proto_map: &Rc<RefCell<ObjectData>>,
) -> Value {
  use PropertyKey as PKey;

  let array_ctor_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(ObjectData::with_prototype(
    Value::Object(Rc::clone(function_proto_map)),
  )));
  array_ctor_map.borrow_mut().properties.insert(
    PKey::String("prototype".to_string().into()),
    PropertyDescriptor::builtin_prototype(Value::Object(Rc::clone(array_proto_map))),
  );

  let array_ctor_props = Rc::clone(&array_ctor_map);
  let array_ctor_fn: BuiltinFn = Rc::new(move |vm, args| {
    let off = static_builtin_arg_offset(vm, args, "Array");
    let tail = &args[off..];
    let (arr_rc, length) = if tail.len() == 1 {
      match tail[0] {
        Value::Number(n)
          if n.is_finite() && n >= 0.0 && n.fract() == 0.0 && n <= 4_294_967_295.0 =>
        {
          (
            Rc::new(RefCell::new(ArrayData::from(Vec::<Option<Value>>::new()))),
            n as usize,
          )
        }
        Value::Number(_) => {
          return Err(vm.throw_range_error("Invalid array length"));
        }
        _ => {
          let elems: Vec<Option<Value>> = tail.iter().map(|v| Some(v.clone())).collect();
          let len = elems.len();
          (Rc::new(RefCell::new(ArrayData::from(elems))), len)
        }
      }
    } else {
      let elems: Vec<Option<Value>> = tail.iter().map(|v| Some(v.clone())).collect();
      let len = elems.len();
      (Rc::new(RefCell::new(ArrayData::from(elems))), len)
    };
    let props: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(ObjectData::new()));
    props.borrow_mut().properties.insert(
      PKey::String("length".to_string().into()),
      array_length_descriptor(length),
    );
    if let Some(pval) = array_ctor_props
      .borrow()
      .properties
      .get(&PKey::String("prototype".to_string().into()))
      .and_then(|d| match d {
        PropertyDescriptor::Data { value, .. } => Some(value.clone()),
        _ => None,
      })
    {
      props.borrow_mut().set_prototype_value(pval);
    }
    Ok(Value::Array(arr_rc, props))
  });
  let array_ctor_val = Value::BuiltinObject(Rc::clone(&array_ctor_fn), Rc::clone(&array_ctor_map));
  set_builtin_name(&array_ctor_val, "Array");
  set_builtin_length(&array_ctor_val, 1.0);
  array_ctor_map.borrow_mut().properties.insert(
    PKey::String("isArray".to_string().into()),
    PropertyDescriptor::builtin_data({
      let builtin = builtin_object(
        Rc::new(|_vm, args| {
          Ok(Value::Bool(
            matches!(args.get(1), Some(v) if is_array_value(v)),
          ))
        }),
        function_proto_map,
      );
      set_builtin_length(&builtin, 1.0);
      builtin
    }),
  );
  array_ctor_map.borrow_mut().properties.insert(
    PKey::String("of".to_string().into()),
    PropertyDescriptor::builtin_data({
      let array_proto_for_of = Rc::clone(array_proto_map);
      let function_proto_map = Rc::clone(function_proto_map);
      let builtin = builtin_object(
        Rc::new(move |vm, args| {
          let off = static_builtin_arg_offset(vm, args, "Array");
          let elems: Vec<Option<Value>> = args.iter().skip(off).cloned().map(Some).collect();
          let arr_rc = Rc::new(RefCell::new(ArrayData::from(elems)));
          let props = Rc::new(RefCell::new(ObjectData::new()));
          props
            .borrow_mut()
            .set_prototype_value(Value::Object(Rc::clone(&array_proto_for_of)));
          props.borrow_mut().properties.insert(
            PKey::String("length".to_string().into()),
            array_length_descriptor(arr_rc.borrow().len()),
          );
          Ok(Value::Array(arr_rc, props))
        }),
        &function_proto_map,
      );
      set_builtin_length(&builtin, 0.0);
      builtin
    }),
  );
  array_ctor_map.borrow_mut().properties.insert(
    PKey::String("from".to_string().into()),
    PropertyDescriptor::builtin_data({
      let function_proto_map = Rc::clone(function_proto_map);
      let builtin = builtin_object(
        Rc::new(move |vm, args| {
          let off = static_builtin_arg_offset(vm, args, "Array");
          let source = args.get(off).cloned().unwrap_or(Value::Undefined);
          let map_fn = args
            .get(off + 1)
            .cloned()
            .unwrap_or(Value::Undefined);
          let this_arg = args
            .get(off + 2)
            .cloned()
            .unwrap_or(Value::Undefined);
          let constructor = vm.this_value();

          let source = vm.to_object_value(source)?;
          if !matches!(map_fn, Value::Undefined) && !Vm::is_callable_value(&map_fn) {
            return Err(vm.throw_type_error("Array.from mapfn is not callable"));
          }

          let iterator_method = crate::runtime_utils::get_method(
            vm,
            &source,
            &PKey::Symbol(SymbolId::well_known_iterator()),
          )?;
          if let Some(iterator_method) = iterator_method {
            let result = if constructor.is_constructable() {
              vm.construct_value(&constructor, &[])?
            } else {
              vm.new_array(Vec::new())
            };
            let (iterator, next) =
              match crate::runtime_utils::get_iterator_record(vm, &source, &iterator_method) {
                Ok(record) => record,
                Err(error) => return Err(error),
              };
            let mut index = 0usize;
            loop {
              let value = match crate::runtime_utils::iterator_step(vm, &iterator, &next) {
                Ok(Some(value)) => value,
                Ok(None) => break,
                Err(error) => {
                  // IteratorClose must not replace the original abrupt
                  // completion with an error from the close operation.
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
                  &[value, Value::Number(usize_to_f64(index))],
                ) {
                  Ok(value) => value,
                  Err(error) => {
                    drop(crate::runtime_utils::iterator_close(vm, &iterator));
                    return Err(error);
                  }
                }
              };
              let key = Value::String(index.to_string().into());
              if let Err(error) = create_data_property_or_throw(vm, &result, &key, value) {
                drop(crate::runtime_utils::iterator_close(vm, &iterator));
                return Err(error);
              }
              index = match index.checked_add(1) {
                Some(next_index) => next_index,
                None => {
                  let error = vm.throw_type_error("Array.from result is too large");
                  drop(crate::runtime_utils::iterator_close(vm, &iterator));
                  return Err(error);
                }
              };
            }
            vm.set_property_host(
              &result,
              &Value::String("length".to_string().into()),
              Value::Number(usize_to_f64(index)),
            )?;
            return Ok(result);
          }

          let length_value =
            vm.get_property_host(&source, &PKey::String("length".to_string().into()))?;
          let length = to_length(vm, &length_value)?;
          let result = if constructor.is_constructable() {
            vm.construct_value(&constructor, &[Value::Number(usize_to_f64(length))])?
          } else {
            vm.new_array(vec![None; length])
          };

          for index in 0..length {
            let key = PKey::String(index.to_string().into());
            let value = vm.get_property_host(&source, &key)?;
            let value = if matches!(map_fn, Value::Undefined) {
              value
            } else {
              vm.call_value_with_this(
                &map_fn,
                &this_arg,
                &[value, Value::Number(usize_to_f64(index))],
              )?
            };
            create_data_property_or_throw(
              vm,
              &result,
              &Value::String(index.to_string().into()),
              value,
            )?;
          }
          vm.set_property_host(
            &result,
            &Value::String("length".to_string().into()),
            Value::Number(usize_to_f64(length)),
          )?;
          Ok(result)
        }),
        &function_proto_map,
      );
      set_builtin_name(&builtin, "from");
      set_builtin_length(&builtin, 1.0);
      builtin
    }),
  );
  array_ctor_map.borrow_mut().properties.insert(
    PKey::String("fromAsync".to_string().into()),
    PropertyDescriptor::builtin_data({
      let array_proto_for_from_async = Rc::clone(array_proto_map);
      let function_proto_map = Rc::clone(function_proto_map);
      let builtin = builtin_object(
        Rc::new(move |vm, args| {
          let off = static_builtin_arg_offset(vm, args, "Array");
          let undefined = Value::Undefined;
          let source = args.get(off).cloned().unwrap_or(undefined);
          let map_fn = args
            .get(off + 1)
            .cloned()
            .unwrap_or(Value::Undefined);
          let this_arg = args
            .get(off + 2)
            .cloned()
            .unwrap_or(Value::Undefined);
          let result_promise =
            crate::gc::track_promise(Rc::new(RefCell::new(PromiseState::Pending {
              on_fulfilled: Vec::new(),
              on_rejected: Vec::new(),
            })));
          let constructor_candidate = vm.this_value();
          let use_constructor = matches!(
              constructor_candidate,
              Value::Closure { ref proto, .. } if proto.is_constructor
          );
          let source_for_job = source;
          let map_fn_for_job = map_fn;
          let this_arg_for_job = this_arg;
          let constructor_for_job = constructor_candidate;
          let array_proto_for_job = Rc::clone(&array_proto_for_from_async);
          let result_promise_for_job = Rc::clone(&result_promise);
          vm.queue_microtask(move |vm| {
            let outcome = execute_array_from_async_job(
              vm,
              &source_for_job,
              &map_fn_for_job,
              &this_arg_for_job,
              &constructor_for_job,
              use_constructor,
              &array_proto_for_job,
            );
            match outcome {
              Ok(value) => {
                resolve_promise_with_value(vm, &result_promise_for_job, value)?;
              }
              Err(err) => {
                let reason = vm
                  .exceptions
                  .last_uncaught
                  .take()
                  .unwrap_or_else(|| Value::String(format!("{}", err).into()));
                reject_pending_promise(vm, &result_promise_for_job, reason);
              }
            }
            Ok(())
          });
          Ok(Value::Promise(result_promise))
        }),
        &function_proto_map,
      );
      set_builtin_length(&builtin, 1.0);
      builtin
    }),
  );
  array_proto_map.borrow_mut().properties.insert(
    PKey::String("constructor".to_string().into()),
    PropertyDescriptor::builtin_data(array_ctor_val.clone()),
  );
  array_proto_map.borrow_mut().properties.insert(
    PKey::String("lastIndexOf".to_string().into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new(|vm, args| {
        let undefined = Value::Undefined;
        let receiver = args.first().unwrap_or(&undefined);
        let search = args.get(1).unwrap_or(&undefined);
        let length = vm.get_property_host(receiver, &PKey::String("length".to_string().into()))?;
        let len_num = vm.to_number_value(&length)?;
        if !len_num.is_finite() || len_num <= 0.0 {
          return Ok(Value::Number(-1.0));
        }
        let len = len_num.floor().to_i64().unwrap_or(i64::MAX);
        let mut k = if let Some(from_index) = args.get(2) {
          let n = vm.to_number_value(from_index)?;
          if n.is_nan() {
            len - 1
          } else if n >= 0.0 {
            n.floor().to_i64().unwrap_or(len - 1).min(len - 1)
          } else {
            let idx = len + n.floor().to_i64().unwrap_or(0);
            if idx < 0 {
              return Ok(Value::Number(-1.0));
            }
            idx
          }
        } else {
          len - 1
        };

        if let Value::Array(arr, _) = receiver {
          let dense_len = arr.borrow().len().to_i64().unwrap_or(i64::MAX);
          let mut idx = k.min(dense_len.saturating_sub(1));
          while idx >= 0 {
            if let Some(Some(element)) = arr.borrow().get(idx as usize)
              && strict_equal(element, search)
            {
              return Ok(Value::Number(idx.to_f64().unwrap_or(f64::NAN)));
            }
            idx -= 1;
          }
        }

        if len > 1_000_000 {
          let mut candidates: Vec<u64> = Vec::new();
          match &receiver {
            Value::Object(obj) => {
              let k_u64 = k.to_u64().unwrap_or(u64::MAX);
              for key in obj.borrow().properties.keys() {
                if let PKey::String(s) = key
                  && let Some(idx) = canonical_index_key(s)
                  && idx <= k_u64
                {
                  candidates.push(idx);
                }
              }
            }
            Value::Array(arr, props) => {
              let k_u64 = k.to_u64().unwrap_or(u64::MAX);
              let dense_len = arr.borrow().len().to_u64().unwrap_or(u64::MAX);
              for idx in 0..dense_len.min(k_u64.saturating_add(1)) {
                candidates.push(idx);
              }
              for key in props.borrow().properties.keys() {
                if let PKey::String(s) = key
                  && let Some(idx) = canonical_index_key(s)
                  && idx <= k_u64
                {
                  candidates.push(idx);
                }
              }
            }
            Value::Closure { props, .. } | Value::BuiltinObject(_, props) => {
              let k_u64 = k.to_u64().unwrap_or(u64::MAX);
              for key in props.borrow().properties.keys() {
                if let PKey::String(s) = key
                  && let Some(idx) = canonical_index_key(s)
                  && idx <= k_u64
                {
                  candidates.push(idx);
                }
              }
            }
            _ => {}
          }

          candidates.sort_unstable();
          candidates.dedup();
          for idx in candidates.into_iter().rev() {
            let key = PKey::String(u64_to_decimal_string(idx).into());
            let element = vm.get_property_host(receiver, &key)?;
            if strict_equal(&element, search) {
              return Ok(Value::Number(idx.to_f64().unwrap_or(f64::NAN)));
            }
          }
          return Ok(Value::Number(-1.0));
        }

        while k >= 0 {
          let key = PKey::String(u64_to_decimal_string(k as u64).into());
          if vm.has_property_host(receiver, &key)? {
            let element = vm.get_property_host(receiver, &key)?;
            if env::var("TSBLITZ_TRACE").ok().as_deref() == Some("1") {
              eprintln!(
                "[trace] lastIndexOf k={} element={} search={}",
                k, element, search
              );
            }
            if strict_equal(&element, search) {
              return Ok(Value::Number(k.to_f64().unwrap_or(f64::NAN)));
            }
          }
          k -= 1;
        }
        Ok(Value::Number(-1.0))
      }),
      function_proto_map,
    )),
  );
  array_proto_map.borrow_mut().properties.insert(
    PKey::String("join".to_string().into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new(|vm, args| {
        let undefined = Value::Undefined;
        let receiver = args.first().unwrap_or(&undefined);
        let sep = match args.get(1) {
          Some(Value::Undefined) | None => JsString::from(","),
          Some(v) => vm.to_string(v)?,
        };
        let Value::Array(arr, _) = receiver else {
          return Ok(Value::String(String::new().into()));
        };
        let mut out = JsStringBuilder::new();
        for (idx, item) in arr.borrow().iter().enumerate() {
          if idx > 0 {
            out.push_string(&sep);
          }
          match item {
            Some(Value::Undefined) | Some(Value::Null) | None => {}
            Some(v) => out.push_string(&vm.to_string(v)?),
          }
        }
        Ok(Value::String(out.finish()))
      }),
      function_proto_map,
    )),
  );

  array_ctor_val
}

fn is_iterable_source(
  vm: &mut Vm,
  source: &Value,
) -> Result<bool, crate::errors::RuntimeError> {
  match source {
    Value::Array(..) => Ok(true),
    other => {
      let iterator_method =
        vm.get_property_host(other, &PropertyKey::Symbol(SymbolId::well_known_iterator()))?;
      Ok(Vm::is_callable_value(&iterator_method))
    }
  }
}

fn close_iterator(
  vm: &mut Vm,
  iter_obj: &Value,
) -> Result<(), crate::errors::RuntimeError> {
  let return_fn =
    vm.get_property_host(iter_obj, &PropertyKey::String("return".to_string().into()))?;
  if Vm::is_callable_value(&return_fn) {
    drop(vm.call_value_with_this(&return_fn, iter_obj, &[])?);
  }
  Ok(())
}

fn execute_array_from_async_job(
  vm: &mut Vm,
  source: &Value,
  map_fn: &Value,
  this_arg: &Value,
  constructor_candidate: &Value,
  use_constructor: bool,
  array_proto_for_from_async: &Rc<RefCell<ObjectData>>,
) -> Result<Value, crate::errors::RuntimeError> {
  if !matches!(map_fn, Value::Undefined) && !Vm::is_callable_value(map_fn) {
    return Err(vm.throw_type_error("Array.fromAsync mapfn is not callable"));
  }

  let created = if use_constructor {
    let ctor_len = if is_iterable_source(vm, source)? {
      Vec::new()
    } else {
      let items = collect_iterable_values(vm, source)?;
      vec![Value::Number(usize_to_f64(items.len()))]
    };
    vm.construct_value(constructor_candidate, &ctor_len)?
  } else {
    let len = if is_iterable_source(vm, source)? {
      0
    } else {
      collect_iterable_values(vm, source)?.len()
    };
    let out: Vec<Option<Value>> = vec![None; len];
    let props = Rc::new(RefCell::new(ObjectData::new()));
    props
      .borrow_mut()
      .set_prototype_value(Value::Object(Rc::clone(array_proto_for_from_async)));
    props.borrow_mut().properties.insert(
      PropertyKey::String("length".to_string().into()),
      array_length_descriptor(len),
    );
    Value::Array(Rc::new(RefCell::new(ArrayData::from(out))), props)
  };

  if is_iterable_source(vm, source)? {
    let iterator_method = vm.get_property_host(
      source,
      &PropertyKey::Symbol(SymbolId::well_known_iterator()),
    )?;
    if !Vm::is_callable_value(&iterator_method) {
      return Err(vm.throw_type_error("Array.fromAsync source is not iterable"));
    }

    let iter_obj = vm.call_value_with_this(&iterator_method, source, &[])?;
    let mut index = 0usize;
    let iter_result: Result<(), crate::errors::RuntimeError> = (|| {
      loop {
        let next_fn =
          vm.get_property_host(&iter_obj, &PropertyKey::String("next".to_string().into()))?;
        let result = vm.call_value_with_this(&next_fn, &iter_obj, &[])?;
        let done_val =
          vm.get_property_host(&result, &PropertyKey::String("done".to_string().into()))?;
        if vm.to_boolean(&done_val) {
          break;
        }
        let mut value =
          vm.get_property_host(&result, &PropertyKey::String("value".to_string().into()))?;
        if !matches!(map_fn, Value::Undefined) {
          let mapped = vm.call_value_with_this(
            map_fn,
            this_arg,
            &[value, Value::Number(usize_to_f64(index))],
          )?;
          value = vm.await_value(mapped)?;
        }
        let key = Value::String(index.to_string().into());
        let mut desc_data = ObjectData::new();
        desc_data.properties.insert(
          PropertyKey::String("value".to_string().into()),
          PropertyDescriptor::ordinary_data(value),
        );
        desc_data.properties.insert(
          PropertyKey::String("writable".to_string().into()),
          PropertyDescriptor::ordinary_data(Value::Bool(true)),
        );
        desc_data.properties.insert(
          PropertyKey::String("enumerable".to_string().into()),
          PropertyDescriptor::ordinary_data(Value::Bool(true)),
        );
        desc_data.properties.insert(
          PropertyKey::String("configurable".to_string().into()),
          PropertyDescriptor::ordinary_data(Value::Bool(true)),
        );
        let desc = Value::Object(Rc::new(RefCell::new(desc_data)));
        if !vm.define_property_value(&created, &key, &desc)? {
          return Err(vm.throw_type_error("Array.fromAsync failed to define element"));
        }
        index += 1;
      }
      Ok(())
    })();
    if let Err(err) = iter_result {
      close_iterator(vm, &iter_obj)?;
      return Err(err);
    }

    let len_key = Value::String("length".to_string().into());
    vm.set_property_host(&created, &len_key, Value::Number(usize_to_f64(index)))?;
  } else {
    let items = collect_iterable_values(vm, source)?;
    let len = items.len();
    for (i, mut value) in items.into_iter().enumerate() {
      if !matches!(map_fn, Value::Undefined) {
        let mapped =
          vm.call_value_with_this(map_fn, this_arg, &[value, Value::Number(usize_to_f64(i))])?;
        value = vm.await_value(mapped)?;
      }
      let key = Value::String(i.to_string().into());
      let mut desc_data = ObjectData::new();
      desc_data.properties.insert(
        PropertyKey::String("value".to_string().into()),
        PropertyDescriptor::ordinary_data(value),
      );
      desc_data.properties.insert(
        PropertyKey::String("writable".to_string().into()),
        PropertyDescriptor::ordinary_data(Value::Bool(true)),
      );
      desc_data.properties.insert(
        PropertyKey::String("enumerable".to_string().into()),
        PropertyDescriptor::ordinary_data(Value::Bool(true)),
      );
      desc_data.properties.insert(
        PropertyKey::String("configurable".to_string().into()),
        PropertyDescriptor::ordinary_data(Value::Bool(true)),
      );
      let desc = Value::Object(Rc::new(RefCell::new(desc_data)));
      if !vm.define_property_value(&created, &key, &desc)? {
        return Err(vm.throw_type_error("Array.fromAsync failed to define element"));
      }
    }
    let len_key = Value::String("length".to_string().into());
    vm.set_property_host(&created, &len_key, Value::Number(usize_to_f64(len)))?;
  }

  Ok(created)
}
