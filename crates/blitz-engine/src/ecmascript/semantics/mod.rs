// SPDX-License-Identifier: MPL-2.0
//! Language-semantic operations and intrinsic bootstrapping.
use js_string::{JsString, JsStringBuilder};
use num_bigint::BigInt as BigIntValue;
use num_traits::ToPrimitive;
use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use crate::ecmascript::builtins;
use crate::realm::Intrinsics;
use crate::runtime_utils::{
  collect_iterable_values, static_builtin_arg_offset, strict_equal, syntax_error, to_index,
  to_integer_or_infinity, to_length,
};
use crate::value::{BuiltinFn, ObjectData, PropertyDescriptor, PropertyKey, SymbolId, Value};
use crate::{Parser, RuntimeError, Vm, compiler};

/// Atomics operations for shared array buffers.
pub mod atomics;
/// Internal backing and branding for byte-oriented ArrayBuffer views.
pub(crate) mod byte_views;
/// ECMAScript intrinsic objects and global bindings.
pub mod intrinsics;
/// Numeric parsing and coercion operations.
pub mod numbers;
/// Property descriptor and enumeration helpers.
pub mod properties;
/// ShadowRealm implementation.
pub mod shadowrealm;
/// URI encoding/decoding functions.
pub mod uri;

pub(crate) use crate::runtime_utils::same_value_zero;
pub(crate) use crate::value::ArrayData;
pub(crate) use byte_views::*;
pub(crate) use intrinsics::{
  builtin_object, install_minimal_kernel_constructors, set_builtin_length, set_builtin_name,
};
pub(crate) use numbers::*;
pub(crate) use properties::*;

fn json_value_identity(value: &Value) -> Option<usize> {
  match value {
    Value::Object(obj) => Some(Rc::as_ptr(obj) as usize),
    Value::Array(arr, _) => Some(Rc::as_ptr(arr) as usize),
    Value::Proxy(proxy) => Some(Rc::as_ptr(proxy) as usize),
    Value::Closure { props, .. } => Some(Rc::as_ptr(props) as usize),
    Value::BuiltinObject(_, props) => Some(Rc::as_ptr(props) as usize),
    Value::Promise(state) => Some(Rc::as_ptr(state) as usize),
    _ => None,
  }
}

/// Creates the argument list for an operation that accepts an array-like object.
///
/// This follows `CreateListFromArrayLike`: the length is read once with ordinary
/// property access and each indexed value is read in order, so holes, accessors,
/// proxies, and abrupt completions retain their ECMAScript behavior.
pub(crate) fn create_list_from_array_like(
  vm: &mut Vm,
  receiver: &Value,
) -> Result<Vec<Value>, RuntimeError> {
  if !receiver.is_object() {
    return Err(vm.throw_type_error("CreateListFromArrayLike requires an object"));
  }

  let length = vm.get_property_host(receiver, &PropertyKey::String("length".to_string().into()))?;
  let length = to_length(vm, &length)?;
  let mut values = Vec::with_capacity(length);
  for index in 0..length {
    values.push(vm.get_property_host(
      receiver,
      &PropertyKey::String(usize_to_decimal_string(index).into()),
    )?);
  }
  Ok(values)
}

fn array_like_length(
  vm: &mut Vm,
  receiver: &Value,
) -> Result<usize, RuntimeError> {
  if let Value::Array(_, props) = receiver {
    let props_ref = props.borrow();
    if let Some(PropertyDescriptor::Data {
      value: Value::Number(len_num),
      ..
    }) = props_ref
      .properties
      .get(&PropertyKey::String("length".to_string().into()))
    {
      let max_safe_len = 9_007_199_254_740_991u128;
      if len_num.is_nan() || *len_num <= 0.0 {
        return Ok(0);
      }
      if !len_num.is_finite() {
        return Ok(max_safe_len as usize);
      }
      let len = len_num
        .trunc()
        .to_u128()
        .unwrap_or(max_safe_len)
        .min(max_safe_len);
      return Ok(len as usize);
    }
  }

  let len_val =
    vm.get_property_host(receiver, &PropertyKey::String("length".to_string().into()))?;
  let len_num = vm.to_number_value(&len_val)?;
  let max_safe_len = 9_007_199_254_740_991u128;
  if len_num.is_nan() || len_num <= 0.0 {
    return Ok(0);
  }
  if !len_num.is_finite() {
    return Ok(max_safe_len as usize);
  }
  let len = len_num
    .trunc()
    .to_u128()
    .unwrap_or(max_safe_len)
    .min(max_safe_len);
  Ok(len as usize)
}

fn collect_indexed_snapshot(
  vm: &mut Vm,
  receiver: &Value,
  len: usize,
) -> Result<(Vec<usize>, HashMap<usize, Value>), RuntimeError> {
  let mut keys = Vec::new();
  let mut values = HashMap::new();
  for key in vm.own_property_keys_value(receiver)? {
    let PropertyKey::String(s) = key else {
      continue;
    };
    let Ok(idx) = s.parse::<usize>() else {
      continue;
    };
    if idx >= len || values.contains_key(&idx) {
      continue;
    }
    let value = vm.get_property_host(receiver, &PropertyKey::String(s))?;
    keys.push(idx);
    values.insert(idx, value);
  }
  keys.sort_unstable();
  Ok((keys, values))
}

fn array_length_is_writable(receiver: &Value) -> bool {
  match receiver {
    Value::Array(_, props) => props
      .borrow()
      .properties
      .get(&PropertyKey::String("length".to_string().into()))
      .is_none_or(|desc| matches!(desc, PropertyDescriptor::Data { writable: true, .. })),
    _ => true,
  }
}

fn clamp_relative_index(
  index: isize,
  len: usize,
) -> usize {
  if index < 0 {
    let back = index.checked_abs().unwrap_or(isize::MAX) as usize;
    len.saturating_sub(back)
  } else {
    (index as usize).min(len)
  }
}

fn json_stringify_value(
  vm: &mut Vm,
  value: &Value,
  seen: &mut HashSet<usize>,
) -> Result<Option<String>, RuntimeError> {
  let result = match value {
    Value::Uninitialized
    | Value::Undefined
    | Value::Builtin(_)
    | Value::BuiltinObject(_, _)
    | Value::Closure { .. } => None,
    Value::Null => Some("null".to_string()),
    Value::Bool(b) => Some(if *b { "true" } else { "false" }.to_string()),
    Value::Number(n) => {
      if n.is_finite() {
        Some(js_number_to_string(*n))
      } else {
        Some("null".to_string())
      }
    }
    Value::String(s) => Some({
      let mut out = String::with_capacity(s.len() + 2);
      out.push('"');
      for unit in s.code_units() {
        match unit {
          0x0022 => out.push_str("\\\""),
          0x005C => out.push_str("\\\\"),
          0x000A => out.push_str("\\n"),
          0x000D => out.push_str("\\r"),
          0x0009 => out.push_str("\\t"),
          0x0008 => out.push_str("\\b"),
          0x000C => out.push_str("\\f"),
          0x0020..=0x007E => {
            out.push(char::from_u32(u32::from(unit)).expect("ASCII is a Unicode scalar"));
          }
          unit => out.push_str(&format!("\\u{unit:04x}")),
        }
      }
      out.push('"');
      out
    }),
    Value::BigInt(_) => {
      return Err(vm.throw_type_error("Do not know how to serialize a BigInt"));
    }
    Value::Symbol(_, _) => None,
    Value::Array(elements, _) => {
      let identity = json_value_identity(value).expect("array has identity");
      if !seen.insert(identity) {
        return Err(vm.throw_type_error("Converting circular structure to JSON"));
      }
      let mut parts = Vec::with_capacity(elements.borrow().len());
      for slot in elements.borrow().iter() {
        match slot {
          Some(item) => match json_stringify_value(vm, item, seen)? {
            Some(json) => parts.push(json),
            None => parts.push("null".to_string()),
          },
          None => parts.push("null".to_string()),
        }
      }
      seen.remove(&identity);
      Some(format!("[{}]", parts.join(",")))
    }
    Value::Object(_) | Value::Proxy(_) | Value::Promise(_) => {
      let identity = json_value_identity(value).expect("object-like value has identity");
      if !seen.insert(identity) {
        return Err(vm.throw_type_error("Converting circular structure to JSON"));
      }
      let mut parts = Vec::new();
      for key in vm.own_enumerable_keys_value(value)? {
        let PropertyKey::String(name) = key else {
          continue;
        };
        let key_value = json_stringify_value(vm, &Value::String(name.clone().into()), seen)?
          .expect("strings stringify");
        let prop_value = vm.get_property_host(value, &PropertyKey::String(name))?;
        if let Some(serialized) = json_stringify_value(vm, &prop_value, seen)? {
          parts.push(format!("{}:{}", key_value, serialized));
        }
      }
      seen.remove(&identity);
      Some(format!("{{{}}}", parts.join(",")))
    }
  };
  Ok(result)
}

pub(crate) fn get_substitution(
  matched: &JsString,
  subject: &JsString,
  start: usize,
  end: usize,
  captures: &[Option<JsString>],
  named_captures: Option<&[(JsString, Option<JsString>)]>,
  replacement: &JsString,
) -> JsString {
  let mut output = JsStringBuilder::with_capacity(replacement.len());
  let units = replacement.as_slice();
  let mut cursor = 0;
  while cursor < units.len() {
    let unit = units[cursor];
    cursor += 1;
    if unit != u16::from(b'$') {
      output.push_code_unit(unit);
      continue;
    }
    match units.get(cursor).copied() {
      Some(next) if next == u16::from(b'$') => {
        cursor += 1;
        output.push_code_unit(u16::from(b'$'));
      }
      Some(next) if next == u16::from(b'&') => {
        cursor += 1;
        output.push_string(matched);
      }
      Some(next) if next == u16::from(b'`') => {
        cursor += 1;
        output.push_string(
          &subject
            .slice(0..start)
            .expect("match start is within the subject"),
        );
      }
      Some(next) if next == u16::from(b'\'') => {
        cursor += 1;
        output.push_string(
          &subject
            .slice(end..subject.len())
            .expect("match end is within the subject"),
        );
      }
      Some(digit) if (u16::from(b'1')..=u16::from(b'9')).contains(&digit) => {
        cursor += 1;
        let mut index = usize::from(digit - u16::from(b'0'));
        if let Some(next) = units.get(cursor).copied()
          && (u16::from(b'0')..=u16::from(b'9')).contains(&next)
        {
          let two_digit = index * 10 + usize::from(next - u16::from(b'0'));
          if two_digit <= captures.len() {
            cursor += 1;
            index = two_digit;
          }
        }
        if index <= captures.len() {
          if let Some(capture) = &captures[index - 1] {
            output.push_string(capture);
          }
        } else {
          output.push_code_unit(u16::from(b'$'));
          output.push_code_unit(digit);
        }
      }
      Some(next) if next == u16::from(b'<') && named_captures.is_some() => {
        let name_start = cursor + 1;
        let Some(relative_end) = units[name_start..]
          .iter()
          .position(|unit| *unit == u16::from(b'>'))
        else {
          output.push_code_unit(u16::from(b'$'));
          continue;
        };
        let name_end = name_start + relative_end;
        let name = JsString::from_utf16(units[name_start..name_end].to_vec());
        cursor = name_end + 1;
        if let Some(value) = named_captures
          .and_then(|captures| {
            captures
              .iter()
              .find(|(candidate, _)| candidate == &name)
          })
          .and_then(|(_, value)| value.as_ref())
        {
          output.push_string(value);
        }
      }
      _ => output.push_code_unit(u16::from(b'$')),
    }
  }
  output.finish()
}
