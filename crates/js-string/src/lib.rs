// SPDX-License-Identifier: MPL-2.0
//! Immutable UTF-16 strings with ECMAScript indexing semantics.
//!
//! [`JsString`] can represent isolated surrogate code units. It deliberately
//! does not implement `Deref<Target = str>` or `AsRef<str>` because not every
//! JavaScript string has a lossless Rust UTF-8 representation.

use std::cmp::Ordering;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::ops::Range;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::OnceLock;

/// A code point read from a JavaScript string.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CodePoint {
  value: u32,
  width: usize,
}

impl CodePoint {
  /// Return the Unicode scalar value or isolated surrogate value.
  #[must_use]
  pub const fn value(self) -> u32 {
    self.value
  }

  /// Return the number of UTF-16 code units consumed.
  #[must_use]
  pub const fn width(self) -> usize {
    self.width
  }
}

const INLINE_ASCII_CAPACITY: usize = 7;

/// An immutable JavaScript string backed by exact UTF-16 code units.
#[derive(Clone)]
pub struct JsString {
  repr: Repr,
}

#[derive(Clone)]
enum Repr {
  Inline(InlineAscii),
  Heap { shared: Arc<HeapString>, len: usize },
}

#[derive(Clone)]
struct InlineAscii {
  units: [u16; INLINE_ASCII_CAPACITY],
  bytes: [u8; INLINE_ASCII_CAPACITY],
  len: u8,
}

struct HeapString {
  storage: Arc<[u16]>,
  start: usize,
  utf8_lossy: OnceLock<String>,
}

impl InlineAscii {
  fn try_from_str(value: &str) -> Option<Self> {
    if value.len() > INLINE_ASCII_CAPACITY || !value.is_ascii() {
      return None;
    }
    let mut inline = Self {
      units: [0; INLINE_ASCII_CAPACITY],
      bytes: [0; INLINE_ASCII_CAPACITY],
      len: value.len() as u8,
    };
    inline.bytes[..value.len()].copy_from_slice(value.as_bytes());
    for (unit, byte) in inline.units.iter_mut().zip(value.bytes()) {
      *unit = u16::from(byte);
    }
    Some(inline)
  }

  fn try_from_units(units: &[u16]) -> Option<Self> {
    if units.len() > INLINE_ASCII_CAPACITY || units.iter().any(|unit| *unit > 0x7F) {
      return None;
    }
    let mut inline = Self {
      units: [0; INLINE_ASCII_CAPACITY],
      bytes: [0; INLINE_ASCII_CAPACITY],
      len: units.len() as u8,
    };
    inline.units[..units.len()].copy_from_slice(units);
    for (byte, unit) in inline.bytes.iter_mut().zip(units.iter().copied()) {
      *byte = unit as u8;
    }
    Some(inline)
  }

  const fn len(&self) -> usize {
    self.len as usize
  }

  fn units(&self) -> &[u16] {
    &self.units[..self.len()]
  }

  fn text(&self) -> &str {
    std::str::from_utf8(&self.bytes[..self.len()]).expect("inline ASCII is valid UTF-8")
  }
}

impl JsString {
  /// Construct an empty JavaScript string.
  #[must_use]
  pub fn empty() -> Self {
    Self::from_utf16(Vec::new())
  }

  /// Encode a Rust string as UTF-16.
  #[must_use]
  pub fn new(value: &str) -> Self {
    if let Some(inline) = InlineAscii::try_from_str(value) {
      return Self {
        repr: Repr::Inline(inline),
      };
    }
    Self::from_utf16(value.encode_utf16().collect::<Vec<_>>())
  }

  /// Construct a JavaScript string from exact UTF-16 code units.
  #[must_use]
  pub fn from_utf16(units: impl Into<Vec<u16>>) -> Self {
    let units = units.into();
    if let Some(inline) = InlineAscii::try_from_units(&units) {
      return Self {
        repr: Repr::Inline(inline),
      };
    }
    let storage: Arc<[u16]> = units.into();
    let len = storage.len();
    Self {
      repr: Repr::Heap {
        shared: Arc::new(HeapString {
          storage,
          start: 0,
          utf8_lossy: OnceLock::new(),
        }),
        len,
      },
    }
  }

  /// Return the number of UTF-16 code units.
  #[must_use]
  pub const fn len(&self) -> usize {
    match &self.repr {
      Repr::Inline(inline) => inline.len(),
      Repr::Heap { len, .. } => *len,
    }
  }

  /// Return whether this string contains no code units.
  #[must_use]
  pub const fn is_empty(&self) -> bool {
    self.len() == 0
  }

  /// Borrow the exact UTF-16 code units.
  #[must_use]
  pub fn as_slice(&self) -> &[u16] {
    match &self.repr {
      Repr::Inline(inline) => inline.units(),
      Repr::Heap { shared, len } => &shared.storage[shared.start..shared.start + len],
    }
  }

  /// Return a code unit by ECMAScript string index.
  #[must_use]
  pub fn code_unit_at(
    &self,
    index: usize,
  ) -> Option<u16> {
    self.as_slice().get(index).copied()
  }

  /// Read one code point, combining a valid surrogate pair when requested.
  ///
  /// With `unicode` false, this always returns one code unit. An isolated
  /// surrogate is returned as its numeric surrogate value.
  #[must_use]
  pub fn code_point_at(
    &self,
    index: usize,
    unicode: bool,
  ) -> Option<CodePoint> {
    let first = self.code_unit_at(index)?;
    if unicode
      && is_leading_surrogate(first)
      && let Some(second) = self.code_unit_at(index + 1)
      && is_trailing_surrogate(second)
    {
      let high = u32::from(first) - 0xD800;
      let low = u32::from(second) - 0xDC00;
      return Some(CodePoint {
        value: 0x1_0000 + (high << 10) + low,
        width: 2,
      });
    }
    Some(CodePoint {
      value: u32::from(first),
      width: 1,
    })
  }

  /// Apply ECMAScript's `AdvanceStringIndex` operation.
  #[must_use]
  pub fn advance_string_index(
    &self,
    index: usize,
    unicode: bool,
  ) -> usize {
    self
      .code_point_at(index, unicode)
      .map_or(index.saturating_add(1), |point| index + point.width)
  }

  /// Return a shared-storage substring for an exact code-unit range.
  #[must_use]
  pub fn slice(
    &self,
    range: Range<usize>,
  ) -> Option<Self> {
    if range.start > range.end || range.end > self.len() {
      return None;
    }
    let len = range.end - range.start;
    Some(match &self.repr {
      Repr::Inline(inline) => Self {
        repr: Repr::Inline(
          InlineAscii::try_from_units(&inline.units()[range])
            .expect("an inline ASCII slice remains inline ASCII"),
        ),
      },
      Repr::Heap { shared, .. } => Self {
        repr: Repr::Heap {
          shared: Arc::new(HeapString {
            storage: Arc::clone(&shared.storage),
            start: shared.start + range.start,
            utf8_lossy: OnceLock::new(),
          }),
          len,
        },
      },
    })
  }

  /// Concatenate two JavaScript strings.
  #[must_use]
  pub fn concat(
    &self,
    other: &Self,
  ) -> Self {
    let mut units = Vec::with_capacity(self.len() + other.len());
    units.extend_from_slice(self.as_slice());
    units.extend_from_slice(other.as_slice());
    Self::from_utf16(units)
  }

  /// Return whether this string begins with the supplied code units.
  #[must_use]
  pub fn starts_with(
    &self,
    needle: &Self,
  ) -> bool {
    self.as_slice().starts_with(needle.as_slice())
  }

  /// Return whether this string ends with the supplied code units.
  #[must_use]
  pub fn ends_with(
    &self,
    needle: &Self,
  ) -> bool {
    self.as_slice().ends_with(needle.as_slice())
  }

  /// Find a string at or after a code-unit offset.
  #[must_use]
  pub fn find(
    &self,
    needle: &Self,
    from: usize,
  ) -> Option<usize> {
    if from > self.len() {
      return None;
    }
    if needle.is_empty() {
      return Some(from);
    }
    self
      .as_slice()
      .get(from..)?
      .windows(needle.len())
      .position(|window| window == needle.as_slice())
      .map(|relative| from + relative)
  }

  /// Apply Unicode default uppercase mappings while preserving isolated
  /// surrogate code units.
  #[must_use]
  pub fn to_uppercase(&self) -> Self {
    self.map_case(char::to_uppercase)
  }

  /// Apply Unicode default lowercase mappings while preserving isolated
  /// surrogate code units.
  #[must_use]
  pub fn to_lowercase(&self) -> Self {
    self.map_case(char::to_lowercase)
  }

  fn map_case<I, F>(
    &self,
    map: F,
  ) -> Self
  where
    I: Iterator<Item = char>,
    F: Fn(char) -> I,
  {
    let mut output = JsStringBuilder::with_capacity(self.len());
    let mut index = 0;
    while let Some(point) = self.code_point_at(index, true) {
      if let Some(character) = char::from_u32(point.value()) {
        for mapped in map(character) {
          let _ = output.push_code_point(mapped.into());
        }
      } else {
        output.push_code_unit(point.value() as u16);
      }
      index += point.width();
    }
    output.finish()
  }

  /// Decode this string, replacing isolated surrogates with U+FFFD.
  #[must_use]
  pub fn to_string_lossy(&self) -> String {
    self.as_str_lossy().to_owned()
  }

  /// Borrow a UTF-8 view, replacing isolated surrogates with U+FFFD.
  ///
  /// Engine code should use this only at APIs that cannot accept UTF-16.
  #[must_use]
  pub fn as_str_lossy(&self) -> &str {
    match &self.repr {
      Repr::Inline(inline) => inline.text(),
      Repr::Heap { shared, .. } => shared
        .utf8_lossy
        .get_or_init(|| String::from_utf16_lossy(self.as_slice())),
    }
  }

  /// Return whether the string contains no isolated surrogate code units.
  #[must_use]
  pub fn is_well_formed(&self) -> bool {
    let mut index = 0;
    while index < self.len() {
      let unit = self.as_slice()[index];
      if is_leading_surrogate(unit) {
        if self
          .as_slice()
          .get(index + 1)
          .is_none_or(|next| !is_trailing_surrogate(*next))
        {
          return false;
        }
        index += 2;
      } else if is_trailing_surrogate(unit) {
        return false;
      } else {
        index += 1;
      }
    }
    true
  }

  /// Replace isolated surrogates with U+FFFD.
  #[must_use]
  pub fn to_well_formed(&self) -> Self {
    if self.is_well_formed() {
      return self.clone();
    }
    let mut units = self.as_slice().to_vec();
    let mut index = 0;
    while index < units.len() {
      if is_leading_surrogate(units[index]) {
        if units
          .get(index + 1)
          .is_some_and(|next| is_trailing_surrogate(*next))
        {
          index += 2;
        } else {
          units[index] = 0xFFFD;
          index += 1;
        }
      } else if is_trailing_surrogate(units[index]) {
        units[index] = 0xFFFD;
        index += 1;
      } else {
        index += 1;
      }
    }
    Self::from_utf16(units)
  }

  /// Iterate over exact UTF-16 code units.
  pub fn code_units(&self) -> impl DoubleEndedIterator<Item = u16> + ExactSizeIterator + '_ {
    self.as_slice().iter().copied()
  }

  /// Parse a well-formed textual representation using a Rust parser.
  ///
  /// Isolated surrogates are converted to U+FFFD, which makes ASCII-oriented
  /// parsers reject the input rather than accepting a different value.
  pub fn parse<T: FromStr>(&self) -> Result<T, T::Err> {
    self.as_str_lossy().parse()
  }
}

impl Default for JsString {
  fn default() -> Self {
    Self::empty()
  }
}

impl From<&str> for JsString {
  fn from(value: &str) -> Self {
    Self::new(value)
  }
}

impl From<String> for JsString {
  fn from(value: String) -> Self {
    Self::new(&value)
  }
}

impl From<Vec<u16>> for JsString {
  fn from(value: Vec<u16>) -> Self {
    Self::from_utf16(value)
  }
}

impl FromIterator<char> for JsString {
  fn from_iter<T: IntoIterator<Item = char>>(iter: T) -> Self {
    let value: String = iter.into_iter().collect();
    Self::new(&value)
  }
}

impl PartialEq for JsString {
  fn eq(
    &self,
    other: &Self,
  ) -> bool {
    self.as_slice() == other.as_slice()
  }
}

impl Eq for JsString {}

impl PartialEq<str> for JsString {
  fn eq(
    &self,
    other: &str,
  ) -> bool {
    self.code_units().eq(other.encode_utf16())
  }
}

impl PartialEq<&str> for JsString {
  fn eq(
    &self,
    other: &&str,
  ) -> bool {
    self == *other
  }
}

impl PartialOrd for JsString {
  fn partial_cmp(
    &self,
    other: &Self,
  ) -> Option<Ordering> {
    Some(self.cmp(other))
  }
}

impl Ord for JsString {
  fn cmp(
    &self,
    other: &Self,
  ) -> Ordering {
    self.as_slice().cmp(other.as_slice())
  }
}

impl Hash for JsString {
  fn hash<H: Hasher>(
    &self,
    state: &mut H,
  ) {
    self.as_slice().hash(state);
  }
}

impl fmt::Display for JsString {
  fn fmt(
    &self,
    formatter: &mut fmt::Formatter<'_>,
  ) -> fmt::Result {
    formatter.write_str(&self.to_string_lossy())
  }
}

impl fmt::Debug for JsString {
  fn fmt(
    &self,
    formatter: &mut fmt::Formatter<'_>,
  ) -> fmt::Result {
    formatter.write_str("\"")?;
    for unit in self.code_units() {
      match unit {
        value if value == u16::from(b'"') => formatter.write_str("\\\"")?,
        value if value == u16::from(b'\\') => formatter.write_str("\\\\")?,
        0x000A => formatter.write_str("\\n")?,
        0x000D => formatter.write_str("\\r")?,
        0x0009 => formatter.write_str("\\t")?,
        value if (0x20..=0x7E).contains(&value) => {
          let character = char::from_u32(u32::from(value)).unwrap_or('\u{FFFD}');
          write!(formatter, "{character}")?;
        }
        value => write!(formatter, "\\u{value:04X}")?,
      }
    }
    formatter.write_str("\"")
  }
}

/// A builder for efficiently assembling a JavaScript string.
#[derive(Debug, Default)]
pub struct JsStringBuilder {
  units: Vec<u16>,
}

impl JsStringBuilder {
  /// Create an empty builder.
  #[must_use]
  pub const fn new() -> Self {
    Self { units: Vec::new() }
  }

  /// Create an empty builder with code-unit capacity.
  #[must_use]
  pub fn with_capacity(capacity: usize) -> Self {
    Self {
      units: Vec::with_capacity(capacity),
    }
  }

  /// Append a JavaScript string.
  pub fn push_string(
    &mut self,
    value: &JsString,
  ) {
    self.units.extend_from_slice(value.as_slice());
  }

  /// Append a Rust string after encoding it as UTF-16.
  pub fn push_str(
    &mut self,
    value: &str,
  ) {
    self.units.extend(value.encode_utf16());
  }

  /// Append one exact UTF-16 code unit.
  pub fn push_code_unit(
    &mut self,
    value: u16,
  ) {
    self.units.push(value);
  }

  /// Append a valid Unicode code point as one or two UTF-16 code units.
  pub fn push_code_point(
    &mut self,
    value: u32,
  ) -> bool {
    if value > 0x10_FFFF || (0xD800..=0xDFFF).contains(&value) {
      return false;
    }
    if value <= 0xFFFF {
      self.units.push(value as u16);
    } else {
      let adjusted = value - 0x1_0000;
      self
        .units
        .push(0xD800 | ((adjusted >> 10) as u16));
      self
        .units
        .push(0xDC00 | ((adjusted & 0x3FF) as u16));
    }
    true
  }

  /// Finish the builder without further copying.
  #[must_use]
  pub fn finish(self) -> JsString {
    JsString::from_utf16(self.units)
  }
}

const fn is_leading_surrogate(unit: u16) -> bool {
  unit >= 0xD800 && unit <= 0xDBFF
}

const fn is_trailing_surrogate(unit: u16) -> bool {
  unit >= 0xDC00 && unit <= 0xDFFF
}

#[cfg(test)]
mod tests;
