//! Runtime values: number, string, bool, null, object, array, closure.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::bytecode::FunctionProto;

#[derive(Clone, Debug)]
/// Runtime value enum
pub enum Value {
    /// Number value
    Number(f64),
    /// String value
    String(String),
    /// Boolean value
    Bool(bool),
    /// Null value
    Null,
    /// Object value with string key-value pairs
    Object(Rc<RefCell<HashMap<String, Value>>>),
    /// Array value with indexed elements plus a property bag for non-index
    /// properties and prototype linkage. The first field stores dense indexed
    /// elements; the second is a property map used for `__proto__` and arbitrary
    /// named properties (so arrays can behave like objects for non-index keys).
    Array(Rc<RefCell<Vec<Value>>>, Rc<RefCell<HashMap<String, Value>>>),
    /// Function closure with prototype and captured environment
    Closure {
        /// Function prototype
        proto: Rc<FunctionProto>,
        /// Captured / closed-over values
        env: Rc<RefCell<Vec<Value>>>,
        /// Mutable property bag for function object properties (e.g., `prototype`).
        props: Rc<RefCell<HashMap<String, Value>>>,
    },
    /// Built-in function
    Builtin(BuiltinFn),
}

/// Built-in function type.
///
/// Builtins are invoked by the VM and receive a mutable reference to the
/// running `Vm` so they can perform VM-level operations (for example, to
/// call closures or manipulate frames). They return `Result<Value, RuntimeError>`
/// so they can signal runtime errors back to the VM using the structured
/// error type defined in `crate::errors`.
pub type BuiltinFn = fn(&mut crate::vm::Vm, &[Value]) -> Result<Value, crate::errors::RuntimeError>;

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
            Value::Null => false,
            Value::Bool(b) => *b,
            Value::Number(n) => *n != 0.0 && !n.is_nan(),
            _ => true,
        }
    }

    /// Returns the type name of the value as a static string
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Number(_) => "number",
            Value::String(_) => "string",
            Value::Bool(_) => "boolean",
            Value::Null => "null",
            Value::Object(_) => "object",
            Value::Array(..) => "object",
            Value::Closure { .. } => "function",
            Value::Builtin(_) => "function",
        }
    }
}

impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Value::Number(n) => write!(f, "{}", n),
            Value::String(s) => write!(f, "{}", s),
            Value::Bool(b) => write!(f, "{}", b),
            Value::Null => write!(f, "null"),
            Value::Object(_map) => write!(f, "[object Object]"),
            Value::Array(arr, _props) => {
                let arr = arr.borrow();
                let parts: Vec<String> = arr.iter().map(|v| format!("{}", v)).collect();
                write!(f, "[{}]", parts.join(", "))
            }
            Value::Closure { proto, .. } => write!(f, "[Function: {}]", proto.name),
            Value::Builtin(_) => write!(f, "[Native Function]"),
        }
    }
}

impl PartialEq for Value {
    fn eq(&self, other: &Value) -> bool {
        match (self, other) {
            (Value::Number(a), Value::Number(b)) => a == b,
            (Value::String(a), Value::String(b)) => a == b,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Null, Value::Null) => true,
            _ => false,
        }
    }
}
