// Compile the production value modules in a private test crate so their
// pub(crate) lifetime API remains testable without widening the library API.
// The small Value stand-in supplies only the parent-module surface used by
// tagged.rs; scope.rs, heap.rs, and tagged.rs themselves are included verbatim.

mod value {
  #[allow(dead_code)]
  #[derive(Clone, Debug, PartialEq)]
  pub(crate) enum Value {
    Uninitialized,
    Number(f64),
    String(String),
    Bool(bool),
    Undefined,
    BigInt,
    Symbol,
    Null,
    Object,
    Array,
    Proxy,
    Closure,
    Builtin,
    BuiltinObject,
    Promise,
  }

  impl Value {
    pub(crate) const fn type_name(&self) -> &'static str {
      match self {
        Self::Uninitialized => "uninitialized",
        Self::Number(_) => "number",
        Self::String(_) => "string",
        Self::Bool(_) => "boolean",
        Self::Undefined => "undefined",
        Self::BigInt => "bigint",
        Self::Symbol => "symbol",
        Self::Null => "object",
        Self::Object
        | Self::Array
        | Self::Proxy
        | Self::Closure
        | Self::Builtin
        | Self::BuiltinObject
        | Self::Promise => "object",
      }
    }
  }

  #[path = "../../src/value/scope.rs"]
  pub(crate) mod scope;

  #[path = "../../src/value/heap.rs"]
  pub(crate) mod heap;

  #[path = "../../src/value/tagged.rs"]
  pub(crate) mod tagged;
}
