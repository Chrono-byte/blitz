// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::bytecode::{Chunk, Function};
use crate::typecheck::Type;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

impl Compiler {
  pub(super) fn compile_function(
    &mut self,
    name: &str,
    self_name: Option<&str>,
    func: &oast::Function,
    return_type: Option<Type>,
    allow_super: bool,
  ) -> Result<(Function, usize), crate::errors::CompilerError> {
    self.compile_function_with_instance_fields(name, self_name, func, return_type, allow_super, &[])
  }

  pub(super) fn compile_function_with_instance_fields(
    &mut self,
    name: &str,
    self_name: Option<&str>,
    func: &oast::Function,
    return_type: Option<Type>,
    allow_super: bool,
    instance_fields: &[crate::compiler::core::PreparedClassField],
  ) -> Result<(Function, usize), crate::errors::CompilerError> {
    let _env_len = self.locals.len();
    let function_is_strict = self.strict_context
      || func
        .body
        .as_ref()
        .is_some_and(|body| Self::has_use_strict_directive(body));
    let body_has_use_strict = func
      .body
      .as_ref()
      .is_some_and(|body| Self::has_use_strict_directive(body));

    if let Some(binding) = &func.id
      && let Some(err) = Self::strict_binding_name_error(function_is_strict, &binding.name)
    {
      return Err(err);
    }
    if let Some(self_name) = self_name
      && let Some(err) = Self::strict_binding_name_error(function_is_strict, self_name)
    {
      return Err(err);
    }
    if function_is_strict {
      let mut seen = HashSet::new();
      for p in &func.params.items {
        if let oast::BindingPattern::BindingIdentifier(id) = &p.pattern
          && !seen.insert(id.name.to_string())
        {
          return Err(crate::errors::CompilerError::from(
            "SyntaxError: duplicate parameter name in strict mode",
          ));
        }
      }
      if let Some(rest) = &func.params.rest
        && let oast::BindingPattern::BindingIdentifier(id) = &rest.rest.argument
        && !seen.insert(id.name.to_string())
      {
        return Err(crate::errors::CompilerError::from(
          "SyntaxError: duplicate parameter name in strict mode",
        ));
      }
    }
    for p in &func.params.items {
      if let Some(err) = Self::strict_binding_pattern_name_error(function_is_strict, &p.pattern) {
        return Err(err);
      }
    }
    if let Some(rest) = &func.params.rest
      && let Some(err) =
        Self::strict_binding_pattern_name_error(function_is_strict, &rest.rest.argument)
    {
      return Err(err);
    }

    let has_simple_parameter_list = Self::has_simple_parameter_list(func);
    if !has_simple_parameter_list {
      if body_has_use_strict {
        return Err(crate::errors::CompilerError::from(
          "SyntaxError: non-simple parameter list in strict mode",
        ));
      }
      let mut seen = HashSet::new();
      for p in &func.params.items {
        let mut names = HashSet::new();
        Self::collect_binding_names(&p.pattern, &mut names);
        let mut names: Vec<_> = names.into_iter().collect();
        names.sort();
        for name in names {
          if !seen.insert(name) {
            return Err(crate::errors::CompilerError::from(
              "SyntaxError: duplicate parameter name in non-simple parameter list",
            ));
          }
        }
      }
      if let Some(rest) = &func.params.rest {
        let mut names = HashSet::new();
        Self::collect_binding_names(&rest.rest.argument, &mut names);
        let mut names: Vec<_> = names.into_iter().collect();
        names.sort();
        for name in names {
          if !seen.insert(name) {
            return Err(crate::errors::CompilerError::from(
              "SyntaxError: duplicate parameter name in non-simple parameter list",
            ));
          }
        }
      }
    }

    let mut inner = Compiler {
      chunk: Chunk::new(Arc::clone(&self.source_text)),
      lowering: super::bytecode::Lowering::new(),
      source_text: Arc::clone(&self.source_text),
      locals: Vec::new(),
      local_types: Vec::new(),
      local_tdz: Vec::new(),
      local_immutable: Vec::new(),
      local_sloppy_immutable_silent: Vec::new(),
      local_mirror_globals: Vec::new(),
      scopes: vec![crate::compiler::Scope {
        kind: crate::compiler::ScopeKind::Function,
        bindings: HashMap::new(),
        lexical_bindings: HashSet::new(),
        block_var_names: HashSet::new(),
        runtime_env_count: 0,
      }],
      globals: HashMap::new(),
      is_root: false,
      strict_context: self.strict_context,
      loop_depth: 0,
      with_depth: 0,
      may_capture_with: self.with_depth > 0 || self.may_capture_with,
      eval_scopes: Arc::clone(&self.eval_scopes),
      dynamic_outer_bindings: self.dynamic_outer_bindings,
      handler_depth: 0,
      continue_patches: Vec::new(),
      break_patches: Vec::new(),
      finally_patches: Vec::new(),
      iterator_close_stack: Vec::new(),
      label_patches: Vec::new(),
      current_module: self.current_module.clone(),
      pending_operand_bytes: 0,
      allow_new_target: true,
      allow_super,
      is_async: func.r#async,
      is_generator: func.generator,
      outer_bindings: self.visible_bindings_snapshot(),
      upvalue_indices: HashMap::new(),
      captured_upvalues: Vec::new(),
      private_names: self.private_names.clone(),
      pending_instance_fields: instance_fields.to_vec(),
      emission_error: None,
    };
    inner.strict_context = function_is_strict;
    inner.dynamic_outer_bindings |= !function_is_strict
      && func
        .scope_id
        .get()
        .is_some_and(|id| self.eval_scopes.contains(&id));
    inner.chunk.allow_new_target = inner.allow_new_target;
    inner.chunk.allow_super = inner.allow_super;
    if !has_simple_parameter_list && !function_is_strict {
      inner.outer_bindings.clear();
    }

    let mut raw_param_slots = Vec::new();
    if has_simple_parameter_list {
      let mut last_parameter_index = HashMap::new();
      for (index, parameter) in func.params.items.iter().enumerate() {
        if let oast::BindingPattern::BindingIdentifier(identifier) = &parameter.pattern {
          last_parameter_index.insert(identifier.name.to_string(), index);
        }
      }
      for (index, parameter) in func.params.items.iter().enumerate() {
        let pname = match &parameter.pattern {
          oast::BindingPattern::BindingIdentifier(identifier)
            if last_parameter_index.get(identifier.name.as_str()) == Some(&index) =>
          {
            identifier.name.to_string()
          }
          _ => format!("\0duplicate_param_{index}"),
        };
        inner.add_local_with_mirror(pname, None, false);
      }
    } else {
      for _ in &func.params.items {
        raw_param_slots.push(inner.add_local_with_mirror(
          format!("\0param_raw_{}", raw_param_slots.len()),
          None,
          false,
        ));
      }
      if func.params.rest.is_some() {
        raw_param_slots.push(inner.add_local_with_mirror(
          "\0param_raw_rest".to_string(),
          None,
          false,
        ));
      }
      let mut parameter_names = HashSet::new();
      for parameter in &func.params.items {
        Self::collect_binding_names(&parameter.pattern, &mut parameter_names);
      }
      if let Some(rest) = &func.params.rest {
        Self::collect_binding_names(&rest.rest.argument, &mut parameter_names);
      }
      let mut parameter_names: Vec<_> = parameter_names.into_iter().collect();
      parameter_names.sort();
      for parameter_name in parameter_names {
        let slot = inner.add_local_with_mirror(parameter_name, None, false);
        inner.mark_local_tdz(slot);
      }
    }
    let self_binding_slot =
      self_name.map(|self_name| inner.add_local_with_mirror(self_name.to_string(), None, false));
    if let Some(slot) = self_binding_slot
      && let Some(flag) = inner.local_immutable.get_mut(slot)
    {
      *flag = true;
    }
    if let Some(slot) = self_binding_slot
      && let Some(flag) = inner.local_sloppy_immutable_silent.get_mut(slot)
    {
      *flag = true;
    }
    let arguments_slot = inner.add_local_with_mirror("arguments".to_string(), None, false);

    if has_simple_parameter_list {
      inner.emit_parameter_initializers_from_current_slots(&func.params)?;
    } else {
      inner.emit_parameter_initializers_from_raw_slots(&func.params, &raw_param_slots)?;
    }
    let parameter_environment_slot_names = inner.environment_slot_names();

    if let Some(body) = &func.body {
      inner.enter_function_body_scope_with_vars(&body.statements, !has_simple_parameter_list);
      inner.pre_declare_functions_oast_stmts(&body.statements);
      inner.emit_hoisted_function_decls_oast_stmts(&body.statements)?;
    }
    if func.generator {
      inner.emit(LoweringOpcode::GeneratorInitDone, LINE);
    }
    if !allow_super {
      inner.emit_pending_instance_field_initializers()?;
    }

    if let Some(body) = &func.body {
      for stmt in &body.statements {
        if matches!(stmt, oast::Statement::FunctionDeclaration(_)) {
          continue;
        }
        inner.compile_statement_oast(stmt)?;
      }
      inner.exit_scope();
    }

    inner.emit_fallthrough_return(LINE);
    let mut code = super::bytecode::Lowering::finish(
      std::mem::take(&mut inner.chunk),
      std::mem::take(&mut inner.lowering),
    )?;
    code.allow_new_target = inner.allow_new_target;
    code.allow_super = inner.allow_super;
    let has_rest = func.params.rest.is_some();
    let has_simple_parameter_list = Self::has_simple_parameter_list(func);
    let mut slot_names = if has_simple_parameter_list {
      inner.environment_slot_names()
    } else {
      parameter_environment_slot_names
    };
    slot_names.resize(inner.locals.len(), None);
    let slot_types = inner.local_types;
    let lexical_tdz = inner.local_tdz;
    let has_prototype_property = !func.r#async || func.generator;
    let is_constructor = !func.r#async && !func.generator;
    Ok((
      Function {
        name: name.to_string(),
        span: func.span,
        arity: func.params.items.len() + usize::from(has_rest),
        length: Self::expected_argument_count(&func.params),
        chunk: Arc::new(code),
        slot_types,
        slot_names,
        lexical_tdz,
        slot_immutable: inner.local_immutable,
        slot_sloppy_immutable_silent: inner.local_sloppy_immutable_silent,
        upvalues: inner.captured_upvalues,
        has_rest,
        has_prototype_property,
        captures_this: false,
        arguments_slot: Some(arguments_slot),
        self_binding_slot,
        has_simple_parameter_list,
        return_type,
        is_strict: function_is_strict,
        is_constructor,
        is_derived_constructor: false,
        is_class_constructor: false,
        is_async: func.r#async,
        is_generator: func.generator,
      },
      _env_len,
    ))
  }

  pub(super) fn compile_arrow_function(
    &mut self,
    name: &str,
    func: &oast::ArrowFunctionExpression,
    return_type: Option<Type>,
  ) -> Result<(Function, usize), crate::errors::CompilerError> {
    let _env_len = self.locals.len();
    let body_has_use_strict = func.has_use_strict_directive();
    let function_is_strict = self.strict_context || body_has_use_strict;
    let has_simple_parameter_list = func.params.rest.is_none()
      && func.params.items.iter().all(|param| {
        matches!(param.pattern, oast::BindingPattern::BindingIdentifier(_))
          && param.initializer.is_none()
      });

    if function_is_strict {
      let mut seen = HashSet::new();
      for p in &func.params.items {
        if let oast::BindingPattern::BindingIdentifier(id) = &p.pattern
          && !seen.insert(id.name.to_string())
        {
          return Err(crate::errors::CompilerError::from(
            "SyntaxError: duplicate parameter name in strict mode",
          ));
        }
      }
      if let Some(rest) = &func.params.rest
        && let oast::BindingPattern::BindingIdentifier(id) = &rest.rest.argument
        && !seen.insert(id.name.to_string())
      {
        return Err(crate::errors::CompilerError::from(
          "SyntaxError: duplicate parameter name in strict mode",
        ));
      }
    }
    for p in &func.params.items {
      if let Some(err) = Self::strict_binding_pattern_name_error(function_is_strict, &p.pattern) {
        return Err(err);
      }
    }
    if let Some(rest) = &func.params.rest
      && let Some(err) =
        Self::strict_binding_pattern_name_error(function_is_strict, &rest.rest.argument)
    {
      return Err(err);
    }

    if !has_simple_parameter_list {
      if body_has_use_strict {
        return Err(crate::errors::CompilerError::from(
          "SyntaxError: non-simple parameter list in strict mode",
        ));
      }
      let mut seen = HashSet::new();
      for p in &func.params.items {
        let mut names = HashSet::new();
        Self::collect_binding_names(&p.pattern, &mut names);
        let mut names: Vec<_> = names.into_iter().collect();
        names.sort();
        for name in names {
          if !seen.insert(name) {
            return Err(crate::errors::CompilerError::from(
              "SyntaxError: duplicate parameter name in non-simple parameter list",
            ));
          }
        }
      }
      if let Some(rest) = &func.params.rest {
        let mut names = HashSet::new();
        Self::collect_binding_names(&rest.rest.argument, &mut names);
        let mut names: Vec<_> = names.into_iter().collect();
        names.sort();
        for name in names {
          if !seen.insert(name) {
            return Err(crate::errors::CompilerError::from(
              "SyntaxError: duplicate parameter name in non-simple parameter list",
            ));
          }
        }
      }
    }

    let mut inner = Compiler {
      chunk: Chunk::new(Arc::clone(&self.source_text)),
      lowering: super::bytecode::Lowering::new(),
      source_text: Arc::clone(&self.source_text),
      locals: Vec::new(),
      local_types: Vec::new(),
      local_tdz: Vec::new(),
      local_immutable: Vec::new(),
      local_sloppy_immutable_silent: Vec::new(),
      local_mirror_globals: Vec::new(),
      scopes: vec![crate::compiler::Scope {
        kind: crate::compiler::ScopeKind::Function,
        bindings: HashMap::new(),
        lexical_bindings: HashSet::new(),
        block_var_names: HashSet::new(),
        runtime_env_count: 0,
      }],
      globals: HashMap::new(),
      is_root: false,
      strict_context: self.strict_context,
      loop_depth: 0,
      with_depth: 0,
      may_capture_with: self.with_depth > 0 || self.may_capture_with,
      eval_scopes: Arc::clone(&self.eval_scopes),
      dynamic_outer_bindings: self.dynamic_outer_bindings,
      handler_depth: 0,
      continue_patches: Vec::new(),
      break_patches: Vec::new(),
      finally_patches: Vec::new(),
      iterator_close_stack: Vec::new(),
      label_patches: Vec::new(),
      current_module: self.current_module.clone(),
      pending_operand_bytes: 0,
      allow_new_target: self.allow_new_target,
      allow_super: self.allow_super,
      is_async: func.r#async,
      is_generator: false,
      outer_bindings: self.visible_bindings_snapshot(),
      upvalue_indices: HashMap::new(),
      captured_upvalues: Vec::new(),
      private_names: self.private_names.clone(),
      pending_instance_fields: Vec::new(),
      emission_error: None,
    };
    inner.strict_context = function_is_strict;
    inner.dynamic_outer_bindings |= !function_is_strict
      && func
        .scope_id
        .get()
        .is_some_and(|id| self.eval_scopes.contains(&id));
    inner.chunk.allow_new_target = inner.allow_new_target;
    inner.chunk.allow_super = inner.allow_super;

    let mut raw_param_slots = Vec::new();
    if has_simple_parameter_list {
      let mut seen = HashSet::new();
      for parameter in &func.params.items {
        if let oast::BindingPattern::BindingIdentifier(identifier) = &parameter.pattern
          && !seen.insert(identifier.name.to_string())
        {
          return Err(crate::errors::CompilerError::from(
            "SyntaxError: duplicate parameter name in arrow function",
          ));
        }
        let pname = match &parameter.pattern {
          oast::BindingPattern::BindingIdentifier(identifier) => identifier.name.to_string(),
          _ => "_".to_string(),
        };
        inner.add_local_with_mirror(pname, None, false);
      }
    } else {
      for _ in &func.params.items {
        raw_param_slots.push(inner.add_local_with_mirror(
          format!("\0param_raw_{}", raw_param_slots.len()),
          None,
          false,
        ));
      }
      if func.params.rest.is_some() {
        raw_param_slots.push(inner.add_local_with_mirror(
          "\0param_raw_rest".to_string(),
          None,
          false,
        ));
      }
      let mut parameter_names = HashSet::new();
      for parameter in &func.params.items {
        Self::collect_binding_names(&parameter.pattern, &mut parameter_names);
      }
      if let Some(rest) = &func.params.rest {
        Self::collect_binding_names(&rest.rest.argument, &mut parameter_names);
      }
      let mut parameter_names: Vec<_> = parameter_names.into_iter().collect();
      parameter_names.sort();
      for parameter_name in parameter_names {
        let slot = inner.add_local_with_mirror(parameter_name, None, false);
        inner.mark_local_tdz(slot);
      }
    }
    if !has_simple_parameter_list {
      inner.emit_parameter_initializers_from_raw_slots(&func.params, &raw_param_slots)?;
    }
    let parameter_environment_slot_names = inner.environment_slot_names();

    let statements = match &func.body {
      oast::ArrowFunctionBody::FunctionBody(body) => body.statements.as_slice(),
      _ => &[],
    };
    inner.enter_function_body_scope_with_vars(statements, !has_simple_parameter_list);
    inner.pre_declare_functions_oast_stmts(statements);
    inner.emit_hoisted_function_decls_oast_stmts(statements)?;

    if let Some(expression) = func.body.as_expression() {
      inner.compile_expr_oast_tail(expression)?;
      inner.emit(LoweringOpcode::Return, LINE);
    } else {
      for stmt in statements {
        if !matches!(stmt, oast::Statement::FunctionDeclaration(_)) {
          inner.compile_statement_oast(stmt)?;
        }
      }
      inner.emit_fallthrough_return(LINE);
    }

    inner.exit_scope();

    let mut code = super::bytecode::Lowering::finish(
      std::mem::take(&mut inner.chunk),
      std::mem::take(&mut inner.lowering),
    )?;
    code.allow_new_target = inner.allow_new_target;
    code.allow_super = inner.allow_super;
    let has_rest = func.params.rest.is_some();
    let mut slot_names = if has_simple_parameter_list {
      inner.environment_slot_names()
    } else {
      parameter_environment_slot_names
    };
    slot_names.resize(inner.locals.len(), None);
    let slot_types = inner.local_types;
    let lexical_tdz = inner.local_tdz;
    Ok((
      Function {
        name: name.to_string(),
        span: func.span,
        arity: func.params.items.len() + usize::from(has_rest),
        length: Self::expected_argument_count(&func.params),
        chunk: Arc::new(code),
        slot_types,
        slot_names,
        lexical_tdz,
        slot_immutable: inner.local_immutable,
        slot_sloppy_immutable_silent: inner.local_sloppy_immutable_silent,
        upvalues: inner.captured_upvalues,
        has_rest,
        has_prototype_property: false,
        captures_this: true,
        arguments_slot: None,
        self_binding_slot: None,
        has_simple_parameter_list,
        return_type,
        is_strict: function_is_strict,
        is_constructor: false,
        is_derived_constructor: false,
        is_class_constructor: false,
        is_async: func.r#async,
        is_generator: false,
      },
      _env_len,
    ))
  }
}
