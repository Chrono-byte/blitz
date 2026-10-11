// SPDX-License-Identifier: MPL-2.0

use crate::errors::RuntimeError;
use crate::realm::{Intrinsics, Realm};
use crate::value::{ObjectData, PropertyDescriptor, PropertyKey, Value};
use crate::vm::Vm;
use crate::vm::runtime::property::{get_property, list_own_keys_of_value};
use crate::vm::state::MAX_PROTOTYPE_DEPTH;
use js_string::JsString;
use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

impl Vm {
  pub(super) fn populate_realm_intrinsics(
    &mut self,
    intrinsics: Intrinsics,
  ) {
    self.realm_state.realm.set_intrinsics(intrinsics);
  }

  /// Debug helper: get a global value by name (cloned).
  pub fn get_global(
    &self,
    name: &str,
  ) -> Option<Value> {
    self
      .realm_state
      .globals
      .borrow()
      .properties
      .get(&PropertyKey::String(name.to_string().into()))
      .and_then(|d| match d {
        PropertyDescriptor::Data { value, .. } => Some(value.clone()),
        _ => None,
      })
  }

  /// Returns the current globals object for the active realm.
  pub fn globals_rc(&self) -> Rc<RefCell<ObjectData>> {
    Rc::clone(&self.realm_state.globals)
  }

  /// Returns a reference to the Realm this VM operates in.
  pub fn realm(&self) -> &Realm {
    &self.realm_state.realm
  }

  /// Returns a mutable reference to the Realm this VM operates in.
  pub fn realm_mut(&mut self) -> &mut Realm {
    &mut self.realm_state.realm
  }

  /// Returns whether the current execution frame is in strict mode.
  /// If there are no frames (root-level execution), returns false.
  pub fn is_strict_mode(&self) -> bool {
    self.execution.active.strict
  }

  pub(crate) fn active_strict_mode(&self) -> bool {
    self.execution.active.strict
  }

  pub(crate) fn set_current_strict_mode(
    &mut self,
    strict: bool,
  ) {
    self.execution.active.strict = strict;
  }

  /// Return own keys for a global value if it is an object or closure.
  pub fn list_global_own_keys(
    &self,
    name: &str,
  ) -> Option<Vec<String>> {
    let g = self.realm_state.globals.borrow();
    let v = g
      .properties
      .get(&PropertyKey::String(name.to_string().into()))
      .and_then(|d| match d {
        PropertyDescriptor::Data { value, .. } => Some(value.clone()),
        _ => None,
      })?;
    list_own_keys_of_value(&v).map(|keys| {
      keys
        .into_iter()
        .map(|key| key.to_string_lossy())
        .collect()
    })
  }

  /// Return prototype keys for a global's prototype property.
  pub fn list_global_prototype_keys(
    &self,
    name: &str,
  ) -> Option<Vec<String>> {
    let g = self.realm_state.globals.borrow();
    let v = g
      .properties
      .get(&PropertyKey::String(name.to_string().into()))
      .and_then(|d| match d {
        PropertyDescriptor::Data { value, .. } => Some(value.clone()),
        _ => None,
      })?;
    match get_property(&v, &PropertyKey::String("prototype".to_string().into()), 0) {
      Ok(pv) => list_own_keys_of_value(&pv).map(|keys| {
        keys
          .into_iter()
          .map(|key| key.to_string_lossy())
          .collect()
      }),
      Err(_) => None,
    }
  }

  /// Return own keys for a value when applicable.
  pub fn list_own_keys_of_value(
    &self,
    v: &Value,
  ) -> Option<Vec<String>> {
    list_own_keys_of_value(v).map(|keys| {
      keys
        .into_iter()
        .map(|key| key.to_string_lossy())
        .collect()
    })
  }

  /// Return enumerable string keys for `for-in`, walking the prototype
  /// chain and skipping duplicates.
  pub fn for_in_keys_of_value(
    &mut self,
    v: &Value,
  ) -> Result<Vec<JsString>, RuntimeError> {
    let mut keys = Vec::new();
    #[expect(
      clippy::mutable_key_type,
      reason = "JsString's interior mutability is limited to a derived UTF-8 cache"
    )]
    let mut seen: HashSet<JsString> = Default::default();
    let mut current = v.clone();
    let mut depth = 0usize;

    while !matches!(current, Value::Null) {
      if depth > MAX_PROTOTYPE_DEPTH {
        return Err(RuntimeError::from(
          "Prototype chain cycle detected or too deep",
        ));
      }

      for key in self.own_enumerable_keys_value(&current)? {
        if let PropertyKey::String(name) = key
          && seen.insert(name.clone())
        {
          keys.push(name);
        }
      }

      current = self.get_prototype(&current);
      depth += 1;
    }

    Ok(keys)
  }

  /// Shallow reference equality check for object-like runtime values.
  pub fn values_same_object(
    &self,
    a: &Value,
    b: &Value,
  ) -> bool {
    match (a, b) {
      (Value::Object(am), Value::Object(bm)) => Rc::ptr_eq(am, bm),
      (Value::Array(ar, _), Value::Array(br, _)) => Rc::ptr_eq(ar, br),
      (Value::Closure { props: ap, .. }, Value::Closure { props: bp, .. }) => Rc::ptr_eq(ap, bp),
      _ => false,
    }
  }
}
