// SPDX-License-Identifier: MPL-2.0
//! WeakRef and FinalizationRegistry builtins backed by the prototype collector.

use std::cell::RefCell;
use std::rc::Rc;

use crate::runtime_utils::{builtin_object, set_builtin_length, set_builtin_name};
use crate::value::{
  BuiltinFn, FinalizationRegistryCell, ObjectData, PropertyDescriptor, PropertyKey, SymbolId,
  Value, WeakKey, WeakValue,
};
use crate::{RuntimeError, Vm};

const WEAK_REF_BRAND: &str = "[[WeakRefBrand]]";

pub(crate) struct WeakReferenceIntrinsics {
  pub(crate) weak_ref_constructor: Value,
  pub(crate) weak_ref_prototype: Value,
  pub(crate) finalization_registry_constructor: Value,
  pub(crate) finalization_registry_prototype: Value,
}

fn direct_object_data(value: &Value) -> Option<&Rc<RefCell<ObjectData>>> {
  match value {
    Value::Object(data)
    | Value::Array(_, data)
    | Value::Closure { props: data, .. }
    | Value::BuiltinObject(_, data)
    | Value::Promise(_, data) => Some(data),
    _ => None,
  }
}

fn require_weak_ref(
  vm: &mut Vm,
  value: &Value,
) -> Result<Rc<RefCell<ObjectData>>, RuntimeError> {
  let Some(data) = direct_object_data(value) else {
    return Err(vm.throw_type_error("WeakRef.prototype.deref called on incompatible receiver"));
  };
  if !matches!(
    data.borrow().internal_slot(WEAK_REF_BRAND),
    Some(Value::Bool(true))
  ) {
    return Err(vm.throw_type_error("WeakRef.prototype.deref called on incompatible receiver"));
  }
  Ok(Rc::clone(data))
}

fn require_registry(
  vm: &mut Vm,
  value: &Value,
  name: &str,
) -> Result<Rc<RefCell<ObjectData>>, RuntimeError> {
  let Some(data) = direct_object_data(value) else {
    return Err(vm.throw_type_error(&format!(
      "FinalizationRegistry.prototype.{name} called on incompatible receiver"
    )));
  };
  if data
    .borrow()
    .finalization_registry_cells
    .is_none()
  {
    return Err(vm.throw_type_error(&format!(
      "FinalizationRegistry.prototype.{name} called on incompatible receiver"
    )));
  }
  Ok(Rc::clone(data))
}

fn object_value(
  vm: &mut Vm,
  value: &Value,
  message: &str,
) -> Result<(), RuntimeError> {
  if value.is_object() {
    Ok(())
  } else {
    Err(vm.throw_type_error(message))
  }
}

fn method(
  function: BuiltinFn,
  function_prototype: &Rc<RefCell<ObjectData>>,
  name: &str,
  length: f64,
) -> Value {
  let value = builtin_object(function, function_prototype);
  set_builtin_name(&value, name);
  set_builtin_length(&value, length);
  value
}

pub(crate) fn create_weak_reference_intrinsics(
  object_prototype: &Rc<RefCell<ObjectData>>,
  function_prototype: &Rc<RefCell<ObjectData>>,
) -> WeakReferenceIntrinsics {
  use PropertyKey as PKey;

  let weak_ref_prototype = Rc::new(RefCell::new(ObjectData::with_prototype(Value::Object(
    Rc::clone(object_prototype),
  ))));
  let weak_ref_constructor_properties = Rc::new(RefCell::new(ObjectData::with_prototype(
    Value::Object(Rc::clone(function_prototype)),
  )));
  weak_ref_constructor_properties
    .borrow_mut()
    .properties
    .insert(
      PKey::String("prototype".into()),
      PropertyDescriptor::builtin_prototype(Value::Object(Rc::clone(&weak_ref_prototype))),
    );
  let weak_ref_constructor_function: BuiltinFn = Rc::new(|vm, args| {
    if vm.pending_constructor_new_target().is_none() {
      return Err(vm.throw_type_error("Constructor WeakRef requires 'new'"));
    }
    let target = args.get(1).cloned().unwrap_or(Value::Undefined);
    object_value(vm, &target, "WeakRef target must be an object")?;
    let receiver = args.first().cloned().unwrap_or(Value::Undefined);
    let Value::Object(object) = receiver else {
      return Err(vm.throw_type_error("WeakRef constructor failed to create an object"));
    };
    object
      .borrow_mut()
      .set_internal_slot(WEAK_REF_BRAND, Value::Bool(true));
    object.borrow_mut().weak_ref_target = Some(WeakValue::new(&target));
    Ok(Value::Object(object))
  });
  let weak_ref_constructor = Value::BuiltinObject(
    weak_ref_constructor_function,
    Rc::clone(&weak_ref_constructor_properties),
  );
  set_builtin_name(&weak_ref_constructor, "WeakRef");
  set_builtin_length(&weak_ref_constructor, 1.0);
  weak_ref_prototype.borrow_mut().properties.insert(
    PKey::String("constructor".into()),
    PropertyDescriptor::builtin_data(weak_ref_constructor.clone()),
  );
  weak_ref_prototype.borrow_mut().properties.insert(
    PKey::String("deref".into()),
    PropertyDescriptor::builtin_data(method(
      Rc::new(|vm, args| {
        let receiver = args.first().cloned().unwrap_or(Value::Undefined);
        let object = require_weak_ref(vm, &receiver)?;
        let target = object
          .borrow()
          .weak_ref_target
          .as_ref()
          .and_then(WeakValue::upgrade);
        if let Some(target) = target {
          vm.keep_alive(target.clone());
          Ok(target)
        } else {
          Ok(Value::Undefined)
        }
      }),
      function_prototype,
      "deref",
      0.0,
    )),
  );
  weak_ref_prototype.borrow_mut().properties.insert(
    PKey::Symbol(SymbolId::well_known_to_string_tag()),
    PropertyDescriptor::Data {
      value: Value::String("WeakRef".into()),
      writable: false,
      enumerable: false,
      configurable: true,
    },
  );

  let registry_prototype = Rc::new(RefCell::new(ObjectData::with_prototype(Value::Object(
    Rc::clone(object_prototype),
  ))));
  let registry_constructor_properties = Rc::new(RefCell::new(ObjectData::with_prototype(
    Value::Object(Rc::clone(function_prototype)),
  )));
  registry_constructor_properties
    .borrow_mut()
    .properties
    .insert(
      PKey::String("prototype".into()),
      PropertyDescriptor::builtin_prototype(Value::Object(Rc::clone(&registry_prototype))),
    );
  let registry_constructor_function: BuiltinFn = Rc::new(|vm, args| {
    if vm.pending_constructor_new_target().is_none() {
      return Err(vm.throw_type_error("Constructor FinalizationRegistry requires 'new'"));
    }
    let cleanup = args.get(1).cloned().unwrap_or(Value::Undefined);
    if !Vm::is_callable_value(&cleanup) {
      return Err(vm.throw_type_error("FinalizationRegistry cleanup callback is not callable"));
    }
    let receiver = args.first().cloned().unwrap_or(Value::Undefined);
    let Value::Object(object) = receiver else {
      return Err(
        vm.throw_type_error("FinalizationRegistry constructor failed to create an object"),
      );
    };
    let mut data = object.borrow_mut();
    data.finalization_registry_cells = Some(Vec::new());
    data.finalization_cleanup_callback = Some(cleanup);
    data.finalization_cleanup_scheduled = false;
    drop(data);
    Ok(Value::Object(object))
  });
  let finalization_registry_constructor = Value::BuiltinObject(
    registry_constructor_function,
    Rc::clone(&registry_constructor_properties),
  );
  set_builtin_name(&finalization_registry_constructor, "FinalizationRegistry");
  set_builtin_length(&finalization_registry_constructor, 1.0);
  registry_prototype.borrow_mut().properties.insert(
    PKey::String("constructor".into()),
    PropertyDescriptor::builtin_data(finalization_registry_constructor.clone()),
  );
  registry_prototype.borrow_mut().properties.insert(
    PKey::String("register".into()),
    PropertyDescriptor::builtin_data(method(
      Rc::new(|vm, args| {
        let receiver = args.first().cloned().unwrap_or(Value::Undefined);
        let target = args.get(1).cloned().unwrap_or(Value::Undefined);
        let holdings = args.get(2).cloned().unwrap_or(Value::Undefined);
        let token = args.get(3).cloned().unwrap_or(Value::Undefined);
        let registry = require_registry(vm, &receiver, "register")?;
        object_value(vm, &target, "FinalizationRegistry target must be an object")?;
        if crate::same_value(&target, &holdings) {
          return Err(vm.throw_type_error("FinalizationRegistry target and holdings must differ"));
        }
        let unregister_token = if matches!(token, Value::Undefined) {
          None
        } else {
          Some(WeakKey::new(&token).ok_or_else(|| {
            vm.throw_type_error("FinalizationRegistry unregister token must be an object")
          })?)
        };
        registry
          .borrow_mut()
          .finalization_registry_cells
          .as_mut()
          .expect("registry brand checked")
          .push(FinalizationRegistryCell {
            target: WeakValue::new(&target),
            holdings,
            unregister_token,
            active: true,
          });
        Ok(Value::Undefined)
      }),
      function_prototype,
      "register",
      2.0,
    )),
  );
  registry_prototype.borrow_mut().properties.insert(
    PKey::String("unregister".into()),
    PropertyDescriptor::builtin_data(method(
      Rc::new(|vm, args| {
        let receiver = args.first().cloned().unwrap_or(Value::Undefined);
        let token = args.get(1).cloned().unwrap_or(Value::Undefined);
        let registry = require_registry(vm, &receiver, "unregister")?;
        WeakKey::new(&token).ok_or_else(|| {
          vm.throw_type_error("FinalizationRegistry unregister token must be an object")
        })?;
        let mut registry = registry.borrow_mut();
        let cells = registry
          .finalization_registry_cells
          .as_mut()
          .expect("registry brand checked");
        let before = cells.len();
        cells.retain(|cell| {
          !cell
            .unregister_token
            .as_ref()
            .is_some_and(|cell_token| cell_token.matches(&token))
        });
        Ok(Value::Bool(cells.len() != before))
      }),
      function_prototype,
      "unregister",
      1.0,
    )),
  );
  registry_prototype.borrow_mut().properties.insert(
    PKey::Symbol(SymbolId::well_known_to_string_tag()),
    PropertyDescriptor::Data {
      value: Value::String("FinalizationRegistry".into()),
      writable: false,
      enumerable: false,
      configurable: true,
    },
  );

  WeakReferenceIntrinsics {
    weak_ref_constructor,
    weak_ref_prototype: Value::Object(weak_ref_prototype),
    finalization_registry_constructor,
    finalization_registry_prototype: Value::Object(registry_prototype),
  }
}
