use crate::ecmascript::semantics::properties::{
  own_property_descriptor, own_property_keys, property_key_to_value,
};
use crate::errors::RuntimeError;
use crate::runtime_utils::{to_length, usize_to_f64};
use crate::value::{ObjectData, PropertyDescriptor, PropertyKey, Value};
use crate::vm::Vm;
use crate::vm::runtime::execution::{
  array_index_from_key, array_length, get_property, same_value, set_property, values_equal,
};
use crate::vm::state::{MAX_PROTOTYPE_DEPTH, PartialDescriptor, PropertyGetIcEntry};

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
    let trap =
      self.get_property_value(&handler, &PropertyKey::String("isExtensible".to_string()))?;
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
        &PropertyKey::String("preventExtensions".to_string()),
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
    let trap =
      self.get_property_value(&handler, &PropertyKey::String("getPrototypeOf".to_string()))?;
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
      let trap =
        self.get_property_value(&handler, &PropertyKey::String("setPrototypeOf".to_string()))?;
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
      return Ok(own_property_descriptor(obj, key));
    };

    let (target, handler) = proxy_target_and_handler(self, proxy, "getOwnPropertyDescriptor")?;
    let trap = self.get_property_value(
      &handler,
      &PropertyKey::String("getOwnPropertyDescriptor".to_string()),
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
    if !descriptor_configurable(&result_desc)
      && target_desc
        .as_ref()
        .is_none_or(descriptor_configurable)
    {
      return Err(self.throw_type_error(
        "Proxy getOwnPropertyDescriptor trap cannot report a new non-configurable property",
      ));
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
    let trap = self.get_property_value(&handler, &PropertyKey::String("ownKeys".to_string()))?;
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
    let length =
      self.get_property_value(&trap_result, &PropertyKey::String("length".to_string()))?;
    let length = to_length(self, &length)?;
    let mut trap_keys = Vec::with_capacity(length);
    for index in 0..length {
      let key = self.get_property_value(&trap_result, &PropertyKey::String(index.to_string()))?;
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

  /// Read a property through the per-instruction monomorphic cache.
  ///
  /// Only own data properties are cached. Prototype and accessor reads stay
  /// on the semantic path so prototype mutation and getter side effects can
  /// never be hidden by the cache.
  pub(super) fn get_property_value_cached(
    &mut self,
    instruction_ip: usize,
    obj: &Value,
    key: &PropertyKey,
  ) -> Result<Value, RuntimeError> {
    let receiver_identity = match obj {
      // Dense array elements and `length` are mutated by native array
      // methods as well as ordinary property writes. Until those mutations
      // all flow through the tracked property path, caching array reads can
      // retain a stale length or element value.
      Value::Object(_) | Value::Closure { .. } | Value::BuiltinObject(_, _) => {
        super::execution::object_identity(obj)
      }
      _ => None,
    };
    let Some(receiver_identity) = receiver_identity else {
      return self.get_property_value(obj, key);
    };
    let receiver_generation = self
      .obj_generations
      .get(&receiver_identity)
      .copied()
      .unwrap_or(0);
    let site = (std::sync::Arc::as_ptr(&self.chunk) as usize, instruction_ip);

    if let Some(entry) = self.property_get_ic.get(&site)
      && entry.receiver_identity == receiver_identity
      && entry.receiver_generation == receiver_generation
      && entry.key == *key
    {
      return Ok(entry.value.clone());
    }

    let value = self.get_property_value(obj, key)?;
    if matches!(
      own_property_descriptor(obj, key),
      Some(PropertyDescriptor::Data { .. })
    ) {
      self.property_get_ic.insert(
        site,
        PropertyGetIcEntry {
          receiver_identity,
          receiver_generation,
          key: key.clone(),
          value: value.clone(),
        },
      );
    } else {
      self.property_get_ic.remove(&site);
    }
    Ok(value)
  }

  fn get_proxy_property(
    &mut self,
    proxy_value: &Value,
    key: &PropertyKey,
    receiver: &Value,
  ) -> Result<Value, RuntimeError> {
    let Value::Proxy(proxy) = proxy_value else {
      return Err(RuntimeError::from("expected proxy value"));
    };
    let (target, handler) = {
      let proxy = proxy.borrow();
      let Some(handler) = proxy.handler.clone() else {
        return Err(self.throw_type_error("Cannot read property of revoked proxy"));
      };
      (proxy.target.clone(), handler)
    };
    let trap = self.get_property_value(&handler, &PropertyKey::String("get".to_string()))?;
    if matches!(trap, Value::Undefined | Value::Null) {
      return self.get_property_with_receiver(&target, key, receiver);
    }
    if !Self::is_callable_value(&trap) {
      return Err(self.throw_type_error("Proxy get trap is not callable"));
    }
    let key_value = match key {
      PropertyKey::String(name) => Value::String(name.clone()),
      PropertyKey::Symbol(id) => Value::Symbol(id.clone(), None),
    };
    let trap_result = self.call_with_this(
      &trap,
      &handler,
      &[target.clone(), key_value, receiver.clone()],
    )?;
    if let Some(target_desc) = self.get_own_property_value(&target, key)? {
      match target_desc {
        PropertyDescriptor::Data {
          value,
          writable: false,
          configurable: false,
          ..
        } if !same_value(&trap_result, &value) => {
          return Err(self.throw_type_error(
            "Proxy get trap must return the value of a non-writable, non-configurable target property",
          ));
        }
        PropertyDescriptor::Accessor {
          get,
          configurable: false,
          ..
        } if get
          .as_ref()
          .is_none_or(|getter| matches!(getter, Value::Undefined))
          && !matches!(trap_result, Value::Undefined) =>
        {
          return Err(self.throw_type_error(
            "Proxy get trap must return undefined for a non-configurable target property with no getter",
          ));
        }
        _ => {}
      }
    }
    Ok(trap_result)
  }

  fn set_proxy_property(
    &mut self,
    proxy_value: &Value,
    key: &PropertyKey,
    value: Value,
    receiver: &Value,
  ) -> Result<bool, RuntimeError> {
    let Value::Proxy(proxy) = proxy_value else {
      return Err(RuntimeError::from("expected proxy value"));
    };
    let (target, handler) = proxy_target_and_handler(self, proxy, "set")?;
    let trap = self.get_property_value(&handler, &PropertyKey::String("set".to_string()))?;
    if matches!(trap, Value::Undefined | Value::Null) {
      return self.set_property_value(&target, key, value, receiver);
    }
    if !Self::is_callable_value(&trap) {
      return Err(self.throw_type_error("Proxy set trap is not callable"));
    }

    let trap_result = self.call_with_this(
      &trap,
      &handler,
      &[
        target.clone(),
        property_key_to_value(key),
        value.clone(),
        receiver.clone(),
      ],
    )?;
    if !self.to_boolean(&trap_result) {
      return Ok(false);
    }

    if let Some(target_desc) = self.get_own_property_value(&target, key)? {
      match target_desc {
        PropertyDescriptor::Data {
          value: target_value,
          writable: false,
          configurable: false,
          ..
        } if !same_value(&value, &target_value) => {
          return Err(self.throw_type_error(
            "Proxy set trap cannot change a non-writable, non-configurable target property",
          ));
        }
        PropertyDescriptor::Accessor {
          set,
          configurable: false,
          ..
        } if set
          .as_ref()
          .is_none_or(|setter| matches!(setter, Value::Undefined)) =>
        {
          return Err(self.throw_type_error(
            "Proxy set trap cannot set a non-configurable target property with no setter",
          ));
        }
        _ => {}
      }
    }
    Ok(true)
  }

  fn set_property_value(
    &mut self,
    target: &Value,
    key: &PropertyKey,
    value: Value,
    receiver: &Value,
  ) -> Result<bool, RuntimeError> {
    if matches!(target, Value::Proxy(_)) {
      return self.set_proxy_property(target, key, value, receiver);
    }

    if let Some((desc, _holder)) = self.lookup_property_descriptor(target, key, 0)? {
      match desc {
        PropertyDescriptor::Accessor { set, .. } => {
          let Some(setter) = set.filter(|setter| !matches!(setter, Value::Undefined)) else {
            return Ok(false);
          };
          drop(self.call_with_this(&setter, receiver, &[value])?);
          self.bump_object_generation(receiver);
          return Ok(true);
        }
        PropertyDescriptor::Data {
          writable: false, ..
        } => return Ok(false),
        PropertyDescriptor::Data { .. } => {}
      }
    }

    if !receiver.is_object() {
      return Ok(false);
    }
    if let Some(receiver_desc) = self.get_own_property_value(receiver, key)? {
      match receiver_desc {
        PropertyDescriptor::Accessor { .. } => return Ok(false),
        PropertyDescriptor::Data {
          writable: false, ..
        } => return Ok(false),
        PropertyDescriptor::Data { .. } => {
          let accepted = self.define_own_property(
            receiver,
            key.clone(),
            PartialDescriptor {
              value: Some(value),
              has_value: true,
              ..PartialDescriptor::default()
            },
          )?;
          if accepted {
            self.bump_object_generation(receiver);
          }
          return Ok(accepted);
        }
      }
    }

    let accepted = self.define_own_property(
      receiver,
      key.clone(),
      PartialDescriptor {
        value: Some(value),
        writable: Some(true),
        enumerable: Some(true),
        configurable: Some(true),
        has_value: true,
        has_writable: true,
        has_enumerable: true,
        has_configurable: true,
        ..PartialDescriptor::default()
      },
    )?;
    if accepted {
      self.bump_object_generation(receiver);
    }
    Ok(accepted)
  }

  pub(super) fn super_base_object(&self) -> Result<Value, RuntimeError> {
    let home_object = self
      .current_method_home_object()
      .ok_or_else(|| RuntimeError::from("super used outside of a method"))?;
    Ok(self.get_prototype(&home_object))
  }

  pub(super) fn get_property_with_receiver(
    &mut self,
    obj: &Value,
    key: &PropertyKey,
    receiver: &Value,
  ) -> Result<Value, RuntimeError> {
    if matches!(obj, Value::Undefined | Value::Null) {
      return Err(self.throw_type_error(&format!(
        "Cannot read property '{:?}' of {}",
        key,
        obj.type_name()
      )));
    }
    if matches!(obj, Value::Proxy(_)) {
      return self.get_proxy_property(obj, key, receiver);
    }
    if let Some((desc, _holder)) = self.lookup_property_descriptor(obj, key, 0)? {
      return match desc {
        PropertyDescriptor::Data { value, .. } => Ok(value),
        PropertyDescriptor::Accessor { get, .. } => {
          if let Some(getter) = get.filter(|getter| !matches!(getter, Value::Undefined)) {
            self.call_with_this(&getter, receiver, &[])
          } else {
            Ok(Value::Undefined)
          }
        }
      };
    }
    if let Value::String(text) = obj {
      if matches!(key, PropertyKey::String(name) if name == "length") {
        return Ok(Value::Number(usize_to_f64(text.chars().count())));
      }
      if let Some(i) = array_index_from_key(key)
        && let Some(ch) = text.chars().nth(i)
      {
        return Ok(Value::String(ch.to_string()));
      }
    }
    Ok(Value::Undefined)
  }

  pub(super) fn set_property_with_receiver(
    &mut self,
    obj: &Value,
    key: &PropertyKey,
    value: Value,
    receiver: &Value,
  ) -> Result<(), RuntimeError> {
    if matches!(obj, Value::Undefined | Value::Null) {
      return Err(self.throw_type_error(&format!(
        "Cannot set property '{:?}' of {}",
        key,
        obj.type_name()
      )));
    }
    if matches!(obj, Value::Proxy(_)) {
      let accepted = self.set_proxy_property(obj, key, value, receiver)?;
      if !accepted && self.is_strict_mode() {
        return Err(self.throw_type_error("Proxy set trap returned false"));
      }
      return Ok(());
    }
    if let Some((desc, _holder)) = self.lookup_property_descriptor(obj, key, 0)? {
      match desc {
        PropertyDescriptor::Accessor { set, .. } => {
          if let Some(setter) = set.filter(|setter| !matches!(setter, Value::Undefined)) {
            drop(self.call_with_this(&setter, receiver, &[value])?);
            self.bump_object_generation(receiver);
            return Ok(());
          }
          if self.is_strict_mode() {
            return Err(self.throw_type_error("Cannot set property with no setter"));
          }
          return Ok(());
        }
        PropertyDescriptor::Data {
          writable: false, ..
        } => {
          if self.is_strict_mode() {
            return Err(self.throw_type_error("Cannot assign to read only property"));
          }
          return Ok(());
        }
        PropertyDescriptor::Data { .. } => {
          self.set_property_tracked(receiver, key, value)?;
          return Ok(());
        }
      }
    }
    self.set_property_tracked(receiver, key, value)?;
    Ok(())
  }

  pub(super) fn get_super_property(
    &mut self,
    receiver: &Value,
    key: &Value,
  ) -> Result<(), RuntimeError> {
    let super_obj = self.super_base_object()?;
    if matches!(super_obj, Value::Null) {
      self.stack.push(Value::Undefined);
      return Ok(());
    }
    let k = self.to_property_key(key)?;
    let val = self.get_property_with_receiver(&super_obj, &k, receiver)?;
    self.stack.push(val);
    Ok(())
  }

  pub(super) fn set_super_property(
    &mut self,
    receiver: &Value,
    key: &Value,
    value: Value,
  ) -> Result<(), RuntimeError> {
    let super_obj = self.super_base_object()?;
    if matches!(super_obj, Value::Null) {
      self.stack.push(Value::Undefined);
      return Ok(());
    }
    let k = self.to_property_key(key)?;
    self.set_property_with_receiver(&super_obj, &k, value.clone(), receiver)?;
    self.stack.push(value);
    Ok(())
  }

  /// Host hook: define an own property with a descriptor.
  ///
  /// # Errors
  ///
  /// Returns a [`crate::errors::RuntimeError`] if the property definition fails.
  pub fn define_property_builtin(
    &mut self,
    args: &[Value],
  ) -> Result<Value, RuntimeError> {
    if args.len() < 3 {
      return Ok(Value::Undefined);
    }
    let target = args[0].clone();
    let ok = self.define_property_value(&target, &args[1], &args[2])?;
    if !ok {
      return Err(self.throw_type_error("Object.defineProperty failed"));
    }
    Ok(target)
  }

  /// Host hook: define an own property from explicit arguments.
  ///
  /// # Errors
  ///
  /// Returns a [`crate::errors::RuntimeError`] if the property definition fails.
  pub fn define_property_value(
    &mut self,
    target: &Value,
    key: &Value,
    descriptor: &Value,
  ) -> Result<bool, RuntimeError> {
    if !matches!(
      target,
      Value::Object(..)
        | Value::Array(..)
        | Value::Closure { .. }
        | Value::BuiltinObject(_, _)
        | Value::Promise(_)
        | Value::Proxy(_)
    ) {
      return Err(self.throw_type_error("Object.defineProperty called on non-object"));
    }
    let key = self.to_property_key(key)?;
    let desc = self.read_descriptor_from_value(descriptor)?;
    let ok = self.define_own_property(target, key, desc)?;
    if ok {
      self.bump_object_generation(target);
    }
    Ok(ok)
  }

  /// Host hook: read a property value.
  ///
  /// # Errors
  ///
  /// Returns a [`crate::errors::RuntimeError`] if the property read fails.
  pub fn get_property_host(
    &mut self,
    obj: &Value,
    key: &PropertyKey,
  ) -> Result<Value, RuntimeError> {
    self.get_property_value(obj, key)
  }

  /// Host hook: test whether a property exists on the receiver or its prototype chain.
  ///
  /// # Errors
  ///
  /// Returns a [`crate::errors::RuntimeError`] if the property lookup fails.
  pub fn has_property_host(
    &mut self,
    obj: &Value,
    key: &PropertyKey,
  ) -> Result<bool, RuntimeError> {
    if let Value::Proxy(proxy) = obj {
      let (target, handler) = proxy_target_and_handler(self, proxy, "has")?;
      let trap = self.get_property_value(&handler, &PropertyKey::String("has".to_string()))?;
      if matches!(trap, Value::Undefined | Value::Null) {
        return self.has_property_host(&target, key);
      }
      if !Self::is_callable_value(&trap) {
        return Err(self.throw_type_error("Proxy has trap is not callable"));
      }
      let trap_result = self.call_with_this(
        &trap,
        &handler,
        &[target.clone(), property_key_to_value(key)],
      )?;
      if self.to_boolean(&trap_result) {
        return Ok(true);
      }
      if let Some(target_desc) = self.get_own_property_value(&target, key)? {
        if !descriptor_configurable(&target_desc) {
          return Err(
            self.throw_type_error("Proxy has trap cannot hide a non-configurable property"),
          );
        }
        if !self.is_extensible_value(&target)? {
          return Err(
            self
              .throw_type_error("Proxy has trap cannot hide a property of a non-extensible target"),
          );
        }
      }
      return Ok(false);
    }
    Ok(
      self
        .lookup_property_descriptor(obj, key, 0)?
        .is_some(),
    )
  }

  /// Host hook: delete an own property.
  ///
  /// # Errors
  ///
  /// Returns a [`crate::errors::RuntimeError`] if the property deletion fails.
  pub fn delete_property_host(
    &mut self,
    obj: &Value,
    key: &PropertyKey,
  ) -> Result<bool, RuntimeError> {
    let deleted_res: Result<bool, RuntimeError> = match obj {
      Value::Proxy(proxy) => {
        let (target, handler) = proxy_target_and_handler(self, proxy, "deleteProperty")?;
        let trap =
          self.get_property_value(&handler, &PropertyKey::String("deleteProperty".to_string()))?;
        if matches!(trap, Value::Undefined | Value::Null) {
          return self.delete_property_host(&target, key);
        }
        if !Self::is_callable_value(&trap) {
          return Err(self.throw_type_error("Proxy deleteProperty trap is not callable"));
        }
        let trap_result = self.call_with_this(
          &trap,
          &handler,
          &[target.clone(), property_key_to_value(key)],
        )?;
        if !self.to_boolean(&trap_result) {
          return Ok(false);
        }
        if let Some(target_desc) = self.get_own_property_value(&target, key)? {
          if !descriptor_configurable(&target_desc) {
            return Err(self.throw_type_error(
              "Proxy deleteProperty trap cannot delete a non-configurable property",
            ));
          }
          if !self.is_extensible_value(&target)? {
            return Err(self.throw_type_error(
              "Proxy deleteProperty trap cannot delete a property of a non-extensible target",
            ));
          }
        }
        return Ok(true);
      }
      Value::Object(data) => {
        let mut d = data.borrow_mut();
        match d.properties.get(key) {
          Some(PropertyDescriptor::Data { configurable, .. })
          | Some(PropertyDescriptor::Accessor { configurable, .. })
            if !*configurable =>
          {
            Ok(false)
          }
          Some(_) => {
            // Property exists and is configurable, delete it
            d.properties.shift_remove(key);
            Ok(true)
          }
          None => {
            // Property doesn't exist, return true
            Ok(true)
          }
        }
      }
      Value::Array(arr, props) => {
        let mut p = props.borrow_mut();
        if let Some(desc) = p.properties.get(key) {
          match desc {
            PropertyDescriptor::Data { configurable, .. }
            | PropertyDescriptor::Accessor { configurable, .. }
              if !*configurable =>
            {
              return Ok(false);
            }
            _ => {
              if let PropertyKey::String(s) = key
                && let Ok(i) = s.parse::<usize>()
                && i < arr.borrow().len()
              {
                arr.borrow_mut()[i] = None;
              }
              p.properties.shift_remove(key);
              return Ok(true);
            }
          }
        }
        drop(p);
        if let PropertyKey::String(s) = key
          && let Ok(i) = s.parse::<usize>()
        {
          let len = arr.borrow().len();
          let array_len = {
            let props_ref = props.borrow();
            array_length(&props_ref)
          };
          if i < len {
            arr.borrow_mut()[i] = None;
            return Ok(true);
          } else if i < array_len {
            // Index is within the array length but not yet in dense store.
            // Resize the array to create a hole.
            arr.borrow_mut().resize(i + 1, None);
            return Ok(true);
          }
        }
        let mut p = props.borrow_mut();
        match p.properties.get(key) {
          Some(PropertyDescriptor::Data { configurable, .. })
          | Some(PropertyDescriptor::Accessor { configurable, .. })
            if !*configurable =>
          {
            Ok(false)
          }
          Some(_) => {
            // Property exists and is configurable, delete it
            p.properties.shift_remove(key);
            Ok(true)
          }
          None => {
            // Property doesn't exist, return true
            Ok(true)
          }
        }
      }
      Value::Closure { props, .. } | Value::BuiltinObject(_, props) => {
        let mut p = props.borrow_mut();
        match p.properties.get(key) {
          Some(PropertyDescriptor::Data { configurable, .. })
          | Some(PropertyDescriptor::Accessor { configurable, .. })
            if !*configurable =>
          {
            Ok(false)
          }
          Some(_) => {
            // Property exists and is configurable, delete it
            p.properties.shift_remove(key);
            Ok(true)
          }
          None => {
            // Property doesn't exist, return true
            Ok(true)
          }
        }
      }
      _ => Ok(false),
    };
    let deleted = deleted_res?;
    if deleted {
      self.bump_object_generation(obj);
    }
    Ok(deleted)
  }

  /// Host hook: set a property value on a target object-like value.
  ///
  /// # Errors
  ///
  /// Returns a [`crate::errors::RuntimeError`] if the property setting fails.
  pub fn set_property_host(
    &mut self,
    obj: &Value,
    key: &Value,
    value: Value,
  ) -> Result<Value, RuntimeError> {
    let prop = self.to_property_key(key)?;
    let result = value.clone();
    self.set_property_tracked(obj, &prop, value)?;
    Ok(result)
  }

  /// Host hook: set a property with Reflect semantics, returning success/failure as a boolean.
  ///
  /// Unlike `set_property_host`, this operation returns `false` on failure instead of throwing
  /// an error, implementing the semantics of `Reflect.set()`.
  ///
  /// # Errors
  ///
  /// Returns a [`crate::errors::RuntimeError`] if the operation encounters a runtime error during lookup.
  pub(crate) fn reflect_set_property_host(
    &mut self,
    target: &Value,
    key: &Value,
    value: Value,
    receiver: Option<Value>,
  ) -> Result<bool, RuntimeError> {
    let prop = self.to_property_key(key)?;
    let receiver = receiver.unwrap_or_else(|| target.clone());
    let saved_strict = self.current_strict_mode;
    self.current_strict_mode = false;
    let result = self.set_property_value(target, &prop, value, &receiver);
    self.current_strict_mode = saved_strict;
    result
  }

  pub(super) fn read_descriptor_from_value(
    &mut self,
    v: &Value,
  ) -> Result<PartialDescriptor, RuntimeError> {
    let mut desc = PartialDescriptor::default();
    if !matches!(
      v,
      Value::Object(_)
        | Value::Array(..)
        | Value::Closure { .. }
        | Value::BuiltinObject(_, _)
        | Value::Proxy(_)
    ) {
      return Err(self.throw_type_error("Property description must be an object"));
    }

    fn get_field(
      vm: &mut Vm,
      object: &Value,
      field: &str,
    ) -> Result<Option<Value>, RuntimeError> {
      let key = PropertyKey::String(field.to_string());
      if !vm.has_property_host(object, &key)? {
        return Ok(None);
      }
      vm.get_property_host(object, &key).map(Some)
    }

    if let Some(value) = get_field(self, v, "value")? {
      desc.value = Some(value);
      desc.has_value = true;
    }
    if let Some(value) = get_field(self, v, "writable")? {
      desc.writable = Some(self.to_boolean(&value));
      desc.has_writable = true;
    }
    if let Some(value) = get_field(self, v, "enumerable")? {
      desc.enumerable = Some(self.to_boolean(&value));
      desc.has_enumerable = true;
    }
    if let Some(value) = get_field(self, v, "configurable")? {
      desc.configurable = Some(self.to_boolean(&value));
      desc.has_configurable = true;
    }
    if let Some(getter) = get_field(self, v, "get")? {
      if !matches!(getter, Value::Undefined) && !Self::is_callable_value(&getter) {
        return Err(self.throw_type_error("Getter must be callable"));
      }
      desc.get = Some(getter);
      desc.has_get = true;
    }
    if let Some(setter) = get_field(self, v, "set")? {
      if !matches!(setter, Value::Undefined) && !Self::is_callable_value(&setter) {
        return Err(self.throw_type_error("Setter must be callable"));
      }
      desc.set = Some(setter);
      desc.has_set = true;
    }

    let has_data = desc.has_value || desc.has_writable;
    let has_accessor = desc.has_get || desc.has_set;
    if has_data && has_accessor {
      return Err(self.throw_type_error(
        "Invalid property descriptor: cannot specify both data and accessor fields",
      ));
    }

    Ok(desc)
  }

  /// Define a property on an object, replacing any existing descriptor.
  ///
  /// # Errors
  ///
  /// Returns a [`crate::errors::RuntimeError`] if the operation fails.
  #[allow(clippy::too_many_lines)]
  pub(super) fn define_own_property(
    &mut self,
    obj: &Value,
    key: PropertyKey,
    desc: PartialDescriptor,
  ) -> Result<bool, RuntimeError> {
    if let Value::Proxy(proxy) = obj {
      let (target, handler) = proxy_target_and_handler(self, proxy, "defineProperty")?;
      let trap =
        self.get_property_value(&handler, &PropertyKey::String("defineProperty".to_string()))?;
      if matches!(trap, Value::Undefined | Value::Null) {
        return self.define_own_property(&target, key, desc);
      }
      if !Self::is_callable_value(&trap) {
        return Err(self.throw_type_error("Proxy defineProperty trap is not callable"));
      }
      let descriptor = partial_descriptor_to_value(self.realm().object_prototype(), &desc);
      let trap_result = self.call_with_this(
        &trap,
        &handler,
        &[target.clone(), property_key_to_value(&key), descriptor],
      )?;
      if !self.to_boolean(&trap_result) {
        return Ok(false);
      }

      let target_desc = self.get_own_property_value(&target, &key)?;
      let extensible = self.is_extensible_value(&target)?;
      let setting_config_false = desc.has_configurable && desc.configurable == Some(false);
      match target_desc.as_ref() {
        None => {
          if !extensible {
            return Err(self.throw_type_error(
              "Proxy defineProperty trap cannot add a property to a non-extensible target",
            ));
          }
          if setting_config_false {
            return Err(self.throw_type_error(
              "Proxy defineProperty trap cannot create a non-configurable property",
            ));
          }
        }
        Some(target_desc) => {
          if !is_compatible_property_descriptor(extensible, &desc, Some(target_desc)) {
            return Err(
              self
                .throw_type_error("Proxy defineProperty trap accepted an incompatible descriptor"),
            );
          }
          if setting_config_false && descriptor_configurable(target_desc) {
            return Err(self.throw_type_error(
              "Proxy defineProperty trap cannot make a configurable target property non-configurable",
            ));
          }
        }
      }
      return Ok(true);
    }
    if let Value::Array(arr, props) = obj
      && matches!(key, PropertyKey::String(ref name) if name == "length")
    {
      if desc.has_get || desc.has_set {
        return Ok(false);
      }

      let next_len = if let Some(value) = &desc.value {
        let _ = self.to_uint32_value(value)?;
        let len_num = self.to_number_value(value)?;
        if !len_num.is_finite() || len_num < 0.0 {
          return Err(self.throw_range_error("Invalid array length"));
        }
        let truncated = len_num.trunc();
        if truncated != len_num || truncated > u32::MAX as f64 {
          return Err(self.throw_range_error("Invalid array length"));
        }
        // Convert the verified integer `truncated` into a `usize` via `u32`.
        // We first cast to `i64` (safe because we've checked bounds) and then
        // use `u32::try_from` to avoid any unchecked truncation. Propagate a
        // RangeError if conversion unexpectedly fails.
        let truncated_i64 = truncated as i64;
        let truncated_u32 = u32::try_from(truncated_i64)
          .map_err(|_| self.throw_range_error("Invalid array length"))?;
        Some(truncated_u32 as usize)
      } else {
        None
      };

      let (current, current_len) = {
        let data = props.borrow();
        (data.properties.get(&key).cloned(), array_length(&data))
      };
      let current_writable = current.as_ref().is_none_or(|d| match d {
        PropertyDescriptor::Data { writable, .. } => *writable,
        PropertyDescriptor::Accessor { .. } => false,
      });
      let current_enumerable = current.as_ref().is_some_and(|d| match d {
        PropertyDescriptor::Data { enumerable, .. } => *enumerable,
        PropertyDescriptor::Accessor { enumerable, .. } => *enumerable,
      });
      let current_configurable = current.as_ref().is_some_and(|d| match d {
        PropertyDescriptor::Data { configurable, .. } => *configurable,
        PropertyDescriptor::Accessor { configurable, .. } => *configurable,
      });

      if let Some(true) = desc.configurable {
        return Ok(false);
      }
      if let Some(enumerable) = desc.enumerable
        && enumerable != current_enumerable
      {
        return Ok(false);
      }
      if !current_configurable {
        if let Some(true) = desc.configurable {
          return Ok(false);
        }
        if let Some(enumerable) = desc.enumerable
          && enumerable != current_enumerable
        {
          return Ok(false);
        }
      }

      let next_writable = desc.writable.unwrap_or(current_writable);
      if !current_writable && desc.writable == Some(true) {
        return Ok(false);
      }

      let next_len = next_len.unwrap_or(current_len);
      if next_len < current_len {
        if !current_writable {
          return Ok(false);
        }
        {
          let mut arr_mut = arr.borrow_mut();
          if next_len < arr_mut.len() {
            arr_mut.truncate(next_len);
          }
        }
        let mut data = props.borrow_mut();
        let delete_keys: Vec<PropertyKey> = data
          .properties
          .keys()
          .filter(|prop_key| array_index_from_key(prop_key).is_some_and(|idx| idx >= next_len))
          .cloned()
          .collect();
        for prop_key in delete_keys {
          data.properties.shift_remove(&prop_key);
        }
      }
      if next_len > current_len && !current_writable {
        return Ok(false);
      }

      let mut data = props.borrow_mut();
      let final_desc = PropertyDescriptor::Data {
        value: Value::Number(usize_to_f64(next_len)),
        writable: next_writable,
        enumerable: current_enumerable,
        configurable: false,
      };
      data.properties.insert(key, final_desc);
      return Ok(true);
    }
    let target = match obj {
      Value::Object(data) => data,
      Value::Array(_, props) => props,
      Value::Closure { props, .. } => props,
      Value::BuiltinObject(_, props) => props,
      _ => return Ok(false),
    };
    let mut data = target.borrow_mut();
    let current = if let Value::Array(arr, _) = obj {
      let current = data.properties.get(&key).cloned();
      if current.is_some() {
        current
      } else if let Some(index) = array_index_from_key(&key) {
        arr
          .borrow()
          .get(index)
          .cloned()
          .flatten()
          .map(|value| PropertyDescriptor::Data {
            value,
            writable: true,
            enumerable: true,
            configurable: true,
          })
      } else {
        None
      }
    } else {
      data.properties.get(&key).cloned()
    };

    if current.is_none() && !data.extensible {
      return Ok(false);
    }

    if let Some(ref cur) = current {
      // Non-configurable restrictions.
      let cur_configurable = match &cur {
        PropertyDescriptor::Data { configurable, .. } => *configurable,
        PropertyDescriptor::Accessor { configurable, .. } => *configurable,
      };
      if !cur_configurable {
        if let Some(true) = desc.configurable {
          return Ok(false);
        }
        if let Some(en) = desc.enumerable {
          let cur_en = match &cur {
            PropertyDescriptor::Data { enumerable, .. } => *enumerable,
            PropertyDescriptor::Accessor { enumerable, .. } => *enumerable,
          };
          if en != cur_en {
            return Ok(false);
          }
        }
        if let PropertyDescriptor::Data {
          writable, value, ..
        } = &cur
          && !*writable
        {
          if let Some(true) = desc.writable {
            return Ok(false);
          }
          if let Some(new_val) = &desc.value
            && !values_equal(new_val, value)
          {
            return Ok(false);
          }
        }
      }
    }

    let is_accessor = desc.get.is_some() || desc.set.is_some();
    let new_desc = if is_accessor {
      let (current_get, current_set) = match &current {
        Some(PropertyDescriptor::Accessor { get, set, .. }) => (get.clone(), set.clone()),
        _ => (None, None),
      };
      PropertyDescriptor::Accessor {
        get: desc.get.or(current_get),
        set: desc.set.or(current_set),
        enumerable: desc.enumerable.unwrap_or_else(|| {
          current.as_ref().is_some_and(|d| match d {
            PropertyDescriptor::Data { enumerable, .. } => *enumerable,
            PropertyDescriptor::Accessor { enumerable, .. } => *enumerable,
          })
        }),
        configurable: desc.configurable.unwrap_or_else(|| {
          current.as_ref().is_some_and(|d| match d {
            PropertyDescriptor::Data { configurable, .. } => *configurable,
            PropertyDescriptor::Accessor { configurable, .. } => *configurable,
          })
        }),
      }
    } else {
      let (cur_value, cur_writable, cur_enumerable, cur_configurable) = match &current {
        Some(PropertyDescriptor::Data {
          value,
          writable,
          enumerable,
          configurable,
        }) => (value.clone(), *writable, *enumerable, *configurable),
        Some(PropertyDescriptor::Accessor {
          enumerable,
          configurable,
          ..
        }) => (Value::Undefined, false, *enumerable, *configurable),
        None => (Value::Undefined, false, false, false),
      };
      PropertyDescriptor::Data {
        value: desc.value.unwrap_or(cur_value),
        writable: desc.writable.unwrap_or(cur_writable),
        enumerable: desc.enumerable.unwrap_or(cur_enumerable),
        configurable: desc.configurable.unwrap_or(cur_configurable),
      }
    };

    let sync_arguments_value = match &new_desc {
      PropertyDescriptor::Data {
        value,
        writable: true,
        ..
      } => Some(value.clone()),
      _ => None,
    };
    if let Value::Array(arr, _) = obj
      && let Some(index) = array_index_from_key(&key)
      && index < arr.borrow().len()
    {
      arr.borrow_mut()[index] = None;
    }
    data.properties.insert(key.clone(), new_desc);
    if let Some(index) = array_index_from_key(&key)
      && index < self.current_arguments_mapped_count
      && matches!(
          self.current_arguments_object.as_ref(),
          Some(current) if values_equal(current, obj)
      )
      && let Some(value) = sync_arguments_value
      && let Some(cell) = self.locals.get(index)
    {
      *cell.borrow_mut() = value;
    }
    Ok(true)
  }

  /// Get the value of a property on an object, following the prototype chain.
  pub(super) fn get_property_value(
    &mut self,
    obj: &Value,
    key: &PropertyKey,
  ) -> Result<Value, RuntimeError> {
    if matches!(obj, Value::Undefined | Value::Null) {
      return Err(self.throw_type_error(&format!(
        "Cannot read property '{:?}' of {}",
        key,
        obj.type_name()
      )));
    }
    if matches!(obj, Value::Proxy(_)) {
      return self.get_proxy_property(obj, key, obj);
    }
    if let Some((desc, _holder)) = self.lookup_property_descriptor(obj, key, 0)? {
      return match desc {
        PropertyDescriptor::Data { value, .. } => Ok(value),
        PropertyDescriptor::Accessor { get, .. } => {
          if let Some(getter) = get.filter(|getter| !matches!(getter, Value::Undefined)) {
            self.call_with_this(&getter, obj, &[])
          } else {
            Ok(Value::Undefined)
          }
        }
      };
    }
    if let Value::String(text) = obj {
      if matches!(key, PropertyKey::String(name) if name == "length") {
        return Ok(Value::Number(usize_to_f64(text.chars().count())));
      }
      if let Some(i) = array_index_from_key(key)
        && let Some(ch) = text.chars().nth(i)
      {
        return Ok(Value::String(ch.to_string()));
      }
      let string_ctor = self
        .globals
        .borrow()
        .properties
        .get(&PropertyKey::String("String".to_string()))
        .and_then(|d| match d {
          PropertyDescriptor::Data { value, .. } => Some(value.clone()),
          _ => None,
        });
      if let Some(string_ctor) = string_ctor
        && let Ok(proto) =
          self.get_property_value(&string_ctor, &PropertyKey::String("prototype".to_string()))
        && !matches!(proto, Value::Null)
        && let Ok(v) = get_property(&proto, key, 0)
      {
        return Ok(v);
      }
    }
    if let Value::Array(_, _) = obj {
      let array_ctor = self
        .globals
        .borrow()
        .properties
        .get(&PropertyKey::String("Array".to_string()))
        .and_then(|d| match d {
          PropertyDescriptor::Data { value, .. } => Some(value.clone()),
          _ => None,
        });
      if let Some(array_ctor) = array_ctor
        && let Ok(proto) =
          self.get_property_value(&array_ctor, &PropertyKey::String("prototype".to_string()))
        && !matches!(proto, Value::Null)
        && let Ok(v) = get_property(&proto, key, 0)
      {
        return Ok(v);
      }
    }
    if let Value::Symbol(_, desc) = obj {
      if matches!(key, PropertyKey::String(name) if name == "description") {
        return Ok(match desc {
          Some(d) => Value::String(d.clone()),
          None => Value::Undefined,
        });
      }
      let symbol_ctor = self
        .globals
        .borrow()
        .properties
        .get(&PropertyKey::String("Symbol".to_string()))
        .and_then(|d| match d {
          PropertyDescriptor::Data { value, .. } => Some(value.clone()),
          _ => None,
        });
      if let Some(symbol_ctor) = symbol_ctor
        && let Ok(proto) =
          self.get_property_value(&symbol_ctor, &PropertyKey::String("prototype".to_string()))
        && !matches!(proto, Value::Null)
        && let Ok(v) = get_property(&proto, key, 0)
      {
        return Ok(v);
      }
    }
    if matches!(key, PropertyKey::String(name) if name == "constructor") {
      let proto = self.get_prototype(obj);
      if !matches!(proto, Value::Null)
        && let Ok(constructor) =
          get_property(&proto, &PropertyKey::String("constructor".to_string()), 0)
        && !matches!(constructor, Value::Undefined)
      {
        return Ok(constructor);
      }
      if let Some(test262_error) = self.get_global("Test262Error")
        && Self::is_callable_value(&test262_error)
      {
        return Ok(test262_error);
      }
      if let Some(error_ctor) = self.get_global("Error")
        && Self::is_callable_value(&error_ctor)
      {
        return Ok(error_ctor);
      }
    }
    Ok(Value::Undefined)
  }

  /// Lookup a property descriptor on an object, following the prototype chain.
  pub(super) fn lookup_property_descriptor(
    &self,
    obj: &Value,
    key: &PropertyKey,
    depth: usize,
  ) -> Result<Option<(PropertyDescriptor, Value)>, RuntimeError> {
    if depth > MAX_PROTOTYPE_DEPTH {
      return Err(RuntimeError::from(
        "Prototype chain cycle detected or too deep",
      ));
    }
    match obj {
      Value::Object(data) => {
        let d = data.borrow();
        if let Some(desc) = d.properties.get(key) {
          return Ok(Some((desc.clone(), obj.clone())));
        }
        let proto = d.get_prototype();
        if !matches!(proto, Value::Null) {
          return self.lookup_property_descriptor(&proto, key, depth + 1);
        }
      }
      Value::Array(arr, props) => {
        let props_ref = props.borrow();
        if let Some(desc) = props_ref.properties.get(key) {
          return Ok(Some((desc.clone(), obj.clone())));
        }
        if let Some(i) = array_index_from_key(key)
          && i < array_length(&props_ref)
          && let Some(opt_v) = arr.borrow().get(i).cloned()
          && let Some(v) = opt_v
        {
          return Ok(Some((
            PropertyDescriptor::Data {
              value: v,
              writable: true,
              enumerable: true,
              configurable: true,
            },
            obj.clone(),
          )));
        }
        let proto = props_ref.get_prototype();
        if !matches!(proto, Value::Null) {
          return self.lookup_property_descriptor(&proto, key, depth + 1);
        }
      }
      Value::Proxy(proxy) => {
        let proxy = proxy.borrow();
        if proxy.is_revoked() {
          return Ok(None);
        }
        return self.lookup_property_descriptor(&proxy.target, key, depth + 1);
      }
      Value::Closure { props, .. } => {
        let p = props.borrow();
        if let Some(desc) = p.properties.get(key) {
          return Ok(Some((desc.clone(), obj.clone())));
        }
        let proto = p.get_prototype();
        if !matches!(proto, Value::Null) {
          return self.lookup_property_descriptor(&proto, key, depth + 1);
        }
      }
      Value::BuiltinObject(_, props) => {
        let p = props.borrow();
        if let Some(desc) = p.properties.get(key) {
          return Ok(Some((desc.clone(), obj.clone())));
        }
        let proto = p.get_prototype();
        if !matches!(proto, Value::Null) {
          return self.lookup_property_descriptor(&proto, key, depth + 1);
        }
      }
      Value::Promise(_) => {
        let proto = self.promise_prototype_value();
        if !matches!(proto, Value::Null) {
          return self.lookup_property_descriptor(&proto, key, depth + 1);
        }
      }
      Value::Bool(_) => {
        if let Some(proto) = self.realm.intrinsics.boolean_prototype.clone() {
          return self.lookup_property_descriptor(&proto, key, depth + 1);
        }
      }
      Value::Number(_) => {
        if let Some(proto) = self.realm.intrinsics.number_prototype.clone() {
          return self.lookup_property_descriptor(&proto, key, depth + 1);
        }
      }
      Value::BigInt(_) => {
        if let Some(proto) = self.realm.intrinsics.bigint_prototype.clone() {
          return self.lookup_property_descriptor(&proto, key, depth + 1);
        }
      }
      _ => {}
    }
    Ok(None)
  }

  /// `SetProperty` wrapper that bumps the generation counter so that stale
  /// IC entries are invalidated on next access.
  pub(super) fn set_property_tracked(
    &mut self,
    obj: &Value,
    key: &PropertyKey,
    value: Value,
  ) -> Result<(), RuntimeError> {
    if matches!(obj, Value::Undefined | Value::Null) {
      return Err(self.throw_type_error(&format!(
        "Cannot set property '{:?}' of {}",
        key,
        obj.type_name()
      )));
    }
    if matches!(obj, Value::Proxy(_)) {
      let accepted = self.set_proxy_property(obj, key, value, obj)?;
      if !accepted && self.is_strict_mode() {
        return Err(self.throw_type_error("Proxy set trap returned false"));
      }
      return Ok(());
    }
    if let Value::Array(arr, props) = obj
      && matches!(key, PropertyKey::String(s) if s == "length")
      && let Value::Number(new_len_f) = value.clone()
    {
      let new_len = if new_len_f.is_nan() || new_len_f < 0.0 {
        0usize
      } else {
        // Ensure the float is an integer within the u32 range before conversion.
        let truncated = new_len_f.trunc();
        if !truncated.is_finite() || truncated < 0.0 || truncated > u32::MAX as f64 {
          return Err(self.throw_range_error("Invalid array length"));
        }
        let truncated_i64 = truncated as i64;
        let truncated_u32 = u32::try_from(truncated_i64)
          .map_err(|_| self.throw_range_error("Invalid array length"))?;
        truncated_u32 as usize
      };
      let mut a = arr.borrow_mut();
      if new_len < a.len() {
        a.truncate(new_len);
      }
      props.borrow_mut().properties.insert(
        PropertyKey::String("length".to_string()),
        PropertyDescriptor::data(Value::Number(usize_to_f64(new_len))),
      );
      self.bump_object_generation(obj);
      return Ok(());
    }
    let mut sync_arguments_slot = None;
    if let Some((desc, _holder)) = self.lookup_property_descriptor(obj, key, 0)? {
      match desc {
        PropertyDescriptor::Accessor { set, .. } => {
          if let Some(setter) = set.filter(|setter| !matches!(setter, Value::Undefined)) {
            drop(self.call_with_this(&setter, obj, &[value])?);
            self.bump_object_generation(obj);
            return Ok(());
          }
          if self.is_strict_mode() {
            return Err(self.throw_type_error("Cannot set property with no setter"));
          }
          return Ok(());
        }
        PropertyDescriptor::Data {
          writable: false, ..
        } => {
          if self.is_strict_mode() {
            return Err(self.throw_type_error("Cannot assign to read only property"));
          }
          return Ok(());
        }
        PropertyDescriptor::Data { .. } => {}
      }
      if matches!(
          self.current_arguments_object.as_ref(),
          Some(current) if values_equal(current, obj)
      ) && let Some(index) = array_index_from_key(key)
        && index < self.current_arguments_mapped_count
      {
        sync_arguments_slot = Some(index);
      }
    }
    if self.is_strict_mode() {
      let extensible = match obj {
        Value::Object(data) => data.borrow().extensible,
        Value::Array(_, props) => props.borrow().extensible,
        Value::Closure { props, .. } => props.borrow().extensible,
        Value::BuiltinObject(_, props) => props.borrow().extensible,
        _ => true,
      };
      if !extensible {
        return Err(self.throw_type_error("Cannot add property to non-extensible object"));
      }
    }
    set_property(obj, key, value)?;
    self.bump_object_generation(obj);
    if let Some(index) = sync_arguments_slot
      && let Some(cell) = self.locals.get(index)
    {
      *cell.borrow_mut() = self
        .lookup_property_descriptor(obj, key, 0)?
        .and_then(|(desc, _)| match desc {
          PropertyDescriptor::Data { value, .. } => Some(value),
          _ => None,
        })
        .unwrap_or(Value::Undefined);
    }

    Ok(())
  }
}

fn remove_key(
  keys: &mut Vec<PropertyKey>,
  key: &PropertyKey,
) -> bool {
  let Some(index) = keys.iter().position(|candidate| candidate == key) else {
    return false;
  };
  keys.swap_remove(index);
  true
}

fn object_is_extensible(value: &Value) -> bool {
  match value {
    Value::Object(data) => data.borrow().extensible,
    Value::Array(_, props) | Value::Closure { props, .. } | Value::BuiltinObject(_, props) => {
      props.borrow().extensible
    }
    Value::Proxy(_) | Value::Builtin(_) | Value::Promise(_) => false,
    _ => false,
  }
}

fn set_object_extensible(
  value: &Value,
  extensible: bool,
) -> bool {
  match value {
    Value::Object(data) => data.borrow_mut().extensible = extensible,
    Value::Array(_, props) | Value::Closure { props, .. } | Value::BuiltinObject(_, props) => {
      props.borrow_mut().extensible = extensible;
    }
    Value::Builtin(_) | Value::Promise(_) => return !extensible,
    _ => return false,
  }
  true
}

fn set_object_prototype(
  value: &Value,
  prototype: Value,
) -> bool {
  match value {
    Value::Object(data) => data.borrow_mut().set_prototype_value(prototype),
    Value::Array(_, props) | Value::Closure { props, .. } | Value::BuiltinObject(_, props) => {
      props.borrow_mut().set_prototype_value(prototype);
    }
    _ => return false,
  }
  true
}

fn proxy_target_and_handler(
  vm: &mut Vm,
  proxy: &std::rc::Rc<std::cell::RefCell<crate::value::ProxyData>>,
  operation: &str,
) -> Result<(Value, Value), RuntimeError> {
  let proxy = proxy.borrow();
  let Some(handler) = proxy.handler.clone() else {
    return Err(vm.throw_type_error(&format!("Cannot perform '{operation}' on a revoked proxy")));
  };
  Ok((proxy.target.clone(), handler))
}

fn descriptor_configurable(desc: &PropertyDescriptor) -> bool {
  match desc {
    PropertyDescriptor::Data { configurable, .. }
    | PropertyDescriptor::Accessor { configurable, .. } => *configurable,
  }
}

fn descriptor_enumerable(desc: &PropertyDescriptor) -> bool {
  match desc {
    PropertyDescriptor::Data { enumerable, .. }
    | PropertyDescriptor::Accessor { enumerable, .. } => *enumerable,
  }
}

fn complete_property_descriptor(desc: PartialDescriptor) -> PropertyDescriptor {
  if desc.has_get || desc.has_set {
    PropertyDescriptor::Accessor {
      get: desc
        .get
        .filter(|value| !matches!(value, Value::Undefined)),
      set: desc
        .set
        .filter(|value| !matches!(value, Value::Undefined)),
      enumerable: desc.enumerable.unwrap_or(false),
      configurable: desc.configurable.unwrap_or(false),
    }
  } else {
    PropertyDescriptor::Data {
      value: desc.value.unwrap_or(Value::Undefined),
      writable: desc.writable.unwrap_or(false),
      enumerable: desc.enumerable.unwrap_or(false),
      configurable: desc.configurable.unwrap_or(false),
    }
  }
}

fn partial_descriptor_from_complete(desc: &PropertyDescriptor) -> PartialDescriptor {
  match desc {
    PropertyDescriptor::Data {
      value,
      writable,
      enumerable,
      configurable,
    } => PartialDescriptor {
      value: Some(value.clone()),
      writable: Some(*writable),
      enumerable: Some(*enumerable),
      configurable: Some(*configurable),
      has_value: true,
      has_writable: true,
      has_enumerable: true,
      has_configurable: true,
      ..PartialDescriptor::default()
    },
    PropertyDescriptor::Accessor {
      get,
      set,
      enumerable,
      configurable,
    } => PartialDescriptor {
      get: Some(get.clone().unwrap_or(Value::Undefined)),
      set: Some(set.clone().unwrap_or(Value::Undefined)),
      enumerable: Some(*enumerable),
      configurable: Some(*configurable),
      has_get: true,
      has_set: true,
      has_enumerable: true,
      has_configurable: true,
      ..PartialDescriptor::default()
    },
  }
}

fn partial_descriptor_to_value(
  prototype: Value,
  desc: &PartialDescriptor,
) -> Value {
  let mut object = ObjectData::with_prototype(prototype);
  let mut insert = |name: &str, value: Value| {
    object.properties.insert(
      PropertyKey::String(name.to_string()),
      PropertyDescriptor::data(value),
    );
  };
  if desc.has_value {
    insert("value", desc.value.clone().unwrap_or(Value::Undefined));
  }
  if desc.has_writable {
    insert("writable", Value::Bool(desc.writable.unwrap_or(false)));
  }
  if desc.has_get {
    insert("get", desc.get.clone().unwrap_or(Value::Undefined));
  }
  if desc.has_set {
    insert("set", desc.set.clone().unwrap_or(Value::Undefined));
  }
  if desc.has_enumerable {
    insert("enumerable", Value::Bool(desc.enumerable.unwrap_or(false)));
  }
  if desc.has_configurable {
    insert(
      "configurable",
      Value::Bool(desc.configurable.unwrap_or(false)),
    );
  }
  Value::Object(std::rc::Rc::new(std::cell::RefCell::new(object)))
}

fn is_compatible_property_descriptor(
  extensible: bool,
  desc: &PartialDescriptor,
  current: Option<&PropertyDescriptor>,
) -> bool {
  let Some(current) = current else {
    return extensible;
  };
  if !desc.has_value
    && !desc.has_writable
    && !desc.has_get
    && !desc.has_set
    && !desc.has_enumerable
    && !desc.has_configurable
  {
    return true;
  }

  let current_configurable = descriptor_configurable(current);
  if !current_configurable {
    if desc.configurable == Some(true) {
      return false;
    }
    if desc.has_enumerable
      && desc
        .enumerable
        .is_some_and(|value| value != descriptor_enumerable(current))
    {
      return false;
    }
  }

  let desc_is_data = desc.has_value || desc.has_writable;
  let desc_is_accessor = desc.has_get || desc.has_set;
  if !desc_is_data && !desc_is_accessor {
    return true;
  }
  let current_is_data = matches!(current, PropertyDescriptor::Data { .. });
  if desc_is_data != current_is_data {
    return current_configurable;
  }

  match current {
    PropertyDescriptor::Data {
      value, writable, ..
    } if !current_configurable && !*writable => {
      if desc.writable == Some(true) {
        return false;
      }
      if desc.has_value
        && desc
          .value
          .as_ref()
          .is_some_and(|next| !same_value(next, value))
      {
        return false;
      }
    }
    PropertyDescriptor::Accessor { get, set, .. } if !current_configurable => {
      let current_get = get.clone().unwrap_or(Value::Undefined);
      let current_set = set.clone().unwrap_or(Value::Undefined);
      if desc.has_get
        && desc
          .get
          .as_ref()
          .is_some_and(|next| !same_value(next, &current_get))
      {
        return false;
      }
      if desc.has_set
        && desc
          .set
          .as_ref()
          .is_some_and(|next| !same_value(next, &current_set))
      {
        return false;
      }
    }
    _ => {}
  }
  true
}
