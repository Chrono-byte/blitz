// SPDX-License-Identifier: MPL-2.0
use super::*;

pub(crate) fn install_hidden_builtins(
  intrinsics: &mut Intrinsics,
  intrinsic_eval: Value,
) {
  intrinsics.hidden_builtins.insert(
    "importMeta".to_string(),
    Value::Builtin(Rc::new(|vm, _args| {
      let Some(specifier) = vm.current_module_specifier().map(str::to_owned) else {
        return Err(vm.throw_type_error("import.meta is only available in a module"));
      };
      vm.module_loader_handle()
        .borrow_mut()
        .import_meta_for(&specifier)
        .ok_or_else(|| vm.throw_type_error("module record is unavailable for import.meta"))
    })),
  );
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
  let direct_eval: BuiltinFn = Rc::new(|vm, args| {
    let src = match args.first() {
      Some(Value::String(s)) => s.as_str_lossy(),
      Some(other) => return Ok(other.clone()),
      None => return Ok(Value::Undefined),
    };
    let inherited_strict = vm.active_strict_mode();
    let eval_src = if inherited_strict {
      crate::js!(format(script), "'use strict'; void 0;\n{src}")
    } else {
      src.to_string()
    };
    let (allow_new_target, allow_super, allow_super_call) = vm.current_eval_context_flags();
    let (code, declaration_only) = crate::typecheck::with_strict_override(None, || {
      compiler::with_compile_context_override(Some((allow_new_target, allow_super)), || {
        let session = Parser::parse_eval_source(&eval_src, allow_new_target)
          .map_err(|e| syntax_error(vm, format!("eval parse: {}", e)))?;
        if !allow_super_call
          && compiler::Compiler::statements_contain_direct_super_call(&session.program().body)
        {
          return Err(syntax_error(
            vm,
            "super() is only valid in a constructor".to_string(),
          ));
        }
        let declaration_only = session.program().body.iter().all(|statement| {
          matches!(
            statement,
            oxc_ast::ast::Statement::FunctionDeclaration(_)
              | oxc_ast::ast::Statement::EmptyStatement(_)
          )
        }) && (!inherited_strict || session.program().directives.len() == 1);
        let code = compiler::compile_repl_session(&session)
          .map_err(|e| syntax_error(vm, format!("eval compile: {}", e)))?;
        Ok::<_, crate::errors::RuntimeError>((code, declaration_only))
      })
    })?;
    if !code.root_strict
      && let Some(name) = vm.direct_eval_lexical_conflict_name(&code)
    {
      return Err(syntax_error(
        vm,
        format!("redeclaration of lexical binding `{}`", name),
      ));
    }
    if !code.root_strict
      && vm.direct_eval_uses_global_var_environment()
      && let Some(name) = vm.global_function_declaration_conflict_name(&code)
    {
      return Err(vm.throw_type_error(&format!("cannot declare global function `{}`", name,)));
    }
    match vm.run_nested(code, true) {
      Ok(value) => Ok(if declaration_only {
        Value::Undefined
      } else {
        value
      }),
      Err(failure) => {
        if let Some(thrown) = failure.thrown {
          Err(vm.throw_exception(thrown))
        } else {
          Err(failure.error)
        }
      }
    }
  });
  intrinsics.hidden_builtins.insert(
    "directEval".to_string(),
    Value::Builtin(Rc::clone(&direct_eval)),
  );
  let intrinsic_eval_for_test = intrinsic_eval.clone();
  intrinsics.hidden_builtins.insert(
    "isIntrinsicEval".to_string(),
    Value::Builtin(Rc::new(move |_vm, args| {
      Ok(Value::Bool(crate::runtime_utils::strict_equal(
        args.first().unwrap_or(&Value::Undefined),
        &intrinsic_eval_for_test,
      )))
    })),
  );
  intrinsics.hidden_builtins.insert(
    "callEvalIdentifier".to_string(),
    Value::Builtin(Rc::new(move |vm, args| {
      let callee = args.first().cloned().unwrap_or(Value::Undefined);
      let direct = crate::runtime_utils::strict_equal(&callee, &intrinsic_eval);
      if direct {
        direct_eval(vm, args.get(1..).unwrap_or_default())
      } else {
        vm.call_value_with_this(
          &callee,
          &Value::Undefined,
          args.get(1..).unwrap_or_default(),
        )
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
    "resolveWithBindingObject".to_string(),
    Value::Builtin(Rc::new(|vm, args| {
      let name = vm.to_string(args.first().unwrap_or(&Value::Undefined))?;
      Ok(
        vm.resolve_with_binding_object(name.as_str_lossy())?
          .unwrap_or(Value::Undefined),
      )
    })),
  );
  intrinsics.hidden_builtins.insert(
    "getResolvedIdentifier".to_string(),
    Value::Builtin(Rc::new(|vm, args| {
      let binding_object = args.first().cloned().unwrap_or(Value::Undefined);
      let name = vm.to_string(args.get(1).unwrap_or(&Value::Undefined))?;
      vm.get_resolved_identifier(&binding_object, name.as_str_lossy())
    })),
  );
  intrinsics.hidden_builtins.insert(
    "setResolvedIdentifier".to_string(),
    Value::Builtin(Rc::new(|vm, args| {
      let binding_object = args.first().cloned().unwrap_or(Value::Undefined);
      let name = vm.to_string(args.get(1).unwrap_or(&Value::Undefined))?;
      let value = args.get(2).cloned().unwrap_or(Value::Undefined);
      vm.set_resolved_identifier(&binding_object, name.as_str_lossy(), value)
    })),
  );
  intrinsics.hidden_builtins.insert(
    "toPropertyKey".to_string(),
    Value::Builtin(Rc::new(|vm, args| {
      let value = args.first().cloned().unwrap_or(Value::Undefined);
      let primitive = if value.is_object() {
        vm.to_primitive(&value, Some("string"))?
      } else {
        value
      };
      match primitive {
        Value::Symbol(_, _) => Ok(primitive),
        _ => Ok(Value::String(vm.to_string(&primitive)?)),
      }
    })),
  );
  intrinsics.hidden_builtins.insert(
    "copyDataProperties".to_string(),
    Value::Builtin(Rc::new(|vm, args| {
      let source = vm.to_object_value(args.first().cloned().unwrap_or(Value::Undefined))?;
      let mut excluded = Vec::with_capacity(args.len().saturating_sub(1));
      for value in args.iter().skip(1) {
        excluded.push(vm.to_property_key(value)?);
      }

      let mut result = ObjectData::new();
      if let Some(prototype) = vm.realm().intrinsics.object_prototype.clone() {
        result.set_prototype_value(prototype);
      }
      for key in vm.own_property_keys_value(&source)? {
        if excluded.contains(&key) {
          continue;
        }
        let Some(descriptor) = vm.get_own_property_value(&source, &key)? else {
          continue;
        };
        let enumerable = match descriptor {
          PropertyDescriptor::Data { enumerable, .. }
          | PropertyDescriptor::Accessor { enumerable, .. } => enumerable,
        };
        if !enumerable {
          continue;
        }
        let value = vm.get_property_host(&source, &key)?;
        result
          .properties
          .insert(key, PropertyDescriptor::ordinary_data(value));
      }
      Ok(Value::Object(crate::gc::track_object(Rc::new(
        RefCell::new(result),
      ))))
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
    "getIterator".to_string(),
    Value::Builtin(Rc::new(|vm, args| {
      vm.get_sync_iterator(args.first().cloned().unwrap_or(Value::Undefined))
    })),
  );
  intrinsics.hidden_builtins.insert(
    "iteratorDestructuringStep".to_string(),
    Value::Builtin(Rc::new(|vm, args| {
      let iterator = args.first().cloned().unwrap_or(Value::Undefined);
      let result = vm.iterator_next(iterator)?;
      let done = vm.get_property_host(&result, &PropertyKey::String("done".to_string().into()))?;
      if vm.to_boolean(&done) {
        Ok(array_from_values(vec![
          Value::Undefined,
          Value::Bool(false),
        ]))
      } else {
        let value =
          vm.get_property_host(&result, &PropertyKey::String("value".to_string().into()))?;
        Ok(array_from_values(vec![value, Value::Bool(true)]))
      }
    })),
  );
  intrinsics.hidden_builtins.insert(
    "iteratorToArray".to_string(),
    Value::Builtin(Rc::new(|vm, args| {
      let iterator = args.first().cloned().unwrap_or(Value::Undefined);
      let mut out = Vec::new();
      loop {
        let step = vm.iterator_next(iterator.clone())?;
        let done = vm.get_property_host(&step, &PropertyKey::String("done".to_string().into()))?;
        if vm.to_boolean(&done) {
          break;
        }
        let value =
          vm.get_property_host(&step, &PropertyKey::String("value".to_string().into()))?;
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
        Value::Symbol(_, description) => description.as_ref().map_or_else(
          || "".into(),
          |description| format!("[{description}]").into(),
        ),
        _ => return Ok(value),
      };
      let name = match args.get(2) {
        Some(Value::String(prefix)) => format!("{prefix} {name}").into(),
        _ => name,
      };
      match &value {
        Value::Closure { props, .. } => {
          let (has_name, current) = {
            let props_ref = props.borrow();
            let key = PropertyKey::String("name".to_string().into());
            let current = props_ref
              .properties
              .get(&key)
              .and_then(|desc| match desc {
                PropertyDescriptor::Data {
                  value: Value::String(s),
                  ..
                } => Some(s.clone()),
                _ => None,
              });
            (props_ref.properties.contains_key(&key), current)
          };
          if !has_name
            || current.as_ref().is_some_and(|s| {
              s.is_empty() || s == "<anonymous>" || s == "<arrow>" || s == "<method>"
            })
          {
            props.borrow_mut().properties.insert(
              PropertyKey::String("name".to_string().into()),
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
            let key = PropertyKey::String("name".to_string().into());
            let current = props_ref
              .properties
              .get(&key)
              .and_then(|desc| match desc {
                PropertyDescriptor::Data {
                  value: Value::String(s),
                  ..
                } => Some(s.clone()),
                _ => None,
              });
            (props_ref.properties.contains_key(&key), current)
          };
          if !has_name
            || current.as_ref().is_some_and(|s| {
              s.is_empty() || s == "<anonymous>" || s == "<arrow>" || s == "<method>"
            })
          {
            props.borrow_mut().properties.insert(
              PropertyKey::String("name".to_string().into()),
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
    "toString".to_string(),
    Value::Builtin(Rc::new(|vm, args| {
      let value = args.first().cloned().unwrap_or(Value::Undefined);
      Ok(Value::String(vm.to_string(&value)?))
    })),
  );
  intrinsics.hidden_builtins.insert(
    "objectSetPrototypeOf".to_string(),
    Value::Builtin(Rc::new(|vm, args| {
      if args.len() < 2 {
        return Ok(Value::Undefined);
      }
      let target = args[0].clone();
      let proto = args[1].clone();
      if !matches!(proto, Value::Null) && !proto.is_object() {
        return Ok(target);
      }
      match &target {
        Value::Object(obj) => {
          obj.borrow_mut().set_prototype_value(proto);
          Ok(target)
        }
        Value::Array(_, props) | Value::Closure { props, .. } | Value::BuiltinObject(_, props) => {
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
          return Err(vm.throw_type_error("Class extends value is not a constructor or null"));
        }
        let proto = vm.get_property_host(
          &superclass,
          &PropertyKey::String("prototype".to_string().into()),
        )?;
        if !matches!(proto, Value::Null | Value::Object(_) | Value::Array(..)) {
          return Err(vm.throw_type_error("Superclass prototype must be an object or null"));
        }
        proto
      };

      match &ctor {
        Value::Closure { props, .. } | Value::BuiltinObject(_, props) => {
          props.borrow_mut().properties.insert(
            PropertyKey::String("[[SuperConstructor]]".to_string().into()),
            PropertyDescriptor::ordinary_data(superclass.clone()),
          );
          if !matches!(superclass, Value::Null) {
            props.borrow_mut().set_prototype_value(superclass);
          }
          let ctor_proto =
            vm.get_property_host(&ctor, &PropertyKey::String("prototype".to_string().into()))?;
          match ctor_proto {
            Value::Object(obj) => {
              obj.borrow_mut().set_prototype_value(super_proto);
            }
            Value::Array(_, props)
            | Value::Closure { props, .. }
            | Value::BuiltinObject(_, props) => {
              props
                .borrow_mut()
                .set_prototype_value(super_proto);
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
