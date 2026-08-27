// SPDX-License-Identifier: MPL-2.0

use super::*;

impl Vm {
  /// Run the `[[IsExtensible]]` internal method for an object value.
  pub(crate) fn is_extensible_value(
    &mut self,
    obj: &Value,
  ) -> Result<bool, RuntimeError> {
    let Value::Proxy(proxy) = obj else {
      return Ok(object_is_extensible(obj));
    };

    let (target, handler) = proxy_target_and_handler(self, proxy, "isExtensible")?;
    let trap = self.get_property_value(
      &handler,
      &PropertyKey::String("isExtensible".to_string().into()),
    )?;
    if matches!(trap, Value::Undefined | Value::Null) {
      return self.is_extensible_value(&target);
    }
    if !Self::is_callable_value(&trap) {
      return Err(self.throw_type_error("Proxy isExtensible trap is not callable"));
    }

    let trap_result = self.call_with_this(&trap, &handler, std::slice::from_ref(&target))?;
    let extensible = self.to_boolean(&trap_result);
    if extensible != self.is_extensible_value(&target)? {
      return Err(
        self.throw_type_error("Proxy isExtensible trap result does not match the target"),
      );
    }
    Ok(extensible)
  }

  /// Run the `[[PreventExtensions]]` internal method for an object value.
  pub(crate) fn prevent_extensions_value(
    &mut self,
    obj: &Value,
  ) -> Result<bool, RuntimeError> {
    if let Value::Proxy(proxy) = obj {
      let (target, handler) = proxy_target_and_handler(self, proxy, "preventExtensions")?;
      let trap = self.get_property_value(
        &handler,
        &PropertyKey::String("preventExtensions".to_string().into()),
      )?;
      if matches!(trap, Value::Undefined | Value::Null) {
        return self.prevent_extensions_value(&target);
      }
      if !Self::is_callable_value(&trap) {
        return Err(self.throw_type_error("Proxy preventExtensions trap is not callable"));
      }

      let trap_result = self.call_with_this(&trap, &handler, std::slice::from_ref(&target))?;
      if !self.to_boolean(&trap_result) {
        return Ok(false);
      }
      if self.is_extensible_value(&target)? {
        return Err(self.throw_type_error(
          "Proxy preventExtensions trap reported success for an extensible target",
        ));
      }
      return Ok(true);
    }

    Ok(set_object_extensible(obj, false))
  }

  /// Run the `[[GetPrototypeOf]]` internal method for an object value.
  pub(crate) fn get_prototype_of_value(
    &mut self,
    obj: &Value,
  ) -> Result<Value, RuntimeError> {
    let Value::Proxy(proxy) = obj else {
      return Ok(self.get_prototype(obj));
    };

    let (target, handler) = proxy_target_and_handler(self, proxy, "getPrototypeOf")?;
    let trap = self.get_property_value(
      &handler,
      &PropertyKey::String("getPrototypeOf".to_string().into()),
    )?;
    if matches!(trap, Value::Undefined | Value::Null) {
      return self.get_prototype_of_value(&target);
    }
    if !Self::is_callable_value(&trap) {
      return Err(self.throw_type_error("Proxy getPrototypeOf trap is not callable"));
    }

    let trap_result = self.call_with_this(&trap, &handler, std::slice::from_ref(&target))?;
    if !matches!(trap_result, Value::Null) && !trap_result.is_object() {
      return Err(self.throw_type_error("Proxy getPrototypeOf trap must return an object or null"));
    }
    if !self.is_extensible_value(&target)? {
      let target_proto = self.get_prototype_of_value(&target)?;
      if !same_value(&trap_result, &target_proto) {
        return Err(self.throw_type_error(
          "Proxy getPrototypeOf trap cannot report a different prototype for a non-extensible target",
        ));
      }
    }
    Ok(trap_result)
  }

  /// Run the `[[SetPrototypeOf]]` internal method for an object value.
  pub(crate) fn set_prototype_of_value(
    &mut self,
    obj: &Value,
    proto: &Value,
  ) -> Result<bool, RuntimeError> {
    if let Value::Proxy(proxy) = obj {
      let (target, handler) = proxy_target_and_handler(self, proxy, "setPrototypeOf")?;
      let trap = self.get_property_value(
        &handler,
        &PropertyKey::String("setPrototypeOf".to_string().into()),
      )?;
      if matches!(trap, Value::Undefined | Value::Null) {
        return self.set_prototype_of_value(&target, proto);
      }
      if !Self::is_callable_value(&trap) {
        return Err(self.throw_type_error("Proxy setPrototypeOf trap is not callable"));
      }

      let trap_result = self.call_with_this(&trap, &handler, &[target.clone(), proto.clone()])?;
      if !self.to_boolean(&trap_result) {
        return Ok(false);
      }
      if !self.is_extensible_value(&target)? {
        let target_proto = self.get_prototype_of_value(&target)?;
        if !same_value(proto, &target_proto) {
          return Err(self.throw_type_error(
            "Proxy setPrototypeOf trap cannot change the prototype of a non-extensible target",
          ));
        }
      }
      return Ok(true);
    }

    let current = self.get_prototype(obj);
    if same_value(&current, proto) {
      return Ok(true);
    }
    if same_value(obj, &self.realm().object_prototype()) {
      return Ok(false);
    }
    if !object_is_extensible(obj) {
      return Ok(false);
    }

    let mut candidate = proto.clone();
    for _ in 0..MAX_PROTOTYPE_DEPTH {
      if matches!(candidate, Value::Null) {
        break;
      }
      if same_value(obj, &candidate) {
        return Ok(false);
      }
      // OrdinarySetPrototypeOf only walks ordinary objects when checking for a
      // cycle. An exotic object is responsible for enforcing its own
      // [[SetPrototypeOf]] invariants and must not have [[GetPrototypeOf]]
      // invoked as a side effect of changing another object's prototype.
      if matches!(candidate, Value::Proxy(_)) {
        break;
      }
      candidate = self.get_prototype_of_value(&candidate)?;
    }
    Ok(set_object_prototype(obj, proto.clone()))
  }

  pub(crate) fn set_integrity_level(
    &mut self,
    obj: &Value,
    frozen: bool,
  ) -> Result<bool, RuntimeError> {
    if !self.prevent_extensions_value(obj)? {
      return Ok(false);
    }
    for key in self.own_property_keys_value(obj)? {
      let mut desc = PartialDescriptor {
        configurable: Some(false),
        has_configurable: true,
        ..PartialDescriptor::default()
      };
      if frozen
        && matches!(
          self.get_own_property_value(obj, &key)?,
          Some(PropertyDescriptor::Data { .. })
        )
      {
        desc.writable = Some(false);
        desc.has_writable = true;
      }
      if !self.define_own_property(obj, key, desc)? {
        return Ok(false);
      }
    }
    Ok(true)
  }

  pub(crate) fn test_integrity_level(
    &mut self,
    obj: &Value,
    frozen: bool,
  ) -> Result<bool, RuntimeError> {
    if self.is_extensible_value(obj)? {
      return Ok(false);
    }
    for key in self.own_property_keys_value(obj)? {
      let Some(desc) = self.get_own_property_value(obj, &key)? else {
        continue;
      };
      if descriptor_configurable(&desc)
        || frozen && matches!(desc, PropertyDescriptor::Data { writable: true, .. })
      {
        return Ok(false);
      }
    }
    Ok(true)
  }

  /// Run the `[[GetOwnProperty]]` internal method for an object value.
  pub(crate) fn get_own_property_value(
    &mut self,
    obj: &Value,
    key: &PropertyKey,
  ) -> Result<Option<PropertyDescriptor>, RuntimeError> {
    let Value::Proxy(proxy) = obj else {
      if let Value::Object(object) = obj
        && let PropertyKey::String(name) = key
        && object
          .borrow()
          .module_namespace_exports
          .as_ref()
          .and_then(|exports| exports.get(name))
          .is_some_and(crate::binding::BindingCell::is_uninitialized)
      {
        return Err(
          self.throw_reference_error("Cannot access module binding before initialization"),
        );
      }
      return Ok(own_property_descriptor(obj, key));
    };

    let (target, handler) = proxy_target_and_handler(self, proxy, "getOwnPropertyDescriptor")?;
    let trap = self.get_property_value(
      &handler,
      &PropertyKey::String("getOwnPropertyDescriptor".to_string().into()),
    )?;
    if matches!(trap, Value::Undefined | Value::Null) {
      return self.get_own_property_value(&target, key);
    }
    if !Self::is_callable_value(&trap) {
      return Err(self.throw_type_error("Proxy getOwnPropertyDescriptor trap is not callable"));
    }

    let trap_result = self.call_with_this(
      &trap,
      &handler,
      &[target.clone(), property_key_to_value(key)],
    )?;
    if !matches!(trap_result, Value::Undefined) && !trap_result.is_object() {
      return Err(self.throw_type_error(
        "Proxy getOwnPropertyDescriptor trap must return an object or undefined",
      ));
    }

    let target_desc = self.get_own_property_value(&target, key)?;
    let extensible = self.is_extensible_value(&target)?;
    if matches!(trap_result, Value::Undefined) {
      let Some(target_desc) = target_desc else {
        return Ok(None);
      };
      if !descriptor_configurable(&target_desc) {
        return Err(self.throw_type_error(
          "Proxy getOwnPropertyDescriptor trap cannot hide a non-configurable property",
        ));
      }
      if !extensible {
        return Err(self.throw_type_error(
          "Proxy getOwnPropertyDescriptor trap cannot hide a property of a non-extensible target",
        ));
      }
      return Ok(None);
    }

    let result_desc = complete_property_descriptor(self.read_descriptor_from_value(&trap_result)?);
    let result_partial = partial_descriptor_from_complete(&result_desc);
    if !is_compatible_property_descriptor(extensible, &result_partial, target_desc.as_ref()) {
      return Err(self.throw_type_error(
        "Proxy getOwnPropertyDescriptor trap returned an incompatible descriptor",
      ));
    }
    if !descriptor_configurable(&result_desc) {
      if target_desc
        .as_ref()
        .is_none_or(descriptor_configurable)
      {
        return Err(self.throw_type_error(
          "Proxy getOwnPropertyDescriptor trap cannot report a new non-configurable property",
        ));
      }
      if matches!(
        result_desc,
        PropertyDescriptor::Data {
          writable: false,
          ..
        }
      ) && matches!(
        target_desc,
        Some(PropertyDescriptor::Data { writable: true, .. })
      ) {
        return Err(self.throw_type_error(
          "Proxy getOwnPropertyDescriptor trap cannot report a non-writable property for a writable non-configurable target property",
        ));
      }
    }
    Ok(Some(result_desc))
  }

  /// Run the `[[OwnPropertyKeys]]` internal method for an object value.
  pub(crate) fn own_property_keys_value(
    &mut self,
    obj: &Value,
  ) -> Result<Vec<PropertyKey>, RuntimeError> {
    let Value::Proxy(proxy) = obj else {
      return Ok(own_property_keys(obj));
    };

    let (target, handler) = {
      let proxy = proxy.borrow();
      let Some(handler) = proxy.handler.clone() else {
        return Err(self.throw_type_error("Cannot perform 'ownKeys' on a revoked proxy"));
      };
      (proxy.target.clone(), handler)
    };
    let trap =
      self.get_property_value(&handler, &PropertyKey::String("ownKeys".to_string().into()))?;
    if matches!(trap, Value::Undefined | Value::Null) {
      return self.own_property_keys_value(&target);
    }
    if !Self::is_callable_value(&trap) {
      return Err(self.throw_type_error("Proxy ownKeys trap is not callable"));
    }

    let trap_result = self.call_with_this(&trap, &handler, std::slice::from_ref(&target))?;
    if !trap_result.is_object() {
      return Err(self.throw_type_error("Proxy ownKeys trap must return an object"));
    }
    let length = self.get_property_value(
      &trap_result,
      &PropertyKey::String("length".to_string().into()),
    )?;
    let length = to_length(self, &length)?;
    let mut trap_keys = Vec::with_capacity(length);
    for index in 0..length {
      let key =
        self.get_property_value(&trap_result, &PropertyKey::String(index.to_string().into()))?;
      let key = match key {
        Value::String(name) => PropertyKey::String(name),
        Value::Symbol(id, _) => PropertyKey::Symbol(id),
        _ => {
          return Err(
            self
              .throw_type_error("Proxy ownKeys trap result must contain only strings and symbols"),
          );
        }
      };
      if trap_keys.contains(&key) {
        return Err(self.throw_type_error("Proxy ownKeys trap returned duplicate entries"));
      }
      trap_keys.push(key);
    }

    let target_keys = self.own_property_keys_value(&target)?;
    let mut unchecked = trap_keys.clone();
    let mut configurable_target_keys = Vec::new();
    for key in &target_keys {
      let non_configurable = self
        .get_own_property_value(&target, key)?
        .is_some_and(|desc| !descriptor_configurable(&desc));
      if non_configurable {
        if !remove_key(&mut unchecked, key) {
          return Err(
            self.throw_type_error("Proxy ownKeys trap omitted a non-configurable property"),
          );
        }
      } else {
        configurable_target_keys.push(key);
      }
    }

    if self.is_extensible_value(&target)? {
      return Ok(trap_keys);
    }
    for key in configurable_target_keys {
      if !remove_key(&mut unchecked, key) {
        return Err(
          self.throw_type_error("Proxy ownKeys trap omitted a property of a non-extensible target"),
        );
      }
    }
    if !unchecked.is_empty() {
      return Err(
        self.throw_type_error("Proxy ownKeys trap added a property to a non-extensible target"),
      );
    }
    Ok(trap_keys)
  }

  /// Return enumerable own property keys through `[[OwnPropertyKeys]]`.
  pub(crate) fn own_enumerable_keys_value(
    &mut self,
    obj: &Value,
  ) -> Result<Vec<PropertyKey>, RuntimeError> {
    let mut enumerable = Vec::new();
    for key in self.own_property_keys_value(obj)? {
      if self
        .get_own_property_value(obj, &key)?
        .is_some_and(|desc| descriptor_enumerable(&desc))
      {
        enumerable.push(key);
      }
    }
    Ok(enumerable)
  }

  /// Return enumerable own string keys through `[[OwnPropertyKeys]]`.
  pub(crate) fn own_enumerable_string_keys_value(
    &mut self,
    obj: &Value,
  ) -> Result<Vec<PropertyKey>, RuntimeError> {
    let mut enumerable = Vec::new();
    for key in self.own_property_keys_value(obj)? {
      if !matches!(key, PropertyKey::String(_)) {
        continue;
      }
      if self
        .get_own_property_value(obj, &key)?
        .is_some_and(|desc| descriptor_enumerable(&desc))
      {
        enumerable.push(key);
      }
    }
    Ok(enumerable)
  }
}
