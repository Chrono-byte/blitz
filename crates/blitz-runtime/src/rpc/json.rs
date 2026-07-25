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

/// Validate a value before it is sent to or returned from an RPC.
///
/// `serde_json::Value` already excludes functions, symbols, cycles, and other
/// JavaScript-only values. This extra pass keeps the boundary explicit and
/// rejects non-finite numbers if a value was assembled programmatically.
pub fn validate_json(value: &JsonValue) -> Result<(), JsonSafetyError> {
  validate_at(value, "$")
}

fn validate_at(
  value: &JsonValue,
  path: &str,
) -> Result<(), JsonSafetyError> {
  match value {
    Value::Null | Value::Bool(_) | Value::String(_) => Ok(()),
    Value::Number(number) => {
      if number
        .as_f64()
        .is_some_and(|number| !number.is_finite())
      {
        Err(JsonSafetyError {
          path: path.to_string(),
          message: "JSON numbers must be finite".to_string(),
        })
      } else {
        Ok(())
      }
    }
    Value::Array(values) => values
      .iter()
      .enumerate()
      .try_for_each(|(index, value)| validate_at(value, &format!("{path}/{index}"))),
    Value::Object(values) => values.iter().try_for_each(|(key, value)| {
      validate_at(
        value,
        &format!("{path}/{}", key.replace('~', "~0").replace('/', "~1")),
      )
    }),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn accepts_only_json_shapes() {
    let value = serde_json::json!({"items": [null, true, 4, "ok"]});
    assert!(validate_json(&value).is_ok());
  }

  #[test]
  fn serde_json_cannot_smuggle_non_finite_numbers() {
    assert!(serde_json::Number::from_f64(f64::NAN).is_none());
    assert!(serde_json::Number::from_f64(f64::INFINITY).is_none());
  }
}
