// SPDX-License-Identifier: MPL-2.0

use crate::errors::RuntimeError;
use crate::value::Value;
use crate::vm::state::arena::{ArenaHandle, GenerationArena, WeakArenaHandle};
use crate::{binding::BindingCell, closure_environment::ClosureEnvironment};
use std::cell::RefCell;
use std::rc::Rc;

/// VM-owned roots and host-visible handles.
pub(crate) struct VmMemory {
  pub(crate) temporary: Vec<Value>,
  /// Objects returned from WeakRef.prototype.deref until the current job ends.
  pub(crate) kept_alive: Vec<Value>,
  host: Rc<RefCell<GenerationArena<Value>>>,
  closure_environments: Rc<RefCell<GenerationArena<Vec<BindingCell>>>>,
}

impl VmMemory {
  pub(crate) fn new() -> Self {
    Self {
      temporary: Vec::new(),
      kept_alive: Vec::new(),
      host: Rc::new(RefCell::new(GenerationArena::new())),
      closure_environments: Rc::new(RefCell::new(GenerationArena::new())),
    }
  }

  pub(crate) fn fork(&self) -> Self {
    Self {
      temporary: Vec::new(),
      kept_alive: Vec::new(),
      host: Rc::clone(&self.host),
      closure_environments: Rc::clone(&self.closure_environments),
    }
  }

  pub(crate) fn share_host_roots_from(
    &mut self,
    other: &Self,
  ) {
    self.host = Rc::clone(&other.host);
  }

  pub(crate) fn create_host_root(
    &mut self,
    value: Value,
  ) -> HostRoot {
    HostRoot(self.host.borrow_mut().insert(value))
  }

  pub(crate) fn allocate_closure_environment(
    &mut self,
    cells: Vec<BindingCell>,
  ) -> ClosureEnvironment {
    let environment = ClosureEnvironment::allocate(Rc::clone(&self.closure_environments), cells);
    crate::gc::register_closure_environment(&environment);
    environment
  }

  pub(crate) fn set_host_root(
    &mut self,
    root: HostRoot,
    value: Value,
  ) -> Result<(), RuntimeError> {
    let mut arena = self.host.borrow_mut();
    let retained = arena
      .get_mut(root.0)
      .ok_or_else(|| RuntimeError::from("Invalid host root"))?;
    *retained = value;
    Ok(())
  }

  pub(crate) fn host_root(
    &self,
    root: HostRoot,
  ) -> Option<Value> {
    self.host.borrow().get(root.0).cloned()
  }

  pub(crate) fn release_host_root(
    &mut self,
    root: HostRoot,
  ) -> Result<(), RuntimeError> {
    self
      .host
      .borrow_mut()
      .remove(root.0)
      .map(drop)
      .ok_or_else(|| RuntimeError::from("Invalid host root"))
  }

  pub(crate) fn downgrade_host_root(
    &self,
    root: HostRoot,
  ) -> Option<WeakHostRoot> {
    self
      .host
      .borrow()
      .downgrade(root.0)
      .map(WeakHostRoot)
  }

  pub(crate) fn upgrade_host_root(
    &self,
    root: WeakHostRoot,
  ) -> Option<HostRoot> {
    self.host.borrow().upgrade(root.0).map(HostRoot)
  }

  pub(crate) fn append_host_roots(
    &self,
    roots: &mut Vec<Value>,
  ) {
    roots.extend(self.host.borrow().values().cloned());
  }

  pub(crate) fn snapshot(&self) -> VmMemorySnapshot {
    let host = self.host.borrow().snapshot();
    let closure_environments = self.closure_environments.borrow().snapshot();
    VmMemorySnapshot {
      allocated_host_roots: host.allocations,
      live_host_roots: host.live,
      released_host_roots: host.removals,
      temporary_roots: self.temporary.len(),
      kept_alive_roots: self.kept_alive.len(),
      allocated_closure_environments: closure_environments.allocations,
      live_closure_environments: closure_environments.live,
      reclaimed_closure_environments: closure_environments.removals,
    }
  }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct VmMemorySnapshot {
  pub allocated_host_roots: usize,
  pub live_host_roots: usize,
  pub released_host_roots: usize,
  pub temporary_roots: usize,
  pub kept_alive_roots: usize,
  pub allocated_closure_environments: usize,
  pub live_closure_environments: usize,
  pub reclaimed_closure_environments: usize,
}

/// Opaque, generation-checked handle to a value retained for a host integration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HostRoot(pub(crate) ArenaHandle);

/// Non-retaining observation of a host root. It upgrades only while the exact
/// allocation generation remains live.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WeakHostRoot(pub(crate) WeakArenaHandle);
