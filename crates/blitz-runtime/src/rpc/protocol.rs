//! Wire envelopes and validation for the first RPC protocol slice.

use std::fmt;

use serde::{Deserialize, Serialize};

use super::error::RpcError;
use super::json::{JsonValue, validate_json};
use super::path::{PathError, RpcPath};

/// Maximum number of requests accepted in one batch.
pub const DEFAULT_MAX_BATCH_SIZE: usize = 32;

/// A remotely callable procedure kind.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RpcKind {
  /// A read-only request.
  Query,
  /// A state-changing request.
  Mutation,
  /// A streaming request (transport support is planned).
  Subscription,
}

/// A validated RPC request envelope.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RpcRequest {
  /// Client-chosen correlation identifier.
  pub id: String,
  /// Operation kind.
  #[serde(rename = "type")]
  pub kind: RpcKind,
  /// Dotted procedure path.
  pub path: String,
  /// JSON input, including `null` for a no-input procedure.
  pub input: JsonValue,
}

impl RpcRequest {
  /// Construct and validate a request envelope.
  pub fn new(
    id: impl Into<String>,
    kind: RpcKind,
    path: impl Into<String>,
    input: JsonValue,
  ) -> Result<Self, ProtocolError> {
    let request = Self {
      id: id.into(),
      kind,
      path: path.into(),
      input,
    };
    request.validate()?;
    Ok(request)
  }

  /// Validate identifiers, paths, and JSON values.
  pub fn validate(&self) -> Result<(), ProtocolError> {
    if self.id.is_empty() {
      return Err(ProtocolError::new("RPC request id must not be empty"));
    }
    RpcPath::parse(&self.path)?;
    validate_json(&self.input).map_err(ProtocolError::from)?;
    Ok(())
  }

  /// Parse a single JSON envelope.
  pub fn from_json(value: JsonValue) -> Result<Self, ProtocolError> {
    let request: Self = serde_json::from_value(value)
      .map_err(|error| ProtocolError::new(format!("invalid RPC request: {error}")))?;
    request.validate()?;
    Ok(request)
  }

  /// Serialize this validated envelope.
  pub fn to_json(&self) -> Result<JsonValue, ProtocolError> {
    self.validate()?;
    serde_json::to_value(self)
      .map_err(|error| ProtocolError::new(format!("failed to encode RPC request: {error}")))
  }
}

/// A validated RPC response envelope.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RpcResponse {
  /// Correlation identifier copied from the request.
  pub id: String,
  /// Whether the procedure completed successfully.
  pub ok: bool,
  /// Successful procedure output.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub data: Option<JsonValue>,
  /// A tagged domain or framework error.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub error: Option<JsonValue>,
}

impl RpcResponse {
  /// Construct a successful response.
  pub fn success(
    id: impl Into<String>,
    data: JsonValue,
  ) -> Result<Self, ProtocolError> {
    validate_json(&data).map_err(ProtocolError::from)?;
    Ok(Self {
      id: id.into(),
      ok: true,
      data: Some(data),
      error: None,
    })
  }

  /// Construct a failed response while preserving a JSON-safe tagged error.
  pub fn failure(
    id: impl Into<String>,
    error: RpcError,
  ) -> Result<Self, ProtocolError> {
    let error = error.to_json();
    validate_error(&error)?;
    Ok(Self {
      id: id.into(),
      ok: false,
      data: None,
      error: Some(error),
    })
  }

  /// Validate response invariants and JSON safety.
  pub fn validate(&self) -> Result<(), ProtocolError> {
    if self.id.is_empty() {
      return Err(ProtocolError::new("RPC response id must not be empty"));
    }
    match (self.ok, &self.data, &self.error) {
      (true, Some(data), None) => validate_json(data).map_err(ProtocolError::from),
      (false, None, Some(error)) => validate_error(error),
      (true, _, _) => Err(ProtocolError::new(
        "successful RPC responses require data and forbid error",
      )),
      (false, _, _) => Err(ProtocolError::new(
        "failed RPC responses require error and forbid data",
      )),
    }
  }

  /// Serialize this validated response.
  pub fn to_json(&self) -> Result<JsonValue, ProtocolError> {
    self.validate()?;
    serde_json::to_value(self)
      .map_err(|error| ProtocolError::new(format!("failed to encode RPC response: {error}")))
  }
}

/// Parse one request or a bounded batch from a JSON body.
pub fn parse_requests(
  value: JsonValue,
  max_batch_size: usize,
) -> Result<Vec<RpcRequest>, ProtocolError> {
  if max_batch_size == 0 {
    return Err(ProtocolError::new(
      "RPC batch size must be greater than zero",
    ));
  }
  match value {
    JsonValue::Array(values) => {
      if values.is_empty() {
        return Err(ProtocolError::new("RPC batches must not be empty"));
      }
      if values.len() > max_batch_size {
        return Err(ProtocolError::new(format!(
          "RPC batch exceeds the {max_batch_size}-request limit"
        )));
      }
      values
        .into_iter()
        .map(RpcRequest::from_json)
        .collect::<Result<Vec<_>, _>>()
    }
    value => Ok(vec![RpcRequest::from_json(value)?]),
  }
}

fn validate_error(error: &JsonValue) -> Result<(), ProtocolError> {
  validate_json(error).map_err(ProtocolError::from)?;
  let JsonValue::Object(object) = error else {
    return Err(ProtocolError::new("RPC errors must be JSON objects"));
  };
  if !object
    .get("_tag")
    .is_some_and(|tag| tag.as_str().is_some_and(|tag| !tag.is_empty()))
  {
    return Err(ProtocolError::new(
      "RPC errors require a non-empty string _tag",
    ));
  }
  Ok(())
}

/// A malformed protocol envelope.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProtocolError {
  /// A stable, displayable validation message.
  pub message: String,
}

impl ProtocolError {
  pub(crate) fn new(message: impl Into<String>) -> Self {
    Self {
      message: message.into(),
    }
  }
}

impl From<PathError> for ProtocolError {
  fn from(error: PathError) -> Self {
    Self::new(error.message)
  }
}

impl From<super::json::JsonSafetyError> for ProtocolError {
  fn from(error: super::json::JsonSafetyError) -> Self {
    Self::new(error.to_string())
  }
}

impl fmt::Display for ProtocolError {
  fn fmt(
    &self,
    formatter: &mut fmt::Formatter<'_>,
  ) -> fmt::Result {
    self.message.fmt(formatter)
  }
}

impl std::error::Error for ProtocolError {}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn validates_wire_shapes_and_round_trips() {
    let request = RpcRequest::new(
      "1",
      RpcKind::Query,
      "users.get",
      serde_json::json!({"id": "42"}),
    )
    .expect("valid request");
    let value = request.to_json().expect("encode request");
    assert_eq!(
      RpcRequest::from_json(value).expect("decode request"),
      request
    );

    let response =
      RpcResponse::success("1", serde_json::json!({"name": "Ada"})).expect("valid response");
    assert_eq!(response.to_json().expect("encode response")["ok"], true);
  }

  #[test]
  fn rejects_invalid_batches_and_error_shapes() {
    assert!(parse_requests(serde_json::json!([]), 32).is_err());
    assert!(parse_requests(serde_json::json!([null]), 32).is_err());
    let response = RpcResponse {
      id: "1".to_string(),
      ok: false,
      data: None,
      error: Some(serde_json::json!({"message": "not tagged"})),
    };
    assert!(response.validate().is_err());
  }
}
