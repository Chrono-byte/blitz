// SPDX-License-Identifier: MPL-2.0
//! VM-thread event delivery and resource liveness primitives.
//!
//! Host tasks may produce owned events, but they must not retain or mutate VM
//! values. [`EventRegistry`] provides the bounded queue used to hand those
//! events back to the VM thread. [`ResourceRegistry`] separately tracks which
//! host resources keep an application alive; closing a resource is explicit
//! and idempotent.

use std::cell::RefCell;
use std::fmt;
use std::rc::Rc;

use tokio::sync::mpsc;

/// A generational identifier for one asynchronous host operation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct OperationId {
  index: usize,
  generation: u64,
}

impl OperationId {
  /// Return the slot index of this operation identifier.
  #[must_use]
  pub const fn index(self) -> usize {
    self.index
  }

  /// Return the generation of this operation identifier.
  #[must_use]
  pub const fn generation(self) -> u64 {
    self.generation
  }
}

/// Lifecycle state for one asynchronous host operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OperationState {
  /// Registered on the VM thread but not yet started.
  Registered,
  /// Work has started outside the VM thread.
  Running,
  /// Interruption won the request race and cancellation is being acknowledged.
  CancelRequested,
}

struct OperationEntry<T> {
  kind: String,
  state: OperationState,
  payload: T,
}

struct OperationSlot<T> {
  generation: u64,
  entry: Option<OperationEntry<T>>,
}

/// VM-thread registry for asynchronous host-operation races.
///
/// Payloads stay on the VM thread. Background work receives only the returned
/// [`OperationId`] and owned inputs, then sends an owned completion event back
/// to the runtime. Exactly one of completion or cancellation may take the
/// payload; repeated or stale terminal events are ignored.
pub struct OperationRegistry<T> {
  slots: Vec<OperationSlot<T>>,
  free: Vec<usize>,
  active_count: usize,
}

impl<T> Default for OperationRegistry<T> {
  fn default() -> Self {
    Self {
      slots: Vec::new(),
      free: Vec::new(),
      active_count: 0,
    }
  }
}

impl<T> OperationRegistry<T> {
  /// Create an empty operation registry.
  #[must_use]
  pub fn new() -> Self {
    Self::default()
  }

  /// Register one operation and retain its VM-thread payload.
  pub fn register(
    &mut self,
    kind: impl Into<String>,
    payload: T,
  ) -> OperationId {
    let index = self.free.pop().unwrap_or_else(|| {
      self.slots.push(OperationSlot {
        generation: 0,
        entry: None,
      });
      self.slots.len() - 1
    });
    let generation = self.slots[index].generation;
    self.slots[index].entry = Some(OperationEntry {
      kind: kind.into(),
      state: OperationState::Registered,
      payload,
    });
    self.active_count += 1;
    OperationId { index, generation }
  }

  /// Mark a registered operation as running.
  ///
  /// Returns `false` for stale identifiers and when cancellation was already
  /// requested.
  pub fn start(
    &mut self,
    id: OperationId,
  ) -> bool {
    let Some(entry) = self.entry_mut(id) else {
      return false;
    };
    if entry.state != OperationState::Registered {
      return false;
    }
    entry.state = OperationState::Running;
    true
  }

  /// Request cancellation for a registered or running operation.
  ///
  /// The payload remains registered until the cancellation acknowledgement or
  /// a racing completion claims it.
  pub fn request_cancel(
    &mut self,
    id: OperationId,
  ) -> bool {
    let Some(entry) = self.entry_mut(id) else {
      return false;
    };
    match entry.state {
      OperationState::Registered | OperationState::Running => {
        entry.state = OperationState::CancelRequested;
        true
      }
      OperationState::CancelRequested => false,
    }
  }

  /// Claim a completion payload exactly once.
  ///
  /// Completion may win while cancellation is pending. The first terminal
  /// claimant closes the slot and advances its generation.
  pub fn complete(
    &mut self,
    id: OperationId,
  ) -> Option<T> {
    self.take(id)
  }

  /// Acknowledge cancellation and claim the payload exactly once.
  pub fn cancelled(
    &mut self,
    id: OperationId,
  ) -> Option<T> {
    if self.state(id) != Some(OperationState::CancelRequested) {
      return None;
    }
    self.take(id)
  }

  /// Cancel and claim every active operation.
  pub fn cancel_all(&mut self) -> Vec<T> {
    let ids = self
      .slots
      .iter()
      .enumerate()
      .filter_map(|(index, slot)| {
        slot.entry.as_ref().map(|_| OperationId {
          index,
          generation: slot.generation,
        })
      })
      .collect::<Vec<_>>();
    ids
      .into_iter()
      .filter_map(|id| {
        let _requested = self.request_cancel(id);
        self.cancelled(id)
      })
      .collect()
  }

  /// Return the current state of an active operation.
  #[must_use]
  pub fn state(
    &self,
    id: OperationId,
  ) -> Option<OperationState> {
    self.entry(id).map(|entry| entry.state)
  }

  /// Return the operation's diagnostic kind.
  #[must_use]
  pub fn kind(
    &self,
    id: OperationId,
  ) -> Option<&str> {
    self.entry(id).map(|entry| entry.kind.as_str())
  }

  /// Return the number of active operations.
  #[must_use]
  pub const fn active_count(&self) -> usize {
    self.active_count
  }

  fn entry(
    &self,
    id: OperationId,
  ) -> Option<&OperationEntry<T>> {
    let slot = self.slots.get(id.index)?;
    (slot.generation == id.generation)
      .then_some(slot.entry.as_ref())
      .flatten()
  }

  fn entry_mut(
    &mut self,
    id: OperationId,
  ) -> Option<&mut OperationEntry<T>> {
    let slot = self.slots.get_mut(id.index)?;
    if slot.generation != id.generation {
      return None;
    }
    slot.entry.as_mut()
  }

  fn take(
    &mut self,
    id: OperationId,
  ) -> Option<T> {
    let slot = self.slots.get_mut(id.index)?;
    if slot.generation != id.generation {
      return None;
    }
    let entry = slot.entry.take()?;
    slot.generation = slot.generation.wrapping_add(1);
    self.free.push(id.index);
    self.active_count -= 1;
    Some(entry.payload)
  }
}

/// A generational identifier for a VM-thread-owned host resource.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ResourceId {
  index: usize,
  generation: u64,
}

impl ResourceId {
  /// Return the slot index of this resource identifier.
  #[must_use]
  pub const fn index(self) -> usize {
    self.index
  }

  /// Return the generation of this resource identifier.
  #[must_use]
  pub const fn generation(self) -> u64 {
    self.generation
  }
}

struct ResourceEntry {
  name: String,
  referenced: bool,
}

struct ResourceSlot {
  generation: u64,
  entry: Option<ResourceEntry>,
}

#[derive(Default)]
struct ResourceState {
  slots: Vec<ResourceSlot>,
  free: Vec<usize>,
  open_count: usize,
  referenced_count: usize,
}

/// VM-thread resource registry shared by host adapters.
///
/// The registry is deliberately not `Send` or `Sync`: resource state belongs
/// to the VM thread. Background tasks should send lifecycle events rather than
/// mutating this registry directly.
#[derive(Clone, Default)]
pub struct ResourceRegistry(Rc<RefCell<ResourceState>>);

impl ResourceRegistry {
  /// Create an empty resource registry.
  #[must_use]
  pub fn new() -> Self {
    Self::default()
  }

  /// Register a resource and return its lifecycle handle.
  pub fn register(
    &self,
    name: impl Into<String>,
    referenced: bool,
  ) -> ResourceHandle {
    let mut state = self.0.borrow_mut();
    let index = state.free.pop().unwrap_or_else(|| {
      state.slots.push(ResourceSlot {
        generation: 0,
        entry: None,
      });
      state.slots.len() - 1
    });
    let generation = state.slots[index].generation;
    state.slots[index].entry = Some(ResourceEntry {
      name: name.into(),
      referenced,
    });
    state.open_count += 1;
    if referenced {
      state.referenced_count += 1;
    }
    let id = ResourceId { index, generation };
    ResourceHandle {
      registry: self.clone(),
      id,
    }
  }

  /// Return the number of currently open resources.
  #[must_use]
  pub fn open_count(&self) -> usize {
    self.0.borrow().open_count
  }

  /// Return the number of open referenced resources.
  #[must_use]
  pub fn referenced_count(&self) -> usize {
    self.0.borrow().referenced_count
  }

  /// Whether at least one referenced resource keeps the runtime alive.
  #[must_use]
  pub fn is_live(&self) -> bool {
    self.referenced_count() != 0
  }

  fn with_entry<R>(
    &self,
    id: ResourceId,
    f: impl FnOnce(&ResourceEntry) -> R,
  ) -> Option<R> {
    let state = self.0.borrow();
    let slot = state.slots.get(id.index)?;
    if slot.generation != id.generation {
      return None;
    }
    slot.entry.as_ref().map(f)
  }

  fn close(
    &self,
    id: ResourceId,
  ) -> bool {
    let mut state = self.0.borrow_mut();
    let (entry, next_generation) = {
      let Some(slot) = state.slots.get_mut(id.index) else {
        return false;
      };
      if slot.generation != id.generation {
        return false;
      }
      let Some(entry) = slot.entry.take() else {
        return false;
      };
      (entry, slot.generation.wrapping_add(1))
    };
    state.open_count -= 1;
    if entry.referenced {
      state.referenced_count -= 1;
    }
    state.slots[id.index].generation = next_generation;
    state.free.push(id.index);
    true
  }

  fn set_referenced(
    &self,
    id: ResourceId,
    referenced: bool,
  ) -> bool {
    let mut state = self.0.borrow_mut();
    let Some(slot) = state.slots.get_mut(id.index) else {
      return false;
    };
    if slot.generation != id.generation {
      return false;
    }
    let Some(entry) = slot.entry.as_mut() else {
      return false;
    };
    if entry.referenced == referenced {
      return true;
    }
    entry.referenced = referenced;
    if referenced {
      state.referenced_count += 1;
    } else {
      state.referenced_count -= 1;
    }
    true
  }
}

/// Handle for changing one registered resource's lifecycle state.
#[derive(Clone)]
pub struct ResourceHandle {
  registry: ResourceRegistry,
  id: ResourceId,
}

impl ResourceHandle {
  /// Return the generational resource identifier.
  #[must_use]
  pub const fn id(&self) -> ResourceId {
    self.id
  }

  /// Return the resource's diagnostic name, if it is still open.
  #[must_use]
  pub fn name(&self) -> Option<String> {
    self
      .registry
      .with_entry(self.id, |entry| entry.name.clone())
  }

  /// Return whether the resource is still open.
  #[must_use]
  pub fn is_open(&self) -> bool {
    self
      .registry
      .with_entry(self.id, |_| ())
      .is_some()
  }

  /// Return whether this resource currently keeps the runtime alive.
  #[must_use]
  pub fn is_referenced(&self) -> bool {
    self
      .registry
      .with_entry(self.id, |entry| entry.referenced)
      .unwrap_or(false)
  }

  /// Mark this resource as keeping the runtime alive.
  pub fn reference(&self) {
    let _changed = self.registry.set_referenced(self.id, true);
  }

  /// Allow the runtime to exit when this is the only remaining resource.
  pub fn unreference(&self) {
    let _changed = self.registry.set_referenced(self.id, false);
  }

  /// Close this resource. Repeated calls and stale handles are no-ops.
  pub fn close(&self) -> bool {
    self.registry.close(self.id)
  }
}

/// Error returned when an event queue cannot be created.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EventRegistryError;

impl fmt::Display for EventRegistryError {
  fn fmt(
    &self,
    formatter: &mut fmt::Formatter<'_>,
  ) -> fmt::Result {
    formatter.write_str("event registry capacity must be greater than zero")
  }
}

impl std::error::Error for EventRegistryError {}

/// A clonable, bounded producer for a runtime event queue.
pub struct EventSender<E>(mpsc::Sender<E>);

impl<E> Clone for EventSender<E> {
  fn clone(&self) -> Self {
    Self(self.0.clone())
  }
}

impl<E> EventSender<E> {
  /// Send an event, waiting for queue capacity when necessary.
  ///
  /// # Errors
  ///
  /// Returns the event when the registry receiver has been dropped.
  pub async fn send(
    &self,
    event: E,
  ) -> Result<(), mpsc::error::SendError<E>> {
    self.0.send(event).await
  }

  /// Try to send an event without waiting for queue capacity.
  ///
  /// # Errors
  ///
  /// Returns the event when the receiver is closed or the queue is full.
  pub fn try_send(
    &self,
    event: E,
  ) -> Result<(), mpsc::error::TrySendError<E>> {
    self.0.try_send(event)
  }

  /// Return the number of events currently waiting in the queue.
  #[must_use]
  pub fn queued(&self) -> usize {
    self.0.max_capacity() - self.0.capacity()
  }
}

/// Bounded runtime event queue with VM-thread resource liveness tracking.
pub struct EventRegistry<E> {
  sender: EventSender<E>,
  receiver: Option<mpsc::Receiver<E>>,
  resources: ResourceRegistry,
}

impl<E> EventRegistry<E> {
  /// Create a bounded event registry.
  ///
  /// A zero-capacity queue is rejected because host event delivery must have
  /// explicit bounded buffering rather than an accidental rendezvous queue.
  ///
  /// # Errors
  ///
  /// Returns [`EventRegistryError`] when `capacity` is zero.
  pub fn new(capacity: usize) -> Result<Self, EventRegistryError> {
    if capacity == 0 {
      return Err(EventRegistryError);
    }
    let (sender, receiver) = mpsc::channel(capacity);
    Ok(Self {
      sender: EventSender(sender),
      receiver: Some(receiver),
      resources: ResourceRegistry::new(),
    })
  }

  /// Return a producer for this registry.
  #[must_use]
  pub fn sender(&self) -> EventSender<E> {
    self.sender.clone()
  }

  /// Return the resource registry associated with this event loop.
  #[must_use]
  pub fn resources(&self) -> ResourceRegistry {
    self.resources.clone()
  }

  /// Register a VM-thread-owned host resource.
  pub fn register(
    &self,
    name: impl Into<String>,
    referenced: bool,
  ) -> ResourceHandle {
    self.resources.register(name, referenced)
  }

  /// Return whether referenced resources or queued events keep the loop alive.
  #[must_use]
  pub fn is_live(&self) -> bool {
    self.resources.is_live()
      || self
        .receiver
        .as_ref()
        .is_some_and(|receiver| !receiver.is_empty())
  }

  /// Return whether the queue currently contains an event.
  #[must_use]
  pub fn has_pending_events(&self) -> bool {
    self
      .receiver
      .as_ref()
      .is_some_and(|receiver| !receiver.is_empty())
  }

  /// Receive one event without blocking.
  pub fn try_recv(&mut self) -> Result<E, mpsc::error::TryRecvError> {
    let Some(receiver) = self.receiver.as_mut() else {
      return Err(mpsc::error::TryRecvError::Disconnected);
    };
    receiver.try_recv()
  }

  /// Receive one event asynchronously.
  pub async fn recv(&mut self) -> Option<E> {
    let mut receiver = self.receiver.take()?;
    let event = receiver.recv().await;
    self.receiver = Some(receiver);
    event
  }

  /// Temporarily take the receiver for a host runtime's `'static` blocking
  /// bridge, such as [`denko::Vm::block_on`].
  pub fn take_receiver(&mut self) -> Option<mpsc::Receiver<E>> {
    self.receiver.take()
  }

  /// Return a receiver previously obtained with [`Self::take_receiver`].
  pub fn put_receiver(
    &mut self,
    receiver: mpsc::Receiver<E>,
  ) {
    debug_assert!(self.receiver.is_none());
    self.receiver = Some(receiver);
  }
}

#[cfg(test)]
mod tests {
  use std::time::Duration;

  use super::*;

  #[test]
  fn resources_are_generational_and_liveness_is_explicit() {
    let resources = ResourceRegistry::new();
    let first = resources.register("first", true);
    let first_id = first.id();
    assert_eq!(resources.open_count(), 1);
    assert!(resources.is_live());

    first.unreference();
    assert!(!resources.is_live());
    assert!(first.close());
    assert!(!first.close());

    let second = resources.register("second", true);
    assert_eq!(second.id().index(), first_id.index());
    assert_ne!(second.id().generation(), first_id.generation());
    first.reference();
    assert_eq!(resources.referenced_count(), 1);
    assert_eq!(first.name(), None);
    assert_eq!(second.name().as_deref(), Some("second"));
  }

  #[test]
  fn operations_are_generational_and_complete_once() {
    let mut operations = OperationRegistry::new();
    let first = operations.register("read", "first");
    assert_eq!(operations.kind(first), Some("read"));
    assert_eq!(operations.state(first), Some(OperationState::Registered));
    assert!(operations.start(first));
    assert_eq!(operations.state(first), Some(OperationState::Running));
    assert_eq!(operations.complete(first), Some("first"));
    assert_eq!(operations.complete(first), None);

    let second = operations.register("write", "second");
    assert_eq!(second.index(), first.index());
    assert_ne!(second.generation(), first.generation());
    assert_eq!(operations.active_count(), 1);
    assert_eq!(operations.state(first), None);
  }

  #[test]
  fn completion_and_cancellation_claim_one_terminal_outcome() {
    let mut completion_wins = OperationRegistry::new();
    let id = completion_wins.register("request", 1);
    assert!(completion_wins.start(id));
    assert!(completion_wins.request_cancel(id));
    assert_eq!(completion_wins.complete(id), Some(1));
    assert_eq!(completion_wins.cancelled(id), None);

    let mut cancellation_wins = OperationRegistry::new();
    let id = cancellation_wins.register("request", 2);
    assert!(cancellation_wins.request_cancel(id));
    assert_eq!(cancellation_wins.cancelled(id), Some(2));
    assert_eq!(cancellation_wins.complete(id), None);
  }

  #[test]
  fn cancel_all_closes_every_active_operation() {
    let mut operations = OperationRegistry::new();
    operations.register("first", 1);
    let second = operations.register("second", 2);
    assert!(operations.start(second));
    let mut payloads = operations.cancel_all();
    payloads.sort_unstable();
    assert_eq!(payloads, vec![1, 2]);
    assert_eq!(operations.active_count(), 0);
  }

  #[test]
  fn unreferenced_resources_do_not_keep_a_loop_alive() {
    let resources = ResourceRegistry::new();
    let handle = resources.register("socket", false);
    assert!(!resources.is_live());
    handle.reference();
    assert!(resources.is_live());
    handle.unreference();
    handle.close();
    assert_eq!(resources.open_count(), 0);
  }

  #[test]
  fn zero_capacity_queues_are_rejected() {
    assert!(EventRegistry::<u8>::new(0).is_err());
    assert!(EventRegistry::<u8>::new(1).is_ok());
  }

  #[tokio::test]
  async fn queued_events_keep_the_registry_alive_until_delivered() {
    let mut registry = EventRegistry::new(2).expect("valid capacity");
    let sender = registry.sender();
    sender.send(7_u8).await.expect("receiver is open");
    assert!(registry.is_live());
    assert_eq!(registry.recv().await, Some(7));
    assert!(!registry.is_live());
  }

  #[tokio::test]
  async fn bounded_sender_waits_for_capacity() {
    let mut registry = EventRegistry::new(1).expect("valid capacity");
    let sender = registry.sender();
    sender.try_send(1_u8).expect("first event fits");
    let pending = sender.send(2_u8);
    tokio::pin!(pending);
    assert!(
      tokio::time::timeout(Duration::from_millis(5), &mut pending)
        .await
        .is_err()
    );
    assert_eq!(registry.recv().await, Some(1));
    pending.await.expect("capacity was released");
    assert_eq!(registry.recv().await, Some(2));
  }
}
