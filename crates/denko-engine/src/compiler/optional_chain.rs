// SPDX-License-Identifier: MPL-2.0

use super::*;

impl Compiler {
  pub(super) fn compile_optional_chain_member_static(
    &mut self,
    member: &oast::StaticMemberExpression<'_>,
    continuation: &mut dyn FnMut(&mut Self) -> Result<(), crate::errors::CompilerError>,
  ) -> Result<(), crate::errors::CompilerError> {
    if matches!(&member.object, oast::Expression::Super(_)) {
      if member.optional {
        return Err(crate::errors::CompilerError::from(
          "optional chaining with super is not supported",
        ));
      }
      self.emit_super_property_get_static(member.property.name.as_str(), false)?;
      return continuation(self);
    }

    self.compile_optional_chain_expression(&member.object, &mut |this| {
      if member.optional {
        this.emit(Op::Copy, LINE);
        this.emit(Op::JumpIfNullish, LINE);
        let nullish = this.current_ip();
        this.emit_jump_placeholder();

        this.emit_constant(Value::String(member.property.name.to_string().into()), LINE);
        this.emit(Op::GetProperty, LINE);
        continuation(this)?;

        this.emit(Op::Jump, LINE);
        let end = this.current_ip();
        this.emit_jump_placeholder();
        this.patch_jump(nullish);
        this.emit(Op::Discard, LINE);
        this.emit(Op::Undefined, LINE);
        this.patch_jump(end);
      } else {
        this.emit_constant(Value::String(member.property.name.to_string().into()), LINE);
        this.emit(Op::GetProperty, LINE);
        continuation(this)?;
      }
      Ok(())
    })
  }

  pub(super) fn compile_optional_chain_member_computed(
    &mut self,
    member: &oast::ComputedMemberExpression<'_>,
    continuation: &mut dyn FnMut(&mut Self) -> Result<(), crate::errors::CompilerError>,
  ) -> Result<(), crate::errors::CompilerError> {
    if matches!(&member.object, oast::Expression::Super(_)) {
      if member.optional {
        return Err(crate::errors::CompilerError::from(
          "optional chaining with super is not supported",
        ));
      }
      self.emit_super_property_get_computed(&member.expression, false)?;
      return continuation(self);
    }

    self.compile_optional_chain_expression(&member.object, &mut |this| {
      if member.optional {
        this.emit(Op::Copy, LINE);
        this.emit(Op::JumpIfNullish, LINE);
        let nullish = this.current_ip();
        this.emit_jump_placeholder();

        this.compile_expr_oast(&member.expression)?;
        this.emit(Op::GetProperty, LINE);
        continuation(this)?;

        this.emit(Op::Jump, LINE);
        let end = this.current_ip();
        this.emit_jump_placeholder();
        this.patch_jump(nullish);
        this.emit(Op::Discard, LINE);
        this.emit(Op::Undefined, LINE);
        this.patch_jump(end);
      } else {
        this.compile_expr_oast(&member.expression)?;
        this.emit(Op::GetProperty, LINE);
        continuation(this)?;
      }
      Ok(())
    })
  }

  pub(super) fn compile_optional_chain_member_private(
    &mut self,
    member: &oast::PrivateFieldExpression<'_>,
    continuation: &mut dyn FnMut(&mut Self) -> Result<(), crate::errors::CompilerError>,
  ) -> Result<(), crate::errors::CompilerError> {
    self.compile_optional_chain_expression(&member.object, &mut |this| {
      if member.optional {
        this.emit(Op::Copy, LINE);
        this.emit(Op::JumpIfNullish, LINE);
        let nullish = this.current_ip();
        this.emit_jump_placeholder();

        let key = this.ensure_private_name_constant(&member.field.name);
        this.emit_constant(key, LINE);
        this.emit(Op::GetProperty, LINE);
        continuation(this)?;

        this.emit(Op::Jump, LINE);
        let end = this.current_ip();
        this.emit_jump_placeholder();
        this.patch_jump(nullish);
        this.emit(Op::Discard, LINE);
        this.emit(Op::Undefined, LINE);
        this.patch_jump(end);
      } else {
        let key = this.ensure_private_name_constant(&member.field.name);
        this.emit_constant(key, LINE);
        this.emit(Op::GetProperty, LINE);
        continuation(this)?;
      }
      Ok(())
    })
  }

  pub(super) fn compile_optional_chain_callee(
    &mut self,
    callee: &oast::Expression<'_>,
    continuation: &mut dyn FnMut(&mut Self) -> Result<(), crate::errors::CompilerError>,
  ) -> Result<(), crate::errors::CompilerError> {
    match callee {
      oast::Expression::StaticMemberExpression(member) => {
        if matches!(&member.object, oast::Expression::Super(_)) {
          if member.optional {
            return Err(crate::errors::CompilerError::from(
              "optional chaining with super is not supported",
            ));
          }
          self.emit_super_property_get_static(member.property.name.as_str(), true)?;
          return continuation(self);
        }
        self.compile_optional_chain_expression(&member.object, &mut |this| {
          if member.optional {
            this.emit(Op::Copy, LINE);
            this.emit(Op::JumpIfNullish, LINE);
            let nullish = this.current_ip();
            this.emit_jump_placeholder();

            this.emit(Op::Copy, LINE);
            this.emit_constant(Value::String(member.property.name.to_string().into()), LINE);
            this.emit(Op::GetProperty, LINE);
            continuation(this)?;

            this.emit(Op::Jump, LINE);
            let end = this.current_ip();
            this.emit_jump_placeholder();
            this.patch_jump(nullish);
            this.emit(Op::Discard, LINE);
            this.emit(Op::Undefined, LINE);
            this.patch_jump(end);
          } else {
            this.emit(Op::Copy, LINE);
            this.emit_constant(Value::String(member.property.name.to_string().into()), LINE);
            this.emit(Op::GetProperty, LINE);
            continuation(this)?;
          }
          Ok(())
        })
      }
      oast::Expression::ComputedMemberExpression(member) => {
        if matches!(&member.object, oast::Expression::Super(_)) {
          if member.optional {
            return Err(crate::errors::CompilerError::from(
              "optional chaining with super is not supported",
            ));
          }
          self.emit_super_property_get_computed(&member.expression, true)?;
          return continuation(self);
        }
        self.compile_optional_chain_expression(&member.object, &mut |this| {
          if member.optional {
            this.emit(Op::Copy, LINE);
            this.emit(Op::JumpIfNullish, LINE);
            let nullish = this.current_ip();
            this.emit_jump_placeholder();

            this.emit(Op::Copy, LINE);
            this.compile_expr_oast(&member.expression)?;
            this.emit(Op::GetProperty, LINE);
            continuation(this)?;

            this.emit(Op::Jump, LINE);
            let end = this.current_ip();
            this.emit_jump_placeholder();
            this.patch_jump(nullish);
            this.emit(Op::Discard, LINE);
            this.emit(Op::Undefined, LINE);
            this.patch_jump(end);
          } else {
            this.emit(Op::Copy, LINE);
            this.compile_expr_oast(&member.expression)?;
            this.emit(Op::GetProperty, LINE);
            continuation(this)?;
          }
          Ok(())
        })
      }
      oast::Expression::PrivateFieldExpression(member) => {
        self.compile_optional_chain_expression(&member.object, &mut |this| {
          if member.optional {
            this.emit(Op::Copy, LINE);
            this.emit(Op::JumpIfNullish, LINE);
            let nullish = this.current_ip();
            this.emit_jump_placeholder();

            this.emit(Op::Copy, LINE);
            let key = this.ensure_private_name_constant(&member.field.name);
            this.emit_constant(key, LINE);
            this.emit(Op::GetProperty, LINE);
            continuation(this)?;

            this.emit(Op::Jump, LINE);
            let end = this.current_ip();
            this.emit_jump_placeholder();
            this.patch_jump(nullish);
            this.emit(Op::Discard, LINE);
            this.emit(Op::Undefined, LINE);
            this.patch_jump(end);
          } else {
            this.emit(Op::Copy, LINE);
            let key = this.ensure_private_name_constant(&member.field.name);
            this.emit_constant(key, LINE);
            this.emit(Op::GetProperty, LINE);
            continuation(this)?;
          }
          Ok(())
        })
      }
      _ => self.compile_optional_chain_expression(callee, continuation),
    }
  }

  pub(super) fn emit_optional_chain_spread_call(
    &mut self,
    call: &oast::CallExpression<'_>,
    has_receiver: bool,
  ) -> Result<(), crate::errors::CompilerError> {
    let arg_count = self.checked_arg_count(call.arguments.len())?;
    let receiver_slot = if has_receiver {
      let callee_slot = self.alloc_temp_local("chain_spread_callee");
      self.emit(Op::StoreLocal, LINE);
      self.emit_slot(callee_slot, LINE);
      let receiver_slot = self.alloc_temp_local("chain_spread_receiver");
      self.emit(Op::StoreLocal, LINE);
      self.emit_slot(receiver_slot, LINE);
      self.emit(Op::LoadLocal, LINE);
      self.emit_slot(callee_slot, LINE);
      Some(receiver_slot)
    } else {
      None
    };

    let mut spread_flags = Vec::with_capacity(call.arguments.len());
    for argument in &call.arguments {
      match argument {
        oast::Argument::SpreadElement(spread) => {
          self.compile_expr_oast(&spread.argument)?;
          spread_flags.push(true);
        }
        _ => match argument.as_expression() {
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
    for (index, is_spread) in spread_flags.into_iter().enumerate() {
      if index == 0
        && let Some(receiver_slot) = receiver_slot
      {
        self.emit(Op::LoadLocal, LINE);
        self.emit_slot(receiver_slot, LINE);
        self.emit(if is_spread { Op::True } else { Op::False }, LINE);
        self.emit(Op::BuildArray, LINE);
        self.emit_byte(2, LINE);
        continue;
      }
      self.emit(if is_spread { Op::True } else { Op::False }, LINE);
    }
    self.emit(Op::BuildArray, LINE);
    self.emit_byte(arg_count, LINE);
    self.emit(Op::Spread, LINE);
    self.emit_byte(arg_count, LINE);
    Ok(())
  }

  pub(super) fn compile_optional_chain_call(
    &mut self,
    call: &oast::CallExpression<'_>,
    continuation: &mut dyn FnMut(&mut Self) -> Result<(), crate::errors::CompilerError>,
  ) -> Result<(), crate::errors::CompilerError> {
    let member_callee = matches!(
      &call.callee,
      oast::Expression::StaticMemberExpression(_)
        | oast::Expression::ComputedMemberExpression(_)
        | oast::Expression::PrivateFieldExpression(_)
    );
    let mut emit_call = |this: &mut Self| {
      let cleanup = if call.optional {
        this.emit(Op::Copy, LINE);
        this.emit(Op::JumpIfNullish, LINE);
        let nullish = this.current_ip();
        this.emit_jump_placeholder();
        Some(nullish)
      } else {
        None
      };

      if call
        .arguments
        .iter()
        .any(|arg| matches!(arg, oast::Argument::SpreadElement(_)))
      {
        this.emit_optional_chain_spread_call(call, member_callee)?;
      } else {
        let arg_count = this.compile_call_arguments(&call.arguments)?;
        this.emit_call_with_arg_count(
          if member_callee {
            Op::CallBuiltin
          } else {
            Op::Call
          },
          arg_count,
        )?;
      }
      continuation(this)?;

      if let Some(nullish) = cleanup {
        this.emit(Op::Jump, LINE);
        let end = this.current_ip();
        this.emit_jump_placeholder();
        this.patch_jump(nullish);
        if member_callee {
          this.emit(Op::Discard, LINE);
        }
        this.emit(Op::Discard, LINE);
        this.emit(Op::Undefined, LINE);
        this.patch_jump(end);
      }
      Ok(())
    };

    if member_callee {
      self.compile_optional_chain_callee(&call.callee, &mut emit_call)
    } else {
      self.compile_optional_chain_expression(&call.callee, &mut emit_call)
    }
  }

  pub(super) fn compile_optional_chain_expression(
    &mut self,
    expr: &oast::Expression<'_>,
    continuation: &mut dyn FnMut(&mut Self) -> Result<(), crate::errors::CompilerError>,
  ) -> Result<(), crate::errors::CompilerError> {
    match expr {
      oast::Expression::StaticMemberExpression(member) => {
        self.compile_optional_chain_member_static(member, continuation)
      }
      oast::Expression::ComputedMemberExpression(member) => {
        self.compile_optional_chain_member_computed(member, continuation)
      }
      oast::Expression::PrivateFieldExpression(member) => {
        self.compile_optional_chain_member_private(member, continuation)
      }
      oast::Expression::CallExpression(call) => {
        self.compile_optional_chain_call(call, continuation)
      }
      oast::Expression::ParenthesizedExpression(parenthesized) => {
        self.compile_expr_oast(&parenthesized.expression)?;
        continuation(self)
      }
      _ => {
        self.compile_expr_oast(expr)?;
        continuation(self)
      }
    }
  }

  pub(super) fn compile_optional_chain_element(
    &mut self,
    element: &oast::ChainElement<'_>,
    continuation: &mut dyn FnMut(&mut Self) -> Result<(), crate::errors::CompilerError>,
  ) -> Result<(), crate::errors::CompilerError> {
    use oast::ChainElement as CE;
    match element {
      CE::StaticMemberExpression(member) => {
        self.compile_optional_chain_member_static(member, continuation)
      }
      CE::ComputedMemberExpression(member) => {
        self.compile_optional_chain_member_computed(member, continuation)
      }
      CE::PrivateFieldExpression(member) => {
        self.compile_optional_chain_member_private(member, continuation)
      }
      CE::CallExpression(call) => self.compile_optional_chain_call(call, continuation),
      CE::TSNonNullExpression(non_null) => {
        self.compile_optional_chain_expression(&non_null.expression, continuation)
      }
    }
  }
}
