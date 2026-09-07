// SPDX-License-Identifier: MPL-2.0
use crate::ecmascript::semantics::byte_views::{
  byte_view_index_from_key, byte_view_index_info, byte_view_index_value, byte_view_metadata,
  canonical_numeric_index_from_key, is_byte_view, write_byte_view_index,
};
use crate::ecmascript::semantics::properties::{
  own_property_descriptor, own_property_keys, property_key_to_value,
};
use crate::errors::RuntimeError;
use crate::runtime_utils::{strict_equal, to_length, usize_to_f64};
use crate::value::{ObjectData, PropertyDescriptor, PropertyKey, Value};
use crate::vm::Vm;
use crate::vm::state::{MAX_PROTOTYPE_DEPTH, PartialDescriptor, PropertyGetIcEntry};

#[cfg(test)]
#[path = "property/array_read_tests.rs"]
mod array_read_tests;
#[path = "property/descriptors.rs"]
mod descriptors;
#[path = "property/meta.rs"]
mod meta;
#[path = "property/ordinary.rs"]
mod ordinary;
#[cfg(test)]
#[path = "property/own_index_tests.rs"]
mod own_index_tests;
#[cfg(test)]
#[path = "property/performance_tests.rs"]
mod performance_tests;

use descriptors::{
  complete_property_descriptor, descriptor_configurable, descriptor_enumerable,
  is_compatible_property_descriptor, partial_descriptor_from_complete,
};
pub use ordinary::same_value;
pub(super) use ordinary::{
  array_index_from_key, array_length, get_property, list_own_keys_of_value, object_identity,
  set_property, values_equal,
};

impl Vm {
  pub(crate) fn set_typed_view_element(
    &mut self,
    receiver: &Value,
    index: usize,
    value: Value,
  ) -> Result<(), RuntimeError> {
    if let Some(info) = byte_view_index_info(receiver)
      && index < info.length
    {
      write_byte_view_index(self, &info, index, &value)?;
      self.bump_object_generation(receiver);
      return Ok(());
    }
    self.set_property_host(receiver, &Value::String(index.to_string().into()), value)?;
    Ok(())
  }

  /// Reverse a validated view without numeric property keys or boxed elements.
  /// No operation in a batch can call JS; release backing borrows before polling.
  pub(crate) fn try_reverse_typed_view(
    &mut self,
    receiver: &Value,
  ) -> Result<bool, RuntimeError> {
    let details = crate::ecmascript::semantics::byte_views::byte_view_details(receiver)
      .ok_or_else(|| self.throw_type_error("TypedArray method called on incompatible receiver"))?;
    if details.data.borrow().packed_bytes().is_none() {
      return Ok(false);
    }
    let width = details.bytes_per_element;
    for start in (0..details.length / 2).step_by(64) {
      self.poll_execution_interrupt()?;
      let mut data = details.data.borrow_mut();
      for index in start..(start + 64).min(details.length / 2) {
        let left = details.byte_offset + index * width;
        let right = details.byte_offset + (details.length - 1 - index) * width;
        let mut a = [0; 8];
        let mut b = [0; 8];
        assert!(data.read_bytes(left, &mut a[..width]));
        assert!(data.read_bytes(right, &mut b[..width]));
        assert!(data.write_bytes(left, &b[..width]));
        assert!(data.write_bytes(right, &a[..width]));
      }
      drop(data);
      self.bump_object_generation(receiver);
    }
    Ok(true)
  }

  /// Append only when every write is an ordinary new dense element. The preflight
  /// invokes no JavaScript, so fallback can perform the full observable algorithm.
  pub(crate) fn try_append_dense_array(
    &mut self,
    receiver: &Value,
    items: &[Value],
    length: usize,
  ) -> bool {
    let Value::Array(elements, properties) = receiver else {
      return false;
    };
    let Some(new_length) = length
      .checked_add(items.len())
      .filter(|value| *value <= u32::MAX as usize)
    else {
      return false;
    };
    if elements.borrow().len() != length {
      return false;
    }
    let length_key = PropertyKey::builtin_length();
    let index_keys: Vec<_> = (length..new_length)
      .map(|index| PropertyKey::String(index.to_string().into()))
      .collect();
    let mut prototype = {
      let properties = properties.borrow();
      if !properties.extensible
        || index_keys
          .iter()
          .any(|key| properties.properties.contains_key(key))
      {
        return false;
      }
      if properties.properties.get(&length_key).is_some_and(|descriptor| !matches!(descriptor,
        PropertyDescriptor::Data { value: Value::Number(value), writable: true, .. } if *value == usize_to_f64(length))) {
        return false;
      }
      properties.get_prototype()
    };
    for _ in 0..MAX_PROTOTYPE_DEPTH {
      match prototype {
        Value::Null => {
          if !items.is_empty() {
            let mut elements = elements.borrow_mut();
            elements.reserve(items.len());
            for item in items {
              // ArrayData::push retains packed numeric storage for numbers.
              elements.push(Some(item.clone()));
            }
          }
          let mut properties = properties.borrow_mut();
          if let Some(PropertyDescriptor::Data { value, .. }) =
            properties.properties.get_mut(&length_key)
          {
            *value = Value::Number(usize_to_f64(new_length));
          } else {
            properties.properties.insert(
              length_key,
              crate::runtime_utils::array_length_descriptor(new_length),
            );
          }
          properties.mutation_generation = properties.mutation_generation.wrapping_add(1);
          return true;
        }
        Value::Object(ref object) => {
          let next = {
            let object = object.borrow();
            if !object.internal_slots.is_empty()
              || index_keys
                .iter()
                .any(|key| object.properties.contains_key(key))
            {
              return false;
            }
            object.get_prototype()
          };
          prototype = next;
        }
        _ => return false,
      }
    }
    false
  }

  /// Read an own data property through a per-instruction position hint.
  ///
  /// Validate the indexed key and descriptor on every read, allowing different
  /// objects and mutations to reuse the same position without caching values.
  pub(super) fn get_property_value_cached(
    &mut self,
    instruction_pc: usize,
    obj: &Value,
    key: &PropertyKey,
  ) -> Result<Value, RuntimeError> {
    if !self.caches.optimized_execution_enabled {
      return self.get_property_value(obj, key);
    }
    let Value::Object(data) = obj else {
      return self.get_property_value(obj, key);
    };
    {
      let object = data.borrow();
      if !object.internal_slots.is_empty() || object.module_namespace_exports.is_some() {
        drop(object);
        return self.get_property_value(obj, key);
      }
    }
    let chunk = &self.execution.active.chunk;
    if self
      .caches
      .active_property_get
      .as_ref()
      .is_none_or(|cache| !std::sync::Arc::ptr_eq(&cache.chunk, chunk))
    {
      let cache = self
        .caches
        .property_get
        .entry(std::sync::Arc::as_ptr(chunk) as usize)
        .or_insert_with(|| {
          let mut count = 0u32;
          let site_indices = chunk
            .words
            .iter()
            .map(|word| {
              if crate::bytecode::opcode_from_word(*word)
                == crate::bytecode::Opcode::GetProperty as u8
              {
                let index = count;
                count += 1;
                index
              } else {
                u32::MAX
              }
            })
            .collect();
          std::rc::Rc::new(crate::vm::state::PropertyGetCache {
            chunk: std::sync::Arc::clone(chunk),
            site_indices,
            entries: std::cell::RefCell::new(vec![None; count as usize]),
          })
        });
      self.caches.active_property_get = Some(std::rc::Rc::clone(cache));
    }
    // Retain this site's storage while falling back through reentrant getters.
    let cache = std::rc::Rc::clone(
      self
        .caches
        .active_property_get
        .as_ref()
        .expect("active property cache"),
    );
    let site = cache.site_indices[instruction_pc] as usize;
    {
      let object = data.borrow();
      if let Some(entry) = cache.entries.borrow()[site]
        && let Some((actual_key, PropertyDescriptor::Data { value, .. })) =
          object.properties.get_index(entry.property_index)
        && actual_key == key
        && !matches!(value, Value::Uninitialized)
      {
        return Ok(value.clone());
      }
      if let Some((index, _, PropertyDescriptor::Data { value, .. })) =
        object.properties.get_full(key)
        && !matches!(value, Value::Uninitialized)
      {
        cache.entries.borrow_mut()[site] = Some(PropertyGetIcEntry {
          property_index: index,
        });
        return Ok(value.clone());
      }
    }
    cache.entries.borrow_mut()[site] = None;
    self.get_property_value(obj, key)
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
    let trap = self.get_property_value(&handler, &PropertyKey::String("get".to_string().into()))?;
    if matches!(trap, Value::Undefined | Value::Null) {
      return self.get_property_with_receiver(&target, key, receiver);
    }
    if !Self::is_callable_value(&trap) {
      return Err(self.throw_type_error("Proxy get trap is not callable"));
    }
    let key_value = property_key_to_value(key);
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
    let trap = self.get_property_value(&handler, &PropertyKey::String("set".to_string().into()))?;
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
    if is_module_namespace_object(target) {
      return Ok(false);
    }
    if canonical_numeric_index_from_key(key).is_some() {
      let mut current = target.clone();
      for _ in 0..MAX_PROTOTYPE_DEPTH {
        if is_byte_view(&current)
          && byte_view_index_from_key(key)
            .and_then(|index| byte_view_index_value(&current, index))
            .is_none()
        {
          if strict_equal(&current, receiver)
            && let Some((_, kind, _, _)) = byte_view_metadata(&current)
          {
            let _ =
              crate::ecmascript::semantics::byte_views::encode_typed_element(self, &kind, &value)?;
          }
          return Ok(true);
        }
        current = self.get_prototype(&current);
        if matches!(current, Value::Null) {
          break;
        }
      }
    }

    let own_desc = self.get_own_property_value(target, key)?;
    if own_desc.is_none() {
      let parent = self.get_prototype(target);
      if !matches!(parent, Value::Null) {
        return self.set_property_value(&parent, key, value, receiver);
      }
    }
    if let Some(desc) = own_desc {
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
          if matches!(receiver, Value::Array(array, _) if array_index_from_key(key)
            .is_some_and(|index| index < array.borrow().len()))
          {
            self.set_property_tracked(receiver, key, value)?;
            return Ok(true);
          }
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

  pub(crate) fn get_property_with_receiver(
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
    if obj.is_object() {
      if let Some(desc) = self.get_own_property_value(obj, key)? {
        return match desc {
          PropertyDescriptor::Data {
            value: Value::Uninitialized,
            ..
          } => {
            Err(self.throw_reference_error("Cannot access module binding before initialization"))
          }
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
      let mut parent = self.get_prototype(obj);
      if matches!(parent, Value::Null) && matches!(obj, Value::Array(..)) {
        parent = self.realm_state.realm.array_prototype();
      }
      if matches!(parent, Value::Null) {
        return Ok(Value::Undefined);
      }
      return self.get_property_with_receiver(&parent, key, receiver);
    }
    if let Some((desc, _holder)) = self.lookup_property_descriptor(obj, key, 0)? {
      return match desc {
        PropertyDescriptor::Data {
          value: Value::Uninitialized,
          ..
        } => Err(self.throw_reference_error("Cannot access module binding before initialization")),
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
        return Ok(Value::Number(usize_to_f64(text.len())));
      }
      if let Some(i) = array_index_from_key(key)
        && let Some(ch) = text.slice(i..i.saturating_add(1))
      {
        return Ok(Value::String(ch));
      }
    }
    Ok(Value::Undefined)
  }

  pub(super) fn set_primitive_property(
    &mut self,
    object: &Value,
    key: &PropertyKey,
    value: Value,
    receiver: &Value,
  ) -> Result<(), RuntimeError> {
    let mut current = object.clone();
    for _ in 0..MAX_PROTOTYPE_DEPTH {
      if matches!(current, Value::Proxy(_)) {
        let accepted = self.set_proxy_property(&current, key, value, receiver)?;
        if !accepted && self.is_strict_mode() {
          return Err(self.throw_type_error("Proxy set trap returned false"));
        }
        return Ok(());
      }
      if self
        .get_own_property_value(&current, key)?
        .is_some()
      {
        return self.set_property_with_receiver(&current, key, value, receiver);
      }
      current = self.get_prototype(&current);
      if matches!(current, Value::Null) {
        if self.is_strict_mode() {
          return Err(self.throw_type_error("Cannot assign to property of primitive value"));
        }
        return Ok(());
      }
    }
    Err(RuntimeError::from("Prototype chain too deep"))
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
    let accepted = self.set_property_value(obj, key, value, receiver)?;
    if !accepted && self.is_strict_mode() {
      if receiver.is_object()
        && self
          .get_own_property_value(receiver, key)?
          .is_none()
        && !self.is_extensible_value(receiver)?
      {
        return Err(self.throw_type_error("Cannot add property to non-extensible object"));
      }
      return Err(self.throw_type_error("Property assignment was rejected"));
    }
    Ok(())
  }

  pub(super) fn get_super_property(
    &mut self,
    receiver: &Value,
    key: &Value,
  ) -> Result<Value, RuntimeError> {
    if matches!(receiver, Value::Uninitialized) {
      return Err(
        self.throw_reference_error("Must call super constructor before accessing 'this'"),
      );
    }
    let super_obj = self.super_base_object()?;
    if matches!(super_obj, Value::Null) {
      return Err(self.throw_type_error("Cannot read properties of null"));
    }
    let k = self.to_property_key(key)?;
    self.get_property_with_receiver(&super_obj, &k, receiver)
  }

  pub(super) fn set_super_property(
    &mut self,
    receiver: &Value,
    key: &Value,
    value: Value,
  ) -> Result<Value, RuntimeError> {
    if matches!(receiver, Value::Uninitialized) {
      return Err(
        self.throw_reference_error("Must call super constructor before accessing 'this'"),
      );
    }
    let super_obj = self.super_base_object()?;
    if matches!(super_obj, Value::Null) {
      return Err(self.throw_type_error("Cannot set properties of null"));
    }
    let k = self.to_property_key(key)?;
    self.set_property_with_receiver(&super_obj, &k, value.clone(), receiver)?;
    Ok(value)
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
        | Value::Promise(..)
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
    self.has_property_value(obj, key, 0)
  }

  fn has_property_value(
    &mut self,
    obj: &Value,
    key: &PropertyKey,
    depth: usize,
  ) -> Result<bool, RuntimeError> {
    if depth > MAX_PROTOTYPE_DEPTH {
      return Err(RuntimeError::from(
        "Prototype chain cycle detected or too deep",
      ));
    }
    if let Value::Proxy(proxy) = obj {
      let (target, handler) = proxy_target_and_handler(self, proxy, "has")?;
      let trap =
        self.get_property_value(&handler, &PropertyKey::String("has".to_string().into()))?;
      if matches!(trap, Value::Undefined | Value::Null) {
        return self.has_property_value(&target, key, depth + 1);
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
    if is_byte_view(obj) && canonical_numeric_index_from_key(key).is_some() {
      return Ok(
        byte_view_index_from_key(key)
          .and_then(|index| byte_view_index_value(obj, index))
          .is_some(),
      );
    }
    if let Value::Object(data) = obj
      && let PropertyKey::String(name) = key
      && data
        .borrow()
        .module_namespace_exports
        .as_ref()
        .is_some_and(|exports| exports.contains_key(name))
    {
      return Ok(true);
    }
    if self.get_own_property_value(obj, key)?.is_some() {
      return Ok(true);
    }
    let prototype = self.get_prototype_of_value(obj)?;
    if matches!(prototype, Value::Null) {
      return Ok(false);
    }
    self.has_property_value(&prototype, key, depth + 1)
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
      Value::Object(data)
        if matches!(key, PropertyKey::String(name) if data
          .borrow()
          .module_namespace_exports
          .as_ref()
          .is_some_and(|exports| exports.contains_key(name))) =>
      {
        Ok(false)
      }
      Value::Proxy(proxy) => {
        let (target, handler) = proxy_target_and_handler(self, proxy, "deleteProperty")?;
        let trap = self.get_property_value(
          &handler,
          &PropertyKey::String("deleteProperty".to_string().into()),
        )?;
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
            d.properties.shift_remove(key);
            Ok(true)
          }
          None => Ok(true),
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
            p.properties.shift_remove(key);
            Ok(true)
          }
          None => Ok(true),
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
            p.properties.shift_remove(key);
            Ok(true)
          }
          None => Ok(true),
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

  /// Host hook: create or replace an enumerable own data property.
  ///
  /// Returns whether the object's `[[DefineOwnProperty]]` operation accepted the descriptor.
  pub fn create_data_property_host(
    &mut self,
    object: &Value,
    key: PropertyKey,
    value: Value,
  ) -> Result<bool, RuntimeError> {
    let accepted = self.define_own_property(
      object,
      key,
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
      self.bump_object_generation(object);
    }
    Ok(accepted)
  }

  /// Host hook: define an own property from an already constructed descriptor.
  ///
  /// Native integrations must use this operation instead of mutating
  /// [`ObjectData::properties`] directly. It preserves ECMAScript descriptor
  /// invariants and invalidates property inline caches after an accepted write.
  pub fn define_own_property_host(
    &mut self,
    object: &Value,
    key: PropertyKey,
    descriptor: PropertyDescriptor,
  ) -> Result<bool, RuntimeError> {
    let accepted =
      self.define_own_property(object, key, partial_descriptor_from_complete(&descriptor))?;
    if accepted {
      self.bump_object_generation(object);
    }
    Ok(accepted)
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
    let saved_strict = self.execution.active.strict;
    self.execution.active.strict = true;
    let set_result = self.set_property_with_receiver(obj, &prop, value, obj);
    self.execution.active.strict = saved_strict;
    set_result?;
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
    let saved_strict = self.execution.active.strict;
    self.execution.active.strict = false;
    let result = self.set_property_value(target, &prop, value, &receiver);
    self.execution.active.strict = saved_strict;
    result
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
    if is_byte_view(obj) && canonical_numeric_index_from_key(key).is_some() {
      return Ok(
        byte_view_index_from_key(key)
          .and_then(|index| byte_view_index_value(obj, index))
          .unwrap_or(Value::Undefined),
      );
    }
    if obj.is_object() {
      return self.get_property_with_receiver(obj, key, obj);
    }
    if let Some((desc, _holder)) = self.lookup_property_descriptor(obj, key, 0)? {
      return match desc {
        PropertyDescriptor::Data {
          value: Value::Uninitialized,
          ..
        } => Err(self.throw_reference_error("Cannot access module binding before initialization")),
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
        return Ok(Value::Number(usize_to_f64(text.len())));
      }
      if let Some(i) = array_index_from_key(key)
        && let Some(ch) = text.slice(i..i.saturating_add(1))
      {
        return Ok(Value::String(ch));
      }
      if let Some(prototype) = self
        .realm_state
        .realm
        .intrinsics
        .string_prototype
        .clone()
      {
        return self.get_property_with_receiver(&prototype, key, obj);
      }
    }
    if let Value::Array(_, _) = obj {
      let array_ctor = self
        .realm_state
        .globals
        .borrow()
        .properties
        .get(&PropertyKey::String("Array".to_string().into()))
        .and_then(|d| match d {
          PropertyDescriptor::Data { value, .. } => Some(value.clone()),
          _ => None,
        });
      if let Some(array_ctor) = array_ctor
        && let Ok(proto) = self.get_property_value(
          &array_ctor,
          &PropertyKey::String("prototype".to_string().into()),
        )
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
        .realm_state
        .globals
        .borrow()
        .properties
        .get(&PropertyKey::String("Symbol".to_string().into()))
        .and_then(|d| match d {
          PropertyDescriptor::Data { value, .. } => Some(value.clone()),
          _ => None,
        });
      if let Some(symbol_ctor) = symbol_ctor
        && let Ok(proto) = self.get_property_value(
          &symbol_ctor,
          &PropertyKey::String("prototype".to_string().into()),
        )
        && !matches!(proto, Value::Null)
        && let Ok(v) = get_property(&proto, key, 0)
      {
        return Ok(v);
      }
    }
    if matches!(key, PropertyKey::String(name) if name == "constructor") {
      let proto = self.get_prototype(obj);
      if !matches!(proto, Value::Null)
        && let Ok(constructor) = get_property(
          &proto,
          &PropertyKey::String("constructor".to_string().into()),
          0,
        )
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
    if is_byte_view(obj) && canonical_numeric_index_from_key(key).is_some() {
      return Ok(
        byte_view_index_from_key(key)
          .and_then(|index| byte_view_index_value(obj, index))
          .map(|value| {
            (
              PropertyDescriptor::Data {
                value,
                writable: true,
                enumerable: true,
                configurable: true,
              },
              obj.clone(),
            )
          }),
      );
    }
    match obj {
      Value::Object(data) => {
        let d = data.borrow();
        if let Some(exports) = &d.module_namespace_exports
          && let PropertyKey::String(name) = key
          && let Some(binding) = exports.get(name)
        {
          return Ok(Some((
            PropertyDescriptor::Data {
              value: binding.value(),
              writable: true,
              enumerable: true,
              configurable: false,
            },
            obj.clone(),
          )));
        }
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
          && let Some(opt_v) = arr.borrow().element_cloned(i)
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
      Value::Promise(_, properties) => {
        let properties = properties.borrow();
        if let Some(descriptor) = properties.properties.get(key) {
          return Ok(Some((descriptor.clone(), obj.clone())));
        }
        let proto = properties.get_prototype();
        if !matches!(proto, Value::Null) {
          return self.lookup_property_descriptor(&proto, key, depth + 1);
        }
      }
      Value::Bool(_) => {
        if let Some(proto) = self
          .realm_state
          .realm
          .intrinsics
          .boolean_prototype
          .clone()
        {
          return self.lookup_property_descriptor(&proto, key, depth + 1);
        }
      }
      Value::Number(_) => {
        if let Some(proto) = self
          .realm_state
          .realm
          .intrinsics
          .number_prototype
          .clone()
        {
          return self.lookup_property_descriptor(&proto, key, depth + 1);
        }
      }
      Value::BigInt(_) => {
        if let Some(proto) = self
          .realm_state
          .realm
          .intrinsics
          .bigint_prototype
          .clone()
        {
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
    if is_module_namespace_object(obj) {
      if self.is_strict_mode() {
        return Err(self.throw_type_error("Cannot assign to a module namespace property"));
      }
      return Ok(());
    }
    if is_byte_view(obj) && canonical_numeric_index_from_key(key).is_some() {
      if let Some(index) = byte_view_index_from_key(key)
        && let Some(info) = byte_view_index_info(obj)
        && index < info.length
      {
        write_byte_view_index(self, &info, index, &value)?;
        self.bump_object_generation(obj);
      }
      return Ok(());
    }
    if matches!(obj, Value::Array(..)) && matches!(key, PropertyKey::String(s) if s == "length") {
      let descriptor = PartialDescriptor {
        value: Some(value),
        has_value: true,
        ..PartialDescriptor::default()
      };
      let accepted = self.define_own_property(obj, key.clone(), descriptor)?;
      if !accepted && self.is_strict_mode() {
        return Err(self.throw_type_error("Cannot assign to read only array length"));
      }
      if accepted {
        self.bump_object_generation(obj);
      }
      return Ok(());
    }
    if let Value::Array(_, props) = obj
      && let Some(index) = array_index_from_key(key)
    {
      let properties = props.borrow();
      let current_len = array_length(&properties);
      let length_writable = properties
        .properties
        .get(&PropertyKey::String("length".into()))
        .is_none_or(|descriptor| {
          matches!(descriptor, PropertyDescriptor::Data { writable: true, .. })
        });
      if index >= current_len && !length_writable {
        if self.is_strict_mode() {
          return Err(self.throw_type_error("Cannot extend an array with read only length"));
        }
        return Ok(());
      }
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
          self.execution.active.arguments.object.as_ref(),
          Some(current) if values_equal(current, obj)
      ) && let Some(index) = array_index_from_key(key)
        && index < self.execution.active.arguments.mapped_count
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
      && let Some(cell) = self.execution.active.locals.get(index)
    {
      cell.set(
        self
          .lookup_property_descriptor(obj, key, 0)?
          .and_then(|(desc, _)| match desc {
            PropertyDescriptor::Data { value, .. } => Some(value),
            _ => None,
          })
          .unwrap_or(Value::Undefined),
      );
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

fn is_module_namespace_object(value: &Value) -> bool {
  matches!(value, Value::Object(data) if data.borrow().module_namespace_exports.is_some())
}

fn object_is_extensible(value: &Value) -> bool {
  match value {
    Value::Object(data) => data.borrow().extensible,
    Value::Array(_, props) | Value::Closure { props, .. } | Value::BuiltinObject(_, props) => {
      props.borrow().extensible
    }
    Value::Proxy(_) | Value::Builtin(_) => false,
    Value::Promise(_, properties) => properties.borrow().extensible,
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
    Value::Builtin(_) => return !extensible,
    Value::Promise(_, properties) => properties.borrow_mut().extensible = extensible,
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
    Value::Array(_, props)
    | Value::Closure { props, .. }
    | Value::BuiltinObject(_, props)
    | Value::Promise(_, props) => {
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
