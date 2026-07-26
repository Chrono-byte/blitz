// SPDX-License-Identifier: MPL-2.0
use std::fmt;

/// The category of a regular-expression syntax error.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorKind {
  /// A flag was repeated.
  DuplicateFlag,
  /// A flag is not recognized.
  InvalidFlag,
  /// The pattern ended before a construct was complete.
  UnexpectedEnd,
  /// A token is not valid at its current pattern position.
  UnexpectedToken,
  /// A parenthesized group was not closed.
  UnterminatedGroup,
  /// A character class was not closed.
  UnterminatedClass,
  /// A character-class range is reversed or otherwise invalid.
  InvalidRange,
  /// A quantifier has invalid bounds or placement.
  InvalidQuantifier,
  /// An escape sequence is malformed.
  InvalidEscape,
  /// A named capture or named backreference has an invalid name.
  InvalidGroupName,
}

/// An ECMAScript regular-expression syntax error.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Error {
  kind: ErrorKind,
  offset: usize,
  message: String,
}

impl Error {
  pub(crate) fn new(
    kind: ErrorKind,
    offset: usize,
    message: impl Into<String>,
  ) -> Self {
    Self {
      kind,
      offset,
      message: message.into(),
    }
  }

  /// Return the error category.
  #[must_use]
  pub const fn kind(&self) -> ErrorKind {
    self.kind
  }

  /// Return the UTF-16 pattern offset where parsing failed.
  #[must_use]
  pub const fn offset(&self) -> usize {
    self.offset
  }
}

impl fmt::Display for Error {
  fn fmt(
    &self,
    formatter: &mut fmt::Formatter<'_>,
  ) -> fmt::Result {
    write!(
      formatter,
      "{} at UTF-16 offset {}",
      self.message, self.offset
    )
  }
}

impl std::error::Error for Error {}
