// SPDX-License-Identifier: MPL-2.0
use std::cell::RefCell;
use std::rc::Rc;

use crate::runtime_utils::{
  ShadowRealmRecord, builtin_object, register_shadow_realm, resolve_shadowrealm_base_dir,
  set_builtin_length, shadowrealm_evaluate, shadowrealm_import_value,
};
use crate::value::{BuiltinFn, ObjectData, PropertyDescriptor, PropertyKey, SymbolId, Value};

/// Builds the ShadowRealm constructor function.
pub(crate) fn build_shadowrealm_constructor(
  object_proto_map: &Rc<RefCell<ObjectData>>,
  function_proto_map: &Rc<RefCell<ObjectData>>,
) -> Value {
  let shadowrealm_proto_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
    ObjectData::with_prototype(Value::Object(Rc::clone(object_proto_map))),
  ));
  let shadowrealm_ctor_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
    ObjectData::with_prototype(Value::Object(Rc::clone(function_proto_map))),
  ));
  let shadowrealm_proto_val = Value::Object(Rc::clone(&shadowrealm_proto_map));
  let shadowrealm_ctor_fn: BuiltinFn = Rc::new({
    let shadowrealm_proto_val = shadowrealm_proto_val;
    move |vm, _args| {
      if vm.current_new_target().is_none() {
        return Err(vm.throw_type_error("ShadowRealm must be called with new"));
      }
      let obj = ObjectData::with_prototype(shadowrealm_proto_val.clone());
      let obj_rc = Rc::new(RefCell::new(obj));
      let record = Rc::new(ShadowRealmRecord::new(resolve_shadowrealm_base_dir(vm)));
      register_shadow_realm(&obj_rc, record);
      Ok(Value::Object(obj_rc))
    }
  });
  let shadowrealm_ctor_val =
    Value::BuiltinObject(shadowrealm_ctor_fn, Rc::clone(&shadowrealm_ctor_map));
  shadowrealm_ctor_map
    .borrow_mut()
    .properties
    .insert(
      PropertyKey::String("prototype".to_string()),
      PropertyDescriptor::builtin_prototype(Value::Object(Rc::clone(&shadowrealm_proto_map))),
    );
  shadowrealm_proto_map
    .borrow_mut()
    .properties
    .insert(
      PropertyKey::String("constructor".to_string()),
      PropertyDescriptor::builtin_data(shadowrealm_ctor_val.clone()),
    );
  set_builtin_length(&shadowrealm_ctor_val, 0.0);
  shadowrealm_ctor_map
    .borrow_mut()
    .properties
    .insert(
      PropertyKey::String("name".to_string()),
      PropertyDescriptor::builtin_metadata(Value::String("ShadowRealm".to_string())),
    );

  let shadowrealm_evaluate_val = builtin_object(Rc::new(shadowrealm_evaluate), function_proto_map);
  {
    set_builtin_length(&shadowrealm_evaluate_val, 1.0);
    if let Value::BuiltinObject(_, props) = &shadowrealm_evaluate_val {
      props.borrow_mut().properties.insert(
        PropertyKey::String("name".to_string()),
        PropertyDescriptor::builtin_metadata(Value::String("evaluate".to_string())),
      );
    }
  }
  shadowrealm_proto_map
    .borrow_mut()
    .properties
    .insert(
      PropertyKey::String("evaluate".to_string()),
      PropertyDescriptor::builtin_data(shadowrealm_evaluate_val),
    );

  let shadowrealm_import_val =
    builtin_object(Rc::new(shadowrealm_import_value), function_proto_map);
  {
    set_builtin_length(&shadowrealm_import_val, 2.0);
    if let Value::BuiltinObject(_, props) = &shadowrealm_import_val {
      props.borrow_mut().properties.insert(
        PropertyKey::String("name".to_string()),
        PropertyDescriptor::builtin_metadata(Value::String("importValue".to_string())),
      );
    }
  }
  shadowrealm_proto_map
    .borrow_mut()
    .properties
    .insert(
      PropertyKey::String("importValue".to_string()),
      PropertyDescriptor::builtin_data(shadowrealm_import_val),
    );
  shadowrealm_proto_map
    .borrow_mut()
    .properties
    .insert(
      PropertyKey::Symbol(SymbolId::well_known_to_string_tag()),
      PropertyDescriptor::Data {
        value: Value::String("ShadowRealm".to_string()),
        writable: false,
        enumerable: false,
        configurable: true,
      },
    );

  shadowrealm_ctor_val
}
