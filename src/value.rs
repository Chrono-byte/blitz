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

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::rc::Weak;

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
    /// Property table
    pub properties: HashMap<PropertyKey, PropertyDescriptor>,
    /// Prototype pointer (weak to prevent reference cycles)
    pub weak_prototype: Option<WeakObject>,
    /// Prototype pointer (strong reference - kept for compatibility during transition)
    /// TODO: Remove this once all code is migrated to use weak_prototype
    pub prototype: Value,
    /// Extensible flag
    pub extensible: bool,
}

impl ObjectData {
    /// Create an empty object data container with a null prototype.
    pub fn new() -> Self {
        Self {
            properties: HashMap::new(),
            weak_prototype: None,
            prototype: Value::Null,
            extensible: true,
        }
    }

    /// Create an empty object with a specific prototype.
    pub fn with_prototype(prototype: Value) -> Self {
        Self {
            properties: HashMap::new(),
            weak_prototype: None,
            prototype,
            extensible: true,
        }
    }

    /// Create an object with a weak prototype reference (prevents cycles).
    pub fn with_weak_prototype(prototype: &Rc<RefCell<ObjectData>>) -> Self {
        Self {
            properties: HashMap::new(),
            weak_prototype: Some(WeakObject::new(prototype)),
            prototype: Value::Object(Rc::clone(prototype)),
            extensible: true,
        }
    }

    /// Get the prototype, preferring weak reference if available.
    /// Returns Value::Null if the weak reference has expired.
    pub fn get_prototype(&self) -> Value {
        if let Some(weak) = &self.weak_prototype {
            weak.to_value()
        } else {
            self.prototype.clone()
        }
    }

    /// Set the prototype using a weak reference (prevents cycles).
    pub fn set_weak_prototype(&mut self, prototype: &Rc<RefCell<ObjectData>>) {
        self.weak_prototype = Some(WeakObject::new(prototype));
        // Keep strong ref for backward compatibility during transition
        self.prototype = Value::Object(Rc::clone(prototype));
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
    /// Promise has been rejected with a reason
    Rejected(Value),
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
    BigInt(i128),
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
        env: Rc<RefCell<Vec<Rc<RefCell<Value>>>>>,
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

/// Built-in function type.
///
/// Builtins are invoked by the VM and receive a mutable reference to the
/// running `Vm` so they can perform VM-level operations (for example, to
/// call closures or manipulate frames). They return `Result<Value, RuntimeError>`
/// so they can signal runtime errors back to the VM using the structured
/// error type defined in `crate::errors`.
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
            Value::BigInt(n) => *n != 0,
            _ => true,
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
            Value::Closure { .. } => "function",
            Value::Builtin(_) | Value::BuiltinObject(_, _) => "function",
            Value::Promise(_) => "object",
        }
    }

    /// Return the internal [[Class]] / object tag used by Object.prototype.toString.
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

    /// Returns whether this value exposes [[Construct]], i.e. can act as a constructor.
    pub fn is_constructable(&self) -> bool {
        match self {
            Value::Closure { proto, .. } => proto.has_prototype_property && proto.is_constructor,
            Value::BuiltinObject(_, props) => props
                .borrow()
                .properties
                .contains_key(&PropertyKey::String("prototype".to_string())),
            _ => false,
        }
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
            Value::Builtin(_) | Value::BuiltinObject(_, _) => write!(f, "[Native Function]"),
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
