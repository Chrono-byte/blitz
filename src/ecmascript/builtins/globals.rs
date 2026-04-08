use std::rc::Rc;

use crate::compiler;
use crate::parser::Parser;
use crate::runtime_utils::{
    global_decode_uri, global_decode_uri_component, global_encode_uri, global_encode_uri_component,
    global_parse_float, global_parse_int, static_builtin_arg_offset, syntax_error,
};
use crate::value::{ObjectData, PropertyDescriptor, PropertyKey, Value};
use crate::vm::Vm;

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
            let trimmed = src.trim();
            if !trimmed.is_empty()
                && trimmed
                    .chars()
                    .all(|c| c == '_' || c == '$' || c.is_ascii_alphanumeric())
                && matches!(trimmed.chars().next(), Some(c) if c == '_' || c == '$' || c.is_ascii_alphabetic())
            {
                if let Some(value) = vm.resolve_identifier_value(trimmed)? {
                    return Ok(value);
                }
                return Err(vm.throw_reference_error(&format!("{trimmed} is not defined")));
            }
            match vm.eval_expression_source(src) {
                Ok(value) => return Ok(value),
                Err(err) => {
                    let msg = err.to_string();
                    if !msg.contains("parse:") && !msg.contains("compile:") {
                        return Err(err);
                    }
                }
            }
            let session = Parser::parse_source(src)
                .map_err(|e| syntax_error(vm, format!("eval parse: {}", e)))?;
            let prog = session.program();
            let chunk = compiler::compile_oxc(prog, src)
                .map_err(|e| syntax_error(vm, format!("eval compile: {}", e)))?;
            let mut eval_vm = Vm::new(vm.globals_rc());
            eval_vm.run(chunk)
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
