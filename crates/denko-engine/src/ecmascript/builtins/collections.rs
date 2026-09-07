// SPDX-License-Identifier: MPL-2.0
//! Collection builtin functions and prototype methods

use std::cell::RefCell;
use std::rc::Rc;

use num_traits::ToPrimitive;

use crate::collection_storage::{CollectionKind, CollectionState};
use crate::runtime_utils::{
  array_from_values, builtin_object, set_builtin_length, set_builtin_name,
  static_builtin_arg_offset, usize_to_f64,
};
use crate::value::{
  BuiltinFn, ObjectData, PropertyDescriptor, PropertyKey, SymbolId, Value, WeakKey, WeakMapEntry,
};

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

fn require_weak_map_state(
  vm: &mut crate::vm::Vm,
  receiver: &Value,
  name: &str,
) -> Result<Rc<RefCell<ObjectData>>, crate::errors::RuntimeError> {
  let Some(data) = direct_object_data(receiver) else {
    return Err(vm.throw_type_error(&format!(
      "Method WeakMap.prototype.{name} called on incompatible receiver"
    )));
  };
  if data.borrow().weak_map_entries.is_none() {
    return Err(vm.throw_type_error(&format!(
      "Method WeakMap.prototype.{name} called on incompatible receiver"
    )));
  }
  Ok(Rc::clone(data))
}

fn require_weak_set_state(
  vm: &mut crate::vm::Vm,
  receiver: &Value,
  name: &str,
) -> Result<Rc<RefCell<ObjectData>>, crate::errors::RuntimeError> {
  let Some(data) = direct_object_data(receiver) else {
    return Err(vm.throw_type_error(&format!(
      "Method WeakSet.prototype.{name} called on incompatible receiver"
    )));
  };
  if data.borrow().weak_set_entries.is_none() {
    return Err(vm.throw_type_error(&format!(
      "Method WeakSet.prototype.{name} called on incompatible receiver"
    )));
  }
  Ok(Rc::clone(data))
}

fn weak_collection_key(
  vm: &mut crate::vm::Vm,
  value: &Value,
  collection: &str,
) -> Result<WeakKey, crate::errors::RuntimeError> {
  if let Value::Symbol(id, _) = value
    && crate::ecmascript::builtins::text_and_symbols::is_registered_symbol(id)
  {
    return Err(vm.throw_type_error(&format!("Invalid value used as weak {collection} key")));
  }
  WeakKey::new(value)
    .ok_or_else(|| vm.throw_type_error(&format!("Invalid value used as weak {collection} key")))
}

fn weak_map_key(
  vm: &mut crate::vm::Vm,
  value: &Value,
) -> Result<WeakKey, crate::errors::RuntimeError> {
  weak_collection_key(vm, value, "map")
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
    Value::Number(number) => Value::Number(if number == 0.0 { 0.0 } else { number }),
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

#[derive(Clone, Copy)]
struct IteratorSlots {
  brand_key: &'static str,
  source_key: &'static str,
  index_key: &'static str,
  done_key: &'static str,
  kind_key: Option<&'static str>,
}

fn iterator_object(
  prototype: &Rc<RefCell<ObjectData>>,
  slots: IteratorSlots,
  source: Value,
  kind: Option<f64>,
) -> Value {
  let mut iterator = ObjectData::with_prototype(Value::Object(Rc::clone(prototype)));
  iterator
    .internal_slots
    .insert(slots.brand_key, Value::Bool(true));
  iterator
    .internal_slots
    .insert(slots.source_key, source);
  iterator
    .internal_slots
    .insert(slots.index_key, Value::Number(0.0));
  iterator
    .internal_slots
    .insert(slots.done_key, Value::Bool(false));
  if let (Some(key), Some(kind)) = (slots.kind_key, kind) {
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
pub(crate) fn consume_collection_iterable(
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
  pub(crate) weak_map_constructor: Value,
  pub(crate) set_constructor: Value,
  pub(crate) weak_set_constructor: Value,
  pub(crate) map_prototype: Value,
  pub(crate) weak_map_prototype: Value,
  pub(crate) set_prototype: Value,
  pub(crate) weak_set_prototype: Value,
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
            let did_set_done = set_internal_slot(&iterator, map_done, Value::Bool(true));
            debug_assert!(did_set_done);
            let did_set_index = set_internal_slot(
              &iterator,
              map_index,
              Value::Number(usize_to_f64(next_index)),
            );
            debug_assert!(did_set_index);
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
      let did_set_index = set_internal_slot(
        &iterator,
        map_index,
        Value::Number(usize_to_f64(next_index)),
      );
      debug_assert!(did_set_index);
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
            let did_set_done = set_internal_slot(&iterator, set_done, Value::Bool(true));
            debug_assert!(did_set_done);
            let did_set_index = set_internal_slot(
              &iterator,
              set_index,
              Value::Number(usize_to_f64(next_index)),
            );
            debug_assert!(did_set_index);
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
      let did_set_index = set_internal_slot(
        &iterator,
        set_index,
        Value::Number(usize_to_f64(next_index)),
      );
      debug_assert!(did_set_index);
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
    if vm.pending_constructor_new_target().is_none() {
      return Err(vm.throw_type_error("Constructor Map requires 'new'"));
    }
    let receiver = args.first().cloned().unwrap_or(Value::Undefined);
    if !receiver.is_object() {
      return Err(vm.throw_type_error("Map receiver must be an object"));
    }
    let did_initialize =
      set_internal_slot(&receiver, map_data_for_ctor, array_from_values(Vec::new()));
    debug_assert!(did_initialize);
    let iterable = args
      .get(static_builtin_arg_offset(vm, args, "Map"))
      .cloned()
      .unwrap_or(Value::Undefined);
    if matches!(iterable, Value::Undefined | Value::Null) {
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
  let map_species = method(
    Rc::new(|_vm, args| Ok(args.first().cloned().unwrap_or(Value::Undefined))),
    function_proto_map,
    "get [Symbol.species]",
    0.0,
  );
  map_ctor.borrow_mut().properties.insert(
    PKey::Symbol(SymbolId::well_known_species()),
    PropertyDescriptor::Accessor {
      get: Some(map_species),
      set: None,
      enumerable: false,
      configurable: true,
    },
  );

  let map_group_by_proto = Rc::clone(&map_proto);
  let map_group_by_data = map_data_key;
  let map_group_by = method(
    Rc::new(move |vm, args| {
      let off = static_builtin_arg_offset(vm, args, "Map");
      let items = args.get(off).cloned().unwrap_or(Value::Undefined);
      let callback = args
        .get(off + 1)
        .cloned()
        .unwrap_or(Value::Undefined);
      if !crate::vm::Vm::is_callable_value(&callback) {
        return Err(vm.throw_type_error("Map.groupBy callback is not callable"));
      }

      let groups = CollectionState::new(CollectionKind::Map);
      let groups_root = vm.create_host_root(groups.value());
      let mut index = 0usize;
      let grouped = consume_collection_iterable(vm, &items, |vm, item| {
        let key = vm.call_value_with_this(
          &callback,
          &Value::Undefined,
          &[item.clone(), Value::Number(usize_to_f64(index))],
        )?;
        index = index.saturating_add(1);
        if let Some(Value::Array(values, properties)) = groups.get(&key) {
          // Groups are private until iteration finishes, so no observable
          // property hooks are involved in appending their next item.
          let length = {
            let mut values = values.borrow_mut();
            values.push(Some(item));
            values.len()
          };
          properties.borrow_mut().properties.insert(
            PropertyKey::builtin_length(),
            crate::runtime_utils::array_length_descriptor(length),
          );
        } else {
          groups.set(key, array_from_values(vec![item]));
        }
        Ok(())
      });
      vm.release_host_root(groups_root)
        .expect("Map.groupBy groups root");
      grouped?;

      let result = Value::Object(Rc::new(RefCell::new(ObjectData::with_prototype(
        Value::Object(Rc::clone(&map_group_by_proto)),
      ))));
      let did_initialize = set_internal_slot(&result, map_group_by_data, groups.value());
      debug_assert!(did_initialize);
      Ok(result)
    }),
    function_proto_map,
    "groupBy",
    2.0,
  );
  map_ctor.borrow_mut().properties.insert(
    PKey::String("groupBy".to_string().into()),
    PropertyDescriptor::builtin_data(map_group_by),
  );

  let map_data = map_data_key;
  let map_get = method(
    Rc::new(move |vm, args| {
      let receiver = args.first().cloned().unwrap_or(Value::Undefined);
      let state = CollectionState::from_value(
        require_state(vm, &receiver, map_data, "get")?,
        CollectionKind::Map,
      );
      let key = args.get(1).cloned().unwrap_or(Value::Undefined);
      Ok(state.get(&key).unwrap_or(Value::Undefined))
    }),
    function_proto_map,
    "get",
    1.0,
  );
  map_proto.borrow_mut().properties.insert(
    PKey::String("get".to_string().into()),
    PropertyDescriptor::builtin_data(map_get),
  );

  for (name, computed) in [("getOrInsert", false), ("getOrInsertComputed", true)] {
    let map_data = map_data_key;
    let upsert = method(
      Rc::new(move |vm, args| {
        let receiver = args.first().cloned().unwrap_or(Value::Undefined);
        let state = CollectionState::from_value(
          require_state(vm, &receiver, map_data, name)?,
          CollectionKind::Map,
        );
        let value_or_callback = args.get(2).cloned().unwrap_or(Value::Undefined);
        if computed && !crate::vm::Vm::is_callable_value(&value_or_callback) {
          return Err(
            vm.throw_type_error("Map.prototype.getOrInsertComputed callback is not callable"),
          );
        }
        let key =
          canonicalize_keyed_collection_value(args.get(1).cloned().unwrap_or(Value::Undefined));
        if let Some(value) = state.get(&key) {
          return Ok(value);
        }
        let value = if computed {
          vm.call_value_with_this(
            &value_or_callback,
            &Value::Undefined,
            std::slice::from_ref(&key),
          )?
        } else {
          value_or_callback
        };
        // Recheck after the callback: it may have inserted, deleted, or cleared.
        state.set(key, value.clone());
        Ok(value)
      }),
      function_proto_map,
      name,
      2.0,
    );
    map_proto.borrow_mut().properties.insert(
      PKey::String(name.into()),
      PropertyDescriptor::builtin_data(upsert),
    );
  }

  let map_data = map_data_key;
  let map_has = method(
    Rc::new(move |vm, args| {
      let receiver = args.first().cloned().unwrap_or(Value::Undefined);
      let state = CollectionState::from_value(
        require_state(vm, &receiver, map_data, "has")?,
        CollectionKind::Map,
      );
      let key = args.get(1).cloned().unwrap_or(Value::Undefined);
      Ok(Value::Bool(state.has(&key)))
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
      let state = CollectionState::from_value(
        require_state(vm, &receiver, map_data, "delete")?,
        CollectionKind::Map,
      );
      let key = args.get(1).cloned().unwrap_or(Value::Undefined);
      Ok(Value::Bool(state.delete(&key)))
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
      let state = CollectionState::from_value(
        require_state(vm, &receiver, map_data, "set")?,
        CollectionKind::Map,
      );
      let key = args.get(1).cloned().unwrap_or(Value::Undefined);
      let value = args.get(2).cloned().unwrap_or(Value::Undefined);
      state.set(key, value);
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
      let state = CollectionState::from_value(
        require_state(vm, &receiver, map_data, "clear")?,
        CollectionKind::Map,
      );
      state.clear();
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
      let state = CollectionState::from_value(
        require_state(vm, &receiver, map_data, "size")?,
        CollectionKind::Map,
      );
      Ok(Value::Number(usize_to_f64(state.len())))
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
          IteratorSlots {
            brand_key: iterator_brand,
            source_key,
            index_key,
            done_key,
            kind_key: Some(kind_key),
          },
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
        IteratorSlots {
          brand_key: map_iterator_brand_for_entries,
          source_key: iterator_source_key_for_map_entries,
          index_key: iterator_index_key_for_map_entries,
          done_key: iterator_done_key_for_map_entries,
          kind_key: Some(iterator_kind_key_for_map_entries),
        },
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

  let weak_map_proto = Rc::new(RefCell::new(ObjectData::with_prototype(Value::Object(
    Rc::clone(object_proto_map),
  ))));
  let weak_map_ctor = Rc::new(RefCell::new(ObjectData::with_prototype(Value::Object(
    Rc::clone(function_proto_map),
  ))));
  let weak_map_ctor_fn: BuiltinFn = Rc::new(move |vm, args| {
    if vm.pending_constructor_new_target().is_none() {
      return Err(vm.throw_type_error("Constructor WeakMap requires 'new'"));
    }
    let receiver = args.first().cloned().unwrap_or(Value::Undefined);
    let Some(data) = direct_object_data(&receiver) else {
      return Err(vm.throw_type_error("WeakMap receiver must be an object"));
    };
    data.borrow_mut().weak_map_entries = Some(Vec::new());
    let iterable = args
      .get(static_builtin_arg_offset(vm, args, "WeakMap"))
      .cloned()
      .unwrap_or(Value::Undefined);
    if matches!(iterable, Value::Undefined | Value::Null) {
      return Ok(receiver);
    }
    let receiver_root = vm.create_host_root(receiver.clone());
    let result = (|| {
      let adder = vm.get_property_host(&receiver, &PKey::String("set".to_string().into()))?;
      if !crate::vm::Vm::is_callable_value(&adder) {
        return Err(vm.throw_type_error("WeakMap.prototype.set is not callable"));
      }
      consume_collection_iterable(vm, &iterable, |vm, item| {
        if !item.is_object() {
          return Err(vm.throw_type_error("Iterator value is not an entry object"));
        }
        let key = vm.get_property_host(&item, &PKey::String("0".to_string().into()))?;
        let value = vm.get_property_host(&item, &PKey::String("1".to_string().into()))?;
        drop(vm.call_value_with_this(&adder, &receiver, &[key, value])?);
        Ok(())
      })
    })();
    vm.release_host_root(receiver_root)
      .expect("WeakMap constructor receiver root");
    result?;
    Ok(receiver)
  });
  let weak_map_ctor_val =
    Value::BuiltinObject(Rc::clone(&weak_map_ctor_fn), Rc::clone(&weak_map_ctor));
  set_builtin_name(&weak_map_ctor_val, "WeakMap");
  set_builtin_length(&weak_map_ctor_val, 0.0);
  weak_map_ctor.borrow_mut().properties.insert(
    PKey::String("prototype".to_string().into()),
    PropertyDescriptor::Data {
      value: Value::Object(Rc::clone(&weak_map_proto)),
      writable: false,
      enumerable: false,
      configurable: false,
    },
  );
  weak_map_proto.borrow_mut().properties.insert(
    PKey::String("constructor".to_string().into()),
    PropertyDescriptor::builtin_data(weak_map_ctor_val.clone()),
  );

  let weak_map_delete = method(
    Rc::new(move |vm, args| {
      let receiver = args.first().cloned().unwrap_or(Value::Undefined);
      let data = require_weak_map_state(vm, &receiver, "delete")?;
      let key = args.get(1).cloned().unwrap_or(Value::Undefined);
      let mut data = data.borrow_mut();
      let entries = data
        .weak_map_entries
        .as_mut()
        .expect("validated WeakMap data");
      let Some(index) = entries
        .iter()
        .position(|entry| entry.key.matches(&key))
      else {
        return Ok(Value::Bool(false));
      };
      entries.swap_remove(index);
      Ok(Value::Bool(true))
    }),
    function_proto_map,
    "delete",
    1.0,
  );
  weak_map_proto.borrow_mut().properties.insert(
    PKey::String("delete".to_string().into()),
    PropertyDescriptor::builtin_data(weak_map_delete),
  );

  let weak_map_get = method(
    Rc::new(move |vm, args| {
      let receiver = args.first().cloned().unwrap_or(Value::Undefined);
      let data = require_weak_map_state(vm, &receiver, "get")?;
      let key = args.get(1).cloned().unwrap_or(Value::Undefined);
      let data = data.borrow();
      let entries = data
        .weak_map_entries
        .as_ref()
        .expect("validated WeakMap data");
      Ok(
        entries
          .iter()
          .find(|entry| entry.key.matches(&key))
          .map_or(Value::Undefined, |entry| entry.value.clone()),
      )
    }),
    function_proto_map,
    "get",
    1.0,
  );
  weak_map_proto.borrow_mut().properties.insert(
    PKey::String("get".to_string().into()),
    PropertyDescriptor::builtin_data(weak_map_get),
  );

  let weak_map_has = method(
    Rc::new(move |vm, args| {
      let receiver = args.first().cloned().unwrap_or(Value::Undefined);
      let data = require_weak_map_state(vm, &receiver, "has")?;
      let key = args.get(1).cloned().unwrap_or(Value::Undefined);
      let data = data.borrow();
      let entries = data
        .weak_map_entries
        .as_ref()
        .expect("validated WeakMap data");
      Ok(Value::Bool(
        entries
          .iter()
          .any(|entry| entry.key.matches(&key)),
      ))
    }),
    function_proto_map,
    "has",
    1.0,
  );
  weak_map_proto.borrow_mut().properties.insert(
    PKey::String("has".to_string().into()),
    PropertyDescriptor::builtin_data(weak_map_has),
  );

  let weak_map_set = method(
    Rc::new(move |vm, args| {
      let receiver = args.first().cloned().unwrap_or(Value::Undefined);
      let data = require_weak_map_state(vm, &receiver, "set")?;
      let key = args.get(1).cloned().unwrap_or(Value::Undefined);
      let weak_key = weak_map_key(vm, &key)?;
      let value = args.get(2).cloned().unwrap_or(Value::Undefined);
      let mut data = data.borrow_mut();
      let entries = data
        .weak_map_entries
        .as_mut()
        .expect("validated WeakMap data");
      if let Some(entry) = entries
        .iter_mut()
        .find(|entry| entry.key.matches(&key))
      {
        entry.value = value;
      } else {
        entries.push(WeakMapEntry {
          key: weak_key,
          value,
        });
      }
      Ok(receiver)
    }),
    function_proto_map,
    "set",
    2.0,
  );
  weak_map_proto.borrow_mut().properties.insert(
    PKey::String("set".to_string().into()),
    PropertyDescriptor::builtin_data(weak_map_set),
  );

  let weak_map_get_or_insert = method(
    Rc::new(move |vm, args| {
      let receiver = args.first().cloned().unwrap_or(Value::Undefined);
      let data = require_weak_map_state(vm, &receiver, "getOrInsert")?;
      let key = args.get(1).cloned().unwrap_or(Value::Undefined);
      let weak_key = weak_map_key(vm, &key)?;
      let value = args.get(2).cloned().unwrap_or(Value::Undefined);
      let mut data = data.borrow_mut();
      let entries = data
        .weak_map_entries
        .as_mut()
        .expect("validated WeakMap data");
      if let Some(entry) = entries
        .iter()
        .find(|entry| entry.key.matches(&key))
      {
        return Ok(entry.value.clone());
      }
      entries.push(WeakMapEntry {
        key: weak_key,
        value: value.clone(),
      });
      Ok(value)
    }),
    function_proto_map,
    "getOrInsert",
    2.0,
  );
  weak_map_proto.borrow_mut().properties.insert(
    PKey::String("getOrInsert".to_string().into()),
    PropertyDescriptor::builtin_data(weak_map_get_or_insert),
  );

  let weak_map_get_or_insert_computed = method(
    Rc::new(move |vm, args| {
      let receiver = args.first().cloned().unwrap_or(Value::Undefined);
      let data = require_weak_map_state(vm, &receiver, "getOrInsertComputed")?;
      let key = args.get(1).cloned().unwrap_or(Value::Undefined);
      let callback = args.get(2).cloned().unwrap_or(Value::Undefined);
      if !crate::vm::Vm::is_callable_value(&callback) {
        return Err(
          vm.throw_type_error("WeakMap.prototype.getOrInsertComputed callback is not callable"),
        );
      }
      let weak_key = weak_map_key(vm, &key)?;
      {
        let data = data.borrow();
        let entries = data
          .weak_map_entries
          .as_ref()
          .expect("validated WeakMap data");
        if let Some(entry) = entries
          .iter()
          .find(|entry| entry.key.matches(&key))
        {
          return Ok(entry.value.clone());
        }
      }
      let value =
        vm.call_value_with_this(&callback, &Value::Undefined, std::slice::from_ref(&key))?;
      let mut data = data.borrow_mut();
      let entries = data
        .weak_map_entries
        .as_mut()
        .expect("validated WeakMap data");
      if let Some(entry) = entries
        .iter_mut()
        .find(|entry| entry.key.matches(&key))
      {
        entry.value = value.clone();
      } else {
        entries.push(WeakMapEntry {
          key: weak_key,
          value: value.clone(),
        });
      }
      Ok(value)
    }),
    function_proto_map,
    "getOrInsertComputed",
    2.0,
  );
  weak_map_proto.borrow_mut().properties.insert(
    PKey::String("getOrInsertComputed".to_string().into()),
    PropertyDescriptor::builtin_data(weak_map_get_or_insert_computed),
  );

  weak_map_proto.borrow_mut().properties.insert(
    PKey::Symbol(SymbolId::well_known_to_string_tag()),
    PropertyDescriptor::Data {
      value: Value::String("WeakMap".to_string().into()),
      writable: false,
      enumerable: false,
      configurable: true,
    },
  );

  let weak_set_proto = Rc::new(RefCell::new(ObjectData::with_prototype(Value::Object(
    Rc::clone(object_proto_map),
  ))));
  let weak_set_ctor = Rc::new(RefCell::new(ObjectData::with_prototype(Value::Object(
    Rc::clone(function_proto_map),
  ))));
  let weak_set_ctor_fn: BuiltinFn = Rc::new(move |vm, args| {
    if vm.pending_constructor_new_target().is_none() {
      return Err(vm.throw_type_error("Constructor WeakSet requires 'new'"));
    }
    let receiver = args.first().cloned().unwrap_or(Value::Undefined);
    let Some(data) = direct_object_data(&receiver) else {
      return Err(vm.throw_type_error("WeakSet receiver must be an object"));
    };
    data.borrow_mut().weak_set_entries = Some(Vec::new());
    let iterable = args
      .get(static_builtin_arg_offset(vm, args, "WeakSet"))
      .cloned()
      .unwrap_or(Value::Undefined);
    if matches!(iterable, Value::Undefined | Value::Null) {
      return Ok(receiver);
    }
    let receiver_root = vm.create_host_root(receiver.clone());
    let result = (|| {
      let adder = vm.get_property_host(&receiver, &PKey::String("add".to_string().into()))?;
      if !crate::vm::Vm::is_callable_value(&adder) {
        return Err(vm.throw_type_error("WeakSet.prototype.add is not callable"));
      }
      consume_collection_iterable(vm, &iterable, |vm, value| {
        drop(vm.call_value_with_this(&adder, &receiver, &[value])?);
        Ok(())
      })
    })();
    vm.release_host_root(receiver_root)
      .expect("WeakSet constructor receiver root");
    result?;
    Ok(receiver)
  });
  let weak_set_ctor_val =
    Value::BuiltinObject(Rc::clone(&weak_set_ctor_fn), Rc::clone(&weak_set_ctor));
  set_builtin_name(&weak_set_ctor_val, "WeakSet");
  set_builtin_length(&weak_set_ctor_val, 0.0);
  weak_set_ctor.borrow_mut().properties.insert(
    PKey::String("prototype".to_string().into()),
    PropertyDescriptor::Data {
      value: Value::Object(Rc::clone(&weak_set_proto)),
      writable: false,
      enumerable: false,
      configurable: false,
    },
  );
  weak_set_proto.borrow_mut().properties.insert(
    PKey::String("constructor".to_string().into()),
    PropertyDescriptor::builtin_data(weak_set_ctor_val.clone()),
  );
  for (name, operation) in [("delete", 0_u8), ("has", 1)] {
    let function = method(
      Rc::new(move |vm, args| {
        let receiver = args.first().cloned().unwrap_or(Value::Undefined);
        let data = require_weak_set_state(vm, &receiver, name)?;
        let key = args.get(1).cloned().unwrap_or(Value::Undefined);
        let mut data = data.borrow_mut();
        let entries = data
          .weak_set_entries
          .as_mut()
          .expect("validated WeakSet data");
        if operation == 1 {
          return Ok(Value::Bool(entries.iter().any(|entry| entry.matches(&key))));
        }
        let Some(index) = entries
          .iter()
          .position(|entry| entry.matches(&key))
        else {
          return Ok(Value::Bool(false));
        };
        entries.swap_remove(index);
        Ok(Value::Bool(true))
      }),
      function_proto_map,
      name,
      1.0,
    );
    weak_set_proto.borrow_mut().properties.insert(
      PKey::String(name.to_string().into()),
      PropertyDescriptor::builtin_data(function),
    );
  }
  let weak_set_add = method(
    Rc::new(move |vm, args| {
      let receiver = args.first().cloned().unwrap_or(Value::Undefined);
      let data = require_weak_set_state(vm, &receiver, "add")?;
      let key = args.get(1).cloned().unwrap_or(Value::Undefined);
      let weak_key = weak_collection_key(vm, &key, "set")?;
      let mut data = data.borrow_mut();
      let entries = data
        .weak_set_entries
        .as_mut()
        .expect("validated WeakSet data");
      if !entries.iter().any(|entry| entry.matches(&key)) {
        entries.push(weak_key);
      }
      Ok(receiver)
    }),
    function_proto_map,
    "add",
    1.0,
  );
  weak_set_proto.borrow_mut().properties.insert(
    PKey::String("add".to_string().into()),
    PropertyDescriptor::builtin_data(weak_set_add),
  );
  weak_set_proto.borrow_mut().properties.insert(
    PKey::Symbol(SymbolId::well_known_to_string_tag()),
    PropertyDescriptor::Data {
      value: Value::String("WeakSet".to_string().into()),
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
    if vm.pending_constructor_new_target().is_none() {
      return Err(vm.throw_type_error("Constructor Set requires 'new'"));
    }
    let receiver = args.first().cloned().unwrap_or(Value::Undefined);
    if !receiver.is_object() {
      return Err(vm.throw_type_error("Set receiver must be an object"));
    }
    let did_initialize =
      set_internal_slot(&receiver, set_data_for_ctor, array_from_values(Vec::new()));
    debug_assert!(did_initialize);
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
  let set_species = method(
    Rc::new(|_vm, args| Ok(args.first().cloned().unwrap_or(Value::Undefined))),
    function_proto_map,
    "get [Symbol.species]",
    0.0,
  );
  set_ctor.borrow_mut().properties.insert(
    PKey::Symbol(SymbolId::well_known_species()),
    PropertyDescriptor::Accessor {
      get: Some(set_species),
      set: None,
      enumerable: false,
      configurable: true,
    },
  );

  let set_data = set_data_key;
  let set_add = method(
    Rc::new(move |vm, args| {
      let receiver = args.first().cloned().unwrap_or(Value::Undefined);
      let state = CollectionState::from_value(
        require_state(vm, &receiver, set_data, "add")?,
        CollectionKind::Set,
      );
      let key = args.get(1).cloned().unwrap_or(Value::Undefined);
      state.add(key);
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
      let state = CollectionState::from_value(
        require_state(vm, &receiver, set_data, "has")?,
        CollectionKind::Set,
      );
      let key = args.get(1).cloned().unwrap_or(Value::Undefined);
      Ok(Value::Bool(state.has(&key)))
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
      let state = CollectionState::from_value(
        require_state(vm, &receiver, set_data, "delete")?,
        CollectionKind::Set,
      );
      let key = args.get(1).cloned().unwrap_or(Value::Undefined);
      Ok(Value::Bool(state.delete(&key)))
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
      let state = CollectionState::from_value(
        require_state(vm, &receiver, set_data, "clear")?,
        CollectionKind::Set,
      );
      state.clear();
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
      let state = CollectionState::from_value(
        require_state(vm, &receiver, set_data, "size")?,
        CollectionKind::Set,
      );
      Ok(Value::Number(usize_to_f64(state.len())))
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
        IteratorSlots {
          brand_key: set_iterator_brand_for_values,
          source_key: iterator_source_key_for_set_values,
          index_key: iterator_index_key_for_set_values,
          done_key: iterator_done_key_for_set_values,
          kind_key: Some(iterator_kind_key_for_set_values),
        },
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
        IteratorSlots {
          brand_key: set_iterator_brand_for_entries,
          source_key: iterator_source_key_for_set_entries,
          index_key: iterator_index_key_for_set_entries,
          done_key: iterator_done_key_for_set_entries,
          kind_key: Some(iterator_kind_key_for_set_entries),
        },
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
    weak_map_constructor: weak_map_ctor_val,
    set_constructor: set_ctor_val,
    weak_set_constructor: weak_set_ctor_val,
    map_prototype: Value::Object(map_proto),
    weak_map_prototype: Value::Object(weak_map_proto),
    set_prototype: Value::Object(set_proto),
    weak_set_prototype: Value::Object(weak_set_proto),
    iterator_prototype: Value::Object(iterator_proto),
    map_iterator_prototype: Value::Object(map_iterator_proto),
    set_iterator_prototype: Value::Object(set_iterator_proto),
  }
}
