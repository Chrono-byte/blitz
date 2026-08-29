// SPDX-License-Identifier: MPL-2.0

use super::*;
use oxc_syntax::number::NumberBase;

impl Compiler {
  pub(super) fn validate_literal_early_errors(
    &self,
    expr: &oast::Expression,
  ) -> Result<(), crate::errors::CompilerError> {
    match expr {
      oast::Expression::NumericLiteral(n) if self.strict_context => {
        let raw = n.raw.as_ref().map_or_else(
          || n.span.source_text(self.source_text.as_str()),
          |raw| raw.as_str(),
        );
        let leading_zero = {
          let mut bytes = raw.bytes();
          matches!(
            (bytes.next(), bytes.next()),
            (Some(b'0'), Some(b'0'..=b'9'))
          )
        };
        if matches!(n.base, NumberBase::Octal) && leading_zero {
          return Err(crate::errors::CompilerError::from(
            "SyntaxError: legacy octal numeric literal in strict mode",
          ));
        }
        if matches!(n.base, NumberBase::Decimal | NumberBase::Float) && leading_zero {
          return Err(crate::errors::CompilerError::from(
            "SyntaxError: leading zero numeric literal in strict mode",
          ));
        }
      }
      oast::Expression::StringLiteral(s) if self.strict_context => {
        let raw = s.span.source_text(self.source_text.as_str());
        let bytes = raw.as_bytes();
        for idx in 0..bytes.len().saturating_sub(1) {
          if bytes[idx] == b'\\' && matches!(bytes[idx + 1], b'0'..=b'7') {
            return Err(crate::errors::CompilerError::from(
              "SyntaxError: legacy octal escape sequence in strict mode",
            ));
          }
        }
      }
      _ => {}
    }
    Ok(())
  }

  pub(super) fn alloc_temp_local(
    &mut self,
    prefix: &str,
  ) -> usize {
    let function_scope = self.current_function_scope_index();
    self.add_local_in_scope(
      function_scope,
      format!("\0{}_{}", prefix, self.locals.len()),
      None,
      false,
    )
  }

  pub(super) fn alloc_reusable_temp_local(
    &mut self,
    name: &str,
  ) -> usize {
    let name = format!("\0reusable_{name}");
    self
      .locals
      .iter()
      .position(|local| local == &name)
      .unwrap_or_else(|| self.add_local_with_mirror(name, None, false))
  }

  pub(super) fn emit_super_property_get_static(
    &mut self,
    property: &str,
    preserve_receiver: bool,
  ) -> Result<(), crate::errors::CompilerError> {
    if !self.allow_super {
      return Err(crate::errors::CompilerError::from(
        "SyntaxError: super used outside of a method",
      ));
    }
    self.emit(LoweringOpcode::GetThis, LINE);
    self.emit(LoweringOpcode::Copy, LINE);
    self.emit_constant(Value::String(property.to_string().into()), LINE);
    self.emit(LoweringOpcode::GetSuperProperty, LINE);
    if !preserve_receiver {
      self.emit(LoweringOpcode::Exchange, LINE);
      self.emit(LoweringOpcode::Discard, LINE);
    }
    Ok(())
  }

  pub(super) fn emit_super_property_get_computed(
    &mut self,
    key_expr: &oast::Expression,
    preserve_receiver: bool,
  ) -> Result<(), crate::errors::CompilerError> {
    if !self.allow_super {
      return Err(crate::errors::CompilerError::from(
        "SyntaxError: super used outside of a method",
      ));
    }
    self.emit(LoweringOpcode::GetThis, LINE);
    self.emit(LoweringOpcode::Copy, LINE);
    self.compile_expr_oast(key_expr)?;
    self.emit(LoweringOpcode::GetSuperProperty, LINE);
    if !preserve_receiver {
      self.emit(LoweringOpcode::Exchange, LINE);
      self.emit(LoweringOpcode::Discard, LINE);
    }
    Ok(())
  }

  pub(super) fn emit_super_property_set_static(
    &mut self,
    property: &str,
    value_expr: &oast::Expression,
  ) -> Result<(), crate::errors::CompilerError> {
    if !self.allow_super {
      return Err(crate::errors::CompilerError::from(
        "SyntaxError: super used outside of a method",
      ));
    }
    self.emit(LoweringOpcode::GetThis, LINE);
    self.emit(LoweringOpcode::Copy, LINE);
    self.emit_constant(Value::String(property.to_string().into()), LINE);
    self.compile_expr_oast(value_expr)?;
    self.emit(LoweringOpcode::SetSuperProperty, LINE);
    self.emit(LoweringOpcode::Exchange, LINE);
    self.emit(LoweringOpcode::Discard, LINE);
    Ok(())
  }

  pub(super) fn emit_super_property_set_computed(
    &mut self,
    key_expr: &oast::Expression,
    value_expr: &oast::Expression,
  ) -> Result<(), crate::errors::CompilerError> {
    if !self.allow_super {
      return Err(crate::errors::CompilerError::from(
        "SyntaxError: super used outside of a method",
      ));
    }
    self.emit(LoweringOpcode::GetThis, LINE);
    self.emit(LoweringOpcode::Copy, LINE);
    self.compile_expr_oast(key_expr)?;
    self.compile_expr_oast(value_expr)?;
    self.emit(LoweringOpcode::SetSuperProperty, LINE);
    self.emit(LoweringOpcode::Exchange, LINE);
    self.emit(LoweringOpcode::Discard, LINE);
    Ok(())
  }

  pub(super) fn compile_static_member_reference(
    &mut self,
    m: &oast::StaticMemberExpression<'_>,
  ) -> Result<(usize, usize), crate::errors::CompilerError> {
    self.compile_expr_oast(&m.object)?;
    let object_slot = self.alloc_temp_local("ref_obj");
    self.emit(LoweringOpcode::StoreLocal, LINE);
    self.emit_slot(object_slot, LINE);

    self.emit_constant(Value::String(m.property.name.to_string().into()), LINE);
    let key_slot = self.alloc_temp_local("ref_key");
    self.emit(LoweringOpcode::StoreLocal, LINE);
    self.emit_slot(key_slot, LINE);
    Ok((object_slot, key_slot))
  }

  pub(super) fn compile_computed_member_reference(
    &mut self,
    m: &oast::ComputedMemberExpression<'_>,
  ) -> Result<(usize, usize), crate::errors::CompilerError> {
    self.compile_expr_oast(&m.object)?;
    let object_slot = self.alloc_temp_local("ref_obj");
    self.emit(LoweringOpcode::StoreLocal, LINE);
    self.emit_slot(object_slot, LINE);

    self.compile_expr_oast(&m.expression)?;
    let key_slot = self.alloc_temp_local("ref_key");
    self.emit(LoweringOpcode::StoreLocal, LINE);
    self.emit_slot(key_slot, LINE);
    Ok((object_slot, key_slot))
  }

  pub(super) fn emit_reference_get(
    &mut self,
    object_slot: usize,
    key_slot: usize,
  ) -> Result<(), crate::errors::CompilerError> {
    self.emit(LoweringOpcode::LoadLocal, LINE);
    self.emit_slot(object_slot, LINE);
    self.emit(LoweringOpcode::LoadLocal, LINE);
    self.emit_slot(key_slot, LINE);
    self.emit(LoweringOpcode::GetProperty, LINE);
    Ok(())
  }

  pub(super) fn emit_reference_set_current_value(
    &mut self,
    object_slot: usize,
    key_slot: usize,
  ) -> Result<(), crate::errors::CompilerError> {
    let value_slot = self.alloc_temp_local("ref_value");
    self.emit(LoweringOpcode::StoreLocal, LINE);
    self.emit_slot(value_slot, LINE);
    self.emit(LoweringOpcode::LoadLocal, LINE);
    self.emit_slot(object_slot, LINE);
    self.emit(LoweringOpcode::LoadLocal, LINE);
    self.emit_slot(key_slot, LINE);
    self.emit(LoweringOpcode::LoadLocal, LINE);
    self.emit_slot(value_slot, LINE);
    self.emit(LoweringOpcode::SetProperty, LINE);
    Ok(())
  }

  pub(super) fn try_emit_compare_jump_if_false(
    &mut self,
    expr: &oast::Expression,
  ) -> Result<Option<usize>, crate::errors::CompilerError> {
    use oxc_ast::ast::Expression as OExpr;
    let OExpr::BinaryExpression(binary) = expr else {
      return Ok(None);
    };

    let cond_type = match binary.operator {
      oast::BinaryOperator::Equality => 0_i32,
      oast::BinaryOperator::Inequality => 1_i32,
      oast::BinaryOperator::LessThan => 2_i32,
      oast::BinaryOperator::LessEqualThan => 3_i32,
      oast::BinaryOperator::GreaterThan => 4_i32,
      oast::BinaryOperator::GreaterEqualThan => 5_i32,
      oast::BinaryOperator::StrictEquality => 6_i32,
      oast::BinaryOperator::StrictInequality => 7_i32,
      _ => return Ok(None),
    };

    self.compile_expr_oast(&binary.left)?;
    self.compile_expr_oast(&binary.right)?;
    self.emit_with_payload(LoweringOpcode::CompareJump, cond_type as u32, LINE);

    let patch_addr = self.current_pc().saturating_sub(1);

    Ok(Some(patch_addr))
  }

  pub(super) fn emit_compound_assignment_op(
    &mut self,
    op: oast::AssignmentOperator,
  ) {
    match op {
      oast::AssignmentOperator::Addition => self.emit(LoweringOpcode::Add, LINE),
      oast::AssignmentOperator::Subtraction => self.emit(LoweringOpcode::Sub, LINE),
      oast::AssignmentOperator::Multiplication => self.emit(LoweringOpcode::Mul, LINE),
      oast::AssignmentOperator::Division => self.emit(LoweringOpcode::Div, LINE),
      oast::AssignmentOperator::Remainder => self.emit(LoweringOpcode::Mod, LINE),
      oast::AssignmentOperator::Exponential => self.emit(LoweringOpcode::Pow, LINE),
      oast::AssignmentOperator::BitwiseAnd => self.emit(LoweringOpcode::BitAnd, LINE),
      oast::AssignmentOperator::BitwiseOR => self.emit(LoweringOpcode::BitOr, LINE),
      oast::AssignmentOperator::BitwiseXOR => self.emit(LoweringOpcode::BitXor, LINE),
      oast::AssignmentOperator::ShiftLeft => self.emit(LoweringOpcode::Shl, LINE),
      oast::AssignmentOperator::ShiftRight => self.emit(LoweringOpcode::Shr, LINE),
      oast::AssignmentOperator::ShiftRightZeroFill => self.emit(LoweringOpcode::Ushr, LINE),
      _ => {}
    }
  }
}
