//! Execution-context lexical environment records.
//!
//! This module provides a lightweight runtime model of declarative environment
//! records with parent links. It is intentionally minimal and is used as a
//! stepping stone toward full ECMAScript execution context semantics.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::errors::RuntimeError;
use crate::value::Value;

#[derive(Clone)]
/// A single lexical binding entry inside an environment record.
pub struct Binding {
    /// Shared storage cell for the binding value.
    pub cell: Rc<RefCell<Value>>,
    /// Whether assignment is permitted after initialization.
    pub mutable: bool,
    /// Whether the binding has been initialized.
    pub initialized: bool,
}

#[derive(Clone)]
/// Declarative environment record with an optional outer environment.
pub struct EnvironmentRecord {
    outer: Option<Rc<RefCell<EnvironmentRecord>>>,
    bindings: HashMap<String, Binding>,
}

impl EnvironmentRecord {
    /// Create a new declarative environment with an optional outer link.
    pub fn new(outer: Option<Rc<RefCell<EnvironmentRecord>>>) -> Self {
        Self {
            outer,
            bindings: HashMap::new(),
        }
    }

    /// Return the outer environment, if any.
    pub fn outer(&self) -> Option<Rc<RefCell<EnvironmentRecord>>> {
        self.outer.clone()
    }

    /// Create a mutable binding in this record.
    pub fn create_mutable_binding(
        &mut self,
        name: String,
        cell: Rc<RefCell<Value>>,
        initialized: bool,
    ) {
        self.bindings.insert(
            name,
            Binding {
                cell,
                mutable: true,
                initialized,
            },
        );
    }

    /// Create an immutable binding in this record.
    pub fn create_immutable_binding(
        &mut self,
        name: String,
        cell: Rc<RefCell<Value>>,
        initialized: bool,
    ) {
        self.bindings.insert(
            name,
            Binding {
                cell,
                mutable: false,
                initialized,
            },
        );
    }

    /// Return whether this record directly contains `name`.
    pub fn has_binding(&self, name: &str) -> bool {
        self.bindings.contains_key(name)
    }

    /// Resolve and read a binding value from this record chain.
    pub fn get_binding_value(&self, name: &str) -> Result<Option<Value>, RuntimeError> {
        if let Some(binding) = self.bindings.get(name) {
            if !binding.initialized {
                return Err(RuntimeError::from(
                    "ReferenceError: Cannot access lexical binding before initialization",
                ));
            }
            return Ok(Some(binding.cell.borrow().clone()));
        }
        if let Some(outer) = &self.outer {
            return outer.borrow().get_binding_value(name);
        }
        Ok(None)
    }

    /// Assign through the environment chain.
    pub fn set_mutable_binding(&mut self, name: &str, value: Value) -> Result<bool, RuntimeError> {
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
            return outer.borrow_mut().set_mutable_binding(name, value);
        }
        Ok(false)
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
            err.to_string()
                .contains("Cannot access lexical binding before initialization")
        );
    }

    #[test]
    fn immutable_binding_cannot_be_reassigned() {
        let mut env = EnvironmentRecord::new(None);
        let cell = Rc::new(RefCell::new(Value::Number(1.0)));
        env.create_immutable_binding("x".to_string(), Rc::clone(&cell), true);
        let err = env
            .set_mutable_binding("x", Value::Number(2.0))
            .expect_err("const assignment should throw");
        assert!(err.to_string().contains("Assignment to constant variable"));
    }
}
