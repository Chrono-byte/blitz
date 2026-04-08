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
//! Expression compilation is recursive and produces stack-based bytecode.

use crate::bytecode::{Chunk, FunctionProto, Op};
use crate::value::Value;
use oxc_ast::ast as oast;
use oxc_syntax::number::NumberBase;
use std::rc::Rc;

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
                    oxc_span::Str::as_str,
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

    pub(super) fn alloc_temp_local(&mut self, prefix: &str) -> usize {
        self.add_local_with_mirror(format!("__{}_{}", prefix, self.locals.len()), None, false)
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
        self.emit(Op::Dup, LINE);
        self.emit_constant(Value::String(property.to_string()), LINE);
        self.emit(Op::GetSuperProperty, LINE);
        if !preserve_receiver {
            self.emit(Op::Swap, LINE);
            self.emit(Op::Pop, LINE);
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
        self.emit(Op::Dup, LINE);
        self.compile_expr_oast(key_expr)?;
        self.emit(Op::GetSuperProperty, LINE);
        if !preserve_receiver {
            self.emit(Op::Swap, LINE);
            self.emit(Op::Pop, LINE);
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
        self.emit(Op::Dup, LINE);
        self.emit_constant(Value::String(property.to_string()), LINE);
        self.compile_expr_oast(value_expr)?;
        self.emit(Op::SetSuperProperty, LINE);
        self.emit(Op::Swap, LINE);
        self.emit(Op::Pop, LINE);
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
        self.emit(Op::Dup, LINE);
        self.compile_expr_oast(key_expr)?;
        self.compile_expr_oast(value_expr)?;
        self.emit(Op::SetSuperProperty, LINE);
        self.emit(Op::Swap, LINE);
        self.emit(Op::Pop, LINE);
        Ok(())
    }

    fn compile_static_member_reference(
        &mut self,
        m: &oast::StaticMemberExpression<'_>,
    ) -> Result<(usize, usize), crate::errors::CompilerError> {
        self.compile_expr_oast(&m.object)?;
        let object_slot = self.alloc_temp_local("ref_obj");
        self.emit(Op::SetLocal, LINE);
        self.emit_byte(object_slot as u8, LINE);

        self.emit_constant(Value::String(m.property.name.to_string()), LINE);
        let key_slot = self.alloc_temp_local("ref_key");
        self.emit(Op::SetLocal, LINE);
        self.emit_byte(key_slot as u8, LINE);
        Ok((object_slot, key_slot))
    }

    fn compile_computed_member_reference(
        &mut self,
        m: &oast::ComputedMemberExpression<'_>,
    ) -> Result<(usize, usize), crate::errors::CompilerError> {
        self.compile_expr_oast(&m.object)?;
        let object_slot = self.alloc_temp_local("ref_obj");
        self.emit(Op::SetLocal, LINE);
        self.emit_byte(object_slot as u8, LINE);

        self.compile_expr_oast(&m.expression)?;
        let key_slot = self.alloc_temp_local("ref_key");
        self.emit(Op::SetLocal, LINE);
        self.emit_byte(key_slot as u8, LINE);
        Ok((object_slot, key_slot))
    }

    fn emit_reference_get(
        &mut self,
        object_slot: usize,
        key_slot: usize,
        use_ic: bool,
    ) -> Result<(), crate::errors::CompilerError> {
        self.emit(Op::GetLocal, LINE);
        self.emit_byte(object_slot as u8, LINE);
        self.emit(Op::GetLocal, LINE);
        self.emit_byte(key_slot as u8, LINE);
        if use_ic {
            self.emit(Op::GetPropertyIC, LINE);
            let slot = self.alloc_ic_slot()?;
            self.emit_byte(slot, LINE);
        } else {
            self.emit(Op::GetProperty, LINE);
        }
        Ok(())
    }

    fn emit_reference_set_from_slot(
        &mut self,
        object_slot: usize,
        key_slot: usize,
        value_slot: usize,
        use_ic: bool,
    ) -> Result<(), crate::errors::CompilerError> {
        self.emit(Op::GetLocal, LINE);
        self.emit_byte(object_slot as u8, LINE);
        self.emit(Op::GetLocal, LINE);
        self.emit_byte(key_slot as u8, LINE);
        self.emit(Op::GetLocal, LINE);
        self.emit_byte(value_slot as u8, LINE);
        if use_ic {
            self.emit(Op::SetPropertyIC, LINE);
            let slot = self.alloc_ic_slot()?;
            self.emit_byte(slot, LINE);
        } else {
            self.emit(Op::SetProperty, LINE);
        }
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

        let compare_op = match binary.operator {
            oast::BinaryOperator::Equality => Op::Eq,
            oast::BinaryOperator::StrictEquality => Op::StrictEq,
            oast::BinaryOperator::Inequality => Op::Neq,
            oast::BinaryOperator::StrictInequality => Op::StrictNeq,
            oast::BinaryOperator::LessThan => Op::Lt,
            oast::BinaryOperator::LessEqualThan => Op::Le,
            oast::BinaryOperator::GreaterThan => Op::Gt,
            oast::BinaryOperator::GreaterEqualThan => Op::Ge,
            _ => return Ok(None),
        };

        self.compile_expr_oast(&binary.left)?;
        self.compile_expr_oast(&binary.right)?;
        self.emit(Op::CompareJumpIfFalse, LINE);
        self.emit_byte(compare_op as u8, LINE);

        // Return the start of the i16 offset field so callers can patch it.
        let patch_addr = self.chunk.code.len();
        self.emit_u16(0, LINE);

        Ok(Some(patch_addr))
    }

    fn emit_compound_assignment_op(&mut self, op: oast::AssignmentOperator) {
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
                        oast::BigintBase::Decimal => {
                            num_bigint::BigInt::parse_bytes(digits_str.as_bytes(), 10)
                        }
                    }
                } else {
                    // Fallback to source text - span gives us byte offsets
                    let source = self.source_text.as_ref();
                    let start = n.span.start as usize;
                    let end = n.span.end as usize;
                    if start < end && end <= source.len() {
                        let digits = source[start..end]
                            .strip_suffix('n')
                            .unwrap_or(&source[start..end])
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
                            oast::BigintBase::Decimal => {
                                num_bigint::BigInt::parse_bytes(digits.as_bytes(), 10)
                            }
                        }
                    } else {
                        None
                    }
                };
                let parsed = parsed
                    .ok_or_else(|| crate::errors::CompilerError::from("invalid bigint literal"))?;
                self.emit_constant(Value::BigInt(parsed), LINE);
            }
            OExpr::StringLiteral(s) => {
                self.validate_literal_early_errors(expr)?;
                self.emit_constant(Value::String(s.value.to_string()), LINE)
            }
            OExpr::TemplateLiteral(t) => {
                let first = t
                    .quasis
                    .first()
                    .and_then(|q| q.value.cooked.as_ref())
                    .map(ToString::to_string)
                    .unwrap_or_default();
                self.emit_constant(Value::String(first), LINE);
                for (i, expr) in t.expressions.iter().enumerate() {
                    self.compile_expr_oast(expr)?;
                    self.emit(Op::Add, LINE);
                    let tail = t
                        .quasis
                        .get(i + 1)
                        .and_then(|q| q.value.cooked.as_ref())
                        .map(ToString::to_string)
                        .unwrap_or_default();
                    self.emit_constant(Value::String(tail), LINE);
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
                self.emit(Op::CallIC, LINE);
                self.emit_byte(arg_count as u8, LINE);
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
                self.emit_byte(idx as u8, LINE);
            }
            OExpr::FunctionExpression(f) => {
                let name = "<anonymous>";
                let self_name = f.id.as_ref().map(|id| id.name.as_str());
                let (proto, _env_len) = self.compile_function(name, self_name, f, None, false)?;
                let idx = self.chunk.add_function(proto);
                self.emit(Op::MakeClosure, LINE);
                self.emit_byte(idx as u8, LINE);
            }
            OExpr::ObjectExpression(obj) => {
                let mut count = 0usize;
                let mut computed_indices: Vec<usize> = Vec::new();
                let mut accessor_indices: Vec<usize> = Vec::new();
                for (pi, prop) in obj.properties.iter().enumerate() {
                    if let oast::ObjectPropertyKind::ObjectProperty(p) = prop {
                        if p.computed {
                            if !matches!(p.kind, oast::PropertyKind::Init) {
                                accessor_indices.push(pi);
                                continue;
                            }
                            let key_debug = format!("{:?}", p.key);
                            if key_debug.contains("Symbol") && key_debug.contains("iterator") {
                                self.emit_constant(
                                    Value::Symbol(
                                        crate::value::SymbolId::well_known_iterator(),
                                        Some("Symbol.iterator".to_string()),
                                    ),
                                    LINE,
                                );
                                if p.shorthand {
                                    match &p.key {
                                        oast::PropertyKey::StaticIdentifier(ident) => {
                                            self.emit_get_binding(ident.name.as_str());
                                        }
                                        oast::PropertyKey::Identifier(ident) => {
                                            self.emit_get_binding(ident.name.as_str());
                                        }
                                        _ => {
                                            self.emit(Op::Undefined, LINE);
                                        }
                                    }
                                } else {
                                    self.compile_expr_oast(&p.value)?;
                                }
                                count += 1;
                                continue;
                            }
                            computed_indices.push(pi);
                            continue;
                        }
                        if !matches!(p.kind, oast::PropertyKind::Init) {
                            accessor_indices.push(pi);
                            continue;
                        }
                        let key = match &p.key {
                            oast::PropertyKey::StaticIdentifier(ident) => ident.name.to_string(),
                            oast::PropertyKey::Identifier(ident) => ident.name.to_string(),
                            oast::PropertyKey::StringLiteral(s) => s.value.to_string(),
                            oast::PropertyKey::NumericLiteral(n) => n.value.to_string(),
                            _ => continue,
                        };
                        self.emit_constant(Value::String(key), LINE);
                        if p.shorthand {
                            match &p.key {
                                oast::PropertyKey::StaticIdentifier(ident) => {
                                    self.emit_get_binding(ident.name.as_str());
                                }
                                oast::PropertyKey::Identifier(ident) => {
                                    self.emit_get_binding(ident.name.as_str());
                                }
                                _ => {
                                    self.emit(Op::Undefined, LINE);
                                }
                            }
                        } else {
                            self.compile_expr_oast(&p.value)?;
                        }
                        count += 1;
                    }
                }
                self.emit(Op::BuildObject, LINE);
                self.emit_byte(count as u8, LINE);

                for idx in computed_indices {
                    if let oast::ObjectPropertyKind::ObjectProperty(p) = &obj.properties[idx] {
                        self.emit(Op::Dup, LINE);

                        let key_expr = p
                            .key
                            .as_expression()
                            .expect("computed key must be an expression");
                        self.compile_expr_oast(key_expr)?;

                        if p.shorthand {
                            match &p.key {
                                oast::PropertyKey::StaticIdentifier(ident) => {
                                    self.emit_get_binding(ident.name.as_str());
                                }
                                oast::PropertyKey::Identifier(ident) => {
                                    self.emit_get_binding(ident.name.as_str());
                                }
                                _ => {
                                    self.emit(Op::Undefined, LINE);
                                }
                            }
                        } else {
                            self.compile_expr_oast(&p.value)?;
                        }

                        self.emit(Op::SetProperty, LINE);
                        self.emit(Op::Pop, LINE);
                    }
                }

                for idx in accessor_indices {
                    if let oast::ObjectPropertyKind::ObjectProperty(p) = &obj.properties[idx] {
                        let key_value = if p.computed {
                            None
                        } else {
                            Some(match &p.key {
                                oast::PropertyKey::StaticIdentifier(ident) => {
                                    Value::String(ident.name.to_string())
                                }
                                oast::PropertyKey::Identifier(ident) => {
                                    Value::String(ident.name.to_string())
                                }
                                oast::PropertyKey::StringLiteral(s) => {
                                    Value::String(s.value.to_string())
                                }
                                oast::PropertyKey::NumericLiteral(n) => {
                                    Value::String(n.value.to_string())
                                }
                                _ => {
                                    return Err(crate::errors::CompilerError::from(
                                        "unsupported accessor key",
                                    ));
                                }
                            })
                        };
                        self.emit(Op::Dup, LINE);
                        self.emit_get_builtin_name("objectDefineProperty");
                        self.emit(Op::Swap, LINE);
                        if let Some(ref key) = key_value {
                            self.emit_constant(key.clone(), LINE);
                        } else {
                            let key_expr = p
                                .key
                                .as_expression()
                                .expect("computed accessor key must be an expression");
                            self.compile_expr_oast(key_expr)?;
                        }

                        let descriptor_prop_name = match p.kind {
                            oast::PropertyKind::Get => "get",
                            oast::PropertyKind::Set => "set",
                            oast::PropertyKind::Init => unreachable!(),
                        };
                        let method_name_owned = if let Some(Value::String(s)) = &key_value {
                            s.clone()
                        } else {
                            "<accessor>".to_string()
                        };
                        self.emit_constant(Value::String(descriptor_prop_name.to_string()), LINE);
                        if let oast::Expression::FunctionExpression(f) = &p.value {
                            let self_name = f.id.as_ref().map(|id| id.name.as_str());
                            let (proto, _env_len) = self.compile_function(
                                &method_name_owned,
                                self_name,
                                f,
                                None,
                                false,
                            )?;
                            let idx = self.chunk.add_function(proto);
                            self.emit(Op::MakeClosure, LINE);
                            self.emit_byte(idx as u8, LINE);
                        } else {
                            self.compile_expr_oast(&p.value)?;
                        }
                        self.emit_constant(Value::String("enumerable".to_string()), LINE);
                        self.emit_constant(Value::Bool(true), LINE);
                        self.emit_constant(Value::String("configurable".to_string()), LINE);
                        self.emit_constant(Value::Bool(true), LINE);
                        self.emit(Op::BuildObject, LINE);
                        self.emit_byte(3, LINE);
                        self.emit(Op::CallIC, LINE);
                        self.emit_byte(3, LINE);
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
                                self.emit(Op::Undefined, LINE);
                            }
                            _ => {
                                if let Some(expr) = el.as_expression() {
                                    self.compile_expr_oast(expr)?;
                                } else {
                                    self.emit(Op::Undefined, LINE);
                                }
                            }
                        }
                    }
                    self.emit(Op::BuildArray, LINE);
                    self.emit_byte(arr.elements.len() as u8, LINE);
                    for el in &arr.elements {
                        if matches!(el, oast::ArrayExpressionElement::SpreadElement(_)) {
                            self.emit(Op::True, LINE);
                        } else {
                            self.emit(Op::False, LINE);
                        }
                    }
                    self.emit(Op::BuildArray, LINE);
                    self.emit_byte(arr.elements.len() as u8, LINE);
                    self.emit(Op::ArrayFromSpread, LINE);
                    self.emit_byte(arr.elements.len() as u8, LINE);
                } else {
                    for el in &arr.elements {
                        if let Some(expr) = el.as_expression() {
                            self.compile_expr_oast(expr)?;
                        } else {
                            self.emit(Op::Undefined, LINE);
                        }
                    }
                    self.emit(Op::BuildArray, LINE);
                    self.emit_byte(arr.elements.len() as u8, LINE);
                }
            }
            OExpr::Identifier(id) => {
                let name = id.name.as_str();
                self.emit_get_binding(name);
            }
            OExpr::AssignmentExpression(a) => {
                let target = a.left.as_simple_assignment_target().ok_or_else(|| {
                    crate::errors::CompilerError::from("unsupported assignment target")
                })?;
                match a.operator {
                    oast::AssignmentOperator::Assign => match target {
                        oast::SimpleAssignmentTarget::AssignmentTargetIdentifier(id) => {
                            let name = id.name.as_str();
                            if let Some(err) =
                                Self::strict_binding_name_error(self.strict_context, name)
                            {
                                return Err(err);
                            }
                            let is_imported = self
                                .current_module
                                .as_ref()
                                .is_some_and(|c| c.imports.contains_key(name));
                            self.compile_expr_oast(&a.right)?;
                            self.emit(Op::Dup, LINE);
                            if let Some(slot) = self.resolve_local(name) {
                                self.emit(Op::SetLocal, LINE);
                                self.emit_byte(slot as u8, LINE);
                                if self.local_mirrors_global(slot) {
                                    self.emit(Op::GetLocal, LINE);
                                    self.emit_byte(slot as u8, LINE);
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
                                    let cname_idx =
                                        self.chunk.add_constant(Value::String(name.to_string()));
                                    self.emit(Op::SetModuleExport, LINE);
                                    self.emit_byte(module_index as u8, LINE);
                                    self.emit_byte(cname_idx as u8, LINE);
                                }
                            } else if let Some(upvalue) = self.resolve_upvalue(name) {
                                self.emit(Op::SetUpvalue, LINE);
                                self.emit_byte(upvalue, LINE);
                            } else if is_imported {
                                self.emit_throw_type_error("Cannot assign to imported binding");
                            } else {
                                self.emit_set_global_name(name);
                            }
                        }
                        oast::SimpleAssignmentTarget::StaticMemberExpression(m) => {
                            if matches!(&m.object, OExpr::Super(_)) {
                                self.emit_super_property_set_static(
                                    m.property.name.as_str(),
                                    &a.right,
                                )?;
                            } else {
                                self.compile_expr_oast(&m.object)?;
                                self.emit_constant(
                                    Value::String(m.property.name.to_string()),
                                    LINE,
                                );
                                self.compile_expr_oast(&a.right)?;
                                self.emit(Op::SetPropertyIC, LINE);
                                let slot = self.alloc_ic_slot()?;
                                self.emit_byte(slot, LINE);
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
                            if let Some(err) =
                                Self::strict_binding_name_error(self.strict_context, name)
                            {
                                return Err(err);
                            }
                            let is_imported = self
                                .current_module
                                .as_ref()
                                .is_some_and(|c| c.imports.contains_key(name));
                            if let Some(slot) = self.resolve_local(name) {
                                self.emit(Op::GetLocal, LINE);
                                self.emit_byte(slot as u8, LINE);
                            } else if let Some(upvalue) = self.resolve_upvalue(name) {
                                self.emit(Op::GetUpvalue, LINE);
                                self.emit_byte(upvalue, LINE);
                            } else if is_imported {
                                self.emit_get_binding(name);
                            } else {
                                self.emit_get_global_name(name);
                            }
                            self.compile_expr_oast(&a.right)?;
                            match a.operator {
                                oast::AssignmentOperator::Addition => self.emit(Op::Add, LINE),
                                oast::AssignmentOperator::Subtraction => self.emit(Op::Sub, LINE),
                                oast::AssignmentOperator::Multiplication => {
                                    self.emit(Op::Mul, LINE)
                                }
                                oast::AssignmentOperator::Division => self.emit(Op::Div, LINE),
                                oast::AssignmentOperator::Remainder => self.emit(Op::Mod, LINE),
                                oast::AssignmentOperator::Exponential => self.emit(Op::Pow, LINE),
                                oast::AssignmentOperator::BitwiseAnd => self.emit(Op::BitAnd, LINE),
                                oast::AssignmentOperator::BitwiseOR => self.emit(Op::BitOr, LINE),
                                oast::AssignmentOperator::BitwiseXOR => self.emit(Op::BitXor, LINE),
                                oast::AssignmentOperator::ShiftLeft => self.emit(Op::Shl, LINE),
                                oast::AssignmentOperator::ShiftRight => self.emit(Op::Shr, LINE),
                                oast::AssignmentOperator::ShiftRightZeroFill => {
                                    self.emit(Op::Ushr, LINE)
                                }
                                _ => {}
                            }
                            self.emit(Op::Dup, LINE);
                            if let Some(slot) = self.resolve_local(name) {
                                self.emit(Op::SetLocal, LINE);
                                self.emit_byte(slot as u8, LINE);
                                if self.local_mirrors_global(slot) {
                                    self.emit(Op::GetLocal, LINE);
                                    self.emit_byte(slot as u8, LINE);
                                    self.emit_set_global_name(name);
                                }
                            } else if let Some(upvalue) = self.resolve_upvalue(name) {
                                self.emit(Op::SetUpvalue, LINE);
                                self.emit_byte(upvalue, LINE);
                            } else if is_imported {
                                self.emit_throw_type_error("Cannot assign to imported binding");
                            } else {
                                self.emit_set_global_name(name);
                            }
                        }
                        oast::SimpleAssignmentTarget::StaticMemberExpression(m) => {
                            if matches!(&m.object, OExpr::Super(_)) {
                                self.emit(Op::GetThis, LINE);
                                self.emit(Op::Dup, LINE);
                                self.emit_constant(
                                    Value::String(m.property.name.to_string()),
                                    LINE,
                                );
                                self.emit(Op::GetSuperProperty, LINE);
                                self.compile_expr_oast(&a.right)?;
                                self.emit_compound_assignment_op(a.operator);
                                self.emit_constant(
                                    Value::String(m.property.name.to_string()),
                                    LINE,
                                );
                                self.emit(Op::Swap, LINE);
                                self.emit(Op::SetSuperProperty, LINE);
                                self.emit(Op::Swap, LINE);
                                self.emit(Op::Pop, LINE);
                            } else {
                                let (object_slot, key_slot) =
                                    self.compile_static_member_reference(m)?;
                                self.emit_reference_get(object_slot, key_slot, true)?;
                                self.compile_expr_oast(&a.right)?;
                                self.emit_compound_assignment_op(a.operator);
                                let value_slot = self.alloc_temp_local("ref_value");
                                self.emit(Op::SetLocal, LINE);
                                self.emit_byte(value_slot as u8, LINE);
                                self.emit_reference_set_from_slot(
                                    object_slot,
                                    key_slot,
                                    value_slot,
                                    true,
                                )?;
                            }
                        }
                        oast::SimpleAssignmentTarget::ComputedMemberExpression(m) => {
                            if matches!(&m.object, OExpr::Super(_)) {
                                let key_slot = self.alloc_temp_local("super_key");
                                let receiver_slot = self.alloc_temp_local("super_receiver");
                                self.emit(Op::GetThis, LINE);
                                self.emit(Op::SetLocal, LINE);
                                self.emit_byte(receiver_slot as u8, LINE);
                                self.emit(Op::GetLocal, LINE);
                                self.emit_byte(receiver_slot as u8, LINE);
                                self.emit(Op::Dup, LINE);
                                self.compile_expr_oast(&m.expression)?;
                                self.emit(Op::SetLocal, LINE);
                                self.emit_byte(key_slot as u8, LINE);
                                self.emit(Op::GetLocal, LINE);
                                self.emit_byte(key_slot as u8, LINE);
                                self.emit(Op::GetSuperProperty, LINE);
                                self.compile_expr_oast(&a.right)?;
                                self.emit_compound_assignment_op(a.operator);
                                self.emit(Op::GetLocal, LINE);
                                self.emit_byte(key_slot as u8, LINE);
                                self.emit(Op::Swap, LINE);
                                self.emit(Op::SetSuperProperty, LINE);
                                self.emit(Op::Swap, LINE);
                                self.emit(Op::Pop, LINE);
                            } else {
                                let (object_slot, key_slot) =
                                    self.compile_computed_member_reference(m)?;
                                self.emit_reference_get(object_slot, key_slot, false)?;
                                self.compile_expr_oast(&a.right)?;
                                self.emit_compound_assignment_op(a.operator);
                                let value_slot = self.alloc_temp_local("ref_value");
                                self.emit(Op::SetLocal, LINE);
                                self.emit_byte(value_slot as u8, LINE);
                                self.emit_reference_set_from_slot(
                                    object_slot,
                                    key_slot,
                                    value_slot,
                                    false,
                                )?;
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
                                self.emit(Op::GetLocal, LINE);
                                self.emit_byte(slot as u8, LINE);
                                self.emit(Op::Dup, LINE);
                                self.emit(Op::JumpIfFalse, LINE);
                                let jump_end = self.chunk.code.len();
                                self.emit_byte(0, LINE);
                                self.emit_byte(0, LINE);
                                self.emit(Op::Pop, LINE);
                                self.compile_expr_oast(&a.right)?;
                                self.emit(Op::Dup, LINE);
                                self.emit(Op::SetLocal, LINE);
                                self.emit_byte(slot as u8, LINE);
                                if self.local_mirrors_global(slot) {
                                    self.emit(Op::GetLocal, LINE);
                                    self.emit_byte(slot as u8, LINE);
                                    self.emit_set_global_name(name);
                                }
                                self.patch_jump(jump_end);
                            } else if let Some(upvalue) = self.resolve_upvalue(name) {
                                self.emit(Op::GetUpvalue, LINE);
                                self.emit_byte(upvalue, LINE);
                                self.emit(Op::Dup, LINE);
                                self.emit(Op::JumpIfFalse, LINE);
                                let jump_end = self.chunk.code.len();
                                self.emit_byte(0, LINE);
                                self.emit_byte(0, LINE);
                                self.emit(Op::Pop, LINE);
                                self.compile_expr_oast(&a.right)?;
                                self.emit(Op::Dup, LINE);
                                self.emit(Op::SetUpvalue, LINE);
                                self.emit_byte(upvalue, LINE);
                                self.patch_jump(jump_end);
                            } else {
                                self.emit_get_global_name(name);
                                self.emit(Op::Dup, LINE);
                                self.emit(Op::JumpIfFalse, LINE);
                                let jump_end = self.chunk.code.len();
                                self.emit_byte(0, LINE);
                                self.emit_byte(0, LINE);
                                self.emit(Op::Pop, LINE);
                                self.compile_expr_oast(&a.right)?;
                                self.emit(Op::Dup, LINE);
                                self.emit_set_global_name(name);
                                self.patch_jump(jump_end);
                            }
                        }
                        oast::SimpleAssignmentTarget::StaticMemberExpression(m) => {
                            let (object_slot, key_slot) =
                                self.compile_static_member_reference(m)?;
                            self.emit_reference_get(object_slot, key_slot, true)?;
                            self.emit(Op::Dup, LINE);
                            self.emit(Op::JumpIfFalse, LINE);
                            let jump_end = self.chunk.code.len();
                            self.emit_byte(0, LINE);
                            self.emit_byte(0, LINE);
                            self.emit(Op::Pop, LINE);
                            self.compile_expr_oast(&a.right)?;
                            let value_slot = self.alloc_temp_local("ref_value");
                            self.emit(Op::SetLocal, LINE);
                            self.emit_byte(value_slot as u8, LINE);
                            self.emit_reference_set_from_slot(
                                object_slot,
                                key_slot,
                                value_slot,
                                true,
                            )?;
                            self.patch_jump(jump_end);
                        }
                        oast::SimpleAssignmentTarget::ComputedMemberExpression(m) => {
                            let (object_slot, key_slot) =
                                self.compile_computed_member_reference(m)?;
                            self.emit_reference_get(object_slot, key_slot, false)?;
                            self.emit(Op::Dup, LINE);
                            self.emit(Op::JumpIfFalse, LINE);
                            let jump_end = self.chunk.code.len();
                            self.emit_byte(0, LINE);
                            self.emit_byte(0, LINE);
                            self.emit(Op::Pop, LINE);
                            self.compile_expr_oast(&a.right)?;
                            let value_slot = self.alloc_temp_local("ref_value");
                            self.emit(Op::SetLocal, LINE);
                            self.emit_byte(value_slot as u8, LINE);
                            self.emit_reference_set_from_slot(
                                object_slot,
                                key_slot,
                                value_slot,
                                false,
                            )?;
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
                                self.emit(Op::GetLocal, LINE);
                                self.emit_byte(slot as u8, LINE);
                                self.emit(Op::Dup, LINE);
                                self.emit(Op::JumpIfTrue, LINE);
                                let jump_end = self.chunk.code.len();
                                self.emit_byte(0, LINE);
                                self.emit_byte(0, LINE);
                                self.emit(Op::Pop, LINE);
                                self.compile_expr_oast(&a.right)?;
                                self.emit(Op::Dup, LINE);
                                self.emit(Op::SetLocal, LINE);
                                self.emit_byte(slot as u8, LINE);
                                if self.local_mirrors_global(slot) {
                                    self.emit(Op::GetLocal, LINE);
                                    self.emit_byte(slot as u8, LINE);
                                    self.emit_set_global_name(name);
                                }
                                self.patch_jump(jump_end);
                            } else if let Some(upvalue) = self.resolve_upvalue(name) {
                                self.emit(Op::GetUpvalue, LINE);
                                self.emit_byte(upvalue, LINE);
                                self.emit(Op::Dup, LINE);
                                self.emit(Op::JumpIfTrue, LINE);
                                let jump_end = self.chunk.code.len();
                                self.emit_byte(0, LINE);
                                self.emit_byte(0, LINE);
                                self.emit(Op::Pop, LINE);
                                self.compile_expr_oast(&a.right)?;
                                self.emit(Op::Dup, LINE);
                                self.emit(Op::SetUpvalue, LINE);
                                self.emit_byte(upvalue, LINE);
                                self.patch_jump(jump_end);
                            } else {
                                self.emit_get_global_name(name);
                                self.emit(Op::Dup, LINE);
                                self.emit(Op::JumpIfTrue, LINE);
                                let jump_end = self.chunk.code.len();
                                self.emit_byte(0, LINE);
                                self.emit_byte(0, LINE);
                                self.emit(Op::Pop, LINE);
                                self.compile_expr_oast(&a.right)?;
                                self.emit(Op::Dup, LINE);
                                self.emit_set_global_name(name);
                                self.patch_jump(jump_end);
                            }
                        }
                        oast::SimpleAssignmentTarget::StaticMemberExpression(m) => {
                            let (object_slot, key_slot) =
                                self.compile_static_member_reference(m)?;
                            self.emit_reference_get(object_slot, key_slot, true)?;
                            self.emit(Op::Dup, LINE);
                            self.emit(Op::JumpIfTrue, LINE);
                            let jump_end = self.chunk.code.len();
                            self.emit_byte(0, LINE);
                            self.emit_byte(0, LINE);
                            self.emit(Op::Pop, LINE);
                            self.compile_expr_oast(&a.right)?;
                            let value_slot = self.alloc_temp_local("ref_value");
                            self.emit(Op::SetLocal, LINE);
                            self.emit_byte(value_slot as u8, LINE);
                            self.emit_reference_set_from_slot(
                                object_slot,
                                key_slot,
                                value_slot,
                                true,
                            )?;
                            self.patch_jump(jump_end);
                        }
                        oast::SimpleAssignmentTarget::ComputedMemberExpression(m) => {
                            let (object_slot, key_slot) =
                                self.compile_computed_member_reference(m)?;
                            self.emit_reference_get(object_slot, key_slot, false)?;
                            self.emit(Op::Dup, LINE);
                            self.emit(Op::JumpIfTrue, LINE);
                            let jump_end = self.chunk.code.len();
                            self.emit_byte(0, LINE);
                            self.emit_byte(0, LINE);
                            self.emit(Op::Pop, LINE);
                            self.compile_expr_oast(&a.right)?;
                            let value_slot = self.alloc_temp_local("ref_value");
                            self.emit(Op::SetLocal, LINE);
                            self.emit_byte(value_slot as u8, LINE);
                            self.emit_reference_set_from_slot(
                                object_slot,
                                key_slot,
                                value_slot,
                                false,
                            )?;
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
                                self.emit(Op::GetLocal, LINE);
                                self.emit_byte(slot as u8, LINE);
                                self.emit(Op::Dup, LINE);
                                self.emit(Op::JumpIfNullish, LINE);
                                let jump_end = self.chunk.code.len();
                                self.emit_byte(0, LINE);
                                self.emit_byte(0, LINE);
                                self.emit(Op::Jump, LINE);
                                let after_store = self.chunk.code.len();
                                self.emit_byte(0, LINE);
                                self.emit_byte(0, LINE);
                                self.patch_jump(jump_end);
                                self.emit(Op::Pop, LINE);
                                self.compile_expr_oast(&a.right)?;
                                self.emit(Op::Dup, LINE);
                                self.emit(Op::SetLocal, LINE);
                                self.emit_byte(slot as u8, LINE);
                                if self.local_mirrors_global(slot) {
                                    self.emit(Op::GetLocal, LINE);
                                    self.emit_byte(slot as u8, LINE);
                                    self.emit_set_global_name(name);
                                }
                                self.patch_jump(after_store);
                            } else if let Some(upvalue) = self.resolve_upvalue(name) {
                                self.emit(Op::GetUpvalue, LINE);
                                self.emit_byte(upvalue, LINE);
                                self.emit(Op::Dup, LINE);
                                self.emit(Op::JumpIfNullish, LINE);
                                let jump_end = self.chunk.code.len();
                                self.emit_byte(0, LINE);
                                self.emit_byte(0, LINE);
                                self.emit(Op::Jump, LINE);
                                let after_store = self.chunk.code.len();
                                self.emit_byte(0, LINE);
                                self.emit_byte(0, LINE);
                                self.patch_jump(jump_end);
                                self.emit(Op::Pop, LINE);
                                self.compile_expr_oast(&a.right)?;
                                self.emit(Op::Dup, LINE);
                                self.emit(Op::SetUpvalue, LINE);
                                self.emit_byte(upvalue, LINE);
                                self.patch_jump(after_store);
                            } else {
                                self.emit_get_global_name(name);
                                self.emit(Op::Dup, LINE);
                                self.emit(Op::JumpIfNullish, LINE);
                                let jump_end = self.chunk.code.len();
                                self.emit_byte(0, LINE);
                                self.emit_byte(0, LINE);
                                self.emit(Op::Jump, LINE);
                                let after_store = self.chunk.code.len();
                                self.emit_byte(0, LINE);
                                self.emit_byte(0, LINE);
                                self.patch_jump(jump_end);
                                self.emit(Op::Pop, LINE);
                                self.compile_expr_oast(&a.right)?;
                                self.emit(Op::Dup, LINE);
                                self.emit_set_global_name(name);
                                self.patch_jump(after_store);
                            }
                        }
                        oast::SimpleAssignmentTarget::StaticMemberExpression(m) => {
                            let (object_slot, key_slot) =
                                self.compile_static_member_reference(m)?;
                            self.emit_reference_get(object_slot, key_slot, true)?;
                            self.emit(Op::Dup, LINE);
                            self.emit(Op::JumpIfNullish, LINE);
                            let jump_end = self.chunk.code.len();
                            self.emit_byte(0, LINE);
                            self.emit_byte(0, LINE);
                            self.emit(Op::Jump, LINE);
                            let after_store = self.chunk.code.len();
                            self.emit_byte(0, LINE);
                            self.emit_byte(0, LINE);
                            self.patch_jump(jump_end);
                            self.emit(Op::Pop, LINE);
                            self.compile_expr_oast(&a.right)?;
                            let value_slot = self.alloc_temp_local("ref_value");
                            self.emit(Op::SetLocal, LINE);
                            self.emit_byte(value_slot as u8, LINE);
                            self.emit_reference_set_from_slot(
                                object_slot,
                                key_slot,
                                value_slot,
                                true,
                            )?;
                            self.patch_jump(after_store);
                        }
                        oast::SimpleAssignmentTarget::ComputedMemberExpression(m) => {
                            let (object_slot, key_slot) =
                                self.compile_computed_member_reference(m)?;
                            self.emit_reference_get(object_slot, key_slot, false)?;
                            self.emit(Op::Dup, LINE);
                            self.emit(Op::JumpIfNullish, LINE);
                            let jump_end = self.chunk.code.len();
                            self.emit_byte(0, LINE);
                            self.emit_byte(0, LINE);
                            self.emit(Op::Jump, LINE);
                            let after_store = self.chunk.code.len();
                            self.emit_byte(0, LINE);
                            self.emit_byte(0, LINE);
                            self.patch_jump(jump_end);
                            self.emit(Op::Pop, LINE);
                            self.compile_expr_oast(&a.right)?;
                            let value_slot = self.alloc_temp_local("ref_value");
                            self.emit(Op::SetLocal, LINE);
                            self.emit_byte(value_slot as u8, LINE);
                            self.emit_reference_set_from_slot(
                                object_slot,
                                key_slot,
                                value_slot,
                                false,
                            )?;
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
                        } else if self.resolve_upvalue(name).is_some() {
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
                        self.emit_constant(Value::String(m.property.name.to_string()), LINE);
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
                            self.emit_constant(Value::String(id.name.to_string()), LINE);
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
                        oast::UnaryOperator::Void => {
                            self.emit(Op::Pop, LINE);
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
                        if let Some(slot) = self.resolve_local(name) {
                            self.emit(Op::GetLocal, LINE);
                            self.emit_byte(slot as u8, LINE);
                            self.emit(Op::Neg, LINE);
                            self.emit(Op::Neg, LINE);
                            if !u.prefix {
                                self.emit(Op::Dup, LINE);
                            }
                            self.emit_constant(Value::Number(1.0), LINE);
                            match u.operator {
                                oast::UpdateOperator::Increment => self.emit(Op::Add, LINE),
                                oast::UpdateOperator::Decrement => self.emit(Op::Sub, LINE),
                            }
                            if u.prefix {
                                self.emit(Op::Dup, LINE);
                            }
                            self.emit(Op::SetLocal, LINE);
                            self.emit_byte(slot as u8, LINE);
                            if self.local_mirrors_global(slot) {
                                self.emit(Op::GetLocal, LINE);
                                self.emit_byte(slot as u8, LINE);
                                self.emit_set_global_name(name);
                            }
                        } else if let Some(upvalue) = self.resolve_upvalue(name) {
                            self.emit(Op::GetUpvalue, LINE);
                            self.emit_byte(upvalue, LINE);
                            self.emit(Op::Neg, LINE);
                            self.emit(Op::Neg, LINE);
                            if !u.prefix {
                                self.emit(Op::Dup, LINE);
                            }
                            self.emit_constant(Value::Number(1.0), LINE);
                            match u.operator {
                                oast::UpdateOperator::Increment => self.emit(Op::Add, LINE),
                                oast::UpdateOperator::Decrement => self.emit(Op::Sub, LINE),
                            }
                            if u.prefix {
                                self.emit(Op::Dup, LINE);
                            }
                            self.emit(Op::SetUpvalue, LINE);
                            self.emit_byte(upvalue, LINE);
                        } else {
                            self.emit_get_global_name(name);
                            self.emit(Op::Neg, LINE);
                            self.emit(Op::Neg, LINE);
                            if !u.prefix {
                                self.emit(Op::Dup, LINE);
                            }
                            self.emit_constant(Value::Number(1.0), LINE);
                            match u.operator {
                                oast::UpdateOperator::Increment => self.emit(Op::Add, LINE),
                                oast::UpdateOperator::Decrement => self.emit(Op::Sub, LINE),
                            }
                            if u.prefix {
                                self.emit(Op::Dup, LINE);
                            }
                            self.emit_set_global_name(name);
                        }
                    }
                    OTarget::StaticMemberExpression(m) => {
                        let (object_slot, key_slot) = self.compile_static_member_reference(m)?;
                        self.emit_reference_get(object_slot, key_slot, true)?;
                        let old_slot = self.alloc_temp_local("ref_old");
                        self.emit(Op::SetLocal, LINE);
                        self.emit_byte(old_slot as u8, LINE);
                        self.emit(Op::GetLocal, LINE);
                        self.emit_byte(old_slot as u8, LINE);
                        self.emit(Op::Neg, LINE);
                        self.emit(Op::Neg, LINE);
                        if !u.prefix {
                            self.emit(Op::Dup, LINE);
                        }
                        self.emit_constant(Value::Number(1.0), LINE);
                        match u.operator {
                            oast::UpdateOperator::Increment => self.emit(Op::Add, LINE),
                            oast::UpdateOperator::Decrement => self.emit(Op::Sub, LINE),
                        }
                        let new_slot = self.alloc_temp_local("ref_new");
                        self.emit(Op::SetLocal, LINE);
                        self.emit_byte(new_slot as u8, LINE);
                        self.emit_reference_set_from_slot(object_slot, key_slot, new_slot, true)?;
                        if !u.prefix {
                            self.emit(Op::Pop, LINE);
                        }
                    }
                    OTarget::ComputedMemberExpression(m) => {
                        let (object_slot, key_slot) = self.compile_computed_member_reference(m)?;
                        self.emit_reference_get(object_slot, key_slot, false)?;
                        let old_slot = self.alloc_temp_local("ref_old");
                        self.emit(Op::SetLocal, LINE);
                        self.emit_byte(old_slot as u8, LINE);
                        self.emit(Op::GetLocal, LINE);
                        self.emit_byte(old_slot as u8, LINE);
                        self.emit(Op::Neg, LINE);
                        self.emit(Op::Neg, LINE);
                        if !u.prefix {
                            self.emit(Op::Dup, LINE);
                        }
                        self.emit_constant(Value::Number(1.0), LINE);
                        match u.operator {
                            oast::UpdateOperator::Increment => self.emit(Op::Add, LINE),
                            oast::UpdateOperator::Decrement => self.emit(Op::Sub, LINE),
                        }
                        let new_slot = self.alloc_temp_local("ref_new");
                        self.emit(Op::SetLocal, LINE);
                        self.emit_byte(new_slot as u8, LINE);
                        self.emit_reference_set_from_slot(object_slot, key_slot, new_slot, false)?;
                        if !u.prefix {
                            self.emit(Op::Pop, LINE);
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
                    self.compile_expr_oast(&b.right)?;
                    self.compile_expr_oast(&b.left)?;
                    self.emit(Op::HasProperty, LINE);
                    return Ok(());
                }
                self.compile_expr_oast(&b.left)?;
                self.compile_expr_oast(&b.right)?;
                match b.operator {
                    oast::BinaryOperator::Equality => self.emit(Op::Eq, LINE),
                    oast::BinaryOperator::StrictEquality => self.emit(Op::StrictEq, LINE),
                    oast::BinaryOperator::Inequality => self.emit(Op::Neq, LINE),
                    oast::BinaryOperator::StrictInequality => self.emit(Op::StrictNeq, LINE),
                    oast::BinaryOperator::LessThan => self.emit(Op::Lt, LINE),
                    oast::BinaryOperator::LessEqualThan => self.emit(Op::Le, LINE),
                    oast::BinaryOperator::GreaterThan => self.emit(Op::Gt, LINE),
                    oast::BinaryOperator::GreaterEqualThan => self.emit(Op::Ge, LINE),
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
                    self.emit(Op::Dup, LINE);
                    self.emit(Op::JumpIfFalse, LINE);
                    let jump_end = self.chunk.code.len();
                    self.emit_byte(0, LINE);
                    self.emit_byte(0, LINE);
                    self.emit(Op::Pop, LINE);
                    self.compile_expr_oast(&l.right)?;
                    self.patch_jump(jump_end);
                }
                oast::LogicalOperator::Or => {
                    self.compile_expr_oast(&l.left)?;
                    self.emit(Op::Dup, LINE);
                    self.emit(Op::JumpIfTrue, LINE);
                    let jump_end = self.chunk.code.len();
                    self.emit_byte(0, LINE);
                    self.emit_byte(0, LINE);
                    self.emit(Op::Pop, LINE);
                    self.compile_expr_oast(&l.right)?;
                    self.patch_jump(jump_end);
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
                    self.compile_expr_oast(&l.right)?;
                    self.patch_jump(jump_end);
                }
            },
            OExpr::ConditionalExpression(c) => {
                if let Some(jump_else) = self.try_emit_compare_jump_if_false(&c.test)? {
                    self.compile_expr_oast(&c.consequent)?;
                    self.emit(Op::Jump, LINE);
                    let jump_end = self.chunk.code.len();
                    self.emit_byte(0, LINE);
                    self.emit_byte(0, LINE);
                    self.patch_jump(jump_else);
                    self.compile_expr_oast(&c.alternate)?;
                    self.patch_jump(jump_end);
                } else {
                    self.compile_expr_oast(&c.test)?;
                    self.emit(Op::JumpIfFalse, LINE);
                    let jump_else = self.chunk.code.len();
                    self.emit_byte(0, LINE);
                    self.emit_byte(0, LINE);
                    self.compile_expr_oast(&c.consequent)?;
                    self.emit(Op::Jump, LINE);
                    let jump_end = self.chunk.code.len();
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
                        self.emit(Op::Pop, LINE);
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
                    self.emit_constant(Value::String(m.property.name.to_string()), LINE);
                    if m.optional {
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
                        self.emit(Op::GetPropertyIC, LINE);
                        let slot = self.alloc_ic_slot()?;
                        self.emit_byte(slot, LINE);
                        self.patch_jump(end);
                    } else {
                        self.emit(Op::GetPropertyIC, LINE);
                        let slot = self.alloc_ic_slot()?;
                        self.emit_byte(slot, LINE);
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
                    self.compile_expr_oast(&m.expression)?;
                    if m.optional {
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
                        self.emit(Op::GetProperty, LINE);
                        self.patch_jump(end);
                    } else {
                        self.emit(Op::GetProperty, LINE);
                    }
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
                            self.emit_constant(Value::String(m.property.name.to_string()), LINE);
                            if m.optional {
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
                                self.emit(Op::GetPropertyIC, LINE);
                                let slot = self.alloc_ic_slot()?;
                                self.emit_byte(slot, LINE);
                                self.patch_jump(end);
                            } else {
                                self.emit(Op::GetPropertyIC, LINE);
                                let slot = self.alloc_ic_slot()?;
                                self.emit_byte(slot, LINE);
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
                            self.compile_expr_oast(&m.expression)?;
                            if m.optional {
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
                                self.emit(Op::GetProperty, LINE);
                                self.patch_jump(end);
                            } else {
                                self.emit(Op::GetProperty, LINE);
                            }
                        }
                    }
                    CE::CallExpression(c) => {
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
                            self.emit(Op::CallMethod, LINE);
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
                            self.emit(Op::CallMethod, LINE);
                            self.emit_byte(arg_count as u8, LINE);
                            return Ok(());
                        }
                        let is_optional_call = c.optional;
                        // Recursively compile the callee (which may be a member expression)
                        self.compile_expr_oast(&c.callee)?;
                        let end = if is_optional_call {
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
                            Some(end)
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
                        if is_optional_call {
                            self.emit(Op::Call, LINE);
                            self.emit_byte(arg_count as u8, LINE);
                            if let Some(end) = end {
                                self.patch_jump(end);
                            }
                        } else {
                            self.emit(Op::CallMethod, LINE);
                            self.emit_byte(arg_count as u8, LINE);
                        }
                    }
                    CE::PrivateFieldExpression(m) => {
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
                            let end_jump = self.chunk.code.len();
                            self.emit_byte(0, LINE);
                            self.emit_byte(0, LINE);
                            self.patch_jump(skip_nullish);
                            let key = self.ensure_private_name_constant(&m.field.name);
                            self.emit_constant(key, LINE);
                            self.emit(Op::GetProperty, LINE);
                            self.patch_jump(end_jump);
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
            OExpr::MetaProperty(m) => {
                if m.meta.name.as_str() == "new" && m.property.name.as_str() == "target" {
                    if !self.allow_new_target {
                        return Err(crate::errors::CompilerError::from(
                            "SyntaxError: new.target is only valid inside a function",
                        ));
                    }
                    self.emit(Op::GetNewTarget, LINE);
                } else {
                    self.emit(Op::Undefined, LINE);
                }
            }
            OExpr::CallExpression(c) => {
                // Handle spread-containing calls first so we don't prematurely lower
                // member/call forms (which would otherwise emit CallMethod/Call
                // before we've had a chance to expand spreads).
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
                    if !self.allow_super {
                        return Err(crate::errors::CompilerError::from(
                            "SyntaxError: super used outside of a method",
                        ));
                    }
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
                    self.emit(Op::CallMethod, LINE);
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
                    self.emit(Op::CallMethod, LINE);
                    self.emit_byte(arg_count as u8, LINE);
                    return Ok(());
                }

                // No spreads: perform the usual lowering for member calls.

                if let OExpr::StaticMemberExpression(m) = &c.callee {
                    // Handle optional calls like obj.method?.()
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
                        self.emit(Op::CallMethod, LINE);
                    }
                    self.emit_byte(arg_count as u8, LINE);
                    return Ok(());
                }

                if let OExpr::ComputedMemberExpression(m) = &c.callee {
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
                        self.emit(Op::CallMethod, LINE);
                    }
                    self.emit_byte(arg_count as u8, LINE);
                    return Ok(());
                }

                if let OExpr::PrivateFieldExpression(m) = &c.callee {
                    // Handle optional private field calls like obj.#method?.()
                    let is_optional_call = c.optional;

                    // Lower this.#private() style calls to preserve receiver.
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
                        self.emit(Op::CallMethod, LINE);
                    }
                    self.emit_byte(arg_count as u8, LINE);
                    return Ok(());
                }

                // Handle optional call on simple identifier like fn?.()
                if c.optional {
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

                // Regular function call without spread
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
                self.emit(Op::CallIC, LINE);
                self.emit_byte(arg_count as u8, LINE);
            }
            OExpr::NewExpression(n) => {
                self.compile_expr_oast(&n.callee)?;
                let mut arg_count = 0usize;
                for arg in &n.arguments {
                    if let Some(expr) = arg.as_expression() {
                        self.compile_expr_oast(expr)?;
                        arg_count += 1;
                    } else {
                        self.emit(Op::Undefined, LINE);
                        arg_count += 1;
                    }
                }
                self.emit(Op::Construct, LINE);
                self.emit_byte(arg_count as u8, LINE);
            }
            OExpr::ImportExpression(i) => {
                self.emit_get_builtin_name("dynamicImport");
                self.compile_expr_oast(&i.source)?;
                if let Some(options) = &i.options {
                    self.compile_expr_oast(options)?;
                } else {
                    self.emit(Op::Undefined, LINE);
                }
                self.emit(Op::CallIC, LINE);
                self.emit_byte(2, LINE);
            }
            OExpr::RegExpLiteral(re) => {
                self.emit_get_global_name("RegExp");
                self.emit_constant(Value::String(re.regex.pattern.text.to_string()), LINE);
                self.emit_constant(Value::String(re.regex.flags.to_string()), LINE);
                self.emit(Op::CallIC, LINE);
                self.emit_byte(2, LINE);
            }
            OExpr::ParenthesizedExpression(p) => {
                self.compile_expr_oast(&p.expression)?;
            }
            OExpr::ClassExpression(c) => {
                // Minimal class expression support: emit a trivial constructor
                // function and push it on the stack. Full class element
                // initialization is not implemented yet; this prevents
                // class-expressions from lowering to `Undefined` which breaks
                // usages like `var C = class {}` followed by `new C()`.
                let name =
                    c.id.as_ref()
                        .map_or_else(|| "<anonymous>".to_string(), |id| id.name.to_string());
                let mut ctor_chunk = Chunk::new(Rc::clone(&self.source_text));
                ctor_chunk.write(Op::Return as u8, LINE);
                let proto = FunctionProto {
                    name,
                    span: c.span,
                    arity: 0,
                    chunk: Rc::new(ctor_chunk),
                    slot_types: Vec::new(),
                    slot_names: Vec::new(),
                    lexical_tdz: Vec::new(),
                    upvalues: Vec::new(),
                    has_rest: false,
                    has_prototype_property: true,
                    captures_this: false,
                    arguments_slot: None,
                    self_binding_slot: None,
                    has_simple_parameter_list: true,
                    return_type: None,
                    is_strict: true,
                    is_constructor: true,
                    is_async: false,
                    is_generator: false,
                };
                let idx = self.chunk.add_function(proto);
                self.emit(Op::MakeClosure, LINE);
                self.emit_byte(idx as u8, LINE);
                // To initialize class elements we need a local slot to hold the
                // constructor while we install methods. Duplicate the ctor and
                // store into a temp local so the original ctor remains on the
                // stack as the expression result.
                let temp_name = format!("__class_ctor_{}", self.locals.len());
                let temp_slot = self.add_local_with_mirror(temp_name, None, false);
                // dup ctor -> [ctor, ctor]
                self.emit(Op::Dup, LINE);
                // set local consumes one ctor -> [ctor]
                self.emit(Op::SetLocal, LINE);
                self.emit_byte(temp_slot as u8, LINE);
                self.emit(Op::Pop, LINE);

                // Emit class elements (methods) similar to declaration lowering.
                let instance_home_slot = if c.body.body.iter().any(|element| {
                    matches!(element, oast::ClassElement::MethodDefinition(md) if !md.r#static)
                }) {
                    self.emit(Op::GetLocal, LINE);
                    self.emit_byte(temp_slot as u8, LINE);
                    self.emit_constant(Value::String("prototype".to_string()), LINE);
                    self.emit(Op::GetProperty, LINE);
                    let home_slot = self.alloc_temp_local("class_home");
                    self.emit(Op::Dup, LINE);
                    self.emit(Op::SetLocal, LINE);
                    self.emit_byte(home_slot as u8, LINE);
                    self.emit(Op::Pop, LINE);
                    Some(home_slot)
                } else {
                    None
                };
                for element in &c.body.body {
                    if let oast::ClassElement::MethodDefinition(md) = element {
                        let target_slot = if md.r#static {
                            temp_slot
                        } else {
                            instance_home_slot.expect("instance home slot")
                        };
                        self.emit_class_member_installation(target_slot, md)?;
                    }
                }
                self.emit(Op::GetLocal, LINE);
                self.emit_byte(temp_slot as u8, LINE);
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
