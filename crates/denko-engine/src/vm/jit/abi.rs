// SPDX-License-Identifier: MPL-2.0

//! Private value representation at the native numeric JIT boundary.

use crate::value::Value;

const CANONICAL_NUMBER_NAN: u64 = 0x7FF8_0000_0000_0000;
const NON_NUMBER_SENTINEL: u64 = 0x7FF9_0000_0000_0000;

/// First high word reserved for non-number values after ignoring the sign bit.
pub(super) const FIRST_NON_NUMBER_HIGH_WORD: u16 = 0x7FF9;

pub(super) struct NumericArguments {
  words: [u64; 2],
  argument_count: usize,
}

impl NumericArguments {
  pub(super) fn from_values(arguments: &[Value]) -> Self {
    Self {
      words: [
        encode_argument(arguments.first()),
        encode_argument(arguments.get(1)),
      ],
      argument_count: arguments.len(),
    }
  }

  pub(super) const fn as_ptr(&self) -> *const u64 {
    self.words.as_ptr()
  }

  pub(super) const fn argument_count(&self) -> usize {
    self.argument_count
  }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct InvalidNumericResult;

pub(super) fn decode_numeric_result(bits: u64) -> Result<Value, InvalidNumericResult> {
  if high_word(bits) >= FIRST_NON_NUMBER_HIGH_WORD {
    return Err(InvalidNumericResult);
  }
  let value = f64::from_bits(bits);
  Ok(Value::Number(if value.is_nan() {
    f64::from_bits(CANONICAL_NUMBER_NAN)
  } else {
    value
  }))
}

fn encode_argument(value: Option<&Value>) -> u64 {
  match value {
    Some(Value::Number(value)) if value.is_nan() => CANONICAL_NUMBER_NAN,
    Some(Value::Number(value)) => value.to_bits(),
    _ => NON_NUMBER_SENTINEL,
  }
}

const fn high_word(bits: u64) -> u16 {
  ((bits >> 48) as u16) & 0x7FFF
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn arguments_preserve_numbers_and_mark_unsupported_values() {
    let arguments =
      NumericArguments::from_values(&[Value::Number(-0.0), Value::Undefined, Value::Number(3.0)]);
    assert_eq!(arguments.argument_count(), 3);
    assert_eq!(arguments.words, [(-0.0f64).to_bits(), NON_NUMBER_SENTINEL]);
  }

  #[test]
  fn numeric_nan_is_canonical_at_both_boundaries() {
    let arguments = NumericArguments::from_values(&[Value::Number(f64::NAN)]);
    assert_eq!(arguments.words[0], CANONICAL_NUMBER_NAN);
    assert!(matches!(
      decode_numeric_result(0x7FF8_0000_0000_0001),
      Ok(Value::Number(value)) if value.to_bits() == CANONICAL_NUMBER_NAN
    ));
  }

  #[test]
  fn reserved_result_is_abi_corruption() {
    assert_eq!(
      decode_numeric_result(NON_NUMBER_SENTINEL),
      Err(InvalidNumericResult)
    );
    assert_eq!(
      decode_numeric_result(NON_NUMBER_SENTINEL | (1 << 63)),
      Err(InvalidNumericResult)
    );
  }
}
