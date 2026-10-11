// SPDX-License-Identifier: MPL-2.0

use super::*;

impl Vm {
  pub(in crate::vm::runtime) fn read_descriptor_from_value(
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
      let key = PropertyKey::String(field.to_string().into());
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

  #[allow(clippy::too_many_lines)]
  pub(in crate::vm::runtime) fn define_own_property(
    &mut self,
    obj: &Value,
    key: PropertyKey,
    desc: PartialDescriptor,
  ) -> Result<bool, RuntimeError> {
    if let Value::Proxy(proxy) = obj {
      let (target, handler) = proxy_target_and_handler(self, proxy, "defineProperty")?;
      let trap = self.get_property_value(
        &handler,
        &PropertyKey::String("defineProperty".to_string().into()),
      )?;
      if matches!(trap, Value::Undefined | Value::Null) {
        return self.define_own_property(&target, key, desc);
      }
      if !Self::is_callable_value(&trap) {
        return Err(self.throw_type_error("Proxy defineProperty trap is not callable"));
      }
      let descriptor =
        partial_descriptor_to_value(self.realm_state.realm.object_prototype(), &desc);
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
          if matches!(
            target_desc,
            PropertyDescriptor::Data {
              configurable: false,
              writable: true,
              ..
            }
          ) && desc.has_writable
            && desc.writable == Some(false)
          {
            return Err(self.throw_type_error(
              "Proxy defineProperty trap cannot make a non-configurable writable property non-writable",
            ));
          }
        }
      }
      return Ok(true);
    }
    if let Value::Object(data) = obj {
      let export_value = {
        let data = data.borrow();
        match (&data.module_namespace_exports, &key) {
          (Some(exports), PropertyKey::String(name)) => exports
            .get(name)
            .map(crate::binding::BindingCell::value),
          _ => None,
        }
      };
      if let Some(export_value) = export_value {
        if desc.has_get
          || desc.has_set
          || desc.configurable == Some(true)
          || desc.enumerable == Some(false)
          || desc.writable == Some(false)
          || desc.has_value
            && desc
              .value
              .as_ref()
              .is_some_and(|value| !same_value(value, &export_value))
        {
          return Ok(false);
        }
        return Ok(true);
      }
    }
    if is_byte_view(obj)
      && let Some(numeric_index) = canonical_numeric_index_from_key(&key)
    {
      let Some(info) = byte_view_index_info(obj) else {
        return Ok(false);
      };
      if numeric_index.is_nan()
        || !numeric_index.is_finite()
        || numeric_index.is_sign_negative()
        || numeric_index.fract() != 0.0
        || numeric_index >= usize_to_f64(info.length)
      {
        return Ok(false);
      }
      if desc.has_get
        || desc.has_set
        || desc.configurable == Some(false)
        || desc.enumerable == Some(false)
        || desc.writable == Some(false)
      {
        return Ok(false);
      }
      if let Some(value) = &desc.value {
        write_byte_view_index(self, &info, numeric_index as usize, value)?;
        self.bump_object_generation(obj);
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

      let mut next_len = next_len.unwrap_or(current_len);
      if next_len < current_len {
        if !current_writable {
          return Ok(false);
        }
        let mut data = props.borrow_mut();
        let mut indexed_properties = data
          .properties
          .iter()
          .filter_map(|(prop_key, descriptor)| {
            array_index_from_key(prop_key)
              .filter(|index| *index >= next_len)
              .map(|index| (index, prop_key.clone(), descriptor_configurable(descriptor)))
          })
          .collect::<Vec<_>>();
        indexed_properties.sort_unstable_by_key(|(index, _, _)| std::cmp::Reverse(*index));
        let mut shrink_failed = false;
        for (index, prop_key, configurable) in indexed_properties {
          if configurable {
            data.properties.shift_remove(&prop_key);
          } else {
            next_len = index + 1;
            shrink_failed = true;
            break;
          }
        }
        drop(data);
        {
          let mut arr_mut = arr.borrow_mut();
          if next_len < arr_mut.len() {
            arr_mut.truncate(next_len);
          }
        }
        if shrink_failed {
          props.borrow_mut().properties.insert(
            key,
            PropertyDescriptor::Data {
              value: Value::Number(usize_to_f64(next_len)),
              writable: desc.writable.unwrap_or(current_writable),
              enumerable: current_enumerable,
              configurable: false,
            },
          );
          return Ok(false);
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
      Value::BuiltinObject(_, props) | Value::Promise(_, props) => props,
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

    let array_length_growth = if let Value::Array(_, _) = obj
      && let Some(index) = array_index_from_key(&key)
    {
      let current_len = array_length(&data);
      if index >= current_len {
        let length_writable = data
          .properties
          .get(&PropertyKey::String("length".into()))
          .is_none_or(|descriptor| {
            matches!(descriptor, PropertyDescriptor::Data { writable: true, .. })
          });
        if !length_writable {
          return Ok(false);
        }
        Some(index + 1)
      } else {
        None
      }
    } else {
      None
    };

    if let Some(ref cur) = current {
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
        let desc_is_accessor = desc.has_get || desc.has_set;
        let desc_is_data = desc.has_value || desc.has_writable;
        if matches!(cur, PropertyDescriptor::Data { .. }) && desc_is_accessor
          || matches!(cur, PropertyDescriptor::Accessor { .. }) && desc_is_data
        {
          return Ok(false);
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
        if let PropertyDescriptor::Accessor { get, set, .. } = &cur {
          if desc.has_get
            && !values_equal(
              desc.get.as_ref().unwrap_or(&Value::Undefined),
              get.as_ref().unwrap_or(&Value::Undefined),
            )
          {
            return Ok(false);
          }
          if desc.has_set
            && !values_equal(
              desc.set.as_ref().unwrap_or(&Value::Undefined),
              set.as_ref().unwrap_or(&Value::Undefined),
            )
          {
            return Ok(false);
          }
        }
      }
    }

    let is_accessor = if desc.has_get || desc.has_set {
      true
    } else if desc.has_value || desc.has_writable {
      false
    } else {
      matches!(current, Some(PropertyDescriptor::Accessor { .. }))
    };
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
    if let Some(new_length) = array_length_growth
      && let Some(PropertyDescriptor::Data { value, .. }) = data
        .properties
        .get_mut(&PropertyKey::String("length".into()))
    {
      *value = Value::Number(usize_to_f64(new_length));
    }
    if let Some(index) = array_index_from_key(&key)
      && index < self.execution.active.arguments.mapped_count
      && matches!(
          self.execution.active.arguments.object.as_ref(),
          Some(current) if values_equal(current, obj)
      )
      && let Some(value) = sync_arguments_value
      && let Some(cell) = self.execution.active.locals.get(index)
    {
      cell.set(value);
    }
    Ok(true)
  }
}

pub(in crate::vm::runtime::property) fn descriptor_configurable(desc: &PropertyDescriptor) -> bool {
  match desc {
    PropertyDescriptor::Data { configurable, .. }
    | PropertyDescriptor::Accessor { configurable, .. } => *configurable,
  }
}

pub(in crate::vm::runtime::property) fn descriptor_enumerable(desc: &PropertyDescriptor) -> bool {
  match desc {
    PropertyDescriptor::Data { enumerable, .. }
    | PropertyDescriptor::Accessor { enumerable, .. } => *enumerable,
  }
}

pub(in crate::vm::runtime::property) fn complete_property_descriptor(
  desc: PartialDescriptor
) -> PropertyDescriptor {
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

pub(in crate::vm::runtime::property) fn partial_descriptor_from_complete(
  desc: &PropertyDescriptor
) -> PartialDescriptor {
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

pub(in crate::vm::runtime::property) fn partial_descriptor_to_value(
  prototype: Value,
  desc: &PartialDescriptor,
) -> Value {
  let mut object = ObjectData::with_prototype(prototype);
  let mut insert = |name: &str, value: Value| {
    object.properties.insert(
      PropertyKey::String(name.to_string().into()),
      PropertyDescriptor::ordinary_data(value),
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

pub(in crate::vm::runtime::property) fn is_compatible_property_descriptor(
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
