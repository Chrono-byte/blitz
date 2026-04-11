use super::*;

pub(crate) fn install_hidden_builtins(intrinsics: &mut Intrinsics) {
    intrinsics.hidden_builtins.insert(
        "dynamicImport".to_string(),
        Value::Builtin(Rc::new(|vm, args| {
            crate::runtime_utils::dynamic_import_value(vm, args)
        })),
    );
    intrinsics.hidden_builtins.insert(
        "throwTypeError".to_string(),
        Value::Builtin(Rc::new(|vm, _args| {
            Err(vm.throw_type_error("Cannot assign to imported binding"))
        })),
    );
    intrinsics.hidden_builtins.insert(
        "directEval".to_string(),
        Value::Builtin(Rc::new(|vm, args| {
            let src = match args.first() {
                Some(Value::String(s)) => s.as_str(),
                Some(other) => return Ok(other.clone()),
                None => return Ok(Value::Undefined),
            };
            let eval_src = if vm.active_strict_mode() {
                format!("'use strict';\n{src}")
            } else {
                src.to_string()
            };
            let (allow_new_target, allow_super) = vm.current_eval_context_flags();
            let chunk = crate::typecheck::with_strict_override(None, || {
                compiler::with_compile_context_override(
                    Some((allow_new_target, allow_super)),
                    || {
                        let session = Parser::parse_source(&eval_src)
                            .map_err(|e| syntax_error(vm, format!("eval parse: {}", e)))?;
                        let prog = session.program();
                        compiler::compile_oxc_repl(prog, &eval_src)
                            .map_err(|e| syntax_error(vm, format!("eval compile: {}", e)))
                    },
                )
            })?;
            if let Some(name) = vm.direct_eval_lexical_conflict_name(&chunk) {
                return Err(syntax_error(
                    vm,
                    format!("redeclaration of lexical binding `{}`", name),
                ));
            }
            match vm.run_nested(chunk, true) {
                Ok(value) => Ok(value),
                Err(err) => {
                    if let Some(thrown) = vm.last_uncaught_exception.take() {
                        Err(vm.throw_exception(thrown))
                    } else {
                        Err(err)
                    }
                }
            }
        })),
    );
    intrinsics.hidden_builtins.insert(
        "requireObjectCoercible".to_string(),
        Value::Builtin(Rc::new(|vm, args| {
            let value = args.first().cloned().unwrap_or(Value::Undefined);
            vm.to_object_value(value.clone())?;
            Ok(value)
        })),
    );
    intrinsics.hidden_builtins.insert(
        "forInKeys".to_string(),
        Value::Builtin(Rc::new(|vm, args| {
            let target = args.first().cloned().unwrap_or(Value::Undefined);
            let keys = vm.for_in_keys_of_value(&target)?;
            Ok(array_from_values(
                keys.into_iter().map(Value::String).collect(),
            ))
        })),
    );
    intrinsics.hidden_builtins.insert(
        "objectDefineProperty".to_string(),
        Value::Builtin(Rc::new(Vm::define_property_builtin)),
    );
}
