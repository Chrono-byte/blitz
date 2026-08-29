// SPDX-License-Identifier: MPL-2.0
//! Declaration phase compilation.
//!
//! This module handles the compilation of declarations including:
//! - Function declarations
//! - Class declarations
//! - Statement compilation (which includes declarations)
//! - Function and arrow function compilation with closure support
//!
//! The declaration phase compiles all declaration-type statements into bytecode,
//! managing function closures, parameter initialization, and class instantiation.

use super::emit::LoweringOpcode;
use crate::Compiler;
use crate::value::Value;
use oxc_ast::ast as oast;
use std::collections::HashSet;

const LINE: u32 = 1;

impl Compiler {
  fn emit_with_env_pops(&mut self) {
    for _ in 0..self.with_depth {
      self.emit(LoweringOpcode::PopEnv, LINE);
    }
  }

  fn emit_handler_pops_to(
    &mut self,
    depth: usize,
  ) {
    for _ in depth..self.handler_depth {
      self.emit(LoweringOpcode::PopHandler, LINE);
    }
  }

  fn nearest_finally_index(&self) -> Option<usize> {
    self
      .finally_patches
      .iter()
      .enumerate()
      .rev()
      .find_map(|(index, state)| (!state.in_finalizer).then_some(index))
  }

  fn emit_control_via_finally(
    &mut self,
    is_continue: bool,
    loop_index: usize,
    label_index: Option<usize>,
  ) -> Result<bool, crate::errors::CompilerError> {
    let Some(index) = self.nearest_finally_index() else {
      return Ok(false);
    };
    let flag_slot = self.alloc_temp_local(if is_continue {
      "finally_continue"
    } else {
      "finally_break"
    });
    self.emit(LoweringOpcode::True, LINE);
    self.emit(LoweringOpcode::StoreLocal, LINE);
    self.emit_slot(flag_slot, LINE);
    let target_depth = if label_index.is_some() {
      loop_index
    } else {
      loop_index + 1
    };
    self.emit_iterator_closes_above(target_depth)?;
    self.emit_handler_pops_to(self.finally_patches[index].handler_depth);
    self.emit_with_env_pops();
    self.emit(LoweringOpcode::Jump, LINE);
    let patch_pos = self.current_pc();
    self.emit_byte(0, LINE);
    self.emit_byte(0, LINE);
    let crossed: Vec<usize> = self.finally_patches[..=index]
      .iter()
      .enumerate()
      .rev()
      .filter(|(_, state)| !state.in_finalizer && target_depth <= state.loop_depth)
      .map(|(state_index, _)| state_index)
      .collect();
    let state = self
      .finally_patches
      .get_mut(index)
      .ok_or_else(|| crate::errors::CompilerError::from("finally state missing"))?;
    state.entry_jumps.push(patch_pos);
    for (position, state_index) in crossed.iter().enumerate() {
      self.finally_patches[*state_index]
        .control_actions
        .push(crate::compiler::core::FinallyControlAction {
          flag_slot,
          is_continue,
          loop_index,
          label_index,
          next_finally: crossed.get(position + 1).copied(),
        });
    }
    Ok(true)
  }

  fn emit_control_dispatch(
    &mut self,
    index: usize,
  ) -> Result<(), crate::errors::CompilerError> {
    let actions = self
      .finally_patches
      .get(index)
      .ok_or_else(|| crate::errors::CompilerError::from("finally state missing"))?
      .control_actions
      .clone();
    for action in actions {
      self.emit(LoweringOpcode::LoadLocal, LINE);
      self.emit_slot(action.flag_slot, LINE);
      self.emit(LoweringOpcode::JumpIfFalse, LINE);
      let skip = self.current_pc();
      self.emit_byte(0, LINE);
      self.emit_byte(0, LINE);
      self.emit(LoweringOpcode::Jump, LINE);
      let patch_pos = self.current_pc();
      self.emit_byte(0, LINE);
      self.emit_byte(0, LINE);
      if let Some(next_finally) = action.next_finally {
        self.emit_handler_pops_to(self.finally_patches[next_finally].handler_depth);
        self.finally_patches[next_finally]
          .entry_jumps
          .push(patch_pos);
      } else if action.is_continue {
        if let Some(label_index) = action.label_index {
          self.label_patches[label_index]
            .continue_patches
            .push(patch_pos);
        } else {
          self.continue_patches[action.loop_index].push(patch_pos);
        }
      } else if let Some(label_index) = action.label_index {
        self.label_patches[label_index]
          .break_patches
          .push(patch_pos);
      } else {
        self.break_patches[action.loop_index].push(patch_pos);
      }
      self.patch_jump(skip);
    }
    Ok(())
  }

  fn emit_iterator_closes_above(
    &mut self,
    target_loop_depth: usize,
  ) -> Result<(), crate::errors::CompilerError> {
    let slots = self
      .iterator_close_stack
      .iter()
      .rev()
      .filter(|state| state.loop_depth > target_loop_depth)
      .map(|state| state.iterator_slot)
      .collect::<Vec<_>>();
    for slot in slots {
      self.emit(LoweringOpcode::LoadLocal, LINE);
      self.emit_slot(slot, LINE);
      self.emit(LoweringOpcode::IteratorClose, LINE);
      self.emit(LoweringOpcode::Discard, LINE);
    }
    Ok(())
  }

  /// Compile a function declaration statement into bytecode.
  ///
  /// Creates a closure from the function and assigns it to a local slot,
  /// optionally mirroring to a global if at module root level.
  fn compile_function_declaration_oast(
    &mut self,
    f: &oast::Function<'_>,
  ) -> Result<(), crate::errors::CompilerError> {
    if let Some(ident) = &f.id {
      let fname = ident.name.to_string();

      if let Some(err) = self.check_var_lexical_conflict(&fname) {
        return Err(crate::errors::CompilerError::from(err));
      }
      if let Some(err) = Self::strict_binding_name_error(self.strict_context, &fname) {
        return Err(err);
      }

      let (proto, _env_len) = self.compile_function(&fname, None, f, None, false)?;
      let idx = self.chunk.add_function(proto);
      self.emit(LoweringOpcode::MakeClosure, LINE);
      self.emit_function_index(idx, LINE);
      let slot = if let Some(s) = self.resolve_local(&fname) {
        s
      } else {
        self.add_local_with_mirror(fname.clone(), None, self.mirrors_root_bindings())
      };

      if slot == self.locals.len() - 1 {
        self.add_lexical_binding(&fname);
      }

      if self.local_mirrors_global(slot) {
        self.emit(LoweringOpcode::Copy, LINE);
      }
      self.emit(LoweringOpcode::StoreLocal, LINE);
      self.emit_slot(slot, LINE);
      if self.local_mirrors_global(slot) {
        self.emit_set_global_name(&fname);
      }
      let is_exported = self
        .current_module
        .as_ref()
        .is_some_and(|c| c.exports.contains(&fname));
      if is_exported {
        self.emit(LoweringOpcode::LoadLocal, LINE);
        self.emit_slot(slot, LINE);
        self.emit_set_own_module_export(&fname)?;
      }
    }

    Ok(())
  }

  /// Compile an oxc AST statement into bytecode.
  ///
  /// Wrapper that delegates to `compile_statement_oast_with_mode` with `skip_pop` = false,
  /// meaning expression values will be discarded from the compiler's virtual
  /// value stack.
  pub(super) fn compile_statement_oast(
    &mut self,
    stmt: &oast::Statement,
  ) -> Result<(), crate::errors::CompilerError> {
    self.compile_statement_oast_with_mode(stmt, false, None)
  }

  /// Compile an oxc AST statement into bytecode with optional result preservation.
  ///
  /// # Arguments
  ///
  /// * `stmt` - The statement to compile
  /// * `skip_pop` - If true, the result of expression statements is preserved
  ///   on the compiler's virtual value stack
  ///
  /// This handles all statement types including:
  /// - Import/export declarations
  /// - Variable, function, and class declarations
  /// - Expression statements
  /// - Control flow (if/else, switch, loops)
  /// - Try/catch/finally blocks
  /// - Labeled statements
  pub(super) fn compile_statement_oast_with_mode(
    &mut self,
    stmt: &oast::Statement,
    skip_pop: bool,
    completion_slot: Option<usize>,
  ) -> Result<(), crate::errors::CompilerError> {
    use oxc_ast::ast::{BindingPattern as OPat, ForStatementLeft as OForLeft, Statement as OStmt};
    match stmt {
      OStmt::ImportDeclaration(_) => {}
      OStmt::ExportAllDeclaration(export) => {
        if let Some(exported) = &export.exported {
          let Some(export_name) = Self::module_export_name_to_string(exported) else {
            return Ok(());
          };
          let Some(module_index) = self.current_module.as_ref().and_then(|context| {
            context
              .namespace_exports
              .get(&export_name)
              .copied()
          }) else {
            return Err(crate::errors::CompilerError::from(format!(
              "missing namespace re-export target for {export_name:?}"
            )));
          };
          let descriptor = self.module_export_descriptor_index(module_index, "*")?;
          self.emit(LoweringOpcode::GetModuleExport, LINE);
          self.emit_byte(descriptor.to_le_bytes()[0], LINE);
          self.emit_byte(descriptor.to_le_bytes()[1], LINE);
          self.emit_set_own_module_export(&export_name)?;
        }
      }
      OStmt::ExportNamedDeclaration(export) => {
        if let Some(decl) = &export.declaration {
          match decl {
            oast::Declaration::VariableDeclaration(v) => {
              self.compile_variable_declaration_oast(v)?
            }
            oast::Declaration::FunctionDeclaration(f) => {
              self.compile_function_declaration_oast(f)?
            }
            oast::Declaration::ClassDeclaration(c) => {
              self.compile_class_declaration_oast(c)?;
            }
            _ => {}
          }
        }
        if export.declaration.is_none() && export.source.is_none() {
          for spec in &export.specifiers {
            let Some(local_name) = Self::module_export_name_to_string(&spec.local) else {
              continue;
            };
            let Some(export_name) = Self::module_export_name_to_string(&spec.exported) else {
              continue;
            };
            self.emit_get_binding(&local_name);
            self.emit_set_own_module_export(&export_name)?;
          }
        }
      }
      OStmt::ExportDefaultDeclaration(export) => {
        match &export.declaration {
          oast::ExportDefaultDeclarationKind::FunctionDeclaration(f) => {
            if let Some(id) = &f.id {
              self.compile_function_declaration_oast(f)?;
              self.emit_get_binding(id.name.as_str());
            } else {
              let fname = "default".to_string();
              let (proto, _env_len) = self.compile_function(&fname, None, f, None, false)?;
              let idx = self.chunk.add_function(proto);
              self.emit(LoweringOpcode::MakeClosure, LINE);
              self.emit_function_index(idx, LINE);
            }
          }
          oast::ExportDefaultDeclarationKind::ClassDeclaration(c) => {
            if let Some(id) = &c.id {
              self.compile_class_declaration_oast(c)?;
              self.emit_get_binding(id.name.as_str());
            } else {
              self.emit_class_from_oast(c, Some("default"))?;
            }
          }
          oast::ExportDefaultDeclarationKind::TSInterfaceDeclaration(_) => {
            return Err(crate::errors::CompilerError::from(
              "unsupported export default declaration",
            ));
          }
          other => {
            if let Some(expr) = other.as_expression() {
              if Self::is_anonymous_function_definition(expr) {
                self.emit_get_builtin_name("setFunctionNameIfAnonymous");
                self.compile_expr_oast(expr)?;
                self.emit_constant(Value::String("default".into()), LINE);
                self.emit(LoweringOpcode::Call, LINE);
                self.emit_byte(2, LINE);
              } else {
                self.compile_expr_oast(expr)?;
              }
            } else {
              return Err(crate::errors::CompilerError::from(
                "unsupported export default declaration",
              ));
            }
          }
        }
        self.emit_set_own_module_export("default")?;
      }
      OStmt::VariableDeclaration(v) => self.compile_variable_declaration_oast(v)?,
      OStmt::FunctionDeclaration(f) => self.compile_function_declaration_oast(f)?,
      OStmt::ClassDeclaration(c) => self.compile_class_declaration_oast(c)?,
      OStmt::ExpressionStatement(e) => {
        self.compile_expr_oast(&e.expression)?;
        if let Some(slot) = completion_slot {
          self.emit(LoweringOpcode::StoreLocal, LINE);
          self.emit_slot(slot, LINE);
        } else if !skip_pop {
          self.emit(LoweringOpcode::Discard, LINE);
        }
      }
      OStmt::LabeledStatement(l) => {
        let can_continue = matches!(
          &l.body,
          OStmt::WhileStatement(_)
            | OStmt::DoWhileStatement(_)
            | OStmt::ForStatement(_)
            | OStmt::ForInStatement(_)
            | OStmt::ForOfStatement(_)
        );
        self
          .label_patches
          .push(crate::compiler::LabelPatchState {
            name: l.label.name.to_string(),
            can_continue,
            owner_loop_depth: if can_continue {
              self.loop_depth + 1
            } else {
              self.loop_depth
            },
            continue_patches: Vec::new(),
            break_patches: Vec::new(),
          });
        self.compile_statement_oast_with_mode(&l.body, false, completion_slot)?;
        let label_state = self
          .label_patches
          .pop()
          .ok_or_else(|| crate::errors::CompilerError::from("label stack underflow"))?;
        if !label_state.can_continue && !label_state.continue_patches.is_empty() {
          return Err(crate::errors::CompilerError::from(
            "'continue' used with non-iteration label",
          ));
        }
        if !label_state.break_patches.is_empty() {
          let target = self.current_pc();
          self.patch_jump_list(label_state.break_patches, target);
        }
      }
      OStmt::WithStatement(w) => {
        if self.strict_context {
          return Err(crate::errors::CompilerError::from(
            "SyntaxError: Strict mode code may not include a with statement",
          ));
        }
        self.compile_expr_oast(&w.object)?;
        self.emit(LoweringOpcode::PushWithEnv, LINE);
        self.with_depth += 1;
        self.emit(LoweringOpcode::PushHandler, LINE);
        let handler_jump = self.current_pc();
        self.emit_byte(0, LINE);
        self.emit_byte(0, LINE);

        let with_body_stmts = match &w.body {
          OStmt::BlockStatement(b) => &b.body[..],
          _ => std::slice::from_ref(&w.body),
        };
        self.enter_block_scope_with_vars(with_body_stmts);
        self.compile_statement_oast_with_mode(&w.body, false, completion_slot)?;
        self.exit_scope();

        self.emit(LoweringOpcode::PopHandler, LINE);
        self.emit(LoweringOpcode::PopEnv, LINE);
        self.with_depth = self.with_depth.saturating_sub(1);
        self.emit(LoweringOpcode::Jump, LINE);
        let skip_handler_jump = self.current_pc();
        self.emit_byte(0, LINE);
        self.emit_byte(0, LINE);

        let _handler_start = self.current_pc();
        self.patch_jump(handler_jump);

        self.emit(LoweringOpcode::PopEnv, LINE);
        self.emit(LoweringOpcode::Throw, LINE);
        let _after_handler = self.current_pc();

        self.patch_jump(skip_handler_jump);
      }
      OStmt::BlockStatement(b) => {
        self.enter_block_scope_with_vars(&b.body);
        for s in &b.body {
          self.compile_statement_oast_with_mode(s, false, completion_slot)?;
        }
        self.exit_scope();
      }
      OStmt::IfStatement(i) => {
        if let Some(jump_over) = self.try_emit_compare_jump_if_false(&i.test)? {
          self.compile_statement_oast_with_mode(&i.consequent, false, completion_slot)?;
          if let Some(alt) = &i.alternate {
            self.emit(LoweringOpcode::Jump, LINE);
            let jump_else = self.current_pc();
            self.emit_byte(0, LINE);
            self.emit_byte(0, LINE);
            self.patch_jump(jump_over);
            self.compile_statement_oast_with_mode(alt, false, completion_slot)?;
            self.patch_jump(jump_else);
          } else {
            self.patch_jump(jump_over);
          }
        } else {
          self.compile_expr_oast(&i.test)?;
          self.emit(LoweringOpcode::JumpIfFalse, LINE);
          let jump_over = self.current_pc();
          self.emit_byte(0, LINE);
          self.emit_byte(0, LINE);
          self.compile_statement_oast_with_mode(&i.consequent, false, completion_slot)?;
          if let Some(alt) = &i.alternate {
            self.emit(LoweringOpcode::Jump, LINE);
            let jump_else = self.current_pc();
            self.emit_byte(0, LINE);
            self.emit_byte(0, LINE);
            self.patch_jump(jump_over);
            self.compile_statement_oast_with_mode(alt, false, completion_slot)?;
            self.patch_jump(jump_else);
          } else {
            self.patch_jump(jump_over);
          }
        }
      }
      OStmt::SwitchStatement(s) => {
        self.compile_expr_oast(&s.discriminant)?;
        let discr_name = format!("\0switch_discriminant_{}", self.locals.len());
        let discr_slot =
          self.add_local_in_scope(self.current_function_scope_index(), discr_name, None, false);
        self.emit(LoweringOpcode::StoreLocal, LINE);
        self.emit_slot(discr_slot, LINE);

        self.enter_block_scope();
        for case in &s.cases {
          self.pre_declare_variables_oast_stmts(&case.consequent);
        }

        self.break_patches.push(Vec::new());

        let mut case_body_jumps: Vec<(usize, usize)> = Vec::new();
        let mut default_index: Option<usize> = None;
        for (case_idx, case) in s.cases.iter().enumerate() {
          if let Some(test) = &case.test {
            self.emit(LoweringOpcode::LoadLocal, LINE);
            self.emit_slot(discr_slot, LINE);
            self.compile_expr_oast(test)?;
            self.emit_with_payload(LoweringOpcode::CompareJump, 6, LINE);
            let skip_jump = self.current_pc();
            self.emit_byte(0, LINE);
            self.emit_byte(0, LINE);

            self.emit(LoweringOpcode::Jump, LINE);
            let body_jump = self.current_pc();
            self.emit_byte(0, LINE);
            self.emit_byte(0, LINE);
            case_body_jumps.push((case_idx, body_jump));

            self.patch_jump(skip_jump);
          } else {
            default_index = Some(case_idx);
          }
        }

        self.emit(LoweringOpcode::Jump, LINE);
        let no_match_jump = self.current_pc();
        self.emit_byte(0, LINE);
        self.emit_byte(0, LINE);

        let mut case_starts: Vec<usize> = vec![0; s.cases.len()];
        for (case_idx, case) in s.cases.iter().enumerate() {
          case_starts[case_idx] = self.current_pc();
          for stmt in &case.consequent {
            self.compile_statement_oast_with_mode(stmt, false, completion_slot)?;
          }
        }
        let switch_end = self.current_pc();

        for (case_idx, jump_pos) in case_body_jumps {
          self.patch_jump_to_target(jump_pos, case_starts[case_idx]);
        }

        let default_target = default_index.map_or(switch_end, |idx| case_starts[idx]);
        self.patch_jump_to_target(no_match_jump, default_target);

        if let Some(patches) = self.break_patches.pop() {
          for patch_pos in patches {
            self.patch_jump_to_target(patch_pos, switch_end);
          }
        }
        self.exit_scope();
      }
      OStmt::WhileStatement(w) => {
        self.loop_depth += 1;
        self.continue_patches.push(Vec::new());
        self.break_patches.push(Vec::new());
        let owns_completion_slot = completion_slot.is_none();
        let loop_completion_slot = if let Some(slot) = completion_slot {
          slot
        } else {
          let slot = self.alloc_temp_local("loop_result");
          self.emit(LoweringOpcode::Undefined, LINE);
          self.emit(LoweringOpcode::StoreLocal, LINE);
          self.emit_slot(slot, LINE);
          slot
        };
        let loop_start = self.current_pc();
        let exit_jump = if let Some(jump_over) = self.try_emit_compare_jump_if_false(&w.test)? {
          Some(jump_over)
        } else {
          self.compile_expr_oast(&w.test)?;
          self.emit(LoweringOpcode::JumpIfFalse, LINE);
          let exit_jump = self.current_pc();
          self.emit_byte(0, LINE);
          self.emit_byte(0, LINE);
          Some(exit_jump)
        };
        self.enter_block_scope();
        self.compile_statement_oast_with_mode(&w.body, false, Some(loop_completion_slot))?;
        self.exit_scope();
        self.emit(LoweringOpcode::Jump, LINE);
        let _back_jump_pos = self.current_pc();
        let back_offset = loop_start as isize - (self.current_pc() as isize);
        self.emit_short_signed(back_offset, LINE);
        if let Some(patches) = self.continue_patches.pop() {
          for patch_pos in patches {
            self.patch_jump_to_target(patch_pos, loop_start);
          }
        }
        let cleanup_start = self.current_pc();
        if owns_completion_slot {
          if let Some(exit_jump) = exit_jump {
            self.patch_jump_to_target(exit_jump, cleanup_start);
          }
          self.emit(LoweringOpcode::LoadLocal, LINE);
          self.emit_slot(loop_completion_slot, LINE);
        } else if let Some(exit_jump) = exit_jump {
          self.patch_jump(exit_jump);
        }
        if let Some(patches) = self.break_patches.pop() {
          for patch_pos in patches {
            self.patch_jump_to_target(patch_pos, cleanup_start);
          }
        }
        let label_patches = if let Some(state) = self.label_patches.last_mut() {
          if state.can_continue && state.owner_loop_depth == self.loop_depth {
            Some((
              std::mem::take(&mut state.continue_patches),
              std::mem::take(&mut state.break_patches),
            ))
          } else {
            None
          }
        } else {
          None
        };
        if let Some((continue_patches, break_patches)) = label_patches {
          self.patch_jump_list(continue_patches, loop_start);
          self.patch_jump_list(break_patches, cleanup_start);
        }
        self.loop_depth = self.loop_depth.saturating_sub(1);
      }
      OStmt::DoWhileStatement(d) => {
        self.loop_depth += 1;
        self.continue_patches.push(Vec::new());
        self.break_patches.push(Vec::new());
        let owns_completion_slot = completion_slot.is_none();
        let loop_completion_slot = if let Some(slot) = completion_slot {
          slot
        } else {
          let slot = self.alloc_temp_local("loop_result");
          self.emit(LoweringOpcode::Undefined, LINE);
          self.emit(LoweringOpcode::StoreLocal, LINE);
          self.emit_slot(slot, LINE);
          slot
        };
        let loop_start = self.current_pc();
        self.enter_block_scope();
        self.compile_statement_oast_with_mode(&d.body, false, Some(loop_completion_slot))?;
        self.exit_scope();
        let test_start = self.current_pc();
        let exit_jump = if let Some(jump_over) = self.try_emit_compare_jump_if_false(&d.test)? {
          Some(jump_over)
        } else {
          self.compile_expr_oast(&d.test)?;
          self.emit(LoweringOpcode::JumpIfFalse, LINE);
          let exit_jump = self.current_pc();
          self.emit_byte(0, LINE);
          self.emit_byte(0, LINE);
          Some(exit_jump)
        };
        self.emit(LoweringOpcode::Jump, LINE);
        let _back_jump_pos = self.current_pc();
        let back_offset = loop_start as isize - (self.current_pc() as isize);
        self.emit_short_signed(back_offset, LINE);
        if let Some(patches) = self.continue_patches.pop() {
          for patch_pos in patches {
            self.patch_jump_to_target(patch_pos, test_start);
          }
        }
        let cleanup_start = self.current_pc();
        if owns_completion_slot {
          self.emit(LoweringOpcode::LoadLocal, LINE);
          self.emit_slot(loop_completion_slot, LINE);
          if let Some(exit_jump) = exit_jump {
            self.patch_jump_to_target(exit_jump, cleanup_start);
          }
        } else if let Some(exit_jump) = exit_jump {
          self.patch_jump(exit_jump);
        }
        if let Some(patches) = self.break_patches.pop() {
          for patch_pos in patches {
            self.patch_jump_to_target(patch_pos, cleanup_start);
          }
        }
        let label_patches = if let Some(state) = self.label_patches.last_mut() {
          if state.can_continue && state.owner_loop_depth == self.loop_depth {
            Some((
              std::mem::take(&mut state.continue_patches),
              std::mem::take(&mut state.break_patches),
            ))
          } else {
            None
          }
        } else {
          None
        };
        if let Some((continue_patches, break_patches)) = label_patches {
          self.patch_jump_list(continue_patches, test_start);
          self.patch_jump_list(break_patches, cleanup_start);
        }
        self.loop_depth = self.loop_depth.saturating_sub(1);
      }
      OStmt::ForStatement(f) => {
        use oxc_ast::ast::ForStatementInit as OForInit;
        self.enter_block_scope();
        self.loop_depth += 1;
        self.continue_patches.push(Vec::new());
        self.break_patches.push(Vec::new());
        let mut per_iteration_slots = Vec::new();

        if let Some(OForInit::VariableDeclaration(v)) = &f.init
          && matches!(
            v.kind,
            oxc_ast::ast::VariableDeclarationKind::Let
              | oxc_ast::ast::VariableDeclarationKind::Const
          )
        {
          let mut names = HashSet::new();
          for declaration in &v.declarations {
            Self::collect_binding_names(&declaration.id, &mut names);
          }
          let mut names: Vec<_> = names.into_iter().collect();
          names.sort();
          for name in names {
            let slot = self.add_local_with_mirror(name.clone(), None, false);
            self.mark_local_tdz(slot);
            self.add_lexical_binding(&name);
            if matches!(v.kind, oxc_ast::ast::VariableDeclarationKind::Const) {
              self.mark_local_immutable(slot);
            }
          }
          self.activate_current_scope_lexical_bindings();
        }

        if let Some(init) = &f.init {
          match init {
            OForInit::VariableDeclaration(v) => {
              if v
                .declarations
                .iter()
                .all(|declaration| matches!(declaration.id, OPat::BindingIdentifier(_)))
              {
                for declaration in &v.declarations {
                  let OPat::BindingIdentifier(identifier) = &declaration.id else {
                    unreachable!("simple for declarations were checked above");
                  };
                  let name = identifier.name.to_string();
                  let mirror_global = self.mirrors_root_bindings()
                    && matches!(v.kind, oxc_ast::ast::VariableDeclarationKind::Var);
                  let slot = if matches!(v.kind, oxc_ast::ast::VariableDeclarationKind::Var) {
                    self
                      .resolve_local(&name)
                      .unwrap_or_else(|| self.add_var_local(name.clone(), None, mirror_global))
                  } else {
                    self
                      .resolve_local(&name)
                      .expect("for-loop lexical binding was predeclared")
                  };
                  if matches!(v.kind, oxc_ast::ast::VariableDeclarationKind::Const) {
                    self.mark_local_immutable(slot);
                  }
                  if let Some(initializer) = &declaration.init {
                    self.compile_expr_oast(initializer)?;
                  } else {
                    self.emit(LoweringOpcode::Undefined, LINE);
                  }
                  if matches!(
                    v.kind,
                    oxc_ast::ast::VariableDeclarationKind::Let
                      | oxc_ast::ast::VariableDeclarationKind::Const
                  ) {
                    per_iteration_slots.push(slot);
                  }
                  if mirror_global {
                    self.emit(LoweringOpcode::Copy, LINE);
                  }
                  self.emit(LoweringOpcode::StoreLocal, LINE);
                  self.emit_slot(slot, LINE);
                  if mirror_global {
                    self.emit_set_global_name(&name);
                  }
                }
              } else {
                self.compile_variable_declaration_oast(v)?;
                if matches!(
                  v.kind,
                  oxc_ast::ast::VariableDeclarationKind::Let
                    | oxc_ast::ast::VariableDeclarationKind::Const
                ) {
                  let mut names = HashSet::new();
                  for declaration in &v.declarations {
                    self.add_var_names_from_binding(&declaration.id, &mut names);
                  }
                  let mut names: Vec<_> = names.into_iter().collect();
                  names.sort();
                  for name in names {
                    if let Some(slot) = self.resolve_local(&name) {
                      per_iteration_slots.push(slot);
                    }
                  }
                }
              }
            }
            other => {
              let expr = other.as_expression().ok_or_else(|| {
                crate::errors::CompilerError::from("unsupported for-loop initializer")
              })?;
              self.compile_expr_oast(expr)?;
              self.emit(LoweringOpcode::Discard, LINE);
            }
          }
        }

        for &slot in &per_iteration_slots {
          self.emit(LoweringOpcode::CloneLocalCell, LINE);
          self.emit_slot(slot, LINE);
        }

        let loop_start = self.current_pc();
        let exit_jump = if let Some(test) = &f.test {
          if let Some(jump_over) = self.try_emit_compare_jump_if_false(test)? {
            Some(jump_over)
          } else {
            self.compile_expr_oast(test)?;
            self.emit(LoweringOpcode::JumpIfFalse, LINE);
            let exit_jump = self.current_pc();
            self.emit_byte(0, LINE);
            self.emit_byte(0, LINE);
            Some(exit_jump)
          }
        } else {
          None
        };

        self.enter_block_scope();
        self.compile_statement_oast_with_mode(&f.body, false, completion_slot)?;
        self.exit_scope();

        let update_start = self.current_pc();
        if let Some(patches) = self.continue_patches.pop() {
          for patch_pos in patches {
            self.patch_jump_to_target(patch_pos, update_start);
          }
        }

        for &slot in &per_iteration_slots {
          self.emit(LoweringOpcode::CloneLocalCell, LINE);
          self.emit_slot(slot, LINE);
        }

        if let Some(update) = &f.update {
          self.compile_expr_oast(update)?;
          self.emit(LoweringOpcode::Discard, LINE);
        }

        self.emit(LoweringOpcode::Jump, LINE);
        let back_offset = loop_start as isize - (self.current_pc() as isize);
        self.emit_short_signed(back_offset, LINE);

        if let Some(exit_jump) = exit_jump {
          self.patch_jump(exit_jump);
        }
        let loop_end = self.current_pc();
        if let Some(patches) = self.break_patches.pop() {
          for patch_pos in patches {
            self.patch_jump_to_target(patch_pos, loop_end);
          }
        }
        let label_patches = if let Some(state) = self.label_patches.last_mut() {
          if state.can_continue && state.owner_loop_depth == self.loop_depth {
            Some((
              std::mem::take(&mut state.continue_patches),
              std::mem::take(&mut state.break_patches),
            ))
          } else {
            None
          }
        } else {
          None
        };
        if let Some((continue_patches, break_patches)) = label_patches {
          let loop_end = self.current_pc();
          self.patch_jump_list(continue_patches, update_start);
          self.patch_jump_list(break_patches, loop_end);
        }
        self.loop_depth = self.loop_depth.saturating_sub(1);
        self.exit_scope();
      }
      OStmt::ForOfStatement(f) => {
        self.enter_block_scope();
        self.loop_depth += 1;
        self.continue_patches.push(Vec::new());
        self.break_patches.push(Vec::new());
        let owns_completion_slot = completion_slot.is_none();
        let loop_completion_slot = if let Some(slot) = completion_slot {
          slot
        } else {
          let slot = self.alloc_temp_local("loop_result");
          self.emit(LoweringOpcode::Undefined, LINE);
          self.emit(LoweringOpcode::StoreLocal, LINE);
          self.emit_slot(slot, LINE);
          slot
        };

        if let oast::ForStatementLeft::VariableDeclaration(v) = &f.left
          && matches!(
            v.kind,
            oxc_ast::ast::VariableDeclarationKind::Let
              | oxc_ast::ast::VariableDeclarationKind::Const
          )
        {
          let Some(dec) = v.declarations.first() else {
            return Err(crate::errors::CompilerError::from(
              "unsupported empty for-of declaration",
            ));
          };
          let mut names = HashSet::new();
          self.add_var_names_from_binding(&dec.id, &mut names);
          let mut names: Vec<_> = names.into_iter().collect();
          names.sort();
          for name in names {
            let slot = self.add_local_with_mirror(name.clone(), None, false);
            self.mark_local_tdz(slot);
            self.add_lexical_binding(&name);
            if matches!(v.kind, oxc_ast::ast::VariableDeclarationKind::Const) {
              self.mark_local_immutable(slot);
            }
            self.emit(LoweringOpcode::ResetLocalCell, LINE);
            self.emit_slot(slot, LINE);
          }
        }
        self.activate_current_scope_lexical_bindings();

        self.compile_expr_oast(&f.right)?;
        if f.r#await {
          self.emit(LoweringOpcode::GetAsyncIterator, LINE);
        } else {
          self.emit_get_builtin_name("getIterator");
          self.emit(LoweringOpcode::Exchange, LINE);
          self.emit(LoweringOpcode::Call, LINE);
          self.emit_byte(1, LINE);
        }

        let iterator_slot = self.add_local_in_scope(
          self.current_function_scope_index(),
          format!("\0forof_iter_{}", self.locals.len()),
          None,
          false,
        );
        self.emit(LoweringOpcode::StoreLocal, LINE);
        self.emit_slot(iterator_slot, LINE);

        let thrown_slot = self.alloc_temp_local("forof_thrown");
        let iterator_close_active_slot = self.alloc_temp_local("forof_close_active");
        self.emit(LoweringOpcode::False, LINE);
        self.emit(LoweringOpcode::StoreLocal, LINE);
        self.emit_slot(iterator_close_active_slot, LINE);
        self.emit(LoweringOpcode::PushHandler, LINE);
        let handler_jump = self.current_pc();
        self.emit_byte(0, LINE);
        self.emit_byte(0, LINE);

        let loop_start = self.current_pc();

        self.emit(LoweringOpcode::False, LINE);
        self.emit(LoweringOpcode::StoreLocal, LINE);
        self.emit_slot(iterator_close_active_slot, LINE);
        self.emit(LoweringOpcode::LoadLocal, LINE);
        self.emit_slot(iterator_slot, LINE);
        self.emit(LoweringOpcode::IteratorNext, LINE);
        self.emit(LoweringOpcode::Copy, LINE);
        self.emit_constant(Value::String("done".to_string().into()), LINE);
        self.emit(LoweringOpcode::GetProperty, LINE);
        self.emit(LoweringOpcode::JumpIfTrue, LINE);
        let exit_jump = self.current_pc() - 1;
        self.emit_byte(0, LINE);
        self.emit_byte(0, LINE);
        self.emit_constant(Value::String("value".to_string().into()), LINE);
        self.emit(LoweringOpcode::GetProperty, LINE);
        self.emit(LoweringOpcode::True, LINE);
        self.emit(LoweringOpcode::StoreLocal, LINE);
        self.emit_slot(iterator_close_active_slot, LINE);

        match &f.left {
          oast::ForStatementLeft::VariableDeclaration(v) => {
            let Some(dec) = v.declarations.first() else {
              return Err(crate::errors::CompilerError::from(
                "unsupported empty for-of declaration",
              ));
            };
            if matches!(
              v.kind,
              oxc_ast::ast::VariableDeclarationKind::Let
                | oxc_ast::ast::VariableDeclarationKind::Const
            ) {
              let mut names = HashSet::new();
              self.add_var_names_from_binding(&dec.id, &mut names);
              let mut names: Vec<_> = names.into_iter().collect();
              names.sort();
              for name in &names {
                if let Some(slot) = self.resolve_local(name) {
                  self.emit(LoweringOpcode::ResetLocalCell, LINE);
                  self.emit_slot(slot, LINE);
                }
              }
            }
            self.compile_binding_pattern_assignment_oast(&dec.id, v.kind)?;
          }
          oast::ForStatementLeft::AssignmentTargetIdentifier(id) => {
            self.compile_assignment_identifier_reference(id)?;
          }
          oast::ForStatementLeft::StaticMemberExpression(m) => {
            self.compile_static_member_assignment_target(m)?;
          }
          oast::ForStatementLeft::ComputedMemberExpression(m) => {
            self.compile_computed_member_assignment_target(m)?;
          }
          oast::ForStatementLeft::ArrayAssignmentTarget(a) => {
            self.compile_array_assignment_target(a)?;
          }
          oast::ForStatementLeft::ObjectAssignmentTarget(o) => {
            self.compile_object_assignment_target(o)?;
          }
          _ => {
            return Err(crate::errors::CompilerError::from(
              "unsupported for-of assignment target",
            ));
          }
        }

        self.enter_block_scope();
        self
          .iterator_close_stack
          .push(crate::compiler::IteratorCloseState {
            iterator_slot,
            loop_depth: self.loop_depth,
          });
        self.compile_statement_oast_with_mode(&f.body, false, Some(loop_completion_slot))?;
        self.iterator_close_stack.pop();
        self.exit_scope();

        let update_start = loop_start;
        if let Some(patches) = self.continue_patches.pop() {
          for patch_pos in patches {
            self.patch_jump_to_target(patch_pos, update_start);
          }
        }

        self.emit(LoweringOpcode::Jump, LINE);
        let back_offset = loop_start as isize - (self.current_pc() as isize);
        self.emit_short_signed(back_offset, LINE);

        self.patch_jump(exit_jump);
        self.emit(LoweringOpcode::Discard, LINE);
        self.emit(LoweringOpcode::PopHandler, LINE);
        self.emit(LoweringOpcode::Jump, LINE);
        let done_end_jump = self.current_pc();
        self.emit_byte(0, LINE);
        self.emit_byte(0, LINE);

        let break_cleanup = self.current_pc();
        if let Some(patches) = self.break_patches.pop() {
          for patch_pos in patches {
            self.patch_jump_to_target(patch_pos, break_cleanup);
          }
        }
        let label_patches = if let Some(state) = self.label_patches.last_mut() {
          if state.can_continue && state.owner_loop_depth == self.loop_depth {
            Some((
              std::mem::take(&mut state.continue_patches),
              std::mem::take(&mut state.break_patches),
            ))
          } else {
            None
          }
        } else {
          None
        };
        if let Some((continue_patches, break_patches)) = label_patches {
          self.patch_jump_list(continue_patches, update_start);
          self.patch_jump_list(break_patches, break_cleanup);
        }
        self.emit(LoweringOpcode::LoadLocal, LINE);
        self.emit_slot(iterator_slot, LINE);
        self.emit(LoweringOpcode::IteratorClose, LINE);
        self.emit(LoweringOpcode::Discard, LINE);
        self.emit(LoweringOpcode::Jump, LINE);
        let end_jump = self.current_pc();
        self.emit_byte(0, LINE);
        self.emit_byte(0, LINE);

        self.patch_jump(handler_jump);
        self.emit(LoweringOpcode::StoreLocal, LINE);
        self.emit_slot(thrown_slot, LINE);
        self.emit(LoweringOpcode::PushHandler, LINE);
        let close_handler_jump = self.current_pc();
        self.emit_byte(0, LINE);
        self.emit_byte(0, LINE);
        self.emit(LoweringOpcode::LoadLocal, LINE);
        self.emit_slot(iterator_close_active_slot, LINE);
        self.emit(LoweringOpcode::JumpIfFalse, LINE);
        let skip_abrupt_close = self.current_pc();
        self.emit_byte(0, LINE);
        self.emit_byte(0, LINE);
        self.emit(LoweringOpcode::LoadLocal, LINE);
        self.emit_slot(iterator_slot, LINE);
        self.emit(LoweringOpcode::IteratorClose, LINE);
        self.emit(LoweringOpcode::Discard, LINE);
        self.patch_jump(skip_abrupt_close);
        self.emit(LoweringOpcode::PopHandler, LINE);
        self.emit(LoweringOpcode::Jump, LINE);
        let close_done_jump = self.current_pc();
        self.emit_byte(0, LINE);
        self.emit_byte(0, LINE);
        self.patch_jump(close_handler_jump);
        self.emit(LoweringOpcode::Discard, LINE);
        self.patch_jump(close_done_jump);
        self.emit(LoweringOpcode::LoadLocal, LINE);
        self.emit_slot(thrown_slot, LINE);
        self.emit(LoweringOpcode::Throw, LINE);
        self.patch_jump(done_end_jump);
        self.patch_jump(end_jump);
        if owns_completion_slot {
          self.emit(LoweringOpcode::LoadLocal, LINE);
          self.emit_slot(loop_completion_slot, LINE);
        }
        self.loop_depth = self.loop_depth.saturating_sub(1);
        self.exit_scope();
      }
      OStmt::ForInStatement(f) => {
        self.enter_block_scope();
        self.loop_depth += 1;
        self.continue_patches.push(Vec::new());
        self.break_patches.push(Vec::new());

        if let OForLeft::VariableDeclaration(v) = &f.left
          && matches!(
            v.kind,
            oxc_ast::ast::VariableDeclarationKind::Let
              | oxc_ast::ast::VariableDeclarationKind::Const
          )
        {
          if v.declarations.len() != 1 {
            return Err(crate::errors::CompilerError::from(
              "SyntaxError: for-in lexical declaration must have exactly one binding",
            ));
          }
          if v
            .declarations
            .first()
            .and_then(|dec| dec.init.as_ref())
            .is_some()
          {
            return Err(crate::errors::CompilerError::from(
              "SyntaxError: for-in lexical declaration may not have an initializer",
            ));
          }
        }

        if let OForLeft::VariableDeclaration(v) = &f.left
          && matches!(
            v.kind,
            oxc_ast::ast::VariableDeclarationKind::Let
              | oxc_ast::ast::VariableDeclarationKind::Const
          )
        {
          let Some(dec) = v.declarations.first() else {
            return Err(crate::errors::CompilerError::from(
              "unsupported empty for-in declaration",
            ));
          };
          let mut names = HashSet::new();
          self.add_var_names_from_binding(&dec.id, &mut names);
          let mut names: Vec<_> = names.into_iter().collect();
          names.sort();
          for name in names {
            let slot = self.add_local_with_mirror(name.clone(), None, false);
            self.mark_local_tdz(slot);
            self.add_lexical_binding(&name);
            if matches!(v.kind, oxc_ast::ast::VariableDeclarationKind::Const) {
              self.mark_local_immutable(slot);
            }
            self.emit(LoweringOpcode::ResetLocalCell, LINE);
            self.emit_slot(slot, LINE);
          }
        }
        self.activate_current_scope_lexical_bindings();

        self.emit_get_builtin_name("forInKeys");
        self.compile_expr_oast(&f.right)?;
        self.emit(LoweringOpcode::Call, LINE);
        self.emit_byte(1, LINE);

        let keys_slot = self.add_local_in_scope(
          self.current_function_scope_index(),
          format!("\0forin_keys_{}", self.locals.len()),
          None,
          false,
        );
        self.emit(LoweringOpcode::StoreLocal, LINE);
        self.emit_slot(keys_slot, LINE);

        self.emit_constant(Value::Number(0.0), LINE);
        let index_slot = self.add_local_in_scope(
          self.current_function_scope_index(),
          format!("\0forin_i_{}", self.locals.len()),
          None,
          false,
        );
        self.emit(LoweringOpcode::StoreLocal, LINE);
        self.emit_slot(index_slot, LINE);

        let loop_start = self.current_pc();

        self.emit(LoweringOpcode::LoadLocal, LINE);
        self.emit_slot(index_slot, LINE);
        self.emit(LoweringOpcode::LoadLocal, LINE);
        self.emit_slot(keys_slot, LINE);
        self.emit_constant(Value::String("length".to_string().into()), LINE);
        self.emit(LoweringOpcode::GetProperty, LINE);
        self.emit_with_payload(LoweringOpcode::CompareJump, 2, LINE);
        let exit_jump = self.current_pc();
        self.emit_byte(0, LINE);
        self.emit_byte(0, LINE);

        self.emit(LoweringOpcode::LoadLocal, LINE);
        self.emit_slot(keys_slot, LINE);
        self.emit(LoweringOpcode::LoadLocal, LINE);
        self.emit_slot(index_slot, LINE);
        self.emit(LoweringOpcode::GetProperty, LINE);

        match &f.left {
          OForLeft::VariableDeclaration(v) => {
            let Some(dec) = v.declarations.first() else {
              return Err(crate::errors::CompilerError::from(
                "unsupported empty for-in declaration",
              ));
            };
            if matches!(
              v.kind,
              oxc_ast::ast::VariableDeclarationKind::Let
                | oxc_ast::ast::VariableDeclarationKind::Const
            ) {
              let mut names = HashSet::new();
              self.add_var_names_from_binding(&dec.id, &mut names);
              let mut names: Vec<_> = names.into_iter().collect();
              names.sort();
              for name in &names {
                if let Some(slot) = self.resolve_local(name) {
                  self.emit(LoweringOpcode::ResetLocalCell, LINE);
                  self.emit_slot(slot, LINE);
                }
              }
            }
            self.compile_binding_pattern_assignment_oast(&dec.id, v.kind)?;
          }
          OForLeft::AssignmentTargetIdentifier(id) => {
            let name = id.name.as_str();
            if let Some(slot) = self.resolve_local(name) {
              self.emit(LoweringOpcode::StoreLocal, LINE);
              self.emit_slot(slot, LINE);
              if self.local_mirrors_global(slot) {
                self.emit(LoweringOpcode::LoadLocal, LINE);
                self.emit_slot(slot, LINE);
                self.emit_set_global_name(name);
              }
            } else if let Some(upvalue) = self.resolve_upvalue(name) {
              self.emit(LoweringOpcode::SetUpvalue, LINE);
              self.emit_u16(upvalue, LINE);
            } else {
              self.emit_set_global_name(name);
            }
          }
          _ => {
            return Err(crate::errors::CompilerError::from(
              "unsupported for-in assignment target",
            ));
          }
        }

        self.enter_block_scope();
        self.compile_statement_oast_with_mode(&f.body, false, completion_slot)?;
        self.exit_scope();

        let update_start = self.current_pc();
        if let Some(patches) = self.continue_patches.pop() {
          for patch_pos in patches {
            self.patch_jump_to_target(patch_pos, update_start);
          }
        }

        self.emit(LoweringOpcode::LoadLocal, LINE);
        self.emit_slot(index_slot, LINE);
        self.emit_constant(Value::Number(1.0), LINE);
        self.emit(LoweringOpcode::Add, LINE);
        self.emit(LoweringOpcode::StoreLocal, LINE);
        self.emit_slot(index_slot, LINE);

        self.emit(LoweringOpcode::Jump, LINE);
        let back_offset = loop_start as isize - (self.current_pc() as isize);
        self.emit_short_signed(back_offset, LINE);

        self.patch_jump(exit_jump);
        let loop_end = self.current_pc();
        if let Some(patches) = self.break_patches.pop() {
          for patch_pos in patches {
            self.patch_jump_to_target(patch_pos, loop_end);
          }
        }
        let label_patches = if let Some(state) = self.label_patches.last_mut() {
          if state.can_continue && state.owner_loop_depth == self.loop_depth {
            Some((
              std::mem::take(&mut state.continue_patches),
              std::mem::take(&mut state.break_patches),
            ))
          } else {
            None
          }
        } else {
          None
        };
        if let Some((continue_patches, break_patches)) = label_patches {
          let loop_end = self.current_pc();
          self.patch_jump_list(continue_patches, update_start);
          self.patch_jump_list(break_patches, loop_end);
        }
        self.loop_depth = self.loop_depth.saturating_sub(1);
        self.exit_scope();
      }
      OStmt::ReturnStatement(r) => {
        let return_target = self.nearest_finally_index().map(|index| {
          (
            index,
            self.finally_patches[index].return_slot,
            self.finally_patches[index].did_return_slot,
          )
        });
        if let Some((finally_index, return_slot, did_return_slot)) = return_target {
          if let Some(arg) = &r.argument {
            self.compile_expr_oast(arg)?;
          } else {
            self.emit(LoweringOpcode::Undefined, LINE);
          }
          self.emit(LoweringOpcode::StoreLocal, LINE);
          self.emit_slot(return_slot, LINE);
          self.emit_handler_pops_to(self.finally_patches[finally_index].handler_depth);
          self.emit_with_env_pops();
          self.emit(LoweringOpcode::True, LINE);
          self.emit(LoweringOpcode::StoreLocal, LINE);
          self.emit_slot(did_return_slot, LINE);
          self.emit(LoweringOpcode::Jump, LINE);
          let patch_pos = self.current_pc();
          self.emit_byte(0, LINE);
          self.emit_byte(0, LINE);
          if let Some(index) = self.nearest_finally_index() {
            let state = &mut self.finally_patches[index];
            if state.in_catch {
              state.catch_return_jumps.push(patch_pos);
            } else {
              state.return_jumps.push(patch_pos);
            }
          }
          return Ok(());
        }
        if let Some(arg) = &r.argument {
          if self.handler_depth == 0
            && self.iterator_close_stack.is_empty()
            && self.with_depth == 0
            && self.finally_patches.is_empty()
          {
            self.compile_expr_oast_tail(arg)?;
          } else {
            self.compile_expr_oast(arg)?;
          }
        } else {
          self.emit(LoweringOpcode::Undefined, LINE);
        }
        if !self.iterator_close_stack.is_empty() {
          let return_slot = self.alloc_temp_local("return_value");
          self.emit(LoweringOpcode::StoreLocal, LINE);
          self.emit_slot(return_slot, LINE);
          self.emit_iterator_closes_above(0)?;
          self.emit(LoweringOpcode::LoadLocal, LINE);
          self.emit_slot(return_slot, LINE);
        }
        self.emit_with_env_pops();
        self.emit(LoweringOpcode::Return, LINE);
      }
      OStmt::ContinueStatement(c) => {
        if let Some(label) = &c.label {
          let label_name = label.name.to_string();
          let idx = self
            .label_patches
            .iter()
            .rposition(|state| state.name == label_name)
            .ok_or_else(|| {
              crate::errors::CompilerError::from(format!(
                "SyntaxError: unknown label '{}'",
                label_name
              ))
            })?;
          if !self.label_patches[idx].can_continue {
            return Err(crate::errors::CompilerError::from(
              "SyntaxError: 'continue' used with non-iteration label",
            ));
          }
          let target_loop_depth = self.label_patches[idx].owner_loop_depth;
          if self.emit_control_via_finally(true, target_loop_depth, Some(idx))? {
            return Ok(());
          }
          self.emit_iterator_closes_above(target_loop_depth)?;
          self.emit_with_env_pops();
          self.emit(LoweringOpcode::Jump, LINE);
          let patch_pos = self.current_pc();
          self.emit_byte(0, LINE);
          self.emit_byte(0, LINE);
          self.label_patches[idx]
            .continue_patches
            .push(patch_pos);
        } else {
          if self.continue_patches.is_empty() {
            return Err(crate::errors::CompilerError::from(
              "SyntaxError: 'continue' used outside of a loop",
            ));
          }
          let loop_index = self.continue_patches.len() - 1;
          if self.emit_control_via_finally(true, loop_index, None)? {
            return Ok(());
          }
          self.emit_with_env_pops();
          self.emit(LoweringOpcode::Jump, LINE);
          let patch_pos = self.current_pc();
          self.emit_byte(0, LINE);
          self.emit_byte(0, LINE);
          if let Some(patches) = self.continue_patches.last_mut() {
            patches.push(patch_pos);
          }
        }
      }
      OStmt::BreakStatement(b) => {
        if let Some(label) = &b.label {
          let label_name = label.name.to_string();
          let idx = self
            .label_patches
            .iter()
            .rposition(|state| state.name == label_name)
            .ok_or_else(|| {
              crate::errors::CompilerError::from(format!(
                "SyntaxError: unknown label '{}'",
                label_name
              ))
            })?;
          let target_loop_depth = self.label_patches[idx].owner_loop_depth;
          if self.emit_control_via_finally(false, target_loop_depth, Some(idx))? {
            return Ok(());
          }
          self.emit_iterator_closes_above(target_loop_depth)?;
          self.emit_with_env_pops();
          self.emit(LoweringOpcode::Jump, LINE);
          let patch_pos = self.current_pc();
          self.emit_byte(0, LINE);
          self.emit_byte(0, LINE);
          self.label_patches[idx]
            .break_patches
            .push(patch_pos);
        } else {
          if self.break_patches.is_empty() {
            return Err(crate::errors::CompilerError::from(
              "SyntaxError: 'break' used outside of a loop",
            ));
          }
          let loop_index = self.break_patches.len() - 1;
          if self.emit_control_via_finally(false, loop_index, None)? {
            return Ok(());
          }
          self.emit_with_env_pops();
          self.emit(LoweringOpcode::Jump, LINE);
          let patch_pos = self.current_pc();
          self.emit_byte(0, LINE);
          self.emit_byte(0, LINE);
          if let Some(patches) = self.break_patches.last_mut() {
            patches.push(patch_pos);
          }
        }
      }
      OStmt::ThrowStatement(t) => {
        self.compile_expr_oast(&t.argument)?;
        self.emit(LoweringOpcode::Throw, LINE);
      }
      OStmt::TryStatement(t) => {
        let owns_completion_slot = completion_slot.is_none();
        let try_completion_slot = if let Some(slot) = completion_slot {
          slot
        } else {
          let slot = self.alloc_temp_local("try_result");
          self.emit(LoweringOpcode::Undefined, LINE);
          self.emit(LoweringOpcode::StoreLocal, LINE);
          self.emit_slot(slot, LINE);
          slot
        };
        let handler = t.handler.as_ref();
        let finalizer = t.finalizer.as_ref();
        if finalizer.is_some() {
          let return_slot = self.add_local_with_mirror(
            format!("\0finally_return_{}", self.locals.len()),
            None,
            false,
          );
          let did_return_slot = self.add_local_with_mirror(
            format!("\0finally_did_return_{}", self.locals.len() + 1),
            None,
            false,
          );
          self.emit(LoweringOpcode::Undefined, LINE);
          self.emit(LoweringOpcode::StoreLocal, LINE);
          self.emit_slot(return_slot, LINE);
          self.emit(LoweringOpcode::False, LINE);
          self.emit(LoweringOpcode::StoreLocal, LINE);
          self.emit_slot(did_return_slot, LINE);
          self
            .finally_patches
            .push(crate::compiler::FinallyPatchState {
              return_slot,
              did_return_slot,
              return_jumps: Vec::new(),
              catch_return_jumps: Vec::new(),
              entry_jumps: Vec::new(),
              control_actions: Vec::new(),
              handler_depth: self.handler_depth,
              loop_depth: self.loop_depth,
              in_finalizer: false,
              in_catch: false,
            });
        }
        let finally_idx = finalizer
          .as_ref()
          .map(|_| self.finally_patches.len().saturating_sub(1));

        let push_handler_pc = self.current_pc();
        self.emit(LoweringOpcode::PushHandler, LINE);
        let handler_jump = self.current_pc();
        self.emit_byte(0, LINE);
        self.emit_byte(0, LINE);

        self.handler_depth += 1;
        self.enter_block_scope_with_vars(&t.block.body);
        for stmt in &t.block.body {
          self.compile_statement_oast_with_mode(stmt, false, Some(try_completion_slot))?;
        }
        self.exit_scope();
        self.handler_depth = self.handler_depth.saturating_sub(1);

        self.emit(LoweringOpcode::PopHandler, LINE);

        if let Some(fin) = finalizer {
          let finally_idx = finally_idx
            .ok_or_else(|| crate::errors::CompilerError::from("finally state missing"))?;
          {
            let state = self
              .finally_patches
              .get_mut(finally_idx)
              .ok_or_else(|| crate::errors::CompilerError::from("finally state missing"))?;
            state.in_finalizer = true;
          }
          let body_start = self.current_pc();
          self.enter_block_scope_with_vars(&fin.body);
          for stmt in &fin.body {
            self.compile_statement_oast_with_mode(stmt, false, Some(try_completion_slot))?;
          }
          self.exit_scope();
          let outer_finally_idx = self.finally_patches[..finally_idx]
            .iter()
            .rposition(|state| !state.in_finalizer);
          let (did_return_slot, return_slot, return_jumps, entry_jumps) = {
            let state = self
              .finally_patches
              .get_mut(finally_idx)
              .ok_or_else(|| crate::errors::CompilerError::from("finally state missing"))?;
            state.in_finalizer = false;
            (
              state.did_return_slot,
              state.return_slot,
              std::mem::take(&mut state.return_jumps),
              std::mem::take(&mut state.entry_jumps),
            )
          };
          for patch_pos in return_jumps.into_iter().chain(entry_jumps) {
            self.patch_jump_to_target(patch_pos, body_start);
          }
          self
            .chunk
            .finally_handlers
            .push(crate::bytecode::FinallyHandler {
              push_pc: push_handler_pc,
              finalizer_pc: body_start,
              return_slot,
              did_return_slot,
            });
          self.emit(LoweringOpcode::LoadLocal, LINE);
          self.emit_slot(did_return_slot, LINE);
          self.emit(LoweringOpcode::JumpIfFalse, LINE);
          let skip_return = self.current_pc();
          self.emit_byte(0, LINE);
          self.emit_byte(0, LINE);
          if let Some(outer_idx) = outer_finally_idx {
            let outer_return_slot = self.finally_patches[outer_idx].return_slot;
            let outer_did_return_slot = self.finally_patches[outer_idx].did_return_slot;
            self.emit(LoweringOpcode::LoadLocal, LINE);
            self.emit_slot(return_slot, LINE);
            self.emit(LoweringOpcode::StoreLocal, LINE);
            self.emit_slot(outer_return_slot, LINE);
            self.emit(LoweringOpcode::True, LINE);
            self.emit(LoweringOpcode::StoreLocal, LINE);
            self.emit_slot(outer_did_return_slot, LINE);
            self.emit_handler_pops_to(self.finally_patches[outer_idx].handler_depth);
            self.emit(LoweringOpcode::Jump, LINE);
            let outer_jump = self.current_pc();
            self.emit_byte(0, LINE);
            self.emit_byte(0, LINE);
            self.finally_patches[outer_idx]
              .entry_jumps
              .push(outer_jump);
          } else {
            self.emit(LoweringOpcode::LoadLocal, LINE);
            self.emit_slot(return_slot, LINE);
            if !self.iterator_close_stack.is_empty() {
              let pending_return_slot = self.alloc_temp_local("finally_return_value");
              self.emit(LoweringOpcode::StoreLocal, LINE);
              self.emit_slot(pending_return_slot, LINE);
              self.emit_iterator_closes_above(0)?;
              self.emit(LoweringOpcode::LoadLocal, LINE);
              self.emit_slot(pending_return_slot, LINE);
            }
            self.emit(LoweringOpcode::Return, LINE);
          }
          self.patch_jump(skip_return);
          self.emit_control_dispatch(finally_idx)?;
        }
        self.emit(LoweringOpcode::Jump, LINE);
        let end_jump = self.current_pc();
        self.emit_byte(0, LINE);
        self.emit_byte(0, LINE);

        self.patch_jump(handler_jump);

        let mut catch_handler_jump = None;
        match handler {
          Some(handler) => {
            if let Some(finally_idx) = finally_idx {
              self.finally_patches[finally_idx].in_catch = true;
            }
            if finalizer.is_some() {
              self.emit(LoweringOpcode::PushHandler, LINE);
              catch_handler_jump = Some(self.current_pc());
              self.emit_byte(0, LINE);
              self.emit_byte(0, LINE);
              self.handler_depth += 1;
            }
            if let Some(param) = &handler.param {
              let name = match &param.pattern {
                OPat::BindingIdentifier(i) => i.name.to_string(),
                _ => "\0catch_pattern".to_string(),
              };
              if let Some(error) =
                Self::strict_binding_pattern_name_error(self.strict_context, &param.pattern)
              {
                return Err(error);
              }
              self.enter_block_scope();
              if !matches!(param.pattern, OPat::BindingIdentifier(_)) {
                let mut binding_names = HashSet::new();
                Self::collect_binding_names(&param.pattern, &mut binding_names);
                let mut binding_names: Vec<_> = binding_names.into_iter().collect();
                binding_names.sort();
                for binding_name in binding_names {
                  let slot = self.add_local_with_mirror(binding_name.clone(), None, false);
                  self.mark_local_tdz(slot);
                  self.add_lexical_binding(&binding_name);
                }
              }
              let slot = self.add_local_with_mirror(name, None, false);
              if !self.locals[slot].starts_with('\0') {
                self.mark_local_tdz(slot);
                let binding_name = self.locals[slot].clone();
                self.add_lexical_binding(&binding_name);
              }
              self.activate_current_scope_lexical_bindings();
              self.emit(LoweringOpcode::StoreLocal, LINE);
              self.emit_slot(slot, LINE);
              if !matches!(param.pattern, OPat::BindingIdentifier(_)) {
                self.emit(LoweringOpcode::LoadLocal, LINE);
                self.emit_slot(slot, LINE);
                self.compile_binding_pattern_assignment_oast(
                  &param.pattern,
                  oxc_ast::ast::VariableDeclarationKind::Let,
                )?;
              }
              self.enter_block_scope_with_vars(&handler.body.body);
              for stmt in &handler.body.body {
                self.compile_statement_oast_with_mode(stmt, false, Some(try_completion_slot))?;
              }
              self.exit_scope();
              self.exit_scope();
            } else {
              self.emit(LoweringOpcode::Discard, LINE);
              self.enter_block_scope_with_vars(&handler.body.body);
              for stmt in &handler.body.body {
                self.compile_statement_oast_with_mode(stmt, false, Some(try_completion_slot))?;
              }
              self.exit_scope();
            }

            if finalizer.is_some() {
              self.handler_depth = self.handler_depth.saturating_sub(1);
              self.emit(LoweringOpcode::PopHandler, LINE);
            }
            if let Some(finally_idx) = finally_idx {
              self.finally_patches[finally_idx].in_catch = false;
            }
            if let Some(fin) = finalizer {
              let finally_idx = finally_idx
                .ok_or_else(|| crate::errors::CompilerError::from("finally state missing"))?;
              {
                let state = self
                  .finally_patches
                  .get_mut(finally_idx)
                  .ok_or_else(|| crate::errors::CompilerError::from("finally state missing"))?;
                state.in_finalizer = true;
              }
              let catch_finalizer_start = self.current_pc();
              self.enter_block_scope_with_vars(&fin.body);
              for stmt in &fin.body {
                self.compile_statement_oast_with_mode(stmt, false, Some(try_completion_slot))?;
              }
              self.exit_scope();
              let (did_return_slot, return_slot, catch_return_jumps) = {
                let state = self
                  .finally_patches
                  .get_mut(finally_idx)
                  .ok_or_else(|| crate::errors::CompilerError::from("finally state missing"))?;
                state.in_finalizer = false;
                (
                  state.did_return_slot,
                  state.return_slot,
                  std::mem::take(&mut state.catch_return_jumps),
                )
              };
              for patch_pos in catch_return_jumps {
                self.patch_jump_to_target(patch_pos, catch_finalizer_start);
              }
              self.emit(LoweringOpcode::LoadLocal, LINE);
              self.emit_slot(did_return_slot, LINE);
              self.emit(LoweringOpcode::JumpIfFalse, LINE);
              let skip_return = self.current_pc();
              self.emit_byte(0, LINE);
              self.emit_byte(0, LINE);
              let outer_finally_idx = self.finally_patches[..finally_idx]
                .iter()
                .rposition(|state| !state.in_finalizer);
              if let Some(outer_idx) = outer_finally_idx {
                let outer_return_slot = self.finally_patches[outer_idx].return_slot;
                let outer_did_return_slot = self.finally_patches[outer_idx].did_return_slot;
                self.emit(LoweringOpcode::LoadLocal, LINE);
                self.emit_slot(return_slot, LINE);
                self.emit(LoweringOpcode::StoreLocal, LINE);
                self.emit_slot(outer_return_slot, LINE);
                self.emit(LoweringOpcode::True, LINE);
                self.emit(LoweringOpcode::StoreLocal, LINE);
                self.emit_slot(outer_did_return_slot, LINE);
                self.emit_handler_pops_to(self.finally_patches[outer_idx].handler_depth);
                self.emit(LoweringOpcode::Jump, LINE);
                let outer_jump = self.current_pc();
                self.emit_byte(0, LINE);
                self.emit_byte(0, LINE);
                self.finally_patches[outer_idx]
                  .entry_jumps
                  .push(outer_jump);
              } else {
                self.emit(LoweringOpcode::LoadLocal, LINE);
                self.emit_slot(return_slot, LINE);
                self.emit(LoweringOpcode::Return, LINE);
              }
              self.patch_jump(skip_return);
              self.emit_control_dispatch(finally_idx)?;

              if let Some(catch_handler_jump) = catch_handler_jump {
                let thrown_slot =
                  self.add_local_with_mirror("\0catch_finally_thrown".to_string(), None, false);
                self.emit(LoweringOpcode::Jump, LINE);
                let skip_abrupt_finalizer = self.current_pc();
                self.emit_byte(0, LINE);
                self.emit_byte(0, LINE);
                self.patch_jump(catch_handler_jump);
                self.emit(LoweringOpcode::StoreLocal, LINE);
                self.emit_slot(thrown_slot, LINE);
                self.finally_patches[finally_idx].in_finalizer = true;
                self.enter_block_scope_with_vars(&fin.body);
                for stmt in &fin.body {
                  self.compile_statement_oast_with_mode(stmt, false, Some(try_completion_slot))?;
                }
                self.exit_scope();
                self.finally_patches[finally_idx].in_finalizer = false;
                self.emit(LoweringOpcode::LoadLocal, LINE);
                self.emit_slot(thrown_slot, LINE);
                self.emit(LoweringOpcode::Throw, LINE);
                self.patch_jump(skip_abrupt_finalizer);
              }
            }
          }
          None => {
            if finalizer.is_none() {
              return Err(crate::errors::CompilerError::from(
                "try without catch/finally not supported",
              ));
            }
            self.enter_block_scope();
            let slot = self.add_local_with_mirror("\0finally_thrown".to_string(), None, false);
            self.emit(LoweringOpcode::StoreLocal, LINE);
            self.emit_slot(slot, LINE);
            if let Some(fin) = finalizer {
              let finally_idx = finally_idx
                .ok_or_else(|| crate::errors::CompilerError::from("finally state missing"))?;
              {
                let state = self
                  .finally_patches
                  .get_mut(finally_idx)
                  .ok_or_else(|| crate::errors::CompilerError::from("finally state missing"))?;
                state.in_finalizer = true;
              }
              self.enter_block_scope_with_vars(&fin.body);
              for stmt in &fin.body {
                self.compile_statement_oast_with_mode(stmt, false, Some(try_completion_slot))?;
              }
              self.exit_scope();
              let state = self
                .finally_patches
                .get_mut(finally_idx)
                .ok_or_else(|| crate::errors::CompilerError::from("finally state missing"))?;
              state.in_finalizer = false;
            }
            self.emit(LoweringOpcode::LoadLocal, LINE);
            self.emit_slot(slot, LINE);
            self.emit(LoweringOpcode::Throw, LINE);
            self.exit_scope();
          }
        }

        self.patch_jump(end_jump);
        if finalizer.is_some() {
          self.finally_patches.pop();
        }
        if owns_completion_slot {
          self.emit(LoweringOpcode::LoadLocal, LINE);
          self.emit_slot(try_completion_slot, LINE);
        }
      }
      _ => {}
    }
    Ok(())
  }
}
