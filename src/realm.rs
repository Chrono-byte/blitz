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
//!   [[Intrinsics]]        - canonical built-in objects
//!   [[GlobalObject]]      - the global object
//!   [[GlobalEnv]]         - the global environment record
//!   [[TemplateMap]]       - (not yet implemented)
//!   [[HostDefined]]       - (not yet implemented)
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
//! - Object -> realm association (future)

use crate::value::{ObjectData, PropertyDescriptor, PropertyKey, Value};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

/// Table of well-known intrinsic objects for a Realm.
///
/// Each field holds the *canonical* instance of a built-in constructor or
/// prototype within this realm. These are used for instanceof checks,
/// prototype chain validation, and per-realm identity.
#[derive(Clone, Debug, Default)]
pub struct Intrinsics {
    // --- Constructors (callable objects) ---
    /// Object constructor for this realm.
    pub object_constructor: Option<Value>,
    /// Array constructor for this realm.
    pub array_constructor: Option<Value>,
    /// Function constructor for this realm.
    pub function_constructor: Option<Value>,
    /// String constructor for this realm.
    pub string_constructor: Option<Value>,
    /// Symbol constructor for this realm.
    pub symbol_constructor: Option<Value>,
    /// Boolean constructor for this realm.
    pub boolean_constructor: Option<Value>,
    /// BigInt constructor for this realm.
    pub bigint_constructor: Option<Value>,
    /// Number constructor for this realm.
    pub number_constructor: Option<Value>,
    /// Error constructor for this realm.
    pub error_constructor: Option<Value>,
    /// AggregateError constructor for this realm.
    pub aggregate_error_constructor: Option<Value>,
    /// TypeError constructor for this realm.
    pub type_error_constructor: Option<Value>,
    /// RangeError constructor for this realm.
    pub range_error_constructor: Option<Value>,
    /// ReferenceError constructor for this realm.
    pub reference_error_constructor: Option<Value>,
    /// SyntaxError constructor for this realm.
    pub syntax_error_constructor: Option<Value>,
    /// EvalError constructor for this realm.
    pub eval_error_constructor: Option<Value>,
    /// URIError constructor for this realm.
    pub uri_error_constructor: Option<Value>,
    /// Promise constructor for this realm.
    pub promise_constructor: Option<Value>,
    /// Date constructor for this realm.
    pub date_constructor: Option<Value>,
    /// RegExp constructor for this realm.
    pub regexp_constructor: Option<Value>,
    /// Map constructor for this realm.
    pub map_constructor: Option<Value>,
    /// Set constructor for this realm.
    pub set_constructor: Option<Value>,
    /// ArrayBuffer constructor for this realm.
    pub arraybuffer_constructor: Option<Value>,
    /// SharedArrayBuffer constructor for this realm.
    pub shared_array_buffer_constructor: Option<Value>,
    /// DataView constructor for this realm.
    pub data_view_constructor: Option<Value>,
    /// TypedArray constructor for this realm.
    pub typed_array_constructor: Option<Value>,

    // --- Prototypes ---
    /// Object prototype for this realm.
    pub object_prototype: Option<Value>,
    /// Array prototype for this realm.
    pub array_prototype: Option<Value>,
    /// Function prototype for this realm.
    pub function_prototype: Option<Value>,
    /// String prototype for this realm.
    pub string_prototype: Option<Value>,
    /// Symbol prototype for this realm.
    pub symbol_prototype: Option<Value>,
    /// Boolean prototype for this realm.
    pub boolean_prototype: Option<Value>,
    /// BigInt prototype for this realm.
    pub bigint_prototype: Option<Value>,
    /// Number prototype for this realm.
    pub number_prototype: Option<Value>,
    /// Error prototype for this realm.
    pub error_prototype: Option<Value>,
    /// Promise prototype for this realm.
    pub promise_prototype: Option<Value>,
    /// Date prototype for this realm.
    pub date_prototype: Option<Value>,
    /// RegExp prototype for this realm.
    pub regexp_prototype: Option<Value>,
    /// Map prototype for this realm.
    pub map_prototype: Option<Value>,
    /// Set prototype for this realm.
    pub set_prototype: Option<Value>,
    /// TypedArray prototype for this realm.
    pub typed_array_prototype: Option<Value>,
    /// ArrayBuffer prototype for this realm.
    pub arraybuffer_prototype: Option<Value>,
    /// SharedArrayBuffer prototype for this realm.
    pub shared_array_buffer_prototype: Option<Value>,
    /// DataView prototype for this realm.
    pub data_view_prototype: Option<Value>,

    // --- Iterators & generators ---
    /// Iterator prototype for this realm.
    pub iterator_prototype: Option<Value>,
    /// AsyncIterator prototype for this realm.
    pub async_iterator_prototype: Option<Value>,
    /// ArrayIterator prototype for this realm.
    pub array_iterator_prototype: Option<Value>,
    /// MapIterator prototype for this realm.
    pub map_iterator_prototype: Option<Value>,
    /// SetIterator prototype for this realm.
    pub set_iterator_prototype: Option<Value>,
    /// StringIterator prototype for this realm.
    pub string_iterator_prototype: Option<Value>,
    /// RegExpStringIterator prototype for this realm.
    pub regexp_string_iterator_prototype: Option<Value>,
    /// Generator prototype for this realm.
    pub generator_prototype: Option<Value>,
    /// GeneratorFunction prototype for this realm.
    pub generator_function_prototype: Option<Value>,
    /// AsyncGenerator prototype for this realm.
    pub async_generator_prototype: Option<Value>,
    /// AsyncFromSyncIterator prototype for this realm.
    pub async_from_sync_iterator_prototype: Option<Value>,

    // --- Special helpers ---
    /// Helper for throwing a TypeError.
    pub throw_type_error: Option<Value>,
    /// Typed array constructors for this realm.
    pub typed_array_constructors: Vec<Value>,
    /// VM-internal builtins that should not be exposed as globals.
    pub hidden_builtins: HashMap<String, Value>,
}

/// An ECMAScript Realm.
///
/// Every Realm owns:
/// - A set of **intrinsics** (the canonical built-in constructors/prototypes)
/// - A **globals** object (the global environment's object record)
#[derive(Clone)]
pub struct Realm {
    /// Canonical intrinsics for this realm.
    pub intrinsics: Rc<Intrinsics>,
    /// The global object (`globalThis` / the global environment's object record).
    pub global_object: Rc<RefCell<ObjectData>>,
}

impl Realm {
    /// Create a new Realm with an empty global object and default intrinsics.
    pub fn new() -> Self {
        Self {
            intrinsics: Rc::new(Intrinsics::default()),
            global_object: Rc::new(RefCell::new(ObjectData::new())),
        }
    }

    /// Create a Realm that wraps an existing globals object.
    pub fn with_globals(globals: Rc<RefCell<ObjectData>>) -> Self {
        Self {
            intrinsics: Rc::new(Intrinsics::default()),
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

    /// Borrow the canonical `Object.prototype` for this realm.
    pub fn object_prototype_ref(&self) -> Option<&Value> {
        self.intrinsics.object_prototype.as_ref()
    }

    /// Get the canonical `Object.prototype` for this realm.
    pub fn object_prototype(&self) -> Value {
        self.object_prototype_ref().cloned().unwrap_or(Value::Null)
    }

    /// Borrow the canonical `Function.prototype` for this realm.
    pub fn function_prototype_ref(&self) -> Option<&Value> {
        self.intrinsics.function_prototype.as_ref()
    }

    /// Get the canonical `Function.prototype` for this realm.
    pub fn function_prototype(&self) -> Value {
        self.function_prototype_ref().cloned().unwrap_or(Value::Null)
    }

    /// Borrow the canonical `GeneratorFunction.prototype` for this realm.
    pub fn generator_function_prototype_ref(&self) -> Option<&Value> {
        self.intrinsics
            .generator_function_prototype
            .as_ref()
            .or_else(|| self.function_prototype_ref())
    }

    /// Get the canonical `GeneratorFunction.prototype` for this realm.
    pub fn generator_function_prototype(&self) -> Value {
        self.generator_function_prototype_ref()
            .cloned()
            .unwrap_or(Value::Null)
    }

    /// Borrow the canonical `Generator.prototype` for this realm.
    pub fn generator_prototype_ref(&self) -> Option<&Value> {
        self.intrinsics
            .generator_prototype
            .as_ref()
            .or_else(|| self.object_prototype_ref())
    }

    /// Get the canonical `Generator.prototype` for this realm.
    pub fn generator_prototype(&self) -> Value {
        self.generator_prototype_ref().cloned().unwrap_or(Value::Null)
    }

    /// Borrow the canonical `Array.prototype` for this realm.
    pub fn array_prototype_ref(&self) -> Option<&Value> {
        self.intrinsics.array_prototype.as_ref()
    }

    /// Get the canonical `Array.prototype` for this realm.
    pub fn array_prototype(&self) -> Value {
        self.array_prototype_ref().cloned().unwrap_or(Value::Null)
    }

    /// Borrow the canonical `Error.prototype` for this realm.
    pub fn error_prototype_ref(&self) -> Option<&Value> {
        self.intrinsics.error_prototype.as_ref()
    }

    /// Get the canonical `Error.prototype` for this realm.
    pub fn error_prototype(&self) -> Value {
        self.error_prototype_ref().cloned().unwrap_or(Value::Null)
    }

    /// Borrow a constructor intrinsic by name.
    pub fn intrinsic_constructor(&self, name: &str) -> Option<&Value> {
        match name {
            "Boolean" => self.intrinsics.boolean_constructor.as_ref(),
            "BigInt" => self.intrinsics.bigint_constructor.as_ref(),
            "Object" => self.intrinsics.object_constructor.as_ref(),
            "Array" => self.intrinsics.array_constructor.as_ref(),
            "Function" => self.intrinsics.function_constructor.as_ref(),
            "String" => self.intrinsics.string_constructor.as_ref(),
            "Symbol" => self.intrinsics.symbol_constructor.as_ref(),
            "Number" => self.intrinsics.number_constructor.as_ref(),
            "Error" => self.intrinsics.error_constructor.as_ref(),
            "AggregateError" => self.intrinsics.aggregate_error_constructor.as_ref(),
            "TypeError" => self.intrinsics.type_error_constructor.as_ref(),
            "RangeError" => self.intrinsics.range_error_constructor.as_ref(),
            "ReferenceError" => self.intrinsics.reference_error_constructor.as_ref(),
            "SyntaxError" => self.intrinsics.syntax_error_constructor.as_ref(),
            "EvalError" => self.intrinsics.eval_error_constructor.as_ref(),
            "URIError" => self.intrinsics.uri_error_constructor.as_ref(),
            "Promise" => self.intrinsics.promise_constructor.as_ref(),
            "Date" => self.intrinsics.date_constructor.as_ref(),
            "RegExp" => self.intrinsics.regexp_constructor.as_ref(),
            "Map" => self.intrinsics.map_constructor.as_ref(),
            "Set" => self.intrinsics.set_constructor.as_ref(),
            "ArrayBuffer" => self.intrinsics.arraybuffer_constructor.as_ref(),
            "SharedArrayBuffer" => self.intrinsics.shared_array_buffer_constructor.as_ref(),
            "DataView" => self.intrinsics.data_view_constructor.as_ref(),
            "TypedArray" => self.intrinsics.typed_array_constructor.as_ref(),
            _ => None,
        }
    }

    /// Retrieve a constructor intrinsic by name.
    pub fn get_intrinsic_constructor(&self, name: &str) -> Option<Value> {
        self.intrinsic_constructor(name).cloned()
    }

    /// Borrow a prototype intrinsic by name.
    pub fn intrinsic_prototype(&self, name: &str) -> Option<&Value> {
        match name {
            "Object" => self.intrinsics.object_prototype.as_ref(),
            "Array" => self.intrinsics.array_prototype.as_ref(),
            "Function" => self.intrinsics.function_prototype.as_ref(),
            "GeneratorFunction" => self.intrinsics.generator_function_prototype.as_ref(),
            "Generator" => self.intrinsics.generator_prototype.as_ref(),
            "String" => self.intrinsics.string_prototype.as_ref(),
            "Symbol" => self.intrinsics.symbol_prototype.as_ref(),
            "Error" => self.intrinsics.error_prototype.as_ref(),
            "Promise" => self.intrinsics.promise_prototype.as_ref(),
            "Date" => self.intrinsics.date_prototype.as_ref(),
            "Map" => self.intrinsics.map_prototype.as_ref(),
            "Set" => self.intrinsics.set_prototype.as_ref(),
            "TypedArray" => self.intrinsics.typed_array_prototype.as_ref(),
            "Boolean" => self.intrinsics.boolean_prototype.as_ref(),
            "Number" => self.intrinsics.number_prototype.as_ref(),
            "BigInt" => self.intrinsics.bigint_prototype.as_ref(),
            "RegExp" => self.intrinsics.regexp_prototype.as_ref(),
            "ArrayBuffer" => self.intrinsics.arraybuffer_prototype.as_ref(),
            "SharedArrayBuffer" => self.intrinsics.shared_array_buffer_prototype.as_ref(),
            "DataView" => self.intrinsics.data_view_prototype.as_ref(),
            _ => None,
        }
    }

    /// Retrieve a prototype intrinsic by name.
    pub fn get_intrinsic_prototype(&self, name: &str) -> Option<Value> {
        self.intrinsic_prototype(name).cloned()
    }

    /// Replace the stored intrinsics with a fresh set constructed by the kernel prelude.
    pub fn set_intrinsics(&mut self, intrinsics: Intrinsics) {
        self.intrinsics = Rc::new(intrinsics);
    }

    /// Register a VM-internal builtin that is resolved by `GetBuiltin`.
    pub fn set_hidden_builtin(&mut self, name: &str, value: Value) {
        Rc::make_mut(&mut self.intrinsics)
            .hidden_builtins
            .insert(name.to_string(), value);
    }

    /// Borrow a VM-internal builtin by name.
    pub fn hidden_builtin(&self, name: &str) -> Option<&Value> {
        self.intrinsics.hidden_builtins.get(name)
    }

    /// Look up a VM-internal builtin by name.
    pub fn get_hidden_builtin(&self, name: &str) -> Option<Value> {
        self.hidden_builtin(name).cloned()
    }
}

impl Default for Realm {
    fn default() -> Self {
        Self::new()
    }
}
