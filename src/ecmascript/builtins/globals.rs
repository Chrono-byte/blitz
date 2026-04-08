use std::rc::Rc;

use crate::compiler;
use crate::parser::Parser;
use crate::runtime_utils::{
    global_decode_uri, global_decode_uri_component, global_encode_uri, global_encode_uri_component,
    global_parse_float, global_parse_int, static_builtin_arg_offset, syntax_error,
};
use crate::value::{ObjectData, PropertyDescriptor, PropertyKey, Value};

fn is_js_line_terminator(c: char) -> bool {
    matches!(c, '\u{000A}' | '\u{000D}' | '\u{2028}' | '\u{2029}')
}

/// Returns `true` when the source contains only whitespace and comments.
///
/// This is a cheap pre-scan for `eval` inputs that lets us skip parsing and
/// compilation entirely for comment-only scripts. That matters for Test262
/// comment conformance cases that call `eval` tens of thousands of times.
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

pub(crate) fn install_global_function_builtins(g: &mut ObjectData) {
    use PropertyKey as PKey;

    g.properties.insert(
        PKey::String("eval".to_string()),
        PropertyDescriptor::data(Value::Builtin(Rc::new(move |vm, args| {
            let off = static_builtin_arg_offset(vm, args, "eval");
            let src = match args.get(off) {
                Some(Value::String(s)) => s.as_str(),
                Some(other) => return Ok(other.clone()),
                None => return Ok(Value::Undefined),
            };
            let trimmed = src.trim_start();
            if trimmed.starts_with('/') {
                if source_is_whitespace_and_comments_only(src) {
                    return Ok(Value::Undefined);
                }
            } else if !trimmed.is_empty()
                && trimmed
                    .chars()
                    .all(|c| c == '_' || c == '$' || c.is_ascii_alphanumeric())
                && matches!(trimmed.chars().next(), Some(c) if c == '_' || c == '$' || c.is_ascii_alphabetic())
            {
                if let Some(value) = vm.resolve_identifier_value(trimmed)? {
                    return Ok(value);
                }
                return Err(vm.throw_reference_error(&format!("{trimmed} is not defined")));
            } else if source_is_whitespace_and_comments_only(src) {
                return Ok(Value::Undefined);
            }
            let strict = vm.active_strict_mode();
            if !strict {
                match vm.eval_expression_source(src) {
                    Ok(value) => return Ok(value),
                    Err(err) => {
                        let msg = err.to_string();
                        if !msg.contains("parse:") && !msg.contains("compile:") {
                            return Err(err);
                        }
                    }
                }
            }
            let eval_src = if strict {
                format!("'use strict';\n{src}")
            } else {
                src.to_string()
            };
            let session = Parser::parse_source(&eval_src)
                .map_err(|e| syntax_error(vm, format!("eval parse: {}", e)))?;
            let prog = session.program();
            let chunk = compiler::compile_oxc(prog, &eval_src)
                .map_err(|e| syntax_error(vm, format!("eval compile: {}", e)))?;
            let mut eval_vm = vm.fork_execution();
            eval_vm.adopt_eval_context(vm);
            eval_vm.set_current_strict_mode(strict);
            match eval_vm.run_nested(chunk) {
                Ok(value) => Ok(value),
                Err(err) => {
                    if let Some(thrown) = eval_vm.last_uncaught_exception.take() {
                        Err(vm.throw_exception(thrown))
                    } else {
                        Err(err)
                    }
                }
            }
        }))),
    );

    g.properties.insert(
        PKey::String("parseInt".to_string()),
        PropertyDescriptor::data(Value::Builtin(Rc::new(global_parse_int))),
    );
    g.properties.insert(
        PKey::String("parseFloat".to_string()),
        PropertyDescriptor::data(Value::Builtin(Rc::new(global_parse_float))),
    );
    g.properties.insert(
        PKey::String("encodeURI".to_string()),
        PropertyDescriptor::data(Value::Builtin(Rc::new(global_encode_uri))),
    );
    g.properties.insert(
        PKey::String("encodeURIComponent".to_string()),
        PropertyDescriptor::data(Value::Builtin(Rc::new(global_encode_uri_component))),
    );
    g.properties.insert(
        PKey::String("decodeURI".to_string()),
        PropertyDescriptor::data(Value::Builtin(Rc::new(global_decode_uri))),
    );
    g.properties.insert(
        PKey::String("decodeURIComponent".to_string()),
        PropertyDescriptor::data(Value::Builtin(Rc::new(global_decode_uri_component))),
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
