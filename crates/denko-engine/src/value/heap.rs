// SPDX-License-Identifier: MPL-2.0
//! Typed, compacting arenas for the VM-owned heap migration.
//!
//! Each JavaScript heap-data kind lives in its own homogeneous arena. Handles
//! are 32-bit typed indexes bound to a no-GC lifetime. Heap-internal references
//! use the explicit `'static` representation and must be rewritten by
//! [`HeapMarkAndSweep::sweep_values`] whenever the arena compacts.

#![allow(dead_code, reason = "introduced ahead of the staged Value migration")]

use super::scope::{GcToken, NoGcScope};
use std::fmt;
use std::marker::PhantomData;
use std::num::NonZeroU32;
use thiserror::Error;

/// Failure resolving or allocating an arena index.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub(crate) enum HeapError {
  #[error("typed heap arena exhausted its 32-bit index")]
  CapacityExhausted,

  #[error("typed heap index {index} is out of bounds for arena length {len}")]
  IndexOutOfBounds { index: u32, len: usize },

  #[error("mark bitmap length {actual} does not match arena length {expected}")]
  InvalidMarkBitmap { expected: usize, actual: usize },
}

/// A nonzero, typed index into a homogeneous heap arena.
///
/// The encoded integer is offset by one so `Option<HeapIndex<T>>` retains the
/// same four-byte representation. The lifetime binds ordinary handles to a
/// no-GC epoch; heap-internal references deliberately use `'static` and are
/// updated during the collector's sweep phase.
#[repr(transparent)]
pub(crate) struct HeapIndex<'gc, T>(
  NonZeroU32,
  PhantomData<fn() -> T>,
  PhantomData<&'gc GcToken>,
);

impl<T> HeapIndex<'_, T> {
  fn from_index(index: usize) -> Result<HeapIndex<'static, T>, HeapError> {
    let index = u32::try_from(index).map_err(|_| HeapError::CapacityExhausted)?;
    let encoded = index
      .checked_add(1)
      .ok_or(HeapError::CapacityExhausted)?;
    Ok(HeapIndex(
      NonZeroU32::new(encoded).ok_or(HeapError::CapacityExhausted)?,
      PhantomData,
      PhantomData,
    ))
  }

  pub(crate) const fn index(self) -> u32 {
    self.0.get() - 1
  }

  /// Bind an unrooted handle to an epoch in which collection cannot occur.
  pub(crate) fn bind<'new_gc>(
    self,
    _gc: NoGcScope<'new_gc, '_>,
  ) -> HeapIndex<'new_gc, T> {
    HeapIndex(self.0, PhantomData, PhantomData)
  }

  /// Remove the epoch lifetime for storage inside the heap or a root set.
  ///
  /// The resulting handle must not be dereferenced until it is rebound with a
  /// current [`NoGcScope`].
  pub(crate) const fn unbind(self) -> HeapIndex<'static, T> {
    HeapIndex(self.0, PhantomData, PhantomData)
  }
}

impl<T> Clone for HeapIndex<'_, T> {
  fn clone(&self) -> Self {
    *self
  }
}

impl<T> Copy for HeapIndex<'_, T> {}

impl<T> PartialEq for HeapIndex<'_, T> {
  fn eq(
    &self,
    other: &Self,
  ) -> bool {
    self.0 == other.0
  }
}

impl<T> Eq for HeapIndex<'_, T> {}

impl<T> std::hash::Hash for HeapIndex<'_, T> {
  fn hash<H: std::hash::Hasher>(
    &self,
    state: &mut H,
  ) {
    self.0.hash(state);
  }
}

impl<T> fmt::Debug for HeapIndex<'_, T> {
  fn fmt(
    &self,
    formatter: &mut fmt::Formatter<'_>,
  ) -> fmt::Result {
    formatter
      .debug_tuple("HeapIndex")
      .field(&self.index())
      .finish()
  }
}

const _: () = assert!(size_of::<HeapIndex<'static, ()>>() == size_of::<u32>());
const _: () = assert!(size_of::<Option<HeapIndex<'static, ()>>>() == size_of::<u32>());

/// Old-to-new index mapping produced by one arena compaction.
#[derive(Debug)]
pub(crate) struct Compaction {
  indexes: Vec<Option<u32>>,
}

impl Compaction {
  pub(crate) fn shift<T>(
    &self,
    handle: &mut HeapIndex<'static, T>,
  ) {
    let old = handle.index() as usize;
    let Some(Some(new)) = self.indexes.get(old) else {
      panic!("strong heap reference points to an unmarked allocation at index {old}");
    };
    *handle = HeapIndex::from_index(*new as usize)
      .unwrap_or_else(|error| panic!("invalid compacted heap index: {error}"));
  }
}

/// Tracing and relocation contract for one homogeneous arena record.
///
/// Missing an outgoing edge or failing to shift a stored handle is a collector
/// correctness bug. Implementations should destructure every field so adding a
/// new reference-bearing field produces a compile error until it is handled.
pub(crate) trait HeapMarkAndSweep: Sized {
  fn mark_values(
    &self,
    visitor: &mut dyn FnMut(HeapIndex<'static, Self>),
  );

  fn sweep_values(
    &mut self,
    compaction: &Compaction,
  );
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct HeapCollectionStats {
  pub(crate) marked: usize,
  pub(crate) reclaimed: usize,
  pub(crate) live: usize,
}

/// Dense, homogeneous storage for one heap-data type.
pub(crate) struct HeapArena<T> {
  values: Vec<T>,
}

impl<T> Default for HeapArena<T> {
  fn default() -> Self {
    Self { values: Vec::new() }
  }
}

impl<T> HeapArena<T> {
  pub(crate) fn new() -> Self {
    Self::default()
  }

  pub(crate) fn len(&self) -> usize {
    self.values.len()
  }

  pub(crate) fn is_empty(&self) -> bool {
    self.values.is_empty()
  }

  /// Allocate in the current no-GC epoch.
  pub(crate) fn allocate<'gc>(
    &mut self,
    value: T,
    gc: NoGcScope<'gc, '_>,
  ) -> Result<HeapIndex<'gc, T>, HeapError> {
    let handle = HeapIndex::from_index(self.values.len())?;
    self.values.push(value);
    Ok(handle.bind(gc))
  }

  pub(crate) fn get<'gc>(
    &self,
    handle: HeapIndex<'gc, T>,
    _gc: NoGcScope<'gc, '_>,
  ) -> Result<&T, HeapError> {
    self
      .values
      .get(handle.index() as usize)
      .ok_or(HeapError::IndexOutOfBounds {
        index: handle.index(),
        len: self.values.len(),
      })
  }

  pub(crate) fn get_mut<'gc>(
    &mut self,
    handle: HeapIndex<'gc, T>,
    _gc: NoGcScope<'gc, '_>,
  ) -> Result<&mut T, HeapError> {
    let len = self.values.len();
    self
      .values
      .get_mut(handle.index() as usize)
      .ok_or(HeapError::IndexOutOfBounds {
        index: handle.index(),
        len,
      })
  }
}

impl<T: HeapMarkAndSweep> HeapArena<T> {
  /// Trace within this arena, compact it, and update every retained edge.
  ///
  /// A complete VM heap collector performs the same operation across all typed
  /// arenas with per-kind work queues and compaction maps.
  pub(crate) fn collect(
    &mut self,
    roots: impl IntoIterator<Item = HeapIndex<'static, T>>,
  ) -> Result<(HeapCollectionStats, Compaction), HeapError> {
    let mut marked = vec![false; self.values.len()];
    let mut worklist: Vec<_> = roots.into_iter().collect();

    while let Some(handle) = worklist.pop() {
      let index = handle.index() as usize;
      let Some(mark) = marked.get_mut(index) else {
        return Err(HeapError::IndexOutOfBounds {
          index: handle.index(),
          len: self.values.len(),
        });
      };
      if *mark {
        continue;
      }
      *mark = true;
      self.values[index].mark_values(&mut |child| worklist.push(child));
    }

    self.compact(marked)
  }

  fn compact(
    &mut self,
    marked: Vec<bool>,
  ) -> Result<(HeapCollectionStats, Compaction), HeapError> {
    if marked.len() != self.values.len() {
      return Err(HeapError::InvalidMarkBitmap {
        expected: self.values.len(),
        actual: marked.len(),
      });
    }

    let old_len = self.values.len();
    let mut next = 0_u32;
    let indexes = marked
      .iter()
      .map(|is_live| {
        is_live.then(|| {
          let current = next;
          next += 1;
          current
        })
      })
      .collect();
    let compaction = Compaction { indexes };

    let mut retained = Vec::with_capacity(next as usize);
    for (mut value, is_live) in self.values.drain(..).zip(marked) {
      if is_live {
        value.sweep_values(&compaction);
        retained.push(value);
      }
    }
    self.values = retained;

    Ok((
      HeapCollectionStats {
        marked: self.values.len(),
        reclaimed: old_len - self.values.len(),
        live: self.values.len(),
      },
      compaction,
    ))
  }
}

#[cfg(test)]
mod tests {
  use super::super::scope::GcContext;
  use super::*;

  #[derive(Debug, Eq, PartialEq)]
  struct Node {
    name: &'static str,
    children: Vec<HeapIndex<'static, Node>>,
  }

  impl Node {
    fn leaf(name: &'static str) -> Self {
      Self {
        name,
        children: Vec::new(),
      }
    }
  }

  impl HeapMarkAndSweep for Node {
    fn mark_values(
      &self,
      visitor: &mut dyn FnMut(HeapIndex<'static, Self>),
    ) {
      let Self { name: _, children } = self;
      children.iter().copied().for_each(visitor);
    }

    fn sweep_values(
      &mut self,
      compaction: &Compaction,
    ) {
      let Self { name: _, children } = self;
      children
        .iter_mut()
        .for_each(|child| compaction.shift(child));
    }
  }

  #[test]
  fn typed_indexes_have_u32_layout_and_option_niche() {
    assert_eq!(size_of::<HeapIndex<'static, Node>>(), size_of::<u32>());
    assert_eq!(
      size_of::<Option<HeapIndex<'static, Node>>>(),
      size_of::<u32>()
    );
  }

  #[test]
  fn allocation_and_access_require_a_no_gc_epoch() {
    let mut context = GcContext::new();
    let gc = context.scope();
    let nogc = gc.nogc();
    let mut arena = HeapArena::new();
    let handle = arena
      .allocate(Node::leaf("first"), nogc)
      .unwrap_or_else(|error| panic!("{error}"));

    assert_eq!(arena.get(handle, nogc).map(|node| node.name), Ok("first"));
    arena
      .get_mut(handle, nogc)
      .unwrap_or_else(|error| panic!("{error}"))
      .name = "updated";
    assert_eq!(arena.get(handle, nogc).map(|node| node.name), Ok("updated"));
  }

  #[test]
  fn collection_compacts_and_rewrites_heap_internal_edges() {
    let mut context = GcContext::new();
    let gc = context.scope();
    let nogc = gc.nogc();
    let mut arena = HeapArena::new();
    let garbage = arena
      .allocate(Node::leaf("garbage"), nogc)
      .unwrap_or_else(|error| panic!("{error}"));
    let child = arena
      .allocate(Node::leaf("child"), nogc)
      .unwrap_or_else(|error| panic!("{error}"));
    let root = arena
      .allocate(
        Node {
          name: "root",
          children: vec![child.unbind()],
        },
        nogc,
      )
      .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(garbage.index(), 0);
    assert_eq!(child.index(), 1);
    assert_eq!(root.index(), 2);

    let (stats, compaction) = arena
      .collect([root.unbind()])
      .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(
      stats,
      HeapCollectionStats {
        marked: 2,
        reclaimed: 1,
        live: 2,
      }
    );

    let mut relocated_root = root.unbind();
    compaction.shift(&mut relocated_root);
    let mut context = GcContext::new();
    let gc = context.scope();
    let nogc = gc.nogc();
    let relocated_root = relocated_root.bind(nogc);
    let root_record = arena
      .get(relocated_root, nogc)
      .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(root_record.name, "root");
    assert_eq!(root_record.children[0].index(), 0);
    assert_eq!(arena.len(), 2);
  }

  #[test]
  #[should_panic(expected = "strong heap reference points to an unmarked allocation")]
  fn retained_strong_edge_to_unmarked_data_is_detected() {
    let mut context = GcContext::new();
    let gc = context.scope();
    let nogc = gc.nogc();
    let mut arena = HeapArena::new();
    let child = arena
      .allocate(Node::leaf("child"), nogc)
      .unwrap_or_else(|error| panic!("{error}"));
    let root = arena
      .allocate(
        Node {
          name: "root",
          children: vec![child.unbind()],
        },
        nogc,
      )
      .unwrap_or_else(|error| panic!("{error}"));

    // Exercise the invariant checker directly with a deliberately incomplete
    // bitmap; normal `collect` marks the child transitively.
    let _ = arena.compact(vec![false, true]);
    let _ = root;
  }
}
