// SPDX-License-Identifier: MPL-2.0
//! Pre-declaration phase for hoisting support.
//!
//! This module handles the pre-declaration phase of compilation, which:
//! - Reserves local slots for function declarations (function hoisting)
//! - Pre-declares variable bindings with proper temporal dead zone (TDZ) tracking
//! - Emits the actual hoisted function closures into the compiler's virtual
//!   instruction stream
//! - Detects "use strict" directives for strict mode context
//!
//! The pre-declaration phase runs before the main statement compilation to ensure
//! that function declarations and variable bindings are visible throughout their
//! scope, matching JavaScript's hoisting semantics.

use super::emit::LoweringOpcode;
use crate::Compiler;
use oxc_ast::ast as oast;

const LINE: u32 = 1;

impl Compiler {
  /// Scan an oxc Program and reserve local slots for top-level function
  /// declarations to preserve hoisting semantics.
  pub(crate) fn pre_declare_functions_oast(
    &mut self,
    program: &oast::Program<'_>,
  ) {
    for stmt in &program.body {
      match stmt {
        oast::Statement::FunctionDeclaration(fn_decl) => {
          if let Some(ident) = &fn_decl.id {
            let name = ident.name.to_string();
            if self.resolve_local(&name).is_none() {
              self.add_local_in_scope(self.current_function_scope_index(), name, None, false);
            }
          }
        }
        oast::Statement::ExportNamedDeclaration(export) => {
          if let Some(oast::Declaration::FunctionDeclaration(fn_decl)) = &export.declaration
            && let Some(ident) = &fn_decl.id
          {
            let name = ident.name.to_string();
            if self.resolve_local(&name).is_none() {
              self.add_local_in_scope(self.current_function_scope_index(), name, None, false);
            }
          }
        }
        oast::Statement::ExportDefaultDeclaration(export) => {
          if let oast::ExportDefaultDeclarationKind::FunctionDeclaration(fn_decl) =
            &export.declaration
            && let Some(ident) = &fn_decl.id
          {
            let name = ident.name.to_string();
            if self.resolve_local(&name).is_none() {
              self.add_local_in_scope(self.current_function_scope_index(), name, None, false);
            }
          }
        }
        _ => {}
      }
    }
  }

  /// Pre-declare variables from a program's top-level statements.
  pub(crate) fn pre_declare_variables_oast(
    &mut self,
    program: &oast::Program<'_>,
  ) {
    self.pre_declare_variables_oast_stmts(&program.body);
  }

  /// Pre-declare variables from an arbitrary statement list, handling both
  /// var declarations (which hoist to function scope) and lexical declarations
  /// (which create block-scoped bindings with temporal dead zone).
  pub(crate) fn pre_declare_variables_oast_stmts(
    &mut self,
    stmts: &[oast::Statement],
  ) {
    use std::collections::HashSet;

    for stmt in stmts {
      let variable = match stmt {
        oast::Statement::VariableDeclaration(variable) => Some(variable.as_ref()),
        oast::Statement::ExportNamedDeclaration(export) => match &export.declaration {
          Some(oast::Declaration::VariableDeclaration(variable)) => Some(variable.as_ref()),
          _ => None,
        },
        _ => None,
      };
      if let Some(v) = variable {
        for dec in &v.declarations {
          let mut names = HashSet::new();
          Self::collect_binding_names(&dec.id, &mut names);
          let mut names: Vec<_> = names.into_iter().collect();
          names.sort();
          for name in names {
            let is_var = matches!(v.kind, oxc_ast::ast::VariableDeclarationKind::Var);
            let slot = if is_var {
              self.resolve_local(&name).unwrap_or_else(|| {
                let mirror_global = self.mirrors_root_bindings();
                self.add_var_local(name.clone(), None, mirror_global)
              })
            } else if let Some(slot) = self
              .scopes
              .last()
              .and_then(|scope| scope.bindings.get(&name).copied())
            {
              slot
            } else {
              let slot = self.add_local_with_mirror(name.clone(), None, false);
              self.mark_local_tdz(slot);
              self.add_lexical_binding(&name);
              slot
            };
            if matches!(v.kind, oxc_ast::ast::VariableDeclarationKind::Const) {
              self.mark_local_immutable(slot);
            }
          }
        }
      }

      let class = match stmt {
        oast::Statement::ClassDeclaration(class) => Some(class.as_ref()),
        oast::Statement::ExportNamedDeclaration(export) => match &export.declaration {
          Some(oast::Declaration::ClassDeclaration(class)) => Some(class.as_ref()),
          _ => None,
        },
        oast::Statement::ExportDefaultDeclaration(export) => match &export.declaration {
          oast::ExportDefaultDeclarationKind::ClassDeclaration(class) => Some(class.as_ref()),
          _ => None,
        },
        _ => None,
      };
      if let Some(class) = class
        && let Some(identifier) = &class.id
      {
        let name = identifier.name.to_string();
        if self
          .scopes
          .last()
          .is_some_and(|scope| !scope.bindings.contains_key(&name))
        {
          let slot = self.add_local_with_mirror(name.clone(), None, false);
          self.mark_local_tdz(slot);
          self.mark_local_immutable(slot);
          self.add_lexical_binding(&name);
        }
      }
    }

    let mut nested_var_names = HashSet::new();
    for stmt in stmts {
      self.add_var_names_from_statement(stmt, &mut nested_var_names);
    }
    let mut nested_var_names: Vec<_> = nested_var_names.into_iter().collect();
    nested_var_names.sort();
    for name in nested_var_names {
      if self.resolve_local(&name).is_none() {
        self.add_var_local(name, None, self.mirrors_root_bindings());
      }
    }
  }

  /// Pre-declare function declarations from a statement list, reserving local
  /// slots for all function declarations encountered.
  pub(crate) fn pre_declare_functions_oast_stmts(
    &mut self,
    stmts: &[oast::Statement],
  ) {
    for stmt in stmts {
      match stmt {
        oast::Statement::FunctionDeclaration(f) => {
          if let Some(ident) = &f.id {
            let name = ident.name.to_string();
            if self.resolve_local(&name).is_none() {
              self.add_local_with_mirror(name, None, self.mirrors_root_bindings());
            }
          }
        }
        oast::Statement::ExportNamedDeclaration(export) => {
          if let Some(oast::Declaration::FunctionDeclaration(f)) = &export.declaration
            && let Some(ident) = &f.id
          {
            let name = ident.name.to_string();
            if self.resolve_local(&name).is_none() {
              self.add_local_with_mirror(name, None, self.mirrors_root_bindings());
            }
          }
        }
        oast::Statement::ExportDefaultDeclaration(export) => {
          if let oast::ExportDefaultDeclarationKind::FunctionDeclaration(f) = &export.declaration
            && let Some(ident) = &f.id
          {
            let name = ident.name.to_string();
            if self.resolve_local(&name).is_none() {
              self.add_local_with_mirror(name, None, self.mirrors_root_bindings());
            }
          }
        }
        _ => {}
      }
    }
  }

  fn emit_function_closure(
    &mut self,
    name: &str,
    func: &oast::Function<'_>,
  ) -> Result<usize, crate::errors::CompilerError> {
    let (proto, _env_len) = self.compile_function(name, None, func, None, false)?;
    let idx = self.code.add_function(proto);
    self.emit(LoweringOpcode::MakeClosure, LINE);
    self.emit_function_index(idx, LINE);
    let slot = if let Some(s) = self.resolve_local(name) {
      s
    } else {
      self.add_local_with_mirror(name.to_string(), None, self.mirrors_root_bindings())
    };
    if self.local_mirrors_global(slot) {
      self.emit(LoweringOpcode::Copy, LINE);
    }
    self.emit(LoweringOpcode::StoreLocal, LINE);
    self.emit_slot(slot, LINE);
    if self.local_mirrors_global(slot) {
      self.emit_set_global_name(name);
    }
    Ok(slot)
  }

  /// Emit the actual function closures for hoisted function declarations.
  ///
  /// This compiles each function declaration statement into a closure object
  /// and assigns it to the previously reserved local slot, making it available
  /// throughout its scope.
  pub(crate) fn emit_hoisted_function_decls_oast_stmts(
    &mut self,
    stmts: &[oast::Statement],
  ) -> Result<(), crate::errors::CompilerError> {
    for stmt in stmts {
      match stmt {
        oast::Statement::FunctionDeclaration(f) => {
          if let Some(ident) = &f.id {
            let fname = ident.name.to_string();
            let slot = self.emit_function_closure(&fname, f)?;
            if self.current_module.is_some() {
              self.emit(LoweringOpcode::LoadLocal, LINE);
              self.emit_slot(slot, LINE);
              self.emit_set_own_module_export(&fname)?;
            }
          }
        }
        oast::Statement::ExportNamedDeclaration(export) => {
          if let Some(oast::Declaration::FunctionDeclaration(f)) = &export.declaration
            && let Some(ident) = &f.id
          {
            let fname = ident.name.to_string();
            let slot = self.emit_function_closure(&fname, f)?;
            if self.current_module.is_some() {
              self.emit(LoweringOpcode::LoadLocal, LINE);
              self.emit_slot(slot, LINE);
              self.emit_set_own_module_export(&fname)?;
            }
          }
        }
        oast::Statement::ExportDefaultDeclaration(export) => {
          if let oast::ExportDefaultDeclarationKind::FunctionDeclaration(f) = &export.declaration {
            if let Some(ident) = &f.id {
              let fname = ident.name.to_string();
              let slot = self.emit_function_closure(&fname, f)?;
              if self.current_module.is_some() {
                self.emit(LoweringOpcode::LoadLocal, LINE);
                self.emit_slot(slot, LINE);
                self.emit_set_own_module_export("default")?;
              }
            } else {
              let fname = "default".to_string();
              let (proto, _env_len) = self.compile_function(&fname, None, f, None, false)?;
              let idx = self.code.add_function(proto);
              self.emit(LoweringOpcode::MakeClosure, LINE);
              self.emit_function_index(idx, LINE);
              if self.current_module.is_some() {
                self.emit_set_own_module_export("default")?;
              }
            }
          }
        }
        _ => {}
      }
    }
    Ok(())
  }

  /// Check whether a function body contains a "use strict" directive prologue.
  ///
  /// Per ECMAScript, directive prologue consists of leading string-literal
  /// expression statements appearing before any other statements. This checks
  /// the parser-recognized directives collection.
  pub(crate) fn has_use_strict_directive(body: &oast::FunctionBody<'_>) -> bool {
    body
      .directives
      .iter()
      .any(|directive| directive.directive.as_str() == "use strict")
  }
}
