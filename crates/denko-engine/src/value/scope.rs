// SPDX-License-Identifier: MPL-2.0
//! Compile-time garbage-collection safepoint capabilities.
//!
//! A handle bound through [`NoGcScope`] borrows the active GC token. Calling
//! code that may collect requires a mutable reborrow through [`GcScope`], so
//! Rust rejects use of the bound handle after that safepoint. Values that must
//! survive collection must first be copied into an explicit root.

#![allow(dead_code, reason = "introduced ahead of the staged Value migration")]

use std::marker::PhantomData;
use std::{cell::RefCell, fmt};

use super::tagged::{HeapHandle, TaggedValue};

/// Private token borrowed by every GC-bound handle.
#[derive(Debug)]
pub(super) struct GcToken;

/// Private token distinguishing one native/JavaScript call scope.
#[derive(Debug)]
struct ScopeToken;

/// Owner of the unique garbage-collector and root-scope capabilities.
///
/// The eventual VM heap owns one context. A fresh [`GcScope`] is borrowed when
/// entering JavaScript execution; constructing independent scopes is not
/// exposed, which preserves the single-active-GC-capability invariant.
#[derive(Debug, Default)]
pub(crate) struct GcContext {
  gc: GcToken,
  scope: ScopeToken,
}

impl Default for GcToken {
  fn default() -> Self {
    Self
  }
}

impl Default for ScopeToken {
  fn default() -> Self {
    Self
  }
}

impl GcContext {
  pub(crate) fn new() -> Self {
    Self::default()
  }

  pub(crate) fn scope(&mut self) -> GcScope<'_, '_> {
    GcScope {
      _gc: PhantomData,
      _scope: PhantomData,
    }
  }
}

/// Capability for code that may reach a garbage-collection safepoint.
///
/// Reborrowing this capability requires exclusive access. That conflicts with
/// the shared borrow held by values bound using [`GcScope::nogc`], preventing
/// those values from being reused across a possible collection.
#[derive(Debug)]
pub(crate) struct GcScope<'gc, 'scope> {
  _gc: PhantomData<&'gc mut GcToken>,
  _scope: PhantomData<&'scope mut ScopeToken>,
}

impl<'gc, 'scope> GcScope<'gc, 'scope> {
  pub(crate) fn reborrow(&mut self) -> GcScope<'_, 'scope> {
    GcScope {
      _gc: PhantomData,
      _scope: PhantomData,
    }
  }

  pub(crate) fn nogc(&self) -> NoGcScope<'_, 'scope> {
    NoGcScope {
      _gc: PhantomData,
      _scope: PhantomData,
    }
  }

  pub(crate) fn into_nogc(self) -> NoGcScope<'gc, 'scope> {
    NoGcScope {
      _gc: PhantomData,
      _scope: PhantomData,
    }
  }
}

/// Proof that the current operation cannot trigger collection.
#[derive(Clone, Copy, Debug)]
pub(crate) struct NoGcScope<'gc, 'scope> {
  _gc: PhantomData<&'gc GcToken>,
  _scope: PhantomData<&'scope ScopeToken>,
}

/// Values in either root table are deliberately stored with the GC lifetime
/// erased. This is a storage boundary, not permission to dereference a value:
/// callers must obtain a fresh [`NoGcScope`] after every possible safepoint.
#[derive(Default)]
struct RootState {
  slots: Vec<Option<TaggedValue<'static>>>,
  free: Vec<u32>,
}

#[derive(Default)]
struct GlobalRootSlot {
  value: Option<TaggedValue<'static>>,
  generation: u32,
}

#[derive(Default)]
struct GlobalRootState {
  slots: Vec<GlobalRootSlot>,
  free: Vec<u32>,
}

/// Explicit roots for values that must remain valid across a GC safepoint.
///
/// Root entries use unbound values because the collector must be able to
/// rewrite them while compacting arenas. Reading a root requires a new
/// [`NoGcScope`], which rebinds the value to the post-safepoint epoch.
#[derive(Default)]
pub(crate) struct RootSet {
  state: RefCell<RootState>,
}

impl fmt::Debug for RootSet {
  fn fmt(
    &self,
    formatter: &mut fmt::Formatter<'_>,
  ) -> fmt::Result {
    formatter
      .debug_struct("RootSet")
      .field("len", &self.len())
      .finish()
  }
}

impl RootSet {
  pub(crate) fn new() -> Self {
    Self::default()
  }

  pub(crate) fn root<'gc, 'scope>(
    &'scope self,
    value: TaggedValue<'gc>,
    _gc: NoGcScope<'gc, 'scope>,
  ) -> Result<ScopedValue<'scope>, RootError> {
    let mut state = self.state.borrow_mut();
    let index = if let Some(index) = state.free.pop() {
      state.slots[index as usize] = Some(value.unbind());
      index
    } else {
      let index = u32::try_from(state.slots.len()).map_err(|_| RootError::CapacityExhausted)?;
      state.slots.push(Some(value.unbind()));
      index
    };
    Ok(ScopedValue { roots: self, index })
  }

  pub(crate) fn len(&self) -> usize {
    self
      .state
      .borrow()
      .slots
      .iter()
      .filter(|slot| slot.is_some())
      .count()
  }

  /// Rewrite all rooted heap references after arena compaction.
  ///
  /// The collector must call this while no bound value is being used. Root
  /// entries are unbound again by the next call to [`ScopedValue::get`].
  pub(crate) fn rewrite_heap_handles(
    &self,
    mut rewrite: impl FnMut(HeapHandle<'static>) -> HeapHandle<'static>,
    _gc: GcScope<'_, '_>,
  ) {
    for value in self.state.borrow_mut().slots.iter_mut().flatten() {
      value.rewrite_heap_handle(&mut rewrite);
    }
  }
}

/// A copyable, persistent identity for one slot in a [`GlobalRootSet`].
///
/// Unlike [`ScopedValue`], this handle does not release itself when dropped.
/// Hosts and the VM must call [`GlobalRootSet::release`] explicitly. The
/// generation prevents a handle retained by a host from becoming valid again
/// when its slot is reused.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct GlobalRoot {
  index: u32,
  generation: u32,
}

impl GlobalRoot {
  pub(crate) const fn index(self) -> u32 {
    self.index
  }

  pub(crate) const fn generation(self) -> u32 {
    self.generation
  }

  pub(crate) fn get<'gc>(
    self,
    roots: &GlobalRootSet,
    gc: NoGcScope<'gc, '_>,
  ) -> Result<TaggedValue<'gc>, RootError> {
    roots.get(self, gc)
  }

  pub(crate) fn release(
    self,
    roots: &GlobalRootSet,
  ) -> Result<(), RootError> {
    roots.release(self)
  }
}

const _: () = assert!(size_of::<GlobalRoot>() == 2 * size_of::<u32>());
const _: () = assert!(align_of::<GlobalRoot>() == align_of::<u32>());

/// Persistent roots owned by a VM, realm, module, or host integration.
///
/// A [`GlobalRoot`] is only an identity; it never grants access to the stored
/// value by itself. [`GlobalRootSet::get`] requires a fresh [`NoGcScope`], so a
/// value obtained before a safepoint cannot be reused after it. Values remain
/// explicitly unbound while stored, allowing the collector to rewrite them.
/// Rewriting and release do not themselves run collection. The collector must
/// rewrite this set at its relocation safepoint before execution resumes.
#[derive(Default)]
pub(crate) struct GlobalRootSet {
  state: RefCell<GlobalRootState>,
}

impl fmt::Debug for GlobalRootSet {
  fn fmt(
    &self,
    formatter: &mut fmt::Formatter<'_>,
  ) -> fmt::Result {
    formatter
      .debug_struct("GlobalRootSet")
      .field("len", &self.len())
      .finish()
  }
}

impl GlobalRootSet {
  pub(crate) fn new() -> Self {
    Self::default()
  }

  /// Store an unbound value and return a handle that remains valid until
  /// [`Self::release`] is called. Rooting is a no-GC operation, but accepting a
  /// [`NoGcScope`] makes the storage boundary explicit at every call site.
  pub(crate) fn root<'gc>(
    &self,
    value: TaggedValue<'gc>,
    _gc: NoGcScope<'gc, '_>,
  ) -> Result<GlobalRoot, RootError> {
    let mut state = self.state.borrow_mut();
    if let Some(index) = state.free.pop() {
      let slot = state
        .slots
        .get_mut(index as usize)
        .expect("global root free-list entry must name a slot");
      debug_assert!(slot.value.is_none());
      slot.value = Some(value.unbind());
      return Ok(GlobalRoot {
        index,
        generation: slot.generation,
      });
    }

    let index = u32::try_from(state.slots.len()).map_err(|_| RootError::GlobalCapacityExhausted)?;
    state.slots.push(GlobalRootSlot {
      value: Some(value.unbind()),
      generation: 0,
    });
    Ok(GlobalRoot {
      index,
      generation: 0,
    })
  }

  /// Recover and rebind a persistent value for the current no-GC epoch.
  pub(crate) fn get<'gc>(
    &self,
    root: GlobalRoot,
    gc: NoGcScope<'gc, '_>,
  ) -> Result<TaggedValue<'gc>, RootError> {
    let state = self.state.borrow();
    let slot = state
      .slots
      .get(root.index as usize)
      .filter(|slot| slot.value.is_some() && slot.generation == root.generation)
      .ok_or(RootError::StaleGlobalRoot {
        index: root.index,
        generation: root.generation,
      })?;
    Ok(
      slot
        .value
        .expect("global root slot was checked to contain a value")
        .bind(gc),
    )
  }

  /// Explicitly release a persistent root. Releasing twice, or releasing a
  /// handle after its slot has been reused, is rejected as a stale-handle use.
  pub(crate) fn release(
    &self,
    root: GlobalRoot,
  ) -> Result<(), RootError> {
    let mut state = self.state.borrow_mut();
    let slot = state
      .slots
      .get_mut(root.index as usize)
      .filter(|slot| slot.value.is_some() && slot.generation == root.generation)
      .ok_or(RootError::StaleGlobalRoot {
        index: root.index,
        generation: root.generation,
      })?;

    slot.value = None;
    // A slot at the terminal generation is quarantined rather than wrapped.
    // Wrapping would make an arbitrarily old host handle valid again (the ABA
    // problem), while quarantining preserves stale-handle detection forever.
    if slot.generation != u32::MAX {
      slot.generation += 1;
      state.free.push(root.index);
    }
    Ok(())
  }

  pub(crate) fn len(&self) -> usize {
    self
      .state
      .borrow()
      .slots
      .iter()
      .filter(|slot| slot.value.is_some())
      .count()
  }

  /// Rewrite every stored heap reference after arena compaction.
  pub(crate) fn rewrite_heap_handles(
    &self,
    mut rewrite: impl FnMut(HeapHandle<'static>) -> HeapHandle<'static>,
    _gc: GcScope<'_, '_>,
  ) {
    for slot in self.state.borrow_mut().slots.iter_mut() {
      if let Some(value) = slot.value.as_mut() {
        value.rewrite_heap_handle(&mut rewrite);
      }
    }
  }
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub(crate) enum RootError {
  #[error("scoped root set exhausted its 32-bit index")]
  CapacityExhausted,

  #[error("global root set exhausted its 32-bit index")]
  GlobalCapacityExhausted,

  #[error("scoped root {index} has already been released")]
  Released { index: u32 },

  #[error("global root {index} generation {generation} is stale")]
  StaleGlobalRoot { index: u32, generation: u32 },
}

use thiserror::Error;

/// A value rooted for one native/JavaScript call scope.
///
/// This contains no GC-epoch lifetime. It therefore survives safepoints, but
/// the contained value can only be recovered by binding it to a fresh epoch.
pub(crate) struct ScopedValue<'scope> {
  roots: &'scope RootSet,
  index: u32,
}

impl ScopedValue<'_> {
  pub(crate) fn get<'gc>(
    &self,
    gc: NoGcScope<'gc, '_>,
  ) -> Result<TaggedValue<'gc>, RootError> {
    self
      .roots
      .state
      .borrow()
      .slots
      .get(self.index as usize)
      .and_then(Option::as_ref)
      .copied()
      .map(|value| value.bind(gc))
      .ok_or(RootError::Released { index: self.index })
  }
}

impl Drop for ScopedValue<'_> {
  fn drop(&mut self) {
    let mut state = self.roots.state.borrow_mut();
    if state
      .slots
      .get_mut(self.index as usize)
      .and_then(Option::take)
      .is_some()
    {
      state.free.push(self.index);
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::value::tagged::{HeapKind, TaggedValue};

  #[test]
  fn scopes_can_be_reborrowed_and_downgraded() {
    let mut context = GcContext::new();
    let mut gc = context.scope();
    {
      let child = gc.reborrow();
      let _nogc = child.nogc();
    }
    let _nogc = gc.into_nogc();
  }

  #[test]
  fn scoped_values_survive_safepoints_and_rebind_after_relocation() {
    let roots = RootSet::new();
    let mut context = GcContext::new();
    let mut gc = context.scope();
    let value = TaggedValue::heap(HeapHandle::new(HeapKind::Object, 7)).bind(gc.nogc());
    let rooted = roots
      .root(value, gc.nogc())
      .unwrap_or_else(|error| panic!("{error}"));

    roots.rewrite_heap_handles(|handle| HeapHandle::new(handle.kind(), 3), gc.reborrow());

    let rebound = rooted
      .get(gc.nogc())
      .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(rebound.as_heap().map(HeapHandle::index), Some(3));
    assert_eq!(roots.len(), 1);
    drop(rooted);
    assert_eq!(roots.len(), 0);
  }

  #[test]
  fn global_roots_survive_relocation_and_rebind() {
    let roots = GlobalRootSet::new();
    let mut context = GcContext::new();
    let mut gc = context.scope();
    let value = TaggedValue::heap(HeapHandle::new(HeapKind::Array, 7)).bind(gc.nogc());
    let root = roots
      .root(value, gc.nogc())
      .unwrap_or_else(|error| panic!("{error}"));

    roots.rewrite_heap_handles(|handle| HeapHandle::new(handle.kind(), 3), gc.reborrow());

    let rebound = root
      .get(&roots, gc.nogc())
      .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(rebound.as_heap().map(HeapHandle::index), Some(3));
    assert_eq!(roots.len(), 1);
  }

  #[test]
  fn global_root_release_reuse_rejects_stale_handles() {
    let roots = GlobalRootSet::new();
    let mut context = GcContext::new();
    let gc = context.scope();
    let first = roots
      .root(TaggedValue::UNDEFINED, gc.nogc())
      .unwrap_or_else(|error| panic!("{error}"));

    assert_eq!(roots.release(first), Ok(()));
    assert_eq!(
      roots.release(first),
      Err(RootError::StaleGlobalRoot {
        index: first.index(),
        generation: first.generation(),
      })
    );

    let replacement = roots
      .root(TaggedValue::NULL, gc.nogc())
      .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(replacement.index(), first.index());
    assert_ne!(replacement.generation(), first.generation());
    assert_eq!(
      roots.get(first, gc.nogc()),
      Err(RootError::StaleGlobalRoot {
        index: first.index(),
        generation: first.generation(),
      })
    );
    assert_eq!(roots.get(replacement, gc.nogc()), Ok(TaggedValue::NULL));
    assert_eq!(
      roots.release(first),
      Err(RootError::StaleGlobalRoot {
        index: first.index(),
        generation: first.generation(),
      })
    );
    assert_eq!(roots.len(), 1);
  }

  #[test]
  fn scoped_and_global_roots_have_independent_lifetimes_and_storage() {
    let scoped_roots = RootSet::new();
    let global_roots = GlobalRootSet::new();
    let mut context = GcContext::new();
    let gc = context.scope();
    let scoped = scoped_roots
      .root(TaggedValue::boolean(true), gc.nogc())
      .unwrap_or_else(|error| panic!("{error}"));
    let global = global_roots
      .root(TaggedValue::boolean(false), gc.nogc())
      .unwrap_or_else(|error| panic!("{error}"));

    assert_eq!(scoped.get(gc.nogc()), Ok(TaggedValue::boolean(true)));
    assert_eq!(
      global_roots.get(global, gc.nogc()),
      Ok(TaggedValue::boolean(false))
    );
    assert_eq!(scoped_roots.len(), 1);
    assert_eq!(global_roots.len(), 1);

    drop(scoped);
    assert_eq!(scoped_roots.len(), 0);
    assert_eq!(
      global_roots.get(global, gc.nogc()),
      Ok(TaggedValue::boolean(false))
    );
    global_roots
      .release(global)
      .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(global_roots.len(), 0);
  }
}
