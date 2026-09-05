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
  empty_closure_environment: Option<crate::closure_environment::WeakClosureEnvironment>,
  closure_environments: Rc<RefCell<GenerationArena<Vec<BindingCell>>>>,
}

impl VmMemory {
  pub(crate) fn new() -> Self {
    Self {
      temporary: Vec::new(),
      empty_closure_environment: None,
      kept_alive: Vec::new(),
      host: Rc::new(RefCell::new(GenerationArena::new())),
      closure_environments: Rc::new(RefCell::new(GenerationArena::new())),
    }
  }

  pub(crate) fn fork(&self) -> Self {
    Self {
      temporary: Vec::new(),
      empty_closure_environment: self.empty_closure_environment.clone(),
      kept_alive: Vec::new(),
      host: Rc::clone(&self.host),
      closure_environments: Rc::clone(&self.closure_environments),
    }
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
    if cells.is_empty()
      && let Some(environment) = self
        .empty_closure_environment
        .as_ref()
        .and_then(|env| env.upgrade())
    {
      return environment;
    }
    let empty = cells.is_empty();
    let environment = ClosureEnvironment::allocate(Rc::clone(&self.closure_environments), cells);
    crate::gc::register_closure_environment(&environment);
    if empty {
      self.empty_closure_environment = Some(environment.downgrade());
    }
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

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn empty_environment_cache_recovers_after_reclamation_in_a_fork() {
    let mut memory = VmMemory::new();
    let first = memory.allocate_closure_environment(Vec::new());
    let mut fork = memory.fork();
    let shared = fork.allocate_closure_environment(Vec::new());
    assert_eq!(first.identity(), shared.identity());
    first.reclaim();
    let replacement = fork.allocate_closure_environment(Vec::new());
    assert_ne!(replacement.identity(), first.identity());
    assert!(replacement.borrow().is_empty());
    let parent_replacement = memory.allocate_closure_environment(Vec::new());
    assert!(parent_replacement.borrow().is_empty());
  }
}
