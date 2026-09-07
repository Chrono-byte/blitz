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

#[test]
fn typed_bulk_operations_preserve_order_aliases_and_coercions() {
  let mut vm = Vm::new(Rc::new(RefCell::new(ObjectData::new())));
  evaluate(
    &mut vm,
    r#"
    var ok = true;
    [Int8Array, Uint8Array, Uint8ClampedArray, Int16Array, Uint16Array,
      Int32Array, Uint32Array, Float32Array, Float64Array, BigInt64Array, BigUint64Array]
      .forEach(function(T) {
        var big = T === BigInt64Array || T === BigUint64Array;
        var cast = function(n) { return big ? BigInt(n) : n; };
        var full = new T(2055);
        full.fill(cast(99));
        var a = new T(full.buffer, T.BYTES_PER_ELEMENT * 2, 2051);
        a.fill(cast(7), 1, -1);
        ok = ok && full[1] === cast(99) && full[2053] === cast(99)
          && a[0] === cast(99) && a[2050] === cast(99) && a[2049] === cast(7);
        a[0] = cast(3); a[1] = cast(1); a[2] = cast(2);
        var copy = a.toSorted();
        ok = ok && copy !== a && copy[0] === cast(1) && a[0] === cast(3);
        a.sort();
        ok = ok && a[0] === cast(1) && a[1] === cast(2) && a[2] === cast(3);
        var b = new T([cast(1),cast(2),cast(3),cast(4),cast(5)]);
        b.copyWithin(1,0,4);
        ok = ok && b.join() === '1,1,2,3,4';
        b.copyWithin(0,1);
        ok = ok && b.join() === '1,2,3,4,4';
      });
    var f = new Float64Array([NaN, 0, -0, Infinity, -Infinity, NaN]);
    f.sort();
    ok = ok && f[0] === -Infinity && Object.is(f[1], -0)
      && Object.is(f[2], 0) && f[3] === Infinity && Number.isNaN(f[4]) && Number.isNaN(f[5]);
    var signed = new BigInt64Array([3n,-4n,0n,-9223372036854775808n]);
    signed.sort();
    ok = ok && signed[0] === -9223372036854775808n && signed[1] === -4n;
    var unsigned = new BigUint64Array([18446744073709551615n,0n,1n]);
    unsigned.sort();
    ok = ok && unsigned[2] === 18446744073709551615n;
    var order = '';
    var v = new Uint8Array(4);
    v.fill({valueOf:function(){order+='v';return 258;}},
      {valueOf:function(){order+='s';return 1;}},
      {valueOf:function(){order+='e';return 3;}});
    ok = ok && order === 'vse' && v.join() === '0,2,2,0';
    var c = new Uint8Array([3,1,2]);
    c.sort(function(a,b){return b-a;});
    ok = ok && c.join() === '3,2,1';
    try { c.sort(function(){throw 42;}); } catch(e) { ok = ok && e === 42; }
    ok = ok && c.join() === '3,2,1';
    var long = new Uint16Array(3000);
    for(var i=0;i<long.length;i++) long[i]=i;
    long.copyWithin(1,0,2999);
    ok = ok && long[1024] === 1023 && long[2999] === 2998;
    long.copyWithin(0,1);
    ok = ok && long[1024] === 1024 && long[2998] === 2998;
  "#,
  );
  assert_eq!(vm.get_global("ok"), Some(Value::Bool(true)));
}

#[test]
fn bulk_typed_storage_preserves_nan_payloads_and_falls_back_for_host_storage() {
  let mut vm = Vm::new(Rc::new(RefCell::new(ObjectData::new())));
  evaluate(&mut vm, "var view = new Float64Array(4);");
  let view = vm.get_global("view").unwrap();
  let details = crate::ecmascript::semantics::byte_views::byte_view_details(&view).unwrap();
  let bits = [
    0x7ff8_0000_0000_0001_u64,
    1.0f64.to_bits(),
    0x7ff8_0000_0000_0002,
    (-0.0f64).to_bits(),
  ];
  for (index, bits) in bits.iter().enumerate() {
    assert!(
      details
        .data
        .borrow_mut()
        .write_bytes(index * 8, &bits.to_le_bytes())
    );
  }
  assert!(vm.try_sort_typed_view(&view, &view).unwrap());
  let mut bytes = [0; 8];
  for (index, expected) in [bits[3], bits[1], bits[0], bits[2]]
    .iter()
    .enumerate()
  {
    assert!(
      details
        .data
        .borrow()
        .read_bytes(index * 8, &mut bytes)
    );
    assert_eq!(u64::from_le_bytes(bytes), *expected);
  }
  vm.interrupt_handle().request_interrupt().unwrap();
  assert!(
    vm.try_sort_typed_view(&view, &view)
      .unwrap_err()
      .is_execution_interrupted()
  );
  details.data.borrow_mut()[0] = Some(Value::Number(0.0));
  assert!(!vm.try_sort_typed_view(&view, &view).unwrap());
  assert!(
    !vm
      .try_fill_typed_view(&view, 0, 4, &1.0f64.to_le_bytes())
      .unwrap()
  );
  assert!(
    !vm
      .try_copy_within_typed_view(&view, 1, 0, 3)
      .unwrap()
  );
  evaluate(
    &mut vm,
    "view.fill(3); view.copyWithin(1,0); view.sort(); var ok = view.join() === '3,3,3,3';",
  );
  assert_eq!(vm.get_global("ok"), Some(Value::Bool(true)));
}

#[test]
fn builtin_method_argument_buffers_preserve_reentrant_calls_and_overflow() {
  let mut vm = Vm::new(Rc::new(RefCell::new(ObjectData::new())));
  evaluate(
    &mut vm,
    r#"
    var a = [];
    var ok = a.push() === 0 && a.push(1,2,3,4) === 4 && a.push(5,6,7,8,9) === 9;
    ok = ok && a.join() === '1,2,3,4,5,6,7,8,9';
    var receiver = {toString:function(){ a.push(10); return 'abc'; }};
    var index = {valueOf:function(){a.push(11); return 1;}};
    ok = ok && String.prototype.charCodeAt.call(receiver, index) === 98 && a.length === 11;
    var threw = false;
    try { 'x'.charCodeAt({valueOf:function(){throw 42;}}); } catch(e) { threw = e === 42; }
    ok = ok && threw && 'abc'.charCodeAt(2) === 99;
  "#,
  );
  assert_eq!(vm.get_global("ok"), Some(Value::Bool(true)));
}
