use crate::value::Value;

/// Represents a partially-specified property descriptor for Object.defineProperty.
#[derive(Clone, Debug, Default)]
pub(crate) struct PartialDescriptor {
  /// The value property if specified.
  pub(crate) value: Option<Value>,
  /// The writable flag if specified.
  pub(crate) writable: Option<bool>,
  /// The enumerable flag if specified.
  pub(crate) enumerable: Option<bool>,
  /// The configurable flag if specified.
  pub(crate) configurable: Option<bool>,
  /// The getter function if specified.
  pub(crate) get: Option<Value>,
  /// The setter function if specified.
  pub(crate) set: Option<Value>,
  /// Whether a value property was explicitly specified.
  pub(crate) has_value: bool,
  /// Whether a writable property was explicitly specified.
  pub(crate) has_writable: bool,
  /// Whether an enumerable property was explicitly specified.
  pub(crate) has_enumerable: bool,
  /// Whether a configurable property was explicitly specified.
  pub(crate) has_configurable: bool,
  /// Whether a get property was explicitly specified.
  pub(crate) has_get: bool,
  /// Whether a set property was explicitly specified.
  pub(crate) has_set: bool,
}
