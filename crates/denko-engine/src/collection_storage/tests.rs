// SPDX-License-Identifier: MPL-2.0
use super::*;
use crate::value::{PropertyDescriptor, PropertyKey};
use crate::{Vm, compiler, parser::Parser};

fn vm() -> Vm {
  let globals = Rc::new(RefCell::new(ObjectData::new()));
  globals.borrow_mut().properties.insert(
    PropertyKey::String("globalThis".into()),
    PropertyDescriptor::ordinary_data(Value::Object(Rc::clone(&globals))),
  );
  crate::runtime_utils::install_minimal_kernel_constructors(&globals);
  globals.borrow_mut().properties.insert(
    PropertyKey::String("collectNow".into()),
    PropertyDescriptor::ordinary_data(Value::Builtin(Rc::new(|vm, _| {
      vm.collect_garbage_prototype();
      Ok(Value::Undefined)
    }))),
  );
  Vm::new(globals)
}

fn run(
  vm: &mut Vm,
  source: &str,
) {
  let session = Parser::parse_source(source).expect("parse collection regression");
  let code = compiler::compile_session(&session).expect("compile collection regression");
  vm.run(code).expect("collection regression");
}

#[test]
fn hash_agrees_with_same_value_zero_for_all_observable_value_kinds() {
  let mut vm = vm();
  run(
    &mut vm,
    r#"
    var object = {}, array = [], proxy = new Proxy({}, {}), closure = () => 1;
    var promise = Promise.resolve(1), symbol = Symbol('same');
    var samples = [undefined, null, true, false, NaN, -NaN, 0, -0, 1, -1,
      Infinity, -Infinity, 9007199254740992, 1n, 999999999999999999999999n,
      '', 'text', 'te' + 'xt', '\ud800', '\ud800', symbol, symbol, Symbol('same'),
      object, object, {}, array, array, [], proxy, proxy, new Proxy(object, {}),
      closure, closure, () => 1, Map, Map, Set, promise, promise, Promise.resolve(1)];
  "#,
  );
  let Value::Array(values, _) = vm.get_global("samples").expect("samples") else {
    panic!("sample array");
  };
  let mut samples = values
    .borrow()
    .iter()
    .flatten()
    .cloned()
    .collect::<Vec<_>>();
  samples.push(Value::Builtin(Rc::new(|_, _| Ok(Value::Undefined))));
  samples.push(samples.last().expect("native function").clone());
  samples.push(Value::Uninitialized);
  let hashes = RandomState::new();
  let state = CollectionState::new(CollectionKind::Map);
  for (i, key) in samples.iter().enumerate() {
    for other in &samples {
      if same_value_zero(key, other) {
        assert_eq!(hash_key(key, &hashes), hash_key(other, &hashes));
      }
    }
    if !matches!(key, Value::Uninitialized) {
      state.set(key.clone(), Value::Number(i as f64));
    }
  }
  for key in samples
    .iter()
    .filter(|key| !matches!(key, Value::Uninitialized))
  {
    let expected = samples
      .iter()
      .rposition(|other| same_value_zero(key, other))
      .expect("equal key");
    assert_eq!(state.get(key), Some(Value::Number(expected as f64)));
  }
}

#[test]
fn hash_collisions_compare_keys_and_skip_other_candidates() {
  let state = CollectionState::new(CollectionKind::Map);
  state.set(Value::String("first".into()), Value::Number(1.0));
  state.set(Value::String("second".into()), Value::Number(2.0));
  let mut index = CollectionIndex {
    initialized: true,
    kind: CollectionKind::Map,
    hashes: RandomState::new(),
    positions: HashMap::from([(0, vec![0, 1])]),
    len: 2,
  };
  let elements = state.elements.borrow();
  assert_eq!(
    CollectionState::find(&elements, &index, 0, &Value::String("second".into())),
    Some(1)
  );
  assert_eq!(
    CollectionState::find(&elements, &index, 0, &Value::String("missing".into())),
    None
  );
  index
    .positions
    .get_mut(&0)
    .expect("collision bucket")
    .swap_remove(0);
  assert_eq!(
    CollectionState::find(&elements, &index, 0, &Value::String("second".into())),
    Some(1)
  );
}

#[test]
fn cloned_property_metadata_rebuilds_lookup_from_new_slots() {
  let original = CollectionState::new(CollectionKind::Set);
  original.add(Value::String("old".into()));
  let properties = Rc::new(RefCell::new(original.properties.borrow().clone()));
  let cloned = CollectionState::from_value(
    Value::Array(
      Rc::new(RefCell::new(ArrayData::from(vec![Some(Value::String(
        "new".into(),
      ))]))),
      properties,
    ),
    CollectionKind::Set,
  );
  assert!(cloned.has(&Value::String("new".into())));
  assert!(!cloned.has(&Value::String("old".into())));
  assert_eq!(cloned.len(), 1);
  assert!(original.has(&Value::String("old".into())));
}

#[test]
fn indexed_collections_keep_live_iteration_and_reentrant_mutation_order() {
  let mut vm = vm();
  run(
    &mut vm,
    r#"
    function check(value) { if (!value) throw new Error('indexed collection regression'); }
    for (const C of [Map, Set]) {
      const c = new C();
      const add = key => C === Map ? c.set(key, key * 10) : c.add(key);
      add(1); add(2); add(3); add(3);
      check(c.size === 3);
      const first = c.keys(), second = c.keys();
      check(first.next().value === 1);
      check(c.delete(2) && !c.delete(2));
      c.delete(1); add(1);
      check(first.next().value === 3 && first.next().value === 1);
      check(second.next().value === 3);
      c.clear(); check(c.size === 0); add(4);
      check(first.next().value === 4 && second.next().value === 4);
      check(first.next().done); add(5); check(first.next().done);
      check(second.next().value === 5 && second.next().done);
      c.clear(); add(1); add(2);
      let visited = [];
      c.forEach((value, key) => {
        visited.push(key);
        if (key === 1) { c.clear(); add(3); }
        if (key === 3) { c.delete(3); add(4); }
      });
      check(visited.join(',') === '1,3,4' && c.size === 1);
    }
    const m = new Map([['before', 0]]);
    const value = m.getOrInsertComputed('key', key => {
      m.clear(); m.set(key, 1); m.delete(key); m.set('other', 2); m.set(key, 3);
      return 4;
    });
    check(value === 4 && m.get('key') === 4 && m.size === 2);
    check(Array.from(m.keys()).join(',') === 'other,key');
    check(m.getOrInsert('key', 5) === 4);
    const z = new Map(); z.set(-0, 1); z.set(0, 2); z.set(NaN, 3); z.set(-NaN, 4);
    check(z.size === 2 && z.get(-0) === 2 && z.get(NaN) === 4);
    check(Object.is(z.keys().next().value, 0));
    const s = new Set([-0, 0, NaN, -NaN]);
    check(s.size === 2 && Object.is(s.values().next().value, 0));
    globalThis.collectionPassed = true;
  "#,
  );
  assert_eq!(vm.get_global("collectionPassed"), Some(Value::Bool(true)));
}

#[test]
fn indexed_group_by_keeps_groups_rooted_across_callback_collection() {
  let mut vm = vm();
  run(
    &mut vm,
    r#"
    var key = {name: 'object'};
    var groups = Map.groupBy([1, 2, 3, 4], (item, index) => {
      collectNow();
      return index < 2 ? key : -0;
    });
    if (groups.size !== 2 || groups.get(key).length !== 2 || groups.get(0).length !== 2
        || groups.get(key).join(',') !== '1,2'
        || groups.get(0).join(',') !== '3,4'
        || !Object.is(Array.from(groups.keys())[1], 0)) throw new Error('grouping');
    var many = Map.groupBy(Array.from({length: 1024}, (_, i) => i), item => item);
    if (many.size !== 1024 || many.get(1023)[0] !== 1023) throw new Error('many groups');
  "#,
  );
}

#[test]
fn iterator_only_roots_keep_indexed_keys_alive_and_deletion_releases_them() {
  let mut vm = vm();
  run(
    &mut vm,
    r#"
    var iterator, reference, setIterator;
    (() => {
      const key = {marker: 42}, map = new Map([[key, {marker: 43}]]);
      reference = new WeakRef(key);
      map.has(key); iterator = map.entries();
      const member = {marker: 44}, set = new Set([member]);
      set.has(member); setIterator = set.values();
    })();
  "#,
  );
  vm.collect_garbage_prototype();
  run(
    &mut vm,
    r#"
    var entry = iterator.next().value;
    if (entry[0].marker !== 42 || entry[1].marker !== 43 || reference.deref().marker !== 42
      || setIterator.next().value.marker !== 44) throw new Error('iterator roots');
    var retained = new Map(), released;
    (() => {
      const key = {marker: 45}; released = new WeakRef(key);
      retained.set(key, key); retained.has(key); retained.delete(key);
    })();
  "#,
  );
  vm.collect_garbage_prototype();
  run(
    &mut vm,
    "if (released.deref() !== undefined) throw new Error('deleted key retained');",
  );
}
