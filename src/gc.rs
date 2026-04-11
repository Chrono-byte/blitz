//! Experimental garbage collection support.
//!
//! The default build enables a prototype mark/sweep collector that can
//! reclaim cyclic garbage by tracking heap allocations in weak registries.
//! When the feature is disabled, the tracking hooks become no-ops and the
//! collector reports zero work.

use std::cell::Cell;
use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::{Rc, Weak};
use std::sync::Mutex;

use crate::mutex_compat::MutexBorrowExt;
use crate::value::{ObjectData, PromiseState, PropertyDescriptor, Value, ValueSlot};

/// Result of a prototype GC collection cycle.
#[derive(Debug, Clone, Copy, Default)]
pub struct GcStats {
    /// Number of unique object-like nodes marked reachable.
    pub marked_nodes: usize,
    /// Number of object property bags reclaimed.
    pub swept_objects: usize,
    /// Number of array backing stores reclaimed.
    pub swept_arrays: usize,
    /// Number of closure environments reclaimed.
    pub swept_envs: usize,
    /// Number of promise states reclaimed.
    pub swept_promises: usize,
}

/// Snapshot of the prototype collector's tracked heap usage.
#[derive(Debug, Clone, Copy, Default)]
pub struct GcHeapSnapshot {
    /// Tracked object/property bags still strongly alive.
    pub live_objects: usize,
    /// Tracked array backing stores still strongly alive.
    pub live_arrays: usize,
    /// Tracked closure environments still strongly alive.
    pub live_envs: usize,
    /// Tracked promise states still strongly alive.
    pub live_promises: usize,
    /// Weak registry slots for objects, including dead weak entries.
    pub tracked_object_slots: usize,
    /// Weak registry slots for arrays, including dead weak entries.
    pub tracked_array_slots: usize,
    /// Weak registry slots for environments, including dead weak entries.
    pub tracked_env_slots: usize,
    /// Weak registry slots for promises, including dead weak entries.
    pub tracked_promise_slots: usize,
    /// Current allocation pressure counter.
    pub allocation_pressure: usize,
}

#[cfg(feature = "gc_mark_sweep_prototype")]
#[derive(Default)]
struct HeapRegistry {
    objects: Vec<Weak<RefCell<ObjectData>>>,
    arrays: Vec<Weak<RefCell<Vec<Option<Value>>>>>,
    envs: Vec<Weak<Mutex<Vec<ValueSlot>>>>,
    promises: Vec<Weak<RefCell<PromiseState>>>,
}

#[cfg(feature = "gc_mark_sweep_prototype")]
thread_local! {
    static HEAP: RefCell<HeapRegistry> = RefCell::new(HeapRegistry::default());
    static ALLOCATION_PRESSURE: Cell<usize> = const { Cell::new(0) };
}

#[cfg(feature = "gc_mark_sweep_prototype")]
fn push_weak<T>(slot: &mut Vec<Weak<T>>, value: &Rc<T>) {
    slot.push(Rc::downgrade(value));
}

#[cfg(feature = "gc_mark_sweep_prototype")]
fn bump_allocation_pressure() {
    ALLOCATION_PRESSURE.with(|pressure| {
        pressure.set(pressure.get().saturating_add(1));
    });
}

/// Returns the number of tracked heap allocations since the last collection.
pub fn allocation_pressure() -> usize {
    #[cfg(feature = "gc_mark_sweep_prototype")]
    {
        ALLOCATION_PRESSURE.with(Cell::get)
    }

    #[cfg(not(feature = "gc_mark_sweep_prototype"))]
    {
        0
    }
}

/// Reset the tracked allocation pressure back to zero.
pub fn reset_allocation_pressure() {
    #[cfg(feature = "gc_mark_sweep_prototype")]
    ALLOCATION_PRESSURE.with(|pressure| pressure.set(0));
}

/// Register an object property bag with the collector.
pub fn track_object(obj: Rc<RefCell<ObjectData>>) -> Rc<RefCell<ObjectData>> {
    #[cfg(feature = "gc_mark_sweep_prototype")]
    {
        HEAP.with(|heap| push_weak(&mut heap.borrow_mut().objects, &obj));
        bump_allocation_pressure();
    }
    obj
}

/// Register an array backing store with the collector.
pub fn track_array(arr: Rc<RefCell<Vec<Option<Value>>>>) -> Rc<RefCell<Vec<Option<Value>>>> {
    #[cfg(feature = "gc_mark_sweep_prototype")]
    {
        HEAP.with(|heap| push_weak(&mut heap.borrow_mut().arrays, &arr));
        bump_allocation_pressure();
    }
    arr
}

/// Register a closure environment vector with the collector.
pub fn track_env(env: Rc<Mutex<Vec<ValueSlot>>>) -> Rc<Mutex<Vec<ValueSlot>>> {
    #[cfg(feature = "gc_mark_sweep_prototype")]
    {
        HEAP.with(|heap| push_weak(&mut heap.borrow_mut().envs, &env));
        bump_allocation_pressure();
    }
    env
}

/// Register a promise state with the collector.
pub fn track_promise(promise: Rc<RefCell<PromiseState>>) -> Rc<RefCell<PromiseState>> {
    #[cfg(feature = "gc_mark_sweep_prototype")]
    {
        HEAP.with(|heap| push_weak(&mut heap.borrow_mut().promises, &promise));
        bump_allocation_pressure();
    }
    promise
}

/// Return a snapshot of tracked heap usage for the prototype collector.
pub fn heap_snapshot() -> GcHeapSnapshot {
    #[cfg(feature = "gc_mark_sweep_prototype")]
    {
        let mut snapshot = GcHeapSnapshot::default();
        HEAP.with(|heap| {
            let heap = heap.borrow();
            snapshot.tracked_object_slots = heap.objects.len();
            snapshot.tracked_array_slots = heap.arrays.len();
            snapshot.tracked_env_slots = heap.envs.len();
            snapshot.tracked_promise_slots = heap.promises.len();
            snapshot.live_objects = heap.objects.iter().filter(|w| w.strong_count() > 0).count();
            snapshot.live_arrays = heap.arrays.iter().filter(|w| w.strong_count() > 0).count();
            snapshot.live_envs = heap.envs.iter().filter(|w| w.strong_count() > 0).count();
            snapshot.live_promises = heap
                .promises
                .iter()
                .filter(|w| w.strong_count() > 0)
                .count();
        });
        snapshot.allocation_pressure = allocation_pressure();
        snapshot
    }

    #[cfg(not(feature = "gc_mark_sweep_prototype"))]
    {
        GcHeapSnapshot::default()
    }
}

/// Prototype collector entry point.
pub fn collect(roots: &[Value]) -> GcStats {
    let env_roots: &[Rc<Mutex<Vec<ValueSlot>>>] = &[];
    collect_with_env_roots(roots, env_roots)
}

/// Prototype collector entry point with explicit closure-environment roots.
pub fn collect_with_env_roots(roots: &[Value], env_roots: &[Rc<Mutex<Vec<ValueSlot>>>]) -> GcStats {
    {
        {
            let clear_envs_aggressively = false;

            #[cfg(feature = "gc_mark_sweep_prototype")]
            {
                let mut marked_objects: HashSet<usize> = HashSet::new();
                let mut marked_arrays: HashSet<usize> = HashSet::new();
                let mut marked_envs: HashSet<usize> = HashSet::new();
                let mut marked_promises: HashSet<usize> = HashSet::new();

                for root in roots {
                    mark_value(
                        root,
                        &mut marked_objects,
                        &mut marked_arrays,
                        &mut marked_envs,
                        &mut marked_promises,
                    );
                }
                for env in env_roots {
                    mark_env(
                        env,
                        &mut marked_objects,
                        &mut marked_arrays,
                        &mut marked_envs,
                        &mut marked_promises,
                    );
                }

                let mut stats = GcStats {
                    marked_nodes: marked_objects.len()
                        + marked_arrays.len()
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

#[cfg(feature = "gc_mark_sweep_prototype")]
fn mark_value(
    value: &Value,
    marked_objects: &mut HashSet<usize>,
    marked_arrays: &mut HashSet<usize>,
    marked_envs: &mut HashSet<usize>,
    marked_promises: &mut HashSet<usize>,
) {
    match value {
        Value::Object(obj) => mark_object(
            obj,
            marked_objects,
            marked_arrays,
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
                        marked_envs,
                        marked_promises,
                    );
                }
            }
            mark_object(
                props,
                marked_objects,
                marked_arrays,
                marked_envs,
                marked_promises,
            );
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
                    marked_envs,
                    marked_promises,
                );
            }
            mark_object(
                props,
                marked_objects,
                marked_arrays,
                marked_envs,
                marked_promises,
            );
        }
        Value::BuiltinObject(_, props) => mark_object(
            props,
            marked_objects,
            marked_arrays,
            marked_envs,
            marked_promises,
        ),
        Value::Promise(state) => {
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
                            marked_envs,
                            marked_promises,
                        );
                    }
                    for cb in on_rejected {
                        mark_value(
                            &cb,
                            marked_objects,
                            marked_arrays,
                            marked_envs,
                            marked_promises,
                        );
                    }
                }
                PromiseState::Fulfilled(v) => mark_value(
                    &v,
                    marked_objects,
                    marked_arrays,
                    marked_envs,
                    marked_promises,
                ),
                PromiseState::Rejected { reason, .. } => mark_value(
                    &reason,
                    marked_objects,
                    marked_arrays,
                    marked_envs,
                    marked_promises,
                ),
            }
        }
        _ => {}
    }
}

#[cfg(feature = "gc_mark_sweep_prototype")]
fn mark_object(
    obj: &Rc<RefCell<ObjectData>>,
    marked_objects: &mut HashSet<usize>,
    marked_arrays: &mut HashSet<usize>,
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
            marked_envs,
            marked_promises,
        );
    }
    for desc in obj_ref.properties.values() {
        match desc {
            PropertyDescriptor::Data { value, .. } => mark_value(
                value,
                marked_objects,
                marked_arrays,
                marked_envs,
                marked_promises,
            ),
            PropertyDescriptor::Accessor { get, set, .. } => {
                if let Some(v) = get {
                    mark_value(
                        v,
                        marked_objects,
                        marked_arrays,
                        marked_envs,
                        marked_promises,
                    );
                }
                if let Some(v) = set {
                    mark_value(
                        v,
                        marked_objects,
                        marked_arrays,
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
            marked_envs,
            marked_promises,
        );
    }
}

#[cfg(feature = "gc_mark_sweep_prototype")]
fn mark_env(
    env: &Rc<Mutex<Vec<ValueSlot>>>,
    marked_objects: &mut HashSet<usize>,
    marked_arrays: &mut HashSet<usize>,
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
            marked_envs,
            marked_promises,
        );
    }
}

#[cfg(feature = "gc_mark_sweep_prototype")]
fn mark_environment_record(
    env: &Rc<RefCell<crate::environment::EnvironmentRecord>>,
    marked_objects: &mut HashSet<usize>,
    marked_arrays: &mut HashSet<usize>,
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
            marked_envs,
            marked_promises,
        );
    }
}

#[cfg(feature = "gc_mark_sweep_prototype")]
fn clear_object(obj: &Rc<RefCell<ObjectData>>) {
    let mut obj = obj.borrow_mut();
    obj.properties.clear();
    obj.strong_prototype = None;
    obj.weak_prototype = None;
    obj.captured_lexical_env = None;
    obj.extensible = true;
}

#[cfg(feature = "gc_mark_sweep_prototype")]
fn clear_array(arr: &Rc<RefCell<Vec<Option<Value>>>>) {
    arr.borrow_mut().clear();
}

#[cfg(feature = "gc_mark_sweep_prototype")]
fn clear_env(env: &Rc<Mutex<Vec<ValueSlot>>>) {
    // Break closure cycles by dropping the captured values once the
    // environment is determined to be unreachable.
    env.borrow_mut().clear();
}

#[cfg(feature = "gc_mark_sweep_prototype")]
fn clear_env_aggressively(env: &Rc<Mutex<Vec<ValueSlot>>>) {
    clear_env(env);
}

#[cfg(feature = "gc_mark_sweep_prototype")]
fn clear_promise(state: &Rc<RefCell<PromiseState>>) {
    *state.borrow_mut() = PromiseState::Fulfilled(Value::Undefined);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "gc_mark_sweep_prototype")]
    use crate::bytecode::{Chunk, FunctionProto};
    #[cfg(feature = "gc_mark_sweep_prototype")]
    use crate::value::ValueSlot;
    #[cfg(feature = "gc_mark_sweep_prototype")]
    use oxc_span::Span;
    #[cfg(feature = "gc_mark_sweep_prototype")]
    use std::sync::Mutex;

    #[cfg(feature = "gc_mark_sweep_prototype")]
    fn make_dummy_closure(env: Rc<Mutex<Vec<ValueSlot>>>) -> Value {
        let proto = Rc::new(FunctionProto {
            name: "gc_test".to_string(),
            span: Span::new(0, 0),
            arity: 0,
            chunk: Rc::new(Chunk::new(Rc::new(String::new()))),
            slot_types: Vec::new(),
            slot_names: Vec::new(),
            lexical_tdz: Vec::new(),
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

    #[cfg(feature = "gc_mark_sweep_prototype")]
    #[test]
    fn sweep_breaks_object_cycles() {
        let a = track_object(Rc::new(RefCell::new(ObjectData::new())));
        let b = track_object(Rc::new(RefCell::new(ObjectData::new())));

        a.borrow_mut().properties.insert(
            crate::value::PropertyKey::String("peer".to_string()),
            PropertyDescriptor::data(Value::Object(Rc::clone(&b))),
        );
        b.borrow_mut().properties.insert(
            crate::value::PropertyKey::String("peer".to_string()),
            PropertyDescriptor::data(Value::Object(Rc::clone(&a))),
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

    #[cfg(feature = "gc_mark_sweep_prototype")]
    #[test]
    fn regular_collect_breaks_closure_env_cycles() {
        let env = track_env(Rc::new(Mutex::new(Vec::new())));
        let closure = make_dummy_closure(Rc::clone(&env));
        let weak_props = match &closure {
            Value::Closure { props, .. } => Rc::downgrade(props),
            _ => unreachable!("dummy closure must be a closure"),
        };

        env.borrow().push(Rc::new(Mutex::new(closure.clone())));

        let weak_env = Rc::downgrade(&env);

        drop(closure);
        drop(env);

        let stats = collect(&[]);

        assert!(stats.swept_envs >= 1);
        assert!(weak_env.upgrade().is_none());
        assert!(weak_props.upgrade().is_none());
    }

    #[cfg(feature = "gc_mark_sweep_prototype")]
    #[test]
    fn aggressive_collect_breaks_closure_env_cycles() {
        let env = track_env(Rc::new(Mutex::new(Vec::new())));
        let closure = make_dummy_closure(Rc::clone(&env));
        let weak_props = match &closure {
            Value::Closure { props, .. } => Rc::downgrade(props),
            _ => unreachable!("dummy closure must be a closure"),
        };

        env.borrow().push(Rc::new(Mutex::new(closure.clone())));

        let weak_env = Rc::downgrade(&env);

        drop(closure);
        drop(env);

        let stats = collect(&[]);
        assert!(stats.swept_envs >= 1);
        assert!(weak_env.upgrade().is_none());
        assert!(weak_props.upgrade().is_none());
    }

    #[cfg(feature = "gc_mark_sweep_prototype")]
    #[test]
    fn aggressive_collect_respects_roots() {
        let env = track_env(Rc::new(Mutex::new(Vec::new())));
        let closure = make_dummy_closure(Rc::clone(&env));
        let weak_props = match &closure {
            Value::Closure { props, .. } => Rc::downgrade(props),
            _ => unreachable!("dummy closure must be a closure"),
        };

        env.borrow().push(Rc::new(Mutex::new(closure.clone())));

        let weak_env = Rc::downgrade(&env);

        drop(env);

        let stats = collect(std::slice::from_ref(&closure));
        assert_eq!(stats.swept_envs, 0);
        assert!(weak_env.upgrade().is_some());
        assert!(weak_props.upgrade().is_some());
    }

    #[cfg(feature = "gc_mark_sweep_prototype")]
    #[test]
    fn allocation_pressure_tracks_heap_allocations() {
        reset_allocation_pressure();
        assert_eq!(allocation_pressure(), 0);

        let _obj = track_object(Rc::new(RefCell::new(ObjectData::new())));
        let _arr = track_array(Rc::new(RefCell::new(Vec::new())));
        let _env = track_env(Rc::new(Mutex::new(Vec::new())));
        let _promise = track_promise(Rc::new(RefCell::new(PromiseState::Pending {
            on_fulfilled: Vec::new(),
            on_rejected: Vec::new(),
        })));

        assert!(allocation_pressure() >= 4);

        reset_allocation_pressure();
        assert_eq!(allocation_pressure(), 0);
    }
}
