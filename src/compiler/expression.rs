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
use std::rc::Rc;

const LINE: u32 = 1;

impl super::Compiler {
    fn alloc_temp_local(&mut self, prefix: &str) -> usize {
        self.add_local_with_mirror(format!("__{}_{}", prefix, self.locals.len()), None, false)
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
            OExpr::NumericLiteral(n) => self.emit_constant(Value::Number(n.value), LINE),
            OExpr::BigIntLiteral(n) => {
                let raw =
                    n.raw.as_ref().map(oxc_span::Str::as_str).ok_or_else(|| {
                        crate::errors::CompilerError::from("invalid bigint literal")
                    })?;
                let digits = raw.strip_suffix('n').unwrap_or(raw).replace('_', "");
                let parsed: Option<num_bigint::BigInt> =
                    if let Some(hex) = digits.strip_prefix("0x") {
                        num_bigint::BigInt::parse_bytes(hex.as_bytes(), 16)
                    } else if let Some(bin) = digits.strip_prefix("0b") {
                        num_bigint::BigInt::parse_bytes(bin.as_bytes(), 2)
                    } else if let Some(oct) = digits.strip_prefix("0o") {
                        num_bigint::BigInt::parse_bytes(oct.as_bytes(), 8)
                    } else {
                        num_bigint::BigInt::parse_bytes(digits.as_bytes(), 10)
                    };
                let parsed = parsed
                    .ok_or_else(|| crate::errors::CompilerError::from("invalid bigint literal"))?;
                self.emit_constant(Value::BigInt(parsed), LINE);
            }
            OExpr::StringLiteral(s) => self.emit_constant(Value::String(s.value.to_string()), LINE),
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
                let (proto, _env_len) = self.compile_function(name, f, None)?;
                let idx = self.chunk.add_function(proto);
                self.emit(Op::MakeClosure, LINE);
                self.emit_byte(idx as u8, LINE);
            }
            OExpr::ObjectExpression(obj) => {
                let mut count = 0usize;
                let mut computed_indices: Vec<usize> = Vec::new();
                self.object_expr_had_set_property = false;
                for (pi, prop) in obj.properties.iter().enumerate() {
                    if let oast::ObjectPropertyKind::ObjectProperty(p) = prop {
                        if p.computed {
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
                        self.object_expr_had_set_property = true;
                    }
                }

                if self.object_expr_had_set_property {
                    self.emit(Op::Pop, LINE);
                }
            }
            OExpr::ArrayExpression(arr) => {
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
                            if self
                                .current_module
                                .as_ref()
                                .is_some_and(|c| c.imports.contains_key(name))
                            {
                                return Err(crate::errors::CompilerError::from(
                                    "cannot assign to imported binding",
                                ));
                            }
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
                            } else {
                                // Assignment to a non-local — could be an import binding
                                if let Some((src_idx, exported_name)) = self
                                    .current_module
                                    .as_ref()
                                    .and_then(|c| c.imports.get(name).map(|(i, n)| (*i, n.clone())))
                                {
                                    let cname_idx =
                                        self.chunk.add_constant(Value::String(exported_name));
                                    self.emit(Op::SetModuleExport, LINE);
                                    self.emit_byte(src_idx as u8, LINE);
                                    self.emit_byte(cname_idx as u8, LINE);
                                } else {
                                    self.emit_set_global_name(name);
                                }
                            }
                        }
                        oast::SimpleAssignmentTarget::StaticMemberExpression(m) => {
                            self.compile_expr_oast(&m.object)?;
                            self.emit_constant(Value::String(m.property.name.to_string()), LINE);
                            self.compile_expr_oast(&a.right)?;
                            self.emit(Op::SetPropertyIC, LINE);
                            let slot = self.alloc_ic_slot()?;
                            self.emit_byte(slot, LINE);
                        }
                        oast::SimpleAssignmentTarget::ComputedMemberExpression(m) => {
                            self.compile_expr_oast(&m.object)?;
                            self.compile_expr_oast(&m.expression)?;
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
                            if let Some(slot) = self.resolve_local(name) {
                                self.emit(Op::GetLocal, LINE);
                                self.emit_byte(slot as u8, LINE);
                            } else if let Some(upvalue) = self.resolve_upvalue(name) {
                                self.emit(Op::GetUpvalue, LINE);
                                self.emit_byte(upvalue, LINE);
                            } else if self
                                .current_module
                                .as_ref()
                                .is_some_and(|c| c.imports.contains_key(name))
                            {
                                return Err(crate::errors::CompilerError::from(
                                    "cannot assign to imported binding",
                                ));
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
                            } else {
                                self.emit_set_global_name(name);
                            }
                        }
                        oast::SimpleAssignmentTarget::StaticMemberExpression(m) => {
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
                        oast::SimpleAssignmentTarget::ComputedMemberExpression(m) => {
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
                        _ => {
                            return Err(crate::errors::CompilerError::from(
                                "unsupported assignment target",
                            ));
                        }
                    },
                    _ => {
                        return Err(crate::errors::CompilerError::from(
                            "unsupported assignment operator",
                        ));
                    }
                }
            }
            OExpr::UnaryExpression(u) => match u.operator {
                oast::UnaryOperator::Typeof => {
                    if let OExpr::Identifier(id) = &u.argument {
                        // Preserve `typeof undeclaredIdentifier` semantics.
                        // Normal global lookup now throws ReferenceError for
                        // missing bindings, so use a name-based helper for the
                        // identifier case.
                        let name = id.name.to_string();
                        if self.resolve_local(&name).is_some()
                            || self.resolve_upvalue(&name).is_some()
                        {
                            self.emit_get_global_name("__native_typeof");
                            self.compile_expr_oast(&u.argument)?;
                            self.emit(Op::CallIC, LINE);
                            self.emit_byte(1, LINE);
                        } else {
                            self.emit_get_global_name("__native_typeof_name");
                            self.emit_constant(Value::String(name), LINE);
                            self.emit(Op::CallIC, LINE);
                            self.emit_byte(1, LINE);
                        }
                    } else {
                        self.emit_get_global_name("__native_typeof");
                        self.compile_expr_oast(&u.argument)?;
                        self.emit(Op::CallIC, LINE);
                        self.emit_byte(1, LINE);
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
                            self.emit_get_global_name("globalThis");
                            self.emit_constant(Value::String(id.name.to_string()), LINE);
                            self.emit(Op::DeleteProperty, LINE);
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
                            self.emit(Op::GetLocal, LINE);
                            self.emit_byte(old_slot as u8, LINE);
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
                            self.emit(Op::GetLocal, LINE);
                            self.emit_byte(old_slot as u8, LINE);
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
                    self.emit_get_global_name("__native_instanceof");
                    self.compile_expr_oast(&b.left)?;
                    self.compile_expr_oast(&b.right)?;
                    self.emit(Op::CallIC, LINE);
                    self.emit_byte(2, LINE);
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
                self.compile_expr_oast(&m.object)?;
                self.emit_constant(Value::String(m.property.name.to_string()), LINE);
                self.emit(Op::GetPropertyIC, LINE);
                let slot = self.alloc_ic_slot()?;
                self.emit_byte(slot, LINE);
            }
            OExpr::ComputedMemberExpression(m) => {
                self.compile_expr_oast(&m.object)?;
                self.compile_expr_oast(&m.expression)?;
                self.emit(Op::GetProperty, LINE);
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

                // No spreads: perform the usual lowering for console.log and member calls.

                if let OExpr::StaticMemberExpression(m) = &c.callee
                    && let OExpr::Identifier(obj_ident) = &m.object
                    && obj_ident.name.as_str() == "console"
                    && m.property.name.as_str() == "log"
                {
                    self.emit_get_global_name("__native_print");
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
                    return Ok(());
                }

                if let OExpr::StaticMemberExpression(m) = &c.callee {
                    self.compile_expr_oast(&m.object)?;
                    self.emit(Op::Dup, LINE);
                    self.emit_constant(Value::String(m.property.name.to_string()), LINE);
                    self.emit(Op::GetPropertyIC, LINE);
                    let slot = self.alloc_ic_slot()?;
                    self.emit_byte(slot, LINE);
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

                if let OExpr::ComputedMemberExpression(m) = &c.callee {
                    self.compile_expr_oast(&m.object)?;
                    self.emit(Op::Dup, LINE);
                    self.compile_expr_oast(&m.expression)?;
                    self.emit(Op::GetProperty, LINE);
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

                if let OExpr::PrivateFieldExpression(m) = &c.callee {
                    // Lower this.#private() style calls to preserve receiver.
                    self.compile_expr_oast(&m.object)?;
                    self.emit(Op::Dup, LINE);
                    // private identifier -> synthetic constant
                    let oast::PrivateIdentifier { name, .. } = &m.field;
                    let key = self.ensure_private_name_constant(name);
                    self.emit_constant(key, LINE);
                    self.emit(Op::GetProperty, LINE);
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
                self.emit_get_global_name("__native_dynamic_import");
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

                // Emit class elements (methods) similar to declaration lowering.
                for element in &c.body.body {
                    if let oast::ClassElement::MethodDefinition(md) = element {
                        let method_name = match &md.key {
                            oast::PropertyKey::StaticIdentifier(ident) => {
                                Some(ident.name.to_string())
                            }
                            oast::PropertyKey::Identifier(ident) => Some(ident.name.to_string()),
                            oast::PropertyKey::StringLiteral(s) => Some(s.value.to_string()),
                            oast::PropertyKey::NumericLiteral(n) => Some(n.value.to_string()),
                            oast::PropertyKey::PrivateIdentifier(pid) => Some(pid.name.to_string()),
                            _ => None,
                        };
                        let (proto, _env_len) = self.compile_function(
                            method_name.as_deref().unwrap_or("<method>"),
                            &md.value,
                            None,
                        )?;
                        let fidx = self.chunk.add_function(proto);

                        if md.r#static {
                            // static: install on constructor
                            self.emit(Op::GetLocal, LINE);
                            self.emit_byte(temp_slot as u8, LINE);
                            match &md.key {
                                oast::PropertyKey::PrivateIdentifier(pid) => {
                                    let key = self.ensure_private_name_constant(&pid.name);
                                    self.emit_constant(key, LINE);
                                }
                                _ => {
                                    let key_str = method_name.unwrap_or_else(|| "".to_string());
                                    self.emit_constant(Value::String(key_str), LINE);
                                }
                            }
                            self.emit(Op::MakeClosure, LINE);
                            self.emit_byte(fidx as u8, LINE);
                            self.emit(Op::SetPropertyIC, LINE);
                            let slot = self.alloc_ic_slot()?;
                            self.emit_byte(slot, LINE);
                        } else {
                            // instance: install on constructor.prototype
                            self.emit(Op::GetLocal, LINE);
                            self.emit_byte(temp_slot as u8, LINE);
                            self.emit_constant(Value::String("prototype".to_string()), LINE);
                            self.emit(Op::GetProperty, LINE);
                            match &md.key {
                                oast::PropertyKey::PrivateIdentifier(pid) => {
                                    let key = self.ensure_private_name_constant(&pid.name);
                                    self.emit_constant(key, LINE);
                                }
                                _ => {
                                    let key_str = method_name.unwrap_or_else(|| "".to_string());
                                    self.emit_constant(Value::String(key_str), LINE);
                                }
                            }
                            self.emit(Op::MakeClosure, LINE);
                            self.emit_byte(fidx as u8, LINE);
                            self.emit(Op::SetPropertyIC, LINE);
                            let slot = self.alloc_ic_slot()?;
                            self.emit_byte(slot, LINE);
                        }
                    }
                }
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
