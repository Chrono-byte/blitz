use crate::errors::RuntimeError;
use crate::value::Value;
use crate::vm::Vm;
use num_bigint::BigInt as BigIntValue;
use num_traits::{Signed, ToPrimitive};

pub(super) fn to_number(v: &Value) -> Result<f64, RuntimeError> {
    match v {
        Value::Number(n) => Ok(*n),
        Value::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
        Value::String(s) => Ok(parse_js_number_string(s)),
        Value::Undefined => Ok(f64::NAN),
        Value::Null => Ok(0.0),
        Value::BigInt(n) => Ok(n.to_f64().unwrap_or_else(|| {
            if n.is_negative() {
                f64::NEG_INFINITY
            } else {
                f64::INFINITY
            }
        })),
        _ => Err(RuntimeError::from("Expected number")),
    }
}

pub(super) fn to_int32_with_vm(vm: &mut Vm, v: &Value) -> Result<i32, RuntimeError> {
    let u = to_uint32_with_vm(vm, v)?;
    if u & 0x8000_0000 != 0 {
        Ok(((u as i64) - 0x1_0000_0000) as i32)
    } else {
        Ok(u as i32)
    }
}

pub(super) fn to_uint32_with_vm(vm: &mut Vm, v: &Value) -> Result<u32, RuntimeError> {
    let n = vm.to_number_value(v)?;

    if n.is_nan() || n == 0.0 || n.is_infinite() {
        return Ok(0);
    }

    const TWO32: f64 = 4294967296.0;
    let int_part = n.trunc();
    let vmod = int_part.rem_euclid(TWO32);
    Ok(vmod as u32)
}

pub(super) fn parse_js_number_string(s: &str) -> f64 {
    let s = s.trim();
    if s.is_empty() {
        return 0.0;
    }
    if matches!(s, "Infinity" | "+Infinity") {
        return f64::INFINITY;
    }
    if s == "-Infinity" {
        return f64::NEG_INFINITY;
    }

    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        return parse_radix_number_string(hex, 16).unwrap_or(f64::NAN);
    }
    if let Some(bin) = s.strip_prefix("0b").or_else(|| s.strip_prefix("0B")) {
        return parse_radix_number_string(bin, 2).unwrap_or(f64::NAN);
    }
    if let Some(oct) = s.strip_prefix("0o").or_else(|| s.strip_prefix("0O")) {
        return parse_radix_number_string(oct, 8).unwrap_or(f64::NAN);
    }

    let (sign, body) = match s.as_bytes().first() {
        Some(b'+') => (1.0, &s[1..]),
        Some(b'-') => (-1.0, &s[1..]),
        _ => (1.0, s),
    };

    sign * body.parse::<f64>().unwrap_or(f64::NAN)
}

fn parse_radix_number_string(body: &str, radix: u32) -> Option<f64> {
    if body.is_empty() {
        return None;
    }

    let mut value = 0.0;
    let radix_f = radix as f64;

    for byte in body.bytes() {
        let digit = match byte {
            b'0'..=b'9' => u32::from(byte - b'0'),
            b'a'..=b'z' => u32::from(byte - b'a' + 10),
            b'A'..=b'Z' => u32::from(byte - b'A' + 10),
            _ => return None,
        };
        if digit >= radix {
            return None;
        }
        value = value * radix_f + digit as f64;
    }

    Some(value)
}

pub(super) fn parse_js_bigint_string(s: &str) -> Option<BigIntValue> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let (neg, body) = match s.as_bytes().first() {
        Some(b'+') => (false, &s[1..]),
        Some(b'-') => (true, &s[1..]),
        _ => (false, s),
    };
    let mut parsed = if let Some(hex) = body.strip_prefix("0x").or_else(|| body.strip_prefix("0X"))
    {
        BigIntValue::parse_bytes(hex.as_bytes(), 16)?
    } else if let Some(bin) = body.strip_prefix("0b").or_else(|| body.strip_prefix("0B")) {
        BigIntValue::parse_bytes(bin.as_bytes(), 2)?
    } else if let Some(oct) = body.strip_prefix("0o").or_else(|| body.strip_prefix("0O")) {
        BigIntValue::parse_bytes(oct.as_bytes(), 8)?
    } else {
        BigIntValue::parse_bytes(body.as_bytes(), 10)?
    };
    if neg {
        parsed = -parsed;
    }
    Some(parsed)
}

impl Vm {
    /// Convert a runtime value with ECMAScript `ToInt32` semantics.
    pub fn to_int32_value(&mut self, v: &Value) -> Result<i32, RuntimeError> {
        to_int32_with_vm(self, v)
    }

    /// Convert a runtime value with ECMAScript `ToUint32` semantics.
    pub fn to_uint32_value(&mut self, v: &Value) -> Result<u32, RuntimeError> {
        to_uint32_with_vm(self, v)
    }
}
