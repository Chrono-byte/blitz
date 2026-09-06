// SPDX-License-Identifier: MPL-2.0

use crate::errors::RuntimeError;
use crate::runtime_utils::usize_to_f64;
use crate::value::{ObjectData, PropertyDescriptor, PropertyKey, Value};
use crate::vm::Vm;
use crate::vm::state::{MAX_DENSE_ARRAY_GROWTH, MAX_PROTOTYPE_DEPTH};
use std::rc::Rc;

/// Return own keys for a value when applicable.
pub(in crate::vm::runtime) fn list_own_keys_of_value(
  v: &Value
) -> Option<Vec<js_string::JsString>> {
  match v {
    Value::Object(data) | Value::Promise(_, data) => {
      let data = data.borrow();
      Some(
        data
          .module_namespace_exports
          .as_ref()
          .map_or_else(
            || {
              data
                .properties
                .keys()
                .filter_map(|key| match key {
                  PropertyKey::String(name) => Some(name.clone()),
                  PropertyKey::Symbol(_) => None,
                })
                .collect()
            },
            |exports| exports.keys().cloned().collect(),
          ),
      )
    }
    Value::Closure { props, .. } => Some(
      props
        .borrow()
        .properties
        .keys()
        .filter_map(|k| match k {
          PropertyKey::String(s) => Some(s.clone()),
          PropertyKey::Symbol(_) => None,
        })
        .collect(),
    ),
    Value::Proxy(proxy) => {
      let proxy = proxy.borrow();
      if proxy.is_revoked() {
        Some(Vec::new())
      } else {
        list_own_keys_of_value(&proxy.target)
      }
    }
    _ => None,
  }
}

pub(in crate::vm::runtime) fn object_identity(obj: &Value) -> Option<usize> {
  match obj {
    Value::Object(map) => Some(map.borrow().identity as usize),
    Value::Array(_, props) => Some(props.borrow().identity as usize),
    Value::Proxy(proxy) => Some(proxy.borrow().identity as usize),
    Value::Closure { props, .. } => Some(props.borrow().identity as usize),
    Value::BuiltinObject(_, props) | Value::Promise(_, props) => {
      Some(props.borrow().identity as usize)
    }
    _ => None,
  }
}

/// Reference-equality semantics for objects/arrays/closures, structural
/// equality for primitives. Matches JavaScript `===` behaviour.
pub(in crate::vm::runtime) fn values_equal(
  a: &Value,
  b: &Value,
) -> bool {
  match (a, b) {
    (Value::Undefined, Value::Undefined) => true,
    (Value::Number(x), Value::Number(y)) => x == y,
    (Value::String(x), Value::String(y)) => x == y,
    (Value::Bool(x), Value::Bool(y)) => x == y,
    (Value::BigInt(x), Value::BigInt(y)) => x == y,
    (Value::Symbol(x, _), Value::Symbol(y, _)) => x == y,
    (Value::Null, Value::Null) => true,
    (Value::Object(am), Value::Object(bm)) => Rc::ptr_eq(am, bm),
    (Value::Array(ar, _), Value::Array(br, _)) => Rc::ptr_eq(ar, br),
    (Value::Proxy(ap), Value::Proxy(bp)) => Rc::ptr_eq(ap, bp),
    (Value::Closure { props: ap, .. }, Value::Closure { props: bp, .. }) => Rc::ptr_eq(ap, bp),
    (Value::BuiltinObject(_, ap), Value::BuiltinObject(_, bp)) => Rc::ptr_eq(ap, bp),
    (Value::Promise(ap, _), Value::Promise(bp, _)) => Rc::ptr_eq(ap, bp),
    (Value::Builtin(af), Value::Builtin(bf)) => Rc::ptr_eq(af, bf),
    _ => false,
  }
}

/// ECMAScript SameValue semantics used by `Object.is`.
pub fn same_value(
  a: &Value,
  b: &Value,
) -> bool {
  match (a, b) {
    (Value::Undefined, Value::Undefined) => true,
    (Value::Null, Value::Null) => true,
    (Value::Bool(x), Value::Bool(y)) => x == y,
    (Value::String(x), Value::String(y)) => x == y,
    (Value::BigInt(x), Value::BigInt(y)) => x == y,
    (Value::Symbol(x, _), Value::Symbol(y, _)) => x == y,
    (Value::Number(x), Value::Number(y)) => {
      if x.is_nan() && y.is_nan() {
        true
      } else if *x == 0.0 && *y == 0.0 {
        x.is_sign_positive() == y.is_sign_positive()
      } else {
        x == y
      }
    }
    (Value::Object(am), Value::Object(bm)) => Rc::ptr_eq(am, bm),
    (Value::Array(ar, _), Value::Array(br, _)) => Rc::ptr_eq(ar, br),
    (Value::Proxy(ap), Value::Proxy(bp)) => Rc::ptr_eq(ap, bp),
    (Value::Closure { props: ap, .. }, Value::Closure { props: bp, .. }) => Rc::ptr_eq(ap, bp),
    (Value::BuiltinObject(_, ap), Value::BuiltinObject(_, bp)) => Rc::ptr_eq(ap, bp),
    (Value::Promise(ap, _), Value::Promise(bp, _)) => Rc::ptr_eq(ap, bp),
    _ => false,
  }
}

pub(in crate::vm::runtime) fn array_index_from_key(key: &PropertyKey) -> Option<usize> {
  let PropertyKey::String(s) = key else {
    return None;
  };
  if s.is_empty() {
    return None;
  }
  if s != "0" && s.code_unit_at(0) == Some(u16::from(b'0')) {
    return None;
  }
  let idx = s.parse::<u32>().ok()?;
  if idx == u32::MAX {
    return None;
  }
  if !crate::runtime_utils::decimal_u64_eq_str(idx as u64, s.as_str_lossy()) {
    return None;
  }
  Some(idx as usize)
}

pub(in crate::vm::runtime) fn array_length(props: &ObjectData) -> usize {
  props
    .properties
    .get(&PropertyKey::String("length".to_string().into()))
    .and_then(|desc| match desc {
      PropertyDescriptor::Data {
        value: Value::Number(n),
        ..
      } if n.is_finite() && *n >= 0.0 => Some(*n as usize),
      _ => None,
    })
    .unwrap_or(0)
}

/// Walk the prototype chain with cycle detection.
pub(in crate::vm::runtime) fn get_property(
  obj: &Value,
  key: &PropertyKey,
  depth: usize,
) -> Result<Value, RuntimeError> {
  if depth > MAX_PROTOTYPE_DEPTH {
    return Err(RuntimeError::from(
      "Prototype chain cycle detected or too deep",
    ));
  }

  match obj {
    Value::Object(data) => {
      let d = data.borrow();
      if let Some(desc) = d.properties.get(key) {
        return match desc {
          PropertyDescriptor::Data { value, .. } => Ok(value.clone()),
          PropertyDescriptor::Accessor { .. } => Ok(Value::Undefined),
        };
      }
      let proto = d.get_prototype();
      if !matches!(proto, Value::Null) {
        return get_property(&proto, key, depth + 1);
      }
      Ok(Value::Undefined)
    }
    Value::Array(arr, props) => {
      let props_ref = props.borrow();
      if let Some(desc) = props_ref.properties.get(key) {
        return match desc {
          PropertyDescriptor::Data { value, .. } => Ok(value.clone()),
          PropertyDescriptor::Accessor { .. } => Ok(Value::Undefined),
        };
      }
      if let Some(i) = array_index_from_key(key)
        && i < array_length(&props_ref)
        && let Some(opt_value) = arr.borrow().element_cloned(i)
        && let Some(value) = opt_value
      {
        return Ok(value);
      }
      let proto = props_ref.get_prototype();
      if !matches!(proto, Value::Null) {
        return get_property(&proto, key, depth + 1);
      }
      Ok(Value::Undefined)
    }
    Value::Closure { props, .. } => {
      let p = props.borrow();
      if let Some(desc) = p.properties.get(key) {
        return match desc {
          PropertyDescriptor::Data { value, .. } => Ok(value.clone()),
          PropertyDescriptor::Accessor { get, .. } => {
            if let Some(Value::Closure { .. } | Value::Builtin(_)) = get {
              Ok(Value::Undefined)
            } else {
              Ok(Value::Undefined)
            }
          }
        };
      }
      let proto = p.get_prototype();
      if !matches!(proto, Value::Null) {
        return get_property(&proto, key, depth + 1);
      }
      Ok(Value::Undefined)
    }
    Value::BuiltinObject(_, props) | Value::Promise(_, props) => {
      let p = props.borrow();
      if let Some(desc) = p.properties.get(key) {
        return match desc {
          PropertyDescriptor::Data { value, .. } => Ok(value.clone()),
          PropertyDescriptor::Accessor { get, .. } => {
            if let Some(Value::Closure { .. } | Value::Builtin(_)) = get {
              Ok(Value::Undefined)
            } else {
              Ok(Value::Undefined)
            }
          }
        };
      }
      let proto = p.get_prototype();
      if !matches!(proto, Value::Null) {
        return get_property(&proto, key, depth + 1);
      }
      Ok(Value::Undefined)
    }
    _ => Ok(Value::Undefined),
  }
}

/// Sets the property with the given key on the given object to the given value.
pub(in crate::vm::runtime) fn set_property(
  obj: &Value,
  key: &PropertyKey,
  value: Value,
) -> Result<(), RuntimeError> {
  match obj {
    Value::Object(map) => {
      let mut m = map.borrow_mut();
      if let Some(desc) = m.properties.get_mut(key) {
        match desc {
          PropertyDescriptor::Data {
            value: v, writable, ..
          } => {
            if !*writable {
              return Ok(());
            }
            *v = value;
            return Ok(());
          }
          PropertyDescriptor::Accessor { set, .. } => {
            if set.is_some() {
              return Ok(());
            }
            return Ok(());
          }
        }
      }
      if !m.extensible {
        return Ok(());
      }
      m.properties
        .insert(key.clone(), PropertyDescriptor::ordinary_data(value));
      Ok(())
    }
    Value::Array(arr, props) => {
      if let Some(i) = array_index_from_key(key) {
        let mut p = props.borrow_mut();
        if let Some(descriptor) = p.properties.get_mut(key) {
          if let PropertyDescriptor::Data {
            value: current,
            writable: true,
            ..
          } = descriptor
          {
            *current = value;
          }
          return Ok(());
        }
        let current_len = array_length(&p);
        let next_len = current_len.max(i.saturating_add(1));
        if next_len > current_len
          && p
            .properties
            .get(&PropertyKey::String("length".into()))
            .is_some_and(|descriptor| {
              !matches!(descriptor, PropertyDescriptor::Data { writable: true, .. })
            })
        {
          return Ok(());
        }
        let dense_limit = arr
          .borrow()
          .len()
          .saturating_add(MAX_DENSE_ARRAY_GROWTH);
        if i <= dense_limit {
          let mut a = arr.borrow_mut();
          if i >= a.len() {
            a.resize(i + 1, None);
          }
          a[i] = Some(value);
        } else {
          p.properties
            .insert(key.clone(), PropertyDescriptor::ordinary_data(value));
        }
        if next_len > current_len
          && let Some(PropertyDescriptor::Data {
            value: length_value,
            ..
          }) = p
            .properties
            .get_mut(&PropertyKey::String("length".into()))
        {
          *length_value = Value::Number(usize_to_f64(next_len));
        }
        return Ok(());
      }
      if matches!(key, PropertyKey::String(s) if s == "length")
        && let Value::Number(new_len_f) = value
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
        let mut p = props.borrow_mut();
        p.properties.insert(
          PropertyKey::String("length".to_string().into()),
          PropertyDescriptor::ordinary_data(Value::Number(usize_to_f64(new_len))),
        );
        return Ok(());
      }
      let mut p = props.borrow_mut();
      if let Some(desc) = p.properties.get_mut(key) {
        match desc {
          PropertyDescriptor::Data {
            value: v, writable, ..
          } => {
            if !*writable {
              return Ok(());
            }
            *v = value;
            return Ok(());
          }
          PropertyDescriptor::Accessor { set, .. } => {
            if set.is_some() {
              return Ok(());
            }
            return Ok(());
          }
        }
      }
      if !p.extensible {
        return Ok(());
      }
      p.properties
        .insert(key.clone(), PropertyDescriptor::ordinary_data(value));
      Ok(())
    }
    Value::Closure { props, .. } => {
      let mut p = props.borrow_mut();
      if let Some(desc) = p.properties.get_mut(key) {
        match desc {
          PropertyDescriptor::Data {
            value: v, writable, ..
          } => {
            if !*writable {
              return Ok(());
            }
            *v = value;
            return Ok(());
          }
          PropertyDescriptor::Accessor { set, .. } => {
            if set.is_some() {
              return Ok(());
            }
            return Ok(());
          }
        }
      }
      if !p.extensible {
        return Ok(());
      }
      p.properties
        .insert(key.clone(), PropertyDescriptor::ordinary_data(value));
      Ok(())
    }
    Value::BuiltinObject(_, props) | Value::Promise(_, props) => {
      let mut p = props.borrow_mut();
      if let Some(desc) = p.properties.get_mut(key) {
        match desc {
          PropertyDescriptor::Data {
            value: v, writable, ..
          } => {
            if !*writable {
              return Ok(());
            }
            *v = value;
            return Ok(());
          }
          PropertyDescriptor::Accessor { set, .. } => {
            if set.is_some() {
              return Ok(());
            }
            return Ok(());
          }
        }
      }
      if !p.extensible {
        return Ok(());
      }
      p.properties
        .insert(key.clone(), PropertyDescriptor::ordinary_data(value));
      Ok(())
    }
    _ => Ok(()),
  }
}
impl Vm {
  pub(in crate::vm::runtime) fn bump_object_generation(
    &mut self,
    obj: &Value,
  ) {
    let data = match obj {
      Value::Object(data) => Some(data),
      Value::Array(_, props)
      | Value::Closure { props, .. }
      | Value::BuiltinObject(_, props)
      | Value::Promise(_, props) => Some(props),
      _ => None,
    };
    if let Some(data) = data {
      let mut data = data.borrow_mut();
      data.mutation_generation = data.mutation_generation.wrapping_add(1);
    }
  }
}
