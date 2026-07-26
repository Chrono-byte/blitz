// SPDX-License-Identifier: MPL-2.0
//! Language-semantic operations and intrinsic bootstrapping.
use js_string::{JsString, JsStringBuilder};
use std::cell::RefCell;
use std::rc::Rc;

use crate::ecmascript::builtins;
use crate::realm::Intrinsics;
use crate::runtime_utils::{static_builtin_arg_offset, syntax_error, to_length};
use crate::value::{BuiltinFn, ObjectData, PropertyDescriptor, PropertyKey, Value};
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

pub(crate) use byte_views::*;
pub(crate) use intrinsics::{
  builtin_object, install_minimal_kernel_constructors, set_builtin_length, set_builtin_name,
};
pub(crate) use numbers::*;
pub(crate) use properties::*;

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
