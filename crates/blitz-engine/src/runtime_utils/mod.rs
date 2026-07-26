// SPDX-License-Identifier: MPL-2.0
//! Utility functions for runtime operations.
//!
//! Notes on ECMAScript conformance
//!
//! This module contains a number of pragmatic, simplified, or host-specific
//! implementations of ECMAScript semantics used by the VM. The implementations
//! in this file are intentionally pragmatic (smaller and easier to test) and
//! therefore diverge from ECMA-262 in several observable ways. Call sites
//! should treat the behaviour here as engine-specific unless the code is
//! adjusted to follow the spec precisely.
//!
//! Known divergences (summary):
//! - Module namespace objects are ordinary non-extensible objects with sorted
//!   accessors backed by live export slots, rather than the spec-defined exotic
//!   namespace object kind. See `ModuleLoader` in `crate::module` for details.
//! - Dynamic `import()` performs graph loading and evaluation synchronously,
//!   then queues Promise settlement on the dynamic-import job queue. Import
//!   options are evaluated but otherwise ignored.
//! - Promise/thenable resolution reads a thenable's `then` property
//!   synchronously, then queues the `then` call and settled reactions on the
//!   VM's microtask queue. This preserves more observable job ordering than
//!   direct invocation, but remains engine-specific.
//! - Microtask/job queue draining is host-specific: thrown errors are recorded
//!   and reported through the host hook while the current implementation
//!   continues draining the remaining jobs.
//! - Some Array prototype methods are simplified and differ from spec. For
//!   example, `Array.prototype.flat` flattens nested arrays by one level even
//!   when a greater depth is requested.
//! - Ordinary objects and arrays use ECMAScript own-key ordering, but several
//!   exotic object kinds still share ordinary storage instead of defining
//!   their own complete internal-method behavior.
//! - TypeScript slot-type runtime checks are a non-standard extension: the
//!   VM can optionally enforce compile-time slot types at runtime; this is
//!   not part of ECMA-262.
//!
//! Where behaviour is observable by user code (module namespace live bindings,
//! promise/microtask ordering, array semantics, property key order), the
//! implementation is considered non-compliant with ECMA-262 and should be
//! updated if strict conformance is required. See the project notes and
//! module-level comments in `crate::module`, `crate::vm::runtime::async_tasks`,
//! and `crate::vm::state` for targeted recommendations.

use std::cell::{RefCell, RefMut};
use std::collections::{HashMap, HashSet};
use std::env;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use crate::compiler;
use crate::ecmascript::semantics::byte_views::{
  ARRAY_BUFFER_DATA_SLOT, ARRAY_BUFFER_SHARED_SLOT, TYPED_ARRAY_BUFFER_SLOT,
  TYPED_ARRAY_BYTE_LENGTH_SLOT, TYPED_ARRAY_BYTE_OFFSET_SLOT, TYPED_ARRAY_DATA_SLOT,
  TYPED_ARRAY_KIND_SLOT, TYPED_ARRAY_LENGTH_SLOT, array_buffer_backing, array_buffer_is_shared,
  byte_view_details, is_array_buffer,
};
use crate::gc;
use crate::parser::Parser;
use crate::realm::Intrinsics;
use crate::value::SymbolId;
use crate::value::{
  ArrayData, BuiltinFn, ObjectData, PromiseState, PropertyDescriptor, PropertyKey, Value,
};
use crate::vm::Vm;
use crate::{RuntimeError, bytecode};
use num_traits::{FromPrimitive, ToPrimitive};

mod atomics;
mod numbers; // kept as a thin compatibility re-export; moved implementation into ecmascript::semantics
mod promises;
mod shadowrealm;
mod uri; // kept as thin compatibility re-export; implementation moved

pub(crate) use atomics::*;
// Re-export semantics implementations from ecmascript::semantics to preserve existing imports.
pub(crate) use crate::ecmascript::semantics::numbers::*;
pub(crate) use crate::ecmascript::semantics::properties::*;
pub(crate) use crate::ecmascript::semantics::uri::*;
pub use promises::install_minimal_kernel_constructors;
pub(crate) use promises::*;
pub(crate) use shadowrealm::*;

/// Sets the "length" property of a builtin function or object.
pub(crate) fn set_builtin_length(
  target: &Value,
  length: f64,
) {
  crate::ecmascript::semantics::set_builtin_length(target, length)
}

/// Sets the "name" property of a builtin function or object.
pub(crate) fn set_builtin_name(
  target: &Value,
  name: &str,
) {
  crate::ecmascript::semantics::set_builtin_name(target, name)
}

/// The maximum array length as a floating-point value.
const MAX_ARRAY_LENGTH_F: f64 = 4294967295.0_f64;

/// Clears the slot types in a chunk and its nested function chunks.
pub fn clear_slot_types_in_chunk(chunk: bytecode::Chunk) -> bytecode::Chunk {
  // Recursive helper that rebuilds chunks and their nested function chunks.
  fn rebuild(
    rc_chunk: &Arc<bytecode::Chunk>,
    visited: &mut HashMap<*const bytecode::Chunk, Arc<bytecode::Chunk>>,
  ) -> Arc<bytecode::Chunk> {
    let key = Arc::as_ptr(rc_chunk);
    if let Some(existing) = visited.get(&key) {
      return Arc::clone(existing);
    }

    // Clone the complete chunk so bytecode metadata is preserved as the format
    // evolves, then replace only the annotations and nested function chunks.
    let mut new_chunk = (**rc_chunk).clone();
    new_chunk.root_slot_types = rc_chunk
      .root_slot_types
      .iter()
      .map(|_| None)
      .collect();
    // Slot/global type annotations removed in JS-first mode; nothing to do.

    // Process functions
    let mut new_funcs = Vec::new();
    for f in &rc_chunk.functions {
      // Rebuild nested chunk for this function
      let nested = rebuild(&f.chunk, visited);
      let mut nf = f.clone();
      // Replace all slot type annotations and return type with None
      nf.slot_types = nf.slot_types.iter().map(|_| None).collect();
      nf.return_type = None;
      nf.chunk = nested;
      new_funcs.push(nf);
    }
    new_chunk.functions = new_funcs;

    let rc_new = Arc::new(new_chunk);
    visited.insert(key, Arc::clone(&rc_new));
    rc_new
  }

  let rc = Arc::new(chunk);
  let mut visited: HashMap<*const bytecode::Chunk, Arc<bytecode::Chunk>> = HashMap::new();
  let rc_cleared = rebuild(&rc, &mut visited);
  Arc::try_unwrap(rc_cleared).unwrap_or_else(|rc| (*rc).clone())
}

/// Determines if two values are strictly equal according to ES6 semantics.
pub(crate) fn strict_equal(
  a: &Value,
  b: &Value,
) -> bool {
  match (a, b) {
    (Value::Undefined, Value::Undefined) => true,
    (Value::Null, Value::Null) => true,
    (Value::Bool(x), Value::Bool(y)) => x == y,
    (Value::String(x), Value::String(y)) => x == y,
    (Value::Number(x), Value::Number(y)) => x == y,
    (Value::BigInt(x), Value::BigInt(y)) => x == y,
    (Value::Symbol(x, _), Value::Symbol(y, _)) => x == y,
    (Value::Object(ax), Value::Object(bx)) => Rc::ptr_eq(ax, bx),
    (Value::Array(ax, _), Value::Array(bx, _)) => Rc::ptr_eq(ax, bx),
    (Value::Proxy(ax), Value::Proxy(bx)) => Rc::ptr_eq(ax, bx),
    (Value::Closure { props: ap, .. }, Value::Closure { props: bp, .. }) => Rc::ptr_eq(ap, bp),
    (Value::BuiltinObject(_, ap), Value::BuiltinObject(_, bp)) => Rc::ptr_eq(ap, bp),
    (Value::Promise(ap), Value::Promise(bp)) => Rc::ptr_eq(ap, bp),
    (Value::Builtin(af), Value::Builtin(bf)) => Rc::ptr_eq(af, bf),
    _ => false,
  }
}

/// Determines if two values are equal using SameValueZero semantics (for use in collection operations).
pub(crate) fn same_value_zero(
  a: &Value,
  b: &Value,
) -> bool {
  match (a, b) {
    (Value::Number(x), Value::Number(y)) => x == y || (x.is_nan() && y.is_nan()),
    _ => strict_equal(a, b),
  }
}

/// Computes the argument offset for static builtin functions based on current call context.
pub(crate) fn static_builtin_arg_offset(
  vm: &Vm,
  args: &[Value],
  global_name: &str,
) -> usize {
  let has_new_target = vm.current_new_target().is_some();
  let prepended_this = vm.builtin_this_arg_prepended();
  let offset = if has_new_target || prepended_this {
    1
  } else {
    0
  };
  if crate::trace::enabled() && global_name == "Object" {
    let first_type = args.first().map_or("<none>", Value::type_name);
    eprintln!(
      "[trace] static_builtin_arg_offset Object -> {} (new_target={}, prepended_this={}, args_len={}, arg0_type={})",
      offset,
      has_new_target,
      prepended_this,
      args.len(),
      first_type
    );
  }
  offset
}

/// Converts a value to a length property value, returning a non-negative integer index.
pub(crate) fn to_length(
  vm: &mut Vm,
  value: &Value,
) -> Result<usize, RuntimeError> {
  let num = vm.to_number_value(value)?;
  if num.is_nan() || num <= 0.0_f64 {
    return Ok(0);
  }
  if !num.is_finite() {
    return Err(vm.throw_range_error("Invalid array length"));
  }
  let int_len = num.trunc();
  if int_len > MAX_ARRAY_LENGTH_F {
    return Err(vm.throw_range_error("Invalid array length"));
  }

  int_len
    .to_usize()
    .ok_or_else(|| vm.throw_range_error("Invalid array length"))
}

/// Converts a value to an array index, returning a non-negative integer index.
pub(crate) fn to_index(
  vm: &mut Vm,
  value: &Value,
) -> Result<usize, RuntimeError> {
  let num = vm.to_number_value(value)?;
  if num.is_nan() {
    return Ok(0);
  }
  if !num.is_finite() || num < 0.0_f64 {
    return Err(vm.throw_range_error("Invalid array index"));
  }
  let int_val = num.trunc();
  if int_val > MAX_ARRAY_LENGTH_F {
    return Err(vm.throw_range_error("Invalid array index"));
  }
  int_val
    .to_usize()
    .ok_or_else(|| vm.throw_range_error("Invalid array index"))
}

/// Converts a value to an integer index using ECMAScript `ToIntegerOrInfinity`
/// semantics, returning an `isize` so callers can apply their own bounds checks.
pub(crate) fn to_integer_or_infinity(
  vm: &mut Vm,
  value: &Value,
) -> Result<isize, RuntimeError> {
  let num = vm.to_number_value(value)?;
  if num.is_nan() || num == 0.0 {
    return Ok(0);
  }
  if num.is_infinite() {
    return Err(vm.throw_range_error("Invalid array index"));
  }
  let int_val = num.trunc();
  if !int_val.is_finite() {
    return Err(vm.throw_range_error("Invalid array index"));
  }
  int_val
    .to_isize()
    .ok_or_else(|| vm.throw_range_error("Invalid array index"))
}

/// Indicates the type of value handled by Atomics operations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AtomicsValueKind {
  /// A numeric value with specified bit width and signedness.
  Number {
    /// The bit width of the numeric value (8, 16, or 32).
    bits: u32,
    /// Whether the numeric value is signed.
    signed: bool,
  },
  /// A BigInt value with specified signedness.
  BigInt {
    /// Whether the BigInt value is signed.
    signed: bool,
  },
}

/// Information about a TypedArray view for Atomics operations.
#[derive(Clone)]
pub(crate) struct AtomicsViewInfo {
  /// The TypedArray view object.
  pub(crate) view: Rc<RefCell<ObjectData>>,
  /// The underlying array data.
  pub(crate) data: Rc<RefCell<ArrayData>>,
  /// The underlying ArrayBuffer or SharedArrayBuffer.
  pub(crate) buffer: Rc<RefCell<ObjectData>>,
  /// The name of the kind of view (e.g., "Uint8Array").
  pub(crate) kind_name: String,
  /// The value kind for this view.
  pub(crate) value_kind: AtomicsValueKind,
  /// The number of elements in the view.
  pub(crate) length: usize,
  /// Whether the buffer is shared (SharedArrayBuffer).
  pub(crate) is_shared: bool,
}

/// Creates a syntax error exception with the given message.
pub(crate) fn syntax_error(
  vm: &mut Vm,
  message: String,
) -> RuntimeError {
  let err_val = host_error_object(vm, "SyntaxError", &message);
  vm.throw_exception(err_val)
}

/// Creates an error object for a given error type and message.
pub(crate) fn host_error_object(
  vm: &mut Vm,
  name: &str,
  message: &str,
) -> Value {
  let mut obj = ObjectData::new();
  let proto = vm
    .realm()
    .get_intrinsic_constructor(name)
    .and_then(|ctor| {
      vm.get_property_host(&ctor, &PropertyKey::String("prototype".to_string().into()))
        .ok()
    })
    .or_else(|| vm.realm().intrinsics.error_prototype.clone());
  if let Some(proto) = proto {
    obj.set_prototype_value(proto);
  }
  obj.properties.insert(
    PropertyKey::String("name".to_string().into()),
    PropertyDescriptor::ordinary_data(Value::String(name.to_string().into())),
  );
  obj.properties.insert(
    PropertyKey::String("message".to_string().into()),
    PropertyDescriptor::ordinary_data(Value::String(message.to_string().into())),
  );
  Value::Object(gc::track_object(Rc::new(RefCell::new(obj))))
}

/// Parses a string as a canonical array index, returning the numeric index if valid.
pub(crate) fn canonical_index_key(s: &js_string::JsString) -> Option<u64> {
  if s.is_empty() {
    return None;
  }
  if s != "0" && s.code_unit_at(0) == Some(u16::from(b'0')) {
    return None;
  }
  let idx = s.parse::<u64>().ok()?;
  if !decimal_u64_eq_str(idx, s.as_str_lossy()) {
    return None;
  }
  Some(idx)
}

/// Structured-clone helper for cross-realm value transfers used by ShadowRealm
/// operations. Implements copying semantics for primitives, Array, Object,
/// Date, RegExp, Map, Set, ArrayBuffer, and TypedArray-shaped objects used by
/// this runtime. Preserves cycles/shared references by using a visited map and
/// uses runtime prototypes where the clone path provides them. Returns
/// `Err(String)` for uncloneable values (functions, promises, symbols,
/// accessors, etc.).
fn clone_value_across_realms(
  vm: &mut Vm,
  value: &Value,
  transfer_set: Option<&HashSet<usize>>,
) -> Result<Value, String> {
  use std::collections::HashMap;

  // visited maps source Rc pointer addresses to already-created clone Values
  // so we can preserve cycles and shared references.
  fn helper(
    vm: &mut Vm,
    v: &Value,
    visited: &mut HashMap<usize, Value>,
    transfer_set: Option<&HashSet<usize>>,
  ) -> Result<Value, String> {
    match v {
      Value::Undefined => Ok(Value::Undefined),
      Value::Null => Ok(Value::Null),
      Value::Bool(b) => Ok(Value::Bool(*b)),
      Value::Number(n) => Ok(Value::Number(*n)),
      Value::String(s) => Ok(Value::String(s.clone())),
      Value::BigInt(bi) => Ok(Value::BigInt(bi.clone())),
      Value::Symbol(_, _) => Err("Cannot clone Symbol across realms".to_string()),
      Value::Promise(_) => Err("Cannot clone Promise across realms".to_string()),
      Value::Proxy(_) => Err("Cannot clone Proxy across realms".to_string()),
      Value::Builtin(_) | Value::BuiltinObject(_, _) | Value::Closure { .. } => {
        Err("Cannot clone functions/closures across realms".to_string())
      }
      Value::Array(arr_rc, props_rc) => {
        let addr = Rc::as_ptr(arr_rc) as usize;
        if let Some(existing) = visited.get(&addr) {
          return Ok(existing.clone());
        }

        let transferring = transfer_set.is_some_and(|set| set.contains(&addr));
        let property_snapshot = props_rc.borrow().properties.clone();
        let transferred_elements = transferring.then(|| arr_rc.borrow_mut().take_elements());

        // Reserve a placeholder array in visited to handle cycles.
        let len = transferred_elements
          .as_ref()
          .map_or_else(|| arr_rc.borrow().len(), Vec::len);
        let placeholder_vec = transferred_elements.unwrap_or_else(|| vec![None; len]);
        let new_arr = gc::track_array(Rc::new(RefCell::new(ArrayData::from(placeholder_vec))));
        let new_props = gc::track_object(Rc::new(RefCell::new(ObjectData::new())));
        if let Some(proto) = vm.realm().intrinsics.array_prototype.clone() {
          new_props.borrow_mut().set_prototype_value(proto);
        }
        // insert length property placeholder
        new_props.borrow_mut().properties.insert(
          PropertyKey::String("length".to_string().into()),
          array_length_descriptor(len),
        );
        let new_val = Value::Array(Rc::clone(&new_arr), Rc::clone(&new_props));
        visited.insert(addr, new_val);

        if !transferring {
          // Fill dense slots while preserving cycles and shared references.
          for (i, slot) in arr_rc.borrow().iter().enumerate() {
            if let Some(sv) = slot {
              let cloned = helper(vm, sv, visited, transfer_set)?;
              new_arr.borrow_mut()[i] = Some(cloned);
            }
          }
        } else {
          props_rc.borrow_mut().properties.insert(
            PropertyKey::String("length".to_string().into()),
            array_length_descriptor(0),
          );
        }

        // Clone property bag (data descriptors only)
        for (k, desc) in &property_snapshot {
          match desc {
            PropertyDescriptor::Data { value, .. } => {
              let cloned = helper(vm, value, visited, transfer_set)?;
              new_props
                .borrow_mut()
                .properties
                .insert(k.clone(), PropertyDescriptor::ordinary_data(cloned));
            }
            _ => {
              return Err("Cannot clone accessor properties across realms".to_string());
            }
          }
        }

        Ok(Value::Array(new_arr, new_props))
      }
      Value::Object(obj_rc) => {
        let addr = Rc::as_ptr(obj_rc) as usize;
        if let Some(existing) = visited.get(&addr) {
          return Ok(existing.clone());
        }

        // Detect special internal shapes used by runtime: ArrayBuffer,
        // TypedArray, Map, Set, Date, RegExp by inspecting internal slots
        let borrow = obj_rc.borrow();

        // ArrayBuffer: identified by its VM-only internal slots.
        let array_buffer_data = if is_array_buffer(&Value::Object(Rc::clone(obj_rc))) {
          Some(
            array_buffer_backing(&Value::Object(Rc::clone(obj_rc)))
              .map(|data| Value::Array(data, Rc::new(RefCell::new(ObjectData::new()))))
              .unwrap_or(Value::Null),
          )
        } else {
          None
        };
        let array_buffer_byte_length = borrow
          .properties
          .get(&PropertyKey::String("byteLength".to_string().into()))
          .and_then(|descriptor| match descriptor {
            PropertyDescriptor::Data { value, .. } => Some(value.clone()),
            PropertyDescriptor::Accessor { .. } => None,
          });
        if let Some(ab_data) = array_buffer_data {
          let transferring = matches!(
            &ab_data,
            Value::Array(backing, _)
              if transfer_set.is_some_and(|set| set.contains(&(Rc::as_ptr(backing) as usize)))
          );
          drop(borrow);

          // Create a placeholder object with the runtime's Array prototype.
          let mut out = ObjectData::new();
          if let Some(proto) = vm.realm().get_intrinsic_prototype("Array") {
            out.set_prototype_value(proto);
          }
          let target_obj = gc::track_object(Rc::new(RefCell::new(out)));
          let target_val = Value::Object(Rc::clone(&target_obj));
          visited.insert(addr, target_val);

          let cloned_data = helper(vm, &ab_data, visited, transfer_set)?;
          target_obj
            .borrow_mut()
            .set_internal_slot(ARRAY_BUFFER_DATA_SLOT, cloned_data);
          target_obj.borrow_mut().set_internal_slot(
            ARRAY_BUFFER_SHARED_SLOT,
            Value::Bool(array_buffer_is_shared(&Value::Object(Rc::clone(obj_rc))).unwrap_or(false)),
          );
          if let Some(value) = array_buffer_byte_length {
            let bl = helper(vm, &value, visited, transfer_set)?;
            target_obj.borrow_mut().properties.insert(
              PropertyKey::String("byteLength".to_string().into()),
              PropertyDescriptor::ordinary_data(bl),
            );
          }
          if transferring {
            obj_rc
              .borrow_mut()
              .set_internal_slot(ARRAY_BUFFER_DATA_SLOT, Value::Null);
            if let Some(PropertyDescriptor::Data { value, .. }) = obj_rc
              .borrow_mut()
              .properties
              .get_mut(&PropertyKey::String("byteLength".to_string().into()))
            {
              *value = Value::Number(0.0);
            }
          }
          return Ok(Value::Object(target_obj));
        }

        // Byte TypedArray: use the VM-only slots so a spoofed property bag is
        // never treated as a view during cloning.
        if let Some(details) = byte_view_details(&Value::Object(Rc::clone(obj_rc))) {
          let mut out = ObjectData::new();
          if let Some(proto) = vm.realm().get_intrinsic_prototype("Array") {
            out.set_prototype_value(proto);
          }
          let target_obj = gc::track_object(Rc::new(RefCell::new(out)));
          let target_val = Value::Object(Rc::clone(&target_obj));
          visited.insert(addr, target_val);

          let cloned_data = helper(
            vm,
            &Value::Array(
              Rc::clone(&details.data),
              Rc::new(RefCell::new(ObjectData::new())),
            ),
            visited,
            transfer_set,
          )?;
          let cloned_buffer = helper(vm, &details.buffer, visited, transfer_set)?;
          let mut target = target_obj.borrow_mut();
          target.set_internal_slot(TYPED_ARRAY_DATA_SLOT, cloned_data);
          target.set_internal_slot(
            TYPED_ARRAY_KIND_SLOT,
            Value::String(details.kind.clone().into()),
          );
          target.set_internal_slot(
            TYPED_ARRAY_LENGTH_SLOT,
            Value::Number(details.length as f64),
          );
          target.set_internal_slot(
            TYPED_ARRAY_BYTE_LENGTH_SLOT,
            Value::Number(details.length as f64),
          );
          target.set_internal_slot(
            TYPED_ARRAY_BYTE_OFFSET_SLOT,
            Value::Number(details.byte_offset as f64),
          );
          target.set_internal_slot(TYPED_ARRAY_BUFFER_SLOT, cloned_buffer.clone());
          target.properties.insert(
            PropertyKey::String("length".to_string().into()),
            PropertyDescriptor::ordinary_data(Value::Number(details.length as f64)),
          );
          target.properties.insert(
            PropertyKey::String("byteLength".to_string().into()),
            PropertyDescriptor::ordinary_data(Value::Number(details.length as f64)),
          );
          target.properties.insert(
            PropertyKey::String("byteOffset".to_string().into()),
            PropertyDescriptor::ordinary_data(Value::Number(details.byte_offset as f64)),
          );
          target.properties.insert(
            PropertyKey::String("buffer".to_string().into()),
            PropertyDescriptor::ordinary_data(cloned_buffer),
          );
          drop(target);
          return Ok(Value::Object(target_obj));
        }

        // Wider TypedArrays retain their existing representation for now.
        if let Some(PropertyDescriptor::Data { value: ta_data, .. }) = borrow.properties.get(
          &PropertyKey::String("[[TypedArrayData]]".to_string().into()),
        ) && !matches!(
          borrow
            .properties
            .get(&PropertyKey::String("[[TypedArrayKind]]".to_string().into())),
          Some(PropertyDescriptor::Data {
            value: Value::String(kind),
            ..
          }) if matches!(
            kind.as_str_lossy(),
            "Uint8Array" | "Int8Array" | "Uint8ClampedArray"
          )
        ) {
          let mut out = ObjectData::new();
          if let Some(proto) = vm.realm().get_intrinsic_prototype("Array") {
            // This clone path uses Array.prototype rather than restoring the
            // typed-array-specific prototype.
            out.set_prototype_value(proto);
          }
          let target_obj = gc::track_object(Rc::new(RefCell::new(out)));
          let target_val = Value::Object(Rc::clone(&target_obj));
          visited.insert(addr, target_val);

          let cloned_ta = helper(vm, ta_data, visited, transfer_set)?;
          target_obj.borrow_mut().properties.insert(
            PropertyKey::String("[[TypedArrayData]]".to_string().into()),
            PropertyDescriptor::ordinary_data(cloned_ta),
          );
          // copy common numeric properties (length, byteLength)
          for key_name in &["length", "byteLength"] {
            if let Some(PropertyDescriptor::Data { value, .. }) = borrow
              .properties
              .get(&PropertyKey::String(key_name.to_string().into()))
            {
              let cloned = helper(vm, value, visited, transfer_set)?;
              target_obj.borrow_mut().properties.insert(
                PropertyKey::String(key_name.to_string().into()),
                if key_name == &"length" {
                  PropertyDescriptor::Data {
                    value: cloned,
                    writable: true,
                    enumerable: false,
                    configurable: false,
                  }
                } else {
                  PropertyDescriptor::ordinary_data(cloned)
                },
              );
            }
          }
          // copy numeric indexed properties if present
          for (k, desc) in &borrow.properties {
            if let PropertyKey::String(s) = k
              && canonical_index_key(s).is_some()
            {
              if let PropertyDescriptor::Data { value, .. } = desc {
                let cloned = helper(vm, value, visited, transfer_set)?;
                target_obj.borrow_mut().properties.insert(
                  PropertyKey::String(s.clone()),
                  PropertyDescriptor::ordinary_data(cloned),
                );
              } else {
                return Err("Cannot clone accessor properties across realms".to_string());
              }
            }
          }
          return Ok(Value::Object(target_obj));
        }

        // Map: preserve its VM-internal data slot without exposing it as a property.
        if let Some(entries) = borrow
          .internal_slots
          .get(crate::ecmascript::builtins::collections::MAP_DATA_SLOT)
        {
          let mut out = ObjectData::new();
          if let Some(proto) = vm.realm().get_intrinsic_prototype("Map") {
            out.set_prototype_value(proto);
          }
          let target_obj = gc::track_object(Rc::new(RefCell::new(out)));
          let target_val = Value::Object(Rc::clone(&target_obj));
          visited.insert(addr, target_val);

          let cloned_entries = helper(vm, entries, visited, transfer_set)?;
          target_obj.borrow_mut().internal_slots.insert(
            crate::ecmascript::builtins::collections::MAP_DATA_SLOT,
            cloned_entries,
          );
          return Ok(Value::Object(target_obj));
        }

        // Set: preserve its VM-internal data slot without exposing it as a property.
        if let Some(set_vals) = borrow
          .internal_slots
          .get(crate::ecmascript::builtins::collections::SET_DATA_SLOT)
        {
          let mut out = ObjectData::new();
          if let Some(proto) = vm.realm().get_intrinsic_prototype("Set") {
            out.set_prototype_value(proto);
          }
          let target_obj = gc::track_object(Rc::new(RefCell::new(out)));
          let target_val = Value::Object(Rc::clone(&target_obj));
          visited.insert(addr, target_val);

          let cloned_vals = helper(vm, set_vals, visited, transfer_set)?;
          target_obj.borrow_mut().internal_slots.insert(
            crate::ecmascript::builtins::collections::SET_DATA_SLOT,
            cloned_vals,
          );
          return Ok(Value::Object(target_obj));
        }

        // Date: look for [[PrimitiveValue]] internal slot
        if let Some(PropertyDescriptor::Data { value: prim, .. }) = borrow.properties.get(
          &PropertyKey::String("[[PrimitiveValue]]".to_string().into()),
        ) {
          let mut out = ObjectData::new();
          if let Some(proto) = vm.realm().get_intrinsic_prototype("Date") {
            out.set_prototype_value(proto);
          }
          let target_obj = gc::track_object(Rc::new(RefCell::new(out)));
          let target_val = Value::Object(Rc::clone(&target_obj));
          visited.insert(addr, target_val);

          let cloned_prim = helper(vm, prim, visited, transfer_set)?;
          target_obj.borrow_mut().properties.insert(
            PropertyKey::String("[[PrimitiveValue]]".to_string().into()),
            PropertyDescriptor::ordinary_data(cloned_prim),
          );
          return Ok(Value::Object(target_obj));
        }

        // RegExp: look for 'source' and 'flags' properties
        if borrow
          .properties
          .contains_key(&PropertyKey::String("source".to_string().into()))
          && borrow
            .properties
            .contains_key(&PropertyKey::String("flags".to_string().into()))
        {
          let mut out = ObjectData::new();
          if let Some(proto) = vm.realm().get_intrinsic_prototype("Object") {
            out.set_prototype_value(proto);
          }
          let target_obj = gc::track_object(Rc::new(RefCell::new(out)));
          let target_val = Value::Object(Rc::clone(&target_obj));
          visited.insert(addr, target_val);

          // copy source, flags, lastIndex if present
          for key in ["source", "flags", "lastIndex"] {
            if let Some(PropertyDescriptor::Data { value, .. }) = borrow
              .properties
              .get(&PropertyKey::String(key.to_string().into()))
            {
              let cloned = helper(vm, value, visited, transfer_set)?;
              target_obj.borrow_mut().properties.insert(
                PropertyKey::String(key.to_string().into()),
                PropertyDescriptor::ordinary_data(cloned),
              );
            }
          }
          return Ok(Value::Object(target_obj));
        }

        // Generic object fallback: create placeholder and copy data properties
        let mut out = ObjectData::new();
        if let Some(proto) = vm.realm().intrinsics.object_prototype.clone() {
          out.set_prototype_value(proto);
        }
        let target_obj = gc::track_object(Rc::new(RefCell::new(out)));
        let target_val = Value::Object(Rc::clone(&target_obj));
        visited.insert(addr, target_val);

        for (k, desc) in &borrow.properties {
          match desc {
            PropertyDescriptor::Data { value, .. } => {
              let cloned = helper(vm, value, visited, transfer_set)?;
              target_obj
                .borrow_mut()
                .properties
                .insert(k.clone(), PropertyDescriptor::ordinary_data(cloned));
            }
            _ => {
              return Err("Cannot clone accessor properties across realms".to_string());
            }
          }
        }

        Ok(Value::Object(target_obj))
      }
      Value::Uninitialized => Err("Cannot clone uninitialized value".to_string()),
    }
  }

  let mut visited: HashMap<usize, Value> = HashMap::new();
  helper(vm, value, &mut visited, transfer_set)
}

// - Intrinsics bootstrapping: `install_minimal_kernel_constructors` installs a
//   limited set of constructors and prototype objects sufficient for the
//   runtime prelude. These intrinsics are intentionally minimal and may omit
//   some property attribute details or exotic object behaviors required by the
//   spec.
//
// Promise constructor matching
//
// - `promise_constructor_matches` currently checks the `constructor` property
//   of a promise object for equality with the provided constructor. This is a
//   pragmatic check that may be fragile for embellished prototype chains or
//   host-constructed promise-like objects. Spec-compliant engines use
//   internal brand checks and the Promise constructor's intrinsic behaviour.
//   Consider replacing property-based checks with a stricter internal brand
//   or explicit slot marking on promise state.

#[cfg(test)]
mod structured_clone_tests {
  use super::*;

  #[test]
  fn transfer_moves_array_buffer_storage_and_detaches_source() {
    let globals = Rc::new(RefCell::new(ObjectData::new()));
    let mut vm = Vm::new(globals);
    let backing = gc::track_array(Rc::new(RefCell::new(ArrayData::new(vec![
      Some(Value::Number(1.0)),
      Some(Value::Number(2.0)),
    ]))));
    let backing_props = gc::track_object(Rc::new(RefCell::new(ObjectData::new())));
    backing_props.borrow_mut().properties.insert(
      PropertyKey::String("length".to_string().into()),
      array_length_descriptor(2),
    );
    let backing_value = Value::Array(Rc::clone(&backing), backing_props);

    let source = gc::track_object(Rc::new(RefCell::new(ObjectData::new())));
    source
      .borrow_mut()
      .set_internal_slot(ARRAY_BUFFER_DATA_SLOT, backing_value);
    source
      .borrow_mut()
      .set_internal_slot(ARRAY_BUFFER_SHARED_SLOT, Value::Bool(false));
    source.borrow_mut().properties.insert(
      PropertyKey::String("byteLength".to_string().into()),
      PropertyDescriptor::ordinary_data(Value::Number(2.0)),
    );

    let transfer_set = HashSet::from([Rc::as_ptr(&backing) as usize]);
    let cloned = clone_value_across_realms(
      &mut vm,
      &Value::Object(Rc::clone(&source)),
      Some(&transfer_set),
    )
    .expect("transfer should succeed");

    assert!(backing.borrow().is_empty());
    assert!(matches!(
      source
        .borrow()
        .properties
        .get(&PropertyKey::String("byteLength".to_string().into())),
      Some(PropertyDescriptor::Data {
        value: Value::Number(0.0),
        ..
      })
    ));

    let Value::Object(cloned) = cloned else {
      panic!("transferred ArrayBuffer should remain an object");
    };
    let cloned_backing = cloned
      .borrow()
      .internal_slot(ARRAY_BUFFER_DATA_SLOT)
      .and_then(|value| match value {
        Value::Array(backing, _) => Some(Rc::clone(backing)),
        _ => None,
      })
      .expect("transferred object should contain backing data");
    assert_eq!(cloned_backing.borrow().len(), 2);
  }
}
