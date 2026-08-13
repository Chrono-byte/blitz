// SPDX-License-Identifier: MPL-2.0
//! Binding phase compilation
//!
//! Handles variable declaration compilation and binding pattern assignment,
//! including support for destructuring patterns (object and array destructuring)
//! in variable declarations.
//!
//! This module is part of the compiler's multi-phase compilation process.
//! The binding phase processes variable declarations and manages the storage
//! of binding identifiers in local scopes.

use super::emit::FrontendOp as Op;
use crate::value::Value;
use oxc_ast::ast as oast;
use std::collections::HashSet;

const LINE: u32 = 1;

enum PreparedPropertyKey {
  Constant(Value),
  Local(usize),
}

impl super::Compiler {
  fn binding_identifier_name(pattern: &oast::BindingPattern<'_>) -> Option<String> {
    match pattern {
      oast::BindingPattern::BindingIdentifier(id) => Some(id.name.to_string()),
      _ => None,
    }
  }

  fn is_anonymous_function_definition(expr: &oast::Expression<'_>) -> bool {
    match expr {
      oast::Expression::ArrowFunctionExpression(_) => true,
      oast::Expression::FunctionExpression(f) => f.id.is_none(),
      oast::Expression::ClassExpression(c) => c.id.is_none(),
      oast::Expression::ParenthesizedExpression(p) => {
        Self::is_anonymous_function_definition(&p.expression)
      }
      _ => false,
    }
  }

  fn assignment_target_identifier_name(target: &oast::AssignmentTarget<'_>) -> Option<String> {
    match target {
      oast::AssignmentTarget::AssignmentTargetIdentifier(id) => Some(id.name.to_string()),
      _ => None,
    }
  }

  fn emit_prepared_property_key(
    &mut self,
    key: &PreparedPropertyKey,
  ) -> Result<(), crate::errors::CompilerError> {
    match key {
      PreparedPropertyKey::Constant(value) => self.emit_constant(value.clone(), LINE),
      PreparedPropertyKey::Local(slot) => {
        self.emit(Op::LoadLocal, LINE);
        self.emit_byte(self.slot_to_u8(*slot)?, LINE);
      }
    }
    Ok(())
  }

  fn compile_destructuring_property_key(
    &mut self,
    key: &oast::PropertyKey<'_>,
  ) -> Result<PreparedPropertyKey, crate::errors::CompilerError> {
    if let Some(value) = Self::binding_property_key_to_value(key) {
      self.emit_constant(value.clone(), LINE);
      return Ok(PreparedPropertyKey::Constant(value));
    }
    let expression = key.as_expression().ok_or_else(|| {
      crate::errors::CompilerError::from("unsupported destructuring property key")
    })?;
    self.emit_get_builtin_name("toPropertyKey");
    self.compile_expr_oast(expression)?;
    self.emit(Op::Call, LINE);
    self.emit_byte(1, LINE);
    let slot = self.alloc_temp_local("destructuring_property_key");
    self.emit(Op::StoreLocal, LINE);
    self.emit_byte(self.slot_to_u8(slot)?, LINE);
    self.emit(Op::LoadLocal, LINE);
    self.emit_byte(self.slot_to_u8(slot)?, LINE);
    Ok(PreparedPropertyKey::Local(slot))
  }

  fn compile_object_rest_value(
    &mut self,
    excluded: &[PreparedPropertyKey],
  ) -> Result<(), crate::errors::CompilerError> {
    let source_slot = self.alloc_temp_local("object_rest_source");
    self.emit(Op::StoreLocal, LINE);
    self.emit_byte(self.slot_to_u8(source_slot)?, LINE);
    self.emit_get_builtin_name("copyDataProperties");
    self.emit(Op::LoadLocal, LINE);
    self.emit_byte(self.slot_to_u8(source_slot)?, LINE);
    for key in excluded {
      self.emit_prepared_property_key(key)?;
    }
    self.emit(Op::Call, LINE);
    self.emit_byte(
      u8::try_from(excluded.len() + 1)
        .map_err(|_| crate::errors::CompilerError::from("too many object rest exclusions"))?,
      LINE,
    );
    Ok(())
  }

  /// Compile a variable declaration statement.
  ///
  /// Processes all declarators in the declaration, evaluating their initializers
  /// (or `undefined` if no initializer) and assigning them to their binding patterns.
  pub(super) fn compile_variable_declaration_oast(
    &mut self,
    v: &oast::VariableDeclaration<'_>,
  ) -> Result<(), crate::errors::CompilerError> {
    for dec in &v.declarations {
      if matches!(v.kind, oxc_ast::ast::VariableDeclarationKind::Var)
        && self.with_depth > 0
        && let oast::BindingPattern::BindingIdentifier(id) = &dec.id
      {
        let name = id.name.as_str();
        if let Some(err) = Self::strict_binding_name_error(self.strict_context, name) {
          return Err(err);
        }
        if let Some(err) = self.check_var_lexical_conflict(name) {
          return Err(crate::errors::CompilerError::from(err));
        }
        if self.resolve_local(name).is_none() {
          self.add_var_local(name.to_string(), None, self.is_root);
        }
        if let Some(init) = &dec.init {
          self.compile_expr_oast(init)?;
          if Self::is_anonymous_function_definition(init) {
            let value_slot = self.alloc_reusable_temp_local("variable_initializer");
            self.emit(Op::StoreLocal, LINE);
            self.emit_byte(self.slot_to_u8(value_slot)?, LINE);
            self.emit_get_builtin_name("setFunctionNameIfAnonymous");
            self.emit(Op::LoadLocal, LINE);
            self.emit_byte(self.slot_to_u8(value_slot)?, LINE);
            self.emit_constant(Value::String(name.to_string().into()), LINE);
            self.emit(Op::Call, LINE);
            self.emit_byte(2, LINE);
          }
          // Inside `with`, a `var` initializer should resolve
          // through the scope chain at runtime so an object
          // property can shadow the hoisted binding.
          self.emit_set_global_name(name);
        }
        continue;
      }

      if matches!(
        v.kind,
        oxc_ast::ast::VariableDeclarationKind::Let | oxc_ast::ast::VariableDeclarationKind::Const
      ) {
        let mut names = HashSet::new();
        self.add_var_names_from_binding(&dec.id, &mut names);
        let mut names: Vec<_> = names.into_iter().collect();
        names.sort();
        for name in names {
          let slot = self.predeclare_lexical_binding(&name, v.kind)?;
          // A loop body can execute the same bytecode repeatedly. Each entry
          // creates fresh lexical bindings in the TDZ before any initializer
          // is evaluated. Ordinary blocks and function bodies already start
          // with fresh frame cells; replacing those would detach closures
          // created during declaration instantiation from the initialized
          // binding.
          if self.loop_depth > 0 {
            self.emit(Op::ResetLocalCell, LINE);
            self.emit_byte(self.slot_to_u8(slot)?, LINE);
          }
        }
      }

      if let Some(init) = &dec.init {
        self.compile_expr_oast(init)?;
        if let Some(name) = Self::binding_identifier_name(&dec.id)
          && Self::is_anonymous_function_definition(init)
        {
          let value_slot = self.alloc_reusable_temp_local("variable_initializer");
          self.emit(Op::StoreLocal, LINE);
          self.emit_byte(self.slot_to_u8(value_slot)?, LINE);
          self.emit_get_builtin_name("setFunctionNameIfAnonymous");
          self.emit(Op::LoadLocal, LINE);
          self.emit_byte(self.slot_to_u8(value_slot)?, LINE);
          self.emit_constant(Value::String(name.into()), LINE);
          self.emit(Op::Call, LINE);
          self.emit_byte(2, LINE);
        }
      } else {
        self.emit(Op::Undefined, LINE);
      }
      self.compile_binding_pattern_assignment_oast(&dec.id, v.kind)?;
    }

    Ok(())
  }

  fn predeclare_lexical_binding(
    &mut self,
    name: &str,
    kind: oxc_ast::ast::VariableDeclarationKind,
  ) -> Result<usize, crate::errors::CompilerError> {
    if let Some(err) = Self::strict_binding_name_error(self.strict_context, name) {
      return Err(err);
    }
    let scope_index = self.scopes.len() - 1;
    if self.scopes[scope_index]
      .lexical_bindings
      .contains(name)
      && let Some(slot) = self.scopes[scope_index]
        .bindings
        .get(name)
        .copied()
    {
      return Ok(slot);
    }
    let slot = self.add_local_with_mirror(name.to_string(), None, false);
    self.mark_local_tdz(slot);
    self.add_lexical_binding(name);
    if matches!(kind, oxc_ast::ast::VariableDeclarationKind::Const) {
      self.mark_local_immutable(slot);
    }
    Ok(slot)
  }

  /// Convert an object binding key to a string value.
  ///
  /// Extracts the string representation from various types of property keys
  /// used in destructuring patterns. Returns `None` for computed properties
  /// that cannot be statically determined.
  pub(super) fn binding_property_key_to_value(key: &oast::PropertyKey<'_>) -> Option<Value> {
    match key {
      oast::PropertyKey::StaticIdentifier(i) => Some(Value::String(i.name.to_string().into())),
      oast::PropertyKey::Identifier(i) => Some(Value::String(i.name.to_string().into())),
      oast::PropertyKey::StringLiteral(s) => Some(Value::String(s.value.to_string().into())),
      oast::PropertyKey::NumericLiteral(n) => {
        Some(Value::String(super::numeric_property_key(n.value).into()))
      }
      _ => None,
    }
  }

  /// Store a binding identifier in the current scope.
  ///
  /// Handles variable scope management, including:
  /// - Conflict detection between `var` and lexical bindings
  /// - Duplicate lexical binding detection
  /// - Temporal Dead Zone (TDZ) marking for `let` and `const`
  /// - Global mirroring for root scope variables
  /// - Module export tracking
  pub(super) fn store_binding_identifier(
    &mut self,
    name: &str,
    kind: oxc_ast::ast::VariableDeclarationKind,
  ) -> Result<(), crate::errors::CompilerError> {
    if let Some(err) = Self::strict_binding_name_error(self.strict_context, name) {
      return Err(err);
    }
    let is_var = matches!(kind, oxc_ast::ast::VariableDeclarationKind::Var);
    let mirror_global = self.is_root && is_var;

    if matches!(kind, oxc_ast::ast::VariableDeclarationKind::Var)
      && let Some(err) = self.check_var_lexical_conflict(name)
    {
      return Err(crate::errors::CompilerError::from(err));
    }

    let slot = if is_var {
      self
        .resolve_local(name)
        .unwrap_or_else(|| self.add_var_local(name.to_string(), None, mirror_global))
    } else if let Some(slot) = self.scopes[self.scopes.len() - 1]
      .bindings
      .get(name)
      .copied()
    {
      slot
    } else {
      let slot = self.add_local_with_mirror(name.to_string(), None, mirror_global);
      self.mark_local_tdz(slot);
      self.add_lexical_binding(name);
      slot
    };
    if matches!(kind, oxc_ast::ast::VariableDeclarationKind::Const) {
      self.mark_local_immutable(slot);
    }
    if mirror_global {
      self.emit(Op::Copy, LINE);
    }
    self.emit(Op::StoreLocal, LINE);
    self.emit_byte(self.slot_to_u8(slot)?, LINE);
    if self.is_root && mirror_global {
      self.emit_set_global_name(name);
    }
    let is_exported = self
      .current_module
      .as_ref()
      .is_some_and(|c| c.exports.contains(name));
    if is_exported {
      self.emit(Op::LoadLocal, LINE);
      self.emit_byte(self.slot_to_u8(slot)?, LINE);
      self.emit_set_own_module_export(name)?;
    }
    Ok(())
  }

  /// Compile a binding pattern assignment.
  ///
  /// Recursively processes binding patterns, which can be:
  /// - Simple identifiers (direct binding)
  /// - Assignment patterns (patterns with default values)
  /// - Array patterns (array destructuring)
  /// - Object patterns (object destructuring)
  ///
  /// For destructuring patterns, this emits code to extract values from the
  /// compiler's virtual value stack and assign them to their respective bindings.
  pub(super) fn store_assignment_identifier(
    &mut self,
    name: &str,
  ) -> Result<(), crate::errors::CompilerError> {
    if let Some(err) = Self::strict_binding_name_error(self.strict_context, name) {
      return Err(err);
    }
    if self
      .current_module
      .as_ref()
      .is_some_and(|c| c.imports.contains_key(name))
    {
      return Err(Self::imported_binding_assignment_error());
    }
    if self.with_depth > 0 {
      self.emit_set_global_name(name);
    } else if let Some(slot) = self.resolve_local(name) {
      self.emit(Op::StoreLocal, LINE);
      self.emit_byte(self.slot_to_u8(slot)?, LINE);
      if self.local_mirrors_global(slot) {
        self.emit(Op::LoadLocal, LINE);
        self.emit_byte(self.slot_to_u8(slot)?, LINE);
        self.emit_set_global_name(name);
      }
      let is_exported = self
        .current_module
        .as_ref()
        .is_some_and(|c| c.exports.contains(name));
      if is_exported {
        let module_index = self
          .current_module
          .as_ref()
          .map(|c| c.module_index)
          .expect("module_index expected");
        let descriptor = self.module_export_descriptor_index(module_index, name)?;
        self.emit(Op::SetModuleExport, LINE);
        self.emit_byte(descriptor.to_le_bytes()[0], LINE);
        self.emit_byte(descriptor.to_le_bytes()[1], LINE);
      }
    } else if let Some(upvalue) = self.resolve_upvalue(name) {
      self.emit(Op::SetUpvalue, LINE);
      self.emit_byte(upvalue, LINE);
    } else {
      self.emit_set_global_name(name);
    }
    Ok(())
  }

  pub(super) fn compile_assignment_identifier_reference(
    &mut self,
    id: &oast::IdentifierReference<'_>,
  ) -> Result<(), crate::errors::CompilerError> {
    self.store_assignment_identifier(id.name.as_str())
  }

  pub(super) fn compile_static_member_assignment_target(
    &mut self,
    m: &oast::StaticMemberExpression<'_>,
  ) -> Result<(), crate::errors::CompilerError> {
    let value_slot = self.alloc_temp_local("assign_member_value");
    self.emit(Op::StoreLocal, LINE);
    self.emit_byte(self.slot_to_u8(value_slot)?, LINE);
    self.compile_expr_oast(&m.object)?;
    self.emit_constant(Value::String(m.property.name.to_string().into()), LINE);
    self.emit(Op::LoadLocal, LINE);
    self.emit_byte(self.slot_to_u8(value_slot)?, LINE);
    self.emit(Op::SetProperty, LINE);
    self.emit(Op::Discard, LINE);
    Ok(())
  }

  pub(super) fn compile_computed_member_assignment_target(
    &mut self,
    m: &oast::ComputedMemberExpression<'_>,
  ) -> Result<(), crate::errors::CompilerError> {
    let value_slot = self.alloc_temp_local("assign_member_value");
    self.emit(Op::StoreLocal, LINE);
    self.emit_byte(self.slot_to_u8(value_slot)?, LINE);
    self.compile_expr_oast(&m.object)?;
    self.compile_expr_oast(&m.expression)?;
    self.emit(Op::LoadLocal, LINE);
    self.emit_byte(self.slot_to_u8(value_slot)?, LINE);
    self.emit(Op::SetProperty, LINE);
    self.emit(Op::Discard, LINE);
    Ok(())
  }

  fn begin_array_destructuring_iterator(
    &mut self,
    label: &str,
  ) -> Result<(usize, usize, usize), crate::errors::CompilerError> {
    self.emit_get_builtin_name("requireObjectCoercible");
    self.emit(Op::Exchange, LINE);
    self.emit(Op::Call, LINE);
    self.emit_byte(1, LINE);

    let iterator_slot = self.alloc_temp_local(label);
    self.emit(Op::Copy, LINE);
    self.emit_get_global_name("Symbol");
    self.emit_constant(Value::String("iterator".to_string().into()), LINE);
    self.emit(Op::GetProperty, LINE);
    self.emit(Op::GetProperty, LINE);
    self.emit(Op::CallBuiltin, LINE);
    self.emit_byte(0, LINE);
    self.emit(Op::StoreLocal, LINE);
    self.emit_byte(self.slot_to_u8(iterator_slot)?, LINE);

    // IteratorDestructuringAssignmentEvaluation closes the iterator when any
    // subsequent step completes abruptly. Keep the original completion while
    // running IteratorClose; an abrupt close must not replace it.
    let thrown_slot = self.alloc_temp_local("array_destructuring_thrown");
    self.emit(Op::PushHandler, LINE);
    let handler_jump = self.current_ip();
    self.emit_byte(0, LINE);
    self.emit_byte(0, LINE);
    self.handler_depth += 1;
    Ok((iterator_slot, thrown_slot, handler_jump))
  }

  fn finish_array_destructuring_iterator(
    &mut self,
    iterator_slot: usize,
    thrown_slot: usize,
    handler_jump: usize,
    close_on_normal: bool,
  ) -> Result<(), crate::errors::CompilerError> {
    self.handler_depth = self.handler_depth.saturating_sub(1);
    self.emit(Op::PopHandler, LINE);
    // A rest element exhausts the iterator through iteratorToArray; the
    // existing rest path deliberately has no separate normal close.
    if close_on_normal {
      self.emit(Op::LoadLocal, LINE);
      self.emit_byte(self.slot_to_u8(iterator_slot)?, LINE);
      self.emit(Op::IteratorClose, LINE);
      self.emit(Op::Discard, LINE);
    }
    self.emit(Op::Jump, LINE);
    let end_jump = self.current_ip();
    self.emit_byte(0, LINE);
    self.emit_byte(0, LINE);

    self.patch_jump(handler_jump);
    self.emit(Op::StoreLocal, LINE);
    self.emit_byte(self.slot_to_u8(thrown_slot)?, LINE);

    // Protect the close itself so that an abrupt IteratorClose preserves the
    // completion that caused the destructuring operation to be closed.
    self.emit(Op::PushHandler, LINE);
    let close_handler_jump = self.current_ip();
    self.emit_byte(0, LINE);
    self.emit_byte(0, LINE);
    self.emit(Op::LoadLocal, LINE);
    self.emit_byte(self.slot_to_u8(iterator_slot)?, LINE);
    self.emit(Op::IteratorClose, LINE);
    self.emit(Op::Discard, LINE);
    self.emit(Op::PopHandler, LINE);
    self.emit(Op::Jump, LINE);
    let close_done_jump = self.current_ip();
    self.emit_byte(0, LINE);
    self.emit_byte(0, LINE);
    self.patch_jump(close_handler_jump);
    self.emit(Op::Discard, LINE);
    self.patch_jump(close_done_jump);

    self.emit(Op::LoadLocal, LINE);
    self.emit_byte(self.slot_to_u8(thrown_slot)?, LINE);
    self.emit(Op::Throw, LINE);
    self.patch_jump(end_jump);
    Ok(())
  }

  pub(super) fn compile_assignment_target_maybe_default_oast(
    &mut self,
    target: &oast::AssignmentTargetMaybeDefault<'_>,
  ) -> Result<(), crate::errors::CompilerError> {
    match target {
      oast::AssignmentTargetMaybeDefault::AssignmentTargetWithDefault(a) => {
        let temp_slot = self.alloc_temp_local("assign_default");
        self.emit(Op::StoreLocal, LINE);
        self.emit_byte(self.slot_to_u8(temp_slot)?, LINE);
        self.emit(Op::LoadLocal, LINE);
        self.emit_byte(self.slot_to_u8(temp_slot)?, LINE);
        self.emit(Op::Undefined, LINE);
        // Use CompareJump: cond_type 6 = StrictEq, jump if false.
        self.emit_with_payload(Op::CompareJump, 6, LINE);
        let skip_default = self.current_ip();
        self.emit_byte(0, LINE);
        self.emit_byte(0, LINE);
        self.compile_expr_oast(&a.init)?;
        self.emit(Op::StoreLocal, LINE);
        self.emit_byte(self.slot_to_u8(temp_slot)?, LINE);
        if let Some(name) = Self::assignment_target_identifier_name(&a.binding)
          && Self::is_anonymous_function_definition(&a.init)
        {
          self.emit_get_builtin_name("setFunctionNameIfAnonymous");
          self.emit(Op::LoadLocal, LINE);
          self.emit_byte(self.slot_to_u8(temp_slot)?, LINE);
          self.emit_constant(Value::String(name.into()), LINE);
          self.emit(Op::Call, LINE);
          self.emit_byte(2, LINE);
          self.emit(Op::StoreLocal, LINE);
          self.emit_byte(self.slot_to_u8(temp_slot)?, LINE);
        }
        self.patch_jump(skip_default);
        self.emit(Op::LoadLocal, LINE);
        self.emit_byte(self.slot_to_u8(temp_slot)?, LINE);
        self.compile_assignment_target_oast(&a.binding)?;
        Ok(())
      }
      oast::AssignmentTargetMaybeDefault::AssignmentTargetIdentifier(id) => {
        self.compile_assignment_identifier_reference(id)
      }
      oast::AssignmentTargetMaybeDefault::StaticMemberExpression(m) => {
        self.compile_static_member_assignment_target(m)
      }
      oast::AssignmentTargetMaybeDefault::ComputedMemberExpression(m) => {
        self.compile_computed_member_assignment_target(m)
      }
      oast::AssignmentTargetMaybeDefault::ArrayAssignmentTarget(a) => {
        self.compile_array_assignment_target(a)
      }
      oast::AssignmentTargetMaybeDefault::ObjectAssignmentTarget(o) => {
        self.compile_object_assignment_target(o)
      }
      _ => Err(crate::errors::CompilerError::from(
        "unsupported assignment target",
      )),
    }
  }

  pub(super) fn compile_array_assignment_target(
    &mut self,
    a: &oast::ArrayAssignmentTarget<'_>,
  ) -> Result<(), crate::errors::CompilerError> {
    let (iterator_slot, thrown_slot, handler_jump) =
      self.begin_array_destructuring_iterator("array_assign_iterator")?;

    for el in &a.elements {
      self.emit(Op::LoadLocal, LINE);
      self.emit_byte(self.slot_to_u8(iterator_slot)?, LINE);
      self.emit(Op::IteratorNext, LINE);
      if let Some(el) = el {
        self.emit(Op::Copy, LINE);
        self.emit_constant(Value::String("done".to_string().into()), LINE);
        self.emit(Op::GetProperty, LINE);
        self.emit(Op::JumpIfTrue, LINE);
        let done_jump = self.current_ip();
        self.emit_byte(0, LINE);
        self.emit_byte(0, LINE);
        self.emit_constant(Value::String("value".to_string().into()), LINE);
        self.emit(Op::GetProperty, LINE);
        self.compile_assignment_target_maybe_default_oast(el)?;
        self.emit(Op::Jump, LINE);
        let after_jump = self.current_ip();
        self.emit_byte(0, LINE);
        self.emit_byte(0, LINE);
        self.patch_jump(done_jump);
        self.emit(Op::Discard, LINE);
        self.emit(Op::Undefined, LINE);
        self.compile_assignment_target_maybe_default_oast(el)?;
        self.patch_jump(after_jump);
      } else {
        self.emit(Op::Discard, LINE);
      }
    }
    if let Some(rest) = &a.rest {
      self.emit(Op::LoadLocal, LINE);
      self.emit_byte(self.slot_to_u8(iterator_slot)?, LINE);
      self.emit_get_builtin_name("iteratorToArray");
      self.emit(Op::Exchange, LINE);
      self.emit(Op::Call, LINE);
      self.emit_byte(1, LINE);
      self.compile_assignment_target_oast(&rest.target)?;
    }
    self.finish_array_destructuring_iterator(
      iterator_slot,
      thrown_slot,
      handler_jump,
      a.rest.is_none(),
    )?;
    Ok(())
  }

  pub(super) fn compile_object_assignment_target(
    &mut self,
    o: &oast::ObjectAssignmentTarget<'_>,
  ) -> Result<(), crate::errors::CompilerError> {
    let mut excluded = Vec::with_capacity(o.properties.len());
    for prop in &o.properties {
      match prop {
        oast::AssignmentTargetProperty::AssignmentTargetPropertyIdentifier(p) => {
          let key = Value::String(p.binding.name.to_string().into());
          excluded.push(PreparedPropertyKey::Constant(key.clone()));
          self.emit(Op::Copy, LINE);
          self.emit_constant(key, LINE);
          self.emit(Op::GetProperty, LINE);
          if let Some(init) = &p.init {
            let temp_slot = self.alloc_temp_local("assign_prop_default");
            self.emit(Op::StoreLocal, LINE);
            self.emit_byte(self.slot_to_u8(temp_slot)?, LINE);
            self.emit(Op::LoadLocal, LINE);
            self.emit_byte(self.slot_to_u8(temp_slot)?, LINE);
            self.emit(Op::Undefined, LINE);
            // Use CompareJump: cond_type 6 = StrictEq, jump if false.
            self.emit_with_payload(Op::CompareJump, 6, LINE);
            let skip_default = self.current_ip();
            self.emit_byte(0, LINE);
            self.emit_byte(0, LINE);
            self.compile_expr_oast(init)?;
            self.emit(Op::StoreLocal, LINE);
            self.emit_byte(self.slot_to_u8(temp_slot)?, LINE);
            if Self::is_anonymous_function_definition(init) {
              self.emit_get_builtin_name("setFunctionNameIfAnonymous");
              self.emit(Op::LoadLocal, LINE);
              self.emit_byte(self.slot_to_u8(temp_slot)?, LINE);
              self.emit_constant(Value::String(p.binding.name.to_string().into()), LINE);
              self.emit(Op::Call, LINE);
              self.emit_byte(2, LINE);
              self.emit(Op::StoreLocal, LINE);
              self.emit_byte(self.slot_to_u8(temp_slot)?, LINE);
            }
            self.patch_jump(skip_default);
            self.emit(Op::LoadLocal, LINE);
            self.emit_byte(self.slot_to_u8(temp_slot)?, LINE);
          }
          self.store_assignment_identifier(p.binding.name.as_str())?;
        }
        oast::AssignmentTargetProperty::AssignmentTargetPropertyProperty(p) => {
          self.emit(Op::Copy, LINE);
          let key = self.compile_destructuring_property_key(&p.name)?;
          self.emit(Op::GetProperty, LINE);
          self.compile_assignment_target_maybe_default_oast(&p.binding)?;
          excluded.push(key);
        }
      }
    }
    if let Some(rest) = &o.rest {
      self.compile_object_rest_value(&excluded)?;
      self.compile_assignment_target_oast(&rest.target)?;
    } else {
      self.emit(Op::Discard, LINE);
    }
    Ok(())
  }

  pub(super) fn compile_assignment_target_oast(
    &mut self,
    target: &oast::AssignmentTarget<'_>,
  ) -> Result<(), crate::errors::CompilerError> {
    match target {
      oast::AssignmentTarget::AssignmentTargetIdentifier(id) => {
        self.compile_assignment_identifier_reference(id)
      }
      oast::AssignmentTarget::StaticMemberExpression(m) => {
        self.compile_static_member_assignment_target(m)
      }
      oast::AssignmentTarget::ComputedMemberExpression(m) => {
        self.compile_computed_member_assignment_target(m)
      }
      oast::AssignmentTarget::ArrayAssignmentTarget(a) => self.compile_array_assignment_target(a),
      oast::AssignmentTarget::ObjectAssignmentTarget(o) => self.compile_object_assignment_target(o),
      _ => Err(crate::errors::CompilerError::from(
        "unsupported assignment target",
      )),
    }
  }

  pub(super) fn compile_binding_pattern_assignment_oast(
    &mut self,
    pattern: &oast::BindingPattern<'_>,
    kind: oxc_ast::ast::VariableDeclarationKind,
  ) -> Result<(), crate::errors::CompilerError> {
    match pattern {
      oast::BindingPattern::BindingIdentifier(i) => {
        self.store_binding_identifier(i.name.as_str(), kind)?;
      }
      oast::BindingPattern::AssignmentPattern(a) => {
        let temp_slot = self.alloc_temp_local("binding_default");
        self.emit(Op::StoreLocal, LINE);
        self.emit_byte(self.slot_to_u8(temp_slot)?, LINE);
        self.emit(Op::LoadLocal, LINE);
        self.emit_byte(self.slot_to_u8(temp_slot)?, LINE);
        self.emit(Op::Undefined, LINE);
        // Use CompareJump: cond_type 6 = StrictEq, jump if false.
        self.emit_with_payload(Op::CompareJump, 6, LINE);
        let skip_default = self.current_ip();
        self.emit_byte(0, LINE);
        self.emit_byte(0, LINE);
        self.compile_expr_oast(&a.right)?;
        self.emit(Op::StoreLocal, LINE);
        self.emit_byte(self.slot_to_u8(temp_slot)?, LINE);
        if let Some(name) = Self::binding_identifier_name(&a.left)
          && Self::is_anonymous_function_definition(&a.right)
        {
          self.emit_get_builtin_name("setFunctionNameIfAnonymous");
          self.emit(Op::LoadLocal, LINE);
          self.emit_byte(self.slot_to_u8(temp_slot)?, LINE);
          self.emit_constant(Value::String(name.into()), LINE);
          self.emit(Op::Call, LINE);
          self.emit_byte(2, LINE);
          self.emit(Op::StoreLocal, LINE);
          self.emit_byte(self.slot_to_u8(temp_slot)?, LINE);
        }
        self.patch_jump(skip_default);
        self.emit(Op::LoadLocal, LINE);
        self.emit_byte(self.slot_to_u8(temp_slot)?, LINE);
        self.compile_binding_pattern_assignment_oast(&a.left, kind)?;
      }
      oast::BindingPattern::ArrayPattern(a) => {
        let (iterator_slot, thrown_slot, handler_jump) =
          self.begin_array_destructuring_iterator("array_binding_iterator")?;

        for el in &a.elements {
          self.emit(Op::LoadLocal, LINE);
          self.emit_byte(self.slot_to_u8(iterator_slot)?, LINE);
          self.emit(Op::IteratorNext, LINE);

          if let Some(el) = el {
            self.emit(Op::Copy, LINE);
            self.emit_constant(Value::String("done".to_string().into()), LINE);
            self.emit(Op::GetProperty, LINE);
            self.emit(Op::JumpIfTrue, LINE);
            let done_jump = self.current_ip();
            self.emit_byte(0, LINE);
            self.emit_byte(0, LINE);
            self.emit_constant(Value::String("value".to_string().into()), LINE);
            self.emit(Op::GetProperty, LINE);
            self.compile_binding_pattern_assignment_oast(el, kind)?;
            self.emit(Op::Jump, LINE);
            let after_jump = self.current_ip();
            self.emit_byte(0, LINE);
            self.emit_byte(0, LINE);
            self.patch_jump(done_jump);
            self.emit(Op::Discard, LINE);
            self.emit(Op::Undefined, LINE);
            self.compile_binding_pattern_assignment_oast(el, kind)?;
            self.patch_jump(after_jump);
          } else {
            self.emit(Op::Discard, LINE);
          }
        }

        if let Some(rest) = &a.rest {
          self.emit(Op::LoadLocal, LINE);
          self.emit_byte(self.slot_to_u8(iterator_slot)?, LINE);
          self.emit_get_builtin_name("iteratorToArray");
          self.emit(Op::Exchange, LINE);
          self.emit(Op::Call, LINE);
          self.emit_byte(1, LINE);
          self.compile_binding_pattern_assignment_oast(&rest.argument, kind)?;
        }
        self.finish_array_destructuring_iterator(
          iterator_slot,
          thrown_slot,
          handler_jump,
          a.rest.is_none(),
        )?;
      }
      oast::BindingPattern::ObjectPattern(o) => {
        self.emit_get_builtin_name("requireObjectCoercible");
        self.emit(Op::Exchange, LINE);
        self.emit(Op::Call, LINE);
        self.emit_byte(1, LINE);
        let mut excluded = Vec::with_capacity(o.properties.len());
        for prop in &o.properties {
          self.emit(Op::Copy, LINE);
          let key = self.compile_destructuring_property_key(&prop.key)?;
          self.emit(Op::GetProperty, LINE);
          self.compile_binding_pattern_assignment_oast(&prop.value, kind)?;
          excluded.push(key);
        }
        if let Some(rest) = &o.rest {
          self.compile_object_rest_value(&excluded)?;
          self.compile_binding_pattern_assignment_oast(&rest.argument, kind)?;
        } else {
          self.emit(Op::Discard, LINE);
        }
      }
    }
    Ok(())
  }
}
