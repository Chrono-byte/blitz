//! Experimental mark/sweep GC prototype.
//!
//! This module is intentionally minimal and gated by the
//! `gc_mark_sweep_prototype` feature. It currently performs mark traversal and
//! reports statistics; sweep/reclamation is a no-op placeholder.

use std::collections::HashSet;
use std::rc::Rc;

use crate::value::{ObjectData, PromiseState, PropertyDescriptor, Value};

/// Result of a prototype GC collection cycle.
#[derive(Debug, Clone, Copy, Default)]
pub struct GcStats {
    /// Number of unique object-like nodes marked reachable.
    pub marked_nodes: usize,
}

/// Prototype mark/sweep collector entry point.
pub fn collect(roots: &[Value]) -> GcStats {
    let mut marked: HashSet<usize> = HashSet::new();
    for root in roots {
        mark_value(root, &mut marked);
    }
    GcStats {
        marked_nodes: marked.len(),
    }
}

/// Recursively mark all reachable values from the given root.
fn mark_value(value: &Value, marked: &mut HashSet<usize>) {
    match value {
        Value::Object(obj) => mark_object(obj, marked),
        Value::Array(arr, props) => {
            let ptr = Rc::as_ptr(arr) as usize;
            if marked.insert(ptr) {
                for elem in arr.borrow().iter().flatten() {
                    mark_value(elem, marked);
                }
            }
            mark_object(props, marked);
        }
        Value::Closure {
            env,
            lexical_this,
            props,
            ..
        } => {
            let env_ptr = Rc::as_ptr(env) as usize;
            if marked.insert(env_ptr) {
                for cell in env.borrow().iter() {
                    mark_value(&cell.borrow(), marked);
                }
            }
            if let Some(this_val) = lexical_this {
                mark_value(this_val, marked);
            }
            mark_object(props, marked);
        }
        Value::BuiltinObject(_, props) => mark_object(props, marked),
        Value::Promise(state) => match &*state.borrow() {
            PromiseState::Pending {
                on_fulfilled,
                on_rejected,
            } => {
                for cb in on_fulfilled {
                    mark_value(cb, marked);
                }
                for cb in on_rejected {
                    mark_value(cb, marked);
                }
            }
            PromiseState::Fulfilled(v) | PromiseState::Rejected(v) => mark_value(v, marked),
        },
        _ => {}
    }
}

fn mark_object(obj: &Rc<std::cell::RefCell<ObjectData>>, marked: &mut HashSet<usize>) {
    let ptr = Rc::as_ptr(obj) as usize;
    if !marked.insert(ptr) {
        return;
    }
    let obj_ref = obj.borrow();
    for desc in obj_ref.properties.values() {
        match desc {
            PropertyDescriptor::Data { value, .. } => mark_value(value, marked),
            PropertyDescriptor::Accessor { get, set, .. } => {
                if let Some(v) = get {
                    mark_value(v, marked);
                }
                if let Some(v) = set {
                    mark_value(v, marked);
                }
            }
        }
    }
    if !matches!(obj_ref.prototype, Value::Null) {
        mark_value(&obj_ref.prototype, marked);
    }
}
