// SPDX-License-Identifier: MPL-2.0
use crate::value::Value;
use crate::{RuntimeError, Vm};
use js_string::{JsString, JsStringBuilder};

fn is_uri_component_safe(
  ch: char,
  component: bool,
) -> bool {
  matches!(
      ch,
      'A'..='Z'
          | 'a'..='z'
          | '0'..='9'
          | '-'
          | '_'
          | '.'
          | '!'
          | '~'
          | '*'
          | '\''
          | '('
          | ')'
  ) || (!component
    && matches!(
      ch,
      ',' | ';' | '/' | '?' | ':' | '@' | '&' | '=' | '+' | '$' | '#'
    ))
}

fn percent_encode(
  vm: &mut Vm,
  text: &JsString,
  component: bool,
) -> Result<String, RuntimeError> {
  let mut out = String::with_capacity(text.len() * 3);
  const HEX: &[u8; 16] = b"0123456789ABCDEF";
  let mut index = 0;
  while let Some(point) = text.code_point_at(index, true) {
    let Some(ch) = char::from_u32(point.value()) else {
      return Err(uri_error(vm));
    };
    if is_uri_component_safe(ch, component) {
      out.push(ch);
    } else {
      let mut buf = [0u8; 4];
      for b in ch.encode_utf8(&mut buf).as_bytes() {
        out.push('%');
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0F) as usize] as char);
      }
    }
    index += point.width();
  }
  Ok(out)
}

fn uri_error(vm: &mut Vm) -> RuntimeError {
  let err = vm
    .create_intrinsic_error("URIError", "malformed URI sequence")
    .unwrap_or(Value::String("URIError".to_string().into()));
  vm.throw_exception(err)
}

fn percent_decode(
  vm: &mut Vm,
  text: &JsString,
  component: bool,
) -> Result<JsString, RuntimeError> {
  let input = text.as_slice();
  let mut output = JsStringBuilder::with_capacity(input.len());
  let mut i = 0usize;
  while i < input.len() {
    let unit = input[i];
    if unit != u16::from(b'%') {
      output.push_code_unit(unit);
      i += 1;
      continue;
    }
    let first = decode_escape_byte(input, i).ok_or_else(|| uri_error(vm))?;
    if first < 0x80 {
      if !component && is_decode_uri_reserved_byte(first) {
        output.push_code_unit(u16::from(b'%'));
        output.push_code_unit(input[i + 1]);
        output.push_code_unit(input[i + 2]);
      } else {
        output.push_code_unit(u16::from(first));
      }
      i += 3;
      continue;
    }

    let byte_count = match first {
      0xC2..=0xDF => 2,
      0xE0..=0xEF => 3,
      0xF0..=0xF4 => 4,
      _ => return Err(uri_error(vm)),
    };
    let mut bytes = Vec::with_capacity(byte_count);
    bytes.push(first);
    for offset in 1..byte_count {
      let escape_index = i + offset * 3;
      let byte = decode_escape_byte(input, escape_index).ok_or_else(|| uri_error(vm))?;
      bytes.push(byte);
    }
    let decoded = std::str::from_utf8(&bytes).map_err(|_| uri_error(vm))?;
    let mut characters = decoded.chars();
    let character = characters.next().ok_or_else(|| uri_error(vm))?;
    if characters.next().is_some() {
      return Err(uri_error(vm));
    }
    if !output.push_code_point(character.into()) {
      return Err(uri_error(vm));
    }
    i += byte_count * 3;
  }
  Ok(output.finish())
}

fn decode_escape_byte(
  input: &[u16],
  index: usize,
) -> Option<u8> {
  if input.get(index) != Some(&u16::from(b'%')) {
    return None;
  }
  let hi = hex_value(*input.get(index + 1)?)?;
  let lo = hex_value(*input.get(index + 2)?)?;
  Some((hi << 4) | lo)
}

fn hex_value(unit: u16) -> Option<u8> {
  match unit {
    value if value == u16::from(b'0') => Some(0),
    value if (u16::from(b'1')..=u16::from(b'9')).contains(&value) => {
      Some((value - u16::from(b'0')) as u8)
    }
    value if (u16::from(b'a')..=u16::from(b'f')).contains(&value) => {
      Some((value - u16::from(b'a') + 10) as u8)
    }
    value if (u16::from(b'A')..=u16::from(b'F')).contains(&value) => {
      Some((value - u16::from(b'A') + 10) as u8)
    }
    _ => None,
  }
}

fn is_decode_uri_reserved_byte(byte: u8) -> bool {
  matches!(
    byte,
    b';' | b'/' | b'?' | b':' | b'@' | b'&' | b'=' | b'+' | b'$' | b'#'
  )
}

pub(crate) fn global_decode_uri(
  vm: &mut Vm,
  args: &[Value],
) -> Result<Value, RuntimeError> {
  let undefined = Value::Undefined;
  let value = args.first().unwrap_or(&undefined);
  let s = vm.to_string(value)?;
  Ok(Value::String(percent_decode(vm, &s, false)?))
}

pub(crate) fn global_decode_uri_component(
  vm: &mut Vm,
  args: &[Value],
) -> Result<Value, RuntimeError> {
  let undefined = Value::Undefined;
  let value = args.first().unwrap_or(&undefined);
  let s = vm.to_string(value)?;
  Ok(Value::String(percent_decode(vm, &s, true)?))
}

pub(crate) fn global_encode_uri(
  vm: &mut Vm,
  args: &[Value],
) -> Result<Value, RuntimeError> {
  let undefined = Value::Undefined;
  let value = args.first().unwrap_or(&undefined);
  let s = vm.to_string(value)?;
  Ok(Value::String(percent_encode(vm, &s, false)?.into()))
}

pub(crate) fn global_encode_uri_component(
  vm: &mut Vm,
  args: &[Value],
) -> Result<Value, RuntimeError> {
  let undefined = Value::Undefined;
  let value = args.first().unwrap_or(&undefined);
  let s = vm.to_string(value)?;
  Ok(Value::String(percent_encode(vm, &s, true)?.into()))
}
