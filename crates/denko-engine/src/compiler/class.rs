// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::bytecode::{Chunk, FunctionProto};
use oxc_span::{GetSpan, SourceType};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

impl Compiler {
  /// Compile a class declaration statement into bytecode.
  ///
  /// Creates a constructor function closure and compiles all class methods,
  /// attaching them to either the constructor or its prototype.
  pub(super) fn compile_class_declaration_oast(
    &mut self,
    c: &oast::Class<'_>,
  ) -> Result<(), crate::errors::CompilerError> {
    let name = c
      .id
      .as_ref()
      .map(|id| id.name.to_string())
      .ok_or_else(|| crate::errors::CompilerError::from("anonymous class declaration"))?;

    let slot = if let Some(slot) = self.resolve_local(&name) {
      slot
    } else {
      let slot = self.add_local_with_mirror(name.clone(), None, false);
      self.mark_local_tdz(slot);
      self.mark_local_immutable(slot);
      self.add_lexical_binding(&name);
      slot
    };
    self.emit_class_from_oast(c, Some(&name))?;
    self.emit(Op::StoreLocal, LINE);
    self.emit_slot(slot, LINE);
    Ok(())
  }

  /// Check if a function has a simple parameter list (no rest, no defaults, all identifiers).
  pub(super) fn has_simple_parameter_list(func: &oast::Function<'_>) -> bool {
    func.params.rest.is_none()
      && func.params.items.iter().all(|param| {
        matches!(param.pattern, oast::BindingPattern::BindingIdentifier(_))
          && param.initializer.is_none()
      })
  }

  pub(super) fn expected_argument_count(params: &oast::FormalParameters<'_>) -> usize {
    params
      .items
      .iter()
      .take_while(|parameter| parameter.initializer.is_none())
      .count()
  }

  /// Emit bytecode to initialize non-simple function parameters from hidden
  /// raw argument slots.
  ///
  /// Raw slots are expected to be laid out before any visible parameter
  /// bindings. This keeps later parameters in TDZ while earlier defaults are
  /// evaluated.
  pub(super) fn emit_parameter_initializers_from_raw_slots(
    &mut self,
    params: &oast::FormalParameters<'_>,
    raw_slots: &[usize],
  ) -> Result<(), crate::errors::CompilerError> {
    let temp_slot = self.alloc_temp_local("param_init_value");

    for (idx, param) in params.items.iter().enumerate() {
      let raw_slot = raw_slots
        .get(idx)
        .copied()
        .ok_or_else(|| crate::errors::CompilerError::from("missing parameter slot"))?;

      self.emit(Op::LoadLocal, LINE);
      self.emit_slot(raw_slot, LINE);

      if let Some(init) = &param.initializer {
        self.emit(Op::StoreLocal, LINE);
        self.emit_slot(temp_slot, LINE);
        self.emit(Op::LoadLocal, LINE);
        self.emit_slot(temp_slot, LINE);
        self.emit(Op::Undefined, LINE);
        // Use CompareJump: cond_type 6 = StrictEq, jump if false.
        self.emit_with_payload(Op::CompareJump, 6, LINE);
        let skip_jump = self.current_ip();
        self.emit_byte(0, LINE);
        self.emit_byte(0, LINE);
        self.compile_expr_oast(init)?;
        self.emit(Op::StoreLocal, LINE);
        self.emit_slot(temp_slot, LINE);
        self.patch_jump(skip_jump);
        self.emit(Op::LoadLocal, LINE);
        self.emit_slot(temp_slot, LINE);
      }

      self.compile_binding_pattern_assignment_oast(
        &param.pattern,
        oxc_ast::ast::VariableDeclarationKind::Var,
      )?;
    }

    if let Some(rest) = &params.rest {
      let raw_slot = raw_slots
        .get(params.items.len())
        .copied()
        .ok_or_else(|| crate::errors::CompilerError::from("missing rest parameter slot"))?;
      self.emit(Op::LoadLocal, LINE);
      self.emit_slot(raw_slot, LINE);
      self.compile_binding_pattern_assignment_oast(
        &rest.rest.argument,
        oxc_ast::ast::VariableDeclarationKind::Var,
      )?;
    }

    Ok(())
  }

  /// Emit bytecode to initialize default parameter values using the current
  /// parameter slots directly.
  pub(super) fn emit_parameter_initializers_from_current_slots(
    &mut self,
    params: &oast::FormalParameters<'_>,
  ) -> Result<(), crate::errors::CompilerError> {
    for (idx, param) in params.items.iter().enumerate() {
      let Some(init) = &param.initializer else {
        continue;
      };
      self.emit(Op::LoadLocal, LINE);
      self.emit_slot(idx, LINE);
      self.emit(Op::Undefined, LINE);
      // Use CompareJump: cond_type 6 = StrictEq, jump if false.
      self.emit_with_payload(Op::CompareJump, 6, LINE);
      let skip_jump = self.current_ip();
      self.emit_byte(0, LINE);
      self.emit_byte(0, LINE);
      self.compile_expr_oast(init)?;
      self.emit(Op::StoreLocal, LINE);
      self.emit_slot(idx, LINE);
      self.patch_jump(skip_jump);
    }
    Ok(())
  }

  pub(super) fn compile_class_initializer_source(
    &mut self,
    source: &str,
  ) -> Result<(), crate::errors::CompilerError> {
    let wrapped = crate::js!(format(tsx), "({source})");
    let session = crate::parser::Parser::parse_source_with_type(&wrapped, SourceType::tsx())
      .map_err(|error| crate::errors::CompilerError::from(error.to_string()))?;
    let Some(oast::Statement::ExpressionStatement(statement)) = session.program().body.first()
    else {
      return Err(crate::errors::CompilerError::from(
        "class field initializer did not parse as an expression",
      ));
    };
    self.compile_expr_oast(&statement.expression)
  }

  pub(super) fn emit_prepared_class_field_key(
    &mut self,
    key: &crate::compiler::core::PreparedClassFieldKey,
  ) {
    match key {
      crate::compiler::core::PreparedClassFieldKey::Named(name) => {
        self.emit_constant(Value::String(name.clone().into()), LINE);
      }
      crate::compiler::core::PreparedClassFieldKey::Private(key) => {
        self.emit_constant(key.clone(), LINE);
      }
      crate::compiler::core::PreparedClassFieldKey::Computed(binding) => {
        self.emit_get_binding(binding);
      }
    }
  }

  pub(super) fn emit_static_class_field(
    &mut self,
    target_slot: usize,
    field: &crate::compiler::core::PreparedClassField,
    initializer: Option<&oast::Expression<'_>>,
  ) -> Result<(), crate::errors::CompilerError> {
    self.emit_get_builtin_name("objectDefineProperty");
    self.emit(Op::LoadLocal, LINE);
    self.emit_slot(target_slot, LINE);
    self.emit_prepared_class_field_key(&field.key);
    self.emit_constant(Value::String("value".to_string().into()), LINE);
    if let Some(initializer) = initializer {
      let proto = self.compile_static_field_initializer(initializer)?;
      let function_index = self.chunk.add_function(proto);
      self.emit(Op::LoadLocal, LINE);
      self.emit_slot(target_slot, LINE);
      self.emit(Op::MakeClosure, LINE);
      self.emit_byte(self.function_index_to_u8(function_index)?, LINE);
      self.emit_set_method_home_object(target_slot)?;
      self.emit(Op::CallBuiltin, LINE);
      self.emit_byte(0, LINE);
    } else {
      self.emit(Op::Undefined, LINE);
    }
    self.emit_constant(Value::String("writable".to_string().into()), LINE);
    self.emit_constant(Value::Bool(true), LINE);
    self.emit_constant(Value::String("enumerable".to_string().into()), LINE);
    self.emit_constant(Value::Bool(true), LINE);
    self.emit_constant(Value::String("configurable".to_string().into()), LINE);
    self.emit_constant(Value::Bool(true), LINE);
    self.emit(Op::BuildObject, LINE);
    self.emit_byte(4, LINE);
    self.emit(Op::Call, LINE);
    self.emit_byte(3, LINE);
    self.emit(Op::Discard, LINE);
    Ok(())
  }

  pub(super) fn emit_pending_instance_field_initializers(
    &mut self
  ) -> Result<(), crate::errors::CompilerError> {
    let fields = self.pending_instance_fields.clone();
    for field in fields.iter().filter(|field| !field.is_static) {
      self.emit_get_builtin_name("objectDefineProperty");
      self.emit(Op::GetThis, LINE);
      self.emit_prepared_class_field_key(&field.key);
      self.emit_constant(Value::String("value".to_string().into()), LINE);
      if let Some(source) = &field.initializer_source {
        self.compile_class_initializer_source(source)?;
      } else {
        self.emit(Op::Undefined, LINE);
      }
      self.emit_constant(Value::String("writable".to_string().into()), LINE);
      self.emit_constant(Value::Bool(true), LINE);
      self.emit_constant(Value::String("enumerable".to_string().into()), LINE);
      self.emit_constant(Value::Bool(true), LINE);
      self.emit_constant(Value::String("configurable".to_string().into()), LINE);
      self.emit_constant(Value::Bool(true), LINE);
      self.emit(Op::BuildObject, LINE);
      self.emit_byte(4, LINE);
      self.emit(Op::Call, LINE);
      self.emit_byte(3, LINE);
      self.emit(Op::Discard, LINE);
    }
    Ok(())
  }

  /// Lower a class method/getter/setter installation to a property
  /// definition with a real descriptor object.
  pub(super) fn emit_class_member_installation(
    &mut self,
    target_slot: usize,
    md: &oast::MethodDefinition<'_>,
  ) -> Result<(), crate::errors::CompilerError> {
    if matches!(md.kind, oast::MethodDefinitionKind::Constructor) {
      return Ok(());
    }

    let method_name = if md.computed {
      None
    } else {
      match &md.key {
        oast::PropertyKey::StaticIdentifier(ident) => Some(ident.name.to_string()),
        oast::PropertyKey::Identifier(ident) => Some(ident.name.to_string()),
        oast::PropertyKey::StringLiteral(s) => Some(s.value.to_string()),
        oast::PropertyKey::NumericLiteral(n) => Some(super::numeric_property_key(n.value)),
        oast::PropertyKey::PrivateIdentifier(pid) => Some(pid.name.to_string()),
        _ => None,
      }
    };

    let self_name = md.value.id.as_ref().map(|id| id.name.as_str());
    let function_name = match (md.kind, method_name.as_deref()) {
      (oast::MethodDefinitionKind::Get, Some(name)) => format!("get {name}"),
      (oast::MethodDefinitionKind::Set, Some(name)) => format!("set {name}"),
      (_, Some(name)) => name.to_string(),
      _ => "<method>".to_string(),
    };
    let outer_strict = self.strict_context;
    self.strict_context = true;
    let compiled = self.compile_function(&function_name, self_name, &md.value, None, true);
    self.strict_context = outer_strict;
    let (mut proto, _env_len) = compiled?;
    proto.is_strict = true;
    proto.has_prototype_property = proto.is_generator;
    proto.is_constructor = false;
    let fidx = self.chunk.add_function(proto);
    let computed_key_slot = if md.computed {
      Some(self.alloc_temp_local("class_computed_method_key"))
    } else {
      None
    };

    self.emit(Op::LoadLocal, LINE);
    self.emit_slot(target_slot, LINE);
    self.emit(Op::Copy, LINE);
    self.emit_get_builtin_name("objectDefineProperty");
    self.emit(Op::Exchange, LINE);

    if let Some(ref key_name) = method_name {
      if matches!(md.key, oast::PropertyKey::PrivateIdentifier(_)) {
        let key = self.ensure_private_name_constant(key_name);
        self.emit_constant(key, LINE);
      } else {
        self.emit_constant(Value::String(key_name.clone().into()), LINE);
      }
    } else {
      let key_expr = md
        .key
        .as_expression()
        .expect("computed class member key must be an expression");
      self.compile_expr_oast(key_expr)?;
      self.emit(Op::Copy, LINE);
      self.emit(Op::StoreLocal, LINE);
      self.emit_slot(
        computed_key_slot.expect("computed method has key slot"),
        LINE,
      );
    }

    let first_descriptor_key = match md.kind {
      oast::MethodDefinitionKind::Method => "value",
      oast::MethodDefinitionKind::Get => "get",
      oast::MethodDefinitionKind::Set => "set",
      oast::MethodDefinitionKind::Constructor => {
        return Err(crate::errors::CompilerError::from(
          "constructor method definition encountered unexpectedly",
        ));
      }
    };
    self.emit_constant(Value::String(first_descriptor_key.to_string().into()), LINE);
    self.emit(Op::MakeClosure, LINE);
    self.emit_byte(self.function_index_to_u8(fidx)?, LINE);
    if let Some(key_slot) = computed_key_slot {
      let closure_slot = self.alloc_temp_local("class_computed_method_closure");
      self.emit(Op::StoreLocal, LINE);
      self.emit_slot(closure_slot, LINE);
      self.emit_get_builtin_name("setFunctionNameIfAnonymous");
      self.emit(Op::LoadLocal, LINE);
      self.emit_slot(closure_slot, LINE);
      self.emit(Op::LoadLocal, LINE);
      self.emit_slot(key_slot, LINE);
      match md.kind {
        oast::MethodDefinitionKind::Get => {
          self.emit_constant(Value::String("get".into()), LINE);
          self.emit(Op::Call, LINE);
          self.emit_byte(3, LINE);
        }
        oast::MethodDefinitionKind::Set => {
          self.emit_constant(Value::String("set".into()), LINE);
          self.emit(Op::Call, LINE);
          self.emit_byte(3, LINE);
        }
        _ => {
          self.emit(Op::Call, LINE);
          self.emit_byte(2, LINE);
        }
      }
    }
    self.emit_set_method_home_object(target_slot)?;

    let prop_count = match md.kind {
      oast::MethodDefinitionKind::Method => {
        self.emit_constant(Value::String("writable".to_string().into()), LINE);
        self.emit_constant(Value::Bool(true), LINE);
        self.emit_constant(Value::String("enumerable".to_string().into()), LINE);
        self.emit_constant(Value::Bool(false), LINE);
        self.emit_constant(Value::String("configurable".to_string().into()), LINE);
        self.emit_constant(Value::Bool(true), LINE);
        4
      }
      oast::MethodDefinitionKind::Get => {
        self.emit_constant(Value::String("enumerable".to_string().into()), LINE);
        self.emit_constant(Value::Bool(false), LINE);
        self.emit_constant(Value::String("configurable".to_string().into()), LINE);
        self.emit_constant(Value::Bool(true), LINE);
        3
      }
      oast::MethodDefinitionKind::Set => {
        self.emit_constant(Value::String("enumerable".to_string().into()), LINE);
        self.emit_constant(Value::Bool(false), LINE);
        self.emit_constant(Value::String("configurable".to_string().into()), LINE);
        self.emit_constant(Value::Bool(true), LINE);
        3
      }
      oast::MethodDefinitionKind::Constructor => unreachable!(),
    };
    self.emit(Op::BuildObject, LINE);
    self.emit_byte(prop_count, LINE);
    self.emit(Op::Call, LINE);
    self.emit_byte(3, LINE);
    self.emit(Op::Discard, LINE);
    self.emit(Op::Discard, LINE);
    Ok(())
  }

  pub(super) fn compile_static_field_initializer(
    &mut self,
    initializer: &oast::Expression<'_>,
  ) -> Result<FunctionProto, crate::errors::CompilerError> {
    let outer_bindings = self.visible_bindings_snapshot();
    let mut inner = Compiler {
      chunk: Chunk::new(Arc::clone(&self.source_text)),
      risc: super::risc::RiscBuilder::new(),
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
      strict_context: true,
      loop_depth: 0,
      with_depth: 0,
      may_capture_with: self.with_depth > 0 || self.may_capture_with,
      handler_depth: 0,
      continue_patches: Vec::new(),
      break_patches: Vec::new(),
      finally_patches: Vec::new(),
      iterator_close_stack: Vec::new(),
      label_patches: Vec::new(),
      current_module: self.current_module.clone(),
      pending_operand_bytes: 0,
      allow_new_target: false,
      allow_super: true,
      outer_bindings,
      upvalue_indices: HashMap::new(),
      captured_upvalues: Vec::new(),
      private_names: self.private_names.clone(),
      pending_instance_fields: Vec::new(),
      emission_error: None,
    };
    inner.chunk.allow_super = true;
    inner.compile_expr_oast(initializer)?;
    inner.emit(Op::Return, LINE);
    let chunk = super::risc::RiscBuilder::finish(
      std::mem::take(&mut inner.chunk),
      std::mem::take(&mut inner.risc),
    )?;
    let slot_names = inner.environment_slot_names();
    Ok(FunctionProto {
      name: "<static field>".to_string(),
      span: initializer.span(),
      arity: 0,
      length: 0,
      chunk: Arc::new(chunk),
      slot_types: inner.local_types,
      slot_names,
      lexical_tdz: inner.local_tdz,
      slot_immutable: inner.local_immutable,
      slot_sloppy_immutable_silent: inner.local_sloppy_immutable_silent,
      upvalues: inner.captured_upvalues,
      has_rest: false,
      has_prototype_property: false,
      captures_this: false,
      arguments_slot: None,
      self_binding_slot: None,
      has_simple_parameter_list: true,
      return_type: None,
      is_strict: true,
      is_constructor: false,
      is_derived_constructor: false,
      is_class_constructor: false,
      is_async: false,
      is_generator: false,
    })
  }

  pub(super) fn compile_static_block_function(
    &mut self,
    block: &oast::StaticBlock<'_>,
  ) -> Result<FunctionProto, crate::errors::CompilerError> {
    let outer_bindings = self.visible_bindings_snapshot();
    let mut inner = Compiler {
      chunk: Chunk::new(Arc::clone(&self.source_text)),
      risc: super::risc::RiscBuilder::new(),
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
      strict_context: true,
      loop_depth: 0,
      with_depth: 0,
      may_capture_with: self.with_depth > 0 || self.may_capture_with,
      handler_depth: 0,
      continue_patches: Vec::new(),
      break_patches: Vec::new(),
      finally_patches: Vec::new(),
      iterator_close_stack: Vec::new(),
      label_patches: Vec::new(),
      current_module: self.current_module.clone(),
      pending_operand_bytes: 0,
      allow_new_target: false,
      allow_super: true,
      outer_bindings,
      upvalue_indices: HashMap::new(),
      captured_upvalues: Vec::new(),
      private_names: self.private_names.clone(),
      pending_instance_fields: Vec::new(),
      emission_error: None,
    };
    inner.chunk.allow_super = true;
    inner.pre_declare_variables_oast_stmts(&block.body);
    inner.pre_declare_functions_oast_stmts(&block.body);
    inner.emit_hoisted_function_decls_oast_stmts(&block.body)?;
    for statement in &block.body {
      if !matches!(statement, oast::Statement::FunctionDeclaration(_)) {
        inner.compile_statement_oast(statement)?;
      }
    }
    inner.emit_fallthrough_return(LINE);
    let chunk = super::risc::RiscBuilder::finish(
      std::mem::take(&mut inner.chunk),
      std::mem::take(&mut inner.risc),
    )?;
    let slot_names = inner.environment_slot_names();
    Ok(FunctionProto {
      name: "<static block>".to_string(),
      span: block.span,
      arity: 0,
      length: 0,
      chunk: Arc::new(chunk),
      slot_types: inner.local_types,
      slot_names,
      lexical_tdz: inner.local_tdz,
      slot_immutable: inner.local_immutable,
      slot_sloppy_immutable_silent: inner.local_sloppy_immutable_silent,
      upvalues: inner.captured_upvalues,
      has_rest: false,
      has_prototype_property: false,
      captures_this: false,
      arguments_slot: None,
      self_binding_slot: None,
      has_simple_parameter_list: true,
      return_type: None,
      is_strict: true,
      is_constructor: false,
      is_derived_constructor: false,
      is_class_constructor: false,
      is_async: false,
      is_generator: false,
    })
  }

  /// Emits a class declaration from an OAST class node.
  #[allow(clippy::too_many_lines)]
  pub(super) fn emit_class_from_oast(
    &mut self,
    c: &oast::Class<'_>,
    declaration_name: Option<&str>,
  ) -> Result<(), crate::errors::CompilerError> {
    let class_name = c
      .id
      .as_ref()
      .map(|id| id.name.to_string())
      .or_else(|| declaration_name.map(ToString::to_string))
      .unwrap_or_else(|| "<anonymous>".to_string());

    // Each class declaration creates fresh private-name identities while retaining
    // access to private names from enclosing classes.
    let enclosing_private_names = self.private_names.clone();
    let mut declared_private_names = HashSet::new();
    for element in &c.body.body {
      let key = match element {
        oast::ClassElement::MethodDefinition(method) => Some(&method.key),
        oast::ClassElement::PropertyDefinition(property) => Some(&property.key),
        oast::ClassElement::AccessorProperty(property) => Some(&property.key),
        _ => None,
      };
      if let Some(oast::PropertyKey::PrivateIdentifier(identifier)) = key {
        declared_private_names.insert(identifier.name.to_string());
      }
    }
    for name in declared_private_names {
      self.private_names.insert(
        name,
        Value::Symbol(crate::value::SymbolId::new_private(), None),
      );
    }

    let mut prepared_fields = Vec::new();
    for (index, element) in c.body.body.iter().enumerate() {
      let oast::ClassElement::PropertyDefinition(field) = element else {
        continue;
      };
      let key = if field.computed {
        let slot = self.alloc_temp_local(&format!("class_field_key_{index}"));
        crate::compiler::core::PreparedClassFieldKey::Computed(self.locals[slot].clone())
      } else {
        match &field.key {
          oast::PropertyKey::StaticIdentifier(identifier) => {
            crate::compiler::core::PreparedClassFieldKey::Named(identifier.name.to_string())
          }
          oast::PropertyKey::Identifier(identifier) => {
            crate::compiler::core::PreparedClassFieldKey::Named(identifier.name.to_string())
          }
          oast::PropertyKey::StringLiteral(literal) => {
            crate::compiler::core::PreparedClassFieldKey::Named(literal.value.to_string())
          }
          oast::PropertyKey::NumericLiteral(literal) => {
            crate::compiler::core::PreparedClassFieldKey::Named(super::numeric_property_key(
              literal.value,
            ))
          }
          oast::PropertyKey::PrivateIdentifier(identifier) => {
            crate::compiler::core::PreparedClassFieldKey::Private(
              self.ensure_private_name_constant(&identifier.name),
            )
          }
          _ => {
            return Err(crate::errors::CompilerError::from(
              "unsupported non-computed class field key",
            ));
          }
        }
      };
      let initializer_source = field.value.as_ref().map(|value| {
        let span = value.span();
        self.source_text[span.start as usize..span.end as usize].to_string()
      });
      prepared_fields.push(crate::compiler::core::PreparedClassField {
        key,
        initializer_source,
        is_static: field.r#static,
      });
    }

    let constructor_method = c.body.body.iter().find_map(|element| {
      if let oast::ClassElement::MethodDefinition(md) = element
        && matches!(md.kind, oast::MethodDefinitionKind::Constructor)
      {
        return Some(md);
      }
      None
    });
    let class_has_super = c.super_class.is_some();
    let proto = if let Some(md) = constructor_method {
      let outer_strict = self.strict_context;
      self.strict_context = true;
      let compiled = self.compile_function_with_instance_fields(
        &class_name,
        None,
        &md.value,
        None,
        class_has_super,
        &prepared_fields,
      );
      self.strict_context = outer_strict;
      let (mut proto, _env_len) = compiled?;
      proto.is_strict = true;
      proto.is_derived_constructor = class_has_super;
      proto.is_class_constructor = true;
      proto
    } else {
      let mut ctor_chunk = Chunk::new(Arc::clone(&self.source_text));
      if class_has_super {
        ctor_chunk.allow_new_target = true;
        ctor_chunk.allow_super = true;
        let mut inner = Compiler {
          chunk: ctor_chunk,
          risc: super::risc::RiscBuilder::new(),
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
          strict_context: true,
          loop_depth: 0,
          with_depth: 0,
          may_capture_with: self.with_depth > 0 || self.may_capture_with,
          handler_depth: 0,
          continue_patches: Vec::new(),
          break_patches: Vec::new(),
          finally_patches: Vec::new(),
          iterator_close_stack: Vec::new(),
          label_patches: Vec::new(),
          current_module: self.current_module.clone(),
          pending_operand_bytes: 0,
          allow_new_target: true,
          allow_super: true,
          outer_bindings: self.visible_bindings_snapshot(),
          upvalue_indices: HashMap::new(),
          captured_upvalues: Vec::new(),
          private_names: self.private_names.clone(),
          pending_instance_fields: prepared_fields.clone(),
          emission_error: None,
        };
        inner.add_local_with_mirror("arguments".to_string(), None, false);
        inner.emit(Op::GetGlobal, LINE);
        let global_idx = inner.global_name_index("Array");
        inner.emit_u16(inner.global_index_to_u16(global_idx)?, LINE);
        inner.emit_constant(Value::String("from".to_string().into()), LINE);
        inner.emit(Op::GetProperty, LINE);
        inner.emit(Op::LoadLocal, LINE);
        inner.emit_byte(0, LINE);
        inner.emit(Op::SuperConstruct, LINE);
        inner.emit_byte(1, LINE);
        inner.emit_pending_instance_field_initializers()?;
        inner.emit(Op::Discard, LINE);
        inner.emit(Op::GetThis, LINE);
        inner.emit(Op::Return, LINE);
        ctor_chunk = super::risc::RiscBuilder::finish(
          std::mem::take(&mut inner.chunk),
          std::mem::take(&mut inner.risc),
        )?;
        FunctionProto {
          name: class_name.clone(),
          span: c.span,
          arity: 0,
          length: 0,
          chunk: Arc::new(ctor_chunk),
          slot_types: inner.local_types,
          slot_names: inner.locals.into_iter().map(Some).collect(),
          lexical_tdz: inner.local_tdz,
          slot_immutable: inner.local_immutable,
          slot_sloppy_immutable_silent: inner.local_sloppy_immutable_silent,
          upvalues: inner.captured_upvalues,
          has_rest: false,
          has_prototype_property: true,
          captures_this: false,
          arguments_slot: Some(0),
          self_binding_slot: None,
          has_simple_parameter_list: true,
          return_type: None,
          is_strict: true,
          is_constructor: true,
          is_derived_constructor: true,
          is_class_constructor: true,
          is_async: false,
          is_generator: false,
        }
      } else {
        let mut inner = Compiler::new(Arc::clone(&self.source_text));
        inner.is_root = false;
        inner.strict_context = true;
        inner.current_module = self.current_module.clone();
        inner.outer_bindings = self.visible_bindings_snapshot();
        inner.private_names = self.private_names.clone();
        inner.pending_instance_fields = prepared_fields.clone();
        inner.emit_pending_instance_field_initializers()?;
        inner.emit(Op::GetThis, LINE);
        inner.emit(Op::Return, LINE);
        let ctor_chunk = super::risc::RiscBuilder::finish(
          std::mem::take(&mut inner.chunk),
          std::mem::take(&mut inner.risc),
        )?;
        FunctionProto {
          name: class_name.clone(),
          span: c.span,
          arity: 0,
          length: 0,
          chunk: Arc::new(ctor_chunk),
          slot_types: inner.local_types,
          slot_names: inner.locals.into_iter().map(Some).collect(),
          lexical_tdz: inner.local_tdz,
          slot_immutable: inner.local_immutable,
          slot_sloppy_immutable_silent: inner.local_sloppy_immutable_silent,
          upvalues: inner.captured_upvalues,
          has_rest: false,
          has_prototype_property: true,
          captures_this: false,
          arguments_slot: None,
          self_binding_slot: None,
          has_simple_parameter_list: true,
          return_type: None,
          is_strict: true,
          is_constructor: true,
          is_derived_constructor: false,
          is_class_constructor: true,
          is_async: false,
          is_generator: false,
        }
      }
    };

    let idx = self.chunk.add_function(proto);
    self.emit(Op::MakeClosure, LINE);
    self.emit_byte(self.function_index_to_u8(idx)?, LINE);
    if let Some(super_class) = &c.super_class {
      self.emit_get_builtin_name("classInitExtends");
      self.emit(Op::Exchange, LINE);
      self.compile_expr_oast(super_class)?;
      self.emit(Op::Call, LINE);
      self.emit_byte(2, LINE);
    }

    let ctor_slot = self.alloc_temp_local("class_ctor_tmp");
    self.emit(Op::StoreLocal, LINE);
    self.emit_slot(ctor_slot, LINE);

    let instance_home_slot = if c
      .body
      .body
      .iter()
      .any(|element| matches!(element, oast::ClassElement::MethodDefinition(md) if !md.r#static))
    {
      self.emit(Op::LoadLocal, LINE);
      self.emit_slot(ctor_slot, LINE);
      self.emit_constant(Value::String("prototype".to_string().into()), LINE);
      self.emit(Op::GetProperty, LINE);
      let home_slot = self.alloc_temp_local("class_home");
      self.emit(Op::StoreLocal, LINE);
      self.emit_slot(home_slot, LINE);
      Some(home_slot)
    } else {
      None
    };
    if let Some(home_slot) = instance_home_slot {
      self.emit(Op::LoadLocal, LINE);
      self.emit_slot(ctor_slot, LINE);
      self.emit_set_method_home_object(home_slot)?;
      self.emit(Op::Discard, LINE);
    }

    let mut prepared_field_iter = prepared_fields.iter();
    for element in &c.body.body {
      match element {
        oast::ClassElement::MethodDefinition(method) => {
          let target_slot = if method.r#static {
            ctor_slot
          } else {
            instance_home_slot.expect("instance home slot")
          };
          self.emit_class_member_installation(target_slot, method)?;
        }
        oast::ClassElement::PropertyDefinition(property) => {
          let field = prepared_field_iter
            .next()
            .expect("every class property has a prepared field");
          if property.computed {
            let crate::compiler::core::PreparedClassFieldKey::Computed(binding) = &field.key else {
              unreachable!("computed class fields have prepared key bindings")
            };
            let key = property
              .key
              .as_expression()
              .expect("computed class field key must be an expression");
            self.compile_expr_oast(key)?;
            let slot = self
              .resolve_local(binding)
              .expect("computed class field key binding must be local");
            self.emit(Op::StoreLocal, LINE);
            self.emit_slot(slot, LINE);
          }
          if field.is_static {
            self.emit_static_class_field(ctor_slot, field, property.value.as_ref())?;
          }
        }
        oast::ClassElement::StaticBlock(block) => {
          let proto = self.compile_static_block_function(block)?;
          let function_index = self.chunk.add_function(proto);
          self.emit(Op::LoadLocal, LINE);
          self.emit_slot(ctor_slot, LINE);
          self.emit(Op::MakeClosure, LINE);
          self.emit_byte(self.function_index_to_u8(function_index)?, LINE);
          self.emit_set_method_home_object(ctor_slot)?;
          self.emit(Op::CallBuiltin, LINE);
          self.emit_byte(0, LINE);
          self.emit(Op::Discard, LINE);
        }
        _ => {}
      }
    }

    self.emit(Op::LoadLocal, LINE);
    self.emit_slot(ctor_slot, LINE);
    self.private_names = enclosing_private_names;
    Ok(())
  }
}
