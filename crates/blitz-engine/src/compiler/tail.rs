// SPDX-License-Identifier: MPL-2.0
use super::emit::FrontendOp as Op;
use crate::value::Value;
use oxc_ast::ast as oast;

const LINE: u32 = 1;
const JUMP_PLACEHOLDER_BYTE: u8 = 0;
const MAX_CALL_ARGUMENTS: usize = u8::MAX as usize;

impl super::Compiler {
  pub(super) fn identifier_name_if_parenthesized<'a>(
    expr: &'a oast::Expression<'a>
  ) -> Option<&'a str> {
    match expr {
      oast::Expression::Identifier(id) => Some(id.name.as_str()),
      oast::Expression::ParenthesizedExpression(p) => {
        Self::identifier_name_if_parenthesized(&p.expression)
      }
      _ => None,
    }
  }

  pub(super) fn emit_jump_placeholder(&mut self) {
    self.emit_byte(JUMP_PLACEHOLDER_BYTE, LINE);
    self.emit_byte(JUMP_PLACEHOLDER_BYTE, LINE);
  }

  pub(super) fn checked_arg_count(
    &self,
    arg_count: usize,
  ) -> Result<u8, crate::errors::CompilerError> {
    u8::try_from(arg_count)
      .map_err(|_| crate::errors::CompilerError::from("too many arguments in call"))
  }

  pub(super) fn compile_call_arguments(
    &mut self,
    arguments: &oxc_allocator::Vec<'_, oast::Argument<'_>>,
  ) -> Result<usize, crate::errors::CompilerError> {
    if arguments.len() > MAX_CALL_ARGUMENTS {
      return Err(crate::errors::CompilerError::from(
        "too many arguments in call",
      ));
    }

    for arg in arguments {
      match arg {
        oast::Argument::SpreadElement(s) => self.compile_expr_oast(&s.argument)?,
        _ => match arg.as_expression() {
          Some(expr) => self.compile_expr_oast(expr)?,
          None => self.emit(Op::Undefined, LINE),
        },
      }
    }

    Ok(arguments.len())
  }

  pub(super) fn emit_call_with_arg_count(
    &mut self,
    op: Op,
    arg_count: usize,
  ) -> Result<(), crate::errors::CompilerError> {
    self.emit(op, LINE);
    self.emit_byte(self.checked_arg_count(arg_count)?, LINE);
    Ok(())
  }

  pub(super) fn compile_optional_member_access<F>(
    &mut self,
    object: &oast::Expression<'_>,
    emit_access: F,
  ) -> Result<(), crate::errors::CompilerError>
  where
    F: FnOnce(&mut Self) -> Result<(), crate::errors::CompilerError>,
  {
    self.compile_expr_oast(object)?;
    self.emit(Op::Copy, LINE);
    self.emit(Op::JumpIfNullish, LINE);
    let skip_nullish = self.current_ip();
    self.emit_jump_placeholder();
    emit_access(self)?;
    self.emit(Op::Jump, LINE);
    let after_access = self.current_ip();
    self.emit_jump_placeholder();
    self.patch_jump(skip_nullish);
    self.emit(Op::Discard, LINE);
    self.emit(Op::Undefined, LINE);
    self.patch_jump(after_access);
    Ok(())
  }

  pub(super) fn compile_spread_call_tail(
    &mut self,
    c: &oast::CallExpression<'_>,
  ) -> Result<bool, crate::errors::CompilerError> {
    if !c
      .arguments
      .iter()
      .any(|arg| matches!(arg, oast::Argument::SpreadElement(_)))
    {
      return Ok(false);
    }

    let arg_count = self.checked_arg_count(c.arguments.len())?;
    self.compile_expr_oast(&c.callee)?;

    let mut spread_flags = Vec::with_capacity(c.arguments.len());
    for arg in &c.arguments {
      match arg {
        oast::Argument::SpreadElement(s) => {
          self.compile_expr_oast(&s.argument)?;
          spread_flags.push(true);
        }
        _ => match arg.as_expression() {
          Some(expr) => {
            self.compile_expr_oast(expr)?;
            spread_flags.push(false);
          }
          None => {
            self.emit(Op::Undefined, LINE);
            spread_flags.push(false);
          }
        },
      }
    }

    self.emit(Op::BuildArray, LINE);
    self.emit_byte(arg_count, LINE);
    for is_spread in spread_flags {
      self.emit(if is_spread { Op::True } else { Op::False }, LINE);
    }
    self.emit(Op::BuildArray, LINE);
    self.emit_byte(arg_count, LINE);
    self.emit(Op::Spread, LINE);
    self.emit_byte(arg_count, LINE);
    Ok(true)
  }

  fn compile_call_expression_tail(
    &mut self,
    c: &oast::CallExpression<'_>,
  ) -> Result<(), crate::errors::CompilerError> {
    use oxc_ast::ast::Expression as OExpr;
    // Tail calls only need the optimized path for direct/non-spread calls.
    // Spread calls still lower through the existing non-tail path.
    if self.compile_spread_call_tail(c)? {
      return Ok(());
    }

    if let OExpr::Super(_) = &c.callee {
      if !self.allow_super {
        return Err(crate::errors::CompilerError::from(
          "SyntaxError: super used outside of a method",
        ));
      }
      let arg_count = self.compile_call_arguments(&c.arguments)?;
      self.emit_call_with_arg_count(Op::SuperConstruct, arg_count)?;
      self.emit_pending_instance_field_initializers()?;
      return Ok(());
    }

    if let OExpr::StaticMemberExpression(m) = &c.callee
      && matches!(&m.object, OExpr::Super(_))
    {
      if c.optional || m.optional {
        return Err(crate::errors::CompilerError::from(
          "optional chaining with super is not supported",
        ));
      }
      self.emit_super_property_get_static(m.property.name.as_str(), true)?;
      let arg_count = self.compile_call_arguments(&c.arguments)?;
      self.emit_call_with_arg_count(Op::CallBuiltin, arg_count)?;
      return Ok(());
    }

    if let OExpr::ComputedMemberExpression(m) = &c.callee
      && matches!(&m.object, OExpr::Super(_))
    {
      if c.optional || m.optional {
        return Err(crate::errors::CompilerError::from(
          "optional chaining with super is not supported",
        ));
      }
      self.emit_super_property_get_computed(&m.expression, true)?;
      let arg_count = self.compile_call_arguments(&c.arguments)?;
      self.emit_call_with_arg_count(Op::CallBuiltin, arg_count)?;
      return Ok(());
    }

    if let oast::Expression::StaticMemberExpression(m) = &c.callee {
      let _is_optional_call = c.optional;

      if m.optional {
        self.compile_optional_member_access(&m.object, |this| {
          this.emit(Op::Copy, LINE);
          this.emit_constant(Value::String(m.property.name.to_string().into()), LINE);
          this.emit(Op::GetProperty, LINE);
          let slot = this.alloc_ic_slot()?;
          this.emit_byte(slot, LINE);
          Ok(())
        })?;
      } else {
        self.compile_expr_oast(&m.object)?;
        self.emit(Op::Copy, LINE);
        self.emit_constant(Value::String(m.property.name.to_string().into()), LINE);
        self.emit(Op::GetProperty, LINE);
        let slot = self.alloc_ic_slot()?;
        self.emit_byte(slot, LINE);
      }

      let arg_count = self.compile_call_arguments(&c.arguments)?;
      self.emit_call_with_arg_count(Op::CallBuiltin, arg_count)?;
      return Ok(());
    }

    if let oast::Expression::ComputedMemberExpression(m) = &c.callee {
      let _is_optional_call = c.optional;

      if m.optional {
        self.compile_optional_member_access(&m.object, |this| {
          this.emit(Op::Copy, LINE);
          this.compile_expr_oast(&m.expression)?;
          this.emit(Op::GetProperty, LINE);
          Ok(())
        })?;
      } else {
        self.compile_expr_oast(&m.object)?;
        self.emit(Op::Copy, LINE);
        self.compile_expr_oast(&m.expression)?;
        self.emit(Op::GetProperty, LINE);
      }

      let arg_count = self.compile_call_arguments(&c.arguments)?;
      self.emit_call_with_arg_count(Op::CallBuiltin, arg_count)?;
      return Ok(());
    }

    if let oast::Expression::PrivateFieldExpression(m) = &c.callee {
      let _is_optional_call = c.optional;

      if m.optional {
        self.compile_optional_member_access(&m.object, |this| {
          this.emit(Op::Copy, LINE);
          let key = this.ensure_private_name_constant(&m.field.name);
          this.emit_constant(key, LINE);
          this.emit(Op::GetProperty, LINE);
          Ok(())
        })?;
      } else {
        self.compile_expr_oast(&m.object)?;
        self.emit(Op::Copy, LINE);
        let key = self.ensure_private_name_constant(&m.field.name);
        self.emit_constant(key, LINE);
        self.emit(Op::GetProperty, LINE);
      }

      let arg_count = self.compile_call_arguments(&c.arguments)?;
      self.emit_call_with_arg_count(Op::CallBuiltin, arg_count)?;
      return Ok(());
    }

    if c.optional {
      // Optional calls short-circuit to `undefined` if the callee is nullish.
      self.compile_expr_oast(&c.callee)?;
      self.emit(Op::Copy, LINE);
      self.emit(Op::JumpIfNullish, LINE);
      let skip_nullish = self.current_ip();
      self.emit_jump_placeholder();
      self.emit(Op::Discard, LINE);
      self.emit(Op::Null, LINE);
      self.emit(Op::Jump, LINE);
      let end = self.current_ip();
      self.emit_jump_placeholder();
      self.patch_jump(skip_nullish);
      let arg_count = self.compile_call_arguments(&c.arguments)?;
      self.emit_call_with_arg_count(Op::Call, arg_count)?;
      self.patch_jump(end);
      return Ok(());
    }

    self.compile_expr_oast(&c.callee)?;
    let arg_count = self.compile_call_arguments(&c.arguments)?;
    let op = if self.strict_context {
      Op::TailCall
    } else {
      Op::Call
    };
    self.emit_call_with_arg_count(op, arg_count)
  }

  fn compile_tagged_template_tail(
    &mut self,
    t: &oast::TaggedTemplateExpression<'_>,
  ) -> Result<(), crate::errors::CompilerError> {
    self.compile_expr_oast(&t.tag)?;
    for quasi in &t.quasi.quasis {
      let cooked = quasi
        .value
        .cooked
        .as_ref()
        .map(ToString::to_string)
        .unwrap_or_default();
      self.emit_constant(Value::String(cooked.into()), LINE);
    }
    self.emit(Op::BuildArray, LINE);
    self.emit_byte(self.checked_arg_count(t.quasi.quasis.len())?, LINE);
    self.emit(Op::Copy, LINE);
    self.emit_constant(Value::String("raw".to_string().into()), LINE);
    for quasi in &t.quasi.quasis {
      self.emit_constant(Value::String(quasi.value.raw.to_string().into()), LINE);
    }
    self.emit(Op::BuildArray, LINE);
    self.emit_byte(self.checked_arg_count(t.quasi.quasis.len())?, LINE);
    self.emit(Op::SetProperty, LINE);
    let slot = self.alloc_ic_slot()?;
    self.emit_byte(slot, LINE);
    self.emit(Op::Discard, LINE);
    let mut arg_count = 1usize;
    for sub in &t.quasi.expressions {
      self.compile_expr_oast(sub)?;
      arg_count += 1;
    }
    self.emit_call_with_arg_count(Op::Call, arg_count)
  }

  pub(super) fn compile_expr_oast_tail(
    &mut self,
    expr: &oast::Expression,
  ) -> Result<(), crate::errors::CompilerError> {
    use oxc_ast::ast::Expression as OExpr;
    match expr {
      OExpr::ParenthesizedExpression(p) => self.compile_expr_oast_tail(&p.expression),
      OExpr::TSAsExpression(a) => self.compile_expr_oast_tail(&a.expression),
      OExpr::TSNonNullExpression(n) => self.compile_expr_oast_tail(&n.expression),
      OExpr::SequenceExpression(seq) => {
        let len = seq.expressions.len();
        for (idx, expr) in seq.expressions.iter().enumerate() {
          if idx + 1 == len {
            self.compile_expr_oast_tail(expr)?;
          } else {
            self.compile_expr_oast(expr)?;
            self.emit(Op::Discard, LINE);
          }
        }
        if len == 0 {
          self.emit(Op::Undefined, LINE);
        }
        Ok(())
      }
      OExpr::LogicalExpression(l) => match l.operator {
        oast::LogicalOperator::And => {
          self.compile_expr_oast(&l.left)?;
          self.emit(Op::Copy, LINE);
          self.emit(Op::JumpIfFalse, LINE);
          let jump_end = self.current_ip();
          self.emit_jump_placeholder();
          self.emit(Op::Discard, LINE);
          self.compile_expr_oast_tail(&l.right)?;
          self.patch_jump(jump_end);
          Ok(())
        }
        oast::LogicalOperator::Or => {
          self.compile_expr_oast(&l.left)?;
          self.emit(Op::Copy, LINE);
          self.emit(Op::JumpIfTrue, LINE);
          let jump_end = self.current_ip();
          self.emit_jump_placeholder();
          self.emit(Op::Discard, LINE);
          self.compile_expr_oast_tail(&l.right)?;
          self.patch_jump(jump_end);
          Ok(())
        }
        oast::LogicalOperator::Coalesce => {
          self.compile_expr_oast(&l.left)?;
          self.emit(Op::Copy, LINE);
          self.emit(Op::JumpIfNullish, LINE);
          let jump_right = self.current_ip();
          self.emit_jump_placeholder();
          self.emit(Op::Jump, LINE);
          let jump_end = self.current_ip();
          self.emit_jump_placeholder();
          self.patch_jump(jump_right);
          self.emit(Op::Discard, LINE);
          self.compile_expr_oast_tail(&l.right)?;
          self.patch_jump(jump_end);
          Ok(())
        }
      },
      OExpr::ConditionalExpression(c) => {
        if let Some(jump_else) = self.try_emit_compare_jump_if_false(&c.test)? {
          self.compile_expr_oast_tail(&c.consequent)?;
          self.emit(Op::Jump, LINE);
          let jump_end = self.current_ip();
          self.emit_jump_placeholder();
          self.patch_jump(jump_else);
          self.compile_expr_oast_tail(&c.alternate)?;
          self.patch_jump(jump_end);
        } else {
          self.compile_expr_oast(&c.test)?;
          self.emit(Op::JumpIfFalse, LINE);
          let jump_else = self.current_ip();
          self.emit_jump_placeholder();
          self.compile_expr_oast_tail(&c.consequent)?;
          self.emit(Op::Jump, LINE);
          let jump_end = self.current_ip();
          self.emit_jump_placeholder();
          self.patch_jump(jump_else);
          self.compile_expr_oast_tail(&c.alternate)?;
          self.patch_jump(jump_end);
        }
        Ok(())
      }
      OExpr::TaggedTemplateExpression(t) => self.compile_tagged_template_tail(t),
      OExpr::CallExpression(c) => self.compile_call_expression_tail(c),
      _ => self.compile_expr_oast(expr),
    }
  }
}
