#![allow(dead_code)]

use crate::bytecode::Op;
use crate::value::Value;
use oxc_ast::ast as oast;

const LINE: u32 = 1;

impl super::Compiler {
    pub(super) fn identifier_name_if_parenthesized<'a>(
        expr: &'a oast::Expression<'a>,
    ) -> Option<&'a str> {
        match expr {
            oast::Expression::Identifier(id) => Some(id.name.as_str()),
            oast::Expression::ParenthesizedExpression(p) => {
                Self::identifier_name_if_parenthesized(&p.expression)
            }
            _ => None,
        }
    }

    fn compile_call_expression_tail(
        &mut self,
        c: &oast::CallExpression<'_>,
    ) -> Result<(), crate::errors::CompilerError> {
        use oxc_ast::ast::Expression as OExpr;
        // Tail calls only need the optimized path for direct/non-spread calls.
        // Spread calls still lower through the existing non-tail path.
        let has_spread = c
            .arguments
            .iter()
            .any(|arg| matches!(arg, oast::Argument::SpreadElement(_)));
        if has_spread {
            self.compile_expr_oast(&c.callee)?;
            for arg in &c.arguments {
                match arg {
                    oast::Argument::SpreadElement(s) => {
                        self.compile_expr_oast(&s.argument)?;
                    }
                    _ => {
                        if let Some(expr) = arg.as_expression() {
                            self.compile_expr_oast(expr)?;
                        } else {
                            self.emit(Op::Undefined, LINE);
                        }
                    }
                }
            }
            self.emit(Op::BuildArray, LINE);
            self.emit_byte(c.arguments.len() as u8, LINE);
            for arg in &c.arguments {
                if matches!(arg, oast::Argument::SpreadElement(_)) {
                    self.emit(Op::True, LINE);
                } else {
                    self.emit(Op::False, LINE);
                }
            }
            self.emit(Op::BuildArray, LINE);
            self.emit_byte(c.arguments.len() as u8, LINE);
            self.emit(Op::Spread, LINE);
            self.emit_byte(c.arguments.len() as u8, LINE);
            return Ok(());
        }

        if let OExpr::Super(_) = &c.callee {
            return Err(crate::errors::CompilerError::from(
                "direct super() calls are not supported yet",
            ));
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
            let mut arg_count = 0usize;
            for arg in &c.arguments {
                if let Some(expr) = arg.as_expression() {
                    self.compile_expr_oast(expr)?;
                    arg_count += 1;
                } else {
                    self.emit(Op::Undefined, LINE);
                    arg_count += 1;
                }
            }
            self.emit(Op::TailCallMethod, LINE);
            self.emit_byte(arg_count as u8, LINE);
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
            let mut arg_count = 0usize;
            for arg in &c.arguments {
                if let Some(expr) = arg.as_expression() {
                    self.compile_expr_oast(expr)?;
                    arg_count += 1;
                } else {
                    self.emit(Op::Undefined, LINE);
                    arg_count += 1;
                }
            }
            self.emit(Op::TailCallMethod, LINE);
            self.emit_byte(arg_count as u8, LINE);
            return Ok(());
        }

        if let oast::Expression::StaticMemberExpression(m) = &c.callee {
            let is_optional_call = c.optional;

            self.compile_expr_oast(&m.object)?;
            if m.optional {
                self.emit(Op::Dup, LINE);
                self.emit(Op::JumpIfNullish, LINE);
                let skip_nullish = self.chunk.code.len();
                self.emit_byte(0, LINE);
                self.emit_byte(0, LINE);
                self.emit(Op::Pop, LINE);
                self.emit(Op::Null, LINE);
                self.emit(Op::Jump, LINE);
                let after_null_check = self.chunk.code.len();
                self.emit_byte(0, LINE);
                self.emit_byte(0, LINE);
                self.patch_jump(skip_nullish);
                self.emit(Op::Dup, LINE);
                self.emit_constant(Value::String(m.property.name.to_string()), LINE);
                self.emit(Op::GetPropertyIC, LINE);
                let slot = self.alloc_ic_slot()?;
                self.emit_byte(slot, LINE);
                self.patch_jump(after_null_check);
            } else {
                self.emit(Op::Dup, LINE);
                self.emit_constant(Value::String(m.property.name.to_string()), LINE);
                self.emit(Op::GetPropertyIC, LINE);
                let slot = self.alloc_ic_slot()?;
                self.emit_byte(slot, LINE);
            }
            let mut arg_count = 0usize;
            for arg in &c.arguments {
                if let Some(expr) = arg.as_expression() {
                    self.compile_expr_oast(expr)?;
                    arg_count += 1;
                } else {
                    self.emit(Op::Undefined, LINE);
                    arg_count += 1;
                }
            }
            if is_optional_call {
                self.emit(Op::Call, LINE);
            } else {
                self.emit(Op::TailCallMethod, LINE);
            }
            self.emit_byte(arg_count as u8, LINE);
            return Ok(());
        }

        if let oast::Expression::ComputedMemberExpression(m) = &c.callee {
            let is_optional_call = c.optional;

            self.compile_expr_oast(&m.object)?;
            if m.optional {
                self.emit(Op::Dup, LINE);
                self.emit(Op::JumpIfNullish, LINE);
                let skip_nullish = self.chunk.code.len();
                self.emit_byte(0, LINE);
                self.emit_byte(0, LINE);
                self.emit(Op::Pop, LINE);
                self.emit(Op::Null, LINE);
                self.emit(Op::Jump, LINE);
                let after_null_check = self.chunk.code.len();
                self.emit_byte(0, LINE);
                self.emit_byte(0, LINE);
                self.patch_jump(skip_nullish);
                self.compile_expr_oast(&m.expression)?;
                self.emit(Op::GetProperty, LINE);
                self.patch_jump(after_null_check);
            } else {
                self.emit(Op::Dup, LINE);
                self.compile_expr_oast(&m.expression)?;
                self.emit(Op::GetProperty, LINE);
            }
            let mut arg_count = 0usize;
            for arg in &c.arguments {
                if let Some(expr) = arg.as_expression() {
                    self.compile_expr_oast(expr)?;
                    arg_count += 1;
                } else {
                    self.emit(Op::Undefined, LINE);
                    arg_count += 1;
                }
            }
            if is_optional_call {
                self.emit(Op::Call, LINE);
            } else {
                self.emit(Op::TailCallMethod, LINE);
            }
            self.emit_byte(arg_count as u8, LINE);
            return Ok(());
        }

        if let oast::Expression::PrivateFieldExpression(m) = &c.callee {
            let is_optional_call = c.optional;

            self.compile_expr_oast(&m.object)?;
            if m.optional {
                self.emit(Op::Dup, LINE);
                self.emit(Op::JumpIfNullish, LINE);
                let skip_nullish = self.chunk.code.len();
                self.emit_byte(0, LINE);
                self.emit_byte(0, LINE);
                self.emit(Op::Pop, LINE);
                self.emit(Op::Null, LINE);
                self.emit(Op::Jump, LINE);
                let after_null_check = self.chunk.code.len();
                self.emit_byte(0, LINE);
                self.emit_byte(0, LINE);
                self.patch_jump(skip_nullish);
                let key = self.ensure_private_name_constant(&m.field.name);
                self.emit_constant(key, LINE);
                self.emit(Op::GetProperty, LINE);
                self.patch_jump(after_null_check);
            } else {
                self.emit(Op::Dup, LINE);
                let key = self.ensure_private_name_constant(&m.field.name);
                self.emit_constant(key, LINE);
                self.emit(Op::GetProperty, LINE);
            }
            let mut arg_count = 0usize;
            for arg in &c.arguments {
                if let Some(expr) = arg.as_expression() {
                    self.compile_expr_oast(expr)?;
                    arg_count += 1;
                } else {
                    self.emit(Op::Undefined, LINE);
                    arg_count += 1;
                }
            }
            if is_optional_call {
                self.emit(Op::Call, LINE);
            } else {
                self.emit(Op::TailCallMethod, LINE);
            }
            self.emit_byte(arg_count as u8, LINE);
            return Ok(());
        }

        if c.optional {
            // Optional calls short-circuit to `undefined` if the callee is nullish.
            self.compile_expr_oast(&c.callee)?;
            self.emit(Op::Dup, LINE);
            self.emit(Op::JumpIfNullish, LINE);
            let skip_nullish = self.chunk.code.len();
            self.emit_byte(0, LINE);
            self.emit_byte(0, LINE);
            self.emit(Op::Pop, LINE);
            self.emit(Op::Null, LINE);
            self.emit(Op::Jump, LINE);
            let end = self.chunk.code.len();
            self.emit_byte(0, LINE);
            self.emit_byte(0, LINE);
            self.patch_jump(skip_nullish);
            let mut arg_count = 0usize;
            for arg in &c.arguments {
                if let Some(expr) = arg.as_expression() {
                    self.compile_expr_oast(expr)?;
                    arg_count += 1;
                } else {
                    self.emit(Op::Undefined, LINE);
                    arg_count += 1;
                }
            }
            self.emit(Op::Call, LINE);
            self.emit_byte(arg_count as u8, LINE);
            self.patch_jump(end);
            return Ok(());
        }

        self.compile_expr_oast(&c.callee)?;
        let mut arg_count = 0usize;
        for arg in &c.arguments {
            if let Some(expr) = arg.as_expression() {
                self.compile_expr_oast(expr)?;
                arg_count += 1;
            } else {
                self.emit(Op::Undefined, LINE);
                arg_count += 1;
            }
        }
        self.emit(Op::TailCallIC, LINE);
        self.emit_byte(arg_count as u8, LINE);
        Ok(())
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
            self.emit_constant(Value::String(cooked), LINE);
        }
        self.emit(Op::BuildArray, LINE);
        self.emit_byte(t.quasi.quasis.len() as u8, LINE);
        self.emit(Op::Dup, LINE);
        self.emit_constant(Value::String("raw".to_string()), LINE);
        for quasi in &t.quasi.quasis {
            self.emit_constant(Value::String(quasi.value.raw.to_string()), LINE);
        }
        self.emit(Op::BuildArray, LINE);
        self.emit_byte(t.quasi.quasis.len() as u8, LINE);
        self.emit(Op::SetPropertyIC, LINE);
        let slot = self.alloc_ic_slot()?;
        self.emit_byte(slot, LINE);
        self.emit(Op::Pop, LINE);
        let mut arg_count = 1usize;
        for sub in &t.quasi.expressions {
            self.compile_expr_oast(sub)?;
            arg_count += 1;
        }
        self.emit(Op::TailCallIC, LINE);
        self.emit_byte(arg_count as u8, LINE);
        Ok(())
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
                        self.emit(Op::Pop, LINE);
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
                    self.emit(Op::Dup, LINE);
                    self.emit(Op::JumpIfFalse, LINE);
                    let jump_end = self.chunk.code.len();
                    self.emit_byte(0, LINE);
                    self.emit_byte(0, LINE);
                    self.emit(Op::Pop, LINE);
                    self.compile_expr_oast_tail(&l.right)?;
                    self.patch_jump(jump_end);
                    Ok(())
                }
                oast::LogicalOperator::Or => {
                    self.compile_expr_oast(&l.left)?;
                    self.emit(Op::Dup, LINE);
                    self.emit(Op::JumpIfTrue, LINE);
                    let jump_end = self.chunk.code.len();
                    self.emit_byte(0, LINE);
                    self.emit_byte(0, LINE);
                    self.emit(Op::Pop, LINE);
                    self.compile_expr_oast_tail(&l.right)?;
                    self.patch_jump(jump_end);
                    Ok(())
                }
                oast::LogicalOperator::Coalesce => {
                    self.compile_expr_oast(&l.left)?;
                    self.emit(Op::Dup, LINE);
                    self.emit(Op::JumpIfNullish, LINE);
                    let jump_right = self.chunk.code.len();
                    self.emit_byte(0, LINE);
                    self.emit_byte(0, LINE);
                    self.emit(Op::Jump, LINE);
                    let jump_end = self.chunk.code.len();
                    self.emit_byte(0, LINE);
                    self.emit_byte(0, LINE);
                    self.patch_jump(jump_right);
                    self.emit(Op::Pop, LINE);
                    self.compile_expr_oast_tail(&l.right)?;
                    self.patch_jump(jump_end);
                    Ok(())
                }
            },
            OExpr::ConditionalExpression(c) => {
                if let Some(jump_else) = self.try_emit_compare_jump_if_false(&c.test)? {
                    self.compile_expr_oast_tail(&c.consequent)?;
                    self.emit(Op::Jump, LINE);
                    let jump_end = self.chunk.code.len();
                    self.emit_byte(0, LINE);
                    self.emit_byte(0, LINE);
                    self.patch_jump(jump_else);
                    self.compile_expr_oast_tail(&c.alternate)?;
                    self.patch_jump(jump_end);
                } else {
                    self.compile_expr_oast(&c.test)?;
                    self.emit(Op::JumpIfFalse, LINE);
                    let jump_else = self.chunk.code.len();
                    self.emit_byte(0, LINE);
                    self.emit_byte(0, LINE);
                    self.compile_expr_oast_tail(&c.consequent)?;
                    self.emit(Op::Jump, LINE);
                    let jump_end = self.chunk.code.len();
                    self.emit_byte(0, LINE);
                    self.emit_byte(0, LINE);
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
