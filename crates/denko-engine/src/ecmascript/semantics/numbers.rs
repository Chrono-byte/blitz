// SPDX-License-Identifier: MPL-2.0
use crate::value::Value;
use crate::{RuntimeError, Vm};
use num_traits::ToPrimitive;

/// Converts a `usize` to a `f64`, returning `f64::NAN` if the value is too large.
pub(crate) fn usize_to_f64(value: usize) -> f64 {
  value.to_f64().unwrap_or(f64::NAN)
}

/// Apply ECMAScript Number exponentiation semantics.
///
/// Rust's `powf` differs from ECMAScript when the base has an absolute value
/// of one and the exponent is infinite: ECMAScript requires `NaN` because the
/// limiting value is indeterminate.
pub(crate) fn number_exponentiate(
  base: f64,
  exponent: f64,
) -> f64 {
  if base.abs() == 1.0_f64 && exponent.is_infinite() {
    f64::NAN
  } else {
    base.powf(exponent)
  }
}

/// Format a JavaScript `Number` for string conversion.
///
/// This intentionally follows ECMAScript `ToString` expectations for the
/// cases we exercise in the VM: integral values should not keep Rust's
/// trailing `.0` formatting.
pub(crate) fn js_number_to_string(n: f64) -> String {
  if n.is_nan() {
    return "NaN".to_string();
  }
  if n == 0.0 {
    return "0".to_string();
  }
  if n == f64::INFINITY {
    return "Infinity".to_string();
  }
  if n == f64::NEG_INFINITY {
    return "-Infinity".to_string();
  }

  let abs = n.abs();
  // Safe integers have an exact decimal representation and need no floating-point formatter.
  if abs <= 9_007_199_254_740_991.0 && n.fract() == 0.0 {
    return (n as i64).to_string();
  }
  if !(1e-6..1e21).contains(&abs) {
    let s = format!("{:e}", n);
    if let Some(e_idx) = s.find('e') {
      let mantissa = s[..e_idx]
        .trim_end_matches('0')
        .trim_end_matches('.');
      let exp = &s[e_idx + 1..];
      let (sign, digits) = if let Some(rest) = exp.strip_prefix('-') {
        ('-', rest)
      } else if let Some(rest) = exp.strip_prefix('+') {
        ('+', rest)
      } else {
        ('+', exp)
      };
      let digits = digits.trim_start_matches('0');
      let digits = if digits.is_empty() { "0" } else { digits };
      return format!("{}e{}{}", mantissa, sign, digits);
    }
    return s;
  }

  if n.fract() == 0.0 {
    format!("{:.0}", n)
  } else {
    n.to_string()
  }
}

fn char_digit_value(ch: char) -> Option<u32> {
  match ch {
    '0'..='9' => Some(ch as u32 - '0' as u32),
    'a'..='z' => Some(ch as u32 - 'a' as u32 + 10),
    'A'..='Z' => Some(ch as u32 - 'A' as u32 + 10),
    _ => None,
  }
}

fn parse_int_string(
  text: &str,
  radix_arg: Option<i32>,
) -> Option<f64> {
  let mut s = text;
  let mut sign = 1.0;
  if let Some(rest) = s.strip_prefix('+') {
    s = rest;
  } else if let Some(rest) = s.strip_prefix('-') {
    sign = -1.0;
    s = rest;
  }

  let mut radix = match radix_arg {
    Some(r) if r != 0 => {
      if !(2..=36).contains(&r) {
        return None;
      }
      u32::try_from(r).ok()?
    }
    _ => 0,
  };

  if radix == 0 {
    if s.len() >= 2 && (s.starts_with("0x") || s.starts_with("0X")) {
      radix = 16;
      s = &s[2..];
    }
    if radix == 0 {
      radix = 10;
    }
  } else if radix == 16 && s.len() >= 2 && (s.starts_with("0x") || s.starts_with("0X")) {
    s = &s[2..];
  }

  let radix_f = radix as f64;
  let mut value = 0.0;
  let mut digits = 0;
  for ch in s.chars() {
    if let Some(digit) = char_digit_value(ch) {
      if digit >= radix {
        break;
      }
      value = value * radix_f + digit as f64;
      digits += 1;
    } else {
      break;
    }
  }

  if digits == 0 {
    return None;
  }

  Some(sign * value)
}

fn parse_float_prefix_len(s: &str) -> Option<usize> {
  let bytes = s.as_bytes();
  let len = bytes.len();
  let mut i = 0;
  if i < len && (bytes[i] == b'+' || bytes[i] == b'-') {
    i += 1;
  }
  let start_digits = i;
  while i < len && bytes[i].is_ascii_digit() {
    i += 1;
  }
  let digits_before = i - start_digits;
  let mut digits_fraction = 0;
  if i < len && bytes[i] == b'.' {
    i += 1;
    let frac_start = i;
    while i < len && bytes[i].is_ascii_digit() {
      i += 1;
    }
    digits_fraction = i - frac_start;
  }
  if digits_before == 0 && digits_fraction == 0 {
    return None;
  }
  let mut end_index = i;
  if i < len && (bytes[i] == b'e' || bytes[i] == b'E') {
    let exp_index = i;
    i += 1;
    if i < len && (bytes[i] == b'+' || bytes[i] == b'-') {
      i += 1;
    }
    let exp_start = i;
    while i < len && bytes[i].is_ascii_digit() {
      i += 1;
    }
    if i == exp_start {
      end_index = exp_index;
    } else {
      end_index = i;
    }
  }
  Some(end_index)
}

/// Checks if a byte offset is properly aligned for a given element count.
pub(crate) fn is_byte_offset_aligned(
  byte_offset: usize,
  element_count: usize,
) -> bool {
  element_count == 1 || byte_offset.is_multiple_of(element_count)
}

fn parse_special_float(s: &str) -> Option<f64> {
  if s.strip_prefix("+Infinity").is_some() {
    return Some(f64::INFINITY);
  }
  if s.strip_prefix("-Infinity").is_some() {
    return Some(f64::NEG_INFINITY);
  }
  if s.strip_prefix("Infinity").is_some() {
    return Some(f64::INFINITY);
  }
  if s.strip_prefix("+NaN").is_some() {
    return Some(f64::NAN);
  }
  if s.strip_prefix("-NaN").is_some() {
    return Some(f64::NAN);
  }
  if s.strip_prefix("NaN").is_some() {
    return Some(f64::NAN);
  }
  None
}

fn parse_float_string(text: &str) -> f64 {
  let trimmed = text.trim_start();
  if trimmed.is_empty() {
    return f64::NAN;
  }
  if let Some(v) = parse_special_float(trimmed) {
    return v;
  }
  if let Some(len) = parse_float_prefix_len(trimmed) {
    if len == 0 {
      return f64::NAN;
    }
    trimmed[..len].parse::<f64>().unwrap_or(f64::NAN)
  } else {
    f64::NAN
  }
}

/// Implements the global parseInt function.
pub(crate) fn global_parse_int(
  vm: &mut Vm,
  args: &[Value],
) -> Result<Value, RuntimeError> {
  let undefined = Value::Undefined;
  let value = args.first().unwrap_or(&undefined);
  let radix_arg = if let Some(radix_value) = args.get(1) {
    Some(vm.to_int32_value(radix_value)?)
  } else {
    None
  };
  let s = match value {
    Value::String(s) => s.as_str_lossy(),
    _ => {
      let s = vm.to_string(value)?;
      return Ok(Value::Number(
        parse_int_string(s.as_str_lossy().trim_start(), radix_arg).unwrap_or(f64::NAN),
      ));
    }
  };
  let parsed = parse_int_string(s.trim_start(), radix_arg).unwrap_or(f64::NAN);
  Ok(Value::Number(parsed))
}

/// Implements the global parseFloat function.
pub(crate) fn global_parse_float(
  vm: &mut Vm,
  args: &[Value],
) -> Result<Value, RuntimeError> {
  let undefined = Value::Undefined;
  let value = args.first().unwrap_or(&undefined);
  let s = match value {
    Value::String(s) => s.as_str_lossy(),
    _ => {
      let s = vm.to_string(value)?;
      return Ok(Value::Number(parse_float_string(s.as_str_lossy())));
    }
  };
  let parsed = parse_float_string(s);
  Ok(Value::Number(parsed))
}

#[cfg(test)]
mod tests {
  use super::{js_number_to_string, number_exponentiate};

  #[test]
  fn safe_integer_strings_preserve_sign_and_boundaries() {
    for value in [
      0_i64,
      1,
      -1,
      100_000,
      -100_000,
      4_294_967_295,
      9_007_199_254_740_991,
      -9_007_199_254_740_991,
    ] {
      assert_eq!(js_number_to_string(value as f64), value.to_string());
    }
    assert_eq!(js_number_to_string(-0.0), "0");
    assert_eq!(js_number_to_string(1.25), "1.25");
    assert_eq!(js_number_to_string(1e21), "1e+21");
  }

  #[test]
  fn exponentiation_of_unit_base_by_infinity_is_nan() {
    for base in [-1.0_f64, 1.0_f64] {
      assert!(number_exponentiate(base, f64::INFINITY).is_nan());
      assert!(number_exponentiate(base, f64::NEG_INFINITY).is_nan());
    }
  }

  #[test]
  fn exponentiation_uses_regular_power_for_finite_values() {
    assert_eq!(number_exponentiate(2.0_f64, -2.0_f64), 0.25_f64);
  }
}
