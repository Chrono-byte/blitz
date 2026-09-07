// SPDX-License-Identifier: MPL-2.0
use super::*;
use std::{cell::RefCell, rc::Rc};

fn evaluate(
  vm: &mut Vm,
  source: &str,
) {
  let session = crate::parser::Parser::parse_source(source).expect("parse");
  vm.run(crate::compiler::compile_repl_session(&session).expect("compile"))
    .expect("evaluate");
}

#[test]
fn pushes_preserve_numeric_storage_and_observable_fallbacks() {
  let mut vm = Vm::new(Rc::new(RefCell::new(ObjectData::new())));
  evaluate(
    &mut vm,
    "var array = [1,2,3].map(function(v) { return v*2; });",
  );
  let Value::Array(elements, _) = vm.get_global("array").expect("array") else {
    panic!("array");
  };
  assert!(elements.borrow().is_packed_numeric());
  evaluate(&mut vm, "array.push(); array.push(-0);");
  assert!(elements.borrow().is_packed_numeric());
  assert_eq!(elements.borrow().len(), 4);
  assert_eq!(
    elements
      .borrow()
      .numeric_element(3)
      .unwrap()
      .to_bits(),
    (-0.0f64).to_bits()
  );
  evaluate(
    &mut vm,
    r#"
    var seen = 0;
    var proto = Object.create(Array.prototype);
    Object.defineProperty(proto, '4', {set: function(value) { seen = value; }});
    Object.setPrototypeOf(array, proto);
    array.push(9);
    var ok = seen === 9 && array.length === 5 && !Object.hasOwn(array, '4');
    Object.setPrototypeOf(array, Array.prototype);
    array.push('mixed');
    ok = ok && array[5] === 'mixed';
    Object.defineProperty(array, 'length', {writable: false});
    var threw = false;
    try { array.push(); } catch(e) { threw = e instanceof TypeError; }
    ok = ok && threw;
  "#,
  );
  assert_eq!(vm.get_global("ok"), Some(Value::Bool(true)));
  evaluate(
    &mut vm,
    "var mixed = [1,2].map(function(v) { return v*2; }); mixed.push('mixed');",
  );
  let Value::Array(mixed, _) = vm.get_global("mixed").expect("mixed array") else {
    panic!("array");
  };
  assert!(!mixed.borrow().is_packed_numeric());
}

#[test]
fn typed_reverse_preserves_widths_aliases_and_special_numbers() {
  let mut vm = Vm::new(Rc::new(RefCell::new(ObjectData::new())));
  evaluate(
    &mut vm,
    r#"
    var types = [Int8Array, Uint8Array, Uint8ClampedArray, Int16Array, Uint16Array,
      Int32Array, Uint32Array, Float32Array, Float64Array];
    var ok = true;
    for(var t=0;t<types.length;t++) {
      var C = types[t];
      var full = new C([7,1,2,3,4,9]);
      var view = new C(full.buffer, C.BYTES_PER_ELEMENT, 4);
      ok = ok && view.reverse() === view && full.join(',') === '7,4,3,2,1,9';
    }
    var big = new BigInt64Array([1n, 9007199254740993n, -2n]);
    big.reverse();
    ok = ok && big[0] === -2n && big[1] === 9007199254740993n && big[2] === 1n;
    var unsigned = new BigUint64Array([1n, 18446744073709551615n]);
    unsigned.reverse();
    ok = ok && unsigned[0] === 18446744073709551615n && unsigned[1] === 1n;
    var special = new Float64Array([-0, NaN, Infinity]);
    special.reverse();
    ok = ok && special[0] === Infinity && Number.isNaN(special[1]) && Object.is(special[2], -0);
  "#,
  );
  assert_eq!(vm.get_global("ok"), Some(Value::Bool(true)));
  let view = vm.get_global("special").expect("special");
  let details = crate::ecmascript::semantics::byte_views::byte_view_details(&view).expect("view");
  // Generic host mutation demotes the backing and must keep the semantic fallback.
  details.data.borrow_mut()[0] = Some(Value::Number(0.0));
  assert!(
    !vm
      .try_reverse_typed_view(&view)
      .expect("fallback")
  );
  evaluate(
    &mut vm,
    "special.reverse(); var restored = Object.is(special[0], -0) && special[2] === Infinity;",
  );
  assert_eq!(vm.get_global("restored"), Some(Value::Bool(true)));
  vm.interrupt_handle()
    .request_interrupt()
    .expect("interrupt");
  let packed = vm.get_global("big").expect("packed view");
  assert!(
    vm.try_reverse_typed_view(&packed)
      .unwrap_err()
      .is_execution_interrupted()
  );
}

#[test]
fn lazy_reductions_preserve_order_seed_and_abrupt_completion() {
  let mut vm = Vm::new(Rc::new(RefCell::new(ObjectData::new())));
  evaluate(
    &mut vm,
    r#"
    var a = new Uint8Array([1,2,3]);
    var order = '';
    var left = a.reduce(function(sum,v,i) { order += i; return sum+v; });
    var right = a.reduceRight(function(sum,v,i) { order += i; return sum+v; }, 10);
    var calls = 0;
    try { a.reduce(function() { calls++; throw 17; }, 0); } catch(e) { calls += e; }
    var empty = new Uint8Array(0);
    var threw = false;
    try { empty.reduceRight(function(){}); } catch(e) { threw = e instanceof TypeError; }
    var ok = left === 6 && right === 16 && order === '12210' && calls === 18
      && threw && empty.reduce(function(){throw 1}, 7) === 7;
  "#,
  );
  assert_eq!(vm.get_global("ok"), Some(Value::Bool(true)));
}

#[test]
fn unsupported_view_does_not_poison_a_later_native_plan() {
  let mut vm = Vm::new(Rc::new(RefCell::new(ObjectData::new())));
  evaluate(
    &mut vm,
    r#"
    function sum(a) { var total=0; for(var i=0;i<a.length;i=i+1) total=total+a[i]; return total; }
    var ordinary = sum(new Float32Array([1,2,3]));
    var native = sum(new Float64Array([4,5,6]));
    var another = sum(new Float32Array([7,8,9]));
    var ok = ordinary === 6 && native === 15 && another === 24;
  "#,
  );
  assert_eq!(vm.get_global("ok"), Some(Value::Bool(true)));
  if cfg!(all(unix, target_arch = "x86_64")) {
    assert!(
      vm.native_execution_statistics()
        .native_loop_iterations
        >= 3
    );
  }
}
