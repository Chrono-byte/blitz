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
//! instructions that are finalized as bytecode v2.

use super::emit::LoweringOpcode;
use crate::value::Value;
use oxc_ast::ast as oast;

const LINE: u32 = 1;

fn string_literal_value(source: &str) -> Option<js_string::JsString> {
  let bytes = source.as_bytes();
  if bytes.len() < 2 || !matches!(bytes[0], b'\'' | b'"') || bytes[bytes.len() - 1] != bytes[0] {
    return None;
  }

  let content = &source[1..source.len() - 1];
  let bytes = content.as_bytes();
  let mut units = Vec::with_capacity(content.len());
  let mut cursor = 0;
  while cursor < bytes.len() {
    if bytes[cursor] != b'\\' {
      let character = content[cursor..].chars().next()?;
      let mut encoded = [0; 2];
      units.extend_from_slice(character.encode_utf16(&mut encoded));
      cursor += character.len_utf8();
      continue;
    }

    cursor += 1;
    let escape = *bytes.get(cursor)?;
    cursor += 1;
    match escape {
      b'b' => units.push(0x0008),
      b'f' => units.push(0x000C),
      b'n' => units.push(0x000A),
      b'r' => units.push(0x000D),
      b't' => units.push(0x0009),
      b'v' => units.push(0x000B),
      b'\n' => {}
      b'\r' => {
        if bytes.get(cursor) == Some(&b'\n') {
          cursor += 1;
        }
      }
      b'x' => {
        let end = cursor.checked_add(2)?;
        let value =
          u16::from_str_radix(std::str::from_utf8(bytes.get(cursor..end)?).ok()?, 16).ok()?;
        units.push(value);
        cursor = end;
      }
      b'u' if bytes.get(cursor) == Some(&b'{') => {
        cursor += 1;
        let end = bytes[cursor..]
          .iter()
          .position(|byte| *byte == b'}')?
          + cursor;
        let value = u32::from_str_radix(std::str::from_utf8(&bytes[cursor..end]).ok()?, 16).ok()?;
        let character = char::from_u32(value)?;
        let mut encoded = [0; 2];
        units.extend_from_slice(character.encode_utf16(&mut encoded));
        cursor = end + 1;
      }
      b'u' => {
        let end = cursor.checked_add(4)?;
        let value =
          u16::from_str_radix(std::str::from_utf8(bytes.get(cursor..end)?).ok()?, 16).ok()?;
        units.push(value);
        cursor = end;
      }
      first @ b'0'..=b'7' => {
        let maximum = if first <= b'3' { 3 } else { 2 };
        let mut value = u16::from(first - b'0');
        let mut digits = 1;
        while digits < maximum
          && let Some(next @ b'0'..=b'7') = bytes.get(cursor).copied()
        {
          value = value * 8 + u16::from(next - b'0');
          cursor += 1;
          digits += 1;
        }
        units.push(value);
      }
      _ => {
        let character = content[cursor - 1..].chars().next()?;
        let mut encoded = [0; 2];
        units.extend_from_slice(character.encode_utf16(&mut encoded));
        cursor += character.len_utf8() - 1;
      }
    }
  }
  Some(js_string::JsString::from_utf16(units))
}

impl super::Compiler {
  /// Compile an expression from the AST into bytecode.
  ///
  /// This is the main entry point for expression compilation. It recursively
  /// processes expressions and emits appropriate bytecode operations to the
  /// current code.
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
        let parsed = if let Some(raw) = &n.raw {
          let s = raw.as_str();
          let digits_str = s.strip_suffix('n').unwrap_or(s).replace('_', "");
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
        let source = &self.source_text[s.span.start as usize..s.span.end as usize];
        let value = string_literal_value(source)
          .unwrap_or_else(|| js_string::JsString::new(s.value.as_str()));
        self.emit_constant(Value::String(value), LINE)
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
          self.emit_get_builtin_name("toString");
          self.compile_expr_oast(expr)?;
          self.emit(LoweringOpcode::Call, LINE);
          self.emit_byte(1, LINE);
          self.emit(LoweringOpcode::Add, LINE);
          let tail = t
            .quasis
            .get(i + 1)
            .and_then(|q| q.value.cooked.as_ref())
            .map(ToString::to_string)
            .unwrap_or_default();
          self.emit_constant(Value::String(tail.into()), LINE);
          self.emit(LoweringOpcode::Add, LINE);
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
        self.emit(LoweringOpcode::BuildArray, LINE);
        self.emit_byte(
          u8::try_from(t.quasi.quasis.len()).expect("quasis.len fits in u8"),
          LINE,
        );
        self.emit(LoweringOpcode::Copy, LINE);
        self.emit_constant(Value::String("raw".to_string().into()), LINE);
        for quasi in &t.quasi.quasis {
          self.emit_constant(Value::String(quasi.value.raw.to_string().into()), LINE);
        }
        self.emit(LoweringOpcode::BuildArray, LINE);
        self.emit_byte(
          u8::try_from(t.quasi.quasis.len()).expect("quasis.len fits in u8"),
          LINE,
        );
        self.emit(LoweringOpcode::SetProperty, LINE);
        self.emit(LoweringOpcode::Discard, LINE);
        let mut arg_count = 1usize;
        for sub in &t.quasi.expressions {
          self.compile_expr_oast(sub)?;
          arg_count += 1;
        }
        self.emit(LoweringOpcode::Call, LINE);
        self.emit_byte(u8::try_from(arg_count).expect("arg_count fits in u8"), LINE);
      }
      OExpr::BooleanLiteral(b) => {
        if b.value {
          self.emit(LoweringOpcode::True, LINE)
        } else {
          self.emit(LoweringOpcode::False, LINE);
        }
      }
      OExpr::NullLiteral(_) => self.emit(LoweringOpcode::Null, LINE),
      OExpr::ThisExpression(_) => {
        self.emit(LoweringOpcode::GetThis, LINE);
      }
      OExpr::ArrowFunctionExpression(a) => {
        let name = "<arrow>";
        let (proto, _env_len) = self.compile_arrow_function(name, a, None)?;
        let idx = self.chunk.add_function(proto);
        self.emit(LoweringOpcode::MakeClosure, LINE);
        self.emit_function_index(idx, LINE);
      }
      OExpr::FunctionExpression(f) => {
        let self_name = f.id.as_ref().map(|id| id.name.as_str());
        let name = self_name.unwrap_or("<anonymous>");
        let (proto, _env_len) = self.compile_function(name, self_name, f, None, false)?;
        let idx = self.chunk.add_function(proto);
        self.emit(LoweringOpcode::MakeClosure, LINE);
        self.emit_function_index(idx, LINE);
      }
      OExpr::ObjectExpression(obj) => {
        self.emit(LoweringOpcode::BuildObject, LINE);
        self.emit_byte(0, LINE);
        let home_object_slot = self.alloc_temp_local("object_literal_home");
        self.emit(LoweringOpcode::Copy, LINE);
        self.emit(LoweringOpcode::StoreLocal, LINE);
        self.emit_slot(home_object_slot, LINE);

        for prop in &obj.properties {
          match prop {
            oast::ObjectPropertyKind::ObjectProperty(p)
              if matches!(p.kind, oast::PropertyKind::Init) =>
            {
              let is_proto_setter = !p.computed
                && !p.method
                && match &p.key {
                  oast::PropertyKey::StaticIdentifier(identifier) => identifier.name == "__proto__",
                  oast::PropertyKey::Identifier(identifier) => identifier.name == "__proto__",
                  oast::PropertyKey::StringLiteral(literal) => literal.value == "__proto__",
                  _ => false,
                };
              if is_proto_setter {
                self.emit_get_builtin_name("objectSetPrototypeOf");
                self.emit(LoweringOpcode::LoadLocal, LINE);
                self.emit_slot(home_object_slot, LINE);
                self.compile_expr_oast(&p.value)?;
                self.emit(LoweringOpcode::Call, LINE);
                self.emit_byte(2, LINE);
                self.emit(LoweringOpcode::Discard, LINE);
                continue;
              }
              let assign_function_name = Self::is_anonymous_function_definition(&p.value);
              let property_key_slot =
                assign_function_name.then(|| self.alloc_temp_local("object_literal_property_key"));
              self.emit(LoweringOpcode::Copy, LINE);
              self.emit_get_builtin_name("objectDefineProperty");
              self.emit(LoweringOpcode::Exchange, LINE);

              if p.computed {
                let key = p
                  .key
                  .as_expression()
                  .expect("computed property key must be an expression");
                self.emit_get_builtin_name("toPropertyKey");
                self.compile_expr_oast(key)?;
                self.emit(LoweringOpcode::Call, LINE);
                self.emit_byte(1, LINE);
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
              if let Some(key_slot) = property_key_slot {
                self.emit(LoweringOpcode::StoreLocal, LINE);
                self.emit_slot(key_slot, LINE);
                self.emit(LoweringOpcode::LoadLocal, LINE);
                self.emit_slot(key_slot, LINE);
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
                  _ => self.emit(LoweringOpcode::Undefined, LINE),
                }
              } else if p.method
                && let oast::Expression::FunctionExpression(function) = &p.value
              {
                let method_name = if p.computed {
                  "<method>".to_string()
                } else {
                  match &p.key {
                    oast::PropertyKey::StaticIdentifier(identifier) => identifier.name.to_string(),
                    oast::PropertyKey::Identifier(identifier) => identifier.name.to_string(),
                    oast::PropertyKey::StringLiteral(string) => string.value.to_string(),
                    oast::PropertyKey::NumericLiteral(number) => {
                      super::numeric_property_key(number.value)
                    }
                    _ => "<method>".to_string(),
                  }
                };
                let (mut proto, _env_len) =
                  self.compile_function(&method_name, None, function, None, true)?;
                proto.has_prototype_property = proto.is_generator;
                proto.is_constructor = false;
                let index = self.chunk.add_function(proto);
                self.emit(LoweringOpcode::MakeClosure, LINE);
                self.emit_function_index(index, LINE);
                self.emit_set_method_home_object(home_object_slot)?;
              } else {
                self.compile_expr_oast(&p.value)?;
              }
              if let Some(key_slot) = property_key_slot {
                let value_slot = self.alloc_temp_local("object_literal_property_value");
                self.emit(LoweringOpcode::StoreLocal, LINE);
                self.emit_slot(value_slot, LINE);
                self.emit_get_builtin_name("setFunctionNameIfAnonymous");
                self.emit(LoweringOpcode::LoadLocal, LINE);
                self.emit_slot(value_slot, LINE);
                self.emit(LoweringOpcode::LoadLocal, LINE);
                self.emit_slot(key_slot, LINE);
                self.emit(LoweringOpcode::Call, LINE);
                self.emit_byte(2, LINE);
              }
              self.emit_constant(Value::String("writable".to_string().into()), LINE);
              self.emit(LoweringOpcode::True, LINE);
              self.emit_constant(Value::String("enumerable".to_string().into()), LINE);
              self.emit(LoweringOpcode::True, LINE);
              self.emit_constant(Value::String("configurable".to_string().into()), LINE);
              self.emit(LoweringOpcode::True, LINE);
              self.emit(LoweringOpcode::BuildObject, LINE);
              self.emit_byte(4, LINE);
              self.emit(LoweringOpcode::Call, LINE);
              self.emit_byte(3, LINE);
              self.emit(LoweringOpcode::Discard, LINE);
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

              self.emit(LoweringOpcode::Copy, LINE);
              self.emit_get_builtin_name("objectDefineProperty");
              self.emit(LoweringOpcode::Exchange, LINE);
              if let Some(key) = key_value.as_ref() {
                self.emit_constant(key.clone(), LINE);
              } else {
                let key = p
                  .key
                  .as_expression()
                  .expect("computed accessor key must be an expression");
                self.emit_get_builtin_name("toPropertyKey");
                self.compile_expr_oast(key)?;
                self.emit(LoweringOpcode::Call, LINE);
                self.emit_byte(1, LINE);
              }
              let property_key_slot = self.alloc_temp_local("object_literal_accessor_key");
              self.emit(LoweringOpcode::StoreLocal, LINE);
              self.emit_slot(property_key_slot, LINE);
              self.emit(LoweringOpcode::LoadLocal, LINE);
              self.emit_slot(property_key_slot, LINE);

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
                .map(|name| format!("{descriptor_prop_name} {name}"))
                .unwrap_or_else(|| "<method>".to_string());
              self.emit_constant(Value::String(descriptor_prop_name.to_string().into()), LINE);
              if let oast::Expression::FunctionExpression(f) = &p.value {
                let (mut proto, _env_len) =
                  self.compile_function(&method_name, None, f, None, true)?;
                proto.has_prototype_property = proto.is_generator;
                proto.is_constructor = false;
                let idx = self.chunk.add_function(proto);
                self.emit(LoweringOpcode::MakeClosure, LINE);
                self.emit_function_index(idx, LINE);
                self.emit_set_method_home_object(home_object_slot)?;
              } else {
                self.compile_expr_oast(&p.value)?;
              }
              let value_slot = self.alloc_temp_local("object_literal_accessor_value");
              self.emit(LoweringOpcode::StoreLocal, LINE);
              self.emit_slot(value_slot, LINE);
              self.emit_get_builtin_name("setFunctionNameIfAnonymous");
              self.emit(LoweringOpcode::LoadLocal, LINE);
              self.emit_slot(value_slot, LINE);
              self.emit(LoweringOpcode::LoadLocal, LINE);
              self.emit_slot(property_key_slot, LINE);
              self.emit_constant(Value::String(descriptor_prop_name.into()), LINE);
              self.emit(LoweringOpcode::Call, LINE);
              self.emit_byte(3, LINE);
              self.emit_constant(Value::String("enumerable".to_string().into()), LINE);
              self.emit(LoweringOpcode::True, LINE);
              self.emit_constant(Value::String("configurable".to_string().into()), LINE);
              self.emit(LoweringOpcode::True, LINE);
              self.emit(LoweringOpcode::BuildObject, LINE);
              self.emit_byte(3, LINE);
              self.emit(LoweringOpcode::Call, LINE);
              self.emit_byte(3, LINE);
              self.emit(LoweringOpcode::Discard, LINE);
            }
            oast::ObjectPropertyKind::SpreadProperty(spread) => {
              let source_slot = self.alloc_temp_local("object_spread");
              self.compile_expr_oast(&spread.argument)?;
              self.emit(LoweringOpcode::StoreLocal, LINE);
              self.emit_slot(source_slot, LINE);
              self.emit(LoweringOpcode::Copy, LINE);
              self.emit_get_global_name("Object");
              self.emit_constant(Value::String("assign".to_string().into()), LINE);
              self.emit(LoweringOpcode::GetProperty, LINE);
              self.emit(LoweringOpcode::Exchange, LINE);
              self.emit(LoweringOpcode::LoadLocal, LINE);
              self.emit_slot(source_slot, LINE);
              self.emit(LoweringOpcode::Call, LINE);
              self.emit_byte(2, LINE);
              self.emit(LoweringOpcode::Discard, LINE);
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
          self.emit(LoweringOpcode::BuildArray, LINE);
          self.emit_byte(self.constant_index_to_u8(arr.elements.len())?, LINE);
          for el in &arr.elements {
            if matches!(el, oast::ArrayExpressionElement::SpreadElement(_)) {
              self.emit(LoweringOpcode::True, LINE);
            } else {
              self.emit(LoweringOpcode::False, LINE);
            }
          }
          self.emit(LoweringOpcode::BuildArray, LINE);
          self.emit_byte(self.constant_index_to_u8(arr.elements.len())?, LINE);
          self.emit(LoweringOpcode::ArrayFromSpread, LINE);
          self.emit_byte(self.constant_index_to_u8(arr.elements.len())?, LINE);
        } else {
          for el in &arr.elements {
            if let Some(expr) = el.as_expression() {
              self.compile_expr_oast(expr)?;
            } else {
              self.emit_constant(Value::Uninitialized, LINE);
            }
          }
          self.emit(LoweringOpcode::BuildArray, LINE);
          self.emit_byte(self.constant_index_to_u8(arr.elements.len())?, LINE);
        }
      }
      OExpr::Identifier(id) => {
        let name = id.name.as_str();
        if name == crate::parser::EVAL_NEW_TARGET_SENTINEL {
          self.emit(LoweringOpcode::GetNewTarget, LINE);
          return Ok(());
        }
        if let Some(error) = Self::strict_identifier_reference_error(self.strict_context, name) {
          return Err(error);
        }
        self.emit_get_binding(name);
      }
      OExpr::AssignmentExpression(a) => {
        if matches!(a.operator, oast::AssignmentOperator::Assign) {
          match &a.left {
            oast::AssignmentTarget::ArrayAssignmentTarget(target) => {
              self.compile_expr_oast(&a.right)?;
              self.emit(LoweringOpcode::Copy, LINE);
              self.compile_array_assignment_target(target)?;
              return Ok(());
            }
            oast::AssignmentTarget::ObjectAssignmentTarget(target) => {
              self.compile_expr_oast(&a.right)?;
              self.emit(LoweringOpcode::Copy, LINE);
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
              let with_reference = (self.with_depth > 0 || self.may_capture_with)
                .then(|| self.emit_resolve_with_identifier(name))
                .transpose()?;
              self.compile_expr_oast(&a.right)?;
              if Self::is_anonymous_function_definition(&a.right) {
                let value_slot = self.alloc_temp_local("assignment_function_name");
                self.emit(LoweringOpcode::StoreLocal, LINE);
                self.emit_slot(value_slot, LINE);
                self.emit_get_builtin_name("setFunctionNameIfAnonymous");
                self.emit(LoweringOpcode::LoadLocal, LINE);
                self.emit_slot(value_slot, LINE);
                self.emit_constant(Value::String(name.into()), LINE);
                self.emit(LoweringOpcode::Call, LINE);
                self.emit_byte(2, LINE);
              }
              if let Some(reference_slot) = with_reference {
                self.emit_set_resolved_identifier(reference_slot, name)?;
              } else {
                self.emit(LoweringOpcode::Copy, LINE);
                if let Some(slot) = self.resolve_local(name) {
                  self.emit(LoweringOpcode::StoreLocal, LINE);
                  self.emit_slot(slot, LINE);
                  if self.local_mirrors_global(slot) {
                    self.emit(LoweringOpcode::LoadLocal, LINE);
                    self.emit_slot(slot, LINE);
                    self.emit_set_global_name(name);
                  }
                  self.emit_sync_local_module_export(name, slot)?;
                } else if let Some(upvalue) = self.resolve_upvalue(name) {
                  self.emit(LoweringOpcode::SetUpvalue, LINE);
                  self.emit_u16(upvalue, LINE);
                  self.emit_sync_upvalue_module_export(upvalue)?;
                } else {
                  self.emit_set_global_name(name);
                }
              }
            }
            oast::SimpleAssignmentTarget::StaticMemberExpression(m) => {
              if matches!(&m.object, OExpr::Super(_)) {
                self.emit_super_property_set_static(m.property.name.as_str(), &a.right)?;
              } else {
                self.compile_expr_oast(&m.object)?;
                self.emit_constant(Value::String(m.property.name.to_string().into()), LINE);
                self.compile_expr_oast(&a.right)?;
                self.emit(LoweringOpcode::SetProperty, LINE);
              }
            }
            oast::SimpleAssignmentTarget::ComputedMemberExpression(m) => {
              if matches!(&m.object, OExpr::Super(_)) {
                self.emit_super_property_set_computed(&m.expression, &a.right)?;
              } else {
                self.compile_expr_oast(&m.object)?;
                self.compile_expr_oast(&m.expression)?;
                self.compile_expr_oast(&a.right)?;
                self.emit(LoweringOpcode::SetProperty, LINE);
              }
            }
            oast::SimpleAssignmentTarget::PrivateFieldExpression(m) => {
              self.compile_expr_oast(&m.object)?;
              let key = self.ensure_private_name_constant(&m.field.name);
              self.emit_constant(key, LINE);
              self.compile_expr_oast(&a.right)?;
              self.emit(LoweringOpcode::SetProperty, LINE);
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
              let with_reference = (self.with_depth > 0 || self.may_capture_with)
                .then(|| self.emit_resolve_with_identifier(name))
                .transpose()?;
              if let Some(reference) = with_reference {
                self.emit_get_resolved_identifier(reference, name)?;
              } else {
                self.emit_get_binding(name);
              }
              self.compile_expr_oast(&a.right)?;
              match a.operator {
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
                oast::AssignmentOperator::ShiftRightZeroFill => {
                  self.emit(LoweringOpcode::Ushr, LINE)
                }
                _ => {}
              }
              self.emit(LoweringOpcode::Copy, LINE);
              if let Some(reference) = with_reference {
                self.emit_set_resolved_identifier(reference, name)?;
                self.emit(LoweringOpcode::Discard, LINE);
              } else {
                self.store_assignment_identifier(name)?;
              }
            }
            oast::SimpleAssignmentTarget::StaticMemberExpression(m) => {
              if matches!(&m.object, OExpr::Super(_)) {
                self.emit(LoweringOpcode::GetThis, LINE);
                self.emit(LoweringOpcode::Copy, LINE);
                self.emit_constant(Value::String(m.property.name.to_string().into()), LINE);
                self.emit(LoweringOpcode::GetSuperProperty, LINE);
                self.compile_expr_oast(&a.right)?;
                self.emit_compound_assignment_op(a.operator);
                self.emit_constant(Value::String(m.property.name.to_string().into()), LINE);
                self.emit(LoweringOpcode::Exchange, LINE);
                self.emit(LoweringOpcode::SetSuperProperty, LINE);
                self.emit(LoweringOpcode::Exchange, LINE);
                self.emit(LoweringOpcode::Discard, LINE);
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
                self.emit(LoweringOpcode::GetThis, LINE);
                self.emit(LoweringOpcode::StoreLocal, LINE);
                self.emit_slot(receiver_slot, LINE);
                self.emit(LoweringOpcode::LoadLocal, LINE);
                self.emit_slot(receiver_slot, LINE);
                self.emit(LoweringOpcode::Copy, LINE);
                self.compile_expr_oast(&m.expression)?;
                self.emit(LoweringOpcode::StoreLocal, LINE);
                self.emit_slot(key_slot, LINE);
                self.emit(LoweringOpcode::LoadLocal, LINE);
                self.emit_slot(key_slot, LINE);
                self.emit(LoweringOpcode::GetSuperProperty, LINE);
                self.compile_expr_oast(&a.right)?;
                self.emit_compound_assignment_op(a.operator);
                self.emit(LoweringOpcode::LoadLocal, LINE);
                self.emit_slot(key_slot, LINE);
                self.emit(LoweringOpcode::Exchange, LINE);
                self.emit(LoweringOpcode::SetSuperProperty, LINE);
                self.emit(LoweringOpcode::Exchange, LINE);
                self.emit(LoweringOpcode::Discard, LINE);
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
                self.emit(LoweringOpcode::LoadLocal, LINE);
                self.emit_slot(slot, LINE);
                self.emit(LoweringOpcode::Copy, LINE);
                self.emit(LoweringOpcode::JumpIfFalse, LINE);
                let jump_end = self.current_pc();
                self.emit_byte(0, LINE);
                self.emit_byte(0, LINE);
                self.emit(LoweringOpcode::Discard, LINE);
                self.compile_expr_oast(&a.right)?;
                self.emit(LoweringOpcode::Copy, LINE);
                self.emit(LoweringOpcode::StoreLocal, LINE);
                self.emit_slot(slot, LINE);
                if self.local_mirrors_global(slot) {
                  self.emit(LoweringOpcode::LoadLocal, LINE);
                  self.emit_slot(slot, LINE);
                  self.emit_set_global_name(name);
                }
                self.patch_jump(jump_end);
              } else if let Some(upvalue) = self.resolve_upvalue(name) {
                self.emit(LoweringOpcode::GetUpvalue, LINE);
                self.emit_u16(upvalue, LINE);
                self.emit(LoweringOpcode::Copy, LINE);
                self.emit(LoweringOpcode::JumpIfFalse, LINE);
                let jump_end = self.current_pc();
                self.emit_byte(0, LINE);
                self.emit_byte(0, LINE);
                self.emit(LoweringOpcode::Discard, LINE);
                self.compile_expr_oast(&a.right)?;
                self.emit(LoweringOpcode::Copy, LINE);
                self.emit(LoweringOpcode::SetUpvalue, LINE);
                self.emit_u16(upvalue, LINE);
                self.patch_jump(jump_end);
              } else {
                self.emit_get_global_name(name);
                self.emit(LoweringOpcode::Copy, LINE);
                self.emit(LoweringOpcode::JumpIfFalse, LINE);
                let jump_end = self.current_pc();
                self.emit_byte(0, LINE);
                self.emit_byte(0, LINE);
                self.emit(LoweringOpcode::Discard, LINE);
                self.compile_expr_oast(&a.right)?;
                self.emit(LoweringOpcode::Copy, LINE);
                self.emit_set_global_name(name);
                self.patch_jump(jump_end);
              }
            }
            oast::SimpleAssignmentTarget::StaticMemberExpression(m) => {
              let (object_slot, key_slot) = self.compile_static_member_reference(m)?;
              self.emit_reference_get(object_slot, key_slot)?;
              self.emit(LoweringOpcode::Copy, LINE);
              self.emit(LoweringOpcode::JumpIfFalse, LINE);
              let jump_end = self.current_pc();
              self.emit_byte(0, LINE);
              self.emit_byte(0, LINE);
              self.emit(LoweringOpcode::Discard, LINE);
              self.compile_expr_oast(&a.right)?;
              self.emit_reference_set_current_value(object_slot, key_slot)?;
              self.patch_jump(jump_end);
            }
            oast::SimpleAssignmentTarget::ComputedMemberExpression(m) => {
              let (object_slot, key_slot) = self.compile_computed_member_reference(m)?;
              self.emit_reference_get(object_slot, key_slot)?;
              self.emit(LoweringOpcode::Copy, LINE);
              self.emit(LoweringOpcode::JumpIfFalse, LINE);
              let jump_end = self.current_pc();
              self.emit_byte(0, LINE);
              self.emit_byte(0, LINE);
              self.emit(LoweringOpcode::Discard, LINE);
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
                self.emit(LoweringOpcode::LoadLocal, LINE);
                self.emit_slot(slot, LINE);
                self.emit(LoweringOpcode::Copy, LINE);
                self.emit(LoweringOpcode::JumpIfTrue, LINE);
                let jump_end = self.current_pc();
                self.emit_byte(0, LINE);
                self.emit_byte(0, LINE);
                self.emit(LoweringOpcode::Discard, LINE);
                self.compile_expr_oast(&a.right)?;
                self.emit(LoweringOpcode::Copy, LINE);
                self.emit(LoweringOpcode::StoreLocal, LINE);
                self.emit_slot(slot, LINE);
                if self.local_mirrors_global(slot) {
                  self.emit(LoweringOpcode::LoadLocal, LINE);
                  self.emit_slot(slot, LINE);
                  self.emit_set_global_name(name);
                }
                self.patch_jump(jump_end);
              } else if let Some(upvalue) = self.resolve_upvalue(name) {
                self.emit(LoweringOpcode::GetUpvalue, LINE);
                self.emit_u16(upvalue, LINE);
                self.emit(LoweringOpcode::Copy, LINE);
                self.emit(LoweringOpcode::JumpIfTrue, LINE);
                let jump_end = self.current_pc();
                self.emit_byte(0, LINE);
                self.emit_byte(0, LINE);
                self.emit(LoweringOpcode::Discard, LINE);
                self.compile_expr_oast(&a.right)?;
                self.emit(LoweringOpcode::Copy, LINE);
                self.emit(LoweringOpcode::SetUpvalue, LINE);
                self.emit_u16(upvalue, LINE);
                self.patch_jump(jump_end);
              } else {
                self.emit_get_global_name(name);
                self.emit(LoweringOpcode::Copy, LINE);
                self.emit(LoweringOpcode::JumpIfTrue, LINE);
                let jump_end = self.current_pc();
                self.emit_byte(0, LINE);
                self.emit_byte(0, LINE);
                self.emit(LoweringOpcode::Discard, LINE);
                self.compile_expr_oast(&a.right)?;
                self.emit(LoweringOpcode::Copy, LINE);
                self.emit_set_global_name(name);
                self.patch_jump(jump_end);
              }
            }
            oast::SimpleAssignmentTarget::StaticMemberExpression(m) => {
              let (object_slot, key_slot) = self.compile_static_member_reference(m)?;
              self.emit_reference_get(object_slot, key_slot)?;
              self.emit(LoweringOpcode::Copy, LINE);
              self.emit(LoweringOpcode::JumpIfTrue, LINE);
              let jump_end = self.current_pc();
              self.emit_byte(0, LINE);
              self.emit_byte(0, LINE);
              self.emit(LoweringOpcode::Discard, LINE);
              self.compile_expr_oast(&a.right)?;
              self.emit_reference_set_current_value(object_slot, key_slot)?;
              self.patch_jump(jump_end);
            }
            oast::SimpleAssignmentTarget::ComputedMemberExpression(m) => {
              let (object_slot, key_slot) = self.compile_computed_member_reference(m)?;
              self.emit_reference_get(object_slot, key_slot)?;
              self.emit(LoweringOpcode::Copy, LINE);
              self.emit(LoweringOpcode::JumpIfTrue, LINE);
              let jump_end = self.current_pc();
              self.emit_byte(0, LINE);
              self.emit_byte(0, LINE);
              self.emit(LoweringOpcode::Discard, LINE);
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
                self.emit(LoweringOpcode::LoadLocal, LINE);
                self.emit_slot(slot, LINE);
                self.emit(LoweringOpcode::Copy, LINE);
                self.emit(LoweringOpcode::JumpIfNullish, LINE);
                let jump_end = self.current_pc();
                self.emit_byte(0, LINE);
                self.emit_byte(0, LINE);
                self.emit(LoweringOpcode::Jump, LINE);
                let after_store = self.current_pc();
                self.emit_byte(0, LINE);
                self.emit_byte(0, LINE);
                self.patch_jump(jump_end);
                self.emit(LoweringOpcode::Discard, LINE);
                self.compile_expr_oast(&a.right)?;
                self.emit(LoweringOpcode::Copy, LINE);
                self.emit(LoweringOpcode::StoreLocal, LINE);
                self.emit_slot(slot, LINE);
                if self.local_mirrors_global(slot) {
                  self.emit(LoweringOpcode::LoadLocal, LINE);
                  self.emit_slot(slot, LINE);
                  self.emit_set_global_name(name);
                }
                self.patch_jump(after_store);
              } else if let Some(upvalue) = self.resolve_upvalue(name) {
                self.emit(LoweringOpcode::GetUpvalue, LINE);
                self.emit_u16(upvalue, LINE);
                self.emit(LoweringOpcode::Copy, LINE);
                self.emit(LoweringOpcode::JumpIfNullish, LINE);
                let jump_end = self.current_pc();
                self.emit_byte(0, LINE);
                self.emit_byte(0, LINE);
                self.emit(LoweringOpcode::Jump, LINE);
                let after_store = self.current_pc();
                self.emit_byte(0, LINE);
                self.emit_byte(0, LINE);
                self.patch_jump(jump_end);
                self.emit(LoweringOpcode::Discard, LINE);
                self.compile_expr_oast(&a.right)?;
                self.emit(LoweringOpcode::Copy, LINE);
                self.emit(LoweringOpcode::SetUpvalue, LINE);
                self.emit_u16(upvalue, LINE);
                self.patch_jump(after_store);
              } else {
                self.emit_get_global_name(name);
                self.emit(LoweringOpcode::Copy, LINE);
                self.emit(LoweringOpcode::JumpIfNullish, LINE);
                let jump_end = self.current_pc();
                self.emit_byte(0, LINE);
                self.emit_byte(0, LINE);
                self.emit(LoweringOpcode::Jump, LINE);
                let after_store = self.current_pc();
                self.emit_byte(0, LINE);
                self.emit_byte(0, LINE);
                self.patch_jump(jump_end);
                self.emit(LoweringOpcode::Discard, LINE);
                self.compile_expr_oast(&a.right)?;
                self.emit(LoweringOpcode::Copy, LINE);
                self.emit_set_global_name(name);
                self.patch_jump(after_store);
              }
            }
            oast::SimpleAssignmentTarget::StaticMemberExpression(m) => {
              let (object_slot, key_slot) = self.compile_static_member_reference(m)?;
              self.emit_reference_get(object_slot, key_slot)?;
              self.emit(LoweringOpcode::Copy, LINE);
              self.emit(LoweringOpcode::JumpIfNullish, LINE);
              let jump_end = self.current_pc();
              self.emit_byte(0, LINE);
              self.emit_byte(0, LINE);
              self.emit(LoweringOpcode::Jump, LINE);
              let after_store = self.current_pc();
              self.emit_byte(0, LINE);
              self.emit_byte(0, LINE);
              self.patch_jump(jump_end);
              self.emit(LoweringOpcode::Discard, LINE);
              self.compile_expr_oast(&a.right)?;
              self.emit_reference_set_current_value(object_slot, key_slot)?;
              self.patch_jump(after_store);
            }
            oast::SimpleAssignmentTarget::ComputedMemberExpression(m) => {
              let (object_slot, key_slot) = self.compile_computed_member_reference(m)?;
              self.emit_reference_get(object_slot, key_slot)?;
              self.emit(LoweringOpcode::Copy, LINE);
              self.emit(LoweringOpcode::JumpIfNullish, LINE);
              let jump_end = self.current_pc();
              self.emit_byte(0, LINE);
              self.emit_byte(0, LINE);
              self.emit(LoweringOpcode::Jump, LINE);
              let after_store = self.current_pc();
              self.emit_byte(0, LINE);
              self.emit_byte(0, LINE);
              self.patch_jump(jump_end);
              self.emit(LoweringOpcode::Discard, LINE);
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
                self.emit(LoweringOpcode::Typeof, LINE);
              }
            } else if self.resolve_upvalue(name).is_some()
              || self
                .current_module
                .as_ref()
                .is_some_and(|module| module.imports.contains_key(name))
            {
              self.compile_expr_oast(&u.argument)?;
              self.emit(LoweringOpcode::Typeof, LINE);
            } else {
              self.emit_typeof_identifier(name);
            }
          } else {
            self.compile_expr_oast(&u.argument)?;
            self.emit(LoweringOpcode::Typeof, LINE);
          }
        }
        oast::UnaryOperator::Delete => {
          if self.strict_context && Self::identifier_name_if_parenthesized(&u.argument).is_some() {
            return Err(crate::errors::CompilerError::from(
              "SyntaxError: delete of an unqualified identifier in strict mode",
            ));
          }
          if let OExpr::StaticMemberExpression(m) = &u.argument {
            if matches!(&m.object, OExpr::Super(_)) {
              self.emit_get_global_name("Object");
              self.emit_constant(Value::String("create".into()), LINE);
              self.emit(LoweringOpcode::GetProperty, LINE);
              self.emit_get_global_name("ReferenceError");
              self.emit_constant(Value::String("prototype".into()), LINE);
              self.emit(LoweringOpcode::GetProperty, LINE);
              self.emit(LoweringOpcode::Call, LINE);
              self.emit_byte(1, LINE);
              self.emit(LoweringOpcode::Throw, LINE);
              self.emit(LoweringOpcode::Undefined, LINE);
            } else {
              self.compile_expr_oast(&m.object)?;
              self.emit_constant(Value::String(m.property.name.to_string().into()), LINE);
              self.emit(LoweringOpcode::DeleteProperty, LINE);
            }
          } else if let OExpr::ComputedMemberExpression(m) = &u.argument {
            if matches!(&m.object, OExpr::Super(_)) {
              self.compile_expr_oast(&m.expression)?;
              self.emit(LoweringOpcode::Discard, LINE);
              self.emit_get_global_name("Object");
              self.emit_constant(Value::String("create".into()), LINE);
              self.emit(LoweringOpcode::GetProperty, LINE);
              self.emit_get_global_name("ReferenceError");
              self.emit_constant(Value::String("prototype".into()), LINE);
              self.emit(LoweringOpcode::GetProperty, LINE);
              self.emit(LoweringOpcode::Call, LINE);
              self.emit_byte(1, LINE);
              self.emit(LoweringOpcode::Throw, LINE);
              self.emit(LoweringOpcode::Undefined, LINE);
            } else {
              self.compile_expr_oast(&m.object)?;
              self.compile_expr_oast(&m.expression)?;
              self.emit(LoweringOpcode::DeleteProperty, LINE);
            }
          } else if let OExpr::Identifier(id) = &u.argument {
            if self.resolve_local(id.name.as_str()).is_some()
              || self.resolve_upvalue(id.name.as_str()).is_some()
            {
              self.emit_constant(Value::Bool(false), LINE);
            } else {
              self.emit_constant(Value::String(id.name.to_string().into()), LINE);
              self.emit(LoweringOpcode::DeleteIdentifier, LINE);
            }
          } else {
            self.emit_constant(Value::Bool(true), LINE);
          }
        }
        _ => {
          self.compile_expr_oast(&u.argument)?;
          match u.operator {
            oast::UnaryOperator::LogicalNot => self.emit(LoweringOpcode::Not, LINE),
            oast::UnaryOperator::UnaryNegation => self.emit(LoweringOpcode::Neg, LINE),
            oast::UnaryOperator::UnaryPlus => {
              self.emit_constant(Value::Number(0.0), LINE);
              self.emit(LoweringOpcode::Sub, LINE);
            }
            oast::UnaryOperator::BitwiseNot => self.emit(LoweringOpcode::BitNot, LINE),
            oast::UnaryOperator::Void => {
              self.emit(LoweringOpcode::Discard, LINE);
              self.emit(LoweringOpcode::Undefined, LINE);
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
            if self
              .current_module
              .as_ref()
              .is_some_and(|c| c.imports.contains_key(name))
            {
              return Err(Self::imported_binding_assignment_error());
            }
            let with_reference = (self.with_depth > 0 || self.may_capture_with)
              .then(|| self.emit_resolve_with_identifier(name))
              .transpose()?;
            if let Some(reference) = with_reference {
              self.emit_get_resolved_identifier(reference, name)?;
            } else {
              self.emit_get_binding(name);
            }
            self.emit(LoweringOpcode::Neg, LINE);
            self.emit(LoweringOpcode::Neg, LINE);
            if !u.prefix {
              self.emit(LoweringOpcode::Copy, LINE);
            }
            self.emit_constant(Value::Number(1.0), LINE);
            match u.operator {
              oast::UpdateOperator::Increment => self.emit(LoweringOpcode::Increment, LINE),
              oast::UpdateOperator::Decrement => self.emit(LoweringOpcode::Decrement, LINE),
            }
            if u.prefix {
              self.emit(LoweringOpcode::Copy, LINE);
            }
            if let Some(reference) = with_reference {
              self.emit_set_resolved_identifier(reference, name)?;
              self.emit(LoweringOpcode::Discard, LINE);
            } else {
              self.store_assignment_identifier(name)?;
            }
          }
          OTarget::StaticMemberExpression(m) => {
            let (object_slot, key_slot) = self.compile_static_member_reference(m)?;
            self.emit_reference_get(object_slot, key_slot)?;
            let old_slot = self.alloc_temp_local("ref_old");
            self.emit(LoweringOpcode::StoreLocal, LINE);
            self.emit_slot(old_slot, LINE);
            self.emit(LoweringOpcode::LoadLocal, LINE);
            self.emit_slot(old_slot, LINE);
            self.emit(LoweringOpcode::Neg, LINE);
            self.emit(LoweringOpcode::Neg, LINE);
            if !u.prefix {
              self.emit(LoweringOpcode::Copy, LINE);
            }
            self.emit_constant(Value::Number(1.0), LINE);
            match u.operator {
              oast::UpdateOperator::Increment => self.emit(LoweringOpcode::Increment, LINE),
              oast::UpdateOperator::Decrement => self.emit(LoweringOpcode::Decrement, LINE),
            }
            self.emit_reference_set_current_value(object_slot, key_slot)?;
            if !u.prefix {
              self.emit(LoweringOpcode::Discard, LINE);
            }
          }
          OTarget::ComputedMemberExpression(m) => {
            let (object_slot, key_slot) = self.compile_computed_member_reference(m)?;
            self.emit_reference_get(object_slot, key_slot)?;
            let old_slot = self.alloc_temp_local("ref_old");
            self.emit(LoweringOpcode::StoreLocal, LINE);
            self.emit_slot(old_slot, LINE);
            self.emit(LoweringOpcode::LoadLocal, LINE);
            self.emit_slot(old_slot, LINE);
            self.emit(LoweringOpcode::Neg, LINE);
            self.emit(LoweringOpcode::Neg, LINE);
            if !u.prefix {
              self.emit(LoweringOpcode::Copy, LINE);
            }
            self.emit_constant(Value::Number(1.0), LINE);
            match u.operator {
              oast::UpdateOperator::Increment => self.emit(LoweringOpcode::Increment, LINE),
              oast::UpdateOperator::Decrement => self.emit(LoweringOpcode::Decrement, LINE),
            }
            self.emit_reference_set_current_value(object_slot, key_slot)?;
            if !u.prefix {
              self.emit(LoweringOpcode::Discard, LINE);
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
          self.emit(LoweringOpcode::Undefined, LINE);
        }
        if y.delegate {
          self.emit(LoweringOpcode::YieldStar, LINE);
        } else {
          self.emit(LoweringOpcode::Yield, LINE);
        }
      }
      OExpr::BinaryExpression(b) => {
        if matches!(b.operator, oast::BinaryOperator::Instanceof) {
          self.compile_expr_oast(&b.left)?;
          self.compile_expr_oast(&b.right)?;
          self.emit(LoweringOpcode::InstanceOf, LINE);
          return Ok(());
        }
        if matches!(b.operator, oast::BinaryOperator::In) {
          self.compile_expr_oast(&b.left)?;
          let key_slot = self.alloc_reusable_temp_local("in_property_key");
          self.emit(LoweringOpcode::StoreLocal, LINE);
          self.emit_slot(key_slot, LINE);
          self.compile_expr_oast(&b.right)?;
          self.emit(LoweringOpcode::LoadLocal, LINE);
          self.emit_slot(key_slot, LINE);
          self.emit(LoweringOpcode::HasProperty, LINE);
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
          self.emit_with_payload(LoweringOpcode::CompareJump, cond_type, LINE);
          let jump_false = self.current_pc();
          self.emit_byte(0, LINE);
          self.emit_byte(0, LINE);
          self.emit(LoweringOpcode::True, LINE);
          self.emit(LoweringOpcode::Jump, LINE);
          let jump_end = self.current_pc();
          self.emit_byte(0, LINE);
          self.emit_byte(0, LINE);
          self.patch_jump(jump_false);
          self.emit(LoweringOpcode::False, LINE);
          self.patch_jump(jump_end);
          return Ok(());
        }
        self.compile_expr_oast(&b.left)?;
        self.compile_expr_oast(&b.right)?;
        match b.operator {
          oast::BinaryOperator::Addition => self.emit(LoweringOpcode::Add, LINE),
          oast::BinaryOperator::Subtraction => self.emit(LoweringOpcode::Sub, LINE),
          oast::BinaryOperator::Multiplication => self.emit(LoweringOpcode::Mul, LINE),
          oast::BinaryOperator::Division => self.emit(LoweringOpcode::Div, LINE),
          oast::BinaryOperator::Remainder => self.emit(LoweringOpcode::Mod, LINE),
          oast::BinaryOperator::Exponential => self.emit(LoweringOpcode::Pow, LINE),
          oast::BinaryOperator::BitwiseAnd => self.emit(LoweringOpcode::BitAnd, LINE),
          oast::BinaryOperator::BitwiseOR => self.emit(LoweringOpcode::BitOr, LINE),
          oast::BinaryOperator::BitwiseXOR => self.emit(LoweringOpcode::BitXor, LINE),
          oast::BinaryOperator::ShiftLeft => self.emit(LoweringOpcode::Shl, LINE),
          oast::BinaryOperator::ShiftRight => self.emit(LoweringOpcode::Shr, LINE),
          oast::BinaryOperator::ShiftRightZeroFill => self.emit(LoweringOpcode::Ushr, LINE),
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
          self.emit(LoweringOpcode::Copy, LINE);
          self.emit(LoweringOpcode::JumpIfFalse, LINE);
          let jump_end = self.current_pc();
          self.emit_byte(0, LINE);
          self.emit_byte(0, LINE);
          self.emit(LoweringOpcode::Discard, LINE);
          self.compile_expr_oast(&l.right)?;
          self.patch_jump(jump_end);
        }
        oast::LogicalOperator::Or => {
          self.compile_expr_oast(&l.left)?;
          self.emit(LoweringOpcode::Copy, LINE);
          self.emit(LoweringOpcode::JumpIfTrue, LINE);
          let jump_end = self.current_pc();
          self.emit_byte(0, LINE);
          self.emit_byte(0, LINE);
          self.emit(LoweringOpcode::Discard, LINE);
          self.compile_expr_oast(&l.right)?;
          self.patch_jump(jump_end);
        }
        oast::LogicalOperator::Coalesce => {
          self.compile_expr_oast(&l.left)?;
          self.emit(LoweringOpcode::Copy, LINE);
          self.emit(LoweringOpcode::JumpIfNullish, LINE);
          let jump_right = self.current_pc();
          self.emit_byte(0, LINE);
          self.emit_byte(0, LINE);
          self.emit(LoweringOpcode::Jump, LINE);
          let jump_end = self.current_pc();
          self.emit_byte(0, LINE);
          self.emit_byte(0, LINE);
          self.patch_jump(jump_right);
          self.emit(LoweringOpcode::Discard, LINE);
          self.compile_expr_oast(&l.right)?;
          self.patch_jump(jump_end);
        }
      },
      OExpr::ConditionalExpression(c) => {
        if let Some(jump_else) = self.try_emit_compare_jump_if_false(&c.test)? {
          self.compile_expr_oast(&c.consequent)?;
          self.emit(LoweringOpcode::Jump, LINE);
          let jump_end = self.current_pc();
          self.emit_byte(0, LINE);
          self.emit_byte(0, LINE);
          self.patch_jump(jump_else);
          self.compile_expr_oast(&c.alternate)?;
          self.patch_jump(jump_end);
        } else {
          self.compile_expr_oast(&c.test)?;
          self.emit(LoweringOpcode::JumpIfFalse, LINE);
          let jump_else = self.current_pc();
          self.emit_byte(0, LINE);
          self.emit_byte(0, LINE);
          self.compile_expr_oast(&c.consequent)?;
          self.emit(LoweringOpcode::Jump, LINE);
          let jump_end = self.current_pc();
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
            self.emit(LoweringOpcode::Discard, LINE);
          }
        }
        if len == 0 {
          self.emit(LoweringOpcode::Undefined, LINE);
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
            self.emit(LoweringOpcode::Copy, LINE);
            self.emit(LoweringOpcode::JumpIfNullish, LINE);
            let nullish = self.current_pc();
            self.emit_jump_placeholder();
            self.emit_constant(Value::String(m.property.name.to_string().into()), LINE);
            self.emit(LoweringOpcode::GetProperty, LINE);
            self.emit(LoweringOpcode::Jump, LINE);
            let end = self.current_pc();
            self.emit_jump_placeholder();
            self.patch_jump(nullish);
            self.emit(LoweringOpcode::Discard, LINE);
            self.emit(LoweringOpcode::Undefined, LINE);
            self.patch_jump(end);
          } else {
            self.emit_constant(Value::String(m.property.name.to_string().into()), LINE);
            self.emit(LoweringOpcode::GetProperty, LINE);
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
            self.emit(LoweringOpcode::Copy, LINE);
            self.emit(LoweringOpcode::JumpIfNullish, LINE);
            let nullish = self.current_pc();
            self.emit_jump_placeholder();
            self.compile_expr_oast(&m.expression)?;
            self.emit(LoweringOpcode::GetProperty, LINE);
            self.emit(LoweringOpcode::Jump, LINE);
            let end = self.current_pc();
            self.emit_jump_placeholder();
            self.patch_jump(nullish);
            self.emit(LoweringOpcode::Discard, LINE);
            self.emit(LoweringOpcode::Undefined, LINE);
            self.patch_jump(end);
          } else {
            self.compile_expr_oast(&m.expression)?;
            self.emit(LoweringOpcode::GetProperty, LINE);
          }
        }
      }
      OExpr::PrivateFieldExpression(m) => {
        self.compile_expr_oast(&m.object)?;
        if m.optional {
          self.emit(LoweringOpcode::Copy, LINE);
          self.emit(LoweringOpcode::JumpIfNullish, LINE);
          let nullish = self.current_pc();
          self.emit_jump_placeholder();
          let key = self.ensure_private_name_constant(&m.field.name);
          self.emit_constant(key, LINE);
          self.emit(LoweringOpcode::GetProperty, LINE);
          self.emit(LoweringOpcode::Jump, LINE);
          let end = self.current_pc();
          self.emit_jump_placeholder();
          self.patch_jump(nullish);
          self.emit(LoweringOpcode::Discard, LINE);
          self.emit(LoweringOpcode::Undefined, LINE);
          self.patch_jump(end);
        } else {
          let key = self.ensure_private_name_constant(&m.field.name);
          self.emit_constant(key, LINE);
          self.emit(LoweringOpcode::GetProperty, LINE);
        }
      }
      OExpr::ChainExpression(c) => {
        let mut continuation = |_this: &mut Self| Ok(());
        self.compile_optional_chain_element(&c.expression, &mut continuation)?;
        return Ok(());
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
        self.emit(LoweringOpcode::GetNewTarget, LINE);
      }
      OExpr::ImportMeta(_) => {
        if self.current_module.is_some() {
          self.emit_get_builtin_name("importMeta");
          self.emit(LoweringOpcode::Call, LINE);
          self.emit_byte(0, LINE);
        } else {
          self.emit(LoweringOpcode::Undefined, LINE);
        }
      }
      OExpr::CallExpression(c) => {
        if !c.optional
          && !c
            .arguments
            .iter()
            .any(|argument| matches!(argument, oast::Argument::SpreadElement(_)))
          && matches!(&c.callee, OExpr::Identifier(identifier) if identifier.name == "eval")
        {
          self.emit_get_builtin_name("callEvalIdentifier");
          self.compile_expr_oast(&c.callee)?;
          let arg_count = self.compile_call_arguments(&c.arguments)?;
          self.emit_call_with_arg_count(LoweringOpcode::Call, arg_count + 1)?;
          return Ok(());
        }
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
          self.emit_call_with_arg_count(LoweringOpcode::SuperConstruct, arg_count)?;
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
          self.emit_call_with_arg_count(LoweringOpcode::CallMethod, arg_count)?;
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
          self.emit_call_with_arg_count(LoweringOpcode::CallMethod, arg_count)?;
          return Ok(());
        }

        if let OExpr::StaticMemberExpression(m) = &c.callee
          && m.optional
          && !matches!(&m.object, OExpr::Super(_))
        {
          self.compile_expr_oast(&m.object)?;
          self.emit(LoweringOpcode::Copy, LINE);
          self.emit(LoweringOpcode::JumpIfNullish, LINE);
          let nullish = self.current_pc();
          self.emit_jump_placeholder();
          self.emit(LoweringOpcode::Copy, LINE);
          self.emit_constant(Value::String(m.property.name.to_string().into()), LINE);
          self.emit(LoweringOpcode::GetProperty, LINE);
          let arg_count = self.compile_call_arguments(&c.arguments)?;
          self.emit_call_with_arg_count(LoweringOpcode::CallMethod, arg_count)?;
          self.emit(LoweringOpcode::Jump, LINE);
          let end = self.current_pc();
          self.emit_jump_placeholder();
          self.patch_jump(nullish);
          self.emit(LoweringOpcode::Discard, LINE);
          self.emit(LoweringOpcode::Undefined, LINE);
          self.patch_jump(end);
          return Ok(());
        }

        if let OExpr::StaticMemberExpression(m) = &c.callee {
          let _is_optional_call = c.optional;

          if m.optional {
            self.compile_optional_member_access(&m.object, |this: &mut Self| {
              this.emit(LoweringOpcode::Copy, LINE);
              this.emit_constant(Value::String(m.property.name.to_string().into()), LINE);
              this.emit(LoweringOpcode::GetProperty, LINE);
              Ok(())
            })?;
          } else {
            self.compile_expr_oast(&m.object)?;
            let receiver_slot = self.alloc_temp_local("call_receiver");
            self.emit(LoweringOpcode::StoreLocal, LINE);
            self.emit_slot(receiver_slot, LINE);
            self.emit(LoweringOpcode::LoadLocal, LINE);
            self.emit_slot(receiver_slot, LINE);
            self.emit(LoweringOpcode::LoadLocal, LINE);
            self.emit_slot(receiver_slot, LINE);
            self.emit_constant(Value::String(m.property.name.to_string().into()), LINE);
            self.emit(LoweringOpcode::GetProperty, LINE);
          }

          let arg_count = self.compile_call_arguments(&c.arguments)?;
          self.emit_call_with_arg_count(LoweringOpcode::CallMethod, arg_count)?;
          return Ok(());
        }

        if let OExpr::ComputedMemberExpression(m) = &c.callee
          && m.optional
          && !matches!(&m.object, OExpr::Super(_))
        {
          self.compile_expr_oast(&m.object)?;
          self.emit(LoweringOpcode::Copy, LINE);
          self.emit(LoweringOpcode::JumpIfNullish, LINE);
          let nullish = self.current_pc();
          self.emit_jump_placeholder();
          self.emit(LoweringOpcode::Copy, LINE);
          self.compile_expr_oast(&m.expression)?;
          self.emit(LoweringOpcode::GetProperty, LINE);
          let arg_count = self.compile_call_arguments(&c.arguments)?;
          self.emit_call_with_arg_count(LoweringOpcode::CallMethod, arg_count)?;
          self.emit(LoweringOpcode::Jump, LINE);
          let end = self.current_pc();
          self.emit_jump_placeholder();
          self.patch_jump(nullish);
          self.emit(LoweringOpcode::Discard, LINE);
          self.emit(LoweringOpcode::Undefined, LINE);
          self.patch_jump(end);
          return Ok(());
        }

        if let OExpr::ComputedMemberExpression(m) = &c.callee {
          let _is_optional_call = c.optional;

          if m.optional {
            self.compile_optional_member_access(&m.object, |this: &mut Self| {
              this.emit(LoweringOpcode::Copy, LINE);
              this.compile_expr_oast(&m.expression)?;
              this.emit(LoweringOpcode::GetProperty, LINE);
              Ok(())
            })?;
          } else {
            self.compile_expr_oast(&m.object)?;
            self.compile_expr_oast(&m.expression)?;
            let key_slot = self.alloc_temp_local("call_key");
            self.emit(LoweringOpcode::StoreLocal, LINE);
            self.emit_slot(key_slot, LINE);
            let receiver_slot = self.alloc_temp_local("call_receiver");
            self.emit(LoweringOpcode::StoreLocal, LINE);
            self.emit_slot(receiver_slot, LINE);
            self.emit(LoweringOpcode::LoadLocal, LINE);
            self.emit_slot(receiver_slot, LINE);
            self.emit(LoweringOpcode::LoadLocal, LINE);
            self.emit_slot(receiver_slot, LINE);
            self.emit(LoweringOpcode::LoadLocal, LINE);
            self.emit_slot(key_slot, LINE);
            self.emit(LoweringOpcode::GetProperty, LINE);
          }

          let arg_count = self.compile_call_arguments(&c.arguments)?;
          self.emit_call_with_arg_count(LoweringOpcode::CallMethod, arg_count)?;
          return Ok(());
        }

        if let OExpr::PrivateFieldExpression(m) = &c.callee {
          let _is_optional_call = c.optional;

          if m.optional {
            self.compile_optional_member_access(&m.object, |this: &mut Self| {
              this.emit(LoweringOpcode::Copy, LINE);
              let key = this.ensure_private_name_constant(&m.field.name);
              this.emit_constant(key, LINE);
              this.emit(LoweringOpcode::GetProperty, LINE);
              Ok(())
            })?;
          } else {
            self.compile_expr_oast(&m.object)?;
            let receiver_slot = self.alloc_temp_local("call_receiver");
            self.emit(LoweringOpcode::StoreLocal, LINE);
            self.emit_slot(receiver_slot, LINE);
            self.emit(LoweringOpcode::LoadLocal, LINE);
            self.emit_slot(receiver_slot, LINE);
            self.emit(LoweringOpcode::LoadLocal, LINE);
            self.emit_slot(receiver_slot, LINE);
            let key = self.ensure_private_name_constant(&m.field.name);
            self.emit_constant(key, LINE);
            self.emit(LoweringOpcode::GetProperty, LINE);
          }

          let arg_count = self.compile_call_arguments(&c.arguments)?;
          self.emit_call_with_arg_count(LoweringOpcode::CallMethod, arg_count)?;
          return Ok(());
        }

        if c.optional {
          self.compile_expr_oast(&c.callee)?;
          self.emit(LoweringOpcode::Copy, LINE);
          self.emit(LoweringOpcode::JumpIfNullish, LINE);
          let nullish = self.current_pc();
          self.emit_jump_placeholder();
          let arg_count = self.compile_call_arguments(&c.arguments)?;
          self.emit_call_with_arg_count(LoweringOpcode::Call, arg_count)?;
          self.emit(LoweringOpcode::Jump, LINE);
          let end = self.current_pc();
          self.emit_jump_placeholder();
          self.patch_jump(nullish);
          self.emit(LoweringOpcode::Discard, LINE);
          self.emit(LoweringOpcode::Undefined, LINE);
          self.patch_jump(end);
          return Ok(());
        }

        self.compile_expr_oast(&c.callee)?;
        let arg_count = self.compile_call_arguments(&c.arguments)?;
        self.emit_call_with_arg_count(LoweringOpcode::Call, arg_count)?;
      }
      OExpr::NewExpression(n) => {
        self.compile_expr_oast(&n.callee)?;
        if n
          .arguments
          .iter()
          .any(|argument| matches!(argument, oast::Argument::SpreadElement(_)))
        {
          let arg_count = self.checked_arg_count(n.arguments.len())?;
          let mut spread_flags = Vec::with_capacity(n.arguments.len());
          for argument in &n.arguments {
            match argument {
              oast::Argument::SpreadElement(spread) => {
                self.compile_expr_oast(&spread.argument)?;
                spread_flags.push(true);
              }
              _ => match argument.as_expression() {
                Some(expression) => {
                  self.compile_expr_oast(expression)?;
                  spread_flags.push(false);
                }
                None => {
                  self.emit(LoweringOpcode::Undefined, LINE);
                  spread_flags.push(false);
                }
              },
            }
          }
          self.emit(LoweringOpcode::BuildArray, LINE);
          self.emit_byte(arg_count, LINE);
          for (index, is_spread) in spread_flags.into_iter().enumerate() {
            if index == 0 {
              self.emit(LoweringOpcode::Undefined, LINE);
              self.emit(
                if is_spread {
                  LoweringOpcode::True
                } else {
                  LoweringOpcode::False
                },
                LINE,
              );
              self.emit(LoweringOpcode::False, LINE);
              self.emit(LoweringOpcode::True, LINE);
              self.emit(LoweringOpcode::BuildArray, LINE);
              self.emit_byte(4, LINE);
            } else {
              self.emit(
                if is_spread {
                  LoweringOpcode::True
                } else {
                  LoweringOpcode::False
                },
                LINE,
              );
            }
          }
          self.emit(LoweringOpcode::BuildArray, LINE);
          self.emit_byte(arg_count, LINE);
          self.emit(LoweringOpcode::Spread, LINE);
          self.emit_byte(arg_count, LINE);
        } else {
          let arg_count = self.compile_call_arguments(&n.arguments)?;
          self.emit_call_with_arg_count(LoweringOpcode::Construct, arg_count)?;
        }
      }
      OExpr::ImportExpression(i) => {
        self.emit_get_builtin_name("dynamicImport");
        self.compile_expr_oast(&i.source)?;
        if let Some(options) = &i.options {
          self.compile_expr_oast(options)?;
        } else {
          self.emit(LoweringOpcode::Undefined, LINE);
        }
        self.emit(LoweringOpcode::Call, LINE);
        self.emit_byte(2, LINE);
      }
      OExpr::RegExpLiteral(re) => {
        self.emit_get_global_name("RegExp");
        self.emit_constant(
          Value::String(re.regex.pattern.text.to_string().into()),
          LINE,
        );
        self.emit_constant(Value::String(re.regex.flags.to_string().into()), LINE);
        self.emit(LoweringOpcode::Call, LINE);
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
        self.emit(LoweringOpcode::AwaitPromise, LINE);
      }
      OExpr::TSAsExpression(a) => {
        self.compile_expr_oast(&a.expression)?;
      }
      OExpr::TSNonNullExpression(n) => {
        self.compile_expr_oast(&n.expression)?;
      }
      _ => self.emit(LoweringOpcode::Undefined, LINE),
    }
    Ok(())
  }
}
