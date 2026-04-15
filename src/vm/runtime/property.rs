use crate::errors::RuntimeError;
use crate::mutex_compat::MutexBorrowExt;
use crate::runtime_utils::usize_to_f64;
use crate::value::{PropertyDescriptor, PropertyKey, Value};
use crate::vm::Vm;
use crate::vm::runtime::execution::{
    array_index_from_key, array_length, get_property, set_property, values_equal,
};
use crate::vm::state::{MAX_PROTOTYPE_DEPTH, PartialDescriptor};

impl Vm {
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
        if let Value::Proxy(proxy) = obj {
            let proxy = proxy.borrow();
            if proxy.is_revoked() {
                return Err(self.throw_type_error("Cannot read property of revoked proxy"));
            }
            return self.get_property_value(&proxy.target, key);
        }
        if let Some((desc, _holder)) = self.lookup_property_descriptor(obj, key, 0)? {
            return match desc {
                PropertyDescriptor::Data { value, .. } => Ok(value),
                PropertyDescriptor::Accessor { get, .. } => {
                    if let Some(getter) = get {
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
        if let Some((desc, _holder)) = self.lookup_property_descriptor(obj, key, 0)? {
            match desc {
                PropertyDescriptor::Accessor { set, .. } => {
                    if let Some(setter) = set {
                        let _ = self.call_with_this(&setter, receiver, &[value])?;
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
    pub fn define_property_builtin(&mut self, args: &[Value]) -> Result<Value, RuntimeError> {
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
    pub fn has_property_host(&self, obj: &Value, key: &PropertyKey) -> Result<bool, RuntimeError> {
        if let Value::Proxy(proxy) = obj {
            let proxy = proxy.borrow();
            if proxy.is_revoked() {
                return Err(RuntimeError::from("Cannot operate on revoked proxy"));
            }
            return self.has_property_host(&proxy.target, key);
        }
        Ok(self.lookup_property_descriptor(obj, key, 0)?.is_some())
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
                let proxy = proxy.borrow();
                if proxy.is_revoked() {
                    return Err(self.throw_type_error("Cannot operate on revoked proxy"));
                }
                return self.delete_property_host(&proxy.target, key);
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
        let result = (|| -> Result<bool, RuntimeError> {
            if let Some((desc, _holder)) = self.lookup_property_descriptor(target, &prop, 0)? {
                match desc {
                    PropertyDescriptor::Accessor { set, .. } => {
                        if let Some(setter) = set {
                            let _ = self.call_with_this(&setter, &receiver, &[value])?;
                            self.bump_object_generation(&receiver);
                            return Ok(true);
                        }
                        return Ok(false);
                    }
                    PropertyDescriptor::Data {
                        writable: false, ..
                    } => return Ok(false),
                    PropertyDescriptor::Data { .. } => {}
                }
            }

            let receiver_can_accept = match &receiver {
                Value::Object(obj) => {
                    let obj = obj.borrow();
                    match obj.properties.get(&prop) {
                        Some(PropertyDescriptor::Data {
                            writable: false, ..
                        }) => false,
                        Some(PropertyDescriptor::Accessor { set: None, .. }) => false,
                        Some(_) => true,
                        None => obj.extensible,
                    }
                }
                Value::Array(_, props)
                | Value::Closure { props, .. }
                | Value::BuiltinObject(_, props) => {
                    let props = props.borrow();
                    match props.properties.get(&prop) {
                        Some(PropertyDescriptor::Data {
                            writable: false, ..
                        }) => false,
                        Some(PropertyDescriptor::Accessor { set: None, .. }) => false,
                        Some(_) => true,
                        None => props.extensible,
                    }
                }
                _ => false,
            };

            if !receiver_can_accept {
                return Ok(false);
            }

            self.set_property_tracked(&receiver, &prop, value)?;
            Ok(true)
        })();
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
    pub(super) fn define_own_property(
        &mut self,
        obj: &Value,
        key: PropertyKey,
        desc: PartialDescriptor,
    ) -> Result<bool, RuntimeError> {
        if let Value::Proxy(proxy) = obj {
            let proxy = proxy.borrow();
            if proxy.is_revoked() {
                return Err(self.throw_type_error("Cannot operate on revoked proxy"));
            }
            return self.define_own_property(&proxy.target, key, desc);
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
                Some(truncated as usize)
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
                    .filter(|prop_key| {
                        array_index_from_key(prop_key).is_some_and(|idx| idx >= next_len)
                    })
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
        let current =
            if let Value::Array(arr, _) = obj {
                let current = data.properties.get(&key).cloned();
                if current.is_some() {
                    current
                } else if let Some(index) = array_index_from_key(&key) {
                    arr.borrow().get(index).cloned().flatten().map(|value| {
                        PropertyDescriptor::Data {
                            value,
                            writable: true,
                            enumerable: true,
                            configurable: true,
                        }
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
        if let Value::Proxy(proxy) = obj {
            let proxy = proxy.borrow();
            if proxy.is_revoked() {
                return Err(self.throw_type_error("Cannot read property of revoked proxy"));
            }
            return self.get_property_value(&proxy.target, key);
        }
        if let Some((desc, _holder)) = self.lookup_property_descriptor(obj, key, 0)? {
            return match desc {
                PropertyDescriptor::Data { value, .. } => Ok(value),
                PropertyDescriptor::Accessor { get, .. } => {
                    if let Some(getter) = get {
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
                && let Ok(proto) = self
                    .get_property_value(&string_ctor, &PropertyKey::String("prototype".to_string()))
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
                && let Ok(proto) = self
                    .get_property_value(&array_ctor, &PropertyKey::String("prototype".to_string()))
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
                && let Ok(proto) = self
                    .get_property_value(&symbol_ctor, &PropertyKey::String("prototype".to_string()))
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
        if let Value::Proxy(proxy) = obj {
            let proxy = proxy.borrow();
            if proxy.is_revoked() {
                return Err(self.throw_type_error("Cannot operate on revoked proxy"));
            }
            return self.set_property_tracked(&proxy.target, key, value);
        }
        if let Value::Array(arr, props) = obj
            && matches!(key, PropertyKey::String(s) if s == "length")
            && let Value::Number(new_len_f) = value.clone()
        {
            let new_len = if new_len_f.is_nan() || new_len_f < 0.0 {
                0usize
            } else {
                new_len_f as usize
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
                    if let Some(setter) = set {
                        let _ = self.call_with_this(&setter, obj, &[value])?;
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
