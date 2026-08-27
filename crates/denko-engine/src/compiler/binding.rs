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

enum PreparedAssignmentTarget {
  Identifier(String),
  StaticMember {
    object_slot: usize,
    property: Value,
  },
  ComputedMember {
    object_slot: usize,
    key_slot: usize,
  },
  SuperStatic {
    receiver_slot: usize,
    property: Value,
  },
  SuperComputed {
    receiver_slot: usize,
    key_slot: usize,
  },
}

impl super::Compiler {
  pub(super) fn emit_resolve_with_identifier(
    &mut self,
    name: &str,
  ) -> Result<usize, crate::errors::CompilerError> {
    let reference_slot = self.alloc_temp_local("with_identifier_reference");
    self.emit_get_builtin_name("resolveWithBindingObject");
    self.emit_constant(Value::String(name.into()), LINE);
    self.emit(Op::Call, LINE);
    self.emit_byte(1, LINE);
    self.emit(Op::StoreLocal, LINE);
    self.emit_byte(self.slot_to_u8(reference_slot)?, LINE);
    Ok(reference_slot)
  }

  pub(super) fn emit_get_resolved_identifier(
    &mut self,
    reference_slot: usize,
    name: &str,
  ) -> Result<(), crate::errors::CompilerError> {
    self.emit_get_builtin_name("getResolvedIdentifier");
    self.emit(Op::LoadLocal, LINE);
    self.emit_byte(self.slot_to_u8(reference_slot)?, LINE);
    self.emit_constant(Value::String(name.into()), LINE);
    self.emit(Op::Call, LINE);
    self.emit_byte(2, LINE);
    Ok(())
  }

  pub(super) fn emit_set_resolved_identifier(
    &mut self,
    reference_slot: usize,
    name: &str,
  ) -> Result<(), crate::errors::CompilerError> {
    let value_slot = self.alloc_reusable_temp_local("with_identifier_value");
    self.emit(Op::StoreLocal, LINE);
    self.emit_byte(self.slot_to_u8(value_slot)?, LINE);
    self.emit_get_builtin_name("setResolvedIdentifier");
    self.emit(Op::LoadLocal, LINE);
    self.emit_byte(self.slot_to_u8(reference_slot)?, LINE);
    self.emit_constant(Value::String(name.into()), LINE);
    self.emit(Op::LoadLocal, LINE);
    self.emit_byte(self.slot_to_u8(value_slot)?, LINE);
    self.emit(Op::Call, LINE);
    self.emit_byte(3, LINE);
    Ok(())
  }

  fn prepare_assignment_target(
    &mut self,
    target: &oast::AssignmentTarget<'_>,
  ) -> Result<Option<PreparedAssignmentTarget>, crate::errors::CompilerError> {
    match target {
      oast::AssignmentTarget::AssignmentTargetIdentifier(id) => Ok(Some(
        PreparedAssignmentTarget::Identifier(id.name.to_string()),
      )),
      oast::AssignmentTarget::StaticMemberExpression(member) => {
        if matches!(&member.object, oast::Expression::Super(_)) {
          let receiver_slot = self.alloc_temp_local("destructuring_super_receiver");
          self.emit(Op::GetThis, LINE);
          self.emit(Op::StoreLocal, LINE);
          self.emit_byte(self.slot_to_u8(receiver_slot)?, LINE);
          return Ok(Some(PreparedAssignmentTarget::SuperStatic {
            receiver_slot,
            property: Value::String(member.property.name.to_string().into()),
          }));
        }
        self.compile_expr_oast(&member.object)?;
        let object_slot = self.alloc_temp_local("destructuring_target_object");
        self.emit(Op::StoreLocal, LINE);
        self.emit_byte(self.slot_to_u8(object_slot)?, LINE);
        Ok(Some(PreparedAssignmentTarget::StaticMember {
          object_slot,
          property: Value::String(member.property.name.to_string().into()),
        }))
      }
      oast::AssignmentTarget::ComputedMemberExpression(member) => {
        if matches!(&member.object, oast::Expression::Super(_)) {
          let receiver_slot = self.alloc_temp_local("destructuring_super_receiver");
          self.emit(Op::GetThis, LINE);
          self.emit(Op::StoreLocal, LINE);
          self.emit_byte(self.slot_to_u8(receiver_slot)?, LINE);
          self.compile_expr_oast(&member.expression)?;
          let key_slot = self.alloc_temp_local("destructuring_super_key");
          self.emit(Op::StoreLocal, LINE);
          self.emit_byte(self.slot_to_u8(key_slot)?, LINE);
          return Ok(Some(PreparedAssignmentTarget::SuperComputed {
            receiver_slot,
            key_slot,
          }));
        }
        self.compile_expr_oast(&member.object)?;
        let object_slot = self.alloc_temp_local("destructuring_target_object");
        self.emit(Op::StoreLocal, LINE);
        self.emit_byte(self.slot_to_u8(object_slot)?, LINE);
        self.compile_expr_oast(&member.expression)?;
        let key_slot = self.alloc_temp_local("destructuring_target_key");
        self.emit(Op::StoreLocal, LINE);
        self.emit_byte(self.slot_to_u8(key_slot)?, LINE);
        Ok(Some(PreparedAssignmentTarget::ComputedMember {
          object_slot,
          key_slot,
        }))
      }
      _ => Ok(None),
    }
  }

  fn prepare_assignment_target_maybe_default(
    &mut self,
    target: &oast::AssignmentTargetMaybeDefault<'_>,
  ) -> Result<Option<PreparedAssignmentTarget>, crate::errors::CompilerError> {
    match target {
      oast::AssignmentTargetMaybeDefault::AssignmentTargetWithDefault(default) => {
        self.prepare_assignment_target(&default.binding)
      }
      oast::AssignmentTargetMaybeDefault::AssignmentTargetIdentifier(id) => Ok(Some(
        PreparedAssignmentTarget::Identifier(id.name.to_string()),
      )),
      oast::AssignmentTargetMaybeDefault::StaticMemberExpression(member) => {
        if matches!(&member.object, oast::Expression::Super(_)) {
          let receiver_slot = self.alloc_temp_local("destructuring_super_receiver");
          self.emit(Op::GetThis, LINE);
          self.emit(Op::StoreLocal, LINE);
          self.emit_byte(self.slot_to_u8(receiver_slot)?, LINE);
          return Ok(Some(PreparedAssignmentTarget::SuperStatic {
            receiver_slot,
            property: Value::String(member.property.name.to_string().into()),
          }));
        }
        self.compile_expr_oast(&member.object)?;
        let object_slot = self.alloc_temp_local("destructuring_target_object");
        self.emit(Op::StoreLocal, LINE);
        self.emit_byte(self.slot_to_u8(object_slot)?, LINE);
        Ok(Some(PreparedAssignmentTarget::StaticMember {
          object_slot,
          property: Value::String(member.property.name.to_string().into()),
        }))
      }
      oast::AssignmentTargetMaybeDefault::ComputedMemberExpression(member) => {
        if matches!(&member.object, oast::Expression::Super(_)) {
          let receiver_slot = self.alloc_temp_local("destructuring_super_receiver");
          self.emit(Op::GetThis, LINE);
          self.emit(Op::StoreLocal, LINE);
          self.emit_byte(self.slot_to_u8(receiver_slot)?, LINE);
          self.compile_expr_oast(&member.expression)?;
          let key_slot = self.alloc_temp_local("destructuring_super_key");
          self.emit(Op::StoreLocal, LINE);
          self.emit_byte(self.slot_to_u8(key_slot)?, LINE);
          return Ok(Some(PreparedAssignmentTarget::SuperComputed {
            receiver_slot,
            key_slot,
          }));
        }
        self.compile_expr_oast(&member.object)?;
        let object_slot = self.alloc_temp_local("destructuring_target_object");
        self.emit(Op::StoreLocal, LINE);
        self.emit_byte(self.slot_to_u8(object_slot)?, LINE);
        self.compile_expr_oast(&member.expression)?;
        let key_slot = self.alloc_temp_local("destructuring_target_key");
        self.emit(Op::StoreLocal, LINE);
        self.emit_byte(self.slot_to_u8(key_slot)?, LINE);
        Ok(Some(PreparedAssignmentTarget::ComputedMember {
          object_slot,
          key_slot,
        }))
      }
      _ => Ok(None),
    }
  }

  fn store_prepared_assignment_target(
    &mut self,
    target: &PreparedAssignmentTarget,
  ) -> Result<(), crate::errors::CompilerError> {
    match target {
      PreparedAssignmentTarget::Identifier(name) => self.store_assignment_identifier(name),
      PreparedAssignmentTarget::StaticMember {
        object_slot,
        property,
      } => {
        let value_slot = self.alloc_temp_local("assign_member_value");
        self.emit(Op::StoreLocal, LINE);
        self.emit_byte(self.slot_to_u8(value_slot)?, LINE);
        self.emit(Op::LoadLocal, LINE);
        self.emit_byte(self.slot_to_u8(*object_slot)?, LINE);
        self.emit_constant(property.clone(), LINE);
        self.emit(Op::LoadLocal, LINE);
        self.emit_byte(self.slot_to_u8(value_slot)?, LINE);
        self.emit(Op::SetProperty, LINE);
        self.emit(Op::Discard, LINE);
        Ok(())
      }
      PreparedAssignmentTarget::ComputedMember {
        object_slot,
        key_slot,
      } => {
        let value_slot = self.alloc_temp_local("assign_member_value");
        self.emit(Op::StoreLocal, LINE);
        self.emit_byte(self.slot_to_u8(value_slot)?, LINE);
        self.emit(Op::LoadLocal, LINE);
        self.emit_byte(self.slot_to_u8(*object_slot)?, LINE);
        self.emit(Op::LoadLocal, LINE);
        self.emit_byte(self.slot_to_u8(*key_slot)?, LINE);
        self.emit(Op::LoadLocal, LINE);
        self.emit_byte(self.slot_to_u8(value_slot)?, LINE);
        self.emit(Op::SetProperty, LINE);
        self.emit(Op::Discard, LINE);
        Ok(())
      }
      PreparedAssignmentTarget::SuperStatic {
        receiver_slot,
        property,
      } => {
        let value_slot = self.alloc_reusable_temp_local("assign_super_value");
        self.emit(Op::StoreLocal, LINE);
        self.emit_byte(self.slot_to_u8(value_slot)?, LINE);
        self.emit(Op::LoadLocal, LINE);
        self.emit_byte(self.slot_to_u8(*receiver_slot)?, LINE);
        self.emit_constant(property.clone(), LINE);
        self.emit(Op::LoadLocal, LINE);
        self.emit_byte(self.slot_to_u8(value_slot)?, LINE);
        self.emit(Op::SetSuperProperty, LINE);
        self.emit(Op::Discard, LINE);
        Ok(())
      }
      PreparedAssignmentTarget::SuperComputed {
        receiver_slot,
        key_slot,
      } => {
        let value_slot = self.alloc_reusable_temp_local("assign_super_value");
        self.emit(Op::StoreLocal, LINE);
        self.emit_byte(self.slot_to_u8(value_slot)?, LINE);
        self.emit(Op::LoadLocal, LINE);
        self.emit_byte(self.slot_to_u8(*receiver_slot)?, LINE);
        self.emit(Op::LoadLocal, LINE);
        self.emit_byte(self.slot_to_u8(*key_slot)?, LINE);
        self.emit(Op::LoadLocal, LINE);
        self.emit_byte(self.slot_to_u8(value_slot)?, LINE);
        self.emit(Op::SetSuperProperty, LINE);
        self.emit(Op::Discard, LINE);
        Ok(())
      }
    }
  }
  fn binding_identifier_name(pattern: &oast::BindingPattern<'_>) -> Option<String> {
    match pattern {
      oast::BindingPattern::BindingIdentifier(id) => Some(id.name.to_string()),
      oast::BindingPattern::AssignmentPattern(pattern) => {
        Self::binding_identifier_name(&pattern.left)
      }
      _ => None,
    }
  }

  pub(super) fn is_anonymous_function_definition(expr: &oast::Expression<'_>) -> bool {
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
    computed: bool,
  ) -> Result<PreparedPropertyKey, crate::errors::CompilerError> {
    if !computed && let Some(value) = Self::binding_property_key_to_value(key) {
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
          self.add_var_local(name.to_string(), None, self.mirrors_root_bindings());
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
    let mirror_global = self.mirrors_root_bindings() && is_var;

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
    self.emit_sync_local_module_export(name, slot)?;
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
      self.emit(Op::Discard, LINE);
      self.emit_get_builtin_name("throwTypeError");
      self.emit(Op::Call, LINE);
      self.emit_byte(0, LINE);
      return Ok(());
    }
    if self.with_depth > 0 || self.may_capture_with {
      self.emit_set_global_name(name);
    } else if let Some(slot) = self.resolve_local(name) {
      let value_slot = self.alloc_reusable_temp_local("assignment_identifier_value");
      self.emit(Op::StoreLocal, LINE);
      self.emit_byte(self.slot_to_u8(value_slot)?, LINE);
      // PutValue cannot initialize a lexical binding. Read the existing cell
      // first so assignment during its TDZ throws instead of silently acting
      // like declaration initialization.
      self.emit(Op::LoadLocal, LINE);
      self.emit_byte(self.slot_to_u8(slot)?, LINE);
      self.emit(Op::Discard, LINE);
      self.emit(Op::LoadLocal, LINE);
      self.emit_byte(self.slot_to_u8(value_slot)?, LINE);
      self.emit(Op::StoreLocal, LINE);
      self.emit_byte(self.slot_to_u8(slot)?, LINE);
      if self.local_mirrors_global(slot) {
        self.emit(Op::LoadLocal, LINE);
        self.emit_byte(self.slot_to_u8(slot)?, LINE);
        self.emit_set_global_name(name);
      }
      self.emit_sync_local_module_export(name, slot)?;
    } else if let Some(upvalue) = self.resolve_upvalue(name) {
      self.emit(Op::SetUpvalue, LINE);
      self.emit_byte(upvalue, LINE);
      self.emit_sync_upvalue_module_export(upvalue)?;
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
  ) -> Result<(usize, usize, usize, usize, usize), crate::errors::CompilerError> {
    self.emit_get_builtin_name("requireObjectCoercible");
    self.emit(Op::Exchange, LINE);
    self.emit(Op::Call, LINE);
    self.emit_byte(1, LINE);

    let iterator_slot = self.alloc_temp_local(label);
    self.emit_get_builtin_name("getIterator");
    self.emit(Op::Exchange, LINE);
    self.emit(Op::Call, LINE);
    self.emit_byte(1, LINE);
    self.emit(Op::StoreLocal, LINE);
    self.emit_byte(self.slot_to_u8(iterator_slot)?, LINE);

    let iterator_active_slot = self.alloc_temp_local("array_destructuring_iterator_active");
    self.emit(Op::True, LINE);
    self.emit(Op::StoreLocal, LINE);
    self.emit_byte(self.slot_to_u8(iterator_active_slot)?, LINE);

    // IteratorDestructuringAssignmentEvaluation closes the iterator when any
    // subsequent step completes abruptly. Keep the original completion while
    // running IteratorClose; an abrupt close must not replace it.
    let thrown_slot = self.alloc_temp_local("array_destructuring_thrown");
    let push_handler_ip = self.current_ip();
    self.emit(Op::PushHandler, LINE);
    let handler_jump = self.current_ip();
    self.emit_byte(0, LINE);
    self.emit_byte(0, LINE);
    self.handler_depth += 1;
    Ok((
      iterator_slot,
      iterator_active_slot,
      thrown_slot,
      push_handler_ip,
      handler_jump,
    ))
  }

  fn set_array_destructuring_iterator_active(
    &mut self,
    active_slot: usize,
    active: bool,
  ) -> Result<(), crate::errors::CompilerError> {
    self.emit(if active { Op::True } else { Op::False }, LINE);
    self.emit(Op::StoreLocal, LINE);
    self.emit_byte(self.slot_to_u8(active_slot)?, LINE);
    Ok(())
  }

  fn emit_array_destructuring_step(
    &mut self,
    iterator_slot: usize,
    active_slot: usize,
  ) -> Result<(), crate::errors::CompilerError> {
    let result_slot = self.alloc_reusable_temp_local("array_destructuring_step");
    self.emit_get_builtin_name("iteratorDestructuringStep");
    self.emit(Op::LoadLocal, LINE);
    self.emit_byte(self.slot_to_u8(iterator_slot)?, LINE);
    self.emit(Op::Call, LINE);
    self.emit_byte(1, LINE);
    self.emit(Op::StoreLocal, LINE);
    self.emit_byte(self.slot_to_u8(result_slot)?, LINE);
    self.emit(Op::LoadLocal, LINE);
    self.emit_byte(self.slot_to_u8(result_slot)?, LINE);
    self.emit_constant(Value::String("1".into()), LINE);
    self.emit(Op::GetProperty, LINE);
    self.emit(Op::StoreLocal, LINE);
    self.emit_byte(self.slot_to_u8(active_slot)?, LINE);
    self.emit(Op::LoadLocal, LINE);
    self.emit_byte(self.slot_to_u8(result_slot)?, LINE);
    self.emit_constant(Value::String("0".into()), LINE);
    self.emit(Op::GetProperty, LINE);
    Ok(())
  }

  fn close_array_destructuring_iterator_if_active(
    &mut self,
    iterator_slot: usize,
    active_slot: usize,
  ) -> Result<(), crate::errors::CompilerError> {
    self.emit(Op::LoadLocal, LINE);
    self.emit_byte(self.slot_to_u8(active_slot)?, LINE);
    self.emit(Op::JumpIfFalse, LINE);
    let skip_close = self.current_ip();
    self.emit_byte(0, LINE);
    self.emit_byte(0, LINE);
    self.emit(Op::LoadLocal, LINE);
    self.emit_byte(self.slot_to_u8(iterator_slot)?, LINE);
    self.emit(Op::IteratorClose, LINE);
    self.emit(Op::Discard, LINE);
    self.patch_jump(skip_close);
    Ok(())
  }

  fn finish_array_destructuring_iterator(
    &mut self,
    iterator_slot: usize,
    iterator_active_slot: usize,
    thrown_slot: usize,
    push_handler_ip: usize,
    handler_jump: usize,
    close_on_normal: bool,
  ) -> Result<(), crate::errors::CompilerError> {
    self.handler_depth = self.handler_depth.saturating_sub(1);
    self.emit(Op::PopHandler, LINE);
    // A rest element exhausts the iterator through iteratorToArray; the
    // existing rest path deliberately has no separate normal close.
    if close_on_normal {
      self.close_array_destructuring_iterator_if_active(iterator_slot, iterator_active_slot)?;
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
    self.close_array_destructuring_iterator_if_active(iterator_slot, iterator_active_slot)?;
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

    // A generator can suspend while evaluating a default initializer. A
    // later generator.return() is an abrupt return completion, not a thrown
    // exception, so register a return-only cleanup for the same protected
    // region. The VM enters this block through its existing finally metadata.
    let return_slot = self.alloc_temp_local("array_destructuring_return");
    let did_return_slot = self.alloc_temp_local("array_destructuring_did_return");
    let return_cleanup_ip = self.current_ip();
    self.close_array_destructuring_iterator_if_active(iterator_slot, iterator_active_slot)?;
    self.emit(Op::LoadLocal, LINE);
    self.emit_byte(self.slot_to_u8(return_slot)?, LINE);
    self.emit(Op::Return, LINE);
    self
      .chunk
      .finally_handlers
      .push(crate::bytecode::FinallyHandler {
        push_ip: push_handler_ip,
        finalizer_ip: return_cleanup_ip,
        return_slot,
        did_return_slot,
      });
    self.patch_jump(end_jump);
    Ok(())
  }

  fn compile_assignment_target_maybe_default_prepared(
    &mut self,
    target: &oast::AssignmentTargetMaybeDefault<'_>,
    prepared: Option<&PreparedAssignmentTarget>,
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
        if let Some(prepared) = prepared {
          self.store_prepared_assignment_target(prepared)?;
        } else {
          self.compile_assignment_target_oast(&a.binding)?;
        }
        Ok(())
      }
      oast::AssignmentTargetMaybeDefault::AssignmentTargetIdentifier(id) => {
        if let Some(prepared) = prepared {
          self.store_prepared_assignment_target(prepared)
        } else {
          self.compile_assignment_identifier_reference(id)
        }
      }
      oast::AssignmentTargetMaybeDefault::StaticMemberExpression(m) => {
        if let Some(prepared) = prepared {
          self.store_prepared_assignment_target(prepared)
        } else {
          self.compile_static_member_assignment_target(m)
        }
      }
      oast::AssignmentTargetMaybeDefault::ComputedMemberExpression(m) => {
        if let Some(prepared) = prepared {
          self.store_prepared_assignment_target(prepared)
        } else {
          self.compile_computed_member_assignment_target(m)
        }
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
    let (iterator_slot, iterator_active_slot, thrown_slot, push_handler_ip, handler_jump) =
      self.begin_array_destructuring_iterator("array_assign_iterator")?;

    for el in &a.elements {
      let prepared = el
        .as_ref()
        .map(|target| self.prepare_assignment_target_maybe_default(target))
        .transpose()?
        .flatten();
      self.emit_array_destructuring_step(iterator_slot, iterator_active_slot)?;
      if let Some(el) = el {
        self.compile_assignment_target_maybe_default_prepared(el, prepared.as_ref())?;
      } else {
        self.emit(Op::Discard, LINE);
      }
    }
    if let Some(rest) = &a.rest {
      let prepared = self.prepare_assignment_target(&rest.target)?;
      self.set_array_destructuring_iterator_active(iterator_active_slot, false)?;
      self.emit(Op::LoadLocal, LINE);
      self.emit_byte(self.slot_to_u8(iterator_slot)?, LINE);
      self.emit_get_builtin_name("iteratorToArray");
      self.emit(Op::Exchange, LINE);
      self.emit(Op::Call, LINE);
      self.emit_byte(1, LINE);
      if let Some(prepared) = &prepared {
        self.store_prepared_assignment_target(prepared)?;
      } else {
        self.compile_assignment_target_oast(&rest.target)?;
      }
    }
    self.finish_array_destructuring_iterator(
      iterator_slot,
      iterator_active_slot,
      thrown_slot,
      push_handler_ip,
      handler_jump,
      a.rest.is_none(),
    )?;
    Ok(())
  }

  pub(super) fn compile_object_assignment_target(
    &mut self,
    o: &oast::ObjectAssignmentTarget<'_>,
  ) -> Result<(), crate::errors::CompilerError> {
    self.emit_get_builtin_name("requireObjectCoercible");
    self.emit(Op::Exchange, LINE);
    self.emit(Op::Call, LINE);
    self.emit_byte(1, LINE);
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
          let key = self.compile_destructuring_property_key(&p.name, p.computed)?;
          let prepared = self.prepare_assignment_target_maybe_default(&p.binding)?;
          self.emit(Op::GetProperty, LINE);
          self.compile_assignment_target_maybe_default_prepared(&p.binding, prepared.as_ref())?;
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
    self.compile_binding_pattern_assignment_prepared(pattern, kind, None)
  }

  fn compile_binding_pattern_assignment_prepared(
    &mut self,
    pattern: &oast::BindingPattern<'_>,
    kind: oxc_ast::ast::VariableDeclarationKind,
    resolved_identifier: Option<(usize, String)>,
  ) -> Result<(), crate::errors::CompilerError> {
    match pattern {
      oast::BindingPattern::BindingIdentifier(i) => {
        if let Some((reference_slot, name)) = resolved_identifier {
          self.emit_set_resolved_identifier(reference_slot, &name)?;
          self.emit(Op::Discard, LINE);
        } else {
          self.store_binding_identifier(i.name.as_str(), kind)?;
        }
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
        self.compile_binding_pattern_assignment_prepared(&a.left, kind, resolved_identifier)?;
      }
      oast::BindingPattern::ArrayPattern(a) => {
        let (iterator_slot, iterator_active_slot, thrown_slot, push_handler_ip, handler_jump) =
          self.begin_array_destructuring_iterator("array_binding_iterator")?;

        for el in &a.elements {
          self.emit_array_destructuring_step(iterator_slot, iterator_active_slot)?;

          if let Some(el) = el {
            self.compile_binding_pattern_assignment_oast(el, kind)?;
          } else {
            self.emit(Op::Discard, LINE);
          }
        }

        if let Some(rest) = &a.rest {
          self.set_array_destructuring_iterator_active(iterator_active_slot, false)?;
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
          iterator_active_slot,
          thrown_slot,
          push_handler_ip,
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
          let key = self.compile_destructuring_property_key(&prop.key, prop.computed)?;
          // KeyedBindingInitialization resolves a var binding before reading
          // the source property. Preserve that reference so object
          // environment HasBinding/unscopables hooks run exactly once and in
          // the specified order.
          let resolved_identifier = if matches!(kind, oast::VariableDeclarationKind::Var)
            && (self.with_depth > 0 || self.may_capture_with)
          {
            Self::binding_identifier_name(&prop.value)
              .map(|name| {
                self
                  .emit_resolve_with_identifier(&name)
                  .map(|slot| (slot, name))
              })
              .transpose()?
          } else {
            None
          };
          self.emit(Op::GetProperty, LINE);
          self.compile_binding_pattern_assignment_prepared(
            &prop.value,
            kind,
            resolved_identifier,
          )?;
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
