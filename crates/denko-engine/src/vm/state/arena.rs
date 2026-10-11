// SPDX-License-Identifier: MPL-2.0

use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct ArenaHandle {
  pub(crate) arena_id: u64,
  pub(crate) slot: usize,
  pub(crate) generation: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct WeakArenaHandle(ArenaHandle);

struct ArenaSlot<T> {
  generation: u64,
  value: Option<T>,
}

pub(crate) struct GenerationArena<T> {
  id: u64,
  slots: Vec<ArenaSlot<T>>,
  free: Vec<usize>,
  allocations: usize,
  removals: usize,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct ArenaSnapshot {
  pub(crate) allocations: usize,
  pub(crate) live: usize,
  pub(crate) removals: usize,
}

impl<T> GenerationArena<T> {
  pub(crate) fn new() -> Self {
    static NEXT_ARENA_ID: AtomicU64 = AtomicU64::new(1);
    Self {
      id: NEXT_ARENA_ID.fetch_add(1, Ordering::Relaxed),
      slots: Vec::new(),
      free: Vec::new(),
      allocations: 0,
      removals: 0,
    }
  }

  pub(crate) fn insert(
    &mut self,
    value: T,
  ) -> ArenaHandle {
    let slot = if let Some(slot) = self.free.pop() {
      self.slots[slot].value = Some(value);
      slot
    } else {
      let slot = self.slots.len();
      self.slots.push(ArenaSlot {
        generation: 0,
        value: Some(value),
      });
      slot
    };
    self.allocations = self.allocations.saturating_add(1);
    ArenaHandle {
      arena_id: self.id,
      slot,
      generation: self.slots[slot].generation,
    }
  }

  pub(crate) fn get(
    &self,
    handle: ArenaHandle,
  ) -> Option<&T> {
    self
      .live_slot(handle)
      .and_then(|slot| slot.value.as_ref())
  }

  pub(crate) fn get_mut(
    &mut self,
    handle: ArenaHandle,
  ) -> Option<&mut T> {
    self
      .live_slot_mut(handle)
      .and_then(|slot| slot.value.as_mut())
  }

  pub(crate) fn remove(
    &mut self,
    handle: ArenaHandle,
  ) -> Option<T> {
    let slot = self.live_slot_mut(handle)?;
    let value = slot.value.take()?;
    slot.generation = slot.generation.wrapping_add(1);
    self.free.push(handle.slot);
    self.removals = self.removals.saturating_add(1);
    Some(value)
  }

  pub(crate) fn values(&self) -> impl Iterator<Item = &T> {
    self
      .slots
      .iter()
      .filter_map(|slot| slot.value.as_ref())
  }

  pub(crate) fn downgrade(
    &self,
    handle: ArenaHandle,
  ) -> Option<WeakArenaHandle> {
    self.get(handle).map(|_| WeakArenaHandle(handle))
  }

  pub(crate) fn upgrade(
    &self,
    handle: WeakArenaHandle,
  ) -> Option<ArenaHandle> {
    self.get(handle.0).map(|_| handle.0)
  }

  pub(crate) fn snapshot(&self) -> ArenaSnapshot {
    ArenaSnapshot {
      allocations: self.allocations,
      live: self.slots.len().saturating_sub(self.free.len()),
      removals: self.removals,
    }
  }

  fn live_slot(
    &self,
    handle: ArenaHandle,
  ) -> Option<&ArenaSlot<T>> {
    (handle.arena_id == self.id)
      .then(|| self.slots.get(handle.slot))
      .flatten()
      .filter(|slot| slot.generation == handle.generation && slot.value.is_some())
  }

  fn live_slot_mut(
    &mut self,
    handle: ArenaHandle,
  ) -> Option<&mut ArenaSlot<T>> {
    (handle.arena_id == self.id)
      .then(|| self.slots.get_mut(handle.slot))
      .flatten()
      .filter(|slot| slot.generation == handle.generation && slot.value.is_some())
  }
}

#[cfg(test)]
mod tests {
  use super::GenerationArena;

  #[test]
  fn weak_handles_do_not_upgrade_after_removal_or_slot_reuse() {
    let mut arena = GenerationArena::new();
    let first = arena.insert("first");
    let weak = arena
      .downgrade(first)
      .expect("live handle downgrades");

    assert_eq!(arena.upgrade(weak), Some(first));
    assert_eq!(arena.remove(first), Some("first"));
    assert_eq!(arena.upgrade(weak), None);

    let second = arena.insert("second");
    assert_eq!(first.slot, second.slot);
    assert_ne!(first.generation, second.generation);
    assert_eq!(arena.upgrade(weak), None);
    assert_eq!(arena.get(second), Some(&"second"));
    assert_eq!(arena.snapshot().allocations, 2);
    assert_eq!(arena.snapshot().live, 1);
    assert_eq!(arena.snapshot().removals, 1);
  }

  #[test]
  fn handles_are_rejected_by_other_arenas() {
    let mut first = GenerationArena::new();
    let mut second = GenerationArena::new();
    let handle = first.insert(1);
    second.insert(2);

    assert_eq!(second.get(handle), None);
    assert_eq!(second.remove(handle), None);
  }
}
