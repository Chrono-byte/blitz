// SPDX-License-Identifier: MPL-2.0
use super::*;
use std::cell::RefCell;
use std::rc::Rc;

fn evaluate(
  source: &str,
  enabled: bool,
) -> Vm {
  let mut vm = Vm::new(Rc::new(RefCell::new(ObjectData::new())));
  vm.caches.optimized_execution_enabled = enabled;
  let session = crate::parser::Parser::parse_source(source).expect("parse");
  vm.run(crate::compiler::compile_repl_session(&session).expect("compile"))
    .expect("evaluate");
  vm
}

#[test]
fn own_index_reads_follow_value_changes_different_receivers_and_key_positions() {
  let source = crate::js!(
    r#"
    function read(record, key) { return record[key]; }
    var a = { before: 1, value: 2, after: 3 };
    var b = { before: 10, value: 20, after: 30 };
    var output = [read(a, 'value'), read(b, 'value')];
    a.value = 4;
    output.push(read(a, 'value'));
    delete a.before;
    output.push(read(a, 'value'));
    delete a.value;
    a.value = 5;
    output.push(read(a, 'value'));
    output.push(read({ value: 6, other: 7 }, 'value'));
    output.push(read(b, 'after'));
    Object.defineProperty(b, 'value', { value: 21, writable: false });
    output.push(read(b, 'value'));
    var symbol = Symbol('key');
    b[symbol] = 50;
    output.push(read(b, symbol));
    var result = output.join(',');
  "#
  );
  for enabled in [false, true] {
    let vm = evaluate(source, enabled);
    assert_eq!(
      vm.get_global("result"),
      Some(Value::String("2,20,4,4,5,6,30,21,50".into()))
    );
    if enabled {
      assert!(!vm.caches.property_get.is_empty());
    }
  }
}

#[test]
fn own_index_reads_preserve_accessors_prototypes_and_exotics() {
  let source = crate::js!(
    r#"
    function read(record) { return record.value; }
    var record = { value: 1 };
    var output = [read(record)];
    var calls = 0;
    Object.defineProperty(record, 'value', { configurable: true, get() { calls++; return 10 + calls; } });
    output.push(read(record), read(record));
    delete record.value;
    var parent = { value: 20 };
    Object.setPrototypeOf(record, parent);
    output.push(read(record));
    parent.value = 21;
    output.push(read(record));
    var proxyReads = 0;
    var proxy = new Proxy({ value: 30 }, { get(target, key) { proxyReads++; return target[key]; } });
    output.push(read(proxy));
    Object.defineProperty(record, 'value', { value: 40, configurable: true });
    output.push(read(record));
    var result = output.join(',') + ':' + calls + ':' + proxyReads;
  "#
  );
  for enabled in [false, true] {
    let vm = evaluate(source, enabled);
    assert_eq!(
      vm.get_global("result"),
      Some(Value::String("1,11,12,20,21,30,40:2:1".into()))
    );
  }
}

#[test]
fn own_index_reads_do_not_retain_stale_host_values_or_hide_uninitialized_state() {
  let session = crate::parser::Parser::parse_source(crate::js!(
    "function read(object) { return object.value; }"
  ))
  .expect("parse");
  let root = crate::compiler::compile_session(&session).expect("compile");
  let chunk = root.functions[0].chunk.clone();
  let site = chunk
    .words
    .iter()
    .position(|word| {
      crate::bytecode::opcode_from_word(*word) == crate::bytecode::Opcode::GetProperty as u8
    })
    .expect("property read");
  let mut vm = Vm::new(Rc::new(RefCell::new(ObjectData::new())));
  vm.execution.active.chunk = chunk;
  let object = Rc::new(RefCell::new(ObjectData::new()));
  let key = PropertyKey::String("value".into());
  object.borrow_mut().properties.insert(
    key.clone(),
    PropertyDescriptor::ordinary_data(Value::Number(1.0)),
  );
  let receiver = Value::Object(Rc::clone(&object));
  assert_eq!(
    vm.get_property_value_cached(site, &receiver, &key)
      .expect("first"),
    Value::Number(1.0)
  );
  object.borrow_mut().properties.insert(
    key.clone(),
    PropertyDescriptor::ordinary_data(Value::Number(2.0)),
  );
  assert_eq!(
    vm.get_property_value_cached(site, &receiver, &key)
      .expect("updated"),
    Value::Number(2.0)
  );
  object.borrow_mut().properties.insert(
    key.clone(),
    PropertyDescriptor::ordinary_data(Value::Uninitialized),
  );
  assert!(
    vm.get_property_value_cached(site, &receiver, &key)
      .is_err()
  );
}
