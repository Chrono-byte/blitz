//! Cancellation-safe host scheduling primitives for VM globals.
//!
//! The scheduler owns no JavaScript values. A host global stores a [`TimerId`]
//! alongside its callback and uses [`TimerScheduler::handle_wakeup`] to turn a
//! delivered [`WakeupEvent`] into a callback-ready [`TimerFired`] value. This
//! keeps callback execution on the VM thread while timer waits run in the
//! VM's background runtime.
//!
//! Timer resources are registered in the caller's [`ResourceRegistry`]. A
//! referenced timer keeps that registry live; unreferencing or cancelling it
//! changes liveness immediately. Wakeups are generation-safe: a wakeup racing
//! with cancellation is discarded rather than invoking a stale callback.

use std::collections::HashMap;
use std::fmt;
use std::time::{Duration, Instant};

use blitz::Vm;
use tokio::sync::{mpsc, watch};

use super::event_loop::{EventRegistry, EventSender, ResourceHandle, ResourceRegistry};

/// A monotonically allocated identifier for a host timer.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct TimerId(u64);

impl TimerId {
  /// Return the numeric value suitable for storing in a host-global table.
  #[must_use]
  pub const fn value(self) -> u64 {
    self.0
  }

  /// Reconstruct a timer identifier supplied by a host global.
  #[must_use]
  pub(crate) const fn from_value(value: u64) -> Self {
    Self(value)
  }
}

/// The callback behavior of a timer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimerKind {
  /// Fire once and then release its resource.
  Timeout,
  /// Fire repeatedly until cancelled.
  Interval,
}

/// A timer wakeup delivered to the VM thread.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TimerWakeup {
  id: TimerId,
  deadline: Instant,
}

impl TimerWakeup {
  /// Return the timer that produced this wakeup.
  #[must_use]
  pub const fn id(self) -> TimerId {
    self.id
  }

  /// Return the deadline associated with this generation of the timer.
  #[must_use]
  pub const fn deadline(self) -> Instant {
    self.deadline
  }
}

/// Events which may wake an otherwise idle host runtime.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WakeupEvent {
  /// A timeout or interval reached its deadline.
  Timer(TimerWakeup),
  /// Work in an external scheduler became runnable.
  EffectScheduler,
}

impl WakeupEvent {
  /// Construct a wakeup for externally scheduled Effect work.
  #[must_use]
  pub const fn effect_scheduler() -> Self {
    Self::EffectScheduler
  }

  /// Return the timer wakeup, if this is a timer event.
  #[must_use]
  pub const fn timer(self) -> Option<TimerWakeup> {
    match self {
      Self::Timer(wakeup) => Some(wakeup),
      Self::EffectScheduler => None,
    }
  }
}

/// A callback-ready notification produced by [`TimerScheduler::handle_wakeup`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TimerFired {
  id: TimerId,
  kind: TimerKind,
}

impl TimerFired {
  /// Return the timer whose callback should run.
  #[must_use]
  pub const fn id(self) -> TimerId {
    self.id
  }

  /// Return whether the callback belongs to a timeout or interval.
  #[must_use]
  pub const fn kind(self) -> TimerKind {
    self.kind
  }
}

/// A process-local monotonic clock suitable for `performance.now()`.
#[derive(Clone, Copy, Debug)]
pub struct MonotonicClock {
  origin: Instant,
}

impl MonotonicClock {
  /// Start a new clock at the current monotonic instant.
  #[must_use]
  pub fn new() -> Self {
    Self {
      origin: Instant::now(),
    }
  }

  /// Return elapsed monotonic time since this clock was created.
  #[must_use]
  pub fn now(&self) -> Duration {
    self.origin.elapsed()
  }

  /// Return elapsed monotonic time in milliseconds.
  #[must_use]
  pub fn now_millis(&self) -> f64 {
    self.now().as_secs_f64() * 1_000.0
  }

  /// Return an absolute deadline after the supplied duration.
  #[must_use]
  pub fn deadline_after(
    &self,
    delay: Duration,
  ) -> Option<Instant> {
    Instant::now().checked_add(delay)
  }
}

impl Default for MonotonicClock {
  fn default() -> Self {
    Self::new()
  }
}

/// Errors raised while allocating a timer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimerError {
  /// All non-zero timer identifiers have been exhausted.
  IdExhausted,
  /// The requested delay cannot be represented by [`Instant`].
  DelayOverflow,
}

impl fmt::Display for TimerError {
  fn fmt(
    &self,
    formatter: &mut fmt::Formatter<'_>,
  ) -> fmt::Result {
    match self {
      Self::IdExhausted => formatter.write_str("host timer identifiers are exhausted"),
      Self::DelayOverflow => formatter.write_str("host timer deadline overflowed"),
    }
  }
}

impl std::error::Error for TimerError {}

struct TimerEntry {
  kind: TimerKind,
  interval: Option<Duration>,
  deadline: Instant,
  resource: ResourceHandle,
  cancelled: Option<watch::Sender<bool>>,
}

/// VM-thread timer state connected to a runtime event queue.
///
/// `E` is the embedder's event type. The mapper turns a scheduler-owned
/// [`WakeupEvent`] into that type, allowing the scheduler to use the existing
/// [`EventRegistry`] rather than creating a second event loop. The common
/// [`WakeupEvent`] specialization is available through [`Self::new`].
pub struct TimerScheduler<E> {
  resources: ResourceRegistry,
  sender: EventSender<E>,
  map_event: fn(WakeupEvent) -> E,
  clock: MonotonicClock,
  timers: HashMap<TimerId, TimerEntry>,
  next_id: u64,
}

impl TimerScheduler<WakeupEvent> {
  /// Create a scheduler whose queue contains [`WakeupEvent`] values directly.
  #[must_use]
  pub fn new(
    resources: ResourceRegistry,
    sender: EventSender<WakeupEvent>,
  ) -> Self {
    Self::with_event_mapper(resources, sender, |event| event)
  }
}

impl<E> TimerScheduler<E>
where
  E: Send + 'static,
{
  /// Create a scheduler which maps wakeups into an existing runtime event.
  #[must_use]
  pub fn with_event_mapper(
    resources: ResourceRegistry,
    sender: EventSender<E>,
    map_event: fn(WakeupEvent) -> E,
  ) -> Self {
    Self {
      resources,
      sender,
      map_event,
      clock: MonotonicClock::new(),
      timers: HashMap::new(),
      next_id: 1,
    }
  }

  /// Register and arm a one-shot timeout.
  ///
  /// The callback itself is never run by the background task. The task only
  /// sends a wakeup; the VM thread must call [`Self::handle_wakeup`].
  ///
  /// # Errors
  ///
  /// Returns an error if an identifier or absolute deadline cannot be made.
  pub fn set_timeout(
    &mut self,
    vm: &Vm,
    delay: Duration,
    referenced: bool,
  ) -> Result<TimerId, TimerError> {
    self.insert_timer(vm, TimerKind::Timeout, delay, None, referenced)
  }

  /// Register and arm a repeating interval.
  ///
  /// An interval has one outstanding wakeup at a time. This prevents a slow VM
  /// thread from accumulating an unbounded backlog of interval callbacks.
  ///
  /// # Errors
  ///
  /// Returns an error if an identifier or absolute deadline cannot be made.
  pub fn set_interval(
    &mut self,
    vm: &Vm,
    interval: Duration,
    referenced: bool,
  ) -> Result<TimerId, TimerError> {
    self.insert_timer(
      vm,
      TimerKind::Interval,
      interval,
      Some(interval),
      referenced,
    )
  }

  /// Cancel a timeout or interval and release its resource.
  ///
  /// This operation is idempotent. A wakeup already in the event queue remains
  /// harmless because its timer ID no longer resolves in the scheduler.
  pub fn clear_timer(
    &mut self,
    id: TimerId,
  ) -> bool {
    let Some(mut entry) = self.timers.remove(&id) else {
      return false;
    };
    drop(entry.cancelled.take());
    entry.resource.close()
  }

  /// Alias for [`Self::clear_timer`] for host `clearTimeout` globals.
  pub fn clear_timeout(
    &mut self,
    id: TimerId,
  ) -> bool {
    self.clear_timer(id)
  }

  /// Alias for [`Self::clear_timer`] for host `clearInterval` globals.
  pub fn clear_interval(
    &mut self,
    id: TimerId,
  ) -> bool {
    self.clear_timer(id)
  }

  /// Mark a timer as referenced by, or unreferenced from, runtime liveness.
  ///
  /// Unreferencing does not stop the timer while another resource keeps the
  /// runtime running; it only permits the runtime to exit without this timer.
  pub fn set_referenced(
    &self,
    id: TimerId,
    referenced: bool,
  ) -> bool {
    self.timers.get(&id).is_some_and(|entry| {
      if referenced {
        entry.resource.reference();
      } else {
        entry.resource.unreference();
      }
      true
    })
  }

  /// Send a coalescible wakeup for work made runnable by an Effect scheduler.
  ///
  /// A full queue means another event is already sufficient to wake the VM.
  ///
  /// # Errors
  ///
  /// Returns the mapped event when the event queue is closed or full.
  pub fn wake_effect_scheduler(&self) -> Result<(), mpsc::error::TrySendError<E>> {
    self
      .sender
      .try_send((self.map_event)(WakeupEvent::effect_scheduler()))
  }

  /// Validate and consume a timer wakeup on the VM thread.
  ///
  /// Stale or cancelled wakeups return `None`. For an interval, the next
  /// wakeup is armed before the callback-ready value is returned.
  pub fn handle_wakeup(
    &mut self,
    vm: &Vm,
    wakeup: TimerWakeup,
    now: Instant,
  ) -> Option<TimerFired> {
    let entry = self.timers.get(&wakeup.id)?;
    if entry.deadline != wakeup.deadline {
      return None;
    }
    if now < entry.deadline {
      self.arm(vm, wakeup.id, entry.deadline);
      return None;
    }

    let kind = entry.kind;
    if let Some(interval) = entry.interval {
      let next_deadline = entry
        .deadline
        .checked_add(interval)
        .filter(|deadline| *deadline > now)
        .or_else(|| now.checked_add(interval));
      let Some(next_deadline) = next_deadline else {
        self.clear_timer(wakeup.id);
        return None;
      };
      if let Some(entry) = self.timers.get_mut(&wakeup.id) {
        entry.deadline = next_deadline;
      }
      self.arm(vm, wakeup.id, next_deadline);
      Some(TimerFired {
        id: wakeup.id,
        kind,
      })
    } else {
      self.clear_timer(wakeup.id);
      Some(TimerFired {
        id: wakeup.id,
        kind,
      })
    }
  }

  /// Return the scheduler's monotonic clock.
  #[must_use]
  pub const fn clock(&self) -> &MonotonicClock {
    &self.clock
  }

  /// Return the next timer deadline, if any timer remains armed.
  #[must_use]
  pub fn next_deadline(&self) -> Option<Instant> {
    self
      .timers
      .values()
      .map(|entry| entry.deadline)
      .min()
  }

  /// Return the number of active timers.
  #[must_use]
  pub fn len(&self) -> usize {
    self.timers.len()
  }

  /// Return whether no active timer remains.
  #[must_use]
  pub fn is_empty(&self) -> bool {
    self.timers.is_empty()
  }

  /// Return whether this scheduler has a referenced timer.
  #[must_use]
  pub fn is_live(&self) -> bool {
    self
      .timers
      .values()
      .any(|entry| entry.resource.is_referenced())
  }

  /// Return the registry shared by this scheduler and its event loop.
  #[must_use]
  pub fn resources(&self) -> ResourceRegistry {
    self.resources.clone()
  }

  fn insert_timer(
    &mut self,
    vm: &Vm,
    kind: TimerKind,
    delay: Duration,
    interval: Option<Duration>,
    referenced: bool,
  ) -> Result<TimerId, TimerError> {
    let deadline = self
      .clock
      .deadline_after(delay)
      .ok_or(TimerError::DelayOverflow)?;
    let id = self.allocate_id()?;
    let resource = self
      .resources
      .register(format!("timer:{}", id.value()), referenced);
    self.timers.insert(
      id,
      TimerEntry {
        kind,
        interval,
        deadline,
        resource,
        cancelled: None,
      },
    );
    self.arm(vm, id, deadline);
    Ok(id)
  }

  fn allocate_id(&mut self) -> Result<TimerId, TimerError> {
    let id = TimerId(self.next_id);
    self.next_id = self
      .next_id
      .checked_add(1)
      .ok_or(TimerError::IdExhausted)?;
    Ok(id)
  }

  fn arm(
    &mut self,
    vm: &Vm,
    id: TimerId,
    deadline: Instant,
  ) {
    let Some(entry) = self.timers.get_mut(&id) else {
      return;
    };
    drop(entry.cancelled.take());
    let (cancel_tx, mut cancel_rx) = watch::channel(false);
    entry.cancelled = Some(cancel_tx);
    let sender = self.sender.clone();
    let event = (self.map_event)(WakeupEvent::Timer(TimerWakeup { id, deadline }));
    vm.spawn_bg(async move {
      let sleep = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline));
      tokio::pin!(sleep);
      tokio::select! {
        _ = &mut sleep => {
          let send = sender.send(event);
          let cancelled = cancel_rx.changed();
          tokio::pin!(cancelled);
          tokio::select! {
            _ = &mut cancelled => {}
            _ = send => {}
          }
        }
        _ = cancel_rx.changed() => {}
      }
    });
  }
}

impl<E> Drop for TimerScheduler<E> {
  fn drop(&mut self) {
    for (_, mut entry) in self.timers.drain() {
      drop(entry.cancelled.take());
      entry.resource.close();
    }
  }
}

/// Create an event registry dedicated to direct [`WakeupEvent`] delivery.
///
/// Most embedders should instead map wakeups into their existing runtime event
/// enum with [`TimerScheduler::with_event_mapper`].
///
/// # Errors
///
/// Returns [`event_loop::EventRegistryError`] when `capacity` is zero.
pub fn new_wakeup_registry(
  capacity: usize
) -> Result<EventRegistry<WakeupEvent>, super::event_loop::EventRegistryError> {
  EventRegistry::new(capacity)
}

#[cfg(test)]
mod tests {
  use std::cell::RefCell;
  use std::rc::Rc;

  use blitz::value::ObjectData;

  use super::*;

  fn test_vm() -> Vm {
    Vm::new(Rc::new(RefCell::new(ObjectData::new())))
  }

  #[test]
  fn monotonic_clock_is_relative_and_non_decreasing() {
    let clock = MonotonicClock::new();
    let first = clock.now_millis();
    let second = clock.now_millis();
    assert!(first >= 0.0);
    assert!(second >= first);
    assert!(
      clock
        .deadline_after(Duration::from_millis(1))
        .is_some()
    );
  }

  #[tokio::test]
  async fn timeout_wakes_and_releases_its_referenced_resource() {
    let vm = test_vm();
    let mut registry = new_wakeup_registry(4).expect("valid wakeup capacity");
    let mut scheduler = TimerScheduler::new(registry.resources(), registry.sender());
    let id = scheduler
      .set_timeout(&vm, Duration::from_millis(1), true)
      .expect("timer should arm");
    assert!(scheduler.is_live());
    let event = tokio::time::timeout(Duration::from_secs(1), registry.recv())
      .await
      .expect("timer should wake")
      .expect("scheduler sender remains open");
    let wakeup = event.timer().expect("timer event");
    assert_eq!(wakeup.id(), id);
    let fired = scheduler
      .handle_wakeup(&vm, wakeup, Instant::now())
      .expect("live timeout should fire");
    assert_eq!(fired.id(), id);
    assert_eq!(fired.kind(), TimerKind::Timeout);
    assert!(!scheduler.is_live());
    assert!(scheduler.is_empty());
  }

  #[tokio::test]
  async fn cancellation_discards_a_racing_wakeup() {
    let vm = test_vm();
    let mut registry = new_wakeup_registry(4).expect("valid wakeup capacity");
    let mut scheduler = TimerScheduler::new(registry.resources(), registry.sender());
    let id = scheduler
      .set_timeout(&vm, Duration::from_millis(20), true)
      .expect("timer should arm");
    assert!(scheduler.clear_timeout(id));
    assert!(!scheduler.clear_timeout(id));
    assert!(!scheduler.is_live());
    assert!(
      tokio::time::timeout(Duration::from_millis(50), registry.recv())
        .await
        .is_err()
    );
  }

  #[tokio::test]
  async fn unreferenced_timer_does_not_keep_its_registry_alive() {
    let vm = test_vm();
    let registry = new_wakeup_registry(4).expect("valid wakeup capacity");
    let mut scheduler = TimerScheduler::new(registry.resources(), registry.sender());
    let id = scheduler
      .set_timeout(&vm, Duration::from_secs(1), false)
      .expect("timer should arm");
    assert!(!scheduler.is_live());
    assert!(!registry.resources().is_live());
    scheduler.set_referenced(id, true);
    assert!(registry.resources().is_live());
    scheduler.set_referenced(id, false);
    assert!(!registry.resources().is_live());
    assert!(scheduler.clear_timer(id));
  }

  #[tokio::test]
  async fn interval_rearms_only_after_the_previous_wakeup_is_consumed() {
    let vm = test_vm();
    let mut registry = new_wakeup_registry(4).expect("valid wakeup capacity");
    let mut scheduler = TimerScheduler::new(registry.resources(), registry.sender());
    let id = scheduler
      .set_interval(&vm, Duration::from_millis(1), false)
      .expect("interval should arm");
    let first = tokio::time::timeout(Duration::from_secs(1), registry.recv())
      .await
      .expect("first interval wakeup")
      .expect("scheduler sender remains open")
      .timer()
      .expect("timer event");
    let fired = scheduler
      .handle_wakeup(&vm, first, Instant::now())
      .expect("first interval should fire");
    assert_eq!(fired.id(), id);
    let second = tokio::time::timeout(Duration::from_secs(1), registry.recv())
      .await
      .expect("second interval wakeup")
      .expect("scheduler sender remains open")
      .timer()
      .expect("timer event");
    assert_eq!(second.id(), id);
    assert_ne!(second.deadline(), first.deadline());
    scheduler.clear_interval(id);
    assert!(!scheduler.is_live());
  }

  #[tokio::test]
  async fn stale_wakeups_are_ignored_after_cancellation() {
    let vm = test_vm();
    let registry = new_wakeup_registry(2).expect("valid wakeup capacity");
    let mut scheduler = TimerScheduler::new(registry.resources(), registry.sender());
    let id = scheduler
      .set_timeout(&vm, Duration::from_secs(1), true)
      .expect("timer should arm");
    let deadline = scheduler.next_deadline().expect("timer deadline");
    assert!(scheduler.clear_timer(id));
    assert!(
      scheduler
        .handle_wakeup(&vm, TimerWakeup { id, deadline }, Instant::now())
        .is_none()
    );
  }
}
