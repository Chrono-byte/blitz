// SPDX-License-Identifier: MPL-2.0
use super::*;

#[test]
fn plain_record_serialization_matches_generic_order_values_and_mutations() {
  let source = crate::js!(
    r#"
    var record = {z: -0, '10': 10, '2': 2, '01': 1, '4294967295': 5,
      text: 'a\n"\\\ud800', absent: undefined, symbol: Symbol(), infinity: Infinity};
    Object.defineProperty(record, 'hidden', {get() {throw 1;}, enumerable: false});
    function same(value) { return JSON.stringify(value) === JSON.stringify(value, undefined, ''); }
    var ok = same(record) && JSON.stringify(record).slice(0, 15) === '{"2":2,"10":10,';
    record.z = 17;
    delete record['2'];
    record.extra = true;
    ok = ok && same(record);
    var calls = 0;
    var prototype = {get toJSON() { calls++; return function() {return 42;}; }};
    Object.setPrototypeOf(record, prototype);
    ok = ok && same(record) && calls === 2;
    var read = 0;
    var accessor = {get value() {read++; return 9;}};
    ok = ok && same(accessor) && read === 2;
    ok = ok && same({nested: [1, 2]}) && same(Object.create(null));
    var replacerCalls = 0;
    var replaced = JSON.stringify({a: 1}, function(key, value) {
      replacerCalls++; return key === 'a' ? 2 : value;
    });
    globalThis.ok = ok && replaced === '{"a":2}' && replacerCalls === 2;
  "#
  );
  let globals = Rc::new(RefCell::new(ObjectData::new()));
  globals.borrow_mut().properties.insert(
    PropertyKey::String("globalThis".into()),
    PropertyDescriptor::ordinary_data(Value::Object(Rc::clone(&globals))),
  );
  let mut vm = Vm::new(globals);
  let session = crate::parser::Parser::parse_source(source).expect("parse");
  vm.run(crate::compiler::compile_session(&session).expect("compile"))
    .expect("run");
  assert_eq!(vm.get_global("ok"), Some(Value::Bool(true)));
}

#[test]
fn plain_record_guard_rejects_observable_and_special_objects() {
  let object = Rc::new(RefCell::new(ObjectData::new()));
  let value = Value::Object(Rc::clone(&object));
  object.borrow_mut().properties.insert(
    PropertyKey::String("a".into()),
    PropertyDescriptor::ordinary_data(Value::Number(1.0)),
  );
  assert_eq!(stringify_plain_record(&value), Some("{\"a\":1}".into()));
  object
    .borrow_mut()
    .set_internal_slot("[[NumberData]]", Value::Number(2.0));
  assert!(stringify_plain_record(&value).is_none());
}
