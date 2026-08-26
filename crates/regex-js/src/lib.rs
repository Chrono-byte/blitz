// SPDX-License-Identifier: MPL-2.0
//! ECMAScript-compatible regular expression parsing and matching.
//!
//! Match offsets are UTF-16 code-unit offsets, matching ECMAScript string
//! indexing. Observable `RegExp` state such as `lastIndex` belongs to the host
//! engine and is deliberately not stored in [`Regex`].

#[allow(
  dead_code,
  reason = "inert compiler layer; execution lands in the next barrier"
)]
mod analysis;
mod ast;
mod canonical;
mod error;
mod flags;
#[allow(
  dead_code,
  reason = "inert compiler layer; execution lands in the next barrier"
)]
mod ir;
mod matcher;
mod parser;
#[allow(
  dead_code,
  reason = "inert compiler layer; execution lands in the next barrier"
)]
mod program;
#[allow(
  dead_code,
  reason = "inert compiler layer; execution lands in the next barrier"
)]
mod verify;

pub use error::{Error, ErrorKind};
pub use flags::Flags;
pub use js_string::JsString as Input;
pub use matcher::{Capture, Match, MatchError};

use ast::Node;
use js_string::{JsString, JsStringBuilder};

/// A parsed ECMAScript regular expression.
#[derive(Clone, Debug)]
pub struct Regex {
  source: JsString,
  flags: Flags,
  body: Node,
  capture_count: usize,
  capture_names: Vec<Option<JsString>>,
}

impl Regex {
  /// Parse an ECMAScript regular expression pattern and flag string.
  pub fn new(
    source: &str,
    flags: &str,
  ) -> Result<Self, Error> {
    Self::from_js(&JsString::new(source), flags)
  }

  /// Parse a pattern that may contain isolated UTF-16 surrogates.
  pub fn from_js(
    source: &JsString,
    flags: &str,
  ) -> Result<Self, Error> {
    let flags = Flags::parse(flags)?;
    let parsed = parser::parse(source, flags)?;
    Ok(Self {
      source: source.clone(),
      flags,
      body: parsed.body,
      capture_count: parsed.capture_count,
      capture_names: parsed.capture_names,
    })
  }

  /// Return the original pattern source.
  #[must_use]
  pub fn source(&self) -> &JsString {
    &self.source
  }

  /// Return the parsed flags.
  #[must_use]
  pub const fn flags(&self) -> Flags {
    self.flags
  }

  /// Return the number of capturing parentheses in the pattern.
  #[must_use]
  pub const fn capture_count(&self) -> usize {
    self.capture_count
  }

  /// Resolve a named capture to its numeric capture index.
  #[must_use]
  pub fn capture_index(
    &self,
    name: &JsString,
  ) -> Option<usize> {
    self
      .capture_names
      .iter()
      .position(|candidate| candidate.as_ref() == Some(name))
  }

  /// Return capture names in numeric capture order, including capture zero.
  #[must_use]
  pub fn capture_names(&self) -> &[Option<JsString>] {
    &self.capture_names
  }

  /// Return the pattern text escaped for `RegExp.prototype.source`.
  #[must_use]
  pub fn escaped_source(&self) -> JsString {
    if self.source.is_empty() {
      return JsString::new("(?:)");
    }
    let mut output = JsStringBuilder::with_capacity(self.source.len());
    for unit in self.source.code_units() {
      match unit {
        value if value == u16::from(b'/') => output.push_str(r"\/"),
        0x000A => output.push_str(r"\n"),
        0x000D => output.push_str(r"\r"),
        0x2028 => output.push_str(r"\u2028"),
        0x2029 => output.push_str(r"\u2029"),
        value => output.push_code_unit(value),
      }
    }
    output.finish()
  }

  /// Find the first match at or after a UTF-16 code-unit offset.
  ///
  /// Sticky expressions only attempt a match at `start`.
  #[must_use]
  pub fn find(
    &self,
    input: &Input,
    start: usize,
  ) -> Option<Match> {
    self.try_find(input, start).ok().flatten()
  }

  /// Find the first match while reporting resource-limit exhaustion.
  ///
  /// Hosts should use this method so an exhausted matcher cannot be
  /// misreported as a successful non-match.
  pub fn try_find(
    &self,
    input: &Input,
    start: usize,
  ) -> Result<Option<Match>, MatchError> {
    matcher::find(self, input, start)
  }

  /// Parse `text` as UTF-16 and find the first match.
  #[must_use]
  pub fn find_str(
    &self,
    text: &str,
    start: usize,
  ) -> Option<Match> {
    self.find(&Input::new(text), start)
  }
}

#[cfg(test)]
mod tests;
