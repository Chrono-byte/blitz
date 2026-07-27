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

#[derive(Default)]
struct RootState {
  slots: Vec<Option<TaggedValue<'static>>>,
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

  pub(crate) fn root<'scope>(
    &'scope self,
    value: TaggedValue<'_>,
    _gc: NoGcScope<'_, 'scope>,
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
  pub(crate) fn rewrite_heap_handles(
    &self,
    mut rewrite: impl FnMut(HeapHandle<'static>) -> HeapHandle<'static>,
  ) {
    for value in self.state.borrow_mut().slots.iter_mut().flatten() {
      value.rewrite_heap_handle(&mut rewrite);
    }
  }
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub(crate) enum RootError {
  #[error("scoped root set exhausted its 32-bit index")]
  CapacityExhausted,

  #[error("scoped root {index} has already been released")]
  Released { index: u32 },
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

    {
      let _safepoint = gc.reborrow();
      roots.rewrite_heap_handles(|handle| HeapHandle::new(handle.kind(), 3));
    }

    let rebound = rooted
      .get(gc.nogc())
      .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(rebound.as_heap().map(HeapHandle::index), Some(3));
    assert_eq!(roots.len(), 1);
    drop(rooted);
    assert_eq!(roots.len(), 0);
  }
}
