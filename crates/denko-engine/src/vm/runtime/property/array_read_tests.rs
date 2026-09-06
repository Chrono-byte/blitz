// SPDX-License-Identifier: MPL-2.0
use super::*;
use std::cell::RefCell;
use std::rc::Rc;

#[test]
fn present_array_reads_preserve_overrides_holes_lengths_and_proxies() {
  let source = crate::js!(
    r#"
    var array = [3, 4, 5];
    var output = [];
    output.push(array[1]);
    var getterCalls = 0;
    Object.defineProperty(array, '1', { configurable: true, get: function() {
      getterCalls = getterCalls + 1;
      return 41;
    }});
    output.push(array[1]);
    Object.defineProperty(array, '1', { configurable: true, value: 42 });
    output.push(array[1]);
    delete array[1];
    var prototype = Object.create(Array.prototype);
    Object.defineProperty(prototype, '1', { get: function() { return this[0] + 50; }});
    Object.setPrototypeOf(array, prototype);
    output.push(array[1]);
    array.length = 1;
    output.push(array[2] === undefined);
    var traps = 0;
    var proxy = new Proxy(array, { get: function(target, key) {
      traps = traps + 1;
      return key === '0' ? 71 : target[key];
    }});
    output.push(proxy[0]);
    var coercions = 0;
    var key = { toString: function() { coercions = coercions + 1; return '0'; } };
    output.push(array[key]);
    var packed = [2, 3].map(function(value) { return value * value; });
    output.push(packed[1]);
    globalThis.result = output.join(',') + ':' + getterCalls + ':' + traps + ':' + coercions;
  "#
  );
  let run = |enabled| {
    let globals = Rc::new(RefCell::new(ObjectData::new()));
    globals.borrow_mut().properties.insert(
      PropertyKey::String("globalThis".into()),
      PropertyDescriptor::ordinary_data(Value::Object(Rc::clone(&globals))),
    );
    crate::runtime_utils::install_minimal_kernel_constructors(&globals);
    let mut vm = Vm::new(globals);
    vm.caches.optimized_execution_enabled = enabled;
    let session = crate::parser::Parser::parse_source(source).expect("parse");
    vm.run(crate::compiler::compile_session(&session).expect("compile"))
      .expect("array reads");
    vm.get_global("result")
  };
  let optimized = run(true);
  assert_eq!(optimized, run(false));
  assert_eq!(
    optimized,
    Some(Value::String("4,41,42,53,true,71,3,9:1:1:1".into()))
  );
}

#[test]
fn direct_array_read_retains_uninitialized_error() {
  for enabled in [false, true] {
    let globals = Rc::new(RefCell::new(ObjectData::new()));
    crate::runtime_utils::install_minimal_kernel_constructors(&globals);
    let mut vm = Vm::new(globals);
    vm.caches.optimized_execution_enabled = enabled;
    let array = vm.new_array(vec![Some(Value::Uninitialized)]);
    assert!(
      vm.get_property_value_cached(0, &array, &PropertyKey::String("0".into()))
        .is_err()
    );
  }
}
