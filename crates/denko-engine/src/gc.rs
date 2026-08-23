// SPDX-License-Identifier: MPL-2.0
//! Garbage collection support.
//!
//! This module provides a mark/sweep collector that can reclaim
//! cyclic garbage by tracking heap allocations in weak registries.

use std::cell::Cell;
use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::{Rc, Weak};

use crate::value::{
  ArrayData, ObjectData, PromiseState, PropertyDescriptor, ProxyData, SharedValue, Value, WeakKey,
  WeakValue,
};

/// Result of a prototype GC collection cycle.
#[derive(Debug, Clone, Copy, Default)]
pub struct GcStats {
  /// Why collection was deferred, when no heap scan was performed.
  pub deferred: Option<GcDeferredReason>,
  /// Number of tracked heap nodes marked reachable.
  pub marked_nodes: usize,
  /// Number of object property bags reclaimed.
  pub swept_objects: usize,
  /// Number of array backing stores reclaimed.
  pub swept_arrays: usize,
  /// Number of proxy backing stores reclaimed.
  pub swept_proxies: usize,
  /// Number of closure environments reclaimed.
  pub swept_envs: usize,
  /// Number of promise states reclaimed.
  pub swept_promises: usize,
}

/// A condition that prevents the prototype collector from safely enumerating
/// every live root.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum GcDeferredReason {
  /// More than one VM shares the current thread-local prototype heap.
  MultipleVms,
  /// A VM has host-side jobs whose captured JavaScript values are opaque.
  PendingJobs,
}

/// Snapshot of the prototype collector's tracked heap usage.
#[derive(Debug, Clone, Copy, Default)]
pub struct GcHeapSnapshot {
  /// Tracked object/property bags still strongly alive.
  pub live_objects: usize,
  /// Tracked array backing stores still strongly alive.
  pub live_arrays: usize,
  /// Tracked proxy backing stores still strongly alive.
  pub live_proxies: usize,
  /// Tracked closure environments still strongly alive.
  pub live_envs: usize,
  /// Tracked promise states still strongly alive.
  pub live_promises: usize,
  /// Weak registry slots for objects, including dead weak entries.
  pub tracked_object_slots: usize,
  /// Weak registry slots for arrays, including dead weak entries.
  pub tracked_array_slots: usize,
  /// Weak registry slots for proxies, including dead weak entries.
  pub tracked_proxy_slots: usize,
  /// Weak registry slots for environments, including dead weak entries.
  pub tracked_env_slots: usize,
  /// Weak registry slots for promises, including dead weak entries.
  pub tracked_promise_slots: usize,
  /// Current allocation pressure counter.
  pub allocation_pressure: usize,
}

#[derive(Default)]
struct HeapRegistry {
  objects: Vec<Weak<RefCell<ObjectData>>>,
  arrays: Vec<Weak<RefCell<ArrayData>>>,
  proxies: Vec<Weak<RefCell<ProxyData>>>,
  envs: Vec<Weak<RefCell<Vec<SharedValue>>>>,
  promises: Vec<Weak<RefCell<PromiseState>>>,
}

thread_local! {
    static HEAP: RefCell<HeapRegistry> = RefCell::new(HeapRegistry::default());
    static ALLOCATION_PRESSURE: Cell<usize> = const { Cell::new(0) };
    static LIVE_VMS: Cell<usize> = const { Cell::new(0) };
    static PENDING_FINALIZATION_CLEANUPS: RefCell<Vec<Weak<RefCell<ObjectData>>>> = const { RefCell::new(Vec::new()) };
}

/// Take registries that acquired unreachable targets during the preceding collection.
pub(crate) fn take_pending_finalization_cleanups() -> Vec<Rc<RefCell<ObjectData>>> {
  PENDING_FINALIZATION_CLEANUPS.with(|pending| {
    let mut seen = HashSet::new();
    pending
      .borrow_mut()
      .drain(..)
      .filter_map(|registry| registry.upgrade())
      .filter(|registry| seen.insert(Rc::as_ptr(registry) as usize))
      .collect()
  })
}

/// Registers a VM that can own roots in the thread-local prototype heap.
///
/// The collector can only enumerate one VM's roots at a time, so collection
/// is conservatively deferred while more than one VM is alive on the thread.
pub(crate) struct VmGcRegistration;

impl VmGcRegistration {
  pub(crate) fn new() -> Self {
    LIVE_VMS.with(|count| count.set(count.get().saturating_add(1)));
    Self
  }
}

impl Drop for VmGcRegistration {
  fn drop(&mut self) {
    // VMs can themselves be retained by another thread-local and dropped
    // during TLS teardown after LIVE_VMS has already been destroyed.
    let _ = LIVE_VMS.try_with(|count| {
      let current = count.get();
      debug_assert!(current > 0);
      count.set(current.saturating_sub(1));
    });
  }
}

pub(crate) fn collection_is_safe() -> bool {
  LIVE_VMS.with(|count| count.get() <= 1)
}

/// Number of VMs registered against the current thread-local prototype heap.
pub fn live_vm_count() -> usize {
  LIVE_VMS.with(Cell::get)
}

fn push_weak<T>(
  slot: &mut Vec<Weak<T>>,
  value: &Rc<T>,
) {
  slot.push(Rc::downgrade(value));
}

fn bump_allocation_pressure() {
  ALLOCATION_PRESSURE.with(|pressure| {
    pressure.set(pressure.get().saturating_add(1));
  });
}

/// Returns the number of tracked heap allocations since the last collection.
pub fn allocation_pressure() -> usize {
  ALLOCATION_PRESSURE.with(Cell::get)
}

/// Reset the tracked allocation pressure back to zero.
pub fn reset_allocation_pressure() {
  ALLOCATION_PRESSURE.with(|pressure| pressure.set(0));
}

/// Register an object property bag with the collector.
pub fn track_object(obj: Rc<RefCell<ObjectData>>) -> Rc<RefCell<ObjectData>> {
  {
    HEAP.with(|heap| push_weak(&mut heap.borrow_mut().objects, &obj));
    bump_allocation_pressure();
  }
  obj
}

/// Register an array backing store with the collector.
pub fn track_array(arr: Rc<RefCell<ArrayData>>) -> Rc<RefCell<ArrayData>> {
  {
    HEAP.with(|heap| push_weak(&mut heap.borrow_mut().arrays, &arr));
    bump_allocation_pressure();
  }
  arr
}

/// Register a proxy backing store with the collector.
pub fn track_proxy(proxy: Rc<RefCell<ProxyData>>) -> Rc<RefCell<ProxyData>> {
  {
    HEAP.with(|heap| push_weak(&mut heap.borrow_mut().proxies, &proxy));
    bump_allocation_pressure();
  }
  proxy
}

/// Register a closure environment vector with the collector.
pub fn track_env(env: Rc<RefCell<Vec<SharedValue>>>) -> Rc<RefCell<Vec<SharedValue>>> {
  {
    HEAP.with(|heap| push_weak(&mut heap.borrow_mut().envs, &env));
    bump_allocation_pressure();
  }
  env
}

/// Register a promise state with the collector.
pub fn track_promise(promise: Rc<RefCell<PromiseState>>) -> Rc<RefCell<PromiseState>> {
  {
    HEAP.with(|heap| push_weak(&mut heap.borrow_mut().promises, &promise));
    bump_allocation_pressure();
  }
  promise
}

/// Return a snapshot of tracked heap usage for the prototype collector.
pub fn heap_snapshot() -> GcHeapSnapshot {
  let mut snapshot = GcHeapSnapshot::default();
  HEAP.with(|heap| {
    let heap = heap.borrow();
    snapshot.tracked_object_slots = heap.objects.len();
    snapshot.tracked_array_slots = heap.arrays.len();
    snapshot.tracked_proxy_slots = heap.proxies.len();
    snapshot.tracked_env_slots = heap.envs.len();
    snapshot.tracked_promise_slots = heap.promises.len();
    snapshot.live_objects = heap
      .objects
      .iter()
      .filter(|w| w.strong_count() > 0)
      .count();
    snapshot.live_arrays = heap
      .arrays
      .iter()
      .filter(|w| w.strong_count() > 0)
      .count();
    snapshot.live_proxies = heap
      .proxies
      .iter()
      .filter(|w| w.strong_count() > 0)
      .count();
    snapshot.live_envs = heap
      .envs
      .iter()
      .filter(|w| w.strong_count() > 0)
      .count();
    snapshot.live_promises = heap
      .promises
      .iter()
      .filter(|w| w.strong_count() > 0)
      .count();
  });
  snapshot.allocation_pressure = allocation_pressure();
  snapshot
}

/// Prototype collector entry point.
pub fn collect(roots: &[Value]) -> GcStats {
  let env_roots: &[Rc<RefCell<Vec<SharedValue>>>] = &[];
  collect_with_env_roots(roots, env_roots)
}

/// Prototype collector entry point with explicit closure-environment roots.
pub fn collect_with_env_roots(
  roots: &[Value],
  env_roots: &[Rc<RefCell<Vec<SharedValue>>>],
) -> GcStats {
  if !collection_is_safe() {
    return GcStats {
      deferred: Some(GcDeferredReason::MultipleVms),
      ..GcStats::default()
    };
  }
  {
    {
      let clear_envs_aggressively = false;

      {
        let mut marked_objects: HashSet<usize> = HashSet::new();
        let mut marked_arrays: HashSet<usize> = HashSet::new();
        let mut marked_proxies: HashSet<usize> = HashSet::new();
        let mut marked_envs: HashSet<usize> = HashSet::new();
        let mut marked_promises: HashSet<usize> = HashSet::new();

        for root in roots {
          mark_value(
            root,
            &mut marked_objects,
            &mut marked_arrays,
            &mut marked_proxies,
            &mut marked_envs,
            &mut marked_promises,
          );
        }
        for env in env_roots {
          mark_env(
            env,
            &mut marked_objects,
            &mut marked_arrays,
            &mut marked_proxies,
            &mut marked_envs,
            &mut marked_promises,
          );
        }
        mark_ephemeron_values(
          &mut marked_objects,
          &mut marked_arrays,
          &mut marked_proxies,
          &mut marked_envs,
          &mut marked_promises,
        );
        prune_dead_ephemerons(
          &marked_objects,
          &marked_arrays,
          &marked_proxies,
          &marked_promises,
        );
        process_weak_references(
          &marked_objects,
          &marked_arrays,
          &marked_proxies,
          &marked_promises,
        );

        let mut stats = GcStats {
          marked_nodes: marked_objects.len()
            + marked_arrays.len()
            + marked_proxies.len()
            + marked_envs.len()
            + marked_promises.len(),
          ..GcStats::default()
        };

        HEAP.with(|heap| {
          let mut heap = heap.borrow_mut();
          let mut new_objects = Vec::with_capacity(heap.objects.len());
          let mut swept_objects = Vec::new();
          for weak in heap.objects.drain(..) {
            if let Some(obj) = weak.upgrade() {
              let ptr = Rc::as_ptr(&obj) as usize;
              if marked_objects.contains(&ptr) {
                new_objects.push(weak);
              } else {
                stats.swept_objects += 1;
                swept_objects.push(obj);
              }
            }
          }
          for obj in swept_objects {
            clear_object(&obj);
          }
          heap.objects = new_objects;

          let mut new_arrays = Vec::with_capacity(heap.arrays.len());
          let mut swept_arrays = Vec::new();
          for weak in heap.arrays.drain(..) {
            if let Some(arr) = weak.upgrade() {
              let ptr = Rc::as_ptr(&arr) as usize;
              if marked_arrays.contains(&ptr) {
                new_arrays.push(weak);
              } else {
                stats.swept_arrays += 1;
                swept_arrays.push(arr);
              }
            }
          }
          for arr in swept_arrays {
            clear_array(&arr);
          }
          heap.arrays = new_arrays;

          let mut new_proxies = Vec::with_capacity(heap.proxies.len());
          let mut swept_proxies = Vec::new();
          for weak in heap.proxies.drain(..) {
            if let Some(proxy) = weak.upgrade() {
              let ptr = Rc::as_ptr(&proxy) as usize;
              if marked_proxies.contains(&ptr) {
                new_proxies.push(weak);
              } else {
                stats.swept_proxies += 1;
                swept_proxies.push(proxy);
              }
            }
          }
          for proxy in swept_proxies {
            clear_proxy(&proxy);
          }
          heap.proxies = new_proxies;

          let mut new_envs = Vec::with_capacity(heap.envs.len());
          let mut swept_envs = Vec::new();
          for weak in heap.envs.drain(..) {
            if let Some(env) = weak.upgrade() {
              let ptr = Rc::as_ptr(&env) as usize;
              if marked_envs.contains(&ptr) {
                new_envs.push(weak);
              } else {
                stats.swept_envs += 1;
                swept_envs.push(env);
              }
            }
          }
          for env in swept_envs {
            if clear_envs_aggressively {
              clear_env_aggressively(&env);
            } else {
              clear_env(&env);
            }
          }
          heap.envs = new_envs;

          let mut new_promises = Vec::with_capacity(heap.promises.len());
          let mut swept_promises = Vec::new();
          for weak in heap.promises.drain(..) {
            if let Some(promise) = weak.upgrade() {
              let ptr = Rc::as_ptr(&promise) as usize;
              if marked_promises.contains(&ptr) {
                new_promises.push(weak);
              } else {
                stats.swept_promises += 1;
                swept_promises.push(promise);
              }
            }
          }
          for promise in swept_promises {
            clear_promise(&promise);
          }
          heap.promises = new_promises;
        });
        reset_allocation_pressure();

        stats
      }
    }
  }
}

/// Marks a value for the garbage collector.
#[allow(clippy::too_many_lines)]
fn mark_value(
  value: &Value,
  marked_objects: &mut HashSet<usize>,
  marked_arrays: &mut HashSet<usize>,
  marked_proxies: &mut HashSet<usize>,
  marked_envs: &mut HashSet<usize>,
  marked_promises: &mut HashSet<usize>,
) {
  match value {
    Value::Object(obj) => mark_object(
      obj,
      marked_objects,
      marked_arrays,
      marked_proxies,
      marked_envs,
      marked_promises,
    ),
    Value::Array(arr, props) => {
      let arr_ptr = Rc::as_ptr(arr) as usize;
      if marked_arrays.insert(arr_ptr) {
        for elem in arr.borrow().iter().flatten() {
          mark_value(
            elem,
            marked_objects,
            marked_arrays,
            marked_proxies,
            marked_envs,
            marked_promises,
          );
        }
      }
      mark_object(
        props,
        marked_objects,
        marked_arrays,
        marked_proxies,
        marked_envs,
        marked_promises,
      );
    }
    Value::Proxy(proxy) => {
      let ptr = Rc::as_ptr(proxy) as usize;
      if marked_proxies.insert(ptr) {
        let proxy = proxy.borrow();
        mark_value(
          &proxy.target,
          marked_objects,
          marked_arrays,
          marked_proxies,
          marked_envs,
          marked_promises,
        );
        if let Some(handler) = &proxy.handler {
          mark_value(
            handler,
            marked_objects,
            marked_arrays,
            marked_proxies,
            marked_envs,
            marked_promises,
          );
        }
      }
    }
    Value::Closure {
      env,
      lexical_this,
      props,
      ..
    } => {
      let env_ptr = Rc::as_ptr(env) as usize;
      if marked_envs.insert(env_ptr) {
        let cells: Vec<Value> = env
          .borrow()
          .iter()
          .map(|cell| cell.borrow().clone())
          .collect();
        for cell in cells {
          mark_value(
            &cell,
            marked_objects,
            marked_arrays,
            marked_proxies,
            marked_envs,
            marked_promises,
          );
        }
      }
      if let Some(this_val) = lexical_this {
        mark_value(
          this_val,
          marked_objects,
          marked_arrays,
          marked_proxies,
          marked_envs,
          marked_promises,
        );
      }
      mark_object(
        props,
        marked_objects,
        marked_arrays,
        marked_proxies,
        marked_envs,
        marked_promises,
      );
    }
    Value::BuiltinObject(_, props) => mark_object(
      props,
      marked_objects,
      marked_arrays,
      marked_proxies,
      marked_envs,
      marked_promises,
    ),
    Value::Promise(state, properties) => {
      mark_object(
        properties,
        marked_objects,
        marked_arrays,
        marked_proxies,
        marked_envs,
        marked_promises,
      );
      let ptr = Rc::as_ptr(state) as usize;
      if !marked_promises.insert(ptr) {
        return;
      }
      match state.borrow().clone() {
        PromiseState::Pending {
          on_fulfilled,
          on_rejected,
        } => {
          for cb in on_fulfilled {
            mark_value(
              &cb,
              marked_objects,
              marked_arrays,
              marked_proxies,
              marked_envs,
              marked_promises,
            );
          }
          for cb in on_rejected {
            mark_value(
              &cb,
              marked_objects,
              marked_arrays,
              marked_proxies,
              marked_envs,
              marked_promises,
            );
          }
        }
        PromiseState::Fulfilled(v) => mark_value(
          &v,
          marked_objects,
          marked_arrays,
          marked_proxies,
          marked_envs,
          marked_promises,
        ),
        PromiseState::Rejected { reason, .. } => mark_value(
          &reason,
          marked_objects,
          marked_arrays,
          marked_proxies,
          marked_envs,
          marked_promises,
        ),
      }
    }
    _ => {}
  }
}

fn mark_object(
  obj: &Rc<RefCell<ObjectData>>,
  marked_objects: &mut HashSet<usize>,
  marked_arrays: &mut HashSet<usize>,
  marked_proxies: &mut HashSet<usize>,
  marked_envs: &mut HashSet<usize>,
  marked_promises: &mut HashSet<usize>,
) {
  let ptr = Rc::as_ptr(obj) as usize;
  if !marked_objects.insert(ptr) {
    return;
  }
  let obj_ref = obj.borrow();
  if let Some(env) = &obj_ref.captured_lexical_env {
    mark_environment_record(
      env,
      marked_objects,
      marked_arrays,
      marked_proxies,
      marked_envs,
      marked_promises,
    );
  }
  for value in obj_ref.internal_slots.values() {
    mark_value(
      value,
      marked_objects,
      marked_arrays,
      marked_proxies,
      marked_envs,
      marked_promises,
    );
  }
  if let Some(callback) = &obj_ref.finalization_cleanup_callback {
    mark_value(
      callback,
      marked_objects,
      marked_arrays,
      marked_proxies,
      marked_envs,
      marked_promises,
    );
  }
  if let Some(cells) = &obj_ref.finalization_registry_cells {
    for cell in cells {
      mark_value(
        &cell.holdings,
        marked_objects,
        marked_arrays,
        marked_proxies,
        marked_envs,
        marked_promises,
      );
    }
  }
  for desc in obj_ref.properties.values() {
    match desc {
      PropertyDescriptor::Data { value, .. } => mark_value(
        value,
        marked_objects,
        marked_arrays,
        marked_proxies,
        marked_envs,
        marked_promises,
      ),
      PropertyDescriptor::Accessor { get, set, .. } => {
        if let Some(v) = get {
          mark_value(
            v,
            marked_objects,
            marked_arrays,
            marked_proxies,
            marked_envs,
            marked_promises,
          );
        }
        if let Some(v) = set {
          mark_value(
            v,
            marked_objects,
            marked_arrays,
            marked_proxies,
            marked_envs,
            marked_promises,
          );
        }
      }
    }
  }
  let prototype = obj_ref.get_prototype();
  if !matches!(prototype, Value::Null) {
    mark_value(
      &prototype,
      marked_objects,
      marked_arrays,
      marked_proxies,
      marked_envs,
      marked_promises,
    );
  }
}

fn weak_key_is_marked(
  key: &WeakKey,
  marked_objects: &HashSet<usize>,
  marked_arrays: &HashSet<usize>,
  marked_proxies: &HashSet<usize>,
  marked_promises: &HashSet<usize>,
) -> bool {
  match key {
    WeakKey::Object(value) | WeakKey::Closure(value) | WeakKey::BuiltinObject(value) => value
      .upgrade()
      .is_some_and(|value| marked_objects.contains(&(Rc::as_ptr(&value) as usize))),
    WeakKey::Array(value) => value
      .upgrade()
      .is_some_and(|value| marked_arrays.contains(&(Rc::as_ptr(&value) as usize))),
    WeakKey::Proxy(value) => value
      .upgrade()
      .is_some_and(|value| marked_proxies.contains(&(Rc::as_ptr(&value) as usize))),
    WeakKey::Promise(value) => value
      .upgrade()
      .is_some_and(|value| marked_promises.contains(&(Rc::as_ptr(&value) as usize))),
    // Bare native functions are not allocations managed by this collector.
    WeakKey::Builtin(value) => value.strong_count() > 0,
    WeakKey::Symbol(value) => value.strong_count() > 0,
  }
}

fn weak_value_is_marked(
  value: &WeakValue,
  marked_objects: &HashSet<usize>,
  marked_arrays: &HashSet<usize>,
  marked_proxies: &HashSet<usize>,
  marked_promises: &HashSet<usize>,
) -> bool {
  match value {
    WeakValue::Primitive(_) => false,
    WeakValue::Object(value) => value
      .upgrade()
      .is_some_and(|value| marked_objects.contains(&(Rc::as_ptr(&value) as usize))),
    WeakValue::Array(data, props) => {
      data
        .upgrade()
        .is_some_and(|value| marked_arrays.contains(&(Rc::as_ptr(&value) as usize)))
        && props
          .upgrade()
          .is_some_and(|value| marked_objects.contains(&(Rc::as_ptr(&value) as usize)))
    }
    WeakValue::Proxy(value) => value
      .upgrade()
      .is_some_and(|value| marked_proxies.contains(&(Rc::as_ptr(&value) as usize))),
    WeakValue::Closure(_, _, _, properties) | WeakValue::BuiltinObject(_, properties) => properties
      .upgrade()
      .is_some_and(|value| marked_objects.contains(&(Rc::as_ptr(&value) as usize))),
    WeakValue::Builtin(value) => value.strong_count() > 0,
    WeakValue::Promise(value, properties) => {
      value
        .upgrade()
        .is_some_and(|value| marked_promises.contains(&(Rc::as_ptr(&value) as usize)))
        && properties
          .upgrade()
          .is_some_and(|value| marked_objects.contains(&(Rc::as_ptr(&value) as usize)))
    }
  }
}

fn process_weak_references(
  marked_objects: &HashSet<usize>,
  marked_arrays: &HashSet<usize>,
  marked_proxies: &HashSet<usize>,
  marked_promises: &HashSet<usize>,
) {
  let registries = HEAP.with(|heap| {
    heap
      .borrow()
      .objects
      .iter()
      .filter_map(Weak::upgrade)
      .filter(|object| marked_objects.contains(&(Rc::as_ptr(object) as usize)))
      .collect::<Vec<_>>()
  });
  for object in registries {
    let object_ref = Rc::clone(&object);
    let mut object = object.borrow_mut();
    if object
      .weak_ref_target
      .as_ref()
      .is_some_and(|target| {
        !weak_value_is_marked(
          target,
          marked_objects,
          marked_arrays,
          marked_proxies,
          marked_promises,
        )
      })
    {
      object.weak_ref_target = None;
    }
    let Some(cells) = object.finalization_registry_cells.as_mut() else {
      continue;
    };
    let mut has_new_cleanup = false;
    for cell in cells.iter_mut().filter(|cell| cell.active) {
      if !weak_value_is_marked(
        &cell.target,
        marked_objects,
        marked_arrays,
        marked_proxies,
        marked_promises,
      ) {
        cell.active = false;
        has_new_cleanup = true;
      }
    }
    if has_new_cleanup && !object.finalization_cleanup_scheduled {
      object.finalization_cleanup_scheduled = true;
      PENDING_FINALIZATION_CLEANUPS.with(|pending| {
        pending
          .borrow_mut()
          .push(Rc::downgrade(&object_ref));
      });
    }
  }
}

fn mark_ephemeron_values(
  marked_objects: &mut HashSet<usize>,
  marked_arrays: &mut HashSet<usize>,
  marked_proxies: &mut HashSet<usize>,
  marked_envs: &mut HashSet<usize>,
  marked_promises: &mut HashSet<usize>,
) {
  loop {
    let marked_before = marked_objects.len()
      + marked_arrays.len()
      + marked_proxies.len()
      + marked_envs.len()
      + marked_promises.len();
    let ephemerons = HEAP.with(|heap| {
      heap
        .borrow()
        .objects
        .iter()
        .filter_map(Weak::upgrade)
        .filter(|object| marked_objects.contains(&(Rc::as_ptr(object) as usize)))
        .flat_map(|object| {
          object
            .borrow()
            .weak_map_entries
            .clone()
            .unwrap_or_default()
        })
        .collect::<Vec<_>>()
    });
    for entry in ephemerons {
      if weak_key_is_marked(
        &entry.key,
        marked_objects,
        marked_arrays,
        marked_proxies,
        marked_promises,
      ) {
        mark_value(
          &entry.value,
          marked_objects,
          marked_arrays,
          marked_proxies,
          marked_envs,
          marked_promises,
        );
      }
    }
    let marked_after = marked_objects.len()
      + marked_arrays.len()
      + marked_proxies.len()
      + marked_envs.len()
      + marked_promises.len();
    if marked_after == marked_before {
      break;
    }
  }
}

fn prune_dead_ephemerons(
  marked_objects: &HashSet<usize>,
  marked_arrays: &HashSet<usize>,
  marked_proxies: &HashSet<usize>,
  marked_promises: &HashSet<usize>,
) {
  HEAP.with(|heap| {
    for object in heap
      .borrow()
      .objects
      .iter()
      .filter_map(Weak::upgrade)
    {
      if !marked_objects.contains(&(Rc::as_ptr(&object) as usize)) {
        continue;
      }
      let mut object = object.borrow_mut();
      if let Some(entries) = object.weak_map_entries.as_mut() {
        entries.retain(|entry| {
          weak_key_is_marked(
            &entry.key,
            marked_objects,
            marked_arrays,
            marked_proxies,
            marked_promises,
          )
        });
      }
      if let Some(entries) = object.weak_set_entries.as_mut() {
        entries.retain(|key| {
          weak_key_is_marked(
            key,
            marked_objects,
            marked_arrays,
            marked_proxies,
            marked_promises,
          )
        });
      }
    }
  });
}

fn mark_env(
  env: &Rc<RefCell<Vec<SharedValue>>>,
  marked_objects: &mut HashSet<usize>,
  marked_arrays: &mut HashSet<usize>,
  marked_proxies: &mut HashSet<usize>,
  marked_envs: &mut HashSet<usize>,
  marked_promises: &mut HashSet<usize>,
) {
  let ptr = Rc::as_ptr(env) as usize;
  if !marked_envs.insert(ptr) {
    return;
  }
  let cells: Vec<Value> = env
    .borrow()
    .iter()
    .map(|cell| cell.borrow().clone())
    .collect();
  for cell in cells {
    mark_value(
      &cell,
      marked_objects,
      marked_arrays,
      marked_proxies,
      marked_envs,
      marked_promises,
    );
  }
}

fn mark_environment_record(
  env: &Rc<RefCell<crate::environment::EnvironmentRecord>>,
  marked_objects: &mut HashSet<usize>,
  marked_arrays: &mut HashSet<usize>,
  marked_proxies: &mut HashSet<usize>,
  marked_envs: &mut HashSet<usize>,
  marked_promises: &mut HashSet<usize>,
) {
  let ptr = Rc::as_ptr(env) as usize;
  if !marked_envs.insert(ptr) {
    return;
  }
  let mut values = Vec::new();
  env.borrow().collect_binding_values(&mut values);
  for value in values {
    mark_value(
      &value,
      marked_objects,
      marked_arrays,
      marked_proxies,
      marked_envs,
      marked_promises,
    );
  }
}

fn clear_object(obj: &Rc<RefCell<ObjectData>>) {
  let mut obj = obj.borrow_mut();
  obj.properties.clear();
  obj.internal_slots.clear();
  obj.weak_ref_target = None;
  obj.finalization_registry_cells = None;
  obj.finalization_cleanup_callback = None;
  obj.finalization_cleanup_scheduled = false;
  obj.weak_map_entries = None;
  obj.weak_set_entries = None;
  obj.strong_prototype = None;
  obj.weak_prototype = None;
  obj.captured_lexical_env = None;
  obj.extensible = true;
}

fn clear_proxy(proxy: &Rc<RefCell<ProxyData>>) {
  let mut proxy = proxy.borrow_mut();
  proxy.target = Value::Undefined;
  proxy.handler = None;
}

fn clear_array(arr: &Rc<RefCell<ArrayData>>) {
  arr.borrow_mut().clear();
}

fn clear_env(env: &Rc<RefCell<Vec<SharedValue>>>) {
  // Break closure cycles by dropping the captured values once the
  // environment is determined to be unreachable.
  env.borrow_mut().clear();
}

fn clear_env_aggressively(env: &Rc<RefCell<Vec<SharedValue>>>) {
  clear_env(env);
}

fn clear_promise(state: &Rc<RefCell<PromiseState>>) {
  *state.borrow_mut() = PromiseState::Fulfilled(Value::Undefined);
}

#[cfg(test)]
mod tests {
  #![allow(clippy::unreachable)]
  use super::*;

  use crate::bytecode::{Chunk, FunctionProto};

  use crate::value::SharedValue;

  use oxc_span::Span;

  use std::sync::Arc;

  fn make_dummy_closure(env: Rc<RefCell<Vec<SharedValue>>>) -> Value {
    let proto = Rc::new(FunctionProto {
      name: "gc_test".to_string(),
      span: Span::new(0, 0),
      arity: 0,
      chunk: Arc::new(Chunk::new(Arc::new(String::new()))),
      slot_types: Vec::new(),
      slot_names: Vec::new(),
      lexical_tdz: Vec::new(),
      slot_immutable: Vec::new(),
      slot_sloppy_immutable_silent: Vec::new(),
      upvalues: Vec::new(),
      has_rest: false,
      has_prototype_property: false,
      captures_this: false,
      arguments_slot: None,
      self_binding_slot: None,
      has_simple_parameter_list: true,
      return_type: None,
      is_strict: false,
      is_constructor: false,
      is_derived_constructor: false,
      is_class_constructor: false,
      is_async: false,
      is_generator: false,
    });
    let props = track_object(Rc::new(RefCell::new(ObjectData::new())));
    Value::Closure {
      proto,
      env,
      lexical_this: None,
      props,
    }
  }

  #[test]
  fn sweep_breaks_object_cycles() {
    let a = track_object(Rc::new(RefCell::new(ObjectData::new())));
    let b = track_object(Rc::new(RefCell::new(ObjectData::new())));

    a.borrow_mut().properties.insert(
      crate::value::PropertyKey::String("peer".to_string().into()),
      PropertyDescriptor::ordinary_data(Value::Object(Rc::clone(&b))),
    );
    b.borrow_mut().properties.insert(
      crate::value::PropertyKey::String("peer".to_string().into()),
      PropertyDescriptor::ordinary_data(Value::Object(Rc::clone(&a))),
    );

    let weak_a = Rc::downgrade(&a);
    let weak_b = Rc::downgrade(&b);

    drop(a);
    drop(b);

    let stats = collect(&[]);
    assert_eq!(stats.swept_objects, 2);
    assert!(weak_a.upgrade().is_none());
    assert!(weak_b.upgrade().is_none());
  }

  #[test]
  fn regular_collect_breaks_closure_env_cycles() {
    let env = track_env(Rc::new(RefCell::new(Vec::new())));
    let closure = make_dummy_closure(Rc::clone(&env));
    let weak_props = match &closure {
      Value::Closure { props, .. } => Rc::downgrade(props),
      _ => unreachable!("dummy closure must be a closure"),
    };

    env
      .borrow_mut()
      .push(Rc::new(RefCell::new(closure.clone())));

    let weak_env = Rc::downgrade(&env);

    drop(closure);
    drop(env);

    let stats = collect(&[]);

    assert!(stats.swept_envs >= 1);
    assert!(weak_env.upgrade().is_none());
    assert!(weak_props.upgrade().is_none());
  }

  #[test]
  fn aggressive_collect_breaks_closure_env_cycles() {
    let env = track_env(Rc::new(RefCell::new(Vec::new())));
    let closure = make_dummy_closure(Rc::clone(&env));
    let weak_props = match &closure {
      Value::Closure { props, .. } => Rc::downgrade(props),
      _ => unreachable!("dummy closure must be a closure"),
    };

    env
      .borrow_mut()
      .push(Rc::new(RefCell::new(closure.clone())));

    let weak_env = Rc::downgrade(&env);

    drop(closure);
    drop(env);

    let stats = collect(&[]);
    assert!(stats.swept_envs >= 1);
    assert!(weak_env.upgrade().is_none());
    assert!(weak_props.upgrade().is_none());
  }

  #[test]
  fn aggressive_collect_respects_roots() {
    let env = track_env(Rc::new(RefCell::new(Vec::new())));
    let closure = make_dummy_closure(Rc::clone(&env));
    let weak_props = match &closure {
      Value::Closure { props, .. } => Rc::downgrade(props),
      _ => unreachable!("dummy closure must be a closure"),
    };

    env
      .borrow_mut()
      .push(Rc::new(RefCell::new(closure.clone())));

    let weak_env = Rc::downgrade(&env);

    drop(env);

    let stats = collect(std::slice::from_ref(&closure));
    assert_eq!(stats.swept_envs, 0);
    assert!(weak_env.upgrade().is_some());
    assert!(weak_props.upgrade().is_some());
  }

  #[test]
  fn allocation_pressure_tracks_heap_allocations() {
    reset_allocation_pressure();
    assert_eq!(allocation_pressure(), 0);

    let _obj = track_object(Rc::new(RefCell::new(ObjectData::new())));
    let _arr = track_array(Rc::new(RefCell::new(ArrayData::from(
      Vec::<Option<Value>>::new(),
    ))));
    let _env = track_env(Rc::new(RefCell::new(Vec::new())));
    let _promise = track_promise(Rc::new(RefCell::new(PromiseState::Pending {
      on_fulfilled: Vec::new(),
      on_rejected: Vec::new(),
    })));

    assert!(allocation_pressure() >= 4);

    reset_allocation_pressure();
    assert_eq!(allocation_pressure(), 0);
  }

  #[test]
  fn collection_reports_multiple_vm_deferral() {
    let first = VmGcRegistration::new();
    let second = VmGcRegistration::new();

    let stats = collect(&[]);

    assert_eq!(stats.deferred, Some(GcDeferredReason::MultipleVms));
    drop(second);
    drop(first);
  }
}
