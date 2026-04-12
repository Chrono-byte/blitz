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
//! | `Number` | IEEE 754 double-precision float |
//! | `String` | UTF-8 string |
//! | `Bool` | Boolean value |
//! | `Null` | Null value |
//! | `Object` | Property map with prototype chain |
//! | `Array` | Dense array with property bag |
//! | `Closure` | Function with captured environment |
//! | `Builtin` | Native Rust function |
//!
//! # Memory Management
//!
//! Objects, arrays, and closures use `Rc<RefCell<...>>` for shared mutable
//! ownership within a single thread. The VM is not thread-safe.
//!
//! Conformance and implementation notes
//!
//! - Prototype representation: ObjectData stores its prototype as a weak
//!   reference to avoid keeping prototype chains alive through ownership
//!   alone. Callers should use `get_prototype` / `set_prototype_value` rather
//!   than storing a strong `Value` reference.
//!
//! - SameValue vs Strict Equality: the runtime contains several equality
//!   helpers. The semantics of `SameValue`, `SameValueZero`, and strict equality
//!   (===) differ in edge cases (for example, NaN should be SameValue-equal to
//!   itself while strict equality is false for NaN). The codebase currently
//!   mixes different equality checks; if strict ECMA-262 conformance is
//!   required, add explicit helpers named `same_value`, `same_value_zero`, and
//!   `strict_equal` and make sure the correct variant is used at each spec
//!   algorithm site (e.g., Map/Set keying, Object.is, SameValue operations).
//!
//! # Example
//!
//! ```
//! use blitz::value::Value;
//!
//! let num = Value::Number(42.0);
//! let s = Value::String("hello".to_string());
//! assert!(num.is_truthy());
//! assert_eq!(num.type_name(), "number");
//! ```

use indexmap::IndexMap;
use std::cell::RefCell;
use std::rc::Rc;
use std::rc::Weak;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use num_bigint::BigInt as BigIntValue;
use num_traits::Zero;

/// Shared mutable slot for live bindings and captured values.
pub type ValueSlot = Rc<Mutex<Value>>;

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
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.is_valid() {
            write!(f, "WeakObject(valid)")
        } else {
            write!(f, "WeakObject(expired)")
        }
    }
}

/// Symbol primitive identity.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SymbolId(u64);

impl SymbolId {
    /// Generate a new unique symbol id.
    pub fn new() -> Self {
        Self(fastrand::u64(..))
    }

    /// Create a well-known symbol id.
    pub fn well_known(id: u64) -> Self {
        Self(id)
    }

    /// Return the raw id.
    pub fn as_u64(&self) -> u64 {
        self.0
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

/// Property key variant (string or symbol).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum PropertyKey {
    /// String key
    String(String),
    /// Symbol key
    Symbol(SymbolId),
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

impl PropertyDescriptor {
    /// Create a default data descriptor with full attributes.
    pub fn data(value: Value) -> Self {
        Self::Data {
            value,
            writable: true,
            enumerable: true,
            configurable: true,
        }
    }
}

/// Object storage with properties and prototype.
#[derive(Clone, Debug)]
pub struct ObjectData {
    /// Stable object identity token used by runtime caches.
    pub identity: u64,
    /// Property table
    pub properties: IndexMap<PropertyKey, PropertyDescriptor>,
    /// Strongly stored prototype for non-object prototype values.
    pub strong_prototype: Option<Value>,
    /// Prototype pointer (weak to prevent reference cycles)
    pub weak_prototype: Option<WeakObject>,
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
    fn next_identity() -> u64 {
        static NEXT_OBJECT_IDENTITY: AtomicU64 = AtomicU64::new(1);
        NEXT_OBJECT_IDENTITY.fetch_add(1, Ordering::Relaxed)
    }

    /// Create an empty object data container with a null prototype.
    pub fn new() -> Self {
        Self {
            identity: Self::next_identity(),
            properties: IndexMap::new(),
            strong_prototype: None,
            weak_prototype: None,
            captured_lexical_env: None,
            extensible: true,
        }
    }

    /// Create an empty object with a specific prototype.
    pub fn with_prototype(prototype: Value) -> Self {
        let mut object = Self::new();
        object.set_prototype_value(prototype);
        object
    }

    /// Create an object with a weak prototype reference (prevents cycles).
    pub fn with_weak_prototype(prototype: &Rc<RefCell<ObjectData>>) -> Self {
        Self {
            identity: Self::next_identity(),
            properties: IndexMap::new(),
            strong_prototype: None,
            weak_prototype: Some(WeakObject::new(prototype)),
            captured_lexical_env: None,
            extensible: true,
        }
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
    pub fn set_weak_prototype(&mut self, prototype: &Rc<RefCell<ObjectData>>) {
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
    pub fn captured_lexical_env(
        &self,
    ) -> Option<Rc<RefCell<crate::environment::EnvironmentRecord>>> {
        self.captured_lexical_env.clone()
    }

    /// Set the prototype from a runtime value.
    pub fn set_prototype_value(&mut self, prototype: Value) {
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
    /// String value
    String(String),
    /// Boolean value
    Bool(bool),
    /// Undefined value
    Undefined,
    /// BigInt value
    BigInt(BigIntValue),
    /// Symbol value
    Symbol(SymbolId, Option<String>),
    /// Null value
    Null,
    /// Object value with property descriptors and prototype
    Object(Rc<RefCell<ObjectData>>),
    /// Array value with indexed elements plus a property bag for non-index
    /// properties and prototype linkage. The first field stores dense indexed
    /// elements; the second is a property map used for `__proto__` and arbitrary
    /// named properties (so arrays can behave like objects for non-index keys).
    /// Holes are represented as `None` values.
    Array(Rc<RefCell<Vec<Option<Value>>>>, Rc<RefCell<ObjectData>>),
    /// Function closure with prototype and captured environment
    Closure {
        /// Function prototype
        proto: Rc<FunctionProto>,
        /// Captured / closed-over binding cells
        env: Rc<Mutex<Vec<ValueSlot>>>,
        /// Lexically captured `this` value for arrow functions.
        lexical_this: Option<Box<Value>>,
        /// Mutable property bag for function object properties (e.g., `prototype`).
        props: Rc<RefCell<ObjectData>>,
    },
    /// Built-in function
    Builtin(BuiltinFn),
    /// Built-in function object with a property bag (e.g. `.prototype`).
    BuiltinObject(BuiltinFn, Rc<RefCell<ObjectData>>),
    /// Promise value with internal state
    Promise(Rc<RefCell<PromiseState>>),
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

/// Full runtime type descriptor structure so the VM can perform stricter checks.
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
    Function(usize), // arity
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
                | Value::Closure { .. }
                | Value::Builtin(_)
                | Value::BuiltinObject(_, _)
                | Value::Promise(_)
        )
    }

    /// Returns true if the value is a callable type.
    pub fn is_callable(&self) -> bool {
        matches!(
            self,
            Value::Closure { .. } | Value::Builtin(_) | Value::BuiltinObject(_, _)
        )
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
            Value::Closure { .. } => "function",
            Value::Builtin(_) | Value::BuiltinObject(_, _) => "function",
            Value::Promise(_) => "object",
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
            Value::Closure { .. } | Value::Builtin(_) | Value::BuiltinObject(_, _) => "Function",
            Value::Promise(_) => "Promise",
        }
    }

    /// Returns whether this value exposes `Construct`, i.e. can act as a constructor.
    pub fn is_constructable(&self) -> bool {
        match self {
            Value::Closure { proto, .. } => proto.has_prototype_property && proto.is_constructor,
            Value::BuiltinObject(_, props) => {
                let props_ref = props.borrow();
                if props_ref
                    .properties
                    .contains_key(&PropertyKey::String("prototype".to_string()))
                {
                    return true;
                }
                if let Some(PropertyDescriptor::Data { value, .. }) = props_ref
                    .properties
                    .get(&PropertyKey::String("[[BoundTargetFunction]]".to_string()))
                {
                    return value.is_constructable();
                }
                false
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
pub fn define_native_global(globals: &Rc<RefCell<ObjectData>>, name: &str, native: NativeFn) {
    globals.borrow_mut().properties.insert(
        PropertyKey::String(name.to_string()),
        PropertyDescriptor::data(Value::Builtin(Rc::new(move |vm, args| {
            native(vm, Value::Undefined, args)
        }))),
    );
}

fn function_display_name_from_props(props: &Rc<RefCell<ObjectData>>) -> Option<String> {
    let props = props.borrow();
    match props
        .properties
        .get(&PropertyKey::String("name".to_string()))
    {
        Some(PropertyDescriptor::Data {
            value: Value::String(name),
            ..
        }) if !name.is_empty() => Some(name.clone()),
        _ => None,
    }
}

impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
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
            Value::Promise(_) => write!(f, "[object Promise]"),
        }
    }
}

impl std::fmt::Debug for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
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
            Value::Promise(state) => write!(f, "Promise({:p})", Rc::as_ptr(state)),
        }
    }
}

impl PartialEq for Value {
    fn eq(&self, other: &Value) -> bool {
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
    fn builtin_object_display_uses_name_property() {
        let props = Rc::new(RefCell::new(ObjectData::new()));
        props.borrow_mut().properties.insert(
            PropertyKey::String("name".to_string()),
            PropertyDescriptor::Data {
                value: Value::String("Object".to_string()),
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
}
