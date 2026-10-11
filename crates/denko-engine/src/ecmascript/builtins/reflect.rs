// SPDX-License-Identifier: MPL-2.0

use std::cell::RefCell;
use std::rc::Rc;

use crate::Vm;
use crate::ecmascript::semantics::{
  builtin_object, create_list_from_array_like, descriptor_value_from_descriptor,
  property_key_to_value, set_builtin_length, set_builtin_name,
};
use crate::runtime_utils::{array_from_values, static_builtin_arg_offset, strict_equal};
use crate::value::{BuiltinFn, ObjectData, PropertyDescriptor, PropertyKey, SymbolId, Value};

fn key(value: &str) -> PropertyKey {
  PropertyKey::String(value.into())
}

fn function(
  function: BuiltinFn,
  function_prototype: &Rc<RefCell<ObjectData>>,
) -> Value {
  builtin_object(function, function_prototype)
}

fn install(
  object: &Rc<RefCell<ObjectData>>,
  function_prototype: &Rc<RefCell<ObjectData>>,
  name: &str,
  implementation: BuiltinFn,
) {
  let length = match name {
    "apply" | "set" | "defineProperty" => 3.0,
    "construct"
    | "deleteProperty"
    | "get"
    | "getOwnPropertyDescriptor"
    | "has"
    | "setPrototypeOf" => 2.0,
    "getPrototypeOf" | "isExtensible" | "ownKeys" | "preventExtensions" => 1.0,
    _ => 0.0,
  };
  let function = function(implementation, function_prototype);
  set_builtin_name(&function, name);
  set_builtin_length(&function, length);
  object
    .borrow_mut()
    .properties
    .insert(key(name), PropertyDescriptor::builtin_data(function));
}

pub(crate) struct ReflectIntrinsics {
  pub(crate) object: Rc<RefCell<ObjectData>>,
}

#[allow(clippy::too_many_lines)]
pub(crate) fn create_reflect_intrinsics(
  object_prototype: &Rc<RefCell<ObjectData>>,
  function_prototype: &Rc<RefCell<ObjectData>>,
) -> ReflectIntrinsics {
  let object = Rc::new(RefCell::new(ObjectData::with_prototype(Value::Object(
    Rc::clone(object_prototype),
  ))));
  object.borrow_mut().properties.insert(
    PropertyKey::Symbol(SymbolId::well_known_to_string_tag()),
    PropertyDescriptor::Data {
      value: Value::String("Reflect".into()),
      writable: false,
      enumerable: false,
      configurable: true,
    },
  );
  install(
    &object,
    function_prototype,
    "apply",
    Rc::new(|vm, args| {
      let offset = static_builtin_arg_offset(vm, args, "Reflect");
      let target = args
        .get(offset)
        .cloned()
        .unwrap_or(Value::Undefined);
      let this_arg = args
        .get(offset + 1)
        .cloned()
        .unwrap_or(Value::Undefined);
      let arguments_list = args
        .get(offset + 2)
        .cloned()
        .unwrap_or(Value::Undefined);
      if !Vm::is_callable_value(&target) {
        return Err(vm.throw_type_error("Reflect.apply: target is not callable"));
      }
      let call_args = create_list_from_array_like(vm, &arguments_list)?;
      vm.call_value_with_this(&target, &this_arg, &call_args)
    }),
  );
  install(
    &object,
    function_prototype,
    "construct",
    Rc::new(|vm, args| {
      let offset = static_builtin_arg_offset(vm, args, "Reflect");
      let target = args
        .get(offset)
        .cloned()
        .unwrap_or(Value::Undefined);
      let arg_list = args
        .get(offset + 1)
        .cloned()
        .unwrap_or(Value::Undefined);
      let new_target = args
        .get(offset + 2)
        .cloned()
        .unwrap_or_else(|| target.clone());
      let constructor = if matches!(new_target, Value::Undefined) {
        &target
      } else {
        &new_target
      };
      if !target.is_constructable() {
        return Err(vm.throw_type_error("Reflect.construct: target is not a constructor"));
      }
      if !constructor.is_constructable() {
        return Err(vm.throw_type_error("Reflect.construct: newTarget is not a constructor"));
      }
      let arguments = create_list_from_array_like(vm, &arg_list)?;
      let result = vm.construct_value_with_new_target(&target, &arguments, new_target.clone())?;
      if matches!(new_target, Value::Undefined) {
        return Ok(result);
      }
      let new_target_prototype = vm
        .get_property_host(&new_target, &key("prototype"))
        .unwrap_or(Value::Undefined);
      let mut desired_prototype = match new_target_prototype {
        Value::Object(_)
        | Value::Array(..)
        | Value::Closure { .. }
        | Value::BuiltinObject(_, _) => Some(new_target_prototype),
        _ => None,
      };
      if desired_prototype.is_none() {
        let constructor_name = vm
          .get_property_host(&target, &key("name"))
          .ok()
          .and_then(|value| match value {
            Value::String(name) => Some(name),
            _ => None,
          })
          .or_else(|| {
            let intrinsics = &vm.realm().intrinsics;
            if intrinsics
              .string_constructor
              .as_ref()
              .is_some_and(|value| strict_equal(value, &target))
            {
              Some("String".into())
            } else if intrinsics
              .array_constructor
              .as_ref()
              .is_some_and(|value| strict_equal(value, &target))
            {
              Some("Array".into())
            } else if intrinsics
              .object_constructor
              .as_ref()
              .is_some_and(|value| strict_equal(value, &target))
            {
              Some("Object".into())
            } else if intrinsics
              .number_constructor
              .as_ref()
              .is_some_and(|value| strict_equal(value, &target))
            {
              Some("Number".into())
            } else if intrinsics
              .promise_constructor
              .as_ref()
              .is_some_and(|value| strict_equal(value, &target))
            {
              Some("Promise".into())
            } else {
              None
            }
          });
        let realm_globals = Vm::function_realm_globals(&new_target);
        if let (Some(name), Some(Value::Object(globals))) = (constructor_name, realm_globals) {
          if name.to_string_lossy() == "GeneratorFunction"
            && let Some((intrinsics, _)) = crate::runtime_utils::realm_kernel(&globals)
            && let Some(prototype) = intrinsics.generator_function_prototype
          {
            desired_prototype = Some(prototype);
          }
          let (constructor_in_realm, intl_namespace) = {
            let globals = globals.borrow();
            let constructor = globals
              .properties
              .get(&PropertyKey::String(name.clone()))
              .and_then(|descriptor| match descriptor {
                PropertyDescriptor::Data { value, .. } => Some(value.clone()),
                PropertyDescriptor::Accessor { .. } => None,
              });
            let intl =
              globals
                .properties
                .get(&key("Intl"))
                .and_then(|descriptor| match descriptor {
                  PropertyDescriptor::Data { value, .. } => Some(value.clone()),
                  PropertyDescriptor::Accessor { .. } => None,
                });
            (constructor, intl)
          };
          let constructor_in_realm = constructor_in_realm.or_else(|| {
            let Value::Object(intl) = intl_namespace? else {
              return None;
            };
            intl
              .borrow()
              .properties
              .get(&PropertyKey::String(name))
              .and_then(|descriptor| match descriptor {
                PropertyDescriptor::Data { value, .. } => Some(value.clone()),
                PropertyDescriptor::Accessor { .. } => None,
              })
          });
          if let Some(constructor_in_realm) = constructor_in_realm {
            let prototype = vm
              .get_property_host(&constructor_in_realm, &key("prototype"))
              .unwrap_or(Value::Undefined);
            if matches!(
              prototype,
              Value::Object(_)
                | Value::Array(..)
                | Value::Closure { .. }
                | Value::BuiltinObject(_, _)
            ) {
              desired_prototype = Some(prototype);
            }
          }
        }
      }
      if let Some(prototype) = desired_prototype {
        let _ = vm.set_prototype_of_value(&result, &prototype)?;
      }
      Ok(result)
    }),
  );
  install(
    &object,
    function_prototype,
    "ownKeys",
    Rc::new(|vm, args| {
      let target = args.get(1).cloned().unwrap_or(Value::Undefined);
      if !target.is_object() {
        return Err(vm.throw_type_error("Reflect.ownKeys called on non-object"));
      }
      let keys = vm
        .own_property_keys_value(&target)?
        .into_iter()
        .map(|key| property_key_to_value(&key))
        .collect();
      Ok(array_from_values(keys))
    }),
  );
  install(
    &object,
    function_prototype,
    "deleteProperty",
    Rc::new(|vm, args| {
      let offset = static_builtin_arg_offset(vm, args, "Reflect");
      let target = args
        .get(offset)
        .cloned()
        .unwrap_or(Value::Undefined);
      let property = args
        .get(offset + 1)
        .cloned()
        .unwrap_or(Value::Undefined);
      if !target.is_object() {
        return Err(vm.throw_type_error("Reflect.deleteProperty called on non-object"));
      }
      let property = vm.to_property_key(&property)?;
      Ok(Value::Bool(vm.delete_property_host(&target, &property)?))
    }),
  );
  install(
    &object,
    function_prototype,
    "defineProperty",
    Rc::new(|vm, args| {
      let offset = static_builtin_arg_offset(vm, args, "Reflect");
      let target = args
        .get(offset)
        .cloned()
        .unwrap_or(Value::Undefined);
      let property = args
        .get(offset + 1)
        .cloned()
        .unwrap_or(Value::Undefined);
      let descriptor = args
        .get(offset + 2)
        .cloned()
        .unwrap_or(Value::Undefined);
      Ok(Value::Bool(vm.define_property_value(
        &target,
        &property,
        &descriptor,
      )?))
    }),
  );
  install(
    &object,
    function_prototype,
    "get",
    Rc::new(|vm, args| {
      let offset = static_builtin_arg_offset(vm, args, "Reflect");
      let target = args
        .get(offset)
        .cloned()
        .unwrap_or(Value::Undefined);
      let property = args
        .get(offset + 1)
        .cloned()
        .unwrap_or(Value::Undefined);
      let receiver = args
        .get(offset + 2)
        .cloned()
        .unwrap_or_else(|| target.clone());
      if !target.is_object() {
        return Err(vm.throw_type_error("Reflect.get called on non-object"));
      }
      let property = vm.to_property_key(&property)?;
      vm.get_property_with_receiver(&target, &property, &receiver)
    }),
  );
  let object_prototype = Rc::clone(object_prototype);
  install(
    &object,
    function_prototype,
    "getOwnPropertyDescriptor",
    Rc::new(move |vm, args| {
      let offset = static_builtin_arg_offset(vm, args, "Reflect");
      let target = args
        .get(offset)
        .cloned()
        .unwrap_or(Value::Undefined);
      let property = args
        .get(offset + 1)
        .cloned()
        .unwrap_or(Value::Undefined);
      if !target.is_object() {
        return Err(vm.throw_type_error("Reflect.getOwnPropertyDescriptor called on non-object"));
      }
      let property = vm.to_property_key(&property)?;
      let Some(descriptor) = vm.get_own_property_value(&target, &property)? else {
        return Ok(Value::Undefined);
      };
      Ok(descriptor_value_from_descriptor(
        &object_prototype,
        &descriptor,
      ))
    }),
  );
  install(
    &object,
    function_prototype,
    "getPrototypeOf",
    Rc::new(|vm, args| {
      let offset = static_builtin_arg_offset(vm, args, "Reflect");
      let target = args
        .get(offset)
        .cloned()
        .unwrap_or(Value::Undefined);
      if !target.is_object() {
        return Err(vm.throw_type_error("Reflect.getPrototypeOf called on non-object"));
      }
      vm.get_prototype_of_value(&target)
    }),
  );
  install(
    &object,
    function_prototype,
    "has",
    Rc::new(|vm, args| {
      let offset = static_builtin_arg_offset(vm, args, "Reflect");
      let target = args
        .get(offset)
        .cloned()
        .unwrap_or(Value::Undefined);
      let property = args
        .get(offset + 1)
        .cloned()
        .unwrap_or(Value::Undefined);
      if !target.is_object() {
        return Err(vm.throw_type_error("Reflect.has called on non-object"));
      }
      let property = vm.to_property_key(&property)?;
      Ok(Value::Bool(vm.has_property_host(&target, &property)?))
    }),
  );
  install(
    &object,
    function_prototype,
    "isExtensible",
    Rc::new(|vm, args| {
      let offset = static_builtin_arg_offset(vm, args, "Reflect");
      let target = args
        .get(offset)
        .cloned()
        .unwrap_or(Value::Undefined);
      if !target.is_object() {
        return Err(vm.throw_type_error("Reflect.isExtensible called on non-object"));
      }
      Ok(Value::Bool(vm.is_extensible_value(&target)?))
    }),
  );
  install(
    &object,
    function_prototype,
    "preventExtensions",
    Rc::new(|vm, args| {
      let offset = static_builtin_arg_offset(vm, args, "Reflect");
      let target = args
        .get(offset)
        .cloned()
        .unwrap_or(Value::Undefined);
      if !target.is_object() {
        return Err(vm.throw_type_error("Reflect.preventExtensions called on non-object"));
      }
      Ok(Value::Bool(vm.prevent_extensions_value(&target)?))
    }),
  );
  install(
    &object,
    function_prototype,
    "setPrototypeOf",
    Rc::new(|vm, args| {
      let offset = static_builtin_arg_offset(vm, args, "Reflect");
      let target = args
        .get(offset)
        .cloned()
        .unwrap_or(Value::Undefined);
      let prototype = args
        .get(offset + 1)
        .cloned()
        .unwrap_or(Value::Undefined);
      if !target.is_object() {
        return Err(vm.throw_type_error("Reflect.setPrototypeOf called on non-object"));
      }
      if !matches!(prototype, Value::Null) && !prototype.is_object() {
        return Err(vm.throw_type_error("Reflect.setPrototypeOf prototype must be object or null"));
      }
      Ok(Value::Bool(vm.set_prototype_of_value(&target, &prototype)?))
    }),
  );
  install(
    &object,
    function_prototype,
    "set",
    Rc::new(|vm, args| {
      let offset = static_builtin_arg_offset(vm, args, "Reflect");
      let target = args
        .get(offset)
        .cloned()
        .unwrap_or(Value::Undefined);
      let property = args
        .get(offset + 1)
        .cloned()
        .unwrap_or(Value::Undefined);
      let value = args
        .get(offset + 2)
        .cloned()
        .unwrap_or(Value::Undefined);
      let receiver = args
        .get(offset + 3)
        .cloned()
        .unwrap_or_else(|| target.clone());
      if !target.is_object() {
        return Err(vm.throw_type_error("Reflect.set called on non-object"));
      }
      let property = property_key_to_value(&vm.to_property_key(&property)?);
      Ok(Value::Bool(vm.reflect_set_property_host(
        &target,
        &property,
        value,
        Some(receiver),
      )?))
    }),
  );

  ReflectIntrinsics { object }
}
