// SPDX-License-Identifier: MPL-2.0

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};

#[repr(C, align(8))]
#[derive(Debug)]
struct InterruptSignal {
  requested_generation: AtomicU64,
}

/// Failure to publish an asynchronous VM interrupt request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InterruptRequestError {
  /// The VM that created the handle has been dropped.
  VmDropped,
  /// The monotonic request generation has reached `u64::MAX`.
  CounterExhausted,
}

impl fmt::Display for InterruptRequestError {
  fn fmt(
    &self,
    formatter: &mut fmt::Formatter<'_>,
  ) -> fmt::Result {
    match self {
      Self::VmDropped => formatter.write_str("the VM has been dropped"),
      Self::CounterExhausted => formatter.write_str("the VM interrupt counter is exhausted"),
    }
  }
}

impl std::error::Error for InterruptRequestError {}

/// A thread-safe producer for requesting termination of VM execution.
///
/// The request is host control, not a JavaScript exception. Requests may
/// coalesce before the VM reaches a safepoint.
#[derive(Clone, Debug)]
pub struct VmInterruptHandle {
  signal: Weak<InterruptSignal>,
}

impl VmInterruptHandle {
  /// Request that the VM terminate at its next execution safepoint.
  pub fn request_interrupt(&self) -> Result<(), InterruptRequestError> {
    let signal = self
      .signal
      .upgrade()
      .ok_or(InterruptRequestError::VmDropped)?;
    signal
      .requested_generation
      .fetch_update(Ordering::Release, Ordering::Relaxed, |generation| {
        generation.checked_add(1)
      })
      .map(|_| ())
      .map_err(|_| InterruptRequestError::CounterExhausted)
  }
}

pub(super) struct VmInterruptState {
  signal: Arc<InterruptSignal>,
  acknowledged_generation: u64,
}

#[derive(Clone, Copy)]
pub(crate) struct NativeInterruptPoll {
  pub(crate) requested_generation: *const AtomicU64,
  pub(crate) acknowledged_generation: u64,
}

impl VmInterruptState {
  pub(super) fn new() -> Self {
    Self {
      signal: Arc::new(InterruptSignal {
        requested_generation: AtomicU64::new(0),
      }),
      acknowledged_generation: 0,
    }
  }

  pub(super) fn handle(&self) -> VmInterruptHandle {
    VmInterruptHandle {
      signal: Arc::downgrade(&self.signal),
    }
  }

  pub(super) fn fork(&self) -> Self {
    Self {
      signal: Arc::clone(&self.signal),
      acknowledged_generation: self.acknowledged_generation,
    }
  }

  pub(super) fn poll_and_acknowledge(&mut self) -> bool {
    let requested = self
      .signal
      .requested_generation
      .load(Ordering::Acquire);
    if requested == self.acknowledged_generation {
      return false;
    }
    self.acknowledged_generation = requested;
    true
  }

  pub(crate) fn native_poll(&self) -> NativeInterruptPoll {
    NativeInterruptPoll {
      requested_generation: &self.signal.requested_generation,
      acknowledged_generation: self.acknowledged_generation,
    }
  }

  pub(crate) fn acknowledge(
    &mut self,
    observed_generation: u64,
  ) {
    self.acknowledged_generation = observed_generation;
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn assert_send_sync<T: Send + Sync>() {}

  #[test]
  fn interrupt_handle_is_send_and_sync() {
    assert_send_sync::<VmInterruptHandle>();
  }

  #[test]
  fn requests_are_acknowledged_once() {
    let mut state = VmInterruptState::new();
    let handle = state.handle();

    assert!(!state.poll_and_acknowledge());
    handle.request_interrupt().expect("live VM");
    assert!(state.poll_and_acknowledge());
    assert!(!state.poll_and_acknowledge());
    handle.request_interrupt().expect("live VM");
    assert!(state.poll_and_acknowledge());
  }

  #[test]
  fn stale_handles_report_a_dropped_vm() {
    let handle = VmInterruptState::new().handle();
    assert_eq!(
      handle.request_interrupt(),
      Err(InterruptRequestError::VmDropped)
    );
  }

  #[test]
  fn acknowledging_an_observed_request_preserves_a_later_request() {
    let mut state = VmInterruptState::new();
    let handle = state.handle();
    handle.request_interrupt().expect("first request");
    let observed = state
      .signal
      .requested_generation
      .load(Ordering::Acquire);
    handle
      .request_interrupt()
      .expect("racing request");

    state.acknowledge(observed);
    assert!(state.poll_and_acknowledge());
    assert!(!state.poll_and_acknowledge());
  }
}
