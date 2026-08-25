// SPDX-License-Identifier: MPL-2.0
//! Allocation of the cyclic Object, Function, and generator intrinsic graph.

use super::*;

pub(crate) struct FoundationIntrinsics {
  pub object_prototype: Rc<RefCell<ObjectData>>,
  pub function_prototype: Rc<RefCell<ObjectData>>,
  pub function_constructor_properties: Rc<RefCell<ObjectData>>,
  pub object_constructor_properties: Rc<RefCell<ObjectData>>,
  pub generator_prototype: Rc<RefCell<ObjectData>>,
  pub generator_function_prototype: Rc<RefCell<ObjectData>>,
  pub throw_type_error: Value,
}

pub(crate) fn create_foundation() -> FoundationIntrinsics {
  let object_prototype = Rc::new(RefCell::new(ObjectData::new()));
  let function_prototype = Rc::new(RefCell::new(ObjectData::with_prototype(Value::Object(
    Rc::clone(&object_prototype),
  ))));

  let function_constructor_properties = Rc::new(RefCell::new(ObjectData::with_prototype(
    Value::Object(Rc::clone(&function_prototype)),
  )));
  function_constructor_properties
    .borrow_mut()
    .properties
    .insert(
      PropertyKey::prototype(),
      PropertyDescriptor::builtin_prototype(Value::Object(Rc::clone(&function_prototype))),
    );

  let generator_prototype = Rc::new(RefCell::new(ObjectData::with_prototype(Value::Object(
    Rc::clone(&object_prototype),
  ))));
  let generator_function_prototype = Rc::new(RefCell::new(ObjectData::with_prototype(
    Value::Object(Rc::clone(&function_prototype)),
  )));
  generator_prototype
    .borrow_mut()
    .properties
    .insert(
      PropertyKey::Symbol(crate::value::SymbolId::well_known_to_string_tag()),
      PropertyDescriptor::Data {
        value: Value::String("Generator".into()),
        writable: false,
        enumerable: false,
        configurable: true,
      },
    );
  for (name, slot) in [
    ("next", "[[GeneratorNext]]"),
    ("return", "[[GeneratorReturn]]"),
    ("throw", "[[GeneratorThrow]]"),
  ] {
    let method = builtin_object(
      Rc::new(move |vm, args| {
        let receiver = args.first().cloned().unwrap_or(Value::Undefined);
        let Value::Object(object) = &receiver else {
          return Err(vm.throw_type_error(&format!(
            "Generator.prototype.{name} called on incompatible receiver"
          )));
        };
        let implementation = object
          .borrow()
          .internal_slot(slot)
          .cloned()
          .ok_or_else(|| {
            vm.throw_type_error(&format!(
              "Generator.prototype.{name} called on incompatible receiver"
            ))
          })?;
        let user_args = if vm.builtin_this_arg_prepended() {
          args.get(1..).unwrap_or_default()
        } else {
          args
        };
        vm.call_value_with_this(&implementation, &receiver, user_args)
      }),
      &function_prototype,
    );
    set_builtin_name(&method, name);
    set_builtin_length(&method, 1.0);
    generator_prototype
      .borrow_mut()
      .properties
      .insert(
        PropertyKey::String(name.into()),
        PropertyDescriptor::builtin_data(method),
      );
  }
  let iterator = builtin_object(
    Rc::new(|_vm, args| Ok(args.first().cloned().unwrap_or(Value::Undefined))),
    &function_prototype,
  );
  set_builtin_name(&iterator, "[Symbol.iterator]");
  set_builtin_length(&iterator, 0.0);
  generator_prototype
    .borrow_mut()
    .properties
    .insert(
      PropertyKey::Symbol(crate::value::SymbolId::well_known_iterator()),
      PropertyDescriptor::builtin_data(iterator),
    );
  generator_function_prototype
    .borrow_mut()
    .properties
    .insert(
      PropertyKey::Symbol(crate::value::SymbolId::well_known_to_string_tag()),
      PropertyDescriptor::Data {
        value: Value::String("GeneratorFunction".into()),
        writable: false,
        enumerable: false,
        configurable: true,
      },
    );
  generator_function_prototype
    .borrow_mut()
    .properties
    .insert(
      PropertyKey::prototype(),
      PropertyDescriptor::builtin_prototype(Value::Object(Rc::clone(&generator_prototype))),
    );
  generator_prototype
    .borrow_mut()
    .properties
    .insert(
      PropertyKey::String("constructor".into()),
      PropertyDescriptor::Data {
        value: Value::Object(Rc::clone(&generator_function_prototype)),
        writable: false,
        enumerable: false,
        configurable: true,
      },
    );

  let throw_type_error = builtin_object(
    Rc::new(|vm, _args| Err(vm.throw_type_error("ThrowTypeError"))),
    &function_prototype,
  );

  let object_constructor_properties = Rc::new(RefCell::new(ObjectData::with_prototype(
    Value::Object(Rc::clone(&function_prototype)),
  )));
  object_constructor_properties
    .borrow_mut()
    .properties
    .insert(
      PropertyKey::prototype(),
      PropertyDescriptor::builtin_prototype(Value::Object(Rc::clone(&object_prototype))),
    );

  FoundationIntrinsics {
    object_prototype,
    function_prototype,
    function_constructor_properties,
    object_constructor_properties,
    generator_prototype,
    generator_function_prototype,
    throw_type_error,
  }
}
