// SPDX-License-Identifier: MPL-2.0
use super::*;

#[test]
fn guarded_species_observes_replacement_deletion_and_intrinsic_default() {
  let source = crate::js!(
    r#"
    var OriginalArray = Array;
    var getter = Object.getOwnPropertyDescriptor(Array, Symbol.species).get;
    var input = [1, 2, 3];
    function twice(value) { return value * 2; }
    var first = input.map(twice);
    var calls = 0;
    Object.defineProperty(Array, Symbol.species, { configurable: true, get: function() {
      calls = calls + 1;
      return null;
    }});
    var second = input.map(twice);
    delete Array[Symbol.species];
    var third = input.map(twice);
    Object.defineProperty(Array, Symbol.species, { configurable: true, get: getter });
    var fourth = input.map(twice);
    globalThis.Array = function() { throw new Error('global Array must not be called'); };
    input.constructor = undefined;
    var fifth = input.map(twice);
    input.constructor = {};
    var sixth = input.map(twice);
    input.map = function() { return 'replaced'; };
    globalThis.ok = calls === 1 && first[2] === 6 && second[2] === 6
      && third[2] === 6 && fourth[2] === 6 && fifth[2] === 6 && sixth[2] === 6
      && OriginalArray.isArray(fifth) && OriginalArray.isArray(sixth)
      && input.map(twice) === 'replaced';
  "#
  );
  let globals = Rc::new(RefCell::new(ObjectData::new()));
  globals.borrow_mut().properties.insert(
    PropertyKey::String("globalThis".into()),
    PropertyDescriptor::ordinary_data(Value::Object(Rc::clone(&globals))),
  );
  crate::runtime_utils::install_minimal_kernel_constructors(&globals);
  let mut vm = Vm::new(globals);
  let session = crate::parser::Parser::parse_source(source).expect("parse");
  let code = crate::compiler::compile_session(&session).expect("compile");
  vm.run(code).expect("species behavior");
  assert_eq!(vm.get_global("ok"), Some(Value::Bool(true)));
}
