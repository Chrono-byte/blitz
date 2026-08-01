// SPDX-License-Identifier: MPL-2.0
//! Shared rebinding for values that borrow a GC epoch.
//!
//! [`Bindable`] is intentionally an owned, type-directed boundary between the
//! two representations used by the prototype memory model:
//!
//! - bound values borrow a [`NoGcScope`] and may be dereferenced by heap APIs;
//! - unbound values use `'static` only as a storage marker and must be rebound
//!   before they are accessed again.
//!
//! The collection implementations only cover owning containers. Rebinding an
//! element in an [`Option`], [`Vec`], or boxed slice does not retain a borrow of
//! the source container, so the resulting container has exactly the requested
//! GC lifetime. Borrowed slices and other borrowed containers deliberately do
//! not implement this trait.
//!
//! # Storage boundaries
//!
//! `bind` is for the duration of a no-GC operation. A bound value, handle, or
//! collection must not be put into a heap record, persistent VM frame, root
//! set, or other storage that can outlive that operation. Call `unbind` at an
//! explicit call/return, heap-storage, or root-storage boundary. The resulting
//! `'static` value is only a compact relocatable representation; it is not
//! evidence that the value can be safely dereferenced without a fresh
//! [`NoGcScope`].
//!
//! In particular, unbinding does not make a value immune to relocation. Roots
//! and heap records own unbound values so the collector can rewrite them, and
//! callers must bind the rewritten value after a safepoint. Because the bound
//! lifetime is carried recursively through owning collections, Rust also
//! rejects using a bound collection after `GcScope::reborrow`.

#![allow(dead_code, reason = "introduced ahead of the staged Value migration")]

use super::heap::HeapIndex;
use super::scope::NoGcScope;
use super::tagged::TaggedValue;

/// Rebind an epoch-borrowing value, or an owning collection of such values.
///
/// Implementations preserve the compact representation while changing only
/// the borrow marker. `Unbound` is the representation permitted at explicit
/// storage boundaries; it is normally the same value with a `'static` marker.
/// The generic associated type makes nested containers retain the new epoch
/// without erasing it to a weaker lifetime.
pub(crate) trait Bindable {
  /// This value bound to the supplied no-GC epoch.
  type Bound<'gc>;

  /// The representation allowed in persistent storage.
  type Unbound;

  /// Bind this value to the epoch represented by `gc`.
  fn bind<'gc, 'scope>(
    self,
    gc: NoGcScope<'gc, 'scope>,
  ) -> Self::Bound<'gc>;

  /// Remove the epoch marker for an explicit storage boundary.
  fn unbind(self) -> Self::Unbound;
}

impl<'old_gc> Bindable for TaggedValue<'old_gc> {
  type Bound<'new_gc> = TaggedValue<'new_gc>;
  type Unbound = TaggedValue<'static>;

  fn bind<'new_gc, 'scope>(
    self,
    gc: NoGcScope<'new_gc, 'scope>,
  ) -> Self::Bound<'new_gc> {
    TaggedValue::bind(self, gc)
  }

  fn unbind(self) -> Self::Unbound {
    TaggedValue::unbind(self)
  }
}

impl<'old_gc, T> Bindable for HeapIndex<'old_gc, T> {
  type Bound<'new_gc> = HeapIndex<'new_gc, T>;
  type Unbound = HeapIndex<'static, T>;

  fn bind<'new_gc, 'scope>(
    self,
    gc: NoGcScope<'new_gc, 'scope>,
  ) -> Self::Bound<'new_gc> {
    HeapIndex::bind(self, gc)
  }

  fn unbind(self) -> Self::Unbound {
    HeapIndex::unbind(self)
  }
}

impl<T: Bindable> Bindable for Option<T> {
  type Bound<'gc> = Option<T::Bound<'gc>>;
  type Unbound = Option<T::Unbound>;

  fn bind<'gc, 'scope>(
    self,
    gc: NoGcScope<'gc, 'scope>,
  ) -> Self::Bound<'gc> {
    self.map(|value| Bindable::bind(value, gc))
  }

  fn unbind(self) -> Self::Unbound {
    self.map(Bindable::unbind)
  }
}

impl<T: Bindable> Bindable for Vec<T> {
  type Bound<'gc> = Vec<T::Bound<'gc>>;
  type Unbound = Vec<T::Unbound>;

  fn bind<'gc, 'scope>(
    self,
    gc: NoGcScope<'gc, 'scope>,
  ) -> Self::Bound<'gc> {
    self
      .into_iter()
      .map(|value| Bindable::bind(value, gc))
      .collect()
  }

  fn unbind(self) -> Self::Unbound {
    self.into_iter().map(Bindable::unbind).collect()
  }
}

impl<T: Bindable> Bindable for Box<[T]> {
  type Bound<'gc> = Box<[T::Bound<'gc>]>;
  type Unbound = Box<[T::Unbound]>;

  fn bind<'gc, 'scope>(
    self,
    gc: NoGcScope<'gc, 'scope>,
  ) -> Self::Bound<'gc> {
    self
      .into_vec()
      .into_iter()
      .map(|value| Bindable::bind(value, gc))
      .collect::<Vec<_>>()
      .into_boxed_slice()
  }

  fn unbind(self) -> Self::Unbound {
    self
      .into_vec()
      .into_iter()
      .map(Bindable::unbind)
      .collect::<Vec<_>>()
      .into_boxed_slice()
  }
}

#[cfg(test)]
mod tests {
  use super::super::heap::HeapArena;
  use super::super::scope::GcContext;
  use super::super::tagged::{HeapHandle, HeapKind};
  use super::*;

  fn assert_tagged_epoch<'gc>(_: &[Option<Box<[TaggedValue<'gc>]>>]) {}

  #[test]
  fn nested_owned_collections_round_trip_tagged_values() {
    let stored: Vec<Option<Box<[TaggedValue<'static>]>>> = vec![
      Some(
        vec![
          TaggedValue::UNDEFINED,
          TaggedValue::heap(HeapHandle::new(HeapKind::Array, 17)),
        ]
        .into_boxed_slice(),
      ),
      None,
    ];
    let expected = stored.clone();

    let mut context = GcContext::new();
    let gc = context.scope();
    let bound = Bindable::bind(stored, gc.nogc());
    assert_tagged_epoch(&bound);
    assert_eq!(bound[0].as_deref().map(|values| values.len()), Some(2));

    let unbound: Vec<Option<Box<[TaggedValue<'static>]>>> = Bindable::unbind(bound);
    assert_eq!(unbound, expected);
  }

  #[test]
  fn nested_owned_collections_round_trip_typed_indexes() {
    let mut context = GcContext::new();
    let gc = context.scope();
    let nogc = gc.nogc();
    let mut arena = HeapArena::new();
    let first = arena
      .allocate((), nogc)
      .unwrap_or_else(|error| panic!("{error}"));
    let second = arena
      .allocate((), nogc)
      .unwrap_or_else(|error| panic!("{error}"));
    let stored = vec![
      Some(vec![first.unbind(), second.unbind()].into_boxed_slice()),
      None,
    ];
    let expected = stored.clone();

    let bound = Bindable::bind(stored, nogc);
    assert_eq!(
      bound[0]
        .as_deref()
        .map(|values| values[1].index()),
      Some(1)
    );
    let unbound = Bindable::unbind(bound);
    assert_eq!(unbound, expected);
  }

  #[test]
  fn binding_again_after_a_safepoint_uses_the_new_epoch() {
    let stored = vec![Some(vec![TaggedValue::boolean(true)].into_boxed_slice())].into_boxed_slice();
    let mut context = GcContext::new();
    let mut gc = context.scope();

    let bound = Bindable::bind(stored, gc.nogc());
    let stored = Bindable::unbind(bound);
    {
      let _safepoint = gc.reborrow();
    }
    let rebound = Bindable::bind(stored, gc.nogc());
    assert_eq!(
      rebound[0].as_deref(),
      Some(&[TaggedValue::boolean(true)][..])
    );
  }
}
