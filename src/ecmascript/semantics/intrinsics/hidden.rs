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
        "iteratorToArray".to_string(),
        Value::Builtin(Rc::new(|vm, args| {
            let iterator = args.first().cloned().unwrap_or(Value::Undefined);
            let mut out = Vec::new();
            loop {
                let next =
                    vm.get_property_host(&iterator, &PropertyKey::String("next".to_string()))?;
                let step = vm.call_value_with_this(&next, &iterator, &[])?;
                let done = vm.get_property_host(&step, &PropertyKey::String("done".to_string()))?;
                if vm.to_boolean(&done) {
                    break;
                }
                let value =
                    vm.get_property_host(&step, &PropertyKey::String("value".to_string()))?;
                out.push(value);
            }
            Ok(array_from_values(out))
        })),
    );
    intrinsics.hidden_builtins.insert(
        "setFunctionNameIfAnonymous".to_string(),
        Value::Builtin(Rc::new(|_vm, args| {
            if args.len() < 2 {
                return Ok(args.first().cloned().unwrap_or(Value::Undefined));
            }
            let value = args[0].clone();
            let name = match &args[1] {
                Value::String(s) => s.clone(),
                _ => return Ok(value),
            };
            match &value {
                Value::Closure { props, .. } => {
                    let (has_name, current) = {
                        let props_ref = props.borrow();
                        let key = PropertyKey::String("name".to_string());
                        let current = props_ref.properties.get(&key).and_then(|desc| match desc {
                            PropertyDescriptor::Data {
                                value: Value::String(s),
                                ..
                            } => Some(s.clone()),
                            _ => None,
                        });
                        (props_ref.properties.contains_key(&key), current)
                    };
                    if !has_name
                        || current
                            .as_deref()
                            .is_some_and(|s| s == "<anonymous>" || s == "<arrow>")
                    {
                        props.borrow_mut().properties.insert(
                            PropertyKey::String("name".to_string()),
                            PropertyDescriptor::Data {
                                value: Value::String(name),
                                writable: false,
                                enumerable: false,
                                configurable: true,
                            },
                        );
                    }
                }
                Value::BuiltinObject(_, props) => {
                    let (has_name, current) = {
                        let props_ref = props.borrow();
                        let key = PropertyKey::String("name".to_string());
                        let current = props_ref.properties.get(&key).and_then(|desc| match desc {
                            PropertyDescriptor::Data {
                                value: Value::String(s),
                                ..
                            } => Some(s.clone()),
                            _ => None,
                        });
                        (props_ref.properties.contains_key(&key), current)
                    };
                    if !has_name
                        || current
                            .as_deref()
                            .is_some_and(|s| s == "<anonymous>" || s == "<arrow>")
                    {
                        props.borrow_mut().properties.insert(
                            PropertyKey::String("name".to_string()),
                            PropertyDescriptor::Data {
                                value: Value::String(name),
                                writable: false,
                                enumerable: false,
                                configurable: true,
                            },
                        );
                    }
                }
                _ => {}
            }
            Ok(value)
        })),
    );
    intrinsics.hidden_builtins.insert(
        "objectDefineProperty".to_string(),
        Value::Builtin(Rc::new(Vm::define_property_builtin)),
    );
    intrinsics.hidden_builtins.insert(
        "objectSetPrototypeOf".to_string(),
        Value::Builtin(Rc::new(|vm, args| {
            if args.len() < 2 {
                return Ok(Value::Undefined);
            }
            let target = args[0].clone();
            let proto = args[1].clone();
            match &target {
                Value::Object(obj) => {
                    obj.borrow_mut().set_prototype_value(proto);
                    Ok(target)
                }
                Value::Array(_, props)
                | Value::Closure { props, .. }
                | Value::BuiltinObject(_, props) => {
                    props.borrow_mut().set_prototype_value(proto);
                    Ok(target)
                }
                _ => Err(vm.throw_type_error("Object.setPrototypeOf called on non-object")),
            }
        })),
    );
    intrinsics.hidden_builtins.insert(
        "classInitExtends".to_string(),
        Value::Builtin(Rc::new(|vm, args| {
            if args.len() < 2 {
                return Ok(Value::Undefined);
            }
            let ctor = args[0].clone();
            let superclass = args[1].clone();

            let super_proto = if matches!(superclass, Value::Null) {
                Value::Null
            } else {
                if !superclass.is_constructable() {
                    return Err(
                        vm.throw_type_error("Class extends value is not a constructor or null")
                    );
                }
                let proto = vm.get_property_host(
                    &superclass,
                    &PropertyKey::String("prototype".to_string()),
                )?;
                if !matches!(proto, Value::Null | Value::Object(_) | Value::Array(..)) {
                    return Err(
                        vm.throw_type_error("Superclass prototype must be an object or null")
                    );
                }
                proto
            };

            match &ctor {
                Value::Closure { props, .. } | Value::BuiltinObject(_, props) => {
                    props.borrow_mut().properties.insert(
                        PropertyKey::String("[[SuperConstructor]]".to_string()),
                        PropertyDescriptor::data(superclass.clone()),
                    );
                    if !matches!(superclass, Value::Null) {
                        props.borrow_mut().set_prototype_value(superclass);
                    }
                    let ctor_proto =
                        vm.get_property_host(&ctor, &PropertyKey::String("prototype".to_string()))?;
                    match ctor_proto {
                        Value::Object(obj) => {
                            obj.borrow_mut().set_prototype_value(super_proto);
                        }
                        Value::Array(_, props)
                        | Value::Closure { props, .. }
                        | Value::BuiltinObject(_, props) => {
                            props.borrow_mut().set_prototype_value(super_proto);
                        }
                        _ => {}
                    }
                    Ok(ctor)
                }
                _ => Err(vm.throw_type_error("Class constructor value must be callable")),
            }
        })),
    );
}
