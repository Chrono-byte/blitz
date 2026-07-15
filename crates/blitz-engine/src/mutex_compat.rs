//! Compatibility helpers for slot-backed `Mutex` values.
//!
//! These helpers keep the existing `borrow` / `borrow_mut` call sites working
//! while selected shared value slots migrate off `RefCell`.

use std::sync::{Mutex, MutexGuard};

/// Extension trait that preserves the existing `borrow` / `borrow_mut`
/// vocabulary while the backing slot type moves from `RefCell` to `Mutex`.
pub trait MutexBorrowExt<T> {
  /// Lock the mutex and return its guard, mapping poison to the inner value.
  fn borrow(&self) -> MutexGuard<'_, T>;
  /// Mutable alias for [`MutexBorrowExt::borrow`].
  fn borrow_mut(&self) -> MutexGuard<'_, T>;
}

impl<T> MutexBorrowExt<T> for Mutex<T> {
  fn borrow(&self) -> MutexGuard<'_, T> {
    match self.lock() {
      Ok(guard) => guard,
      Err(poisoned) => poisoned.into_inner(),
    }
  }

  fn borrow_mut(&self) -> MutexGuard<'_, T> {
    match self.lock() {
      Ok(guard) => guard,
      Err(poisoned) => poisoned.into_inner(),
    }
  }
}
