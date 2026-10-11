// SPDX-License-Identifier: MPL-2.0

use crate::binding::BindingCell;
use crate::vm::state::arena::{ArenaHandle, GenerationArena, WeakArenaHandle};
#[cfg(test)]
use std::cell::RefMut;
use std::cell::{Ref, RefCell};
use std::rc::{Rc, Weak};

/// Shared captured bindings for one JavaScript closure.
///
/// Callers operate on the environment rather than its storage representation,
/// keeping identity, weak references, and mutation behind one migration seam.
#[derive(Clone)]
pub struct ClosureEnvironment {
  shared: Rc<ClosureEnvironmentData>,
}

struct ClosureEnvironmentData {
  arena: Rc<RefCell<GenerationArena<Vec<BindingCell>>>>,
  handle: ArenaHandle,
}

impl ClosureEnvironment {
  #[cfg(test)]
  pub(crate) fn new(cells: Vec<BindingCell>) -> Self {
    Self::allocate(Rc::new(RefCell::new(GenerationArena::new())), cells)
  }

  pub(crate) fn allocate(
    arena: Rc<RefCell<GenerationArena<Vec<BindingCell>>>>,
    cells: Vec<BindingCell>,
  ) -> Self {
    let handle = arena.borrow_mut().insert(cells);
    Self {
      shared: Rc::new(ClosureEnvironmentData { arena, handle }),
    }
  }

  pub(crate) fn borrow(&self) -> Ref<'_, Vec<BindingCell>> {
    Ref::map(self.shared.arena.borrow(), |arena| {
      arena
        .get(self.shared.handle)
        .expect("live closure environment handle")
    })
  }

  #[cfg(test)]
  pub(crate) fn borrow_mut(&self) -> RefMut<'_, Vec<BindingCell>> {
    RefMut::map(self.shared.arena.borrow_mut(), |arena| {
      arena
        .get_mut(self.shared.handle)
        .expect("live closure environment handle")
    })
  }

  pub(crate) fn identity(&self) -> ArenaHandle {
    self.shared.handle
  }

  pub(crate) fn downgrade(&self) -> WeakClosureEnvironment {
    let handle = self
      .shared
      .arena
      .borrow()
      .downgrade(self.shared.handle)
      .expect("live closure environment handle");
    WeakClosureEnvironment {
      arena: Rc::downgrade(&self.shared.arena),
      handle,
      shared: Rc::downgrade(&self.shared),
    }
  }

  pub(crate) fn reclaim(&self) {
    self
      .shared
      .arena
      .borrow_mut()
      .remove(self.shared.handle);
  }
}

#[derive(Clone)]
pub(crate) struct WeakClosureEnvironment {
  arena: Weak<RefCell<GenerationArena<Vec<BindingCell>>>>,
  handle: WeakArenaHandle,
  shared: Weak<ClosureEnvironmentData>,
}

impl WeakClosureEnvironment {
  pub(crate) fn upgrade(&self) -> Option<ClosureEnvironment> {
    let arena = self.arena.upgrade()?;
    let handle = arena.borrow().upgrade(self.handle)?;
    let shared = self
      .shared
      .upgrade()
      .unwrap_or_else(|| Rc::new(ClosureEnvironmentData { arena, handle }));
    Some(ClosureEnvironment { shared })
  }

  pub(crate) fn strong_count(&self) -> usize {
    usize::from(
      self
        .arena
        .upgrade()
        .is_some_and(|arena| arena.borrow().upgrade(self.handle).is_some()),
    )
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn weak_handle_recovers_arena_owned_cells_after_shared_handle_is_dropped() {
    let arena = Rc::new(RefCell::new(GenerationArena::new()));
    let environment = ClosureEnvironment::allocate(
      Rc::clone(&arena),
      vec![BindingCell::new(crate::value::Value::Number(42.0))],
    );
    let identity = environment.identity();
    let weak = environment.downgrade();
    drop(environment);

    assert_eq!(weak.strong_count(), 1);
    let restored = weak
      .upgrade()
      .expect("arena still owns the binding cells");
    assert_eq!(restored.identity(), identity);
    assert_eq!(
      restored.borrow()[0].value(),
      crate::value::Value::Number(42.0)
    );
    restored.reclaim();
    assert_eq!(weak.strong_count(), 0);
    assert!(weak.upgrade().is_none());
  }

  #[test]
  fn reclaimed_environment_weak_handle_cannot_observe_reused_slot() {
    let arena = Rc::new(RefCell::new(GenerationArena::new()));
    let first = ClosureEnvironment::allocate(Rc::clone(&arena), Vec::new());
    let first_identity = first.identity();
    let weak = first.downgrade();

    first.reclaim();
    assert!(weak.upgrade().is_none());

    let second = ClosureEnvironment::allocate(arena, Vec::new());
    assert_eq!(first_identity.slot, second.identity().slot);
    assert_ne!(first_identity.generation, second.identity().generation);
    assert!(weak.upgrade().is_none());
  }
}
