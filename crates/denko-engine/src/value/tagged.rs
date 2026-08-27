// SPDX-License-Identifier: MPL-2.0

//! Compact immediate values passed to native JIT code.

use super::Value;
use std::fmt;
use thiserror::Error;

const TAG_MASK: u64 = 0xFFFF_0000_0000_0000;
const PAYLOAD_MASK: u64 = 0x0000_FFFF_FFFF_FFFF;
const CANONICAL_NUMBER_NAN: u64 = 0x7FF8_0000_0000_0000;
const TAG_UNDEFINED: u64 = 0x7FF9_0000_0000_0000;
const TAG_NULL: u64 = 0x7FFA_0000_0000_0000;
const TAG_BOOLEAN: u64 = 0x7FFB_0000_0000_0000;
const TAG_UNINITIALIZED: u64 = 0x7FFC_0000_0000_0000;

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub(crate) enum TaggedValueError {
  #[error("runtime value of kind {kind} is not an immediate")]
  HeapAllocationRequired { kind: &'static str },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TaggedValueKind {
  Number,
  Undefined,
  Null,
  Boolean,
  Uninitialized,
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) struct TaggedValue(u64);

impl TaggedValue {
  pub(crate) const UNDEFINED: Self = Self(TAG_UNDEFINED);
  pub(crate) const NULL: Self = Self(TAG_NULL);
  pub(crate) const UNINITIALIZED: Self = Self(TAG_UNINITIALIZED);

  pub(crate) fn number(value: f64) -> Self {
    if value.is_nan() {
      Self(CANONICAL_NUMBER_NAN)
    } else {
      Self(value.to_bits())
    }
  }

  pub(crate) const fn boolean(value: bool) -> Self {
    Self(TAG_BOOLEAN | value as u64)
  }

  pub(crate) const fn kind(self) -> TaggedValueKind {
    match self.0 & TAG_MASK {
      TAG_UNDEFINED => TaggedValueKind::Undefined,
      TAG_NULL => TaggedValueKind::Null,
      TAG_BOOLEAN => TaggedValueKind::Boolean,
      TAG_UNINITIALIZED => TaggedValueKind::Uninitialized,
      _ => TaggedValueKind::Number,
    }
  }

  pub(crate) const fn as_boolean(self) -> Option<bool> {
    if self.0 & TAG_MASK == TAG_BOOLEAN && self.0 & PAYLOAD_MASK <= 1 {
      Some(self.0 & 1 == 1)
    } else {
      None
    }
  }

  pub(crate) const fn to_bits(self) -> u64 {
    self.0
  }
}

impl TryFrom<&Value> for TaggedValue {
  type Error = TaggedValueError;

  fn try_from(value: &Value) -> Result<Self, Self::Error> {
    match value {
      Value::Uninitialized => Ok(Self::UNINITIALIZED),
      Value::Number(value) => Ok(Self::number(*value)),
      Value::Bool(value) => Ok(Self::boolean(*value)),
      Value::Undefined => Ok(Self::UNDEFINED),
      Value::Null => Ok(Self::NULL),
      value => Err(TaggedValueError::HeapAllocationRequired {
        kind: value.type_name(),
      }),
    }
  }
}

impl fmt::Debug for TaggedValue {
  fn fmt(
    &self,
    formatter: &mut fmt::Formatter<'_>,
  ) -> fmt::Result {
    match self.kind() {
      TaggedValueKind::Number => formatter
        .debug_tuple("Number")
        .field(&f64::from_bits(self.0))
        .finish(),
      TaggedValueKind::Undefined => formatter.write_str("Undefined"),
      TaggedValueKind::Null => formatter.write_str("Null"),
      TaggedValueKind::Boolean => formatter
        .debug_tuple("Boolean")
        .field(&self.as_boolean().unwrap_or(false))
        .finish(),
      TaggedValueKind::Uninitialized => formatter.write_str("Uninitialized"),
    }
  }
}
