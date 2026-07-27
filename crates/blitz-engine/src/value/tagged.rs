// SPDX-License-Identifier: MPL-2.0
//! Register-sized values for the VM-owned, typed heap migration.
//!
//! Numbers remain native `f64` values. Non-number tags occupy canonicalized
//! positive quiet-NaN space. Heap references carry an arena kind and a 32-bit
//! index; their lifetime is bound to a [`NoGcScope`] so they cannot be reused
//! across a possible compacting collection without explicit rooting.

#![allow(dead_code, reason = "introduced ahead of the staged Value migration")]

use super::Value;
use super::scope::{GcToken, NoGcScope};
use std::fmt;
use std::marker::PhantomData;
use thiserror::Error;

const TAG_MASK: u64 = 0xFFFF_0000_0000_0000;
const PAYLOAD_MASK: u64 = 0x0000_FFFF_FFFF_FFFF;
const HEAP_KIND_SHIFT: u32 = 32;
const HEAP_UNUSED_MASK: u64 = 0x0000_FF00_0000_0000;

const CANONICAL_NUMBER_NAN: u64 = 0x7FF8_0000_0000_0000;
const TAG_UNDEFINED: u64 = 0x7FF9_0000_0000_0000;
const TAG_NULL: u64 = 0x7FFA_0000_0000_0000;
const TAG_BOOLEAN: u64 = 0x7FFB_0000_0000_0000;
const TAG_UNINITIALIZED: u64 = 0x7FFC_0000_0000_0000;
const TAG_HEAP: u64 = 0x7FFD_0000_0000_0000;
const FIRST_RESERVED_TAG: u64 = 0x7FFE_0000_0000_0000;
const LAST_RESERVED_TAG: u64 = 0x7FFF_0000_0000_0000;

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub(crate) enum TaggedValueError {
  #[error("non-canonical NaN bit pattern {bits:#018x}")]
  NonCanonicalNaN { bits: u64 },

  #[error("invalid payload {payload:#014x} for compact value tag {tag:#06x}")]
  InvalidTagPayload { tag: u16, payload: u64 },

  #[error("compact value tag {tag:#06x} is reserved")]
  ReservedTag { tag: u16 },

  #[error("unknown VM heap kind {kind}")]
  UnknownHeapKind { kind: u8 },

  #[error("runtime value of kind {kind} is not an immediate")]
  HeapAllocationRequired { kind: &'static str },

  #[error("heap {kind:?} index {index} requires its owning VM heap")]
  HeapResolutionRequired { kind: HeapKind, index: u32 },
}

/// Homogeneous arena selected by a dynamic JavaScript value.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum HeapKind {
  String = 1,
  BigInt,
  Symbol,
  Object,
  Array,
  Closure,
  Proxy,
  Promise,
}

impl TryFrom<u8> for HeapKind {
  type Error = TaggedValueError;

  fn try_from(value: u8) -> Result<Self, Self::Error> {
    match value {
      1 => Ok(Self::String),
      2 => Ok(Self::BigInt),
      3 => Ok(Self::Symbol),
      4 => Ok(Self::Object),
      5 => Ok(Self::Array),
      6 => Ok(Self::Closure),
      7 => Ok(Self::Proxy),
      8 => Ok(Self::Promise),
      kind => Err(TaggedValueError::UnknownHeapKind { kind }),
    }
  }
}

/// Dynamic reference to one of the VM's homogeneous arenas.
#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub(crate) struct HeapHandle<'gc> {
  kind: HeapKind,
  index: u32,
  _gc: PhantomData<&'gc GcToken>,
}

impl HeapHandle<'static> {
  pub(crate) const fn new(
    kind: HeapKind,
    index: u32,
  ) -> Self {
    Self {
      kind,
      index,
      _gc: PhantomData,
    }
  }

  fn from_payload(payload: u64) -> Result<Self, TaggedValueError> {
    if payload & HEAP_UNUSED_MASK != 0 {
      return Err(TaggedValueError::InvalidTagPayload {
        tag: (TAG_HEAP >> 48) as u16,
        payload,
      });
    }
    Ok(Self::new(
      HeapKind::try_from(((payload >> HEAP_KIND_SHIFT) & 0xFF) as u8)?,
      payload as u32,
    ))
  }
}

impl<'gc> HeapHandle<'gc> {
  pub(crate) const fn kind(self) -> HeapKind {
    self.kind
  }

  pub(crate) const fn index(self) -> u32 {
    self.index
  }

  const fn payload(self) -> u64 {
    ((self.kind as u64) << HEAP_KIND_SHIFT) | self.index as u64
  }

  pub(crate) fn bind<'new_gc>(
    self,
    _gc: NoGcScope<'new_gc, '_>,
  ) -> HeapHandle<'new_gc> {
    HeapHandle {
      kind: self.kind,
      index: self.index,
      _gc: PhantomData,
    }
  }

  pub(crate) const fn unbind(self) -> HeapHandle<'static> {
    HeapHandle::new(self.kind, self.index)
  }
}

impl fmt::Debug for HeapHandle<'_> {
  fn fmt(
    &self,
    formatter: &mut fmt::Formatter<'_>,
  ) -> fmt::Result {
    formatter
      .debug_struct("HeapHandle")
      .field("kind", &self.kind)
      .field("index", &self.index)
      .finish()
  }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TaggedValueKind {
  Number,
  Undefined,
  Null,
  Boolean,
  Uninitialized,
  Heap,
}

/// JIT-compatible, 64-bit value bound to one no-GC epoch.
#[repr(transparent)]
#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub(crate) struct TaggedValue<'gc>(u64, PhantomData<&'gc GcToken>);

impl<'gc> TaggedValue<'gc> {
  pub(crate) const UNDEFINED: Self = Self(TAG_UNDEFINED, PhantomData);
  pub(crate) const NULL: Self = Self(TAG_NULL, PhantomData);
  pub(crate) const UNINITIALIZED: Self = Self(TAG_UNINITIALIZED, PhantomData);

  pub(crate) fn number(value: f64) -> Self {
    Self(
      if value.is_nan() {
        CANONICAL_NUMBER_NAN
      } else {
        value.to_bits()
      },
      PhantomData,
    )
  }

  pub(crate) const fn boolean(value: bool) -> Self {
    Self(TAG_BOOLEAN | value as u64, PhantomData)
  }

  pub(crate) const fn heap(handle: HeapHandle<'gc>) -> Self {
    Self(TAG_HEAP | handle.payload(), PhantomData)
  }

  pub(crate) const fn kind(self) -> TaggedValueKind {
    match self.0 & TAG_MASK {
      TAG_UNDEFINED => TaggedValueKind::Undefined,
      TAG_NULL => TaggedValueKind::Null,
      TAG_BOOLEAN => TaggedValueKind::Boolean,
      TAG_UNINITIALIZED => TaggedValueKind::Uninitialized,
      TAG_HEAP => TaggedValueKind::Heap,
      _ => TaggedValueKind::Number,
    }
  }

  pub(crate) fn as_number(self) -> Option<f64> {
    (self.kind() == TaggedValueKind::Number).then(|| f64::from_bits(self.0))
  }

  pub(crate) const fn as_boolean(self) -> Option<bool> {
    if (self.0 & TAG_MASK) == TAG_BOOLEAN {
      Some((self.0 & 1) != 0)
    } else {
      None
    }
  }

  pub(crate) fn as_heap(self) -> Option<HeapHandle<'gc>> {
    if (self.0 & TAG_MASK) != TAG_HEAP {
      return None;
    }
    HeapHandle::from_payload(self.0 & PAYLOAD_MASK)
      .ok()
      .map(|handle| HeapHandle {
        kind: handle.kind,
        index: handle.index,
        _gc: PhantomData,
      })
  }

  pub(crate) const fn to_bits(self) -> u64 {
    self.0
  }

  pub(crate) fn bind<'new_gc>(
    self,
    _gc: NoGcScope<'new_gc, '_>,
  ) -> TaggedValue<'new_gc> {
    TaggedValue(self.0, PhantomData)
  }

  pub(crate) const fn unbind(self) -> TaggedValue<'static> {
    TaggedValue(self.0, PhantomData)
  }

  pub(super) fn rewrite_heap_handle(
    &mut self,
    rewrite: &mut impl FnMut(HeapHandle<'static>) -> HeapHandle<'static>,
  ) {
    let Some(handle) = self.as_heap() else {
      return;
    };
    *self = TaggedValue::heap(rewrite(handle.unbind()));
  }
}

impl TaggedValue<'static> {
  /// Validate unbound scalar bits received from bytecode or generated code.
  pub(crate) fn try_from_bits(bits: u64) -> Result<Self, TaggedValueError> {
    let tag = bits & TAG_MASK;
    let payload = bits & PAYLOAD_MASK;
    match tag {
      TAG_UNDEFINED | TAG_NULL | TAG_UNINITIALIZED if payload != 0 => {
        Err(TaggedValueError::InvalidTagPayload {
          tag: (tag >> 48) as u16,
          payload,
        })
      }
      TAG_BOOLEAN if payload > 1 => Err(TaggedValueError::InvalidTagPayload {
        tag: (tag >> 48) as u16,
        payload,
      }),
      TAG_HEAP => {
        HeapHandle::from_payload(payload)?;
        Ok(Self(bits, PhantomData))
      }
      FIRST_RESERVED_TAG | LAST_RESERVED_TAG => Err(TaggedValueError::ReservedTag {
        tag: (tag >> 48) as u16,
      }),
      TAG_UNDEFINED | TAG_NULL | TAG_BOOLEAN | TAG_UNINITIALIZED => Ok(Self(bits, PhantomData)),
      _ if is_nan_bits(bits) && bits != CANONICAL_NUMBER_NAN => {
        Err(TaggedValueError::NonCanonicalNaN { bits })
      }
      _ => Ok(Self(bits, PhantomData)),
    }
  }
}

impl TryFrom<&Value> for TaggedValue<'static> {
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

impl TryFrom<TaggedValue<'_>> for Value {
  type Error = TaggedValueError;

  fn try_from(value: TaggedValue<'_>) -> Result<Self, Self::Error> {
    match value.kind() {
      TaggedValueKind::Number => Ok(Self::Number(f64::from_bits(value.0))),
      TaggedValueKind::Undefined => Ok(Self::Undefined),
      TaggedValueKind::Null => Ok(Self::Null),
      TaggedValueKind::Boolean => Ok(Self::Bool(value.as_boolean().unwrap_or(false))),
      TaggedValueKind::Uninitialized => Ok(Self::Uninitialized),
      TaggedValueKind::Heap => {
        let handle = value
          .as_heap()
          .expect("validated heap-tagged value must decode");
        Err(TaggedValueError::HeapResolutionRequired {
          kind: handle.kind(),
          index: handle.index(),
        })
      }
    }
  }
}

const fn is_nan_bits(bits: u64) -> bool {
  let exponent = bits & 0x7FF0_0000_0000_0000;
  let fraction = bits & 0x000F_FFFF_FFFF_FFFF;
  exponent == 0x7FF0_0000_0000_0000 && fraction != 0
}

impl fmt::Debug for TaggedValue<'_> {
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
      TaggedValueKind::Heap => formatter
        .debug_tuple("Heap")
        .field(&self.as_heap())
        .finish(),
    }
  }
}

#[cfg(test)]
mod tests {
  use super::super::scope::GcContext;
  use super::*;

  #[test]
  fn tagged_value_has_scalar_layout() {
    assert_eq!(size_of::<TaggedValue<'static>>(), size_of::<u64>());
    assert_eq!(align_of::<TaggedValue<'static>>(), align_of::<u64>());
  }

  #[test]
  fn numbers_are_inline_and_canonicalize_nan() {
    for number in [0.0, -0.0, 1.5, -27.25, f64::INFINITY, f64::NEG_INFINITY] {
      let encoded = TaggedValue::number(number);
      assert_eq!(
        encoded.as_number().map(f64::to_bits),
        Some(number.to_bits())
      );
    }
    assert_eq!(
      TaggedValue::number(f64::from_bits(u64::MAX)).to_bits(),
      CANONICAL_NUMBER_NAN
    );
  }

  #[test]
  fn immediates_have_distinct_encodings() {
    let values = [
      TaggedValue::UNDEFINED,
      TaggedValue::NULL,
      TaggedValue::boolean(false),
      TaggedValue::boolean(true),
      TaggedValue::UNINITIALIZED,
    ];
    for (index, value) in values.iter().enumerate() {
      assert!(
        values[index + 1..]
          .iter()
          .all(|other| value != other)
      );
    }
  }

  #[test]
  fn heap_kind_and_index_round_trip_and_bind_to_epoch() {
    let unbound = HeapHandle::new(HeapKind::Array, u32::MAX);
    let encoded = TaggedValue::heap(unbound);
    assert_eq!(encoded.as_heap(), Some(unbound));

    let mut context = GcContext::new();
    let gc = context.scope();
    let bound = encoded.bind(gc.nogc());
    let handle = bound.as_heap().expect("heap handle");
    assert_eq!(handle.kind(), HeapKind::Array);
    assert_eq!(handle.index(), u32::MAX);
    assert_eq!(bound.unbind(), encoded);
  }

  #[test]
  fn raw_bits_reject_invalid_tags_heap_kinds_and_nans() {
    assert!(matches!(
      TaggedValue::try_from_bits(TAG_BOOLEAN | 2),
      Err(TaggedValueError::InvalidTagPayload { .. })
    ));
    assert_eq!(
      TaggedValue::try_from_bits(TAG_HEAP),
      Err(TaggedValueError::UnknownHeapKind { kind: 0 })
    );
    assert!(matches!(
      TaggedValue::try_from_bits(TAG_HEAP | HEAP_UNUSED_MASK),
      Err(TaggedValueError::InvalidTagPayload { .. })
    ));
    assert!(matches!(
      TaggedValue::try_from_bits(0x7FF8_0000_0000_0042),
      Err(TaggedValueError::NonCanonicalNaN { .. })
    ));
    assert_eq!(
      TaggedValue::try_from_bits(CANONICAL_NUMBER_NAN),
      Ok(TaggedValue::number(f64::NAN))
    );
  }

  #[test]
  fn existing_immediates_round_trip() {
    let values = [
      Value::Uninitialized,
      Value::Number(-0.0),
      Value::Number(17.25),
      Value::Number(f64::NAN),
      Value::Bool(false),
      Value::Bool(true),
      Value::Undefined,
      Value::Null,
    ];
    for value in values {
      let tagged = TaggedValue::try_from(&value).unwrap_or_else(|error| panic!("{error}"));
      let round_trip = Value::try_from(tagged).unwrap_or_else(|error| panic!("{error}"));
      match (&value, &round_trip) {
        (Value::Number(expected), Value::Number(actual)) if expected.is_nan() => {
          assert!(actual.is_nan());
        }
        (Value::Number(expected), Value::Number(actual)) => {
          assert_eq!(expected.to_bits(), actual.to_bits());
        }
        _ => assert_eq!(value, round_trip),
      }
    }
  }

  #[test]
  fn heap_backed_values_require_the_heap_boundary() {
    assert_eq!(
      TaggedValue::try_from(&Value::String("heap".into())),
      Err(TaggedValueError::HeapAllocationRequired { kind: "string" })
    );
    assert_eq!(
      Value::try_from(TaggedValue::heap(HeapHandle::new(HeapKind::String, 9))),
      Err(TaggedValueError::HeapResolutionRequired {
        kind: HeapKind::String,
        index: 9,
      })
    );
  }
}
