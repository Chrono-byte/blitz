//! Binding phase compilation
//!
//! Handles variable declaration compilation and binding pattern assignment,
//! including support for destructuring patterns (object and array destructuring)
//! in variable declarations.
//!
//! This module is part of the compiler's multi-phase compilation process.
//! The binding phase processes variable declarations and manages the storage
//! of binding identifiers in local scopes.

use crate::bytecode::Op;
use crate::value::Value;
use oxc_ast::ast as oast;

const LINE: u32 = 1;

impl super::Compiler {
    /// Compile a variable declaration statement.
    ///
    /// Processes all declarators in the declaration, evaluating their initializers
    /// (or `undefined` if no initializer) and assigning them to their binding patterns.
    pub(super) fn compile_variable_declaration_oast(
        &mut self,
        v: &oast::VariableDeclaration<'_>,
    ) -> Result<(), crate::errors::CompilerError> {
        for dec in &v.declarations {
            if let Some(init) = &dec.init {
                self.compile_expr_oast(init)?;
            } else {
                self.emit(Op::Undefined, LINE);
            }
            self.compile_binding_pattern_assignment_oast(&dec.id, v.kind)?;
        }

        Ok(())
    }

    /// Convert an object binding key to a string value.
    ///
    /// Extracts the string representation from various types of property keys
    /// used in destructuring patterns. Returns `None` for computed properties
    /// that cannot be statically determined.
    pub(super) fn binding_property_key_to_value(key: &oast::PropertyKey<'_>) -> Option<Value> {
        match key {
            oast::PropertyKey::StaticIdentifier(i) => Some(Value::String(i.name.to_string())),
            oast::PropertyKey::Identifier(i) => Some(Value::String(i.name.to_string())),
            oast::PropertyKey::StringLiteral(s) => Some(Value::String(s.value.to_string())),
            oast::PropertyKey::NumericLiteral(n) => Some(Value::String(n.value.to_string())),
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
        let mirror_global = self.is_root;

        if matches!(kind, oxc_ast::ast::VariableDeclarationKind::Var)
            && let Some(err) = self.check_var_lexical_conflict(name)
        {
            return Err(crate::errors::CompilerError::from(err));
        }

        let is_var = matches!(kind, oxc_ast::ast::VariableDeclarationKind::Var);
        let slot = if is_var {
            self.resolve_local(name)
                .unwrap_or_else(|| self.add_var_local(name.to_string(), None, mirror_global))
        } else if let Some(slot) = self.scopes[self.scopes.len() - 1]
            .bindings
            .get(name)
            .copied()
        {
            // Check for duplicate lexical declaration (const/let with same name)
            if self.scopes[self.scopes.len() - 1]
                .lexical_bindings
                .contains(name)
            {
                return Err(crate::errors::CompilerError::from(format!(
                    "SyntaxError: redeclaration of lexical binding `{}`",
                    name
                )));
            }
            slot
        } else {
            let slot = self.add_local_with_mirror(name.to_string(), None, mirror_global);
            self.mark_local_tdz(slot);
            self.add_lexical_binding(name);
            slot
        };
        if self.is_root {
            self.emit(Op::Dup, LINE);
        }
        self.emit(Op::SetLocal, LINE);
        self.emit_byte(slot as u8, LINE);
        if self.is_root && mirror_global {
            self.emit_set_global_name(name);
        }
        let is_exported = self
            .current_module
            .as_ref()
            .is_some_and(|c| c.exports.contains(name));
        if is_exported {
            self.emit(Op::GetLocal, LINE);
            self.emit_byte(slot as u8, LINE);
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
    /// stack and assign them to their respective bindings.
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
                self.compile_binding_pattern_assignment_oast(&a.left, kind)?;
            }
            oast::BindingPattern::ArrayPattern(a) => {
                for (idx, el) in a.elements.iter().enumerate() {
                    let Some(el) = el else { continue };
                    self.emit(Op::Dup, LINE);
                    self.emit_constant(Value::String(idx.to_string()), LINE);
                    self.emit(Op::GetProperty, LINE);
                    self.compile_binding_pattern_assignment_oast(el, kind)?;
                }
                if let Some(rest) = &a.rest {
                    self.emit(Op::Dup, LINE);
                    self.compile_binding_pattern_assignment_oast(&rest.argument, kind)?;
                }
                self.emit(Op::Pop, LINE);
            }
            oast::BindingPattern::ObjectPattern(o) => {
                for prop in &o.properties {
                    let key = Self::binding_property_key_to_value(&prop.key).ok_or_else(|| {
                        crate::errors::CompilerError::from("unsupported object binding key")
                    })?;
                    self.emit(Op::Dup, LINE);
                    self.emit_constant(key, LINE);
                    self.emit(Op::GetProperty, LINE);
                    self.compile_binding_pattern_assignment_oast(&prop.value, kind)?;
                }
                if let Some(rest) = &o.rest {
                    self.emit(Op::Dup, LINE);
                    self.compile_binding_pattern_assignment_oast(&rest.argument, kind)?;
                }
                self.emit(Op::Pop, LINE);
            }
        }
        Ok(())
    }
}
