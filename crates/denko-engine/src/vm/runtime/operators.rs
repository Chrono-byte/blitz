// SPDX-License-Identifier: MPL-2.0

use crate::errors::RuntimeError;
use crate::value::{PropertyKey, SymbolId, Value};
use crate::vm::Vm;
use crate::vm::runtime::property::{object_identity, values_equal};
use std::collections::HashSet;

impl Vm {
  pub fn instance_of(
    &mut self,
    value: &Value,
    constructor: &Value,
  ) -> Result<bool, RuntimeError> {
    let has_instance_key = PropertyKey::Symbol(SymbolId::well_known_has_instance());
    let method = self.get_property_host(constructor, &has_instance_key)?;
    if !matches!(method, Value::Undefined | Value::Null) {
      if !method.is_callable() {
        return Err(self.throw_type_error("Symbol.hasInstance property is not callable"));
      }
      let result = self.call_with_this(&method, constructor, std::slice::from_ref(value))?;
      return Ok(result.is_truthy());
    }
    if !constructor.is_callable() {
      return Err(self.throw_type_error("Right-hand side of instanceof is not callable"));
    }
    self.ordinary_has_instance(constructor, value)
  }

  /// ECMAScript `OrdinaryHasInstance`, also exposed by
  /// `Function.prototype[Symbol.hasInstance]`.
  pub(crate) fn ordinary_has_instance(
    &mut self,
    constructor: &Value,
    value: &Value,
  ) -> Result<bool, RuntimeError> {
    if !constructor.is_callable() {
      return Ok(false);
    }
    if let Value::BuiltinObject(_, properties) = constructor
      && let Some(bound_target) = properties
        .borrow()
        .internal_slot("[[BoundTargetFunction]]")
        .cloned()
    {
      return self.instance_of(value, &bound_target);
    }
    if !value.is_object() {
      return Ok(false);
    }

    let proto_key = PropertyKey::String("prototype".to_string().into());
    let ctor_proto = self.get_property_host(constructor, &proto_key)?;
    if !ctor_proto.is_object() {
      return Err(self.throw_type_error("Function has non-object prototype in instanceof check"));
    }

    let mut current = self.get_prototype_of_value(value)?;
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
      current = self.get_prototype_of_value(&current)?;
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
      | Value::Proxy(_)
      | Value::Promise(..) => proto.clone(),
      _ => return Ok(false),
    };

    if !value.is_object() {
      return Ok(false);
    }
    let mut current = self.get_prototype_of_value(value)?;

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
      current = self.get_prototype_of_value(&current)?;
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
    self.has_property_host(object, &key)
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
