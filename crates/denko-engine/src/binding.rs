// SPDX-License-Identifier: MPL-2.0

//! Mutable storage identity for ECMAScript bindings.

use crate::value::Value;
use std::cell::RefCell;
use std::rc::Rc;

/// A shared binding location.
///
/// Policy such as mutability belongs to the environment entry that refers to
/// this cell. This type owns only storage identity and the current value, so a
/// read-only import can observe an exporter updating the same location.
#[derive(Clone, Debug)]
pub struct BindingCell(Rc<RefCell<Value>>);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct BindingIdentity(*const RefCell<Value>);

impl BindingCell {
  pub fn new(value: Value) -> Self {
    Self(Rc::new(RefCell::new(value)))
  }

  pub fn value(&self) -> Value {
    self.0.borrow().clone()
  }

  pub fn set(
    &self,
    value: Value,
  ) {
    *self.0.borrow_mut() = value;
  }

  pub fn replace(
    &self,
    value: Value,
  ) -> Value {
    std::mem::replace(&mut self.0.borrow_mut(), value)
  }

  pub fn is_uninitialized(&self) -> bool {
    matches!(*self.0.borrow(), Value::Uninitialized)
  }

  pub fn same_storage(
    &self,
    other: &Self,
  ) -> bool {
    Rc::ptr_eq(&self.0, &other.0)
  }

  pub fn identity(&self) -> BindingIdentity {
    BindingIdentity(Rc::as_ptr(&self.0))
  }

  pub(crate) fn strong_count(&self) -> usize {
    Rc::strong_count(&self.0)
  }
}
