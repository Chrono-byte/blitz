//! Collection builtin functions and prototype methods

use std::cell::RefCell;
use std::rc::Rc;

use num_traits::ToPrimitive;

use crate::runtime_utils::{
  builtin_object, same_value_zero, static_builtin_arg_offset, usize_to_f64,
};
use crate::value::{
  ArrayData, BuiltinFn, ObjectData, PropertyDescriptor, PropertyKey, SymbolId, Value,
};

fn make_map_iterator(
  entries: Value,
  kind: &str,
  function_proto: &Rc<RefCell<ObjectData>>,
) -> Value {
  let mut iter = ObjectData::with_prototype(Value::Object(Rc::clone(function_proto)));
  iter.properties.insert(
    PropertyKey::String("__iter_entries".to_string()),
    PropertyDescriptor::data(entries),
  );
  iter.properties.insert(
    PropertyKey::String("__iter_index".to_string()),
    PropertyDescriptor::data(Value::Number(0.0)),
  );

  let kind = kind.to_string();
  let next_fn = builtin_object(
    Rc::new(move |vm, args| {
      let undefined = Value::Undefined;
      let iterator = args.first().unwrap_or(&undefined);
      let entries = vm
        .get_property_host(iterator, &PropertyKey::String("__iter_entries".to_string()))
        .unwrap_or(Value::Undefined);
      let index_value = vm
        .get_property_host(iterator, &PropertyKey::String("__iter_index".to_string()))
        .unwrap_or(Value::Number(0.0));
      let mut index = vm
        .to_number_value(&index_value)
        .ok()
        .and_then(|number| number.trunc().to_usize())
        .unwrap_or(0);
      let mut done = true;
      let mut value = Value::Undefined;

      if let Value::Array(entries, _) = entries {
        loop {
          let entry = entries.borrow().get(index).cloned().flatten();
          let Some(entry) = entry else {
            if index >= entries.borrow().len() {
              break;
            }
            index += 1;
            continue;
          };
          index += 1;
          let Value::Array(pair, _) = &entry else {
            continue;
          };
          value = match kind.as_str() {
            "keys" => pair
              .borrow()
              .first()
              .and_then(Clone::clone)
              .unwrap_or(Value::Undefined),
            "values" => pair
              .borrow()
              .get(1)
              .and_then(Clone::clone)
              .unwrap_or(Value::Undefined),
            _ => entry,
          };
          done = false;
          break;
        }
      }

      drop(vm.set_property_host(
        iterator,
        &Value::String("__iter_index".to_string()),
        Value::Number(usize_to_f64(index)),
      )?);
      let mut result = ObjectData::new();
      result.properties.insert(
        PropertyKey::String("value".to_string()),
        PropertyDescriptor::data(value),
      );
      result.properties.insert(
        PropertyKey::String("done".to_string()),
        PropertyDescriptor::data(Value::Bool(done)),
      );
      Ok(Value::Object(Rc::new(RefCell::new(result))))
    }),
    function_proto,
  );
  iter.properties.insert(
    PropertyKey::String("next".to_string()),
    PropertyDescriptor::data(next_fn),
  );
  iter.properties.insert(
    PropertyKey::Symbol(SymbolId::well_known_iterator()),
    PropertyDescriptor::data(builtin_object(
      Rc::new(|_vm, args| Ok(args.first().cloned().unwrap_or(Value::Undefined))),
      function_proto,
    )),
  );
  Value::Object(Rc::new(RefCell::new(iter)))
}

fn close_collection_iterator(
  vm: &mut crate::vm::Vm,
  iterator: &Value,
) -> Result<(), crate::errors::RuntimeError> {
  let return_method = vm.get_property_host(iterator, &PropertyKey::String("return".to_string()))?;
  if matches!(return_method, Value::Undefined | Value::Null) {
    return Ok(());
  }
  if !crate::vm::Vm::is_callable_value(&return_method) {
    return Err(vm.throw_type_error("Iterator return is not callable"));
  }
  let result = vm.call_value_with_this(&return_method, iterator, &[])?;
  if !result.is_object() {
    return Err(vm.throw_type_error("Iterator return result is not an object"));
  }
  Ok(())
}

/// Consumes a synchronous ECMAScript iterable, closing its iterator if value
/// processing completes abruptly.
fn consume_collection_iterable(
  vm: &mut crate::vm::Vm,
  iterable: &Value,
  mut consume: impl FnMut(&mut crate::vm::Vm, Value) -> Result<(), crate::errors::RuntimeError>,
) -> Result<(), crate::errors::RuntimeError> {
  let iterator_method = vm.get_property_host(
    iterable,
    &PropertyKey::Symbol(SymbolId::well_known_iterator()),
  )?;
  if !crate::vm::Vm::is_callable_value(&iterator_method) {
    return Err(vm.throw_type_error("Value is not iterable"));
  }

  let iterator = vm.call_value_with_this(&iterator_method, iterable, &[])?;
  if !iterator.is_object() {
    return Err(vm.throw_type_error("Iterator method must return an object"));
  }

  let result = (|| {
    loop {
      let next_method =
        vm.get_property_host(&iterator, &PropertyKey::String("next".to_string()))?;
      if !crate::vm::Vm::is_callable_value(&next_method) {
        return Err(vm.throw_type_error("Iterator next method is not callable"));
      }
      let next_result = vm.call_value_with_this(&next_method, &iterator, &[])?;
      if !next_result.is_object() {
        return Err(vm.throw_type_error("Iterator result must be an object"));
      }
      let done = vm.get_property_host(&next_result, &PropertyKey::String("done".to_string()))?;
      if vm.to_boolean(&done) {
        return Ok(());
      }
      let value = vm.get_property_host(&next_result, &PropertyKey::String("value".to_string()))?;
      consume(vm, value)?;
    }
  })();

  match result {
    Ok(()) => Ok(()),
    Err(error) => {
      // A JavaScript accessor/callback may already have unwound the VM to a
      // language-level handler. Calling back into that suspended execution
      // would run the handler twice, so leave propagation to the VM in that
      // case. Host-side abrupt completions can still close normally.
      if vm.has_pending_exception() {
        return match vm.close_iterator_in_fork(&iterator) {
          Ok(()) => Err(error),
          Err(close_error) => Err(close_error),
        };
      }
      match close_collection_iterator(vm, &iterator) {
        Ok(()) => Err(error),
        Err(close_error) => Err(close_error),
      }
    }
  }
}

/// Installs the Map and Set constructors on the given object and function prototypes.
#[allow(clippy::too_many_lines)]
pub(crate) fn install_map_set_constructors(
  object_proto_map: &Rc<RefCell<ObjectData>>,
  function_proto_map: &Rc<RefCell<ObjectData>>,
) -> (Value, Value) {
  use PropertyKey as PKey;

  let map_proto_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(ObjectData::with_prototype(
    Value::Object(Rc::clone(object_proto_map)),
  )));
  let map_ctor_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(ObjectData::with_prototype(
    Value::Object(Rc::clone(function_proto_map)),
  )));
  let map_ctor_fn: BuiltinFn = Rc::new(move |vm, args| {
    if vm.current_new_target().is_none() {
      return Err(vm.throw_type_error("Constructor Map requires 'new'"));
    }
    let receiver = args.first().cloned().unwrap_or(Value::Undefined);
    if !receiver.is_object() {
      return Err(vm.throw_type_error("Map receiver must be an object"));
    }

    let arr_rc = Rc::new(RefCell::new(ArrayData::from(Vec::<Option<Value>>::new())));
    let arr_props = Rc::new(RefCell::new(ObjectData::new()));
    arr_props.borrow_mut().properties.insert(
      PropertyKey::String("length".to_string()),
      PropertyDescriptor::data(Value::Number(0.0)),
    );
    drop(vm.set_property_host(
      &receiver,
      &Value::String("__map_entries".to_string()),
      Value::Array(Rc::clone(&arr_rc), arr_props),
    )?);
    drop(vm.set_property_host(
      &receiver,
      &Value::String("size".to_string()),
      Value::Number(0.0),
    )?);

    let iterable = args
      .get(static_builtin_arg_offset(vm, args, "Map"))
      .cloned()
      .unwrap_or(Value::Undefined);
    if matches!(iterable, Value::Undefined) {
      return Ok(receiver);
    }

    let adder = vm.get_property_host(&receiver, &PropertyKey::String("set".to_string()))?;
    if !crate::vm::Vm::is_callable_value(&adder) {
      return Err(vm.throw_type_error("Map.prototype.set is not callable"));
    }
    consume_collection_iterable(vm, &iterable, |vm, item| {
      if !item.is_object() {
        return Err(vm.throw_type_error("Iterator value is not an entry object"));
      }
      let key = vm.get_property_host(&item, &PropertyKey::String("0".to_string()))?;
      let value = vm.get_property_host(&item, &PropertyKey::String("1".to_string()))?;
      drop(vm.call_value_with_this(&adder, &receiver, &[key, value])?);
      Ok(())
    })?;
    Ok(receiver)
  });
  let map_ctor_val = Value::BuiltinObject(Rc::clone(&map_ctor_fn), Rc::clone(&map_ctor_map));
  map_ctor_map.borrow_mut().properties.insert(
    PKey::String("prototype".to_string()),
    PropertyDescriptor::data(Value::Object(Rc::clone(&map_proto_map))),
  );
  map_proto_map.borrow_mut().properties.insert(
    PKey::String("constructor".to_string()),
    PropertyDescriptor::data(map_ctor_val.clone()),
  );
  map_proto_map.borrow_mut().properties.insert(
    PKey::String("get".to_string()),
    PropertyDescriptor::data(builtin_object(
      Rc::new(|vm, args| {
        let undefined = Value::Undefined;
        let this = args.first().unwrap_or(&undefined);
        let key = args.get(1).cloned().unwrap_or(Value::Undefined);
        let entries =
          vm.get_property_host(this, &PropertyKey::String("__map_entries".to_string()))?;
        if let Value::Array(arr, _) = entries {
          for item in arr.borrow().iter() {
            if let Some(Value::Array(pair, _)) = item {
              let p = pair.borrow();
              if p.len() >= 2 && same_value_zero(p[0].as_ref().unwrap_or(&Value::Undefined), &key) {
                return Ok(p[1].as_ref().unwrap_or(&Value::Undefined).clone());
              }
            }
          }
        }
        Ok(Value::Undefined)
      }),
      function_proto_map,
    )),
  );
  map_proto_map.borrow_mut().properties.insert(
    PKey::String("has".to_string()),
    PropertyDescriptor::data(builtin_object(
      Rc::new(|vm, args| {
        let undefined = Value::Undefined;
        let this = args.first().unwrap_or(&undefined);
        let key = args.get(1).cloned().unwrap_or(Value::Undefined);
        let entries =
          vm.get_property_host(this, &PropertyKey::String("__map_entries".to_string()))?;
        if let Value::Array(arr, _) = entries {
          for item in arr.borrow().iter() {
            if let Some(Value::Array(pair, _)) = item {
              let p = pair.borrow();
              if !p.is_empty() && same_value_zero(p[0].as_ref().unwrap_or(&Value::Undefined), &key)
              {
                return Ok(Value::Bool(true));
              }
            }
          }
        }
        Ok(Value::Bool(false))
      }),
      function_proto_map,
    )),
  );
  map_proto_map.borrow_mut().properties.insert(
    PKey::String("delete".to_string()),
    PropertyDescriptor::data(builtin_object(
      Rc::new(|vm, args| {
        let undefined = Value::Undefined;
        let this = args.first().unwrap_or(&undefined);
        let key = args.get(1).cloned().unwrap_or(Value::Undefined);
        let entries_key = PropertyKey::String("__map_entries".to_string());
        let entries = vm.get_property_host(this, &entries_key)?;
        if let Value::Array(arr, _) = entries {
          let mut entries = arr.borrow_mut();
          let removed = if let Some(pos) = entries.iter().position(|item| {
            if let Some(Value::Array(pair, _)) = item {
              let pair = pair.borrow();
              !pair.is_empty()
                && same_value_zero(pair[0].as_ref().unwrap_or(&Value::Undefined), &key)
            } else {
              false
            }
          }) {
            // Map iterators retain their next index. Leave a tombstone here
            // instead of shifting later entries past an active iterator.
            entries[pos] = None;
            true
          } else {
            false
          };
          let size = entries.iter().flatten().count();
          drop(entries);
          drop(vm.set_property_host(
            this,
            &Value::String("size".to_string()),
            Value::Number(usize_to_f64(size)),
          )?);
          return Ok(Value::Bool(removed));
        }
        Ok(Value::Bool(false))
      }),
      function_proto_map,
    )),
  );
  map_proto_map.borrow_mut().properties.insert(
    PKey::String("set".to_string()),
    PropertyDescriptor::data(builtin_object(
      Rc::new(|vm, args| {
        let undefined = Value::Undefined;
        let this = args.first().unwrap_or(&undefined);
        let key = args.get(1).cloned().unwrap_or(Value::Undefined);
        let value = args.get(2).cloned().unwrap_or(Value::Undefined);
        let entries_key = PropertyKey::String("__map_entries".to_string());
        let entries = vm.get_property_host(this, &entries_key)?;
        if let Value::Array(arr, props) = entries {
          let mut updated = false;
          for item in arr.borrow_mut().iter_mut() {
            if let Some(Value::Array(pair, _)) = item {
              let mut p = pair.borrow_mut();
              if !p.is_empty() && same_value_zero(p[0].as_ref().unwrap_or(&Value::Undefined), &key)
              {
                if p.len() < 2 {
                  p.push(Some(value.clone()));
                } else {
                  p[1] = Some(value.clone());
                }
                updated = true;
                break;
              }
            }
          }
          if !updated {
            let pair_rc = Rc::new(RefCell::new(ArrayData::from(vec![Some(key), Some(value)])));
            let pair_props = Rc::new(RefCell::new(ObjectData::new()));
            pair_props.borrow_mut().properties.insert(
              PropertyKey::String("length".to_string()),
              PropertyDescriptor::data(Value::Number(2.0)),
            );
            arr
              .borrow_mut()
              .push(Some(Value::Array(pair_rc, pair_props)));
            props.borrow_mut().properties.insert(
              PropertyKey::String("length".to_string()),
              PropertyDescriptor::data(Value::Number(usize_to_f64(arr.borrow().len()))),
            );
          }
          let size = arr.borrow().iter().flatten().count();
          drop(vm.set_property_host(
            this,
            &Value::String("size".to_string()),
            Value::Number(usize_to_f64(size)),
          )?);
        }
        Ok(this.clone())
      }),
      function_proto_map,
    )),
  );
  let function_proto_for_map_iter = Rc::clone(function_proto_map);
  for (name, kind) in [("keys", "keys"), ("values", "values")] {
    map_proto_map.borrow_mut().properties.insert(
      PKey::String(name.to_string()),
      PropertyDescriptor::data(builtin_object(
        Rc::new({
          let function_proto_for_map_iter = Rc::clone(&function_proto_for_map_iter);
          move |vm, args| {
            let undefined = Value::Undefined;
            let this = args.first().unwrap_or(&undefined);
            let entries =
              vm.get_property_host(this, &PropertyKey::String("__map_entries".to_string()))?;
            Ok(make_map_iterator(
              entries,
              kind,
              &function_proto_for_map_iter,
            ))
          }
        }),
        function_proto_map,
      )),
    );
  }
  let entries_function = builtin_object(
    Rc::new({
      let function_proto_for_map_iter = Rc::clone(&function_proto_for_map_iter);
      move |vm, args| {
        let undefined = Value::Undefined;
        let this = args.first().unwrap_or(&undefined);
        let entries =
          vm.get_property_host(this, &PropertyKey::String("__map_entries".to_string()))?;
        Ok(make_map_iterator(
          entries,
          "entries",
          &function_proto_for_map_iter,
        ))
      }
    }),
    function_proto_map,
  );
  map_proto_map.borrow_mut().properties.insert(
    PKey::String("entries".to_string()),
    PropertyDescriptor::data(entries_function.clone()),
  );
  map_proto_map.borrow_mut().properties.insert(
    PKey::Symbol(SymbolId::well_known_iterator()),
    PropertyDescriptor::data(entries_function),
  );

  let set_proto_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(ObjectData::with_prototype(
    Value::Object(Rc::clone(object_proto_map)),
  )));
  let set_ctor_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(ObjectData::with_prototype(
    Value::Object(Rc::clone(function_proto_map)),
  )));
  let set_ctor_fn: BuiltinFn = Rc::new(move |vm, args| {
    if vm.current_new_target().is_none() {
      return Err(vm.throw_type_error("Constructor Set requires 'new'"));
    }
    let receiver = args.first().cloned().unwrap_or(Value::Undefined);
    if !receiver.is_object() {
      return Err(vm.throw_type_error("Set receiver must be an object"));
    }

    let arr_rc = Rc::new(RefCell::new(ArrayData::from(Vec::<Option<Value>>::new())));
    let arr_props = Rc::new(RefCell::new(ObjectData::new()));
    arr_props.borrow_mut().properties.insert(
      PropertyKey::String("length".to_string()),
      PropertyDescriptor::data(Value::Number(0.0)),
    );
    drop(vm.set_property_host(
      &receiver,
      &Value::String("__set_values".to_string()),
      Value::Array(Rc::clone(&arr_rc), arr_props),
    )?);
    drop(vm.set_property_host(
      &receiver,
      &Value::String("size".to_string()),
      Value::Number(0.0),
    )?);

    let iterable = args
      .get(static_builtin_arg_offset(vm, args, "Set"))
      .cloned()
      .unwrap_or(Value::Undefined);
    if matches!(iterable, Value::Undefined) {
      return Ok(receiver);
    }

    let adder = vm.get_property_host(&receiver, &PropertyKey::String("add".to_string()))?;
    if !crate::vm::Vm::is_callable_value(&adder) {
      return Err(vm.throw_type_error("Set.prototype.add is not callable"));
    }
    consume_collection_iterable(vm, &iterable, |vm, value| {
      drop(vm.call_value_with_this(&adder, &receiver, &[value])?);
      Ok(())
    })?;
    Ok(receiver)
  });
  let set_ctor_val = Value::BuiltinObject(Rc::clone(&set_ctor_fn), Rc::clone(&set_ctor_map));
  set_ctor_map.borrow_mut().properties.insert(
    PKey::String("prototype".to_string()),
    PropertyDescriptor::data(Value::Object(Rc::clone(&set_proto_map))),
  );
  set_proto_map.borrow_mut().properties.insert(
    PKey::String("constructor".to_string()),
    PropertyDescriptor::data(set_ctor_val.clone()),
  );
  set_proto_map.borrow_mut().properties.insert(
    PKey::String("add".to_string()),
    PropertyDescriptor::data(builtin_object(
      Rc::new(|vm, args| {
        let undefined = Value::Undefined;
        let this = args.first().unwrap_or(&undefined);
        let value = args.get(1).cloned().unwrap_or(Value::Undefined);
        let values =
          vm.get_property_host(this, &PropertyKey::String("__set_values".to_string()))?;
        if let Value::Array(arr, props) = values {
          let mut exists = false;
          for existing in arr.borrow().iter().flatten() {
            if same_value_zero(existing, &value) {
              exists = true;
              break;
            }
          }
          if !exists {
            arr.borrow_mut().push(Some(value));
            props.borrow_mut().properties.insert(
              PropertyKey::String("length".to_string()),
              PropertyDescriptor::data(Value::Number(usize_to_f64(arr.borrow().len()))),
            );
          }
          let size = arr.borrow().iter().flatten().count();
          drop(vm.set_property_host(
            this,
            &Value::String("size".to_string()),
            Value::Number(usize_to_f64(size)),
          )?);
        }
        Ok(this.clone())
      }),
      function_proto_map,
    )),
  );
  set_proto_map.borrow_mut().properties.insert(
    PKey::String("has".to_string()),
    PropertyDescriptor::data(builtin_object(
      Rc::new(|vm, args| {
        let undefined = Value::Undefined;
        let this = args.first().unwrap_or(&undefined);
        let value = args.get(1).cloned().unwrap_or(Value::Undefined);
        let values =
          vm.get_property_host(this, &PropertyKey::String("__set_values".to_string()))?;
        if let Value::Array(arr, _) = values {
          for existing in arr.borrow().iter().flatten() {
            if same_value_zero(existing, &value) {
              return Ok(Value::Bool(true));
            }
          }
        }
        Ok(Value::Bool(false))
      }),
      function_proto_map,
    )),
  );
  set_proto_map.borrow_mut().properties.insert(
    PKey::String("delete".to_string()),
    PropertyDescriptor::data(builtin_object(
      Rc::new(|vm, args| {
        let undefined = Value::Undefined;
        let this = args.first().unwrap_or(&undefined);
        let value = args.get(1).cloned().unwrap_or(Value::Undefined);
        let values =
          vm.get_property_host(this, &PropertyKey::String("__set_values".to_string()))?;
        let Value::Array(values, _) = values else {
          return Ok(Value::Bool(false));
        };
        let index = values.borrow().iter().position(|existing| {
          existing
            .as_ref()
            .is_some_and(|existing| same_value_zero(existing, &value))
        });
        let Some(index) = index else {
          return Ok(Value::Bool(false));
        };
        values.borrow_mut()[index] = None;
        let size = values.borrow().iter().flatten().count();
        drop(vm.set_property_host(
          this,
          &Value::String("size".to_string()),
          Value::Number(usize_to_f64(size)),
        )?);
        Ok(Value::Bool(true))
      }),
      function_proto_map,
    )),
  );
  let function_proto_for_set_iter = Rc::clone(function_proto_map);
  set_proto_map.borrow_mut().properties.insert(
    PKey::Symbol(SymbolId::well_known_iterator()),
    PropertyDescriptor::data(builtin_object(
      Rc::new(move |vm, args| {
        let this = args.first().cloned().unwrap_or(Value::Undefined);
        let values =
          vm.get_property_host(&this, &PropertyKey::String("__set_values".to_string()))?;
        let mut iter = ObjectData::new();
        iter.properties.insert(
          PropertyKey::String("__iter_values".to_string()),
          PropertyDescriptor::data(values),
        );
        iter.properties.insert(
          PropertyKey::String("__iter_index".to_string()),
          PropertyDescriptor::data(Value::Number(0.0)),
        );
        let function_proto_for_next = Rc::clone(&function_proto_for_set_iter);
        let next_fn = builtin_object(
          Rc::new(|vm, args| {
            let it = args.first().cloned().unwrap_or(Value::Undefined);
            let values = vm
              .get_property_host(&it, &PropertyKey::String("__iter_values".to_string()))
              .unwrap_or(Value::Undefined);
            let idx_val = vm
              .get_property_host(&it, &PropertyKey::String("__iter_index".to_string()))
              .unwrap_or(Value::Number(0.0));
            let mut idx = vm
              .to_number_value(&idx_val)
              .ok()
              .and_then(|n| n.trunc().to_usize())
              .unwrap_or(0);
            let mut done = true;
            let mut value = Value::Undefined;
            if let Value::Array(arr, _) = values {
              loop {
                let item = arr.borrow().get(idx).cloned().flatten();
                let Some(item) = item else {
                  if idx >= arr.borrow().len() {
                    break;
                  }
                  idx += 1;
                  continue;
                };
                value = item;
                idx += 1;
                done = false;
                break;
              }
            }
            drop(vm.set_property_host(
              &it,
              &Value::String("__iter_index".to_string()),
              Value::Number(usize_to_f64(idx)),
            )?);
            let mut result = ObjectData::new();
            result.properties.insert(
              PropertyKey::String("value".to_string()),
              PropertyDescriptor::data(value),
            );
            result.properties.insert(
              PropertyKey::String("done".to_string()),
              PropertyDescriptor::data(Value::Bool(done)),
            );
            Ok(Value::Object(Rc::new(RefCell::new(result))))
          }),
          &function_proto_for_next,
        );
        iter.properties.insert(
          PropertyKey::String("next".to_string()),
          PropertyDescriptor::data(next_fn),
        );
        iter.properties.insert(
          PropertyKey::Symbol(SymbolId::well_known_iterator()),
          PropertyDescriptor::data(builtin_object(
            Rc::new(|_vm, args| Ok(args.first().cloned().unwrap_or(Value::Undefined))),
            &function_proto_for_set_iter,
          )),
        );
        Ok(Value::Object(Rc::new(RefCell::new(iter))))
      }),
      function_proto_map,
    )),
  );

  (map_ctor_val, set_ctor_val)
}
