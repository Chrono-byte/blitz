// SPDX-License-Identifier: MPL-2.0
//! Collection builtin functions and prototype methods

use std::cell::RefCell;
use std::rc::Rc;

use num_traits::ToPrimitive;

use crate::runtime_utils::{
  array_from_values, builtin_object, same_value_zero, set_builtin_length, set_builtin_name,
  static_builtin_arg_offset, usize_to_f64,
};
use crate::value::{BuiltinFn, ObjectData, PropertyDescriptor, PropertyKey, SymbolId, Value};

pub(crate) const MAP_DATA_SLOT: &str = "[[MapData]]";
pub(crate) const SET_DATA_SLOT: &str = "[[SetData]]";
const MAP_ITERATOR_BRAND_SLOT: &str = "[[MapIteratorBrand]]";
const SET_ITERATOR_BRAND_SLOT: &str = "[[SetIteratorBrand]]";
const ITERATOR_SOURCE_SLOT: &str = "[[CollectionIteratorSource]]";
const ITERATOR_INDEX_SLOT: &str = "[[CollectionIteratorIndex]]";
const ITERATOR_DONE_SLOT: &str = "[[CollectionIteratorDone]]";
const ITERATOR_KIND_SLOT: &str = "[[CollectionIteratorKind]]";

fn direct_object_data(value: &Value) -> Option<&Rc<RefCell<ObjectData>>> {
  match value {
    Value::Object(data)
    | Value::Array(_, data)
    | Value::Closure { props: data, .. }
    | Value::BuiltinObject(_, data) => Some(data),
    _ => None,
  }
}

fn internal_slot(
  value: &Value,
  key: &'static str,
) -> Option<Value> {
  direct_object_data(value)?
    .borrow()
    .internal_slots
    .get(key)
    .cloned()
}

fn set_internal_slot(
  value: &Value,
  key: &'static str,
  slot_value: Value,
) -> bool {
  let Some(data) = direct_object_data(value) else {
    return false;
  };
  data
    .borrow_mut()
    .internal_slots
    .insert(key, slot_value);
  true
}

fn require_state(
  vm: &mut crate::vm::Vm,
  receiver: &Value,
  key: &'static str,
  name: &str,
) -> Result<Value, crate::errors::RuntimeError> {
  match internal_slot(receiver, key) {
    Some(state @ Value::Array(..)) => Ok(state),
    _ => Err(vm.throw_type_error(&format!(
      "Method Map.prototype.{name} called on incompatible receiver"
    ))),
  }
}

fn method(
  f: BuiltinFn,
  function_proto: &Rc<RefCell<ObjectData>>,
  name: &str,
  length: f64,
) -> Value {
  let value = builtin_object(f, function_proto);
  set_builtin_name(&value, name);
  set_builtin_length(&value, length);
  value
}

fn iterator_result(
  vm: &crate::vm::Vm,
  value: Value,
  done: bool,
) -> Value {
  let mut result = ObjectData::with_prototype(vm.realm().object_prototype());
  result.properties.insert(
    PropertyKey::String("value".to_string().into()),
    PropertyDescriptor::ordinary_data(value),
  );
  result.properties.insert(
    PropertyKey::String("done".to_string().into()),
    PropertyDescriptor::ordinary_data(Value::Bool(done)),
  );
  Value::Object(Rc::new(RefCell::new(result)))
}

fn collection_entry_array(
  vm: &crate::vm::Vm,
  key: Value,
  value: Value,
) -> Value {
  let array = array_from_values(vec![key, value]);
  if let Value::Array(_, properties) = &array {
    properties
      .borrow_mut()
      .set_prototype_value(vm.realm().array_prototype());
  }
  array
}

fn canonicalize_keyed_collection_value(value: Value) -> Value {
  match value {
    Value::Number(number) if number == 0.0 => Value::Number(0.0),
    value => value,
  }
}

fn pair_values(entry: &Value) -> Option<(Value, Value)> {
  let Value::Array(pair, _) = entry else {
    return None;
  };
  let pair = pair.borrow();
  Some((
    pair
      .first()
      .cloned()
      .flatten()
      .unwrap_or(Value::Undefined),
    pair
      .get(1)
      .cloned()
      .flatten()
      .unwrap_or(Value::Undefined),
  ))
}

fn iterator_object(
  prototype: &Rc<RefCell<ObjectData>>,
  brand_key: &'static str,
  source_key: &'static str,
  index_key: &'static str,
  done_key: &'static str,
  kind_key: Option<&'static str>,
  source: Value,
  kind: Option<f64>,
) -> Value {
  let mut iterator = ObjectData::with_prototype(Value::Object(Rc::clone(prototype)));
  iterator
    .internal_slots
    .insert(brand_key, Value::Bool(true));
  iterator.internal_slots.insert(source_key, source);
  iterator
    .internal_slots
    .insert(index_key, Value::Number(0.0));
  iterator
    .internal_slots
    .insert(done_key, Value::Bool(false));
  if let (Some(key), Some(kind)) = (kind_key, kind) {
    iterator
      .internal_slots
      .insert(key, Value::Number(kind));
  }
  Value::Object(Rc::new(RefCell::new(iterator)))
}

fn close_collection_iterator(
  vm: &mut crate::vm::Vm,
  iterator: &Value,
) -> Result<(), crate::errors::RuntimeError> {
  let return_method =
    vm.get_property_host(iterator, &PropertyKey::String("return".to_string().into()))?;
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
  let next_method =
    vm.get_property_host(&iterator, &PropertyKey::String("next".to_string().into()))?;
  if !crate::vm::Vm::is_callable_value(&next_method) {
    return Err(vm.throw_type_error("Iterator next method is not callable"));
  }

  let result = (|| {
    loop {
      let next_result = vm.call_value_with_this(&next_method, &iterator, &[])?;
      if !next_result.is_object() {
        return Err(vm.throw_type_error("Iterator result must be an object"));
      }
      let done = vm.get_property_host(
        &next_result,
        &PropertyKey::String("done".to_string().into()),
      )?;
      if vm.to_boolean(&done) {
        return Ok(());
      }
      let value = vm.get_property_host(
        &next_result,
        &PropertyKey::String("value".to_string().into()),
      )?;
      consume(vm, value)?;
    }
  })();

  match result {
    Ok(()) => Ok(()),
    Err(error) => {
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
pub(crate) struct CollectionIntrinsics {
  pub(crate) map_constructor: Value,
  pub(crate) set_constructor: Value,
  pub(crate) map_prototype: Value,
  pub(crate) set_prototype: Value,
  pub(crate) iterator_prototype: Value,
  pub(crate) map_iterator_prototype: Value,
  pub(crate) set_iterator_prototype: Value,
}

#[allow(clippy::too_many_lines)]
pub(crate) fn install_map_set_constructors(
  object_proto_map: &Rc<RefCell<ObjectData>>,
  function_proto_map: &Rc<RefCell<ObjectData>>,
) -> CollectionIntrinsics {
  use PropertyKey as PKey;

  let map_data_key = MAP_DATA_SLOT;
  let set_data_key = SET_DATA_SLOT;
  let map_iterator_brand = MAP_ITERATOR_BRAND_SLOT;
  let set_iterator_brand = SET_ITERATOR_BRAND_SLOT;
  let iterator_source_key = ITERATOR_SOURCE_SLOT;
  let iterator_index_key = ITERATOR_INDEX_SLOT;
  let iterator_done_key = ITERATOR_DONE_SLOT;
  let iterator_kind_key = ITERATOR_KIND_SLOT;

  let iterator_proto = Rc::new(RefCell::new(ObjectData::with_prototype(Value::Object(
    Rc::clone(object_proto_map),
  ))));
  iterator_proto.borrow_mut().properties.insert(
    PKey::Symbol(SymbolId::well_known_iterator()),
    PropertyDescriptor::builtin_data(method(
      Rc::new(|_vm, args| Ok(args.first().cloned().unwrap_or(Value::Undefined))),
      function_proto_map,
      "[Symbol.iterator]",
      0.0,
    )),
  );
  let map_iterator_proto = Rc::new(RefCell::new(ObjectData::with_prototype(Value::Object(
    Rc::clone(&iterator_proto),
  ))));
  let set_iterator_proto = Rc::new(RefCell::new(ObjectData::with_prototype(Value::Object(
    Rc::clone(&iterator_proto),
  ))));

  let map_brand = map_iterator_brand;
  let map_source = iterator_source_key;
  let map_index = iterator_index_key;
  let map_done = iterator_done_key;
  let map_kind = iterator_kind_key;
  let map_next = method(
    Rc::new(move |vm, args| {
      let iterator = args.first().cloned().unwrap_or(Value::Undefined);
      let valid = matches!(internal_slot(&iterator, map_brand), Some(Value::Bool(true)));
      if !valid {
        return Err(
          vm.throw_type_error("Map Iterator.prototype.next called on incompatible receiver"),
        );
      }
      let done = matches!(internal_slot(&iterator, map_done), Some(Value::Bool(true)));
      if done {
        return Ok(iterator_result(vm, Value::Undefined, true));
      }
      let Some(Value::Array(entries, _)) = internal_slot(&iterator, map_source) else {
        return Err(vm.throw_type_error("Map iterator has no map"));
      };
      let index = internal_slot(&iterator, map_index)
        .and_then(|value| vm.to_number_value(&value).ok())
        .and_then(|number| number.trunc().to_usize())
        .unwrap_or(0);
      let kind = internal_slot(&iterator, map_kind)
        .and_then(|value| vm.to_number_value(&value).ok())
        .unwrap_or(2.0);
      let mut next_index = index;
      let value = loop {
        let entry = entries
          .borrow()
          .get(next_index)
          .cloned()
          .flatten();
        let Some(entry) = entry else {
          if next_index >= entries.borrow().len() {
            debug_assert!(set_internal_slot(&iterator, map_done, Value::Bool(true)));
            debug_assert!(set_internal_slot(
              &iterator,
              map_index,
              Value::Number(usize_to_f64(next_index)),
            ));
            return Ok(iterator_result(vm, Value::Undefined, true));
          }
          next_index += 1;
          continue;
        };
        next_index += 1;
        let Some((key, entry_value)) = pair_values(&entry) else {
          continue;
        };
        break match kind as u8 {
          0 => key,
          1 => entry_value,
          _ => collection_entry_array(vm, key, entry_value),
        };
      };
      debug_assert!(set_internal_slot(
        &iterator,
        map_index,
        Value::Number(usize_to_f64(next_index)),
      ));
      Ok(iterator_result(vm, value, false))
    }),
    function_proto_map,
    "next",
    0.0,
  );
  map_iterator_proto.borrow_mut().properties.insert(
    PKey::String("next".to_string().into()),
    PropertyDescriptor::builtin_data(map_next),
  );

  map_iterator_proto.borrow_mut().properties.insert(
    PKey::Symbol(SymbolId::well_known_to_string_tag()),
    PropertyDescriptor::Data {
      value: Value::String("Map Iterator".to_string().into()),
      writable: false,
      enumerable: false,
      configurable: true,
    },
  );

  let set_brand = set_iterator_brand;
  let set_source = iterator_source_key;
  let set_index = iterator_index_key;
  let set_done = iterator_done_key;
  let set_kind = iterator_kind_key;
  let set_next = method(
    Rc::new(move |vm, args| {
      let iterator = args.first().cloned().unwrap_or(Value::Undefined);
      if !matches!(internal_slot(&iterator, set_brand), Some(Value::Bool(true))) {
        return Err(
          vm.throw_type_error("Set Iterator.prototype.next called on incompatible receiver"),
        );
      }
      if matches!(internal_slot(&iterator, set_done), Some(Value::Bool(true))) {
        return Ok(iterator_result(vm, Value::Undefined, true));
      }
      let Some(Value::Array(values, _)) = internal_slot(&iterator, set_source) else {
        return Err(vm.throw_type_error("Set iterator has no set"));
      };
      let index = internal_slot(&iterator, set_index)
        .and_then(|value| vm.to_number_value(&value).ok())
        .and_then(|number| number.trunc().to_usize())
        .unwrap_or(0);
      let kind = internal_slot(&iterator, set_kind)
        .and_then(|value| vm.to_number_value(&value).ok())
        .unwrap_or(0.0);
      let mut next_index = index;
      let value = loop {
        let item = values.borrow().get(next_index).cloned().flatten();
        let Some(item) = item else {
          if next_index >= values.borrow().len() {
            debug_assert!(set_internal_slot(&iterator, set_done, Value::Bool(true)));
            debug_assert!(set_internal_slot(
              &iterator,
              set_index,
              Value::Number(usize_to_f64(next_index)),
            ));
            return Ok(iterator_result(vm, Value::Undefined, true));
          }
          next_index += 1;
          continue;
        };
        next_index += 1;
        break if kind as u8 == 1 {
          collection_entry_array(vm, item.clone(), item)
        } else {
          item
        };
      };
      debug_assert!(set_internal_slot(
        &iterator,
        set_index,
        Value::Number(usize_to_f64(next_index)),
      ));
      Ok(iterator_result(vm, value, false))
    }),
    function_proto_map,
    "next",
    0.0,
  );
  set_iterator_proto.borrow_mut().properties.insert(
    PKey::String("next".to_string().into()),
    PropertyDescriptor::builtin_data(set_next),
  );
  set_iterator_proto.borrow_mut().properties.insert(
    PKey::Symbol(SymbolId::well_known_to_string_tag()),
    PropertyDescriptor::Data {
      value: Value::String("Set Iterator".to_string().into()),
      writable: false,
      enumerable: false,
      configurable: true,
    },
  );

  let map_proto = Rc::new(RefCell::new(ObjectData::with_prototype(Value::Object(
    Rc::clone(object_proto_map),
  ))));
  let map_ctor = Rc::new(RefCell::new(ObjectData::with_prototype(Value::Object(
    Rc::clone(function_proto_map),
  ))));
  let map_data_for_ctor = map_data_key;
  let map_ctor_fn: BuiltinFn = Rc::new(move |vm, args| {
    if vm.current_new_target().is_none() {
      return Err(vm.throw_type_error("Constructor Map requires 'new'"));
    }
    let receiver = args.first().cloned().unwrap_or(Value::Undefined);
    if !receiver.is_object() {
      return Err(vm.throw_type_error("Map receiver must be an object"));
    }
    debug_assert!(set_internal_slot(
      &receiver,
      map_data_for_ctor,
      array_from_values(Vec::new()),
    ));
    let iterable = args
      .get(static_builtin_arg_offset(vm, args, "Map"))
      .cloned()
      .unwrap_or(Value::Undefined);
    if matches!(iterable, Value::Undefined) {
      return Ok(receiver);
    }
    let adder = vm.get_property_host(&receiver, &PKey::String("set".to_string().into()))?;
    if !crate::vm::Vm::is_callable_value(&adder) {
      return Err(vm.throw_type_error("Map.prototype.set is not callable"));
    }
    consume_collection_iterable(vm, &iterable, |vm, item| {
      if !item.is_object() {
        return Err(vm.throw_type_error("Iterator value is not an entry object"));
      }
      let key = vm.get_property_host(&item, &PKey::String("0".to_string().into()))?;
      let value = vm.get_property_host(&item, &PKey::String("1".to_string().into()))?;
      drop(vm.call_value_with_this(&adder, &receiver, &[key, value])?);
      Ok(())
    })?;
    Ok(receiver)
  });
  let map_ctor_val = Value::BuiltinObject(Rc::clone(&map_ctor_fn), Rc::clone(&map_ctor));
  set_builtin_name(&map_ctor_val, "Map");
  set_builtin_length(&map_ctor_val, 0.0);
  map_ctor.borrow_mut().properties.insert(
    PKey::String("prototype".to_string().into()),
    PropertyDescriptor::Data {
      value: Value::Object(Rc::clone(&map_proto)),
      writable: false,
      enumerable: false,
      configurable: false,
    },
  );
  map_proto.borrow_mut().properties.insert(
    PKey::String("constructor".to_string().into()),
    PropertyDescriptor::builtin_data(map_ctor_val.clone()),
  );

  let map_data = map_data_key;
  let map_get = method(
    Rc::new(move |vm, args| {
      let receiver = args.first().cloned().unwrap_or(Value::Undefined);
      let entries = require_state(vm, &receiver, map_data, "get")?;
      let key = args.get(1).cloned().unwrap_or(Value::Undefined);
      if let Value::Array(entries, _) = entries {
        for entry in entries.borrow().iter().flatten() {
          if let Some((entry_key, value)) = pair_values(entry)
            && same_value_zero(&entry_key, &key)
          {
            return Ok(value);
          }
        }
      }
      Ok(Value::Undefined)
    }),
    function_proto_map,
    "get",
    1.0,
  );
  map_proto.borrow_mut().properties.insert(
    PKey::String("get".to_string().into()),
    PropertyDescriptor::builtin_data(map_get),
  );

  let map_data = map_data_key;
  let map_has = method(
    Rc::new(move |vm, args| {
      let receiver = args.first().cloned().unwrap_or(Value::Undefined);
      let entries = require_state(vm, &receiver, map_data, "has")?;
      let key = args.get(1).cloned().unwrap_or(Value::Undefined);
      if let Value::Array(entries, _) = entries {
        return Ok(Value::Bool(entries.borrow().iter().flatten().any(
          |entry| {
            pair_values(entry).is_some_and(|(entry_key, _)| same_value_zero(&entry_key, &key))
          },
        )));
      }
      Ok(Value::Bool(false))
    }),
    function_proto_map,
    "has",
    1.0,
  );
  map_proto.borrow_mut().properties.insert(
    PKey::String("has".to_string().into()),
    PropertyDescriptor::builtin_data(map_has),
  );

  let map_data = map_data_key;
  let map_delete = method(
    Rc::new(move |vm, args| {
      let receiver = args.first().cloned().unwrap_or(Value::Undefined);
      let entries = require_state(vm, &receiver, map_data, "delete")?;
      let key = args.get(1).cloned().unwrap_or(Value::Undefined);
      let Value::Array(entries, _) = entries else {
        return Ok(Value::Bool(false));
      };
      let index = entries.borrow().iter().position(|entry| {
        entry
          .as_ref()
          .and_then(pair_values)
          .is_some_and(|(entry_key, _)| same_value_zero(&entry_key, &key))
      });
      let Some(index) = index else {
        return Ok(Value::Bool(false));
      };
      entries.borrow_mut()[index] = None;
      Ok(Value::Bool(true))
    }),
    function_proto_map,
    "delete",
    1.0,
  );
  map_proto.borrow_mut().properties.insert(
    PKey::String("delete".to_string().into()),
    PropertyDescriptor::builtin_data(map_delete),
  );

  let map_data = map_data_key;
  let map_set = method(
    Rc::new(move |vm, args| {
      let receiver = args.first().cloned().unwrap_or(Value::Undefined);
      let entries = require_state(vm, &receiver, map_data, "set")?;
      let key =
        canonicalize_keyed_collection_value(args.get(1).cloned().unwrap_or(Value::Undefined));
      let value = args.get(2).cloned().unwrap_or(Value::Undefined);
      let Value::Array(entries, _) = entries else {
        return Ok(receiver);
      };
      for entry in entries.borrow_mut().iter_mut().flatten() {
        if let Some((entry_key, _)) = pair_values(entry)
          && same_value_zero(&entry_key, &key)
          && let Value::Array(pair, _) = entry
        {
          let mut pair = pair.borrow_mut();
          if pair.len() < 2 {
            pair.push(Some(value));
          } else {
            pair[1] = Some(value);
          }
          return Ok(receiver);
        }
      }
      entries
        .borrow_mut()
        .push(Some(array_from_values(vec![key, value])));
      Ok(receiver)
    }),
    function_proto_map,
    "set",
    2.0,
  );
  map_proto.borrow_mut().properties.insert(
    PKey::String("set".to_string().into()),
    PropertyDescriptor::builtin_data(map_set),
  );

  let map_data = map_data_key;
  let map_clear = method(
    Rc::new(move |vm, args| {
      let receiver = args.first().cloned().unwrap_or(Value::Undefined);
      let entries = require_state(vm, &receiver, map_data, "clear")?;
      if let Value::Array(entries, _) = entries {
        for entry in entries.borrow_mut().iter_mut() {
          *entry = None;
        }
      }
      Ok(Value::Undefined)
    }),
    function_proto_map,
    "clear",
    0.0,
  );
  map_proto.borrow_mut().properties.insert(
    PKey::String("clear".to_string().into()),
    PropertyDescriptor::builtin_data(map_clear),
  );

  let map_data = map_data_key;
  let map_for_each = method(
    Rc::new(move |vm, args| {
      let receiver = args.first().cloned().unwrap_or(Value::Undefined);
      let entries = require_state(vm, &receiver, map_data, "forEach")?;
      let callback = args.get(1).cloned().unwrap_or(Value::Undefined);
      if !crate::vm::Vm::is_callable_value(&callback) {
        return Err(vm.throw_type_error("Map.prototype.forEach callback is not callable"));
      }
      let this_arg = args.get(2).cloned().unwrap_or(Value::Undefined);
      let Value::Array(entries, _) = entries else {
        return Ok(Value::Undefined);
      };
      let mut index = 0usize;
      loop {
        let entry = entries.borrow().get(index).cloned();
        let Some(entry) = entry else {
          break;
        };
        index += 1;
        let Some(entry) = entry else {
          continue;
        };
        let Some((key, value)) = pair_values(&entry) else {
          continue;
        };
        drop(vm.call_value_with_this(&callback, &this_arg, &[value, key, receiver.clone()])?);
      }
      Ok(Value::Undefined)
    }),
    function_proto_map,
    "forEach",
    1.0,
  );
  map_proto.borrow_mut().properties.insert(
    PKey::String("forEach".to_string().into()),
    PropertyDescriptor::builtin_data(map_for_each),
  );

  let map_data = map_data_key;
  let map_size = method(
    Rc::new(move |vm, args| {
      let receiver = args.first().cloned().unwrap_or(Value::Undefined);
      let entries = require_state(vm, &receiver, map_data, "size")?;
      let size = if let Value::Array(entries, _) = entries {
        entries.borrow().iter().flatten().count()
      } else {
        0
      };
      Ok(Value::Number(usize_to_f64(size)))
    }),
    function_proto_map,
    "get size",
    0.0,
  );
  map_proto.borrow_mut().properties.insert(
    PKey::String("size".to_string().into()),
    PropertyDescriptor::Accessor {
      get: Some(map_size),
      set: None,
      enumerable: false,
      configurable: true,
    },
  );

  let map_data = map_data_key;
  let map_iterator_proto_for_methods = Rc::clone(&map_iterator_proto);
  for (name, kind) in [("keys", 0.0), ("values", 1.0)] {
    let data = map_data;
    let iterator_proto = Rc::clone(&map_iterator_proto_for_methods);
    let iterator_brand = map_iterator_brand;
    let source_key = iterator_source_key;
    let index_key = iterator_index_key;
    let done_key = iterator_done_key;
    let kind_key = iterator_kind_key;
    let function = method(
      Rc::new(move |vm, args| {
        let receiver = args.first().cloned().unwrap_or(Value::Undefined);
        let state = require_state(vm, &receiver, data, name)?;
        Ok(iterator_object(
          &iterator_proto,
          iterator_brand,
          source_key,
          index_key,
          done_key,
          Some(kind_key),
          state,
          Some(kind),
        ))
      }),
      function_proto_map,
      name,
      0.0,
    );
    map_proto.borrow_mut().properties.insert(
      PKey::String(name.to_string().into()),
      PropertyDescriptor::builtin_data(function),
    );
  }

  let map_data = map_data_key;
  let map_iterator_proto_for_entries = Rc::clone(&map_iterator_proto);
  let map_iterator_brand_for_entries = map_iterator_brand;
  let iterator_source_key_for_map_entries = iterator_source_key;
  let iterator_index_key_for_map_entries = iterator_index_key;
  let iterator_done_key_for_map_entries = iterator_done_key;
  let iterator_kind_key_for_map_entries = iterator_kind_key;
  let entries_function = method(
    Rc::new(move |vm, args| {
      let receiver = args.first().cloned().unwrap_or(Value::Undefined);
      let state = require_state(vm, &receiver, map_data, "entries")?;
      Ok(iterator_object(
        &map_iterator_proto_for_entries,
        map_iterator_brand_for_entries,
        iterator_source_key_for_map_entries,
        iterator_index_key_for_map_entries,
        iterator_done_key_for_map_entries,
        Some(iterator_kind_key_for_map_entries),
        state,
        Some(2.0),
      ))
    }),
    function_proto_map,
    "entries",
    0.0,
  );
  map_proto.borrow_mut().properties.insert(
    PKey::String("entries".to_string().into()),
    PropertyDescriptor::builtin_data(entries_function.clone()),
  );
  map_proto.borrow_mut().properties.insert(
    PKey::Symbol(SymbolId::well_known_iterator()),
    PropertyDescriptor::builtin_data(entries_function),
  );
  map_proto.borrow_mut().properties.insert(
    PKey::Symbol(SymbolId::well_known_to_string_tag()),
    PropertyDescriptor::Data {
      value: Value::String("Map".to_string().into()),
      writable: false,
      enumerable: false,
      configurable: true,
    },
  );

  let set_proto = Rc::new(RefCell::new(ObjectData::with_prototype(Value::Object(
    Rc::clone(object_proto_map),
  ))));
  let set_ctor = Rc::new(RefCell::new(ObjectData::with_prototype(Value::Object(
    Rc::clone(function_proto_map),
  ))));
  let set_data_for_ctor = set_data_key;
  let set_ctor_fn: BuiltinFn = Rc::new(move |vm, args| {
    if vm.current_new_target().is_none() {
      return Err(vm.throw_type_error("Constructor Set requires 'new'"));
    }
    let receiver = args.first().cloned().unwrap_or(Value::Undefined);
    if !receiver.is_object() {
      return Err(vm.throw_type_error("Set receiver must be an object"));
    }
    debug_assert!(set_internal_slot(
      &receiver,
      set_data_for_ctor,
      array_from_values(Vec::new()),
    ));
    let iterable = args
      .get(static_builtin_arg_offset(vm, args, "Set"))
      .cloned()
      .unwrap_or(Value::Undefined);
    if matches!(iterable, Value::Undefined) {
      return Ok(receiver);
    }
    let adder = vm.get_property_host(&receiver, &PKey::String("add".to_string().into()))?;
    if !crate::vm::Vm::is_callable_value(&adder) {
      return Err(vm.throw_type_error("Set.prototype.add is not callable"));
    }
    consume_collection_iterable(vm, &iterable, |vm, value| {
      drop(vm.call_value_with_this(&adder, &receiver, &[value])?);
      Ok(())
    })?;
    Ok(receiver)
  });
  let set_ctor_val = Value::BuiltinObject(Rc::clone(&set_ctor_fn), Rc::clone(&set_ctor));
  set_builtin_name(&set_ctor_val, "Set");
  set_builtin_length(&set_ctor_val, 0.0);
  set_ctor.borrow_mut().properties.insert(
    PKey::String("prototype".to_string().into()),
    PropertyDescriptor::Data {
      value: Value::Object(Rc::clone(&set_proto)),
      writable: false,
      enumerable: false,
      configurable: false,
    },
  );
  set_proto.borrow_mut().properties.insert(
    PKey::String("constructor".to_string().into()),
    PropertyDescriptor::builtin_data(set_ctor_val.clone()),
  );

  let set_data = set_data_key;
  let set_add = method(
    Rc::new(move |vm, args| {
      let receiver = args.first().cloned().unwrap_or(Value::Undefined);
      let values = require_state(vm, &receiver, set_data, "add")?;
      let value =
        canonicalize_keyed_collection_value(args.get(1).cloned().unwrap_or(Value::Undefined));
      let Value::Array(values, _) = values else {
        return Ok(receiver);
      };
      if !values
        .borrow()
        .iter()
        .flatten()
        .any(|item| same_value_zero(item, &value))
      {
        values.borrow_mut().push(Some(value));
      }
      Ok(receiver)
    }),
    function_proto_map,
    "add",
    1.0,
  );
  set_proto.borrow_mut().properties.insert(
    PKey::String("add".to_string().into()),
    PropertyDescriptor::builtin_data(set_add),
  );

  let set_data = set_data_key;
  let set_has = method(
    Rc::new(move |vm, args| {
      let receiver = args.first().cloned().unwrap_or(Value::Undefined);
      let values = require_state(vm, &receiver, set_data, "has")?;
      let value = args.get(1).cloned().unwrap_or(Value::Undefined);
      if let Value::Array(values, _) = values {
        return Ok(Value::Bool(
          values
            .borrow()
            .iter()
            .flatten()
            .any(|item| same_value_zero(item, &value)),
        ));
      }
      Ok(Value::Bool(false))
    }),
    function_proto_map,
    "has",
    1.0,
  );
  set_proto.borrow_mut().properties.insert(
    PKey::String("has".to_string().into()),
    PropertyDescriptor::builtin_data(set_has),
  );

  let set_data = set_data_key;
  let set_delete = method(
    Rc::new(move |vm, args| {
      let receiver = args.first().cloned().unwrap_or(Value::Undefined);
      let values = require_state(vm, &receiver, set_data, "delete")?;
      let value = args.get(1).cloned().unwrap_or(Value::Undefined);
      let Value::Array(values, _) = values else {
        return Ok(Value::Bool(false));
      };
      let index = values.borrow().iter().position(|item| {
        item
          .as_ref()
          .is_some_and(|item| same_value_zero(item, &value))
      });
      let Some(index) = index else {
        return Ok(Value::Bool(false));
      };
      values.borrow_mut()[index] = None;
      Ok(Value::Bool(true))
    }),
    function_proto_map,
    "delete",
    1.0,
  );
  set_proto.borrow_mut().properties.insert(
    PKey::String("delete".to_string().into()),
    PropertyDescriptor::builtin_data(set_delete),
  );

  let set_data = set_data_key;
  let set_clear = method(
    Rc::new(move |vm, args| {
      let receiver = args.first().cloned().unwrap_or(Value::Undefined);
      let values = require_state(vm, &receiver, set_data, "clear")?;
      if let Value::Array(values, _) = values {
        for value in values.borrow_mut().iter_mut() {
          *value = None;
        }
      }
      Ok(Value::Undefined)
    }),
    function_proto_map,
    "clear",
    0.0,
  );
  set_proto.borrow_mut().properties.insert(
    PKey::String("clear".to_string().into()),
    PropertyDescriptor::builtin_data(set_clear),
  );

  let set_data = set_data_key;
  let set_for_each = method(
    Rc::new(move |vm, args| {
      let receiver = args.first().cloned().unwrap_or(Value::Undefined);
      let values = require_state(vm, &receiver, set_data, "forEach")?;
      let callback = args.get(1).cloned().unwrap_or(Value::Undefined);
      if !crate::vm::Vm::is_callable_value(&callback) {
        return Err(vm.throw_type_error("Set.prototype.forEach callback is not callable"));
      }
      let this_arg = args.get(2).cloned().unwrap_or(Value::Undefined);
      let Value::Array(values, _) = values else {
        return Ok(Value::Undefined);
      };
      let mut index = 0usize;
      loop {
        let value = values.borrow().get(index).cloned();
        let Some(value) = value else {
          break;
        };
        index += 1;
        let Some(value) = value else {
          continue;
        };
        drop(vm.call_value_with_this(
          &callback,
          &this_arg,
          &[value.clone(), value, receiver.clone()],
        )?);
      }
      Ok(Value::Undefined)
    }),
    function_proto_map,
    "forEach",
    1.0,
  );
  set_proto.borrow_mut().properties.insert(
    PKey::String("forEach".to_string().into()),
    PropertyDescriptor::builtin_data(set_for_each),
  );

  let set_data = set_data_key;
  let set_size = method(
    Rc::new(move |vm, args| {
      let receiver = args.first().cloned().unwrap_or(Value::Undefined);
      let values = require_state(vm, &receiver, set_data, "size")?;
      let size = if let Value::Array(values, _) = values {
        values.borrow().iter().flatten().count()
      } else {
        0
      };
      Ok(Value::Number(usize_to_f64(size)))
    }),
    function_proto_map,
    "get size",
    0.0,
  );
  set_proto.borrow_mut().properties.insert(
    PKey::String("size".to_string().into()),
    PropertyDescriptor::Accessor {
      get: Some(set_size),
      set: None,
      enumerable: false,
      configurable: true,
    },
  );

  let set_data = set_data_key;
  let set_iterator_proto_for_values = Rc::clone(&set_iterator_proto);
  let set_iterator_brand_for_values = set_iterator_brand;
  let iterator_source_key_for_set_values = iterator_source_key;
  let iterator_index_key_for_set_values = iterator_index_key;
  let iterator_done_key_for_set_values = iterator_done_key;
  let iterator_kind_key_for_set_values = iterator_kind_key;
  let set_values = method(
    Rc::new(move |vm, args| {
      let receiver = args.first().cloned().unwrap_or(Value::Undefined);
      let state = require_state(vm, &receiver, set_data, "values")?;
      Ok(iterator_object(
        &set_iterator_proto_for_values,
        set_iterator_brand_for_values,
        iterator_source_key_for_set_values,
        iterator_index_key_for_set_values,
        iterator_done_key_for_set_values,
        Some(iterator_kind_key_for_set_values),
        state,
        Some(0.0),
      ))
    }),
    function_proto_map,
    "values",
    0.0,
  );
  set_proto.borrow_mut().properties.insert(
    PKey::String("values".to_string().into()),
    PropertyDescriptor::builtin_data(set_values.clone()),
  );
  set_proto.borrow_mut().properties.insert(
    PKey::String("keys".to_string().into()),
    PropertyDescriptor::builtin_data(set_values.clone()),
  );
  set_proto.borrow_mut().properties.insert(
    PKey::Symbol(SymbolId::well_known_iterator()),
    PropertyDescriptor::builtin_data(set_values),
  );

  let set_data = set_data_key;
  let set_iterator_proto_for_entries = Rc::clone(&set_iterator_proto);
  let set_iterator_brand_for_entries = set_iterator_brand;
  let iterator_source_key_for_set_entries = iterator_source_key;
  let iterator_index_key_for_set_entries = iterator_index_key;
  let iterator_done_key_for_set_entries = iterator_done_key;
  let iterator_kind_key_for_set_entries = iterator_kind_key;
  let set_entries = method(
    Rc::new(move |vm, args| {
      let receiver = args.first().cloned().unwrap_or(Value::Undefined);
      let state = require_state(vm, &receiver, set_data, "entries")?;
      Ok(iterator_object(
        &set_iterator_proto_for_entries,
        set_iterator_brand_for_entries,
        iterator_source_key_for_set_entries,
        iterator_index_key_for_set_entries,
        iterator_done_key_for_set_entries,
        Some(iterator_kind_key_for_set_entries),
        state,
        Some(1.0),
      ))
    }),
    function_proto_map,
    "entries",
    0.0,
  );
  set_proto.borrow_mut().properties.insert(
    PKey::String("entries".to_string().into()),
    PropertyDescriptor::builtin_data(set_entries),
  );
  set_proto.borrow_mut().properties.insert(
    PKey::Symbol(SymbolId::well_known_to_string_tag()),
    PropertyDescriptor::Data {
      value: Value::String("Set".to_string().into()),
      writable: false,
      enumerable: false,
      configurable: true,
    },
  );

  CollectionIntrinsics {
    map_constructor: map_ctor_val,
    set_constructor: set_ctor_val,
    map_prototype: Value::Object(map_proto),
    set_prototype: Value::Object(set_proto),
    iterator_prototype: Value::Object(iterator_proto),
    map_iterator_prototype: Value::Object(map_iterator_proto),
    set_iterator_prototype: Value::Object(set_iterator_proto),
  }
}
