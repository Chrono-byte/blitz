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
      PropertyKey::String("prototype".into()),
      PropertyDescriptor::builtin_prototype(Value::Object(Rc::clone(&function_prototype))),
    );

  let generator_prototype = Rc::new(RefCell::new(ObjectData::with_prototype(Value::Object(
    Rc::clone(&object_prototype),
  ))));
  let generator_function_prototype = Rc::new(RefCell::new(ObjectData::with_prototype(
    Value::Object(Rc::clone(&function_prototype)),
  )));
  generator_function_prototype
    .borrow_mut()
    .properties
    .insert(
      PropertyKey::String("prototype".into()),
      PropertyDescriptor::builtin_prototype(Value::Object(Rc::clone(&generator_prototype))),
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
      PropertyKey::String("prototype".into()),
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
