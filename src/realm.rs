//! ECMAScript Realm implementation.
//!
//! A Realm holds the set of intrinsic objects and the global environment for
//! a distinct ECMAScript execution context. Every VM operates within exactly
//! one Realm.
//!
//! # Spec reference
//!
//! ```text
//! Realm Record:
//!   [[Intrinsics]]        – canonical built-in objects
//!   [[GlobalObject]]      – the global object
//!   [[GlobalEnv]]         – the global environment record
//!   [[TemplateMap]]       – (not yet implemented)
//!   [[HostDefined]]       – (not yet implemented)
//! ```
//!
//! # Why this matters
//!
//! Without a Realm, all built-in constructors live in a flat globals map.
//! That means `instanceof`, cross-realm object identity, and per-realm
//! constructor semantics are subtly wrong. The Realm provides:
//!
//! - Per-realm constructor identity (`Array !== otherRealm.Array`)
//! - Canonical prototype objects for `instanceof`
//! - Object → realm association (future)

use crate::value::{ObjectData, PropertyDescriptor, PropertyKey, Value};
use std::cell::RefCell;
use std::rc::Rc;

/// Table of well-known intrinsic objects for a Realm.
///
/// Each field holds the *canonical* instance of a built-in constructor or
/// prototype within this realm. These are used for instanceof checks,
/// prototype chain validation, and per-realm identity.
#[derive(Clone, Debug, Default)]
pub struct Intrinsics {
    // --- Constructors (callable objects) ---
    /// Canonical `Object` constructor for this realm.
    pub object_constructor: Option<Value>,
    /// Canonical `Array` constructor for this realm.
    pub array_constructor: Option<Value>,
    /// Canonical `Function` constructor for this realm.
    pub function_constructor: Option<Value>,
    /// Canonical `String` constructor for this realm.
    pub string_constructor: Option<Value>,
    /// Canonical `Symbol` constructor for this realm.
    pub symbol_constructor: Option<Value>,
    /// Canonical `Error` constructor for this realm.
    pub error_constructor: Option<Value>,
    /// Canonical `TypeError` constructor for this realm.
    pub type_error_constructor: Option<Value>,
    /// Canonical `RangeError` constructor for this realm.
    pub range_error_constructor: Option<Value>,
    /// Canonical `ReferenceError` constructor for this realm.
    pub reference_error_constructor: Option<Value>,
    /// Canonical `SyntaxError` constructor for this realm.
    pub syntax_error_constructor: Option<Value>,
    /// Canonical `Promise` constructor for this realm.
    pub promise_constructor: Option<Value>,
    /// Canonical `Date` constructor for this realm.
    pub date_constructor: Option<Value>,
    /// Canonical `Map` constructor for this realm.
    pub map_constructor: Option<Value>,
    /// Canonical `Number` constructor for this realm.
    pub number_constructor: Option<Value>,

    // --- Prototypes ---
    /// Canonical `Object.prototype` for this realm.
    pub object_prototype: Option<Value>,
    /// Canonical `Array.prototype` for this realm.
    pub array_prototype: Option<Value>,
    /// Canonical `Function.prototype` for this realm.
    pub function_prototype: Option<Value>,
    /// Canonical `String.prototype` for this realm.
    pub string_prototype: Option<Value>,
    /// Canonical `Symbol.prototype` for this realm.
    pub symbol_prototype: Option<Value>,
    /// Canonical `Error.prototype` for this realm.
    pub error_prototype: Option<Value>,
    /// Canonical `Promise.prototype` for this realm.
    pub promise_prototype: Option<Value>,
    /// Canonical `Date.prototype` for this realm.
    pub date_prototype: Option<Value>,
    /// Canonical `Map.prototype` for this realm.
    pub map_prototype: Option<Value>,
    /// Canonical `Boolean.prototype` for this realm.
    pub boolean_prototype: Option<Value>,
    /// Canonical `Number.prototype` for this realm.
    pub number_prototype: Option<Value>,
    /// Canonical `BigInt.prototype` for this realm.
    pub bigint_prototype: Option<Value>,

    /// Canonical `EvalError` constructor for this realm.
    pub eval_error_constructor: Option<Value>,
    /// Canonical `URIError` constructor for this realm.
    pub uri_error_constructor: Option<Value>,
}

/// An ECMAScript Realm.
///
/// Every Realm owns:
/// - A set of **intrinsics** (the canonical built-in constructors/prototypes)
/// - A **globals** object (the global environment's object record)
#[derive(Clone)]
pub struct Realm {
    /// Canonical intrinsics for this realm.
    pub intrinsics: Intrinsics,
    /// The global object (`globalThis` / the global environment's object record).
    pub global_object: Rc<RefCell<ObjectData>>,
}

impl Realm {
    /// Create a new Realm with an empty global object and default intrinsics.
    pub fn new() -> Self {
        Self {
            intrinsics: Intrinsics::default(),
            global_object: Rc::new(RefCell::new(ObjectData::new())),
        }
    }

    /// Create a Realm that wraps an existing globals object.
    pub fn with_globals(globals: Rc<RefCell<ObjectData>>) -> Self {
        Self {
            intrinsics: Intrinsics::default(),
            global_object: globals,
        }
    }

    /// Get the global object as `ObjectData`.
    pub fn global(&self) -> &Rc<RefCell<ObjectData>> {
        &self.global_object
    }

    /// Look up a global property by string name.
    pub fn get_global(&self, name: &str) -> Option<Value> {
        self.global_object
            .borrow()
            .properties
            .get(&PropertyKey::String(name.to_string()))
            .and_then(|desc| match desc {
                PropertyDescriptor::Data { value, .. } => Some(value.clone()),
                _ => None,
            })
    }

    /// Set a global property by string name.
    pub fn set_global(&self, name: &str, value: Value) {
        self.global_object.borrow_mut().properties.insert(
            PropertyKey::String(name.to_string()),
            PropertyDescriptor::data(value),
        );
    }

    /// Get the canonical `Object.prototype` for this realm.
    pub fn object_prototype(&self) -> Value {
        self.intrinsics
            .object_prototype
            .clone()
            .unwrap_or(Value::Null)
    }

    /// Get the canonical `Function.prototype` for this realm.
    pub fn function_prototype(&self) -> Value {
        self.intrinsics
            .function_prototype
            .clone()
            .unwrap_or(Value::Null)
    }

    /// Get the canonical `Array.prototype` for this realm.
    pub fn array_prototype(&self) -> Value {
        self.intrinsics
            .array_prototype
            .clone()
            .unwrap_or(Value::Null)
    }

    /// Get the canonical `Error.prototype` for this realm.
    pub fn error_prototype(&self) -> Value {
        self.intrinsics
            .error_prototype
            .clone()
            .unwrap_or(Value::Null)
    }

    /// Retrieve a constructor intrinsic by name.
    pub fn get_intrinsic_constructor(&self, name: &str) -> Option<Value> {
        match name {
            "Object" => self.intrinsics.object_constructor.clone(),
            "Array" => self.intrinsics.array_constructor.clone(),
            "Function" => self.intrinsics.function_constructor.clone(),
            "String" => self.intrinsics.string_constructor.clone(),
            "Symbol" => self.intrinsics.symbol_constructor.clone(),
            "Error" => self.intrinsics.error_constructor.clone(),
            "TypeError" => self.intrinsics.type_error_constructor.clone(),
            "RangeError" => self.intrinsics.range_error_constructor.clone(),
            "ReferenceError" => self.intrinsics.reference_error_constructor.clone(),
            "SyntaxError" => self.intrinsics.syntax_error_constructor.clone(),
            "Promise" => self.intrinsics.promise_constructor.clone(),
            "Date" => self.intrinsics.date_constructor.clone(),
            "Map" => self.intrinsics.map_constructor.clone(),
            "Number" => self.intrinsics.number_constructor.clone(),
            _ => None,
        }
    }

    /// Retrieve a prototype intrinsic by name.
    pub fn get_intrinsic_prototype(&self, name: &str) -> Option<Value> {
        match name {
            "Object" => self.intrinsics.object_prototype.clone(),
            "Array" => self.intrinsics.array_prototype.clone(),
            "Function" => self.intrinsics.function_prototype.clone(),
            "String" => self.intrinsics.string_prototype.clone(),
            "Symbol" => self.intrinsics.symbol_prototype.clone(),
            "Error" => self.intrinsics.error_prototype.clone(),
            "Promise" => self.intrinsics.promise_prototype.clone(),
            "Date" => self.intrinsics.date_prototype.clone(),
            "Map" => self.intrinsics.map_prototype.clone(),
            _ => None,
        }
    }
}

impl Default for Realm {
    fn default() -> Self {
        Self::new()
    }
}
