// SPDX-License-Identifier: MPL-2.0

use crate::errors::RuntimeError;
use crate::value::{PropertyKey, SymbolId, Value};
use crate::vm::Vm;
use crate::vm::runtime::property::{get_property, object_identity, values_equal};
use std::collections::HashSet;

impl Vm {
  pub fn instance_of(
    &mut self,
    value: &Value,
    constructor: &Value,
  ) -> Result<bool, RuntimeError> {
    // Per spec: if constructor has @@hasInstance, call it.
    let has_instance_key = PropertyKey::Symbol(SymbolId::well_known_has_instance());
    if let Ok(method) = get_property(constructor, &has_instance_key, 0)
      && matches!(
        method,
        Value::Closure { .. } | Value::Builtin(_) | Value::BuiltinObject(_, _)
      )
    {
      let result = self.call_with_this(&method, constructor, std::slice::from_ref(value))?;
      return Ok(result.is_truthy());
    }

    // Only objects have prototype chains.
    match value {
      Value::Object(_)
      | Value::Array(..)
      | Value::Closure { .. }
      | Value::BuiltinObject(_, _)
      | Value::Promise(_) => {}
      _ => return Ok(false),
    }

    // Get constructor.prototype
    let proto_key = PropertyKey::String("prototype".to_string().into());
    let ctor_proto = get_property(constructor, &proto_key, 0)?;
    // prototype must be an object
    match ctor_proto {
      Value::Object(_) | Value::Array(..) | Value::Closure { .. } | Value::BuiltinObject(_, _) => {}
      _ => return Ok(false),
    }

    // Walk value's prototype chain
    let mut current = match value {
      Value::Object(d) => d.borrow().get_prototype(),
      Value::Array(_, props) => props.borrow().get_prototype(),
      Value::Closure { props, .. } => props.borrow().get_prototype(),
      Value::BuiltinObject(_, props) => props.borrow().get_prototype(),
      Value::Promise(_) => self.promise_prototype_value(),
      _ => Value::Null,
    };
    let mut seen: HashSet<usize> = HashSet::new();

    while !matches!(current, Value::Null) {
      if values_equal(&current, &ctor_proto) {
        return Ok(true);
      }
      let Some(identity) = object_identity(&current) else {
        break;
      };
      if !seen.insert(identity) {
        return Ok(false);
      }
      // advance
      current = match &current {
        Value::Object(d) => d.borrow().get_prototype(),
        Value::Array(_, props) => props.borrow().get_prototype(),
        Value::Closure { props, .. } => props.borrow().get_prototype(),
        Value::BuiltinObject(_, props) => props.borrow().get_prototype(),
        _ => Value::Null,
      };
    }

    Ok(false)
  }

  /// ECMAScript `Object.prototype.isPrototypeOf` semantics.
  pub fn is_prototype_of(
    &mut self,
    proto: &Value,
    value: &Value,
  ) -> Result<bool, RuntimeError> {
    let target_proto = match proto {
      Value::Object(_)
      | Value::Array(..)
      | Value::Closure { .. }
      | Value::BuiltinObject(_, _)
      | Value::Promise(_) => proto.clone(),
      _ => return Ok(false),
    };

    let mut current = match value {
      Value::Object(d) => d.borrow().get_prototype(),
      Value::Array(_, props) => props.borrow().get_prototype(),
      Value::Closure { props, .. } => props.borrow().get_prototype(),
      Value::BuiltinObject(_, props) => props.borrow().get_prototype(),
      Value::Promise(_) => self.promise_prototype_value(),
      _ => return Ok(false),
    };

    let mut seen: HashSet<usize> = HashSet::new();
    while !matches!(current, Value::Null) {
      if values_equal(&current, &target_proto) {
        return Ok(true);
      }
      let Some(identity) = object_identity(&current) else {
        break;
      };
      if !seen.insert(identity) {
        return Ok(false);
      }
      current = match &current {
        Value::Object(d) => d.borrow().get_prototype(),
        Value::Array(_, props) => props.borrow().get_prototype(),
        Value::Closure { props, .. } => props.borrow().get_prototype(),
        Value::BuiltinObject(_, props) => props.borrow().get_prototype(),
        _ => Value::Null,
      };
    }
    Ok(false)
  }

  /// 'in' operator helper. Returns true if property (after ToPropertyKey) is in object.
  ///
  /// # Errors
  ///
  /// Returns a [`crate::errors::RuntimeError`] if the operation fails.
  pub fn in_property(
    &mut self,
    property: &Value,
    object: &Value,
  ) -> Result<bool, RuntimeError> {
    let key = self.to_property_key(property)?;
    Ok(
      self
        .lookup_property_descriptor(object, &key, 0)?
        .is_some(),
    )
  }

  /// typeof helper returning the type string for a value.
  pub fn typeof_value(
    &self,
    value: &Value,
  ) -> String {
    // `typeof null` is 'object'
    value.type_name().to_string()
  }
}
