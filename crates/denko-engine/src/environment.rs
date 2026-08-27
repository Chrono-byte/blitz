// SPDX-License-Identifier: MPL-2.0
//! Execution-context lexical environment records.
//!
//! This module provides a lightweight runtime model of lexical and object
//! environment records with parent links. The VM uses these records for
//! lexical bindings, direct `eval`, and `with`-environment lookup.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::errors::RuntimeError;
use crate::value::{SharedValue, Value};

#[derive(Clone, Debug)]
/// A single lexical binding entry inside an environment record.
pub struct Binding {
  /// Shared storage cell for the binding value.
  pub cell: SharedValue,
  /// Whether assignment is permitted after initialization.
  pub mutable: bool,
  /// Whether the binding has been initialized.
  pub initialized: bool,
  /// Whether this is a lexical (`let`, `const`, or class) binding.
  pub lexical: bool,
}

#[derive(Clone, Debug)]
/// Declarative environment record with an optional outer environment.
pub struct EnvironmentRecord {
  outer: Option<Rc<RefCell<EnvironmentRecord>>>,
  bindings: HashMap<String, Binding>,
  binding_object: Option<Value>,
  with_environment: bool,
  has_object_environment: bool,
}

impl EnvironmentRecord {
  /// Create a new declarative environment with an optional outer link.
  pub fn new(outer: Option<Rc<RefCell<EnvironmentRecord>>>) -> Self {
    let has_object_environment = outer
      .as_ref()
      .is_some_and(|outer| outer.borrow().has_object_environment);
    Self {
      outer,
      bindings: HashMap::new(),
      binding_object: None,
      with_environment: false,
      has_object_environment,
    }
  }

  /// Create an object-backed environment, used by `with`.
  pub fn new_object(
    outer: Option<Rc<RefCell<EnvironmentRecord>>>,
    binding_object: Value,
    with_environment: bool,
  ) -> Self {
    Self {
      outer,
      bindings: HashMap::new(),
      binding_object: Some(binding_object),
      with_environment,
      has_object_environment: true,
    }
  }

  /// Return the outer environment, if any.
  pub fn outer(&self) -> Option<Rc<RefCell<EnvironmentRecord>>> {
    self.outer.clone()
  }

  /// Return the object used for object-environment binding resolution, if any.
  pub fn binding_object(&self) -> Option<Value> {
    self.binding_object.clone()
  }

  /// Return whether this object environment applies `with` / @@unscopables rules.
  pub fn is_with_environment(&self) -> bool {
    self.with_environment
  }

  /// Return whether this record or one of its outers is object-backed.
  pub fn has_object_environment(&self) -> bool {
    self.has_object_environment
  }

  /// Create a mutable binding in this record.
  pub fn create_mutable_binding(
    &mut self,
    name: String,
    cell: SharedValue,
    initialized: bool,
  ) {
    self.bindings.insert(
      name,
      Binding {
        cell,
        mutable: true,
        initialized,
        lexical: false,
      },
    );
  }

  /// Create a mutable lexical binding in this record.
  pub fn create_mutable_lexical_binding(
    &mut self,
    name: String,
    cell: SharedValue,
    initialized: bool,
  ) {
    self.bindings.insert(
      name,
      Binding {
        cell,
        mutable: true,
        initialized,
        lexical: true,
      },
    );
  }

  /// Create an immutable binding in this record.
  pub fn create_immutable_binding(
    &mut self,
    name: String,
    cell: SharedValue,
    initialized: bool,
  ) {
    self.bindings.insert(
      name,
      Binding {
        cell,
        mutable: false,
        initialized,
        lexical: true,
      },
    );
  }

  /// Mark an existing binding as initialized, if present.
  pub fn set_binding_initialized(
    &mut self,
    name: &str,
    initialized: bool,
  ) -> bool {
    if let Some(binding) = self.bindings.get_mut(name) {
      binding.initialized = initialized;
      return true;
    }
    false
  }

  /// Mark the first matching binding in this record chain as initialized.
  pub fn set_binding_initialized_in_chain(
    &mut self,
    name: &str,
    initialized: bool,
  ) -> bool {
    if let Some(binding) = self.bindings.get_mut(name) {
      binding.initialized = initialized;
      let cell = Rc::clone(&binding.cell);
      if let Some(outer) = &self.outer {
        outer
          .borrow_mut()
          .set_binding_initialized_for_cell(name, &cell, initialized);
      }
      return true;
    }
    if let Some(outer) = &self.outer {
      return outer
        .borrow_mut()
        .set_binding_initialized_in_chain(name, initialized);
    }
    false
  }

  fn set_binding_initialized_for_cell(
    &mut self,
    name: &str,
    cell: &SharedValue,
    initialized: bool,
  ) {
    if let Some(binding) = self.bindings.get_mut(name)
      && Rc::ptr_eq(&binding.cell, cell)
    {
      binding.initialized = initialized;
    }
    if let Some(outer) = &self.outer {
      outer
        .borrow_mut()
        .set_binding_initialized_for_cell(name, cell, initialized);
    }
  }

  /// Mark the binding backed by `cell` initialized anywhere in this chain.
  pub fn set_binding_initialized_for_cell_in_chain(
    &mut self,
    cell: &SharedValue,
    initialized: bool,
  ) -> bool {
    if let Some(binding) = self
      .bindings
      .values_mut()
      .find(|binding| Rc::ptr_eq(&binding.cell, cell))
    {
      binding.initialized = initialized;
      return true;
    }
    self.outer.as_ref().is_some_and(|outer| {
      outer
        .borrow_mut()
        .set_binding_initialized_for_cell_in_chain(cell, initialized)
    })
  }

  /// Return whether this record directly contains `name`.
  pub fn has_binding(
    &self,
    name: &str,
  ) -> bool {
    self.bindings.contains_key(name)
  }

  /// Return whether this record directly contains a lexical binding named `name`.
  pub fn has_lexical_binding(
    &self,
    name: &str,
  ) -> bool {
    self
      .bindings
      .get(name)
      .is_some_and(|binding| binding.lexical)
  }

  /// Return the storage cell for an own binding without walking outer records.
  pub fn own_binding_cell(
    &self,
    name: &str,
  ) -> Option<SharedValue> {
    self
      .bindings
      .get(name)
      .map(|binding| Rc::clone(&binding.cell))
  }

  /// Return mutability and initialization state for an own binding.
  pub fn own_binding_state(
    &self,
    name: &str,
  ) -> Option<(bool, bool)> {
    self
      .bindings
      .get(name)
      .map(|binding| (binding.mutable, binding.initialized))
  }

  /// Read a binding from this record without walking outer environments.
  ///
  /// # Errors
  ///
  /// Returns a [`crate::errors::RuntimeError`] if the binding is not initialized.
  pub fn get_own_binding_value(
    &self,
    name: &str,
  ) -> Result<Option<Value>, RuntimeError> {
    if let Some(binding) = self.bindings.get(name) {
      if !binding.initialized {
        return Err(RuntimeError::from(
          "ReferenceError: Cannot access lexical binding before initialization",
        ));
      }
      return Ok(Some(binding.cell.borrow().clone()));
    }
    Ok(None)
  }

  /// Resolve and read a binding value from this record chain.
  ///
  /// # Errors
  ///
  /// Returns a [`crate::errors::RuntimeError`] if the binding is not initialized.
  pub fn get_binding_value(
    &self,
    name: &str,
  ) -> Result<Option<Value>, RuntimeError> {
    if let Some(value) = self.get_own_binding_value(name)? {
      return Ok(Some(value));
    }
    if let Some(outer) = &self.outer {
      return outer.borrow().get_binding_value(name);
    }
    Ok(None)
  }

  /// Assign through the environment chain.
  ///
  /// # Errors
  ///
  /// Returns a [`crate::errors::RuntimeError`] if an initialized immutable
  /// binding is assigned.
  pub fn set_mutable_binding(
    &mut self,
    name: &str,
    value: Value,
  ) -> Result<bool, RuntimeError> {
    if let Some(binding) = self.bindings.get_mut(name) {
      if !binding.mutable && binding.initialized {
        return Err(RuntimeError::from(format!(
          "TypeError: Assignment to constant variable '{}'",
          name
        )));
      }
      *binding.cell.borrow_mut() = value;
      binding.initialized = true;
      return Ok(true);
    }
    if let Some(outer) = &self.outer {
      return outer
        .borrow_mut()
        .set_mutable_binding(name, value);
    }
    Ok(false)
  }

  /// Collect all currently stored binding values in this environment chain.
  ///
  /// This is used by the prototype GC as a conservative root walk for live
  /// lexical environments.
  pub fn collect_binding_values(
    &self,
    values: &mut Vec<Value>,
  ) {
    for binding in self.bindings.values() {
      values.push(binding.cell.borrow().clone());
    }
    if let Some(outer) = &self.outer {
      outer.borrow().collect_binding_values(values);
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn uninitialized_binding_read_throws() {
    let mut env = EnvironmentRecord::new(None);
    let cell = Rc::new(RefCell::new(Value::Uninitialized));
    env.create_mutable_binding("x".to_string(), cell, false);
    let err = env
      .get_binding_value("x")
      .expect_err("expected tdz-style reference error");
    assert!(
      err
        .to_string()
        .contains("Cannot access lexical binding before initialization")
    );
  }

  #[test]
  fn immutable_binding_cannot_be_reassigned() {
    let mut env = EnvironmentRecord::new(None);
    let cell = Rc::new(RefCell::new(Value::Number(1.0)));
    env.create_immutable_binding("x".to_string(), cell, true);
    let err = env
      .set_mutable_binding("x", Value::Number(2.0))
      .expect_err("const assignment should throw");
    assert!(
      err
        .to_string()
        .contains("Assignment to constant variable")
    );
  }
}
