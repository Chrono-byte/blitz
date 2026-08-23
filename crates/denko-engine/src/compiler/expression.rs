// SPDX-License-Identifier: MPL-2.0
//! Expression compilation
//!
//! Handles compilation of expressions from the AST into bytecode,
//! including:
//! - Literals (numeric, string, boolean, null, this)
//! - Template literals and tagged templates
//! - Object and array literals
//! - Function and arrow function expressions
//! - Member access (static and computed)
//! - Binary and unary operators
//! - Assignment expressions
//! - Conditional (ternary) expressions
//! - Sequence expressions
//! - Logical operators (&&, ||, ??)
//!
//! This module is part of the compiler's multi-phase compilation process.
//! Expression compilation is recursive and produces compiler-owned virtual
//! instructions that are finalized as RISC v2 bytecode.

use super::emit::FrontendOp as Op;
use crate::value::Value;
use oxc_ast::ast as oast;
use oxc_syntax::number::NumberBase;

const LINE: u32 = 1;

impl super::Compiler {
  fn validate_literal_early_errors(
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
    self.emit(Op::GetThis, LINE);
    self.emit(Op::Copy, LINE);
    self.emit_constant(Value::String(property.to_string().into()), LINE);
    self.emit(Op::GetSuperProperty, LINE);
    if !preserve_receiver {
      self.emit(Op::Exchange, LINE);
      self.emit(Op::Discard, LINE);
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
    self.emit(Op::GetThis, LINE);
    self.emit(Op::Copy, LINE);
    self.compile_expr_oast(key_expr)?;
    self.emit(Op::GetSuperProperty, LINE);
    if !preserve_receiver {
      self.emit(Op::Exchange, LINE);
      self.emit(Op::Discard, LINE);
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
    self.emit(Op::GetThis, LINE);
    self.emit(Op::Copy, LINE);
    self.emit_constant(Value::String(property.to_string().into()), LINE);
    self.compile_expr_oast(value_expr)?;
    self.emit(Op::SetSuperProperty, LINE);
    self.emit(Op::Exchange, LINE);
    self.emit(Op::Discard, LINE);
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
    self.emit(Op::GetThis, LINE);
    self.emit(Op::Copy, LINE);
    self.compile_expr_oast(key_expr)?;
    self.compile_expr_oast(value_expr)?;
    self.emit(Op::SetSuperProperty, LINE);
    self.emit(Op::Exchange, LINE);
    self.emit(Op::Discard, LINE);
    Ok(())
  }

  fn compile_static_member_reference(
    &mut self,
    m: &oast::StaticMemberExpression<'_>,
  ) -> Result<(usize, usize), crate::errors::CompilerError> {
    self.compile_expr_oast(&m.object)?;
    let object_slot = self.alloc_temp_local("ref_obj");
    self.emit(Op::StoreLocal, LINE);
    self.emit_byte(
      u8::try_from(object_slot).expect("object_slot fits in u8"),
      LINE,
    );

    self.emit_constant(Value::String(m.property.name.to_string().into()), LINE);
    let key_slot = self.alloc_temp_local("ref_key");
    self.emit(Op::StoreLocal, LINE);
    self.emit_byte(u8::try_from(key_slot).expect("key_slot fits in u8"), LINE);
    Ok((object_slot, key_slot))
  }

  fn compile_computed_member_reference(
    &mut self,
    m: &oast::ComputedMemberExpression<'_>,
  ) -> Result<(usize, usize), crate::errors::CompilerError> {
    self.compile_expr_oast(&m.object)?;
    let object_slot = self.alloc_temp_local("ref_obj");
    self.emit(Op::StoreLocal, LINE);
    self.emit_byte(self.slot_to_u8(object_slot)?, LINE);

    self.compile_expr_oast(&m.expression)?;
    let key_slot = self.alloc_temp_local("ref_key");
    self.emit(Op::StoreLocal, LINE);
    self.emit_byte(self.slot_to_u8(key_slot)?, LINE);
    Ok((object_slot, key_slot))
  }

  fn emit_reference_get(
    &mut self,
    object_slot: usize,
    key_slot: usize,
  ) -> Result<(), crate::errors::CompilerError> {
    self.emit(Op::LoadLocal, LINE);
    self.emit_byte(self.slot_to_u8(object_slot)?, LINE);
    self.emit(Op::LoadLocal, LINE);
    self.emit_byte(self.slot_to_u8(key_slot)?, LINE);
    self.emit(Op::GetProperty, LINE);
    Ok(())
  }

  fn emit_reference_set_current_value(
    &mut self,
    object_slot: usize,
    key_slot: usize,
  ) -> Result<(), crate::errors::CompilerError> {
    let value_slot = self.alloc_temp_local("ref_value");
    self.emit(Op::StoreLocal, LINE);
    self.emit_byte(
      u8::try_from(value_slot).expect("value_slot fits in u8"),
      LINE,
    );
    self.emit(Op::LoadLocal, LINE);
    self.emit_byte(
      u8::try_from(object_slot).expect("object_slot fits in u8"),
      LINE,
    );
    self.emit(Op::LoadLocal, LINE);
    self.emit_byte(u8::try_from(key_slot).expect("key_slot fits in u8"), LINE);
    self.emit(Op::LoadLocal, LINE);
    self.emit_byte(
      u8::try_from(value_slot).expect("value_slot fits in u8"),
      LINE,
    );
    self.emit(Op::SetProperty, LINE);
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

    // CompareJump cond_type values:
    // 0=Eq, 1=Neq, 2=Lt, 3=Lte, 4=Gt, 5=Gte, 6=StrictEq, 7=StrictNeq
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
    // Payload layout for CompareJump:
    // - payload byte 0: cond_type (3 bits, rest unused)
    // - payload bytes 1..2: relative jump offset placeholder (patched later)
    self.emit_with_payload(Op::CompareJump, cond_type as u32, LINE);

    // Return the instruction index containing the compare+jump payload so
    // callers can patch the branch offset while preserving cond_type.
    let patch_addr = self.current_ip().saturating_sub(1);

    Ok(Some(patch_addr))
  }

  fn emit_compound_assignment_op(
    &mut self,
    op: oast::AssignmentOperator,
  ) {
    match op {
      oast::AssignmentOperator::Addition => self.emit(Op::Add, LINE),
      oast::AssignmentOperator::Subtraction => self.emit(Op::Sub, LINE),
      oast::AssignmentOperator::Multiplication => self.emit(Op::Mul, LINE),
      oast::AssignmentOperator::Division => self.emit(Op::Div, LINE),
      oast::AssignmentOperator::Remainder => self.emit(Op::Mod, LINE),
      oast::AssignmentOperator::Exponential => self.emit(Op::Pow, LINE),
      oast::AssignmentOperator::BitwiseAnd => self.emit(Op::BitAnd, LINE),
      oast::AssignmentOperator::BitwiseOR => self.emit(Op::BitOr, LINE),
      oast::AssignmentOperator::BitwiseXOR => self.emit(Op::BitXor, LINE),
      oast::AssignmentOperator::ShiftLeft => self.emit(Op::Shl, LINE),
      oast::AssignmentOperator::ShiftRight => self.emit(Op::Shr, LINE),
      oast::AssignmentOperator::ShiftRightZeroFill => self.emit(Op::Ushr, LINE),
      _ => {}
    }
  }

  /// Compile an expression from the AST into bytecode.
  ///
  /// This is the main entry point for expression compilation. It recursively
  /// processes expressions and emits appropriate bytecode operations to the
  /// current chunk.
  ///
  /// # Expressions Supported
  ///
  /// - Literals: numeric, string, boolean, null, BigInt
  /// - Template literals and tagged templates
  /// - Object and array construction
  /// - Function and arrow function expressions
  /// - Member access (static and computed)
  /// - Binary and unary operations
  /// - Assignment and compound assignment
  /// - Conditional (ternary) expressions
  /// - Sequence expressions
  /// - Call expressions
  /// - New expressions
  /// - Update expressions (++ and --)
  /// - Logical operators with short-circuit evaluation
  pub(super) fn compile_expr_oast(
    &mut self,
    expr: &oast::Expression,
  ) -> Result<(), crate::errors::CompilerError> {
    use oxc_ast::ast::Expression as OExpr;
    match expr {
      OExpr::NumericLiteral(n) => {
        self.validate_literal_early_errors(expr)?;
        self.emit_constant(Value::Number(n.value), LINE)
      }
      OExpr::BigIntLiteral(n) => {
        // Use n.raw if available, otherwise use source text directly
        let parsed = if let Some(raw) = &n.raw {
          let s = raw.as_str();
          let digits_str = s.strip_suffix('n').unwrap_or(s).replace('_', "");
          // Use the base field to determine radix, handle both lowercase and uppercase prefixes
          match n.base {
            oast::BigintBase::Binary => {
              let after = digits_str
                .strip_prefix("0b")
                .or_else(|| digits_str.strip_prefix("0B"))
                .unwrap_or(&digits_str);
              num_bigint::BigInt::parse_bytes(after.as_bytes(), 2)
            }
            oast::BigintBase::Octal => {
              let after = digits_str
                .strip_prefix("0o")
                .or_else(|| digits_str.strip_prefix("0O"))
                .unwrap_or(&digits_str);
              num_bigint::BigInt::parse_bytes(after.as_bytes(), 8)
            }
            oast::BigintBase::Hex => {
              let after = digits_str
                .strip_prefix("0x")
                .or_else(|| digits_str.strip_prefix("0X"))
                .unwrap_or(&digits_str);
              num_bigint::BigInt::parse_bytes(after.as_bytes(), 16)
            }
            oast::BigintBase::Decimal => num_bigint::BigInt::parse_bytes(digits_str.as_bytes(), 10),
          }
        } else {
          // Recover the digits from the original source when Oxc did not
          // retain a normalized bigint spelling; the span uses byte offsets.
          let source = self.source_text.as_ref();
          let start = n.span.start as usize;
          let end = n.span.end as usize;
          if start < end && end <= source.len() {
            let digits = source[start..end]
              .strip_suffix('n')
              .unwrap_or_else(|| {
                let range = start..end;
                &source[range]
              })
              .replace('_', "");
            // Use base field to determine radix, handle both lowercase and uppercase prefixes
            match n.base {
              oast::BigintBase::Binary => {
                let after = digits
                  .strip_prefix("0b")
                  .or_else(|| digits.strip_prefix("0B"))
                  .unwrap_or(&digits);
                num_bigint::BigInt::parse_bytes(after.as_bytes(), 2)
              }
              oast::BigintBase::Octal => {
                let after = digits
                  .strip_prefix("0o")
                  .or_else(|| digits.strip_prefix("0O"))
                  .unwrap_or(&digits);
                num_bigint::BigInt::parse_bytes(after.as_bytes(), 8)
              }
              oast::BigintBase::Hex => {
                let after = digits
                  .strip_prefix("0x")
                  .or_else(|| digits.strip_prefix("0X"))
                  .unwrap_or(&digits);
                num_bigint::BigInt::parse_bytes(after.as_bytes(), 16)
              }
              oast::BigintBase::Decimal => num_bigint::BigInt::parse_bytes(digits.as_bytes(), 10),
            }
          } else {
            None
          }
        };
        let parsed =
          parsed.ok_or_else(|| crate::errors::CompilerError::from("invalid bigint literal"))?;
        self.emit_constant(Value::BigInt(parsed), LINE);
      }
      OExpr::StringLiteral(s) => {
        self.validate_literal_early_errors(expr)?;
        self.emit_constant(Value::String(s.value.to_string().into()), LINE)
      }
      OExpr::TemplateLiteral(t) => {
        let first = t
          .quasis
          .first()
          .and_then(|q| q.value.cooked.as_ref())
          .map(ToString::to_string)
          .unwrap_or_default();
        self.emit_constant(Value::String(first.into()), LINE);
        for (i, expr) in t.expressions.iter().enumerate() {
          // Apply ECMAScript ToString to each substitution before it is
          // concatenated. In particular, ToString uses the string hint for
          // objects and rejects Symbols.
          self.emit_get_builtin_name("toString");
          self.compile_expr_oast(expr)?;
          self.emit(Op::Call, LINE);
          self.emit_byte(1, LINE);
          self.emit(Op::Add, LINE);
          let tail = t
            .quasis
            .get(i + 1)
            .and_then(|q| q.value.cooked.as_ref())
            .map(ToString::to_string)
            .unwrap_or_default();
          self.emit_constant(Value::String(tail.into()), LINE);
          self.emit(Op::Add, LINE);
        }
      }
      OExpr::TaggedTemplateExpression(t) => {
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
        self.emit_byte(
          u8::try_from(t.quasi.quasis.len()).expect("quasis.len fits in u8"),
          LINE,
        );
        self.emit(Op::Copy, LINE);
        self.emit_constant(Value::String("raw".to_string().into()), LINE);
        for quasi in &t.quasi.quasis {
          self.emit_constant(Value::String(quasi.value.raw.to_string().into()), LINE);
        }
        self.emit(Op::BuildArray, LINE);
        self.emit_byte(
          u8::try_from(t.quasi.quasis.len()).expect("quasis.len fits in u8"),
          LINE,
        );
        self.emit(Op::SetProperty, LINE);
        self.emit(Op::Discard, LINE);
        let mut arg_count = 1usize;
        for sub in &t.quasi.expressions {
          self.compile_expr_oast(sub)?;
          arg_count += 1;
        }
        self.emit(Op::Call, LINE);
        self.emit_byte(u8::try_from(arg_count).expect("arg_count fits in u8"), LINE);
      }
      OExpr::BooleanLiteral(b) => {
        if b.value {
          self.emit(Op::True, LINE)
        } else {
          self.emit(Op::False, LINE);
        }
      }
      OExpr::NullLiteral(_) => self.emit(Op::Null, LINE),
      OExpr::ThisExpression(_) => {
        // Emit GetThis to push the current this binding for the running frame.
        self.emit(Op::GetThis, LINE);
      }
      OExpr::ArrowFunctionExpression(a) => {
        let name = "<arrow>";
        let (proto, _env_len) = self.compile_arrow_function(name, a, None)?;
        let idx = self.chunk.add_function(proto);
        self.emit(Op::MakeClosure, LINE);
        self.emit_byte(self.function_index_to_u8(idx)?, LINE);
      }
      OExpr::FunctionExpression(f) => {
        let self_name = f.id.as_ref().map(|id| id.name.as_str());
        let name = self_name.unwrap_or("<anonymous>");
        let (proto, _env_len) = self.compile_function(name, self_name, f, None, false)?;
        let idx = self.chunk.add_function(proto);
        self.emit(Op::MakeClosure, LINE);
        self.emit_byte(self.function_index_to_u8(idx)?, LINE);
      }
      OExpr::ObjectExpression(obj) => {
        // Start with an empty object and define each property as it is visited.
        // In particular, do not collect static, computed, accessor, and spread
        // properties into separate buckets: keys and values can have effects.
        self.emit(Op::BuildObject, LINE);
        self.emit_byte(0, LINE);

        for prop in &obj.properties {
          match prop {
            oast::ObjectPropertyKind::ObjectProperty(p)
              if matches!(p.kind, oast::PropertyKind::Init) =>
            {
              self.emit(Op::Copy, LINE);
              self.emit_get_builtin_name("objectDefineProperty");
              self.emit(Op::Exchange, LINE);

              if p.computed {
                let key = p
                  .key
                  .as_expression()
                  .expect("computed property key must be an expression");
                self.compile_expr_oast(key)?;
              } else {
                let key = match &p.key {
                  oast::PropertyKey::StaticIdentifier(ident) => ident.name.to_string(),
                  oast::PropertyKey::Identifier(ident) => ident.name.to_string(),
                  oast::PropertyKey::StringLiteral(s) => s.value.to_string(),
                  oast::PropertyKey::NumericLiteral(n) => super::numeric_property_key(n.value),
                  _ => return Err(crate::errors::CompilerError::from("unsupported object key")),
                };
                self.emit_constant(Value::String(key.into()), LINE);
              }

              self.emit_constant(Value::String("value".to_string().into()), LINE);
              if p.shorthand {
                match &p.key {
                  oast::PropertyKey::StaticIdentifier(ident) => {
                    self.emit_get_binding(ident.name.as_str());
                  }
                  oast::PropertyKey::Identifier(ident) => {
                    self.emit_get_binding(ident.name.as_str());
                  }
                  _ => self.emit(Op::Undefined, LINE),
                }
              } else {
                self.compile_expr_oast(&p.value)?;
              }
              self.emit_constant(Value::String("writable".to_string().into()), LINE);
              self.emit(Op::True, LINE);
              self.emit_constant(Value::String("enumerable".to_string().into()), LINE);
              self.emit(Op::True, LINE);
              self.emit_constant(Value::String("configurable".to_string().into()), LINE);
              self.emit(Op::True, LINE);
              self.emit(Op::BuildObject, LINE);
              self.emit_byte(4, LINE);
              self.emit(Op::Call, LINE);
              self.emit_byte(3, LINE);
              self.emit(Op::Discard, LINE);
            }
            oast::ObjectPropertyKind::ObjectProperty(p) => {
              let key_value = if p.computed {
                None
              } else {
                Some(match &p.key {
                  oast::PropertyKey::StaticIdentifier(ident) => {
                    Value::String(ident.name.to_string().into())
                  }
                  oast::PropertyKey::Identifier(ident) => {
                    Value::String(ident.name.to_string().into())
                  }
                  oast::PropertyKey::StringLiteral(s) => Value::String(s.value.to_string().into()),
                  oast::PropertyKey::NumericLiteral(n) => {
                    Value::String(super::numeric_property_key(n.value).into())
                  }
                  _ => {
                    return Err(crate::errors::CompilerError::from(
                      "unsupported accessor key",
                    ));
                  }
                })
              };

              self.emit(Op::Copy, LINE);
              self.emit_get_builtin_name("objectDefineProperty");
              self.emit(Op::Exchange, LINE);
              if let Some(key) = key_value.as_ref() {
                self.emit_constant(key.clone(), LINE);
              } else {
                let key = p
                  .key
                  .as_expression()
                  .expect("computed accessor key must be an expression");
                self.compile_expr_oast(key)?;
              }

              let descriptor_prop_name = match p.kind {
                oast::PropertyKind::Get => "get",
                oast::PropertyKind::Set => "set",
                oast::PropertyKind::Init => unreachable!(),
              };
              let method_name = key_value
                .as_ref()
                .and_then(|key| match key {
                  Value::String(key) => Some(key.to_string_lossy()),
                  _ => None,
                })
                .unwrap_or_else(|| "<accessor>".to_string());
              self.emit_constant(Value::String(descriptor_prop_name.to_string().into()), LINE);
              if let oast::Expression::FunctionExpression(f) = &p.value {
                let self_name = f.id.as_ref().map(|id| id.name.as_str());
                let (proto, _env_len) =
                  self.compile_function(&method_name, self_name, f, None, false)?;
                let idx = self.chunk.add_function(proto);
                self.emit(Op::MakeClosure, LINE);
                self.emit_byte(self.function_index_to_u8(idx)?, LINE);
              } else {
                self.compile_expr_oast(&p.value)?;
              }
              self.emit_constant(Value::String("enumerable".to_string().into()), LINE);
              self.emit(Op::True, LINE);
              self.emit_constant(Value::String("configurable".to_string().into()), LINE);
              self.emit(Op::True, LINE);
              self.emit(Op::BuildObject, LINE);
              self.emit_byte(3, LINE);
              self.emit(Op::Call, LINE);
              self.emit_byte(3, LINE);
              self.emit(Op::Discard, LINE);
            }
            oast::ObjectPropertyKind::SpreadProperty(spread) => {
              // Evaluate the spread expression before doing any copying.
              let source_slot = self.alloc_temp_local("object_spread");
              self.compile_expr_oast(&spread.argument)?;
              self.emit(Op::StoreLocal, LINE);
              self.emit_byte(self.slot_to_u8(source_slot)?, LINE);
              self.emit(Op::Copy, LINE);
              self.emit_get_global_name("Object");
              self.emit_constant(Value::String("assign".to_string().into()), LINE);
              self.emit(Op::GetProperty, LINE);
              self.emit(Op::Exchange, LINE);
              self.emit(Op::LoadLocal, LINE);
              self.emit_byte(self.slot_to_u8(source_slot)?, LINE);
              self.emit(Op::Call, LINE);
              self.emit_byte(2, LINE);
              self.emit(Op::Discard, LINE);
            }
          }
        }
      }
      OExpr::ArrayExpression(arr) => {
        let has_spread = arr
          .elements
          .iter()
          .any(|el| matches!(el, oast::ArrayExpressionElement::SpreadElement(_)));

        if has_spread {
          for el in &arr.elements {
            match el {
              oast::ArrayExpressionElement::SpreadElement(s) => {
                self.compile_expr_oast(&s.argument)?;
              }
              oast::ArrayExpressionElement::Elision(_) => {
                self.emit_constant(Value::Uninitialized, LINE);
              }
              _ => {
                if let Some(expr) = el.as_expression() {
                  self.compile_expr_oast(expr)?;
                } else {
                  self.emit_constant(Value::Uninitialized, LINE);
                }
              }
            }
          }
          self.emit(Op::BuildArray, LINE);
          self.emit_byte(self.constant_index_to_u8(arr.elements.len())?, LINE);
          for el in &arr.elements {
            if matches!(el, oast::ArrayExpressionElement::SpreadElement(_)) {
              self.emit(Op::True, LINE);
            } else {
              self.emit(Op::False, LINE);
            }
          }
          self.emit(Op::BuildArray, LINE);
          self.emit_byte(self.constant_index_to_u8(arr.elements.len())?, LINE);
          self.emit(Op::ArrayFromSpread, LINE);
          self.emit_byte(self.constant_index_to_u8(arr.elements.len())?, LINE);
        } else {
          for el in &arr.elements {
            if let Some(expr) = el.as_expression() {
              self.compile_expr_oast(expr)?;
            } else {
              self.emit_constant(Value::Uninitialized, LINE);
            }
          }
          self.emit(Op::BuildArray, LINE);
          self.emit_byte(self.constant_index_to_u8(arr.elements.len())?, LINE);
        }
      }
      OExpr::Identifier(id) => {
        let name = id.name.as_str();
        self.emit_get_binding(name);
      }
      OExpr::AssignmentExpression(a) => {
        if matches!(a.operator, oast::AssignmentOperator::Assign) {
          match &a.left {
            oast::AssignmentTarget::ArrayAssignmentTarget(target) => {
              self.compile_expr_oast(&a.right)?;
              self.emit(Op::Copy, LINE);
              self.compile_array_assignment_target(target)?;
              return Ok(());
            }
            oast::AssignmentTarget::ObjectAssignmentTarget(target) => {
              self.compile_expr_oast(&a.right)?;
              self.emit(Op::Copy, LINE);
              self.compile_object_assignment_target(target)?;
              return Ok(());
            }
            _ => {}
          }
        }
        let target = a
          .left
          .as_simple_assignment_target()
          .ok_or_else(|| crate::errors::CompilerError::from("unsupported assignment target"))?;
        match a.operator {
          oast::AssignmentOperator::Assign => match target {
            oast::SimpleAssignmentTarget::AssignmentTargetIdentifier(id) => {
              let name = id.name.as_str();
              if let Some(err) = Self::strict_binding_name_error(self.strict_context, name) {
                return Err(err);
              }
              let is_imported = self
                .current_module
                .as_ref()
                .is_some_and(|c| c.imports.contains_key(name));
              if is_imported {
                return Err(Self::imported_binding_assignment_error());
              }
              self.compile_expr_oast(&a.right)?;
              self.emit(Op::Copy, LINE);
              if let Some(slot) = self.resolve_local(name) {
                self.emit(Op::StoreLocal, LINE);
                self.emit_byte(self.slot_to_u8(slot)?, LINE);
                if self.local_mirrors_global(slot) {
                  self.emit(Op::LoadLocal, LINE);
                  self.emit_byte(self.slot_to_u8(slot)?, LINE);
                  self.emit_set_global_name(name);
                }
                // Check export membership without holding a borrow
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
            }
            oast::SimpleAssignmentTarget::StaticMemberExpression(m) => {
              if matches!(&m.object, OExpr::Super(_)) {
                self.emit_super_property_set_static(m.property.name.as_str(), &a.right)?;
              } else {
                self.compile_expr_oast(&m.object)?;
                self.emit_constant(Value::String(m.property.name.to_string().into()), LINE);
                self.compile_expr_oast(&a.right)?;
                self.emit(Op::SetProperty, LINE);
              }
            }
            oast::SimpleAssignmentTarget::ComputedMemberExpression(m) => {
              if matches!(&m.object, OExpr::Super(_)) {
                self.emit_super_property_set_computed(&m.expression, &a.right)?;
              } else {
                self.compile_expr_oast(&m.object)?;
                self.compile_expr_oast(&m.expression)?;
                self.compile_expr_oast(&a.right)?;
                self.emit(Op::SetProperty, LINE);
              }
            }
            oast::SimpleAssignmentTarget::PrivateFieldExpression(m) => {
              self.compile_expr_oast(&m.object)?;
              let key = self.ensure_private_name_constant(&m.field.name);
              self.emit_constant(key, LINE);
              self.compile_expr_oast(&a.right)?;
              self.emit(Op::SetProperty, LINE);
            }
            _ => {
              return Err(crate::errors::CompilerError::from(
                "unsupported assignment target",
              ));
            }
          },
          oast::AssignmentOperator::Addition
          | oast::AssignmentOperator::Subtraction
          | oast::AssignmentOperator::Multiplication
          | oast::AssignmentOperator::Division
          | oast::AssignmentOperator::Remainder
          | oast::AssignmentOperator::Exponential
          | oast::AssignmentOperator::BitwiseAnd
          | oast::AssignmentOperator::BitwiseOR
          | oast::AssignmentOperator::BitwiseXOR
          | oast::AssignmentOperator::ShiftLeft
          | oast::AssignmentOperator::ShiftRight
          | oast::AssignmentOperator::ShiftRightZeroFill => match target {
            oast::SimpleAssignmentTarget::AssignmentTargetIdentifier(id) => {
              let name = id.name.as_str();
              if let Some(err) = Self::strict_binding_name_error(self.strict_context, name) {
                return Err(err);
              }
              let is_imported = self
                .current_module
                .as_ref()
                .is_some_and(|c| c.imports.contains_key(name));
              if is_imported {
                return Err(Self::imported_binding_assignment_error());
              }
              self.emit_get_binding(name);
              self.compile_expr_oast(&a.right)?;
              match a.operator {
                oast::AssignmentOperator::Addition => self.emit(Op::Add, LINE),
                oast::AssignmentOperator::Subtraction => self.emit(Op::Sub, LINE),
                oast::AssignmentOperator::Multiplication => self.emit(Op::Mul, LINE),
                oast::AssignmentOperator::Division => self.emit(Op::Div, LINE),
                oast::AssignmentOperator::Remainder => self.emit(Op::Mod, LINE),
                oast::AssignmentOperator::Exponential => self.emit(Op::Pow, LINE),
                oast::AssignmentOperator::BitwiseAnd => self.emit(Op::BitAnd, LINE),
                oast::AssignmentOperator::BitwiseOR => self.emit(Op::BitOr, LINE),
                oast::AssignmentOperator::BitwiseXOR => self.emit(Op::BitXor, LINE),
                oast::AssignmentOperator::ShiftLeft => self.emit(Op::Shl, LINE),
                oast::AssignmentOperator::ShiftRight => self.emit(Op::Shr, LINE),
                oast::AssignmentOperator::ShiftRightZeroFill => self.emit(Op::Ushr, LINE),
                _ => {}
              }
              self.emit(Op::Copy, LINE);
              self.store_assignment_identifier(name)?;
            }
            oast::SimpleAssignmentTarget::StaticMemberExpression(m) => {
              if matches!(&m.object, OExpr::Super(_)) {
                self.emit(Op::GetThis, LINE);
                self.emit(Op::Copy, LINE);
                self.emit_constant(Value::String(m.property.name.to_string().into()), LINE);
                self.emit(Op::GetSuperProperty, LINE);
                self.compile_expr_oast(&a.right)?;
                self.emit_compound_assignment_op(a.operator);
                self.emit_constant(Value::String(m.property.name.to_string().into()), LINE);
                self.emit(Op::Exchange, LINE);
                self.emit(Op::SetSuperProperty, LINE);
                self.emit(Op::Exchange, LINE);
                self.emit(Op::Discard, LINE);
              } else {
                let (object_slot, key_slot) = self.compile_static_member_reference(m)?;
                self.emit_reference_get(object_slot, key_slot)?;
                self.compile_expr_oast(&a.right)?;
                self.emit_compound_assignment_op(a.operator);
                self.emit_reference_set_current_value(object_slot, key_slot)?;
              }
            }
            oast::SimpleAssignmentTarget::ComputedMemberExpression(m) => {
              if matches!(&m.object, OExpr::Super(_)) {
                let key_slot = self.alloc_temp_local("super_key");
                let receiver_slot = self.alloc_temp_local("super_receiver");
                self.emit(Op::GetThis, LINE);
                self.emit(Op::StoreLocal, LINE);
                self.emit_byte(self.slot_to_u8(receiver_slot)?, LINE);
                self.emit(Op::LoadLocal, LINE);
                self.emit_byte(self.slot_to_u8(receiver_slot)?, LINE);
                self.emit(Op::Copy, LINE);
                self.compile_expr_oast(&m.expression)?;
                self.emit(Op::StoreLocal, LINE);
                self.emit_byte(self.slot_to_u8(key_slot)?, LINE);
                self.emit(Op::LoadLocal, LINE);
                self.emit_byte(self.slot_to_u8(key_slot)?, LINE);
                self.emit(Op::GetSuperProperty, LINE);
                self.compile_expr_oast(&a.right)?;
                self.emit_compound_assignment_op(a.operator);
                self.emit(Op::LoadLocal, LINE);
                self.emit_byte(self.slot_to_u8(key_slot)?, LINE);
                self.emit(Op::Exchange, LINE);
                self.emit(Op::SetSuperProperty, LINE);
                self.emit(Op::Exchange, LINE);
                self.emit(Op::Discard, LINE);
              } else {
                let (object_slot, key_slot) = self.compile_computed_member_reference(m)?;
                self.emit_reference_get(object_slot, key_slot)?;
                self.compile_expr_oast(&a.right)?;
                self.emit_compound_assignment_op(a.operator);
                self.emit_reference_set_current_value(object_slot, key_slot)?;
              }
            }
            _ => {
              return Err(crate::errors::CompilerError::from(
                "unsupported assignment target",
              ));
            }
          },
          oast::AssignmentOperator::LogicalAnd => match target {
            oast::SimpleAssignmentTarget::AssignmentTargetIdentifier(id) => {
              let name = id.name.as_str();
              if let Some(slot) = self.resolve_local(name) {
                self.emit(Op::LoadLocal, LINE);
                self.emit_byte(self.slot_to_u8(slot)?, LINE);
                self.emit(Op::Copy, LINE);
                self.emit(Op::JumpIfFalse, LINE);
                let jump_end = self.current_ip();
                self.emit_byte(0, LINE);
                self.emit_byte(0, LINE);
                self.emit(Op::Discard, LINE);
                self.compile_expr_oast(&a.right)?;
                self.emit(Op::Copy, LINE);
                self.emit(Op::StoreLocal, LINE);
                self.emit_byte(self.slot_to_u8(slot)?, LINE);
                if self.local_mirrors_global(slot) {
                  self.emit(Op::LoadLocal, LINE);
                  self.emit_byte(self.slot_to_u8(slot)?, LINE);
                  self.emit_set_global_name(name);
                }
                self.patch_jump(jump_end);
              } else if let Some(upvalue) = self.resolve_upvalue(name) {
                self.emit(Op::GetUpvalue, LINE);
                self.emit_byte(upvalue, LINE);
                self.emit(Op::Copy, LINE);
                self.emit(Op::JumpIfFalse, LINE);
                let jump_end = self.current_ip();
                self.emit_byte(0, LINE);
                self.emit_byte(0, LINE);
                self.emit(Op::Discard, LINE);
                self.compile_expr_oast(&a.right)?;
                self.emit(Op::Copy, LINE);
                self.emit(Op::SetUpvalue, LINE);
                self.emit_byte(upvalue, LINE);
                self.patch_jump(jump_end);
              } else {
                self.emit_get_global_name(name);
                self.emit(Op::Copy, LINE);
                self.emit(Op::JumpIfFalse, LINE);
                let jump_end = self.current_ip();
                self.emit_byte(0, LINE);
                self.emit_byte(0, LINE);
                self.emit(Op::Discard, LINE);
                self.compile_expr_oast(&a.right)?;
                self.emit(Op::Copy, LINE);
                self.emit_set_global_name(name);
                self.patch_jump(jump_end);
              }
            }
            oast::SimpleAssignmentTarget::StaticMemberExpression(m) => {
              let (object_slot, key_slot) = self.compile_static_member_reference(m)?;
              self.emit_reference_get(object_slot, key_slot)?;
              self.emit(Op::Copy, LINE);
              self.emit(Op::JumpIfFalse, LINE);
              let jump_end = self.current_ip();
              self.emit_byte(0, LINE);
              self.emit_byte(0, LINE);
              self.emit(Op::Discard, LINE);
              self.compile_expr_oast(&a.right)?;
              self.emit_reference_set_current_value(object_slot, key_slot)?;
              self.patch_jump(jump_end);
            }
            oast::SimpleAssignmentTarget::ComputedMemberExpression(m) => {
              let (object_slot, key_slot) = self.compile_computed_member_reference(m)?;
              self.emit_reference_get(object_slot, key_slot)?;
              self.emit(Op::Copy, LINE);
              self.emit(Op::JumpIfFalse, LINE);
              let jump_end = self.current_ip();
              self.emit_byte(0, LINE);
              self.emit_byte(0, LINE);
              self.emit(Op::Discard, LINE);
              self.compile_expr_oast(&a.right)?;
              self.emit_reference_set_current_value(object_slot, key_slot)?;
              self.patch_jump(jump_end);
            }
            _ => {
              return Err(crate::errors::CompilerError::from(
                "unsupported assignment target",
              ));
            }
          },
          oast::AssignmentOperator::LogicalOr => match target {
            oast::SimpleAssignmentTarget::AssignmentTargetIdentifier(id) => {
              let name = id.name.as_str();
              if let Some(slot) = self.resolve_local(name) {
                self.emit(Op::LoadLocal, LINE);
                self.emit_byte(self.slot_to_u8(slot)?, LINE);
                self.emit(Op::Copy, LINE);
                self.emit(Op::JumpIfTrue, LINE);
                let jump_end = self.current_ip();
                self.emit_byte(0, LINE);
                self.emit_byte(0, LINE);
                self.emit(Op::Discard, LINE);
                self.compile_expr_oast(&a.right)?;
                self.emit(Op::Copy, LINE);
                self.emit(Op::StoreLocal, LINE);
                self.emit_byte(self.slot_to_u8(slot)?, LINE);
                if self.local_mirrors_global(slot) {
                  self.emit(Op::LoadLocal, LINE);
                  self.emit_byte(self.slot_to_u8(slot)?, LINE);
                  self.emit_set_global_name(name);
                }
                self.patch_jump(jump_end);
              } else if let Some(upvalue) = self.resolve_upvalue(name) {
                self.emit(Op::GetUpvalue, LINE);
                self.emit_byte(upvalue, LINE);
                self.emit(Op::Copy, LINE);
                self.emit(Op::JumpIfTrue, LINE);
                let jump_end = self.current_ip();
                self.emit_byte(0, LINE);
                self.emit_byte(0, LINE);
                self.emit(Op::Discard, LINE);
                self.compile_expr_oast(&a.right)?;
                self.emit(Op::Copy, LINE);
                self.emit(Op::SetUpvalue, LINE);
                self.emit_byte(upvalue, LINE);
                self.patch_jump(jump_end);
              } else {
                self.emit_get_global_name(name);
                self.emit(Op::Copy, LINE);
                self.emit(Op::JumpIfTrue, LINE);
                let jump_end = self.current_ip();
                self.emit_byte(0, LINE);
                self.emit_byte(0, LINE);
                self.emit(Op::Discard, LINE);
                self.compile_expr_oast(&a.right)?;
                self.emit(Op::Copy, LINE);
                self.emit_set_global_name(name);
                self.patch_jump(jump_end);
              }
            }
            oast::SimpleAssignmentTarget::StaticMemberExpression(m) => {
              let (object_slot, key_slot) = self.compile_static_member_reference(m)?;
              self.emit_reference_get(object_slot, key_slot)?;
              self.emit(Op::Copy, LINE);
              self.emit(Op::JumpIfTrue, LINE);
              let jump_end = self.current_ip();
              self.emit_byte(0, LINE);
              self.emit_byte(0, LINE);
              self.emit(Op::Discard, LINE);
              self.compile_expr_oast(&a.right)?;
              self.emit_reference_set_current_value(object_slot, key_slot)?;
              self.patch_jump(jump_end);
            }
            oast::SimpleAssignmentTarget::ComputedMemberExpression(m) => {
              let (object_slot, key_slot) = self.compile_computed_member_reference(m)?;
              self.emit_reference_get(object_slot, key_slot)?;
              self.emit(Op::Copy, LINE);
              self.emit(Op::JumpIfTrue, LINE);
              let jump_end = self.current_ip();
              self.emit_byte(0, LINE);
              self.emit_byte(0, LINE);
              self.emit(Op::Discard, LINE);
              self.compile_expr_oast(&a.right)?;
              self.emit_reference_set_current_value(object_slot, key_slot)?;
              self.patch_jump(jump_end);
            }
            _ => {
              return Err(crate::errors::CompilerError::from(
                "unsupported assignment target",
              ));
            }
          },
          oast::AssignmentOperator::LogicalNullish => match target {
            oast::SimpleAssignmentTarget::AssignmentTargetIdentifier(id) => {
              let name = id.name.as_str();
              if let Some(slot) = self.resolve_local(name) {
                self.emit(Op::LoadLocal, LINE);
                self.emit_byte(self.slot_to_u8(slot)?, LINE);
                self.emit(Op::Copy, LINE);
                self.emit(Op::JumpIfNullish, LINE);
                let jump_end = self.current_ip();
                self.emit_byte(0, LINE);
                self.emit_byte(0, LINE);
                self.emit(Op::Jump, LINE);
                let after_store = self.current_ip();
                self.emit_byte(0, LINE);
                self.emit_byte(0, LINE);
                self.patch_jump(jump_end);
                self.emit(Op::Discard, LINE);
                self.compile_expr_oast(&a.right)?;
                self.emit(Op::Copy, LINE);
                self.emit(Op::StoreLocal, LINE);
                self.emit_byte(self.slot_to_u8(slot)?, LINE);
                if self.local_mirrors_global(slot) {
                  self.emit(Op::LoadLocal, LINE);
                  self.emit_byte(self.slot_to_u8(slot)?, LINE);
                  self.emit_set_global_name(name);
                }
                self.patch_jump(after_store);
              } else if let Some(upvalue) = self.resolve_upvalue(name) {
                self.emit(Op::GetUpvalue, LINE);
                self.emit_byte(upvalue, LINE);
                self.emit(Op::Copy, LINE);
                self.emit(Op::JumpIfNullish, LINE);
                let jump_end = self.current_ip();
                self.emit_byte(0, LINE);
                self.emit_byte(0, LINE);
                self.emit(Op::Jump, LINE);
                let after_store = self.current_ip();
                self.emit_byte(0, LINE);
                self.emit_byte(0, LINE);
                self.patch_jump(jump_end);
                self.emit(Op::Discard, LINE);
                self.compile_expr_oast(&a.right)?;
                self.emit(Op::Copy, LINE);
                self.emit(Op::SetUpvalue, LINE);
                self.emit_byte(upvalue, LINE);
                self.patch_jump(after_store);
              } else {
                self.emit_get_global_name(name);
                self.emit(Op::Copy, LINE);
                self.emit(Op::JumpIfNullish, LINE);
                let jump_end = self.current_ip();
                self.emit_byte(0, LINE);
                self.emit_byte(0, LINE);
                self.emit(Op::Jump, LINE);
                let after_store = self.current_ip();
                self.emit_byte(0, LINE);
                self.emit_byte(0, LINE);
                self.patch_jump(jump_end);
                self.emit(Op::Discard, LINE);
                self.compile_expr_oast(&a.right)?;
                self.emit(Op::Copy, LINE);
                self.emit_set_global_name(name);
                self.patch_jump(after_store);
              }
            }
            oast::SimpleAssignmentTarget::StaticMemberExpression(m) => {
              let (object_slot, key_slot) = self.compile_static_member_reference(m)?;
              self.emit_reference_get(object_slot, key_slot)?;
              self.emit(Op::Copy, LINE);
              self.emit(Op::JumpIfNullish, LINE);
              let jump_end = self.current_ip();
              self.emit_byte(0, LINE);
              self.emit_byte(0, LINE);
              self.emit(Op::Jump, LINE);
              let after_store = self.current_ip();
              self.emit_byte(0, LINE);
              self.emit_byte(0, LINE);
              self.patch_jump(jump_end);
              self.emit(Op::Discard, LINE);
              self.compile_expr_oast(&a.right)?;
              self.emit_reference_set_current_value(object_slot, key_slot)?;
              self.patch_jump(after_store);
            }
            oast::SimpleAssignmentTarget::ComputedMemberExpression(m) => {
              let (object_slot, key_slot) = self.compile_computed_member_reference(m)?;
              self.emit_reference_get(object_slot, key_slot)?;
              self.emit(Op::Copy, LINE);
              self.emit(Op::JumpIfNullish, LINE);
              let jump_end = self.current_ip();
              self.emit_byte(0, LINE);
              self.emit_byte(0, LINE);
              self.emit(Op::Jump, LINE);
              let after_store = self.current_ip();
              self.emit_byte(0, LINE);
              self.emit_byte(0, LINE);
              self.patch_jump(jump_end);
              self.emit(Op::Discard, LINE);
              self.compile_expr_oast(&a.right)?;
              self.emit_reference_set_current_value(object_slot, key_slot)?;
              self.patch_jump(after_store);
            }
            _ => {
              return Err(crate::errors::CompilerError::from(
                "unsupported assignment target",
              ));
            }
          },
        }
      }
      OExpr::UnaryExpression(u) => match u.operator {
        oast::UnaryOperator::Typeof => {
          if let Some(name) = Self::identifier_name_if_parenthesized(&u.argument) {
            if let Some(slot) = self.resolve_local(name) {
              if self.local_mirrors_global(slot) {
                self.emit_typeof_identifier(name);
              } else {
                self.compile_expr_oast(&u.argument)?;
                self.emit(Op::Typeof, LINE);
              }
            } else if self.resolve_upvalue(name).is_some()
              || self
                .current_module
                .as_ref()
                .is_some_and(|module| module.imports.contains_key(name))
            {
              self.compile_expr_oast(&u.argument)?;
              self.emit(Op::Typeof, LINE);
            } else {
              self.emit_typeof_identifier(name);
            }
          } else {
            self.compile_expr_oast(&u.argument)?;
            self.emit(Op::Typeof, LINE);
          }
        }
        oast::UnaryOperator::Delete => {
          if let OExpr::StaticMemberExpression(m) = &u.argument {
            self.compile_expr_oast(&m.object)?;
            self.emit_constant(Value::String(m.property.name.to_string().into()), LINE);
            self.emit(Op::DeleteProperty, LINE);
          } else if let OExpr::ComputedMemberExpression(m) = &u.argument {
            self.compile_expr_oast(&m.object)?;
            self.compile_expr_oast(&m.expression)?;
            self.emit(Op::DeleteProperty, LINE);
          } else if let OExpr::Identifier(id) = &u.argument {
            if self.resolve_local(id.name.as_str()).is_some()
              || self.resolve_upvalue(id.name.as_str()).is_some()
            {
              self.emit_constant(Value::Bool(false), LINE);
            } else {
              self.emit_constant(Value::String(id.name.to_string().into()), LINE);
              self.emit(Op::DeleteIdentifier, LINE);
            }
          } else {
            self.emit_constant(Value::Bool(true), LINE);
          }
        }
        _ => {
          self.compile_expr_oast(&u.argument)?;
          match u.operator {
            oast::UnaryOperator::LogicalNot => self.emit(Op::Not, LINE),
            oast::UnaryOperator::UnaryNegation => self.emit(Op::Neg, LINE),
            oast::UnaryOperator::UnaryPlus => {
              // ToNumber rejects BigInt. Subtracting +0 applies the same
              // numeric coercion while preserving the sign of -0.
              self.emit_constant(Value::Number(0.0), LINE);
              self.emit(Op::Sub, LINE);
            }
            oast::UnaryOperator::BitwiseNot => self.emit(Op::BitNot, LINE),
            oast::UnaryOperator::Void => {
              self.emit(Op::Discard, LINE);
              self.emit(Op::Undefined, LINE);
            }
            _ => {}
          }
        }
      },
      OExpr::UpdateExpression(u) => {
        use oxc_ast::ast::SimpleAssignmentTarget as OTarget;

        match &u.argument {
          OTarget::AssignmentTargetIdentifier(id) => {
            let name = id.name.as_str();
            self.emit_get_binding(name);
            self.emit(Op::Neg, LINE);
            self.emit(Op::Neg, LINE);
            if !u.prefix {
              self.emit(Op::Copy, LINE);
            }
            self.emit_constant(Value::Number(1.0), LINE);
            match u.operator {
              oast::UpdateOperator::Increment => self.emit(Op::Add, LINE),
              oast::UpdateOperator::Decrement => self.emit(Op::Sub, LINE),
            }
            if u.prefix {
              self.emit(Op::Copy, LINE);
            }
            self.store_assignment_identifier(name)?;
          }
          OTarget::StaticMemberExpression(m) => {
            let (object_slot, key_slot) = self.compile_static_member_reference(m)?;
            self.emit_reference_get(object_slot, key_slot)?;
            let old_slot = self.alloc_temp_local("ref_old");
            self.emit(Op::StoreLocal, LINE);
            self.emit_byte(self.slot_to_u8(old_slot)?, LINE);
            self.emit(Op::LoadLocal, LINE);
            self.emit_byte(self.slot_to_u8(old_slot)?, LINE);
            self.emit(Op::Neg, LINE);
            self.emit(Op::Neg, LINE);
            if !u.prefix {
              self.emit(Op::Copy, LINE);
            }
            self.emit_constant(Value::Number(1.0), LINE);
            match u.operator {
              oast::UpdateOperator::Increment => self.emit(Op::Add, LINE),
              oast::UpdateOperator::Decrement => self.emit(Op::Sub, LINE),
            }
            self.emit_reference_set_current_value(object_slot, key_slot)?;
            if !u.prefix {
              self.emit(Op::Discard, LINE);
            }
          }
          OTarget::ComputedMemberExpression(m) => {
            let (object_slot, key_slot) = self.compile_computed_member_reference(m)?;
            self.emit_reference_get(object_slot, key_slot)?;
            let old_slot = self.alloc_temp_local("ref_old");
            self.emit(Op::StoreLocal, LINE);
            self.emit_byte(self.slot_to_u8(old_slot)?, LINE);
            self.emit(Op::LoadLocal, LINE);
            self.emit_byte(self.slot_to_u8(old_slot)?, LINE);
            self.emit(Op::Neg, LINE);
            self.emit(Op::Neg, LINE);
            if !u.prefix {
              self.emit(Op::Copy, LINE);
            }
            self.emit_constant(Value::Number(1.0), LINE);
            match u.operator {
              oast::UpdateOperator::Increment => self.emit(Op::Add, LINE),
              oast::UpdateOperator::Decrement => self.emit(Op::Sub, LINE),
            }
            self.emit_reference_set_current_value(object_slot, key_slot)?;
            if !u.prefix {
              self.emit(Op::Discard, LINE);
            }
          }
          _ => {
            return Err(crate::errors::CompilerError::from(
              "unsupported update target",
            ));
          }
        }
      }
      OExpr::YieldExpression(y) => {
        if let Some(arg) = &y.argument {
          self.compile_expr_oast(arg)?;
        } else {
          self.emit(Op::Undefined, LINE);
        }
        if y.delegate {
          self.emit(Op::YieldStar, LINE);
        } else {
          self.emit(Op::Yield, LINE);
        }
      }
      OExpr::BinaryExpression(b) => {
        if matches!(b.operator, oast::BinaryOperator::Instanceof) {
          self.compile_expr_oast(&b.left)?;
          self.compile_expr_oast(&b.right)?;
          self.emit(Op::InstanceOf, LINE);
          return Ok(());
        }
        if matches!(b.operator, oast::BinaryOperator::In) {
          self.compile_expr_oast(&b.left)?;
          let key_slot = self.alloc_reusable_temp_local("in_property_key");
          self.emit(Op::StoreLocal, LINE);
          self.emit_byte(self.slot_to_u8(key_slot)?, LINE);
          self.compile_expr_oast(&b.right)?;
          self.emit(Op::LoadLocal, LINE);
          self.emit_byte(self.slot_to_u8(key_slot)?, LINE);
          self.emit(Op::HasProperty, LINE);
          return Ok(());
        }
        let compare_cond_type = match b.operator {
          oast::BinaryOperator::Equality => Some(0),
          oast::BinaryOperator::Inequality => Some(1),
          oast::BinaryOperator::LessThan => Some(2),
          oast::BinaryOperator::LessEqualThan => Some(3),
          oast::BinaryOperator::GreaterThan => Some(4),
          oast::BinaryOperator::GreaterEqualThan => Some(5),
          oast::BinaryOperator::StrictEquality => Some(6),
          oast::BinaryOperator::StrictInequality => Some(7),
          _ => None,
        };
        if let Some(cond_type) = compare_cond_type {
          self.compile_expr_oast(&b.left)?;
          self.compile_expr_oast(&b.right)?;
          self.emit_with_payload(Op::CompareJump, cond_type, LINE);
          let jump_false = self.current_ip();
          self.emit_byte(0, LINE);
          self.emit_byte(0, LINE);
          self.emit(Op::True, LINE);
          self.emit(Op::Jump, LINE);
          let jump_end = self.current_ip();
          self.emit_byte(0, LINE);
          self.emit_byte(0, LINE);
          self.patch_jump(jump_false);
          self.emit(Op::False, LINE);
          self.patch_jump(jump_end);
          return Ok(());
        }
        self.compile_expr_oast(&b.left)?;
        self.compile_expr_oast(&b.right)?;
        match b.operator {
          oast::BinaryOperator::Addition => self.emit(Op::Add, LINE),
          oast::BinaryOperator::Subtraction => self.emit(Op::Sub, LINE),
          oast::BinaryOperator::Multiplication => self.emit(Op::Mul, LINE),
          oast::BinaryOperator::Division => self.emit(Op::Div, LINE),
          oast::BinaryOperator::Remainder => self.emit(Op::Mod, LINE),
          oast::BinaryOperator::Exponential => {
            // Lower exponentiation to a dedicated opcode
            self.emit(Op::Pow, LINE)
          }
          oast::BinaryOperator::BitwiseAnd => self.emit(Op::BitAnd, LINE),
          oast::BinaryOperator::BitwiseOR => self.emit(Op::BitOr, LINE),
          oast::BinaryOperator::BitwiseXOR => self.emit(Op::BitXor, LINE),
          oast::BinaryOperator::ShiftLeft => self.emit(Op::Shl, LINE),
          oast::BinaryOperator::ShiftRight => self.emit(Op::Shr, LINE),
          oast::BinaryOperator::ShiftRightZeroFill => self.emit(Op::Ushr, LINE),
          _ => {
            return Err(crate::errors::CompilerError::from(
              "unsupported binary operator",
            ));
          }
        }
      }
      OExpr::LogicalExpression(l) => match l.operator {
        oast::LogicalOperator::And => {
          self.compile_expr_oast(&l.left)?;
          self.emit(Op::Copy, LINE);
          self.emit(Op::JumpIfFalse, LINE);
          let jump_end = self.current_ip();
          self.emit_byte(0, LINE);
          self.emit_byte(0, LINE);
          self.emit(Op::Discard, LINE);
          self.compile_expr_oast(&l.right)?;
          self.patch_jump(jump_end);
        }
        oast::LogicalOperator::Or => {
          self.compile_expr_oast(&l.left)?;
          self.emit(Op::Copy, LINE);
          self.emit(Op::JumpIfTrue, LINE);
          let jump_end = self.current_ip();
          self.emit_byte(0, LINE);
          self.emit_byte(0, LINE);
          self.emit(Op::Discard, LINE);
          self.compile_expr_oast(&l.right)?;
          self.patch_jump(jump_end);
        }
        oast::LogicalOperator::Coalesce => {
          self.compile_expr_oast(&l.left)?;
          self.emit(Op::Copy, LINE);
          self.emit(Op::JumpIfNullish, LINE);
          let jump_right = self.current_ip();
          self.emit_byte(0, LINE);
          self.emit_byte(0, LINE);
          self.emit(Op::Jump, LINE);
          let jump_end = self.current_ip();
          self.emit_byte(0, LINE);
          self.emit_byte(0, LINE);
          self.patch_jump(jump_right);
          self.emit(Op::Discard, LINE);
          self.compile_expr_oast(&l.right)?;
          self.patch_jump(jump_end);
        }
      },
      OExpr::ConditionalExpression(c) => {
        if let Some(jump_else) = self.try_emit_compare_jump_if_false(&c.test)? {
          self.compile_expr_oast(&c.consequent)?;
          self.emit(Op::Jump, LINE);
          let jump_end = self.current_ip();
          self.emit_byte(0, LINE);
          self.emit_byte(0, LINE);
          self.patch_jump(jump_else);
          self.compile_expr_oast(&c.alternate)?;
          self.patch_jump(jump_end);
        } else {
          self.compile_expr_oast(&c.test)?;
          self.emit(Op::JumpIfFalse, LINE);
          let jump_else = self.current_ip();
          self.emit_byte(0, LINE);
          self.emit_byte(0, LINE);
          self.compile_expr_oast(&c.consequent)?;
          self.emit(Op::Jump, LINE);
          let jump_end = self.current_ip();
          self.emit_byte(0, LINE);
          self.emit_byte(0, LINE);
          self.patch_jump(jump_else);
          self.compile_expr_oast(&c.alternate)?;
          self.patch_jump(jump_end);
        }
      }
      OExpr::SequenceExpression(seq) => {
        let len = seq.expressions.len();
        for (idx, expr) in seq.expressions.iter().enumerate() {
          self.compile_expr_oast(expr)?;
          if idx + 1 != len {
            self.emit(Op::Discard, LINE);
          }
        }
        if len == 0 {
          self.emit(Op::Undefined, LINE);
        }
      }
      OExpr::StaticMemberExpression(m) => {
        if matches!(&m.object, OExpr::Super(_)) {
          if m.optional {
            return Err(crate::errors::CompilerError::from(
              "optional chaining with super is not supported",
            ));
          }
          self.emit_super_property_get_static(m.property.name.as_str(), false)?;
        } else {
          self.compile_expr_oast(&m.object)?;
          if m.optional {
            self.emit(Op::Copy, LINE);
            self.emit(Op::JumpIfNullish, LINE);
            let nullish = self.current_ip();
            self.emit_jump_placeholder();
            self.emit_constant(Value::String(m.property.name.to_string().into()), LINE);
            self.emit(Op::GetProperty, LINE);
            self.emit(Op::Jump, LINE);
            let end = self.current_ip();
            self.emit_jump_placeholder();
            self.patch_jump(nullish);
            self.emit(Op::Discard, LINE);
            self.emit(Op::Undefined, LINE);
            self.patch_jump(end);
          } else {
            self.emit_constant(Value::String(m.property.name.to_string().into()), LINE);
            self.emit(Op::GetProperty, LINE);
          }
        }
      }
      OExpr::ComputedMemberExpression(m) => {
        if matches!(&m.object, OExpr::Super(_)) {
          if m.optional {
            return Err(crate::errors::CompilerError::from(
              "optional chaining with super is not supported",
            ));
          }
          self.emit_super_property_get_computed(&m.expression, false)?;
        } else {
          self.compile_expr_oast(&m.object)?;
          if m.optional {
            self.emit(Op::Copy, LINE);
            self.emit(Op::JumpIfNullish, LINE);
            let nullish = self.current_ip();
            self.emit_jump_placeholder();
            self.compile_expr_oast(&m.expression)?;
            self.emit(Op::GetProperty, LINE);
            self.emit(Op::Jump, LINE);
            let end = self.current_ip();
            self.emit_jump_placeholder();
            self.patch_jump(nullish);
            self.emit(Op::Discard, LINE);
            self.emit(Op::Undefined, LINE);
            self.patch_jump(end);
          } else {
            self.compile_expr_oast(&m.expression)?;
            self.emit(Op::GetProperty, LINE);
          }
        }
      }
      OExpr::PrivateFieldExpression(m) => {
        self.compile_expr_oast(&m.object)?;
        if m.optional {
          self.emit(Op::Copy, LINE);
          self.emit(Op::JumpIfNullish, LINE);
          let nullish = self.current_ip();
          self.emit_jump_placeholder();
          let key = self.ensure_private_name_constant(&m.field.name);
          self.emit_constant(key, LINE);
          self.emit(Op::GetProperty, LINE);
          self.emit(Op::Jump, LINE);
          let end = self.current_ip();
          self.emit_jump_placeholder();
          self.patch_jump(nullish);
          self.emit(Op::Discard, LINE);
          self.emit(Op::Undefined, LINE);
          self.patch_jump(end);
        } else {
          let key = self.ensure_private_name_constant(&m.field.name);
          self.emit_constant(key, LINE);
          self.emit(Op::GetProperty, LINE);
        }
      }
      OExpr::ChainExpression(c) => {
        use oast::ChainElement as CE;
        match &c.expression {
          CE::StaticMemberExpression(m) => {
            if matches!(&m.object, OExpr::Super(_)) {
              if m.optional {
                return Err(crate::errors::CompilerError::from(
                  "optional chaining with super is not supported",
                ));
              }
              self.emit_super_property_get_static(m.property.name.as_str(), false)?;
            } else {
              self.compile_expr_oast(&m.object)?;
              if m.optional {
                self.emit(Op::Copy, LINE);
                self.emit(Op::JumpIfNullish, LINE);
                let nullish = self.current_ip();
                self.emit_jump_placeholder();
                self.emit_constant(Value::String(m.property.name.to_string().into()), LINE);
                self.emit(Op::GetProperty, LINE);
                self.emit(Op::Jump, LINE);
                let end = self.current_ip();
                self.emit_jump_placeholder();
                self.patch_jump(nullish);
                self.emit(Op::Discard, LINE);
                self.emit(Op::Undefined, LINE);
                self.patch_jump(end);
              } else {
                self.emit_constant(Value::String(m.property.name.to_string().into()), LINE);
                self.emit(Op::GetProperty, LINE);
              }
            }
          }
          CE::ComputedMemberExpression(m) => {
            if matches!(&m.object, OExpr::Super(_)) {
              if m.optional {
                return Err(crate::errors::CompilerError::from(
                  "optional chaining with super is not supported",
                ));
              }
              self.emit_super_property_get_computed(&m.expression, false)?;
            } else {
              self.compile_expr_oast(&m.object)?;
              if m.optional {
                self.emit(Op::Copy, LINE);
                self.emit(Op::JumpIfNullish, LINE);
                let nullish = self.current_ip();
                self.emit_jump_placeholder();
                self.compile_expr_oast(&m.expression)?;
                self.emit(Op::GetProperty, LINE);
                self.emit(Op::Jump, LINE);
                let end = self.current_ip();
                self.emit_jump_placeholder();
                self.patch_jump(nullish);
                self.emit(Op::Discard, LINE);
                self.emit(Op::Undefined, LINE);
                self.patch_jump(end);
              } else {
                self.compile_expr_oast(&m.expression)?;
                self.emit(Op::GetProperty, LINE);
              }
            }
          }
          CE::CallExpression(c) => {
            if self.compile_spread_call_tail(c)? {
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
              self.emit_byte(self.constant_index_to_u8(arg_count)?, LINE);
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
              self.emit(Op::Call, LINE);
              self.emit_byte(self.constant_index_to_u8(arg_count)?, LINE);
              return Ok(());
            }
            if let OExpr::StaticMemberExpression(m) = &c.callee
              && c.optional
              && !m.optional
              && !matches!(&m.object, OExpr::Super(_))
            {
              self.compile_expr_oast(&m.object)?;
              self.emit(Op::Copy, LINE);
              self.emit_constant(Value::String(m.property.name.to_string().into()), LINE);
              self.emit(Op::GetProperty, LINE);
              self.emit(Op::Copy, LINE);
              self.emit(Op::JumpIfNullish, LINE);
              let nullish = self.current_ip();
              self.emit_jump_placeholder();
              let arg_count = self.compile_call_arguments(&c.arguments)?;
              self.emit_call_with_arg_count(Op::CallBuiltin, arg_count)?;
              self.emit(Op::Jump, LINE);
              let end = self.current_ip();
              self.emit_jump_placeholder();
              self.patch_jump(nullish);
              self.emit(Op::Discard, LINE);
              self.emit(Op::Discard, LINE);
              self.emit(Op::Undefined, LINE);
              self.patch_jump(end);
              return Ok(());
            }
            if let OExpr::StaticMemberExpression(m) = &c.callee
              && m.optional
              && !matches!(&m.object, OExpr::Super(_))
            {
              self.compile_expr_oast(&m.object)?;
              self.emit(Op::Copy, LINE);
              self.emit(Op::JumpIfNullish, LINE);
              let object_nullish = self.current_ip();
              self.emit_jump_placeholder();
              self.emit(Op::Copy, LINE);
              self.emit_constant(Value::String(m.property.name.to_string().into()), LINE);
              self.emit(Op::GetProperty, LINE);
              let callee_nullish = if c.optional {
                self.emit(Op::Copy, LINE);
                self.emit(Op::JumpIfNullish, LINE);
                let patch = self.current_ip();
                self.emit_jump_placeholder();
                Some(patch)
              } else {
                None
              };
              let arg_count = self.compile_call_arguments(&c.arguments)?;
              self.emit_call_with_arg_count(Op::CallBuiltin, arg_count)?;
              self.emit(Op::Jump, LINE);
              let normal_end = self.current_ip();
              self.emit_jump_placeholder();
              let callee_end = if let Some(callee_nullish) = callee_nullish {
                self.patch_jump(callee_nullish);
                self.emit(Op::Discard, LINE);
                self.emit(Op::Discard, LINE);
                self.emit(Op::Undefined, LINE);
                self.emit(Op::Jump, LINE);
                let patch = self.current_ip();
                self.emit_jump_placeholder();
                Some(patch)
              } else {
                None
              };
              self.patch_jump(object_nullish);
              self.emit(Op::Discard, LINE);
              self.emit(Op::Undefined, LINE);
              self.patch_jump(normal_end);
              if let Some(callee_end) = callee_end {
                self.patch_jump(callee_end);
              }
              return Ok(());
            }
            if let OExpr::ComputedMemberExpression(m) = &c.callee
              && m.optional
              && !matches!(&m.object, OExpr::Super(_))
            {
              self.compile_expr_oast(&m.object)?;
              self.emit(Op::Copy, LINE);
              self.emit(Op::JumpIfNullish, LINE);
              let object_nullish = self.current_ip();
              self.emit_jump_placeholder();
              self.emit(Op::Copy, LINE);
              self.compile_expr_oast(&m.expression)?;
              self.emit(Op::GetProperty, LINE);
              let callee_nullish = if c.optional {
                self.emit(Op::Copy, LINE);
                self.emit(Op::JumpIfNullish, LINE);
                let patch = self.current_ip();
                self.emit_jump_placeholder();
                Some(patch)
              } else {
                None
              };
              let arg_count = self.compile_call_arguments(&c.arguments)?;
              self.emit_call_with_arg_count(Op::CallBuiltin, arg_count)?;
              self.emit(Op::Jump, LINE);
              let normal_end = self.current_ip();
              self.emit_jump_placeholder();
              let callee_end = if let Some(callee_nullish) = callee_nullish {
                self.patch_jump(callee_nullish);
                self.emit(Op::Discard, LINE);
                self.emit(Op::Discard, LINE);
                self.emit(Op::Undefined, LINE);
                self.emit(Op::Jump, LINE);
                let patch = self.current_ip();
                self.emit_jump_placeholder();
                Some(patch)
              } else {
                None
              };
              self.patch_jump(object_nullish);
              self.emit(Op::Discard, LINE);
              self.emit(Op::Undefined, LINE);
              self.patch_jump(normal_end);
              if let Some(callee_end) = callee_end {
                self.patch_jump(callee_end);
              }
              return Ok(());
            }
            let is_optional_call = c.optional;
            // Recursively compile the callee (which may be a member expression)
            self.compile_expr_oast(&c.callee)?;
            let nullish = if is_optional_call {
              self.emit(Op::Copy, LINE);
              self.emit(Op::JumpIfNullish, LINE);
              let nullish = self.current_ip();
              self.emit_jump_placeholder();
              Some(nullish)
            } else {
              None
            };
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
            self.emit_byte(self.constant_index_to_u8(arg_count)?, LINE);
            if let Some(nullish) = nullish {
              self.emit(Op::Jump, LINE);
              let end = self.current_ip();
              self.emit_jump_placeholder();
              self.patch_jump(nullish);
              self.emit(Op::Discard, LINE);
              self.emit(Op::Undefined, LINE);
              self.patch_jump(end);
            }
          }
          CE::PrivateFieldExpression(m) => {
            self.compile_expr_oast(&m.object)?;
            if m.optional {
              self.emit(Op::Copy, LINE);
              self.emit(Op::JumpIfNullish, LINE);
              let nullish = self.current_ip();
              self.emit_jump_placeholder();
              let key = self.ensure_private_name_constant(&m.field.name);
              self.emit_constant(key, LINE);
              self.emit(Op::GetProperty, LINE);
              self.emit(Op::Jump, LINE);
              let end = self.current_ip();
              self.emit_jump_placeholder();
              self.patch_jump(nullish);
              self.emit(Op::Discard, LINE);
              self.emit(Op::Undefined, LINE);
              self.patch_jump(end);
            } else {
              let key = self.ensure_private_name_constant(&m.field.name);
              self.emit_constant(key, LINE);
              self.emit(Op::GetProperty, LINE);
            }
          }
          CE::TSNonNullExpression(n) => {
            // Just compile the inner expression, as we don't have non-null assertions
            self.compile_expr_oast(&n.expression)?;
          }
        }
      }
      OExpr::Super(_s) => {
        if !self.allow_super {
          return Err(crate::errors::CompilerError::from(
            "SyntaxError: super used outside of a method",
          ));
        }
        return Err(crate::errors::CompilerError::from(
          "super can only be used in property accesses or calls",
        ));
      }
      OExpr::NewTarget(_) => {
        if !self.allow_new_target {
          return Err(crate::errors::CompilerError::from(
            "SyntaxError: new.target is only valid inside a function",
          ));
        }
        self.emit(Op::GetNewTarget, LINE);
      }
      OExpr::ImportMeta(_) => {
        if self.current_module.is_some() {
          self.emit_get_builtin_name("importMeta");
          self.emit(Op::Call, LINE);
          self.emit_byte(0, LINE);
        } else {
          self.emit(Op::Undefined, LINE);
        }
      }
      OExpr::CallExpression(c) => {
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

        if let OExpr::StaticMemberExpression(m) = &c.callee
          && m.optional
          && !matches!(&m.object, OExpr::Super(_))
        {
          self.compile_expr_oast(&m.object)?;
          self.emit(Op::Copy, LINE);
          self.emit(Op::JumpIfNullish, LINE);
          let nullish = self.current_ip();
          self.emit_jump_placeholder();
          self.emit(Op::Copy, LINE);
          self.emit_constant(Value::String(m.property.name.to_string().into()), LINE);
          self.emit(Op::GetProperty, LINE);
          let arg_count = self.compile_call_arguments(&c.arguments)?;
          self.emit_call_with_arg_count(Op::CallBuiltin, arg_count)?;
          self.emit(Op::Jump, LINE);
          let end = self.current_ip();
          self.emit_jump_placeholder();
          self.patch_jump(nullish);
          self.emit(Op::Discard, LINE);
          self.emit(Op::Undefined, LINE);
          self.patch_jump(end);
          return Ok(());
        }

        if let OExpr::StaticMemberExpression(m) = &c.callee {
          let _is_optional_call = c.optional;

          if m.optional {
            self.compile_optional_member_access(&m.object, |this: &mut Self| {
              this.emit(Op::Copy, LINE);
              this.emit_constant(Value::String(m.property.name.to_string().into()), LINE);
              this.emit(Op::GetProperty, LINE);
              Ok(())
            })?;
          } else {
            self.compile_expr_oast(&m.object)?;
            let receiver_slot = self.alloc_temp_local("call_receiver");
            self.emit(Op::StoreLocal, LINE);
            self.emit_byte(self.slot_to_u8(receiver_slot)?, LINE);
            self.emit(Op::LoadLocal, LINE);
            self.emit_byte(self.slot_to_u8(receiver_slot)?, LINE);
            self.emit(Op::LoadLocal, LINE);
            self.emit_byte(self.slot_to_u8(receiver_slot)?, LINE);
            self.emit_constant(Value::String(m.property.name.to_string().into()), LINE);
            self.emit(Op::GetProperty, LINE);
          }

          let arg_count = self.compile_call_arguments(&c.arguments)?;
          self.emit_call_with_arg_count(Op::CallBuiltin, arg_count)?;
          return Ok(());
        }

        if let OExpr::ComputedMemberExpression(m) = &c.callee
          && m.optional
          && !matches!(&m.object, OExpr::Super(_))
        {
          self.compile_expr_oast(&m.object)?;
          self.emit(Op::Copy, LINE);
          self.emit(Op::JumpIfNullish, LINE);
          let nullish = self.current_ip();
          self.emit_jump_placeholder();
          self.emit(Op::Copy, LINE);
          self.compile_expr_oast(&m.expression)?;
          self.emit(Op::GetProperty, LINE);
          let arg_count = self.compile_call_arguments(&c.arguments)?;
          self.emit_call_with_arg_count(Op::CallBuiltin, arg_count)?;
          self.emit(Op::Jump, LINE);
          let end = self.current_ip();
          self.emit_jump_placeholder();
          self.patch_jump(nullish);
          self.emit(Op::Discard, LINE);
          self.emit(Op::Undefined, LINE);
          self.patch_jump(end);
          return Ok(());
        }

        if let OExpr::ComputedMemberExpression(m) = &c.callee {
          let _is_optional_call = c.optional;

          if m.optional {
            self.compile_optional_member_access(&m.object, |this: &mut Self| {
              this.emit(Op::Copy, LINE);
              this.compile_expr_oast(&m.expression)?;
              this.emit(Op::GetProperty, LINE);
              Ok(())
            })?;
          } else {
            self.compile_expr_oast(&m.object)?;
            self.compile_expr_oast(&m.expression)?;
            let key_slot = self.alloc_temp_local("call_key");
            self.emit(Op::StoreLocal, LINE);
            self.emit_byte(self.slot_to_u8(key_slot)?, LINE);
            let receiver_slot = self.alloc_temp_local("call_receiver");
            self.emit(Op::StoreLocal, LINE);
            self.emit_byte(self.slot_to_u8(receiver_slot)?, LINE);
            self.emit(Op::LoadLocal, LINE);
            self.emit_byte(self.slot_to_u8(receiver_slot)?, LINE);
            self.emit(Op::LoadLocal, LINE);
            self.emit_byte(self.slot_to_u8(receiver_slot)?, LINE);
            self.emit(Op::LoadLocal, LINE);
            self.emit_byte(self.slot_to_u8(key_slot)?, LINE);
            self.emit(Op::GetProperty, LINE);
          }

          let arg_count = self.compile_call_arguments(&c.arguments)?;
          self.emit_call_with_arg_count(Op::CallBuiltin, arg_count)?;
          return Ok(());
        }

        if let OExpr::PrivateFieldExpression(m) = &c.callee {
          let _is_optional_call = c.optional;

          if m.optional {
            self.compile_optional_member_access(&m.object, |this: &mut Self| {
              this.emit(Op::Copy, LINE);
              let key = this.ensure_private_name_constant(&m.field.name);
              this.emit_constant(key, LINE);
              this.emit(Op::GetProperty, LINE);
              Ok(())
            })?;
          } else {
            self.compile_expr_oast(&m.object)?;
            let receiver_slot = self.alloc_temp_local("call_receiver");
            self.emit(Op::StoreLocal, LINE);
            self.emit_byte(self.slot_to_u8(receiver_slot)?, LINE);
            self.emit(Op::LoadLocal, LINE);
            self.emit_byte(self.slot_to_u8(receiver_slot)?, LINE);
            self.emit(Op::LoadLocal, LINE);
            self.emit_byte(self.slot_to_u8(receiver_slot)?, LINE);
            let key = self.ensure_private_name_constant(&m.field.name);
            self.emit_constant(key, LINE);
            self.emit(Op::GetProperty, LINE);
          }

          let arg_count = self.compile_call_arguments(&c.arguments)?;
          self.emit_call_with_arg_count(Op::CallBuiltin, arg_count)?;
          return Ok(());
        }

        if c.optional {
          self.compile_expr_oast(&c.callee)?;
          self.emit(Op::Copy, LINE);
          self.emit(Op::JumpIfNullish, LINE);
          let nullish = self.current_ip();
          self.emit_jump_placeholder();
          let arg_count = self.compile_call_arguments(&c.arguments)?;
          self.emit_call_with_arg_count(Op::Call, arg_count)?;
          self.emit(Op::Jump, LINE);
          let end = self.current_ip();
          self.emit_jump_placeholder();
          self.patch_jump(nullish);
          self.emit(Op::Discard, LINE);
          self.emit(Op::Undefined, LINE);
          self.patch_jump(end);
          return Ok(());
        }

        self.compile_expr_oast(&c.callee)?;
        let arg_count = self.compile_call_arguments(&c.arguments)?;
        self.emit_call_with_arg_count(Op::Call, arg_count)?;
      }
      OExpr::NewExpression(n) => {
        self.compile_expr_oast(&n.callee)?;
        let arg_count = self.compile_call_arguments(&n.arguments)?;
        self.emit_call_with_arg_count(Op::Construct, arg_count)?;
      }
      OExpr::ImportExpression(i) => {
        self.emit_get_builtin_name("dynamicImport");
        self.compile_expr_oast(&i.source)?;
        if let Some(options) = &i.options {
          self.compile_expr_oast(options)?;
        } else {
          self.emit(Op::Undefined, LINE);
        }
        self.emit(Op::Call, LINE);
        self.emit_byte(2, LINE);
      }
      OExpr::RegExpLiteral(re) => {
        self.emit_get_global_name("RegExp");
        self.emit_constant(
          Value::String(re.regex.pattern.text.to_string().into()),
          LINE,
        );
        self.emit_constant(Value::String(re.regex.flags.to_string().into()), LINE);
        self.emit(Op::Call, LINE);
        self.emit_byte(2, LINE);
      }
      OExpr::ParenthesizedExpression(p) => {
        self.compile_expr_oast(&p.expression)?;
      }
      OExpr::ClassExpression(c) => {
        self.emit_class_from_oast(c, None)?;
      }
      OExpr::AwaitExpression(a) => {
        self.compile_expr_oast(&a.argument)?;
        self.emit(Op::AwaitPromise, LINE);
      }
      OExpr::TSAsExpression(a) => {
        self.compile_expr_oast(&a.expression)?;
      }
      OExpr::TSNonNullExpression(n) => {
        self.compile_expr_oast(&n.expression)?;
      }
      _ => self.emit(Op::Undefined, LINE),
    }
    Ok(())
  }
}
