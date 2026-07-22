//! Collection builtin functions and prototype methods

use std::cell::RefCell;
use std::rc::Rc;

use num_traits::ToPrimitive;

use crate::runtime_utils::{builtin_object, strict_equal, usize_to_f64};
use crate::value::{
  ArrayData, BuiltinFn, ObjectData, PropertyDescriptor, PropertyKey, SymbolId, Value,
};

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
    let receiver = args.first().cloned().unwrap_or(Value::Undefined);
    if matches!(
      receiver,
      Value::Object(_) | Value::Array(..) | Value::Closure { .. } | Value::BuiltinObject(_, _)
    ) {
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
      let iterable = args.get(1).cloned().unwrap_or(Value::Undefined);
      if let Value::Array(items, _) = iterable {
        for item in items.borrow().iter().flatten() {
          if let Value::Array(pair, _) = item {
            let p = pair.borrow();
            if p.len() >= 2 {
              let key = p[0].as_ref().cloned().unwrap_or(Value::Undefined);
              let val = p[1].as_ref().cloned().unwrap_or(Value::Undefined);
              let mut updated = false;
              for existing in arr_rc.borrow_mut().iter_mut() {
                if let Some(Value::Array(existing_pair, _)) = existing {
                  let mut ep = existing_pair.borrow_mut();
                  if !ep.is_empty()
                    && strict_equal(ep[0].as_ref().unwrap_or(&Value::Undefined), &key)
                  {
                    if ep.len() < 2 {
                      ep.push(Some(val.clone()));
                    } else {
                      ep[1] = Some(val.clone());
                    }
                    updated = true;
                    break;
                  }
                }
              }
              if !updated {
                let pair_rc = Rc::new(RefCell::new(ArrayData::from(vec![Some(key), Some(val)])));
                let pair_props = Rc::new(RefCell::new(ObjectData::new()));
                pair_props.borrow_mut().properties.insert(
                  PropertyKey::String("length".to_string()),
                  PropertyDescriptor::data(Value::Number(2.0)),
                );
                arr_rc
                  .borrow_mut()
                  .push(Some(Value::Array(pair_rc, pair_props)));
              }
            }
          }
        }
        drop(vm.set_property_host(
          &receiver,
          &Value::String("size".to_string()),
          Value::Number(usize_to_f64(arr_rc.borrow().len())),
        )?);
      }
      return Ok(receiver);
    }
    Ok(Value::Object(Rc::new(RefCell::new(ObjectData::new()))))
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
              if p.len() >= 2 && strict_equal(p[0].as_ref().unwrap_or(&Value::Undefined), &key) {
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
              if !p.is_empty() && strict_equal(p[0].as_ref().unwrap_or(&Value::Undefined), &key) {
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
        if let Value::Array(arr, props) = entries {
          let mut vec = arr.borrow_mut();
          let removed = if let Some(pos) = vec.iter().position(|item| {
            if let Some(Value::Array(pair, _)) = item {
              let p = pair.borrow();
              !p.is_empty() && strict_equal(p[0].as_ref().unwrap_or(&Value::Undefined), &key)
            } else {
              false
            }
          }) {
            vec.remove(pos);
            true
          } else {
            false
          };
          let new_len = usize_to_f64(vec.len());
          drop(vec);
          props.borrow_mut().properties.insert(
            PropertyKey::String("length".to_string()),
            PropertyDescriptor::data(Value::Number(new_len)),
          );
          drop(vm.set_property_host(
            this,
            &Value::String("size".to_string()),
            Value::Number(new_len),
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
              if !p.is_empty() && strict_equal(p[0].as_ref().unwrap_or(&Value::Undefined), &key) {
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
          drop(vm.set_property_host(
            this,
            &Value::String("size".to_string()),
            Value::Number(usize_to_f64(arr.borrow().len())),
          )?);
        }
        Ok(this.clone())
      }),
      function_proto_map,
    )),
  );
  let function_proto_for_map_iter = Rc::clone(function_proto_map);
  map_proto_map.borrow_mut().properties.insert(
    PKey::Symbol(SymbolId::well_known_iterator()),
    PropertyDescriptor::data(builtin_object(
      Rc::new(move |vm, args| {
        let undefined = Value::Undefined;
        let this = args.first().unwrap_or(&undefined);
        let entries =
          vm.get_property_host(this, &PropertyKey::String("__map_entries".to_string()))?;
        let mut iter = ObjectData::new();
        iter.properties.insert(
          PropertyKey::String("__iter_entries".to_string()),
          PropertyDescriptor::data(entries),
        );
        iter.properties.insert(
          PropertyKey::String("__iter_index".to_string()),
          PropertyDescriptor::data(Value::Number(0.0)),
        );
        let function_proto_for_next = Rc::clone(&function_proto_for_map_iter);
        let next_fn = builtin_object(
          Rc::new(|vm, args| {
            let undefined = Value::Undefined;
            let it = args.first().unwrap_or(&undefined);
            let entries = vm
              .get_property_host(it, &PropertyKey::String("__iter_entries".to_string()))
              .unwrap_or(Value::Undefined);
            let idx_val = vm
              .get_property_host(it, &PropertyKey::String("__iter_index".to_string()))
              .unwrap_or(Value::Number(0.0));
            let mut idx = vm
              .to_number_value(&idx_val)
              .ok()
              .and_then(|n| n.trunc().to_usize())
              .unwrap_or(0);
            let mut done = true;
            let mut value = Value::Undefined;
            if let Value::Array(arr, _) = entries
              && idx < arr.borrow().len()
            {
              value = arr.borrow()[idx]
                .as_ref()
                .cloned()
                .unwrap_or(Value::Undefined);
              idx += 1;
              done = false;
            }
            drop(vm.set_property_host(
              it,
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
            &function_proto_for_map_iter,
          )),
        );
        Ok(Value::Object(Rc::new(RefCell::new(iter))))
      }),
      function_proto_map,
    )),
  );

  let set_proto_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(ObjectData::with_prototype(
    Value::Object(Rc::clone(object_proto_map)),
  )));
  let set_ctor_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(ObjectData::with_prototype(
    Value::Object(Rc::clone(function_proto_map)),
  )));
  let set_ctor_fn: BuiltinFn = Rc::new(move |vm, args| {
    let undefined = Value::Undefined;
    let receiver = args.first().unwrap_or(&undefined);
    if matches!(
      receiver,
      Value::Object(_) | Value::Array(..) | Value::Closure { .. } | Value::BuiltinObject(_, _)
    ) {
      let arr_rc = Rc::new(RefCell::new(ArrayData::from(Vec::<Option<Value>>::new())));
      let arr_props = Rc::new(RefCell::new(ObjectData::new()));
      arr_props.borrow_mut().properties.insert(
        PropertyKey::String("length".to_string()),
        PropertyDescriptor::data(Value::Number(0.0)),
      );
      drop(vm.set_property_host(
        receiver,
        &Value::String("__set_values".to_string()),
        Value::Array(Rc::clone(&arr_rc), arr_props),
      )?);
      drop(vm.set_property_host(
        receiver,
        &Value::String("size".to_string()),
        Value::Number(0.0),
      )?);
      let iterable = args.get(1).cloned().unwrap_or(Value::Undefined);
      if let Value::Array(items, _) = iterable {
        for item in items.borrow().iter().flatten() {
          let mut exists = false;
          for existing in arr_rc.borrow().iter().flatten() {
            if strict_equal(existing, item) {
              exists = true;
              break;
            }
          }
          if !exists {
            arr_rc.borrow_mut().push(Some(item.clone()));
          }
        }
        drop(vm.set_property_host(
          receiver,
          &Value::String("size".to_string()),
          Value::Number(usize_to_f64(arr_rc.borrow().len())),
        )?);
      }
      return Ok(receiver.clone());
    }
    Ok(Value::Object(Rc::new(RefCell::new(ObjectData::new()))))
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
            if strict_equal(existing, &value) {
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
          drop(vm.set_property_host(
            this,
            &Value::String("size".to_string()),
            Value::Number(usize_to_f64(arr.borrow().len())),
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
            if strict_equal(existing, &value) {
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
            .is_some_and(|existing| strict_equal(existing, &value))
        });
        let Some(index) = index else {
          return Ok(Value::Bool(false));
        };
        values.borrow_mut().remove(index);
        let size = values.borrow().len();
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
            if let Value::Array(arr, _) = values
              && idx < arr.borrow().len()
            {
              value = arr.borrow()[idx]
                .as_ref()
                .cloned()
                .unwrap_or(Value::Undefined);
              idx += 1;
              done = false;
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
