use std::borrow::Cow;

use crate::value::{PropertyDescriptor, PropertyKey, Value};
use crate::{RuntimeError, Vm};

fn is_uri_component_safe(ch: char, component: bool) -> bool {
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
            | ';'
            | ','
            | '/'
            | '?'
            | ':'
            | '@'
            | '&'
            | '='
            | '+'
            | '$'
            | '#'
    ) || (!component
        && matches!(
            ch,
            ',' | ';' | '/' | '?' | ':' | '@' | '&' | '=' | '+' | '$' | '#'
        ))
}

fn percent_encode(text: &str, component: bool) -> String {
    let mut out = String::with_capacity(text.len() * 3);
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for ch in text.chars() {
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
    }
    out
}

fn uri_error(vm: &mut Vm) -> RuntimeError {
    let err = vm
        .create_intrinsic_error("URIError", "malformed URI sequence")
        .unwrap_or(Value::String("URIError".to_string()));
    vm.throw_exception(err)
}

fn percent_decode(vm: &mut Vm, text: &str, component: bool) -> Result<String, RuntimeError> {
    let input = text.as_bytes();
    let mut bytes = Vec::with_capacity(input.len());
    let mut i = 0usize;
    while i < input.len() {
        let byte = input[i];
        if byte != b'%' {
            bytes.push(byte);
            i += 1;
            continue;
        }
        if i + 2 >= input.len() {
            return Err(uri_error(vm));
        }
        let Some(hi) = hex_value(input[i + 1]) else {
            return Err(uri_error(vm));
        };
        let Some(lo) = hex_value(input[i + 2]) else {
            return Err(uri_error(vm));
        };
        let decoded = (hi << 4) | lo;
        if !component && is_decode_uri_reserved_byte(decoded) {
            bytes.extend_from_slice(&input[i..i + 3]);
        } else {
            bytes.push(decoded);
        }
        i += 3;
    }
    Ok(unsafe { String::from_utf8_unchecked(bytes) })
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn is_decode_uri_reserved_byte(byte: u8) -> bool {
    matches!(
        byte,
        b';' | b'/' | b'?' | b':' | b'@' | b'&' | b'=' | b'+' | b'$' | b'#'
    )
}

/// Implements the global decodeURI function.
pub(crate) fn global_decode_uri(vm: &mut Vm, args: &[Value]) -> Result<Value, RuntimeError> {
    let undefined = Value::Undefined;
    let value = args.first().unwrap_or(&undefined);
    let s = match value {
        Value::String(s) => s.as_str(),
        _ => {
            let s = vm.to_string(value)?;
            return Ok(Value::String(percent_decode(vm, &s, false)?));
        }
    };
    Ok(Value::String(percent_decode(vm, s, false)?))
}

/// Implements the global decodeURI function.
/// Implements the global decodeURIComponent function.
pub(crate) fn global_decode_uri_component(
    vm: &mut Vm,
    args: &[Value],
) -> Result<Value, RuntimeError> {
    let undefined = Value::Undefined;
    let value = args.first().unwrap_or(&undefined);
    let s = match value {
        Value::String(s) => s.as_str(),
        _ => {
            let s = vm.to_string(value)?;
            return Ok(Value::String(percent_decode(vm, &s, true)?));
        }
    };
    Ok(Value::String(percent_decode(vm, s, true)?))
}

/// Implements the global encodeURI function.
pub(crate) fn global_encode_uri(vm: &mut Vm, args: &[Value]) -> Result<Value, RuntimeError> {
    let undefined = Value::Undefined;
    let value = args.first().unwrap_or(&undefined);
    let s = match value {
        Value::String(s) => s.as_str(),
        _ => {
            let s = vm.to_string(value)?;
            return Ok(Value::String(percent_encode(&s, false)));
        }
    };
    Ok(Value::String(percent_encode(s, false)))
}

/// Implements the global encodeURI function.
/// Implements the global encodeURIComponent function.
pub(crate) fn global_encode_uri_component(
    vm: &mut Vm,
    args: &[Value],
) -> Result<Value, RuntimeError> {
    let undefined = Value::Undefined;
    let value = args.first().unwrap_or(&undefined);
    let s = match value {
        Value::String(s) => s.as_str(),
        _ => {
            let s = vm.to_string(value)?;
            return Ok(Value::String(percent_encode(&s, true)));
        }
    };
    Ok(Value::String(percent_encode(s, true)))
}

/// Gets a string value reference from the String prototype methods.
pub(crate) fn this_string_value_ref<'a>(
    vm: &mut Vm,
    value: &'a Value,
) -> Result<Cow<'a, str>, RuntimeError> {
    match value {
        Value::String(s) => Ok(Cow::Borrowed(s)),
        Value::Object(obj) => {
            if let Some(PropertyDescriptor::Data { value, .. }) = obj
                .borrow()
                .properties
                .get(&PropertyKey::String("[[PrimitiveValue]]".to_string()))
                && let Value::String(s) = value
            {
                return Ok(Cow::Owned(s.clone()));
            }
            Err(vm.throw_type_error("String method called on incompatible receiver"))
        }
        _ => Err(vm.throw_type_error("String method called on incompatible receiver")),
    }
}
