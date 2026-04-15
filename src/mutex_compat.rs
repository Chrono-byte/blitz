//! Compatibility helpers for slot-backed `Mutex` values.
//!
//! These helpers keep the existing `borrow` / `borrow_mut` call sites working
//! while selected shared value slots migrate off `RefCell`.

use std::sync::{Mutex, MutexGuard, TryLockError};

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
    self.try_lock().unwrap_or_else(|err| match err {
      TryLockError::WouldBlock => panic!("Mutex already borrowed"),
      TryLockError::Poisoned(poisoned) => poisoned.into_inner(),
    })
  }

  fn borrow_mut(&self) -> MutexGuard<'_, T> {
    self.try_lock().unwrap_or_else(|err| match err {
      TryLockError::WouldBlock => panic!("Mutex already borrowed"),
      TryLockError::Poisoned(poisoned) => poisoned.into_inner(),
    })
  }
}
