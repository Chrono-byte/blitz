//! Validated RPC procedure paths.

use std::fmt;

/// A canonical dotted RPC path, such as `users.get`.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RpcPath(String);

/// An invalid RPC path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PathError {
  /// A human-readable description of the invalid path.
  pub message: String,
}

impl PathError {
  fn new(message: impl Into<String>) -> Self {
    Self {
      message: message.into(),
    }
  }
}

impl fmt::Display for PathError {
  fn fmt(
    &self,
    formatter: &mut fmt::Formatter<'_>,
  ) -> fmt::Result {
    self.message.fmt(formatter)
  }
}

impl std::error::Error for PathError {}

impl RpcPath {
  /// Parse and validate a dotted path.
  pub fn parse(path: impl AsRef<str>) -> Result<Self, PathError> {
    let path = path.as_ref();
    if path.is_empty() {
      return Err(PathError::new("RPC path must not be empty"));
    }
    if path.starts_with('.') || path.ends_with('.') || path.contains("..") {
      return Err(PathError::new("RPC path contains an empty segment"));
    }
    if path
      .split('.')
      .any(|segment| segment.is_empty() || !segment.bytes().all(is_segment_byte))
    {
      return Err(PathError::new(
        "RPC path segments may contain only ASCII letters, digits, '_' and '-'",
      ));
    }
    Ok(Self(path.to_string()))
  }

  /// Return the path segments.
  pub fn segments(&self) -> impl Iterator<Item = &str> {
    self.0.split('.')
  }

  /// Return the number of segments in this path.
  #[must_use]
  pub fn len(&self) -> usize {
    self.0.split('.').count()
  }

  /// Whether this path contains no segments. Always false for a valid path.
  #[must_use]
  pub const fn is_empty(&self) -> bool {
    false
  }

  /// Join this path with a child segment or path.
  pub fn join(
    &self,
    child: impl AsRef<str>,
  ) -> Result<Self, PathError> {
    Self::parse(format!("{}.{}", self.0, child.as_ref()))
  }
}

impl AsRef<str> for RpcPath {
  fn as_ref(&self) -> &str {
    &self.0
  }
}

impl fmt::Display for RpcPath {
  fn fmt(
    &self,
    formatter: &mut fmt::Formatter<'_>,
  ) -> fmt::Result {
    self.0.fmt(formatter)
  }
}

impl TryFrom<&str> for RpcPath {
  type Error = PathError;

  fn try_from(path: &str) -> Result<Self, Self::Error> {
    Self::parse(path)
  }
}

fn is_segment_byte(byte: u8) -> bool {
  byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn validates_and_joins_paths() {
    let path = RpcPath::parse("users.get").expect("valid path");
    assert_eq!(path.len(), 2);
    assert_eq!(path.to_string(), "users.get");
    assert_eq!(
      path
        .join("by-id")
        .expect("valid child")
        .to_string(),
      "users.get.by-id"
    );
  }

  #[test]
  fn rejects_ambiguous_paths() {
    for path in [
      "",
      ".users",
      "users.",
      "users..get",
      "users/get",
      "users get",
    ] {
      assert!(RpcPath::parse(path).is_err(), "{path} should be invalid");
    }
  }
}
