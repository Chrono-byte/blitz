// SPDX-License-Identifier: MPL-2.0

use std::rc::Rc;

use js_string::JsString;

use crate::compiler;
use crate::parser::Parser;
use crate::runtime_utils::{
  global_decode_uri, global_decode_uri_component, global_encode_uri, global_encode_uri_component,
  global_parse_float, global_parse_int, static_builtin_arg_offset, syntax_error,
};
use crate::value::{ObjectData, PropertyDescriptor, PropertyKey, Value};

fn annex_b_escape(
  vm: &mut crate::Vm,
  args: &[Value],
) -> Result<Value, crate::RuntimeError> {
  let value = args.first().cloned().unwrap_or(Value::Undefined);
  let string = vm.to_string(&value)?;
  let mut output = String::new();
  for unit in string.code_units() {
    if matches!(unit, 0x41..=0x5A | 0x61..=0x7A | 0x30..=0x39)
      || matches!(unit, 0x40 | 0x2A | 0x5F | 0x2B | 0x2D | 0x2E | 0x2F)
    {
      output.push(char::from_u32(u32::from(unit)).expect("ASCII escape unit"));
    } else if unit < 0x100 {
      output.push_str(&format!("%{unit:02X}"));
    } else {
      output.push_str(&format!("%u{unit:04X}"));
    }
  }
  Ok(Value::String(output.into()))
}

fn annex_b_unescape(
  vm: &mut crate::Vm,
  args: &[Value],
) -> Result<Value, crate::RuntimeError> {
  let value = args.first().cloned().unwrap_or(Value::Undefined);
  let string = vm.to_string(&value)?;
  let units = string.code_units().collect::<Vec<_>>();
  let mut output = Vec::with_capacity(units.len());
  let hex = |unit: u16| match unit {
    0x30..=0x39 => Some(unit - 0x30),
    0x41..=0x46 => Some(unit - 0x41 + 10),
    0x61..=0x66 => Some(unit - 0x61 + 10),
    _ => None,
  };
  let mut index = 0;
  while index < units.len() {
    if units[index] == 0x25 {
      let (digits, offset) = if units.get(index + 1) == Some(&0x75) {
        (4, 2)
      } else {
        (2, 1)
      };
      if index + offset + digits <= units.len() {
        let decoded = units[index + offset..index + offset + digits]
          .iter()
          .copied()
          .try_fold(0_u16, |value, unit| {
            hex(unit).map(|digit| value * 16 + digit)
          });
        if let Some(decoded) = decoded {
          output.push(decoded);
          index += offset + digits;
          continue;
        }
      }
    }
    output.push(units[index]);
    index += 1;
  }
  Ok(Value::String(JsString::from_utf16(output)))
}

fn is_js_line_terminator(c: char) -> bool {
  matches!(c, '\u{000A}' | '\u{000D}' | '\u{2028}' | '\u{2029}')
}

fn source_is_whitespace_and_comments_only(src: &str) -> bool {
  let mut chars = src.chars().peekable();

  loop {
    while chars.peek().is_some_and(|c| c.is_whitespace()) {
      chars.next();
    }

    match chars.peek().copied() {
      None => return true,
      Some('/') => {
        chars.next();
        match chars.peek().copied() {
          Some('/') => {
            chars.next();
            for c in chars.by_ref() {
              if is_js_line_terminator(c) {
                break;
              }
            }
          }
          Some('*') => {
            chars.next();
            let mut prev = '\0';
            let mut closed = false;
            for c in chars.by_ref() {
              if prev == '*' && c == '/' {
                closed = true;
                break;
              }
              prev = c;
            }
            if !closed {
              return false;
            }
          }
          _ => return false,
        }
      }
      Some(_) => return false,
    }
  }
}

pub(crate) fn install_global_function_builtins(
  g: &mut ObjectData,
  function_proto: &Rc<std::cell::RefCell<ObjectData>>,
) {
  use PropertyKey as PKey;

  g.properties.insert(
    PKey::String("eval".to_string().into()),
    PropertyDescriptor::builtin_data(crate::ecmascript::semantics::builtin_object(
      Rc::new(move |vm, args| {
        let off = static_builtin_arg_offset(vm, args, "eval");
        let src = match args.get(off) {
          Some(Value::String(s)) => s.as_str_lossy(),
          Some(other) => return Ok(other.clone()),
          None => return Ok(Value::Undefined),
        };
        if source_is_whitespace_and_comments_only(src) {
          return Ok(Value::Undefined);
        }
        let code = crate::typecheck::with_strict_override(None, || {
          compiler::with_compile_context_override(Some((false, false)), || {
            let session = Parser::parse_source(src)
              .map_err(|e| syntax_error(vm, format!("eval parse: {}", e)))?;
            compiler::compile_repl_session(&session)
              .map_err(|e| syntax_error(vm, format!("eval compile: {}", e)))
          })
        })?;
        if !code.root_strict
          && let Some(name) = vm.global_eval_lexical_conflict_name(&code)
        {
          return Err(syntax_error(
            vm,
            format!("redeclaration of global lexical binding `{}`", name),
          ));
        }
        if !code.root_strict
          && let Some(name) = vm.global_function_declaration_conflict_name(&code)
        {
          return Err(vm.throw_type_error(&format!("cannot declare global function `{}`", name,)));
        }
        match vm.run_nested(code, false) {
          Ok(value) => Ok(value),
          Err(failure) => {
            if let Some(thrown) = failure.thrown {
              Err(vm.throw_exception(thrown))
            } else {
              Err(failure.error)
            }
          }
        }
      }),
      function_proto,
    )),
  );

  g.properties.insert(
    PKey::String("parseInt".to_string().into()),
    PropertyDescriptor::builtin_data(crate::ecmascript::semantics::builtin_object(
      Rc::new(global_parse_int),
      function_proto,
    )),
  );
  g.properties.insert(
    PKey::String("parseFloat".to_string().into()),
    PropertyDescriptor::builtin_data(crate::ecmascript::semantics::builtin_object(
      Rc::new(global_parse_float),
      function_proto,
    )),
  );
  g.properties.insert(
    PKey::String("encodeURI".to_string().into()),
    PropertyDescriptor::builtin_data(crate::ecmascript::semantics::builtin_object(
      Rc::new(global_encode_uri),
      function_proto,
    )),
  );
  g.properties.insert(
    PKey::String("encodeURIComponent".to_string().into()),
    PropertyDescriptor::builtin_data(crate::ecmascript::semantics::builtin_object(
      Rc::new(global_encode_uri_component),
      function_proto,
    )),
  );
  g.properties.insert(
    PKey::String("decodeURI".to_string().into()),
    PropertyDescriptor::builtin_data(crate::ecmascript::semantics::builtin_object(
      Rc::new(global_decode_uri),
      function_proto,
    )),
  );
  g.properties.insert(
    PKey::String("decodeURIComponent".to_string().into()),
    PropertyDescriptor::builtin_data(crate::ecmascript::semantics::builtin_object(
      Rc::new(global_decode_uri_component),
      function_proto,
    )),
  );
  g.properties.insert(
    PKey::String("escape".into()),
    PropertyDescriptor::builtin_data(crate::ecmascript::semantics::builtin_object(
      Rc::new(annex_b_escape),
      function_proto,
    )),
  );
  g.properties.insert(
    PKey::String("unescape".into()),
    PropertyDescriptor::builtin_data(crate::ecmascript::semantics::builtin_object(
      Rc::new(annex_b_unescape),
      function_proto,
    )),
  );
}

#[cfg(test)]
mod tests {
  use super::source_is_whitespace_and_comments_only;

  #[test]
  fn detects_comment_only_sources() {
    assert!(source_is_whitespace_and_comments_only("/* comment */"));
    assert!(source_is_whitespace_and_comments_only("// comment\n"));
    assert!(source_is_whitespace_and_comments_only(" \t/* a */\n// b"));
  }

  #[test]
  fn rejects_sources_with_code() {
    assert!(!source_is_whitespace_and_comments_only("x"));
    assert!(!source_is_whitespace_and_comments_only("/* comment */ x"));
    assert!(!source_is_whitespace_and_comments_only("// comment\nx"));
  }

  #[test]
  fn treats_unterminated_block_comment_as_non_comment_only() {
    assert!(!source_is_whitespace_and_comments_only("/* comment"));
  }
}
