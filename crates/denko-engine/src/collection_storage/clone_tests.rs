// SPDX-License-Identifier: MPL-2.0
use super::*;
use crate::collection_storage::{CollectionKind, CollectionState};
use crate::ecmascript::builtins::collections::{MAP_DATA_SLOT, SET_DATA_SLOT};

#[test]
fn indexed_collection_clone_rebuilds_identity_hashes_and_preserves_cycles() {
  let globals = Rc::new(RefCell::new(ObjectData::new()));
  install_minimal_kernel_constructors(&globals);
  let mut vm = Vm::new(globals);
  let map = Rc::new(RefCell::new(ObjectData::new()));
  let set = Rc::new(RefCell::new(ObjectData::new()));
  let key = Value::Object(Rc::new(RefCell::new(ObjectData::new())));
  let source_map = CollectionState::new(CollectionKind::Map);
  let source_set = CollectionState::new(CollectionKind::Set);
  map
    .borrow_mut()
    .internal_slots
    .insert(MAP_DATA_SLOT, source_map.value());
  set
    .borrow_mut()
    .internal_slots
    .insert(SET_DATA_SLOT, source_set.value());
  source_map.set(key.clone(), Value::Object(Rc::clone(&set)));
  source_map.set(Value::Object(Rc::clone(&map)), key.clone());
  source_set.add(key.clone());
  source_set.add(Value::Object(Rc::clone(&map)));
  assert!(source_map.has(&key));
  assert!(source_set.has(&key));
  let cloned = clone_value_across_realms(&mut vm, &Value::Object(Rc::clone(&map)), None)
    .expect("clone indexed cyclic map/set");
  let Value::Object(cloned_object) = &cloned else {
    panic!("cloned map")
  };
  let cloned_map = CollectionState::from_value(
    cloned_object
      .borrow()
      .internal_slots
      .get(MAP_DATA_SLOT)
      .expect("Map data")
      .clone(),
    CollectionKind::Map,
  );
  assert_eq!(cloned_map.len(), 2);
  assert!(!cloned_map.has(&key));
  let cloned_key = cloned_map
    .get(&cloned)
    .expect("self key maps to cloned object key");
  assert!(!strict_equal(&cloned_key, &key));
  let Value::Object(cloned_set_object) = cloned_map.get(&cloned_key).expect("cloned set") else {
    panic!("cloned set object");
  };
  let cloned_set = CollectionState::from_value(
    cloned_set_object
      .borrow()
      .internal_slots
      .get(SET_DATA_SLOT)
      .expect("Set data")
      .clone(),
    CollectionKind::Set,
  );
  assert_eq!(cloned_set.len(), 2);
  assert!(cloned_set.has(&cloned_key));
  assert!(cloned_set.has(&cloned));
  assert!(!cloned_set.has(&key));
  assert!(cloned_map.delete(&cloned_key));
  cloned_set.clear();
  assert_eq!(cloned_map.len(), 1);
  assert_eq!(cloned_set.len(), 0);
  assert!(source_map.has(&key));
  assert!(source_set.has(&key));
  assert_eq!(source_map.len(), 2);
  assert_eq!(source_set.len(), 2);
}
