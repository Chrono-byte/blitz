//! Experimental garbage collection support.
//!
//! The default build enables a prototype mark/sweep collector that can
//! reclaim cyclic garbage by tracking heap allocations in weak registries.
//! When the feature is disabled, the tracking hooks become no-ops and the
//! collector reports zero work.

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::{Rc, Weak};

use crate::value::{ObjectData, PromiseState, PropertyDescriptor, Value};

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

#[cfg(feature = "gc_mark_sweep_prototype")]
#[derive(Default)]
struct HeapRegistry {
    objects: Vec<Weak<RefCell<ObjectData>>>,
    arrays: Vec<Weak<RefCell<Vec<Option<Value>>>>>,
    envs: Vec<Weak<RefCell<Vec<Rc<RefCell<Value>>>>>>,
    promises: Vec<Weak<RefCell<PromiseState>>>,
}

#[cfg(feature = "gc_mark_sweep_prototype")]
thread_local! {
    static HEAP: RefCell<HeapRegistry> = RefCell::new(HeapRegistry::default());
}

#[cfg(feature = "gc_mark_sweep_prototype")]
fn push_weak<T>(slot: &mut Vec<Weak<T>>, value: &Rc<T>) {
    slot.push(Rc::downgrade(value));
}

/// Register an object property bag with the collector.
pub fn track_object(obj: Rc<RefCell<ObjectData>>) -> Rc<RefCell<ObjectData>> {
    #[cfg(feature = "gc_mark_sweep_prototype")]
    HEAP.with(|heap| push_weak(&mut heap.borrow_mut().objects, &obj));
    obj
}

/// Register an array backing store with the collector.
pub fn track_array(arr: Rc<RefCell<Vec<Option<Value>>>>) -> Rc<RefCell<Vec<Option<Value>>>> {
    #[cfg(feature = "gc_mark_sweep_prototype")]
    HEAP.with(|heap| push_weak(&mut heap.borrow_mut().arrays, &arr));
    arr
}

/// Register a closure environment vector with the collector.
pub fn track_env(
    env: Rc<RefCell<Vec<Rc<RefCell<Value>>>>>,
) -> Rc<RefCell<Vec<Rc<RefCell<Value>>>>> {
    #[cfg(feature = "gc_mark_sweep_prototype")]
    HEAP.with(|heap| push_weak(&mut heap.borrow_mut().envs, &env));
    env
}

/// Register a promise state with the collector.
pub fn track_promise(promise: Rc<RefCell<PromiseState>>) -> Rc<RefCell<PromiseState>> {
    #[cfg(feature = "gc_mark_sweep_prototype")]
    HEAP.with(|heap| push_weak(&mut heap.borrow_mut().promises, &promise));
    promise
}

/// Prototype collector entry point.
pub fn collect(roots: &[Value]) -> GcStats {
    #[cfg(not(feature = "gc_mark_sweep_prototype"))]
    {
        let _ = roots;
        return GcStats::default();
    }

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
                clear_env(&env);
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

        stats
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
                for cell in env.borrow().iter() {
                    mark_value(
                        &cell.borrow(),
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
            match &*state.borrow() {
                PromiseState::Pending {
                    on_fulfilled,
                    on_rejected,
                } => {
                    for cb in on_fulfilled {
                        mark_value(
                            cb,
                            marked_objects,
                            marked_arrays,
                            marked_envs,
                            marked_promises,
                        );
                    }
                    for cb in on_rejected {
                        mark_value(
                            cb,
                            marked_objects,
                            marked_arrays,
                            marked_envs,
                            marked_promises,
                        );
                    }
                }
                PromiseState::Fulfilled(v) | PromiseState::Rejected(v) => mark_value(
                    v,
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
fn clear_object(obj: &Rc<RefCell<ObjectData>>) {
    let mut obj = obj.borrow_mut();
    obj.properties.clear();
    obj.weak_prototype = None;
    obj.extensible = true;
}

#[cfg(feature = "gc_mark_sweep_prototype")]
fn clear_array(arr: &Rc<RefCell<Vec<Option<Value>>>>) {
    arr.borrow_mut().clear();
}

#[cfg(feature = "gc_mark_sweep_prototype")]
fn clear_env(env: &Rc<RefCell<Vec<Rc<RefCell<Value>>>>>) {
    env.borrow_mut().clear();
}

#[cfg(feature = "gc_mark_sweep_prototype")]
fn clear_promise(state: &Rc<RefCell<PromiseState>>) {
    *state.borrow_mut() = PromiseState::Fulfilled(Value::Undefined);
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
