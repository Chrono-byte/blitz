// SPDX-License-Identifier: MPL-2.0
//! JSON-safe values used at the RPC boundary.

use std::fmt;

use serde_json::Value;

/// The JSON value accepted by the RPC protocol.
pub type JsonValue = Value;

/// The error returned when a value cannot safely cross the JSON boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JsonSafetyError {
  /// Location of the unsafe value, using a JSON-pointer-like path.
  pub path: String,
  /// A human-readable description of the violation.
  pub message: String,
}

impl fmt::Display for JsonSafetyError {
  fn fmt(
    &self,
    formatter: &mut fmt::Formatter<'_>,
  ) -> fmt::Result {
    if self.path.is_empty() {
      write!(formatter, "{}", self.message)
    } else {
      write!(formatter, "{} at {}", self.message, self.path)
    }
  }
}

impl std::error::Error for JsonSafetyError {}

/// Accept a value already parsed into the RPC JSON representation.
///
/// `serde_json::Value` cannot represent non-finite numbers, functions, symbols,
/// or cycles. Validation of JavaScript values belongs at their conversion into
/// this type. This compatibility entry point requires no recursive traversal.
pub fn validate_json(_value: &JsonValue) -> Result<(), JsonSafetyError> {
  Ok(())
}
