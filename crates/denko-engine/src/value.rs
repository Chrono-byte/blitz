// SPDX-License-Identifier: MPL-2.0
//! Runtime value types for the VM.
//!
//! This module defines the core value types that the VM operates on:
//! - [`Value`] enum: All runtime values (numbers, strings, objects, functions)
//! - [`BuiltinFn`] type: Native function signature
//! - [`RuntimeType`] enum: Runtime type descriptors
//!
//! # Value Types
//!
//! | Variant | Description |
//! |---------|-------------|
//! | `Uninitialized` | Internal temporal-dead-zone sentinel |
//! | `Number` | IEEE 754 double-precision float |
//! | `String` | UTF-8 string |
//! | `Bool` | Boolean value |
//! | `Undefined` | Undefined value |
//! | `BigInt` | Arbitrary-precision integer |
//! | `Symbol` | Symbol identity and optional description |
//! | `Null` | Null value |
//! | `Object` | Property map with prototype chain |
//! | `Array` | Dense array with property bag |
//! | `Proxy` | Proxy target and handler state |
//! | `Closure` | Function with captured environment |
//! | `Builtin` | Native Rust function |
//! | `BuiltinObject` | Native function with object properties |
//! | `Promise` | Promise state and reactions |
//!
//! # Memory Management
//!
//! Objects, arrays, and closures use `Rc<RefCell<...>>` for shared mutable
//! ownership within a single thread. The VM is not thread-safe.
//!
//! Conformance and implementation notes
//!
//! - Prototype representation: ObjectData stores the current prototype as a
//!   strong `Value` and keeps a weak mirror for object prototypes. Callers
//!   should use `get_prototype` / `set_prototype_value` rather than modifying
//!   the storage fields directly.
//!
//! - SameValue vs Strict Equality: the runtime contains several equality
//!   helpers. The semantics of `SameValue`, `SameValueZero`, and strict equality
//!   (===) differ in edge cases (for example, NaN should be SameValue-equal to
//!   itself while strict equality is false for NaN). The runtime provides
//!   explicit `same_value`, `same_value_zero`, and `strict_equal` helpers;
//!   callers must use the variant required by each spec algorithm (for
//!   example, Map/Set keying, `Object.is`, or strict equality).
//!
//! # Example
//!
//! ```
//! use denko::value::Value;
//!
//! let num = Value::Number(42.0);
//! let s = Value::String("hello".to_string().into());
//! assert!(num.is_truthy());
//! assert_eq!(num.type_name(), "number");
//! ```

use indexmap::IndexMap;
use js_string::JsString;
use std::cell::RefCell;
use std::ops::{Deref, DerefMut};
use std::rc::Rc;
use std::rc::Weak;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use crate::binding::BindingCell;

use num_bigint::BigInt as BigIntValue;
use num_traits::Zero;

/// A weak reference to an ObjectData that can be upgraded when needed.
/// This helps prevent reference cycles in prototype chains.
#[derive(Clone)]
pub struct WeakObject {
  weak: Weak<RefCell<ObjectData>>,
}

impl WeakObject {
  /// Create a new weak reference from a strong reference.
  pub fn new(obj: &Rc<RefCell<ObjectData>>) -> Self {
    Self {
      weak: Rc::downgrade(obj),
    }
  }

  /// Create a null weak reference (no target).
  pub fn null() -> Self {
    Self { weak: Weak::new() }
  }

  /// Check if the reference is still valid (hasn't been dropped).
  pub fn is_valid(&self) -> bool {
    self.weak.strong_count() > 0
  }

  /// Try to upgrade to a strong reference.
  /// Returns None if the object has been dropped.
  pub fn upgrade(&self) -> Option<Rc<RefCell<ObjectData>>> {
    self.weak.upgrade()
  }

  /// Get the Value for this weak reference, or Null if expired.
  pub fn to_value(&self) -> Value {
    match self.upgrade() {
      Some(obj) => Value::Object(obj),
      None => Value::Null,
    }
  }
}

impl std::fmt::Debug for WeakObject {
  fn fmt(
    &self,
    f: &mut std::fmt::Formatter<'_>,
  ) -> std::fmt::Result {
    if self.is_valid() {
      write!(f, "WeakObject(valid)")
    } else {
      write!(f, "WeakObject(expired)")
    }
  }
}

/// Symbol primitive identity.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SymbolId(Arc<u64>);

impl SymbolId {
  const PRIVATE_BIT: u64 = 1 << 63;

  /// Generate a new unique symbol id.
  pub fn new() -> Self {
    Self(Arc::new(fastrand::u64(..) & !Self::PRIVATE_BIT))
  }

  /// Generate an engine-private class name that JavaScript cannot create.
  pub(crate) fn new_private() -> Self {
    Self(Arc::new(
      (fastrand::u64(..) & !Self::PRIVATE_BIT) | Self::PRIVATE_BIT,
    ))
  }

  /// Whether this identity denotes an engine-private class name.
  pub(crate) fn is_private(&self) -> bool {
    self.as_u64() & Self::PRIVATE_BIT != 0
  }

  /// Create a well-known symbol id.
  pub fn well_known(id: u64) -> Self {
    Self(Arc::new(id))
  }

  /// Return the raw id.
  pub fn as_u64(&self) -> u64 {
    *self.0
  }

  /// Well-known symbol for @@toPrimitive
  pub fn well_known_to_primitive() -> Self {
    Self::well_known(1)
  }

  /// Well-known symbol for `@@iterator`.
  pub fn well_known_iterator() -> Self {
    Self::well_known(2)
  }
  /// Well-known symbol for `@@asyncIterator`.
  pub fn well_known_async_iterator() -> Self {
    Self::well_known(3)
  }
  /// Well-known symbol for `@@toStringTag`.
  pub fn well_known_to_string_tag() -> Self {
    Self::well_known(4)
  }
  /// Well-known symbol for `@@species`.
  pub fn well_known_species() -> Self {
    Self::well_known(5)
  }
  /// Well-known symbol for `@@hasInstance`.
  pub fn well_known_has_instance() -> Self {
    Self::well_known(6)
  }
  /// Well-known symbol for `@@isConcatSpreadable`.
  pub fn well_known_is_concat_spreadable() -> Self {
    Self::well_known(7)
  }
  /// Well-known symbol for `@@match`.
  pub fn well_known_match() -> Self {
    Self::well_known(8)
  }
  /// Well-known symbol for `@@matchAll`.
  pub fn well_known_match_all() -> Self {
    Self::well_known(9)
  }
  /// Well-known symbol for `@@replace`.
  pub fn well_known_replace() -> Self {
    Self::well_known(10)
  }
  /// Well-known symbol for `@@search`.
  pub fn well_known_search() -> Self {
    Self::well_known(11)
  }
  /// Well-known symbol for `@@split`.
  pub fn well_known_split() -> Self {
    Self::well_known(12)
  }
  /// Well-known symbol for `@@unscopables`.
  pub fn well_known_unscopables() -> Self {
    Self::well_known(13)
  }
}

impl Default for SymbolId {
  fn default() -> Self {
    Self::new()
  }
}

/// Property key variant (string or symbol).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum PropertyKey {
  /// ECMAScript string key.
  String(JsString),
  /// Symbol key
  Symbol(SymbolId),
}

impl PropertyKey {
  fn shared_string(
    name: &'static str,
    key: &'static OnceLock<JsString>,
  ) -> Self {
    Self::String(key.get_or_init(|| name.into()).clone())
  }

  pub(crate) fn builtin_name() -> Self {
    static KEY: OnceLock<JsString> = OnceLock::new();
    Self::shared_string("name", &KEY)
  }

  pub(crate) fn builtin_length() -> Self {
    static KEY: OnceLock<JsString> = OnceLock::new();
    Self::shared_string("length", &KEY)
  }

  pub(crate) fn prototype() -> Self {
    static KEY: OnceLock<JsString> = OnceLock::new();
    Self::shared_string("prototype", &KEY)
  }
}

/// Property descriptor (data or accessor).
#[derive(Clone, Debug)]
pub enum PropertyDescriptor {
  /// Data descriptor
  Data {
    /// Stored value
    value: Value,
    /// Writable flag
    writable: bool,
    /// Enumerable flag
    enumerable: bool,
    /// Configurable flag
    configurable: bool,
  },
  /// Accessor descriptor
  Accessor {
    /// Getter function
    get: Option<Value>,
    /// Setter function
    set: Option<Value>,
    /// Enumerable flag
    enumerable: bool,
    /// Configurable flag
    configurable: bool,
  },
}

/// Backing store for dense array elements.
///
/// This wrapper keeps the storage layout centralized so the runtime can evolve
/// beyond a plain `Vec<Option<Value>>` without changing every array consumer.
#[derive(Clone, Debug)]
pub struct ArrayData {
  elements: Vec<Option<Value>>,
}

impl ArrayData {
  /// Create backing storage from dense elements.
  pub fn new(elements: Vec<Option<Value>>) -> Self {
    Self { elements }
  }

  /// Return a cloned copy of the dense elements.
  pub fn clone_elements(&self) -> Vec<Option<Value>> {
    self.elements.clone()
  }

  /// Remove and return every dense element, leaving an empty backing store.
  pub fn take_elements(&mut self) -> Vec<Option<Value>> {
    std::mem::take(&mut self.elements)
  }

  /// Return the array contents as packed numbers if every element is a
  /// present `Number` value.
  pub fn dense_numbers(&self) -> Option<Vec<f64>> {
    let mut out = Vec::with_capacity(self.elements.len());
    for slot in &self.elements {
      match slot {
        Some(Value::Number(n)) => out.push(*n),
        _ => return None,
      }
    }
    Some(out)
  }

  /// Replace the dense contents with packed numbers.
  pub fn write_dense_numbers(
    &mut self,
    numbers: &[f64],
  ) {
    debug_assert_eq!(self.elements.len(), numbers.len());
    self.elements.clear();
    self.elements.reserve(numbers.len());
    self.elements.extend(
      numbers
        .iter()
        .copied()
        .map(|n| Some(Value::Number(n))),
    );
  }

  /// Fill a dense numeric array range using vector-friendly chunked writes.
  pub fn fill_dense_numbers(
    &mut self,
    start: usize,
    end: usize,
    value: f64,
  ) -> bool {
    let Some(mut numbers) = self.dense_numbers() else {
      return false;
    };
    if start > end || end > numbers.len() {
      return false;
    }
    fill_f64_slice(&mut numbers[start..end], value);
    self.write_dense_numbers(&numbers);
    true
  }

  /// Copy a dense numeric range within the array.
  pub fn copy_within_dense_numbers(
    &mut self,
    to: usize,
    from: usize,
    end: usize,
  ) -> bool {
    let Some(mut numbers) = self.dense_numbers() else {
      return false;
    };
    if to > numbers.len() || from > end || end > numbers.len() {
      return false;
    }
    let count = end
      .saturating_sub(from)
      .min(numbers.len().saturating_sub(to));
    numbers.copy_within(from..from.saturating_add(count), to);
    self.write_dense_numbers(&numbers);
    true
  }
}

fn fill_f64_slice(
  slice: &mut [f64],
  value: f64,
) {
  #[cfg(target_arch = "x86_64")]
  {
    use std::arch::x86_64::{_mm_set1_pd, _mm_storeu_pd};

    // SAFETY: x86_64 guarantees SSE2 support, and the pointers come from
    // valid mutable slices with sufficient alignment for unaligned stores.
    unsafe {
      let packed = _mm_set1_pd(value);
      let mut chunks = slice.chunks_exact_mut(2);
      for chunk in &mut chunks {
        _mm_storeu_pd(chunk.as_mut_ptr(), packed);
      }
      for item in chunks.into_remainder() {
        *item = value;
      }
    }
  }

  #[cfg(not(target_arch = "x86_64"))]
  {
    for item in slice {
      *item = value;
    }
  }
}

impl From<Vec<Option<Value>>> for ArrayData {
  fn from(elements: Vec<Option<Value>>) -> Self {
    Self::new(elements)
  }
}

impl Deref for ArrayData {
  type Target = Vec<Option<Value>>;

  fn deref(&self) -> &Self::Target {
    &self.elements
  }
}

impl DerefMut for ArrayData {
  fn deref_mut(&mut self) -> &mut Self::Target {
    &mut self.elements
  }
}

/// Opaque synchronous access to a genuine `Uint8Array` backing store.
///
/// Host integrations can use this handle without depending on VM internal-slot
/// names or invoking JavaScript property operations. Every operation verifies
/// that the original ArrayBuffer is still attached to the captured backing
/// store, so a retained handle cannot access bytes after detachment.
#[derive(Clone)]
pub struct Uint8ArrayByteView {
  buffer: Value,
  data: Rc<RefCell<ArrayData>>,
  byte_offset: usize,
  length: usize,
}

impl Uint8ArrayByteView {
  pub(crate) fn new(
    buffer: Value,
    data: Rc<RefCell<ArrayData>>,
    byte_offset: usize,
    length: usize,
  ) -> Self {
    Self {
      buffer,
      data,
      byte_offset,
      length,
    }
  }

  fn attached_range(
    &self,
    offset: usize,
    length: usize,
  ) -> Option<std::ops::Range<usize>> {
    let Value::Object(buffer) = &self.buffer else {
      return None;
    };
    let attached = buffer
      .borrow()
      .internal_slot("[[ArrayBufferData]]")
      .and_then(|value| match value {
        Value::Array(data, _) => Some(Rc::ptr_eq(data, &self.data)),
        _ => None,
      })
      .unwrap_or(false);
    if !attached {
      return None;
    }
    let end = offset.checked_add(length)?;
    if end > self.length {
      return None;
    }
    let start = self.byte_offset.checked_add(offset)?;
    let end = start.checked_add(length)?;
    if end > self.data.borrow().len() {
      return None;
    }
    Some(start..end)
  }

  /// Return the view length, or zero after its ArrayBuffer is detached.
  #[must_use]
  pub fn len(&self) -> usize {
    self
      .attached_range(0, self.length)
      .map_or(0, |range| range.len())
  }

  /// Return whether this view currently contains no accessible bytes.
  #[must_use]
  pub fn is_empty(&self) -> bool {
    self.len() == 0
  }

  /// Read one byte from the view.
  #[must_use]
  pub fn read(
    &self,
    index: usize,
  ) -> Option<u8> {
    let range = self.attached_range(index, 1)?;
    self
      .data
      .borrow()
      .get(range.start)
      .and_then(|slot| slot.as_ref())
      .and_then(|value| match value {
        Value::Number(number) if number.is_finite() && (0.0..=255.0).contains(number) => {
          Some(*number as u8)
        }
        _ => None,
      })
  }

  /// Copy bytes from this view into an owned host buffer.
  #[must_use]
  pub fn read_into(
    &self,
    offset: usize,
    output: &mut [u8],
  ) -> bool {
    let Some(range) = self.attached_range(offset, output.len()) else {
      return false;
    };
    let data = self.data.borrow();
    for (destination, source) in output.iter_mut().zip(range) {
      let Some(Value::Number(byte)) = data.get(source).and_then(|slot| slot.as_ref()) else {
        return false;
      };
      if !byte.is_finite() || !(0.0..=255.0).contains(byte) {
        return false;
      }
      *destination = *byte as u8;
    }
    true
  }

  /// Write one byte into the view.
  #[must_use]
  pub fn write(
    &self,
    index: usize,
    byte: u8,
  ) -> bool {
    self.write_from(index, &[byte])
  }

  /// Copy host bytes into this view without invoking JavaScript setters.
  #[must_use]
  pub fn write_from(
    &self,
    offset: usize,
    bytes: &[u8],
  ) -> bool {
    let Some(range) = self.attached_range(offset, bytes.len()) else {
      return false;
    };
    let mut data = self.data.borrow_mut();
    for (destination, byte) in range.zip(bytes.iter().copied()) {
      data[destination] = Some(Value::Number(f64::from(byte)));
    }
    true
  }
}

/// Backing store for proxy objects.
#[derive(Clone, Debug)]
pub struct ProxyData {
  /// Stable identity used for cycle detection and future cache guards.
  pub identity: u64,
  /// The target object being proxied.
  pub target: Value,
  /// The handler object that defines the proxy behavior.
  pub handler: Option<Value>,
  callable: bool,
  constructable: bool,
}

impl ProxyData {
  /// Create a new proxy object with the given target and handler.
  pub fn new(
    target: Value,
    handler: Value,
  ) -> Self {
    let callable = target.is_callable();
    let constructable = target.is_constructable();
    Self {
      identity: ObjectData::next_identity(),
      target,
      handler: Some(handler),
      callable,
      constructable,
    }
  }

  /// Revoke the proxy, making it unusable.
  pub fn revoke(&mut self) {
    self.handler = None;
  }

  /// Check if the proxy has been revoked.
  pub fn is_revoked(&self) -> bool {
    self.handler.is_none()
  }

  pub fn is_callable(&self) -> bool {
    self.callable
  }

  pub fn is_constructable(&self) -> bool {
    self.constructable
  }
}

impl PropertyDescriptor {
  /// Create the descriptor used by ECMAScript `CreateDataProperty`.
  ///
  /// This is appropriate for properties produced by assignment, object
  /// literals, and specification algorithms that explicitly create an
  /// ordinary data property.
  pub fn ordinary_data(value: Value) -> Self {
    Self::Data {
      value,
      writable: true,
      enumerable: true,
      configurable: true,
    }
  }

  /// Create the default descriptor for a standard built-in data property.
  ///
  /// ECMAScript standard built-in data properties are writable and
  /// configurable, but non-enumerable unless their definition says otherwise.
  pub fn builtin_data(value: Value) -> Self {
    Self::Data {
      value,
      writable: true,
      enumerable: false,
      configurable: true,
    }
  }

  /// Create the standard descriptor for a built-in constructor's `prototype`.
  pub fn builtin_prototype(value: Value) -> Self {
    Self::Data {
      value,
      writable: false,
      enumerable: false,
      configurable: false,
    }
  }

  /// Create the descriptor used by built-in function `name` and `length`.
  pub fn builtin_metadata(value: Value) -> Self {
    Self::Data {
      value,
      writable: false,
      enumerable: false,
      configurable: true,
    }
  }

  /// Create a non-writable, non-enumerable, non-configurable data property.
  pub fn immutable_data(value: Value) -> Self {
    Self::Data {
      value,
      writable: false,
      enumerable: false,
      configurable: false,
    }
  }
}

/// Object storage with properties and prototype.
#[derive(Clone, Debug)]
pub struct ObjectData {
  /// Stable object identity token used by runtime caches.
  pub identity: u64,
  /// Mutation generation used by property inline caches.
  pub(crate) mutation_generation: u64,
  /// Property table
  pub properties: IndexMap<PropertyKey, PropertyDescriptor>,
  /// Strong reference to the current prototype value.
  pub strong_prototype: Option<Value>,
  /// Weak mirror of an object prototype, used when a weak reference is useful.
  pub weak_prototype: Option<WeakObject>,
  /// VM-internal slots that are never exposed through ECMAScript property operations.
  pub(crate) internal_slots: IndexMap<&'static str, Value>,
  /// Live export bindings for a module namespace exotic object. The map is
  /// populated in code-unit sorted export-name order and is deliberately kept
  /// outside `properties`: namespace exports have specialized internal
  /// methods rather than ordinary mutable data properties.
  pub(crate) module_namespace_exports: Option<IndexMap<JsString, BindingCell>>,
  /// Ephemeron entries for objects with a `[[WeakMapData]]` internal slot.
  pub(crate) weak_map_entries: Option<Vec<WeakMapEntry>>,
  /// Weak keys for objects with a `[[WeakSetData]]` internal slot.
  pub(crate) weak_set_entries: Option<Vec<WeakKey>>,
  /// Target for objects with a `[[WeakRefTarget]]` internal slot.
  pub(crate) weak_ref_target: Option<WeakValue>,
  /// Registered cells for objects with a `[[FinalizationRegistryCells]]` internal slot.
  pub(crate) finalization_registry_cells: Option<Vec<FinalizationRegistryCell>>,
  /// Cleanup callback for a FinalizationRegistry object.
  pub(crate) finalization_cleanup_callback: Option<Value>,
  /// Whether a cleanup job has been requested for this registry.
  pub(crate) finalization_cleanup_scheduled: bool,
  /// Compiled matcher for objects with a `[[RegExpMatcher]]` internal slot.
  pub(crate) regexp_matcher: Option<regex_js::Regex>,
  /// Lexical environment captured by function objects, if any.
  pub captured_lexical_env: Option<Rc<RefCell<crate::environment::EnvironmentRecord>>>,
  /// Extensible flag
  pub extensible: bool,
}

impl Default for ObjectData {
  fn default() -> Self {
    Self::new()
  }
}

impl ObjectData {
  pub(crate) fn next_identity() -> u64 {
    static NEXT_OBJECT_IDENTITY: AtomicU64 = AtomicU64::new(1);
    NEXT_OBJECT_IDENTITY.fetch_add(1, Ordering::Relaxed)
  }

  /// Create an empty object data container with a null prototype.
  pub fn new() -> Self {
    Self::with_property_capacity(0)
  }

  /// Create an empty object with reserved ordinary-property storage.
  pub(crate) fn with_property_capacity(capacity: usize) -> Self {
    Self {
      identity: Self::next_identity(),
      mutation_generation: 0,
      properties: IndexMap::with_capacity(capacity),
      strong_prototype: None,
      weak_prototype: None,
      internal_slots: IndexMap::new(),
      module_namespace_exports: None,
      weak_map_entries: None,
      weak_set_entries: None,
      weak_ref_target: None,
      finalization_registry_cells: None,
      finalization_cleanup_callback: None,
      finalization_cleanup_scheduled: false,
      regexp_matcher: None,
      captured_lexical_env: None,
      extensible: true,
    }
  }

  /// Create an empty object with a specific prototype.
  pub fn with_prototype(prototype: Value) -> Self {
    Self::with_prototype_and_property_capacity(prototype, 0)
  }

  pub(crate) fn with_prototype_and_property_capacity(
    prototype: Value,
    capacity: usize,
  ) -> Self {
    let mut object = Self::with_property_capacity(capacity);
    object.set_prototype_value(prototype);
    object
  }

  /// Create an object with a weak prototype reference (prevents cycles).
  pub fn with_weak_prototype(prototype: &Rc<RefCell<ObjectData>>) -> Self {
    Self {
      identity: Self::next_identity(),
      mutation_generation: 0,
      properties: IndexMap::new(),
      strong_prototype: None,
      weak_prototype: Some(WeakObject::new(prototype)),
      internal_slots: IndexMap::new(),
      module_namespace_exports: None,
      weak_map_entries: None,
      weak_set_entries: None,
      weak_ref_target: None,
      finalization_registry_cells: None,
      finalization_cleanup_callback: None,
      finalization_cleanup_scheduled: false,
      regexp_matcher: None,
      captured_lexical_env: None,
      extensible: true,
    }
  }

  /// Read a host or engine internal slot without exposing it to ECMAScript
  /// property operations.
  pub fn internal_slot(
    &self,
    name: &'static str,
  ) -> Option<&Value> {
    self.internal_slots.get(name)
  }

  /// Store a host or engine internal slot without creating a JavaScript-visible
  /// property.
  pub fn set_internal_slot(
    &mut self,
    name: &'static str,
    value: Value,
  ) {
    self.internal_slots.insert(name, value);
  }

  /// Get the prototype, preferring weak reference if available.
  /// Returns Value::Null if the weak reference has expired.
  pub fn get_prototype(&self) -> Value {
    if let Some(proto) = &self.strong_prototype {
      return proto.clone();
    }
    if let Some(weak) = &self.weak_prototype {
      weak.to_value()
    } else {
      Value::Null
    }
  }

  /// Set the prototype using a weak reference (prevents cycles).
  pub fn set_weak_prototype(
    &mut self,
    prototype: &Rc<RefCell<ObjectData>>,
  ) {
    self.strong_prototype = None;
    self.weak_prototype = Some(WeakObject::new(prototype));
  }

  /// Store a captured lexical environment on this object.
  pub fn set_captured_lexical_env(
    &mut self,
    env: Option<Rc<RefCell<crate::environment::EnvironmentRecord>>>,
  ) {
    self.captured_lexical_env = env;
  }

  /// Return the captured lexical environment, if any.
  pub fn captured_lexical_env(&self) -> Option<Rc<RefCell<crate::environment::EnvironmentRecord>>> {
    self.captured_lexical_env.clone()
  }

  /// Set the prototype from a runtime value.
  pub fn set_prototype_value(
    &mut self,
    prototype: Value,
  ) {
    self.strong_prototype = Some(prototype.clone());
    self.weak_prototype = match prototype {
      Value::Object(obj) => Some(WeakObject::new(&obj)),
      _ => None,
    }
  }
}

use crate::bytecode::FunctionProto;

/// Promise internal state
#[derive(Clone, Debug)]
pub enum PromiseState {
  /// Promise is waiting for resolution
  Pending {
    /// Callbacks registered for fulfillment (on_fulfilled)
    on_fulfilled: Vec<Value>,
    /// Callbacks registered for rejection (on_rejected)
    on_rejected: Vec<Value>,
  },
  /// Promise has been resolved with a value
  Fulfilled(Value),
  /// Promise has been rejected with a reason.
  Rejected {
    /// Rejection reason.
    reason: Value,
    /// Whether the rejection has a handler attached.
    handled: bool,
  },
}

#[derive(Clone)]
/// Runtime value enum
pub enum Value {
  /// Internal sentinel for temporal dead zone bindings.
  Uninitialized,
  /// Number value
  Number(f64),
  /// ECMAScript UTF-16 string value.
  String(JsString),
  /// Boolean value
  Bool(bool),
  /// Undefined value
  Undefined,
  /// BigInt value
  BigInt(BigIntValue),
  /// Symbol value
  Symbol(SymbolId, Option<JsString>),
  /// Null value
  Null,
  /// Object value with property descriptors and prototype
  Object(Rc<RefCell<ObjectData>>),
  /// Array value with indexed elements plus a property bag for non-index
  /// properties and prototype linkage. The first field stores dense indexed
  /// elements; the second is a property map used for `__proto__` and arbitrary
  /// named properties (so arrays can behave like objects for non-index keys).
  /// Holes are represented as `None` values.
  Array(Rc<RefCell<ArrayData>>, Rc<RefCell<ObjectData>>),
  /// Proxy value with target and handler state.
  Proxy(Rc<RefCell<ProxyData>>),
  /// Function closure with prototype and captured environment
  Closure {
    /// Function prototype
    proto: Rc<FunctionProto>,
    /// Captured / closed-over binding cells
    env: crate::closure_environment::ClosureEnvironment,
    /// Lexically captured `this` value for arrow functions.
    lexical_this: Option<Box<Value>>,
    /// Mutable property bag for function object properties (e.g., `prototype`).
    props: Rc<RefCell<ObjectData>>,
  },
  /// Built-in function
  Builtin(BuiltinFn),
  /// Built-in function object with a property bag (e.g. `.prototype`).
  BuiltinObject(BuiltinFn, Rc<RefCell<ObjectData>>),
  /// Promise value with internal state and ordinary object data.
  Promise(Rc<RefCell<PromiseState>>, Rc<RefCell<ObjectData>>),
}

/// Host-provided native function signature.
///
/// The extra `Value` argument is the current receiver (`this`) and mirrors the
/// signature used by ECMAScript when calling host-defined functions.
pub type NativeFn =
  Rc<dyn Fn(&mut crate::vm::Vm, Value, &[Value]) -> Result<Value, crate::errors::RuntimeError>>;

/// Built-in function type.
///
/// Builtins are invoked by the VM and receive a mutable reference to the
/// running `Vm` so they can perform VM-level operations (for example, to
/// call closures or manipulate frames). They return `Result<Value, RuntimeError>`
/// so they can signal runtime errors back to the VM using the structured
/// error type defined in `crate::errors`. The argument slice already has the
/// receiver (`this`) as the first element when invoked via `Vm::call_with_this`.
pub type BuiltinFn =
  Rc<dyn Fn(&mut crate::vm::Vm, &[Value]) -> Result<Value, crate::errors::RuntimeError>>;

type WeakBuiltinFn =
  Weak<dyn Fn(&mut crate::vm::Vm, &[Value]) -> Result<Value, crate::errors::RuntimeError>>;

/// A non-marking, lossless weak carrier for a runtime value. It is separate
/// from `WeakKey`: identity keys deliberately omit the components required to
/// reconstruct callable, array, and promise values.
#[derive(Clone)]
pub(crate) enum WeakValue {
  Primitive(Value),
  Object(Weak<RefCell<ObjectData>>),
  Array(Weak<RefCell<ArrayData>>, Weak<RefCell<ObjectData>>),
  Proxy(Weak<RefCell<ProxyData>>),
  Closure(
    Weak<FunctionProto>,
    crate::closure_environment::WeakClosureEnvironment,
    Option<Box<WeakValue>>,
    Weak<RefCell<ObjectData>>,
  ),
  Builtin(WeakBuiltinFn),
  BuiltinObject(WeakBuiltinFn, Weak<RefCell<ObjectData>>),
  Promise(Weak<RefCell<PromiseState>>, Weak<RefCell<ObjectData>>),
}

impl std::fmt::Debug for WeakValue {
  fn fmt(
    &self,
    formatter: &mut std::fmt::Formatter<'_>,
  ) -> std::fmt::Result {
    let name = match self {
      Self::Primitive(_) => "Primitive",
      Self::Object(_) => "Object",
      Self::Array(_, _) => "Array",
      Self::Proxy(_) => "Proxy",
      Self::Closure(_, _, _, _) => "Closure",
      Self::Builtin(_) => "Builtin",
      Self::BuiltinObject(_, _) => "BuiltinObject",
      Self::Promise(_, _) => "Promise",
    };
    formatter
      .debug_tuple("WeakValue")
      .field(&name)
      .finish()
  }
}

/// One registration held by a FinalizationRegistry.
#[derive(Clone, Debug)]
pub(crate) struct FinalizationRegistryCell {
  pub(crate) target: WeakValue,
  pub(crate) holdings: Value,
  pub(crate) unregister_token: Option<WeakKey>,
  pub(crate) active: bool,
}

impl WeakValue {
  pub(crate) fn new(value: &Value) -> Self {
    match value {
      Value::Object(value) => Self::Object(Rc::downgrade(value)),
      Value::Array(data, props) => Self::Array(Rc::downgrade(data), Rc::downgrade(props)),
      Value::Proxy(value) => Self::Proxy(Rc::downgrade(value)),
      Value::Closure {
        proto,
        env,
        lexical_this,
        props,
      } => Self::Closure(
        Rc::downgrade(proto),
        env.downgrade(),
        lexical_this
          .as_ref()
          .map(|value| Box::new(Self::new(value))),
        Rc::downgrade(props),
      ),
      Value::Builtin(value) => Self::Builtin(Rc::downgrade(value)),
      Value::BuiltinObject(value, props) => {
        Self::BuiltinObject(Rc::downgrade(value), Rc::downgrade(props))
      }
      Value::Promise(state, props) => Self::Promise(Rc::downgrade(state), Rc::downgrade(props)),
      value => Self::Primitive(value.clone()),
    }
  }

  pub(crate) fn upgrade(&self) -> Option<Value> {
    match self {
      Self::Primitive(value) => Some(value.clone()),
      Self::Object(value) => value.upgrade().map(Value::Object),
      Self::Array(data, props) => Some(Value::Array(data.upgrade()?, props.upgrade()?)),
      Self::Proxy(value) => value.upgrade().map(Value::Proxy),
      Self::Closure(proto, env, lexical_this, props) => Some(Value::Closure {
        proto: proto.upgrade()?,
        env: env.upgrade()?,
        lexical_this: match lexical_this {
          Some(value) => Some(Box::new(value.upgrade()?)),
          None => None,
        },
        props: props.upgrade()?,
      }),
      Self::Builtin(value) => value.upgrade().map(Value::Builtin),
      Self::BuiltinObject(value, props) => {
        Some(Value::BuiltinObject(value.upgrade()?, props.upgrade()?))
      }
      Self::Promise(state, props) => Some(Value::Promise(state.upgrade()?, props.upgrade()?)),
    }
  }
}

/// A garbage-collector-aware weak reference to any ECMAScript object identity.
#[derive(Clone)]
pub(crate) enum WeakKey {
  Object(Weak<RefCell<ObjectData>>),
  Array(Weak<RefCell<ArrayData>>),
  Proxy(Weak<RefCell<ProxyData>>),
  Closure(Weak<RefCell<ObjectData>>),
  Builtin(WeakBuiltinFn),
  BuiltinObject(Weak<RefCell<ObjectData>>),
  Promise(Weak<RefCell<PromiseState>>),
  Symbol(std::sync::Weak<u64>),
}

impl WeakKey {
  /// Create a weak key for an ECMAScript object value.
  pub(crate) fn new(value: &Value) -> Option<Self> {
    match value {
      Value::Object(value) => Some(Self::Object(Rc::downgrade(value))),
      Value::Array(value, _) => Some(Self::Array(Rc::downgrade(value))),
      Value::Proxy(value) => Some(Self::Proxy(Rc::downgrade(value))),
      Value::Closure { props, .. } => Some(Self::Closure(Rc::downgrade(props))),
      Value::Builtin(value) => Some(Self::Builtin(Rc::downgrade(value))),
      Value::BuiltinObject(_, props) => Some(Self::BuiltinObject(Rc::downgrade(props))),
      Value::Promise(value, _) => Some(Self::Promise(Rc::downgrade(value))),
      Value::Symbol(id, _) => Some(Self::Symbol(Arc::downgrade(&id.0))),
      _ => None,
    }
  }

  /// Return whether this weak key has the same identity as `value`.
  pub(crate) fn matches(
    &self,
    value: &Value,
  ) -> bool {
    match (self, value) {
      (Self::Object(key), Value::Object(value)) => key.ptr_eq(&Rc::downgrade(value)),
      (Self::Array(key), Value::Array(value, _)) => key.ptr_eq(&Rc::downgrade(value)),
      (Self::Proxy(key), Value::Proxy(value)) => key.ptr_eq(&Rc::downgrade(value)),
      (Self::Closure(key), Value::Closure { props, .. }) => key.ptr_eq(&Rc::downgrade(props)),
      (Self::Builtin(key), Value::Builtin(value)) => key.ptr_eq(&Rc::downgrade(value)),
      (Self::BuiltinObject(key), Value::BuiltinObject(_, props)) => {
        key.ptr_eq(&Rc::downgrade(props))
      }
      (Self::Promise(key), Value::Promise(value, _)) => key.ptr_eq(&Rc::downgrade(value)),
      (Self::Symbol(key), Value::Symbol(value, _)) => key
        .upgrade()
        .is_some_and(|key| *key == value.as_u64()),
      _ => false,
    }
  }

  /// Return whether the allocation still has at least one strong owner.
  pub(crate) fn is_live(&self) -> bool {
    match self {
      Self::Object(value) | Self::Closure(value) | Self::BuiltinObject(value) => {
        value.strong_count() > 0
      }
      Self::Array(value) => value.strong_count() > 0,
      Self::Proxy(value) => value.strong_count() > 0,
      Self::Builtin(value) => value.strong_count() > 0,
      Self::Promise(value) => value.strong_count() > 0,
      Self::Symbol(value) => value.strong_count() > 0,
    }
  }
}

impl std::fmt::Debug for WeakKey {
  fn fmt(
    &self,
    f: &mut std::fmt::Formatter<'_>,
  ) -> std::fmt::Result {
    f.debug_struct("WeakKey")
      .field("live", &self.is_live())
      .finish()
  }
}

/// One `WeakMap` ephemeron: a weak key and its conditionally strong value.
#[derive(Clone, Debug)]
pub(crate) struct WeakMapEntry {
  pub(crate) key: WeakKey,
  pub(crate) value: Value,
}

/// Runtime type descriptor structure retained with compiled type metadata.
#[derive(Clone, Debug)]
pub enum RuntimeType {
  /// Number type
  Number,
  /// String type
  String,
  /// Boolean type
  Boolean,
  /// Null type
  Null,
  /// Void type
  Void,
  /// Array type
  Array(Box<RuntimeType>),
  /// Object type
  Object,
  /// Function type
  Function(usize),
}

impl Value {
  /// Returns true if the value is truthy (not falsy)
  pub fn is_truthy(&self) -> bool {
    match self {
      Value::Uninitialized => false,
      Value::Undefined => false,
      Value::Null => false,
      Value::Bool(b) => *b,
      Value::Number(n) => *n != 0.0 && !n.is_nan(),
      Value::String(s) => !s.is_empty(),
      Value::BigInt(n) => !n.is_zero(),
      _ => true,
    }
  }

  /// Returns true if the value is an object type (per ECMAScript).
  pub fn is_object(&self) -> bool {
    matches!(
      self,
      Value::Object(_)
        | Value::Array(..)
        | Value::Proxy(_)
        | Value::Closure { .. }
        | Value::Builtin(_)
        | Value::BuiltinObject(_, _)
        | Value::Promise(..)
    )
  }

  /// Returns true if the value is a callable type.
  pub fn is_callable(&self) -> bool {
    match self {
      Value::Closure { .. } | Value::Builtin(_) | Value::BuiltinObject(_, _) => true,
      Value::Proxy(proxy) => {
        let proxy = proxy.borrow();
        proxy.is_callable()
      }
      _ => false,
    }
  }

  /// Returns the type name of the value as a static string
  pub fn type_name(&self) -> &'static str {
    match self {
      Value::Uninitialized => "uninitialized",
      Value::Undefined => "undefined",
      Value::Number(_) => "number",
      Value::String(_) => "string",
      Value::Bool(_) => "boolean",
      Value::BigInt(_) => "bigint",
      Value::Symbol(_, _) => "symbol",
      Value::Null => "object",
      Value::Object(_) => "object",
      Value::Array(..) => "object",
      Value::Proxy(proxy) => {
        let proxy = proxy.borrow();
        if proxy.is_callable() {
          "function"
        } else {
          "object"
        }
      }
      Value::Closure { .. } => "function",
      Value::Builtin(_) | Value::BuiltinObject(_, _) => "function",
      Value::Promise(..) => "object",
    }
  }

  /// Return the internal `Class` / object tag used by Object.prototype.toString.
  pub fn object_tag(&self) -> &'static str {
    match self {
      Value::Uninitialized => "Undefined",
      Value::Undefined => "Undefined",
      Value::Null => "Null",
      Value::Bool(_) => "Boolean",
      Value::Number(_) => "Number",
      Value::BigInt(_) => "BigInt",
      Value::String(_) => "String",
      Value::Symbol(_, _) => "Symbol",
      Value::Array(..) => "Array",
      Value::Object(_) => "Object",
      Value::Proxy(proxy) => {
        let proxy = proxy.borrow();
        if proxy.is_revoked() {
          "Object"
        } else if proxy.target.is_callable() {
          "Function"
        } else {
          "Object"
        }
      }
      Value::Closure { .. } | Value::Builtin(_) | Value::BuiltinObject(_, _) => "Function",
      Value::Promise(..) => "Promise",
    }
  }

  /// Returns whether this value exposes `Construct`, i.e. can act as a constructor.
  pub fn is_constructable(&self) -> bool {
    match self {
      Value::Closure { proto, .. } => proto.has_prototype_property && proto.is_constructor,
      Value::BuiltinObject(_, props) => {
        let props_ref = props.borrow();
        if let Some(value) = props_ref.internal_slot("[[BoundTargetFunction]]") {
          return value.is_constructable();
        }
        if matches!(
          props_ref.internal_slot("[[Constructable]]"),
          Some(Value::Bool(true))
        ) {
          return true;
        }
        if props_ref
          .properties
          .contains_key(&PropertyKey::String("prototype".to_string().into()))
        {
          return true;
        }
        false
      }
      Value::Proxy(proxy) => {
        let proxy = proxy.borrow();
        proxy.is_constructable()
      }
      _ => false,
    }
  }
}

/// Wrap a host `NativeFn` so it can be called as a builtin by the VM.
pub fn native_to_builtin(native: NativeFn) -> BuiltinFn {
  Rc::new(move |vm, args| {
    let mut iter = args.iter();
    let receiver = iter.next().cloned().unwrap_or(Value::Undefined);
    let rest: Vec<Value> = iter.cloned().collect();
    native(vm, receiver, &rest)
  })
}

/// Install a host-provided native function onto the specified global object.
pub fn define_native_global(
  globals: &Rc<RefCell<ObjectData>>,
  name: &str,
  native: NativeFn,
) {
  globals.borrow_mut().properties.insert(
    PropertyKey::String(name.to_string().into()),
    PropertyDescriptor::ordinary_data(Value::Builtin(Rc::new(move |vm, args| {
      native(vm, Value::Undefined, args)
    }))),
  );
}

fn function_display_name_from_props(props: &Rc<RefCell<ObjectData>>) -> Option<String> {
  let props = props.borrow();
  match props
    .properties
    .get(&PropertyKey::String("name".to_string().into()))
  {
    Some(PropertyDescriptor::Data {
      value: Value::String(name),
      ..
    }) if !name.is_empty() => Some(name.to_string_lossy()),
    _ => None,
  }
}

impl std::fmt::Display for Value {
  fn fmt(
    &self,
    f: &mut std::fmt::Formatter,
  ) -> std::fmt::Result {
    match self {
      Value::Uninitialized => write!(f, "<uninitialized>"),
      Value::Undefined => write!(f, "undefined"),
      Value::Number(n) => write!(f, "{}", n),
      Value::String(s) => write!(f, "{}", s),
      Value::Bool(b) => write!(f, "{}", b),
      Value::BigInt(n) => write!(f, "{}", n),
      Value::Symbol(_, desc) => {
        if let Some(d) = desc {
          write!(f, "Symbol({})", d)
        } else {
          write!(f, "Symbol()")
        }
      }
      Value::Null => write!(f, "null"),
      Value::Object(_map) => write!(f, "[object Object]"),
      Value::Proxy(_proxy) => write!(f, "[object Object]"),
      Value::Array(arr, _props) => {
        let arr = arr.borrow();
        let parts: Vec<String> = arr
          .iter()
          .map(|v| format!("{}", v.as_ref().unwrap_or(&Value::Undefined)))
          .collect();
        write!(f, "[{}]", parts.join(", "))
      }
      Value::Closure { proto, .. } => write!(f, "[Function: {}]", proto.name),
      Value::Builtin(_) => write!(f, "[Native Function]"),
      Value::BuiltinObject(_, props) => {
        if let Some(name) = function_display_name_from_props(props) {
          write!(f, "[Function: {}]", name)
        } else {
          write!(f, "[Native Function]")
        }
      }
      Value::Promise(..) => write!(f, "[object Promise]"),
    }
  }
}

impl std::fmt::Debug for Value {
  fn fmt(
    &self,
    f: &mut std::fmt::Formatter<'_>,
  ) -> std::fmt::Result {
    match self {
      Value::Uninitialized => write!(f, "Uninitialized"),
      Value::Undefined => write!(f, "Undefined"),
      Value::Number(n) => write!(f, "Number({})", n),
      Value::String(s) => write!(f, "String({:?})", s),
      Value::Bool(b) => write!(f, "Bool({})", b),
      Value::BigInt(n) => write!(f, "BigInt({})", n),
      Value::Symbol(id, desc) => write!(f, "Symbol({:?},{:?})", id, desc),
      Value::Null => write!(f, "Null"),
      Value::Object(map) => write!(f, "Object({:p})", Rc::as_ptr(map)),
      Value::Proxy(proxy) => write!(f, "Proxy({:p})", Rc::as_ptr(proxy)),
      Value::Array(arr, props) => write!(
        f,
        "Array(arr={:p}, props={:p})",
        Rc::as_ptr(arr),
        Rc::as_ptr(props)
      ),
      Value::Closure { proto, .. } => {
        write!(f, "Closure(name='{}' arity={})", proto.name, proto.arity)
      }
      Value::Builtin(_) => write!(f, "Builtin(<fn>)"),
      Value::BuiltinObject(_, props) => {
        write!(f, "BuiltinObject(props={:p})", Rc::as_ptr(props))
      }
      Value::Promise(state, _) => write!(f, "Promise({:p})", Rc::as_ptr(state)),
    }
  }
}

impl PartialEq for Value {
  fn eq(
    &self,
    other: &Value,
  ) -> bool {
    match (self, other) {
      (Value::Uninitialized, Value::Uninitialized) => true,
      (Value::Undefined, Value::Undefined) => true,
      (Value::Number(a), Value::Number(b)) => a == b,
      (Value::String(a), Value::String(b)) => a == b,
      (Value::Bool(a), Value::Bool(b)) => a == b,
      (Value::BigInt(a), Value::BigInt(b)) => a == b,
      (Value::Symbol(a, _), Value::Symbol(b, _)) => a == b,
      (Value::Null, Value::Null) => true,
      _ => false,
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn value_layout_does_not_grow_with_short_strings() {
    assert!(std::mem::size_of::<Value>() <= 48);
  }

  #[test]
  fn proxy_identities_are_stable_and_distinct() {
    let target = || Value::Object(Rc::new(RefCell::new(ObjectData::new())));
    let first = ProxyData::new(target(), target());
    let second = ProxyData::new(target(), target());
    assert_ne!(first.identity, second.identity);
  }

  #[test]
  fn builtin_object_display_uses_name_property() {
    let props = Rc::new(RefCell::new(ObjectData::new()));
    props.borrow_mut().properties.insert(
      PropertyKey::String("name".to_string().into()),
      PropertyDescriptor::Data {
        value: Value::String("Object".to_string().into()),
        writable: false,
        enumerable: false,
        configurable: true,
      },
    );
    let value = Value::BuiltinObject(Rc::new(|_, _| Ok(Value::Undefined)), props);
    assert_eq!(format!("{}", value), "[Function: Object]");
  }

  #[test]
  fn builtin_object_display_falls_back_without_name() {
    let props = Rc::new(RefCell::new(ObjectData::new()));
    let value = Value::BuiltinObject(Rc::new(|_, _| Ok(Value::Undefined)), props);
    assert_eq!(format!("{}", value), "[Native Function]");
  }

  #[test]
  fn array_data_numeric_fast_path_round_trips() {
    let mut arr = ArrayData::from(vec![
      Some(Value::Number(1.0)),
      Some(Value::Number(2.0)),
      Some(Value::Number(3.0)),
      Some(Value::Number(4.0)),
    ]);

    assert_eq!(
      arr.dense_numbers(),
      Some(vec![1.0_f64, 2.0_f64, 3.0_f64, 4.0_f64])
    );
    assert!(arr.fill_dense_numbers(1, 3, 9.0));
    assert_eq!(
      arr.clone_elements(),
      vec![
        Some(Value::Number(1.0)),
        Some(Value::Number(9.0)),
        Some(Value::Number(9.0)),
        Some(Value::Number(4.0)),
      ]
    );

    assert!(arr.copy_within_dense_numbers(0, 2, 4));
    assert_eq!(
      arr.clone_elements(),
      vec![
        Some(Value::Number(9.0)),
        Some(Value::Number(4.0)),
        Some(Value::Number(9.0)),
        Some(Value::Number(4.0)),
      ]
    );
  }

  #[test]
  fn array_data_copy_within_handles_overlapping_ranges() {
    let mut arr = ArrayData::from(vec![
      Some(Value::Number(0.0)),
      Some(Value::Number(1.0)),
      Some(Value::Number(2.0)),
      Some(Value::Number(3.0)),
    ]);

    assert!(arr.copy_within_dense_numbers(3, 0, 4));
    assert_eq!(
      arr.clone_elements(),
      vec![
        Some(Value::Number(0.0)),
        Some(Value::Number(1.0)),
        Some(Value::Number(2.0)),
        Some(Value::Number(0.0)),
      ]
    );
  }
}
