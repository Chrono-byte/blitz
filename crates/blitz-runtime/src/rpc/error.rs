// SPDX-License-Identifier: MPL-2.0

use std::fmt;

use super::json::JsonValue;

/// A decoder rejected an untrusted input value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RpcDecodeError {
  /// Explanation safe to return to the caller.
  pub message: String,
  /// Optional input location.
  pub path: Option<String>,
}

impl RpcDecodeError {
  /// Construct a decoder error.
  pub fn new(message: impl Into<String>) -> Self {
    Self {
      message: message.into(),
      path: None,
    }
  }

  /// Attach a location to this decoder error.
  #[must_use]
  pub fn at(
    mut self,
    path: impl Into<String>,
  ) -> Self {
    self.path = Some(path.into());
    self
  }

  fn to_json(&self) -> JsonValue {
    let mut object = serde_json::Map::new();
    object.insert(
      "_tag".to_string(),
      JsonValue::String("RpcDecodeError".to_string()),
    );
    object.insert(
      "message".to_string(),
      JsonValue::String(self.message.clone()),
    );
    if let Some(path) = &self.path {
      object.insert("path".to_string(), JsonValue::String(path.clone()));
    }
    JsonValue::Object(object)
  }
}

impl fmt::Display for RpcDecodeError {
  fn fmt(
    &self,
    formatter: &mut fmt::Formatter<'_>,
  ) -> fmt::Result {
    match &self.path {
      Some(path) => write!(formatter, "{} at {path}", self.message),
      None => self.message.fmt(formatter),
    }
  }
}

impl std::error::Error for RpcDecodeError {}

/// A redacted framework error. Internal causes must not be put on the wire.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RpcFrameworkError {
  /// Stable framework error tag.
  pub tag: String,
  /// Safe message for the caller.
  pub message: String,
  /// Correlation identifier, when one is available.
  pub request_id: Option<String>,
}

impl RpcFrameworkError {
  pub(crate) fn new(
    tag: impl Into<String>,
    message: impl Into<String>,
  ) -> Self {
    Self {
      tag: tag.into(),
      message: message.into(),
      request_id: None,
    }
  }

  pub(crate) fn with_request_id(
    mut self,
    request_id: &str,
  ) -> Self {
    self.request_id = Some(request_id.to_string());
    self
  }

  fn to_json(&self) -> JsonValue {
    let mut object = serde_json::Map::new();
    object.insert("_tag".to_string(), JsonValue::String(self.tag.clone()));
    object.insert(
      "message".to_string(),
      JsonValue::String(self.message.clone()),
    );
    if let Some(request_id) = &self.request_id {
      object.insert(
        "requestId".to_string(),
        JsonValue::String(request_id.clone()),
      );
    }
    JsonValue::Object(object)
  }
}

/// An RPC failure. Domain errors are preserved; framework errors are safe and redacted.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RpcError {
  /// Input validation failed.
  Decode(RpcDecodeError),
  /// A JSON-safe tagged application error.
  Domain(JsonValue),
  /// A framework-generated error.
  Framework(RpcFrameworkError),
}

impl RpcError {
  /// Construct a decoder error.
  #[must_use]
  pub fn decode(error: RpcDecodeError) -> Self {
    Self::Decode(error)
  }

  /// Construct a tagged domain error.
  pub fn domain(error: JsonValue) -> Result<Self, RpcDecodeError> {
    let JsonValue::Object(object) = &error else {
      return Err(RpcDecodeError::new("domain errors must be JSON objects"));
    };
    if !object
      .get("_tag")
      .is_some_and(|tag| tag.as_str().is_some_and(|tag| !tag.is_empty()))
    {
      return Err(RpcDecodeError::new(
        "domain errors require a non-empty string _tag",
      ));
    }
    super::json::validate_json(&error).map_err(|error| RpcDecodeError::new(error.to_string()))?;
    Ok(Self::Domain(error))
  }

  /// Construct a safe framework error.
  #[must_use]
  pub fn framework(
    tag: impl Into<String>,
    message: impl Into<String>,
  ) -> Self {
    Self::Framework(RpcFrameworkError::new(tag, message))
  }

  /// Construct an unknown-route error.
  #[must_use]
  pub fn unknown_path(path: &str) -> Self {
    Self::framework("RpcRouteError", format!("unknown procedure path: {path}"))
  }

  /// Attach a request identifier to a framework error without changing domain errors.
  #[must_use]
  pub fn with_request_id(
    self,
    request_id: &str,
  ) -> Self {
    match self {
      Self::Framework(error) if !request_id.is_empty() => {
        Self::Framework(error.with_request_id(request_id))
      }
      error => error,
    }
  }

  /// Return the JSON-safe wire representation.
  #[must_use]
  pub fn to_json(&self) -> JsonValue {
    match self {
      Self::Decode(error) => error.to_json(),
      Self::Domain(error) => error.clone(),
      Self::Framework(error) => error.to_json(),
    }
  }
}

impl fmt::Display for RpcError {
  fn fmt(
    &self,
    formatter: &mut fmt::Formatter<'_>,
  ) -> fmt::Result {
    match self {
      Self::Decode(error) => error.fmt(formatter),
      Self::Domain(error) => write!(formatter, "domain error: {error}"),
      Self::Framework(error) => write!(formatter, "{}: {}", error.tag, error.message),
    }
  }
}

impl std::error::Error for RpcError {}
