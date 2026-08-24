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

use super::emit::FrontendOp as Op;
use crate::Compiler;
use crate::bytecode::{Chunk, FunctionProto};
use crate::typecheck::Type;
use crate::value::Value;
use oxc_ast::ast as oast;
use oxc_span::{GetSpan, SourceType};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

const LINE: u32 = 1;

impl Compiler {
  fn emit_with_env_pops(&mut self) {
    for _ in 0..self.with_depth {
      self.emit(Op::PopEnv, LINE);
    }
  }

  fn emit_handler_pops_to(
    &mut self,
    depth: usize,
  ) {
    for _ in depth..self.handler_depth {
      self.emit(Op::PopHandler, LINE);
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
    self.emit(Op::True, LINE);
    self.emit(Op::StoreLocal, LINE);
    self.emit_byte(self.slot_to_u8(flag_slot)?, LINE);
    let target_depth = if label_index.is_some() {
      loop_index
    } else {
      loop_index + 1
    };
    self.emit_iterator_closes_above(target_depth)?;
    self.emit_handler_pops_to(self.finally_patches[index].handler_depth);
    self.emit_with_env_pops();
    self.emit(Op::Jump, LINE);
    let patch_pos = self.current_ip();
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
      self.emit(Op::LoadLocal, LINE);
      self.emit_byte(self.slot_to_u8(action.flag_slot)?, LINE);
      self.emit(Op::JumpIfFalse, LINE);
      let skip = self.current_ip();
      self.emit_byte(0, LINE);
      self.emit_byte(0, LINE);
      self.emit(Op::Jump, LINE);
      let patch_pos = self.current_ip();
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
      self.emit(Op::LoadLocal, LINE);
      self.emit_byte(self.slot_to_u8(slot)?, LINE);
      self.emit(Op::IteratorClose, LINE);
      self.emit(Op::Discard, LINE);
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
      self.emit(Op::MakeClosure, LINE);
      self.emit_byte(self.function_index_to_u8(idx)?, LINE);
      let slot = if let Some(s) = self.resolve_local(&fname) {
        s
      } else {
        self.add_local_with_mirror(fname.clone(), None, self.is_root)
      };

      if slot == self.locals.len() - 1 {
        self.add_lexical_binding(&fname);
      }

      if self.is_root {
        self.emit(Op::Copy, LINE);
      }
      self.emit(Op::StoreLocal, LINE);
      self.emit_byte(self.slot_to_u8(slot)?, LINE);
      if self.is_root {
        self.emit_set_global_name(&fname);
      }
      let is_exported = self
        .current_module
        .as_ref()
        .is_some_and(|c| c.exports.contains(&fname));
      if is_exported {
        self.emit(Op::LoadLocal, LINE);
        self.emit_byte(self.slot_to_u8(slot)?, LINE);
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
  fn compile_statement_oast(
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
          self.emit(Op::GetModuleExport, LINE);
          self.emit_byte(descriptor.to_le_bytes()[0], LINE);
          self.emit_byte(descriptor.to_le_bytes()[1], LINE);
          self.emit_set_own_module_export(&export_name)?;
        }
        // Plain `export * from "module"` is resolved by the module loader;
        // it has no runtime side effects in this module.
      }
      OStmt::ExportNamedDeclaration(export) => {
        // export {  } from "module"
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
              self.emit(Op::MakeClosure, LINE);
              self.emit_byte(self.function_index_to_u8(idx)?, LINE);
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
              self.compile_expr_oast(expr)?;
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
          self.emit(Op::StoreLocal, LINE);
          self.emit_byte(self.slot_to_u8(slot)?, LINE);
        } else if !skip_pop {
          self.emit(Op::Discard, LINE);
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
          let target = self.current_ip();
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
        self.emit(Op::PushWithEnv, LINE);
        self.with_depth += 1;
        self.emit(Op::PushHandler, LINE);
        let handler_jump = self.current_ip();
        self.emit_byte(0, LINE);
        self.emit_byte(0, LINE);

        let with_body_stmts = match &w.body {
          OStmt::BlockStatement(b) => &b.body[..],
          _ => std::slice::from_ref(&w.body),
        };
        self.enter_block_scope_with_vars(with_body_stmts);
        self.compile_statement_oast_with_mode(&w.body, false, completion_slot)?;
        self.exit_scope();

        self.emit(Op::PopHandler, LINE);
        self.emit(Op::PopEnv, LINE);
        self.with_depth = self.with_depth.saturating_sub(1);
        self.emit(Op::Jump, LINE);
        let skip_handler_jump = self.current_ip();
        self.emit_byte(0, LINE);
        self.emit_byte(0, LINE);

        let _handler_start = self.current_ip();
        self.patch_jump(handler_jump);

        self.emit(Op::PopEnv, LINE);
        self.with_depth = self.with_depth.saturating_sub(1);
        self.emit(Op::Throw, LINE);
        let _after_handler = self.current_ip();

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
            self.emit(Op::Jump, LINE);
            let jump_else = self.current_ip();
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
          self.emit(Op::JumpIfFalse, LINE);
          let jump_over = self.current_ip();
          self.emit_byte(0, LINE);
          self.emit_byte(0, LINE);
          self.compile_statement_oast_with_mode(&i.consequent, false, completion_slot)?;
          if let Some(alt) = &i.alternate {
            self.emit(Op::Jump, LINE);
            let jump_else = self.current_ip();
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
        // Evaluate discriminant once and store in a local slot.
        self.compile_expr_oast(&s.discriminant)?;
        let discr_name = format!("\0switch_discriminant_{}", self.locals.len());
        let discr_slot =
          self.add_local_in_scope(self.current_function_scope_index(), discr_name, None, false);
        self.emit(Op::StoreLocal, LINE);
        self.emit_byte(self.slot_to_u8(discr_slot)?, LINE);

        // Case clauses share one lexical environment, instantiated before any
        // case selector or consequent is evaluated.
        self.enter_block_scope();
        for case in &s.cases {
          self.pre_declare_variables_oast_stmts(&case.consequent);
        }

        // `break` inside a switch should jump to switch end.
        self.break_patches.push(Vec::new());

        // First phase: dispatch checks, each successful match jumps to
        // the corresponding case body.
        let mut case_body_jumps: Vec<(usize, usize)> = Vec::new();
        let mut default_index: Option<usize> = None;
        for (case_idx, case) in s.cases.iter().enumerate() {
          if let Some(test) = &case.test {
            self.emit(Op::LoadLocal, LINE);
            self.emit_byte(self.slot_to_u8(discr_slot)?, LINE);
            self.compile_expr_oast(test)?;
            // Use CompareJump: cond_type 6 = StrictEq, jump if false.
            self.emit_with_payload(Op::CompareJump, 6, LINE);
            let skip_jump = self.current_ip();
            self.emit_byte(0, LINE);
            self.emit_byte(0, LINE);

            self.emit(Op::Jump, LINE);
            let body_jump = self.current_ip();
            self.emit_byte(0, LINE);
            self.emit_byte(0, LINE);
            case_body_jumps.push((case_idx, body_jump));

            self.patch_jump(skip_jump);
          } else {
            default_index = Some(case_idx);
          }
        }

        // If no case matched, jump to default (if any), else to end.
        self.emit(Op::Jump, LINE);
        let no_match_jump = self.current_ip();
        self.emit_byte(0, LINE);
        self.emit_byte(0, LINE);

        // Second phase: emit case bodies in source order. Fallthrough is
        // represented naturally by layout.
        let mut case_starts: Vec<usize> = vec![0; s.cases.len()];
        for (case_idx, case) in s.cases.iter().enumerate() {
          case_starts[case_idx] = self.current_ip();
          for stmt in &case.consequent {
            self.compile_statement_oast_with_mode(stmt, false, completion_slot)?;
          }
        }
        let switch_end = self.current_ip();

        // Patch successful-case jumps to case body starts.
        for (case_idx, jump_pos) in case_body_jumps {
          self.patch_jump_to_target(jump_pos, case_starts[case_idx]);
        }

        // Patch no-match jump to default start or end.
        let default_target = default_index.map_or(switch_end, |idx| case_starts[idx]);
        self.patch_jump_to_target(no_match_jump, default_target);

        // Patch `break` statements emitted within this switch.
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
          self.emit(Op::Undefined, LINE);
          self.emit(Op::StoreLocal, LINE);
          self.emit_byte(self.slot_to_u8(slot)?, LINE);
          slot
        };
        let loop_start = self.current_ip();
        let exit_jump = if let Some(jump_over) = self.try_emit_compare_jump_if_false(&w.test)? {
          Some(jump_over)
        } else {
          self.compile_expr_oast(&w.test)?;
          self.emit(Op::JumpIfFalse, LINE);
          let exit_jump = self.current_ip();
          self.emit_byte(0, LINE);
          self.emit_byte(0, LINE);
          Some(exit_jump)
        };
        self.enter_block_scope();
        self.compile_statement_oast_with_mode(&w.body, false, Some(loop_completion_slot))?;
        self.exit_scope();
        self.emit(Op::Jump, LINE);
        let _back_jump_pos = self.current_ip();
        // back_offset in instruction units: loop_start - (current_ip + 1)
        let back_offset = loop_start as isize - (self.current_ip() as isize);
        self.emit_short_signed(back_offset, LINE);
        if let Some(patches) = self.continue_patches.pop() {
          for patch_pos in patches {
            self.patch_jump_to_target(patch_pos, loop_start);
          }
        }
        let cleanup_start = self.current_ip();
        if owns_completion_slot {
          self.emit(Op::LoadLocal, LINE);
          self.emit_byte(self.slot_to_u8(loop_completion_slot)?, LINE);
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
          self.emit(Op::Undefined, LINE);
          self.emit(Op::StoreLocal, LINE);
          self.emit_byte(self.slot_to_u8(slot)?, LINE);
          slot
        };
        let loop_start = self.current_ip();
        self.enter_block_scope();
        self.compile_statement_oast_with_mode(&d.body, false, Some(loop_completion_slot))?;
        self.exit_scope();
        let test_start = self.current_ip();
        let exit_jump = if let Some(jump_over) = self.try_emit_compare_jump_if_false(&d.test)? {
          Some(jump_over)
        } else {
          self.compile_expr_oast(&d.test)?;
          self.emit(Op::JumpIfFalse, LINE);
          let exit_jump = self.current_ip();
          self.emit_byte(0, LINE);
          self.emit_byte(0, LINE);
          Some(exit_jump)
        };
        self.emit(Op::Jump, LINE);
        let _back_jump_pos = self.current_ip();
        let back_offset = loop_start as isize - (self.current_ip() as isize);
        self.emit_short_signed(back_offset, LINE);
        if let Some(patches) = self.continue_patches.pop() {
          for patch_pos in patches {
            self.patch_jump_to_target(patch_pos, test_start);
          }
        }
        let cleanup_start = self.current_ip();
        if owns_completion_slot {
          self.emit(Op::LoadLocal, LINE);
          self.emit_byte(self.slot_to_u8(loop_completion_slot)?, LINE);
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
        // The initializer's lexical declarations belong to a scope enclosing
        // the test, update, and body, but not code after the loop.
        self.enter_block_scope();
        self.loop_depth += 1;
        self.continue_patches.push(Vec::new());
        self.break_patches.push(Vec::new());
        let mut per_iteration_slots = Vec::new();

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
                  let mirror_global =
                    self.is_root && matches!(v.kind, oxc_ast::ast::VariableDeclarationKind::Var);
                  let slot = if matches!(v.kind, oxc_ast::ast::VariableDeclarationKind::Var) {
                    self
                      .resolve_local(&name)
                      .unwrap_or_else(|| self.add_var_local(name.clone(), None, mirror_global))
                  } else {
                    let slot = self.add_local_with_mirror(name.clone(), None, false);
                    self.mark_local_tdz(slot);
                    self.add_lexical_binding(&name);
                    slot
                  };
                  if matches!(v.kind, oxc_ast::ast::VariableDeclarationKind::Const) {
                    self.mark_local_immutable(slot);
                  }
                  // Lexical bindings are instantiated before evaluating any
                  // initializer, so self-references observe the TDZ.
                  if let Some(initializer) = &declaration.init {
                    self.compile_expr_oast(initializer)?;
                  } else {
                    self.emit(Op::Undefined, LINE);
                  }
                  if matches!(
                    v.kind,
                    oxc_ast::ast::VariableDeclarationKind::Let
                      | oxc_ast::ast::VariableDeclarationKind::Const
                  ) {
                    per_iteration_slots.push(slot);
                  }
                  if mirror_global {
                    self.emit(Op::Copy, LINE);
                  }
                  self.emit(Op::StoreLocal, LINE);
                  self.emit_byte(self.slot_to_u8(slot)?, LINE);
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
              self.emit(Op::Discard, LINE);
            }
          }
        }

        for &slot in &per_iteration_slots {
          self.emit(Op::CloneLocalCell, LINE);
          self.emit_byte(self.slot_to_u8(slot)?, LINE);
        }

        let loop_start = self.current_ip();
        let exit_jump = if let Some(test) = &f.test {
          if let Some(jump_over) = self.try_emit_compare_jump_if_false(test)? {
            Some(jump_over)
          } else {
            self.compile_expr_oast(test)?;
            self.emit(Op::JumpIfFalse, LINE);
            let exit_jump = self.current_ip();
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

        let update_start = self.current_ip();
        if let Some(patches) = self.continue_patches.pop() {
          for patch_pos in patches {
            self.patch_jump_to_target(patch_pos, update_start);
          }
        }

        for &slot in &per_iteration_slots {
          self.emit(Op::CloneLocalCell, LINE);
          self.emit_byte(self.slot_to_u8(slot)?, LINE);
        }

        if let Some(update) = &f.update {
          self.compile_expr_oast(update)?;
          self.emit(Op::Discard, LINE);
        }

        self.emit(Op::Jump, LINE);
        let back_offset = loop_start as isize - (self.current_ip() as isize);
        self.emit_short_signed(back_offset, LINE);

        if let Some(exit_jump) = exit_jump {
          self.patch_jump(exit_jump);
        }
        let loop_end = self.current_ip();
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
          let loop_end = self.current_ip();
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
          self.emit(Op::Undefined, LINE);
          self.emit(Op::StoreLocal, LINE);
          self.emit_byte(self.slot_to_u8(slot)?, LINE);
          slot
        };

        // Acquire either the ordinary synchronous iterator record or the
        // async iterator record (including async-from-sync fallback).
        self.compile_expr_oast(&f.right)?;
        if f.r#await {
          self.emit(Op::GetAsyncIterator, LINE);
        } else {
          self.emit_get_builtin_name("getIterator");
          self.emit(Op::Exchange, LINE);
          self.emit(Op::Call, LINE);
          self.emit_byte(1, LINE);
        }

        let iterator_slot = self.add_local_in_scope(
          self.current_function_scope_index(),
          format!("\0forof_iter_{}", self.locals.len()),
          None,
          false,
        );
        self.emit(Op::StoreLocal, LINE);
        self.emit_byte(self.slot_to_u8(iterator_slot)?, LINE);

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
            if self.resolve_local(&name).is_none() {
              self.add_local_with_mirror(name.clone(), None, false);
            }
          }
        }

        let thrown_slot = self.alloc_temp_local("forof_thrown");
        let iterator_close_active_slot = self.alloc_temp_local("forof_close_active");
        self.emit(Op::False, LINE);
        self.emit(Op::StoreLocal, LINE);
        self.emit_byte(self.slot_to_u8(iterator_close_active_slot)?, LINE);
        self.emit(Op::PushHandler, LINE);
        let handler_jump = self.current_ip();
        self.emit_byte(0, LINE);
        self.emit_byte(0, LINE);

        let loop_start = self.current_ip();

        self.emit(Op::False, LINE);
        self.emit(Op::StoreLocal, LINE);
        self.emit_byte(self.slot_to_u8(iterator_close_active_slot)?, LINE);
        self.emit(Op::LoadLocal, LINE);
        self.emit_byte(self.slot_to_u8(iterator_slot)?, LINE);
        self.emit(Op::IteratorNext, LINE);
        self.emit(Op::Copy, LINE);
        self.emit_constant(Value::String("done".to_string().into()), LINE);
        self.emit(Op::GetProperty, LINE);
        self.emit(Op::JumpIfTrue, LINE);
        let exit_jump = self.current_ip() - 1;
        self.emit_byte(0, LINE);
        self.emit_byte(0, LINE);
        self.emit_constant(Value::String("value".to_string().into()), LINE);
        self.emit(Op::GetProperty, LINE);
        // IteratorStep and IteratorValue abrupt completions return directly;
        // only failures after a value was produced perform IteratorClose.
        self.emit(Op::True, LINE);
        self.emit(Op::StoreLocal, LINE);
        self.emit_byte(self.slot_to_u8(iterator_close_active_slot)?, LINE);

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
                  self.emit(Op::ResetLocalCell, LINE);
                  self.emit_byte(self.slot_to_u8(slot)?, LINE);
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

        self.emit(Op::Jump, LINE);
        let back_offset = loop_start as isize - (self.current_ip() as isize);
        self.emit_short_signed(back_offset, LINE);

        self.patch_jump(exit_jump);
        // Normal iterator exhaustion should not perform IteratorClose.
        // Also discard the final iterator result object left on the virtual
        // value stack.
        self.emit(Op::Discard, LINE);
        self.emit(Op::PopHandler, LINE);
        self.emit(Op::Jump, LINE);
        let done_end_jump = self.current_ip();
        self.emit_byte(0, LINE);
        self.emit_byte(0, LINE);

        let break_cleanup = self.current_ip();
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
        self.emit(Op::LoadLocal, LINE);
        self.emit_byte(self.slot_to_u8(iterator_slot)?, LINE);
        self.emit(Op::IteratorClose, LINE);
        self.emit(Op::Discard, LINE);
        self.emit(Op::Jump, LINE);
        let end_jump = self.current_ip();
        self.emit_byte(0, LINE);
        self.emit_byte(0, LINE);

        self.patch_jump(handler_jump);
        self.emit(Op::StoreLocal, LINE);
        self.emit_byte(self.slot_to_u8(thrown_slot)?, LINE);
        self.emit(Op::PushHandler, LINE);
        let close_handler_jump = self.current_ip();
        self.emit_byte(0, LINE);
        self.emit_byte(0, LINE);
        self.emit(Op::LoadLocal, LINE);
        self.emit_byte(self.slot_to_u8(iterator_close_active_slot)?, LINE);
        self.emit(Op::JumpIfFalse, LINE);
        let skip_abrupt_close = self.current_ip();
        self.emit_byte(0, LINE);
        self.emit_byte(0, LINE);
        self.emit(Op::LoadLocal, LINE);
        self.emit_byte(self.slot_to_u8(iterator_slot)?, LINE);
        self.emit(Op::IteratorClose, LINE);
        self.emit(Op::Discard, LINE);
        self.patch_jump(skip_abrupt_close);
        self.emit(Op::PopHandler, LINE);
        self.emit(Op::Jump, LINE);
        let close_done_jump = self.current_ip();
        self.emit_byte(0, LINE);
        self.emit_byte(0, LINE);
        self.patch_jump(close_handler_jump);
        self.emit(Op::Discard, LINE);
        self.patch_jump(close_done_jump);
        self.emit(Op::LoadLocal, LINE);
        self.emit_byte(self.slot_to_u8(thrown_slot)?, LINE);
        self.emit(Op::Throw, LINE);
        self.patch_jump(done_end_jump);
        self.patch_jump(end_jump);
        if owns_completion_slot {
          self.emit(Op::LoadLocal, LINE);
          self.emit_byte(self.slot_to_u8(loop_completion_slot)?, LINE);
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

        self.emit_get_builtin_name("forInKeys");
        self.compile_expr_oast(&f.right)?;
        self.emit(Op::Call, LINE);
        self.emit_byte(1, LINE);

        let keys_slot = self.add_local_in_scope(
          self.current_function_scope_index(),
          format!("\0forin_keys_{}", self.locals.len()),
          None,
          false,
        );
        self.emit(Op::StoreLocal, LINE);
        self.emit_byte(self.slot_to_u8(keys_slot)?, LINE);

        self.emit_constant(Value::Number(0.0), LINE);
        let index_slot = self.add_local_in_scope(
          self.current_function_scope_index(),
          format!("\0forin_i_{}", self.locals.len()),
          None,
          false,
        );
        self.emit(Op::StoreLocal, LINE);
        self.emit_byte(self.slot_to_u8(index_slot)?, LINE);

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
            if self.resolve_local(&name).is_none() {
              self.add_local_with_mirror(name.clone(), None, false);
            }
          }
        }

        let loop_start = self.current_ip();

        self.emit(Op::LoadLocal, LINE);
        self.emit_byte(self.slot_to_u8(index_slot)?, LINE);
        self.emit(Op::LoadLocal, LINE);
        self.emit_byte(self.slot_to_u8(keys_slot)?, LINE);
        self.emit_constant(Value::String("length".to_string().into()), LINE);
        self.emit(Op::GetProperty, LINE);
        // Use CompareJump: cond_type 2 = Lt, jump if false.
        self.emit_with_payload(Op::CompareJump, 2, LINE);
        let exit_jump = self.current_ip();
        self.emit_byte(0, LINE);
        self.emit_byte(0, LINE);

        self.emit(Op::LoadLocal, LINE);
        self.emit_byte(self.slot_to_u8(keys_slot)?, LINE);
        self.emit(Op::LoadLocal, LINE);
        self.emit_byte(self.slot_to_u8(index_slot)?, LINE);
        self.emit(Op::GetProperty, LINE);

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
                  self.emit(Op::ResetLocalCell, LINE);
                  self.emit_byte(self.slot_to_u8(slot)?, LINE);
                }
              }
            }
            self.compile_binding_pattern_assignment_oast(&dec.id, v.kind)?;
          }
          OForLeft::AssignmentTargetIdentifier(id) => {
            let name = id.name.as_str();
            if let Some(slot) = self.resolve_local(name) {
              self.emit(Op::StoreLocal, LINE);
              self.emit_byte(self.slot_to_u8(slot)?, LINE);
              if self.local_mirrors_global(slot) {
                self.emit(Op::LoadLocal, LINE);
                self.emit_byte(self.slot_to_u8(slot)?, LINE);
                self.emit_set_global_name(name);
              }
            } else if let Some(upvalue) = self.resolve_upvalue(name) {
              self.emit(Op::SetUpvalue, LINE);
              self.emit_byte(upvalue, LINE);
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

        let update_start = self.current_ip();
        if let Some(patches) = self.continue_patches.pop() {
          for patch_pos in patches {
            self.patch_jump_to_target(patch_pos, update_start);
          }
        }

        self.emit(Op::LoadLocal, LINE);
        self.emit_byte(self.slot_to_u8(index_slot)?, LINE);
        self.emit_constant(Value::Number(1.0), LINE);
        self.emit(Op::Add, LINE);
        self.emit(Op::StoreLocal, LINE);
        self.emit_byte(self.slot_to_u8(index_slot)?, LINE);

        self.emit(Op::Jump, LINE);
        let back_offset = loop_start as isize - (self.current_ip() as isize);
        self.emit_short_signed(back_offset, LINE);

        self.patch_jump(exit_jump);
        let loop_end = self.current_ip();
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
          let loop_end = self.current_ip();
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
            self.emit(Op::Undefined, LINE);
          }
          self.emit(Op::StoreLocal, LINE);
          self.emit_byte(self.slot_to_u8(return_slot)?, LINE);
          self.emit_handler_pops_to(self.finally_patches[finally_index].handler_depth);
          self.emit_with_env_pops();
          self.emit(Op::True, LINE);
          self.emit(Op::StoreLocal, LINE);
          self.emit_byte(self.slot_to_u8(did_return_slot)?, LINE);
          self.emit(Op::Jump, LINE);
          let patch_pos = self.current_ip();
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
          if self.handler_depth == 0 && self.iterator_close_stack.is_empty() && self.with_depth == 0
          {
            self.compile_expr_oast_tail(arg)?;
          } else {
            self.compile_expr_oast(arg)?;
          }
        } else {
          self.emit(Op::Undefined, LINE);
        }
        if !self.iterator_close_stack.is_empty() {
          let return_slot = self.alloc_temp_local("return_value");
          self.emit(Op::StoreLocal, LINE);
          self.emit_byte(self.slot_to_u8(return_slot)?, LINE);
          self.emit_iterator_closes_above(0)?;
          self.emit(Op::LoadLocal, LINE);
          self.emit_byte(self.slot_to_u8(return_slot)?, LINE);
        }
        self.emit_with_env_pops();
        self.emit(Op::Return, LINE);
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
          self.emit(Op::Jump, LINE);
          let patch_pos = self.current_ip();
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
          self.emit(Op::Jump, LINE);
          let patch_pos = self.current_ip();
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
          self.emit(Op::Jump, LINE);
          let patch_pos = self.current_ip();
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
          self.emit(Op::Jump, LINE);
          let patch_pos = self.current_ip();
          self.emit_byte(0, LINE);
          self.emit_byte(0, LINE);
          if let Some(patches) = self.break_patches.last_mut() {
            patches.push(patch_pos);
          }
        }
      }
      OStmt::ThrowStatement(t) => {
        self.compile_expr_oast(&t.argument)?;
        self.emit(Op::Throw, LINE);
      }
      OStmt::TryStatement(t) => {
        let owns_completion_slot = completion_slot.is_none();
        let try_completion_slot = if let Some(slot) = completion_slot {
          slot
        } else {
          let slot = self.alloc_temp_local("try_result");
          self.emit(Op::Undefined, LINE);
          self.emit(Op::StoreLocal, LINE);
          self.emit_byte(self.slot_to_u8(slot)?, LINE);
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
          self.emit(Op::Undefined, LINE);
          self.emit(Op::StoreLocal, LINE);
          self.emit_byte(self.slot_to_u8(return_slot)?, LINE);
          self.emit(Op::False, LINE);
          self.emit(Op::StoreLocal, LINE);
          self.emit_byte(self.slot_to_u8(did_return_slot)?, LINE);
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

        let push_handler_ip = self.current_ip();
        self.emit(Op::PushHandler, LINE);
        let handler_jump = self.current_ip();
        self.emit_byte(0, LINE);
        self.emit_byte(0, LINE);

        self.handler_depth += 1;
        self.enter_block_scope_with_vars(&t.block.body);
        for stmt in &t.block.body {
          self.compile_statement_oast_with_mode(stmt, false, Some(try_completion_slot))?;
        }
        self.exit_scope();
        self.handler_depth = self.handler_depth.saturating_sub(1);

        self.emit(Op::PopHandler, LINE);

        // Normal completion path: run finalizer (if present), then skip handler path.
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
          let body_start = self.current_ip();
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
              push_ip: push_handler_ip,
              finalizer_ip: body_start,
              return_slot,
              did_return_slot,
            });
          self.emit(Op::LoadLocal, LINE);
          self.emit_byte(self.slot_to_u8(did_return_slot)?, LINE);
          self.emit(Op::JumpIfFalse, LINE);
          let skip_return = self.current_ip();
          self.emit_byte(0, LINE);
          self.emit_byte(0, LINE);
          if let Some(outer_idx) = outer_finally_idx {
            let outer_return_slot = self.finally_patches[outer_idx].return_slot;
            let outer_did_return_slot = self.finally_patches[outer_idx].did_return_slot;
            self.emit(Op::LoadLocal, LINE);
            self.emit_byte(self.slot_to_u8(return_slot)?, LINE);
            self.emit(Op::StoreLocal, LINE);
            self.emit_byte(self.slot_to_u8(outer_return_slot)?, LINE);
            self.emit(Op::True, LINE);
            self.emit(Op::StoreLocal, LINE);
            self.emit_byte(self.slot_to_u8(outer_did_return_slot)?, LINE);
            self.emit_handler_pops_to(self.finally_patches[outer_idx].handler_depth);
            self.emit(Op::Jump, LINE);
            let outer_jump = self.current_ip();
            self.emit_byte(0, LINE);
            self.emit_byte(0, LINE);
            self.finally_patches[outer_idx]
              .entry_jumps
              .push(outer_jump);
          } else {
            self.emit(Op::LoadLocal, LINE);
            self.emit_byte(self.slot_to_u8(return_slot)?, LINE);
            if !self.iterator_close_stack.is_empty() {
              let pending_return_slot = self.alloc_temp_local("finally_return_value");
              self.emit(Op::StoreLocal, LINE);
              self.emit_byte(self.slot_to_u8(pending_return_slot)?, LINE);
              self.emit_iterator_closes_above(0)?;
              self.emit(Op::LoadLocal, LINE);
              self.emit_byte(self.slot_to_u8(pending_return_slot)?, LINE);
            }
            self.emit(Op::Return, LINE);
          }
          self.patch_jump(skip_return);
          self.emit_control_dispatch(finally_idx)?;
        }
        self.emit(Op::Jump, LINE);
        let end_jump = self.current_ip();
        self.emit_byte(0, LINE);
        self.emit_byte(0, LINE);

        // Exception path starts here.
        self.patch_jump(handler_jump);

        let mut catch_handler_jump = None;
        match handler {
          Some(handler) => {
            if let Some(finally_idx) = finally_idx {
              self.finally_patches[finally_idx].in_catch = true;
            }
            if finalizer.is_some() {
              self.emit(Op::PushHandler, LINE);
              catch_handler_jump = Some(self.current_ip());
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
              self.enter_block_scope_with_vars(&handler.body.body);
              if !matches!(param.pattern, OPat::BindingIdentifier(_)) {
                let mut binding_names = HashSet::new();
                Self::collect_binding_names(&param.pattern, &mut binding_names);
                let mut binding_names: Vec<_> = binding_names.into_iter().collect();
                binding_names.sort();
                for binding_name in binding_names {
                  let slot = self.add_local_with_mirror(binding_name, None, false);
                  self.mark_local_tdz(slot);
                }
              }
              let slot = self.add_local_with_mirror(name, None, false);
              self.emit(Op::StoreLocal, LINE);
              self.emit_byte(self.slot_to_u8(slot)?, LINE);
              if !matches!(param.pattern, OPat::BindingIdentifier(_)) {
                self.emit(Op::LoadLocal, LINE);
                self.emit_byte(self.slot_to_u8(slot)?, LINE);
                self.compile_binding_pattern_assignment_oast(
                  &param.pattern,
                  oxc_ast::ast::VariableDeclarationKind::Let,
                )?;
              }
              for stmt in &handler.body.body {
                self.compile_statement_oast_with_mode(stmt, false, Some(try_completion_slot))?;
              }
              self.exit_scope();
            } else {
              self.emit(Op::Discard, LINE);
              self.enter_block_scope_with_vars(&handler.body.body);
              for stmt in &handler.body.body {
                self.compile_statement_oast_with_mode(stmt, false, Some(try_completion_slot))?;
              }
              self.exit_scope();
            }

            if finalizer.is_some() {
              self.handler_depth = self.handler_depth.saturating_sub(1);
              self.emit(Op::PopHandler, LINE);
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
              let catch_finalizer_start = self.current_ip();
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
              self.emit(Op::LoadLocal, LINE);
              self.emit_byte(self.slot_to_u8(did_return_slot)?, LINE);
              self.emit(Op::JumpIfFalse, LINE);
              let skip_return = self.current_ip();
              self.emit_byte(0, LINE);
              self.emit_byte(0, LINE);
              let outer_finally_idx = self.finally_patches[..finally_idx]
                .iter()
                .rposition(|state| !state.in_finalizer);
              if let Some(outer_idx) = outer_finally_idx {
                let outer_return_slot = self.finally_patches[outer_idx].return_slot;
                let outer_did_return_slot = self.finally_patches[outer_idx].did_return_slot;
                self.emit(Op::LoadLocal, LINE);
                self.emit_byte(self.slot_to_u8(return_slot)?, LINE);
                self.emit(Op::StoreLocal, LINE);
                self.emit_byte(self.slot_to_u8(outer_return_slot)?, LINE);
                self.emit(Op::True, LINE);
                self.emit(Op::StoreLocal, LINE);
                self.emit_byte(self.slot_to_u8(outer_did_return_slot)?, LINE);
                self.emit_handler_pops_to(self.finally_patches[outer_idx].handler_depth);
                self.emit(Op::Jump, LINE);
                let outer_jump = self.current_ip();
                self.emit_byte(0, LINE);
                self.emit_byte(0, LINE);
                self.finally_patches[outer_idx]
                  .entry_jumps
                  .push(outer_jump);
              } else {
                self.emit(Op::LoadLocal, LINE);
                self.emit_byte(self.slot_to_u8(return_slot)?, LINE);
                self.emit(Op::Return, LINE);
              }
              self.patch_jump(skip_return);
              self.emit_control_dispatch(finally_idx)?;

              if let Some(catch_handler_jump) = catch_handler_jump {
                let thrown_slot =
                  self.add_local_with_mirror("\0catch_finally_thrown".to_string(), None, false);
                self.emit(Op::Jump, LINE);
                let skip_abrupt_finalizer = self.current_ip();
                self.emit_byte(0, LINE);
                self.emit_byte(0, LINE);
                self.patch_jump(catch_handler_jump);
                self.emit(Op::StoreLocal, LINE);
                self.emit_byte(self.slot_to_u8(thrown_slot)?, LINE);
                self.finally_patches[finally_idx].in_finalizer = true;
                self.enter_block_scope_with_vars(&fin.body);
                for stmt in &fin.body {
                  self.compile_statement_oast_with_mode(stmt, false, Some(try_completion_slot))?;
                }
                self.exit_scope();
                self.finally_patches[finally_idx].in_finalizer = false;
                self.emit(Op::LoadLocal, LINE);
                self.emit_byte(self.slot_to_u8(thrown_slot)?, LINE);
                self.emit(Op::Throw, LINE);
                self.patch_jump(skip_abrupt_finalizer);
              }
            }
          }
          None => {
            // try/finally with no catch: preserve thrown value, run finally, rethrow.
            if finalizer.is_none() {
              return Err(crate::errors::CompilerError::from(
                "try without catch/finally not supported",
              ));
            }
            self.enter_block_scope();
            let slot = self.add_local_with_mirror("\0finally_thrown".to_string(), None, false);
            self.emit(Op::StoreLocal, LINE);
            self.emit_byte(self.slot_to_u8(slot)?, LINE);
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
            self.emit(Op::LoadLocal, LINE);
            self.emit_byte(self.slot_to_u8(slot)?, LINE);
            self.emit(Op::Throw, LINE);
            self.exit_scope();
          }
        }

        self.patch_jump(end_jump);
        if finalizer.is_some() {
          self.finally_patches.pop();
        }
        if owns_completion_slot {
          self.emit(Op::LoadLocal, LINE);
          self.emit_byte(self.slot_to_u8(try_completion_slot)?, LINE);
        }
      }
      _ => {}
    }
    Ok(())
  }

  /// Compile a class declaration statement into bytecode.
  ///
  /// Creates a constructor function closure and compiles all class methods,
  /// attaching them to either the constructor or its prototype.
  fn compile_class_declaration_oast(
    &mut self,
    c: &oast::Class<'_>,
  ) -> Result<(), crate::errors::CompilerError> {
    let name = c
      .id
      .as_ref()
      .map(|id| id.name.to_string())
      .ok_or_else(|| crate::errors::CompilerError::from("anonymous class declaration"))?;

    if let Some(err) = self.check_var_lexical_conflict(&name) {
      return Err(crate::errors::CompilerError::from(err));
    }

    let slot = if let Some(slot) = self.resolve_local(&name) {
      slot
    } else {
      let slot = self.add_local_with_mirror(name.clone(), None, false);
      self.mark_local_tdz(slot);
      self.mark_local_immutable(slot);
      self.add_lexical_binding(&name);
      slot
    };
    self.emit_class_from_oast(c, Some(&name))?;
    self.emit(Op::StoreLocal, LINE);
    self.emit_byte(self.slot_to_u8(slot)?, LINE);
    Ok(())
  }

  /// Check if a function has a simple parameter list (no rest, no defaults, all identifiers).
  fn has_simple_parameter_list(func: &oast::Function<'_>) -> bool {
    func.params.rest.is_none()
      && func.params.items.iter().all(|param| {
        matches!(param.pattern, oast::BindingPattern::BindingIdentifier(_))
          && param.initializer.is_none()
      })
  }

  fn expected_argument_count(params: &oast::FormalParameters<'_>) -> usize {
    params
      .items
      .iter()
      .take_while(|parameter| parameter.initializer.is_none())
      .count()
  }

  /// Emit bytecode to initialize non-simple function parameters from hidden
  /// raw argument slots.
  ///
  /// Raw slots are expected to be laid out before any visible parameter
  /// bindings. This keeps later parameters in TDZ while earlier defaults are
  /// evaluated.
  fn emit_parameter_initializers_from_raw_slots(
    &mut self,
    params: &oast::FormalParameters<'_>,
    raw_slots: &[usize],
  ) -> Result<(), crate::errors::CompilerError> {
    let temp_slot = self.alloc_temp_local("param_init_value");

    for (idx, param) in params.items.iter().enumerate() {
      let raw_slot = raw_slots
        .get(idx)
        .copied()
        .ok_or_else(|| crate::errors::CompilerError::from("missing parameter slot"))?;

      self.emit(Op::LoadLocal, LINE);
      self.emit_byte(self.slot_to_u8(raw_slot)?, LINE);

      if let Some(init) = &param.initializer {
        self.emit(Op::StoreLocal, LINE);
        self.emit_byte(self.slot_to_u8(temp_slot)?, LINE);
        self.emit(Op::LoadLocal, LINE);
        self.emit_byte(self.slot_to_u8(temp_slot)?, LINE);
        self.emit(Op::Undefined, LINE);
        // Use CompareJump: cond_type 6 = StrictEq, jump if false.
        self.emit_with_payload(Op::CompareJump, 6, LINE);
        let skip_jump = self.current_ip();
        self.emit_byte(0, LINE);
        self.emit_byte(0, LINE);
        self.compile_expr_oast(init)?;
        self.emit(Op::StoreLocal, LINE);
        self.emit_byte(self.slot_to_u8(temp_slot)?, LINE);
        self.patch_jump(skip_jump);
        self.emit(Op::LoadLocal, LINE);
        self.emit_byte(self.slot_to_u8(temp_slot)?, LINE);
      }

      self.compile_binding_pattern_assignment_oast(
        &param.pattern,
        oxc_ast::ast::VariableDeclarationKind::Var,
      )?;
    }

    if let Some(rest) = &params.rest {
      let raw_slot = raw_slots
        .get(params.items.len())
        .copied()
        .ok_or_else(|| crate::errors::CompilerError::from("missing rest parameter slot"))?;
      self.emit(Op::LoadLocal, LINE);
      self.emit_byte(self.slot_to_u8(raw_slot)?, LINE);
      self.compile_binding_pattern_assignment_oast(
        &rest.rest.argument,
        oxc_ast::ast::VariableDeclarationKind::Var,
      )?;
    }

    Ok(())
  }

  /// Emit bytecode to initialize default parameter values using the current
  /// parameter slots directly.
  fn emit_parameter_initializers_from_current_slots(
    &mut self,
    params: &oast::FormalParameters<'_>,
  ) -> Result<(), crate::errors::CompilerError> {
    for (idx, param) in params.items.iter().enumerate() {
      let Some(init) = &param.initializer else {
        continue;
      };
      self.emit(Op::LoadLocal, LINE);
      self.emit_byte(self.slot_to_u8(idx)?, LINE);
      self.emit(Op::Undefined, LINE);
      // Use CompareJump: cond_type 6 = StrictEq, jump if false.
      self.emit_with_payload(Op::CompareJump, 6, LINE);
      let skip_jump = self.current_ip();
      self.emit_byte(0, LINE);
      self.emit_byte(0, LINE);
      self.compile_expr_oast(init)?;
      self.emit(Op::StoreLocal, LINE);
      self.emit_byte(self.slot_to_u8(idx)?, LINE);
      self.patch_jump(skip_jump);
    }
    Ok(())
  }

  fn compile_class_initializer_source(
    &mut self,
    source: &str,
  ) -> Result<(), crate::errors::CompilerError> {
    let wrapped = crate::js!(format(tsx), "({source})");
    let session = crate::parser::Parser::parse_source_with_type(&wrapped, SourceType::tsx())
      .map_err(|error| crate::errors::CompilerError::from(error.to_string()))?;
    let Some(oast::Statement::ExpressionStatement(statement)) = session.program().body.first()
    else {
      return Err(crate::errors::CompilerError::from(
        "class field initializer did not parse as an expression",
      ));
    };
    self.compile_expr_oast(&statement.expression)
  }

  fn emit_prepared_class_field_key(
    &mut self,
    key: &crate::compiler::core::PreparedClassFieldKey,
  ) {
    match key {
      crate::compiler::core::PreparedClassFieldKey::Named(name) => {
        self.emit_constant(Value::String(name.clone().into()), LINE);
      }
      crate::compiler::core::PreparedClassFieldKey::Private(key) => {
        self.emit_constant(key.clone(), LINE);
      }
      crate::compiler::core::PreparedClassFieldKey::Computed(binding) => {
        self.emit_get_binding(binding);
      }
    }
  }

  fn emit_static_class_field(
    &mut self,
    target_slot: usize,
    field: &crate::compiler::core::PreparedClassField,
    initializer: Option<&oast::Expression<'_>>,
  ) -> Result<(), crate::errors::CompilerError> {
    self.emit_get_builtin_name("objectDefineProperty");
    self.emit(Op::LoadLocal, LINE);
    self.emit_byte(self.slot_to_u8(target_slot)?, LINE);
    self.emit_prepared_class_field_key(&field.key);
    self.emit_constant(Value::String("value".to_string().into()), LINE);
    if let Some(initializer) = initializer {
      let proto = self.compile_static_field_initializer(initializer)?;
      let function_index = self.chunk.add_function(proto);
      self.emit(Op::LoadLocal, LINE);
      self.emit_byte(self.slot_to_u8(target_slot)?, LINE);
      self.emit(Op::MakeClosure, LINE);
      self.emit_byte(self.function_index_to_u8(function_index)?, LINE);
      self.emit_set_method_home_object(target_slot)?;
      self.emit(Op::CallBuiltin, LINE);
      self.emit_byte(0, LINE);
    } else {
      self.emit(Op::Undefined, LINE);
    }
    self.emit_constant(Value::String("writable".to_string().into()), LINE);
    self.emit_constant(Value::Bool(true), LINE);
    self.emit_constant(Value::String("enumerable".to_string().into()), LINE);
    self.emit_constant(Value::Bool(true), LINE);
    self.emit_constant(Value::String("configurable".to_string().into()), LINE);
    self.emit_constant(Value::Bool(true), LINE);
    self.emit(Op::BuildObject, LINE);
    self.emit_byte(4, LINE);
    self.emit(Op::Call, LINE);
    self.emit_byte(3, LINE);
    self.emit(Op::Discard, LINE);
    Ok(())
  }

  pub(super) fn emit_pending_instance_field_initializers(
    &mut self
  ) -> Result<(), crate::errors::CompilerError> {
    let fields = self.pending_instance_fields.clone();
    for field in fields.iter().filter(|field| !field.is_static) {
      self.emit_get_builtin_name("objectDefineProperty");
      self.emit(Op::GetThis, LINE);
      self.emit_prepared_class_field_key(&field.key);
      self.emit_constant(Value::String("value".to_string().into()), LINE);
      if let Some(source) = &field.initializer_source {
        self.compile_class_initializer_source(source)?;
      } else {
        self.emit(Op::Undefined, LINE);
      }
      self.emit_constant(Value::String("writable".to_string().into()), LINE);
      self.emit_constant(Value::Bool(true), LINE);
      self.emit_constant(Value::String("enumerable".to_string().into()), LINE);
      self.emit_constant(Value::Bool(true), LINE);
      self.emit_constant(Value::String("configurable".to_string().into()), LINE);
      self.emit_constant(Value::Bool(true), LINE);
      self.emit(Op::BuildObject, LINE);
      self.emit_byte(4, LINE);
      self.emit(Op::Call, LINE);
      self.emit_byte(3, LINE);
      self.emit(Op::Discard, LINE);
    }
    Ok(())
  }

  /// Lower a class method/getter/setter installation to a property
  /// definition with a real descriptor object.
  pub(super) fn emit_class_member_installation(
    &mut self,
    target_slot: usize,
    md: &oast::MethodDefinition<'_>,
  ) -> Result<(), crate::errors::CompilerError> {
    if matches!(md.kind, oast::MethodDefinitionKind::Constructor) {
      return Ok(());
    }

    let method_name = if md.computed {
      None
    } else {
      match &md.key {
        oast::PropertyKey::StaticIdentifier(ident) => Some(ident.name.to_string()),
        oast::PropertyKey::Identifier(ident) => Some(ident.name.to_string()),
        oast::PropertyKey::StringLiteral(s) => Some(s.value.to_string()),
        oast::PropertyKey::NumericLiteral(n) => Some(super::numeric_property_key(n.value)),
        oast::PropertyKey::PrivateIdentifier(pid) => Some(pid.name.to_string()),
        _ => None,
      }
    };

    let self_name = md.value.id.as_ref().map(|id| id.name.as_str());
    let function_name = match (md.kind, method_name.as_deref()) {
      (oast::MethodDefinitionKind::Get, Some(name)) => format!("get {name}"),
      (oast::MethodDefinitionKind::Set, Some(name)) => format!("set {name}"),
      (_, Some(name)) => name.to_string(),
      _ => "<method>".to_string(),
    };
    let outer_strict = self.strict_context;
    self.strict_context = true;
    let compiled = self.compile_function(&function_name, self_name, &md.value, None, true);
    self.strict_context = outer_strict;
    let (mut proto, _env_len) = compiled?;
    proto.is_strict = true;
    let fidx = self.chunk.add_function(proto);
    let computed_key_slot = if md.computed {
      Some(self.alloc_temp_local("class_computed_method_key"))
    } else {
      None
    };

    self.emit(Op::LoadLocal, LINE);
    self.emit_byte(self.slot_to_u8(target_slot)?, LINE);
    self.emit(Op::Copy, LINE);
    self.emit_get_builtin_name("objectDefineProperty");
    self.emit(Op::Exchange, LINE);

    if let Some(ref key_name) = method_name {
      if matches!(md.key, oast::PropertyKey::PrivateIdentifier(_)) {
        let key = self.ensure_private_name_constant(key_name);
        self.emit_constant(key, LINE);
      } else {
        self.emit_constant(Value::String(key_name.clone().into()), LINE);
      }
    } else {
      let key_expr = md
        .key
        .as_expression()
        .expect("computed class member key must be an expression");
      self.compile_expr_oast(key_expr)?;
      self.emit(Op::Copy, LINE);
      self.emit(Op::StoreLocal, LINE);
      self.emit_byte(
        self.slot_to_u8(computed_key_slot.expect("computed method has key slot"))?,
        LINE,
      );
    }

    let first_descriptor_key = match md.kind {
      oast::MethodDefinitionKind::Method => "value",
      oast::MethodDefinitionKind::Get => "get",
      oast::MethodDefinitionKind::Set => "set",
      oast::MethodDefinitionKind::Constructor => {
        return Err(crate::errors::CompilerError::from(
          "constructor method definition encountered unexpectedly",
        ));
      }
    };
    self.emit_constant(Value::String(first_descriptor_key.to_string().into()), LINE);
    self.emit(Op::MakeClosure, LINE);
    self.emit_byte(self.function_index_to_u8(fidx)?, LINE);
    if let Some(key_slot) = computed_key_slot {
      let closure_slot = self.alloc_temp_local("class_computed_method_closure");
      self.emit(Op::StoreLocal, LINE);
      self.emit_byte(self.slot_to_u8(closure_slot)?, LINE);
      self.emit_get_builtin_name("setFunctionNameIfAnonymous");
      self.emit(Op::LoadLocal, LINE);
      self.emit_byte(self.slot_to_u8(closure_slot)?, LINE);
      self.emit(Op::LoadLocal, LINE);
      self.emit_byte(self.slot_to_u8(key_slot)?, LINE);
      match md.kind {
        oast::MethodDefinitionKind::Get => {
          self.emit_constant(Value::String("get".into()), LINE);
          self.emit(Op::Call, LINE);
          self.emit_byte(3, LINE);
        }
        oast::MethodDefinitionKind::Set => {
          self.emit_constant(Value::String("set".into()), LINE);
          self.emit(Op::Call, LINE);
          self.emit_byte(3, LINE);
        }
        _ => {
          self.emit(Op::Call, LINE);
          self.emit_byte(2, LINE);
        }
      }
    }
    self.emit_set_method_home_object(target_slot)?;

    let prop_count = match md.kind {
      oast::MethodDefinitionKind::Method => {
        self.emit_constant(Value::String("writable".to_string().into()), LINE);
        self.emit_constant(Value::Bool(true), LINE);
        self.emit_constant(Value::String("enumerable".to_string().into()), LINE);
        self.emit_constant(Value::Bool(false), LINE);
        self.emit_constant(Value::String("configurable".to_string().into()), LINE);
        self.emit_constant(Value::Bool(true), LINE);
        4
      }
      oast::MethodDefinitionKind::Get => {
        self.emit_constant(Value::String("enumerable".to_string().into()), LINE);
        self.emit_constant(Value::Bool(false), LINE);
        self.emit_constant(Value::String("configurable".to_string().into()), LINE);
        self.emit_constant(Value::Bool(true), LINE);
        3
      }
      oast::MethodDefinitionKind::Set => {
        self.emit_constant(Value::String("enumerable".to_string().into()), LINE);
        self.emit_constant(Value::Bool(false), LINE);
        self.emit_constant(Value::String("configurable".to_string().into()), LINE);
        self.emit_constant(Value::Bool(true), LINE);
        3
      }
      oast::MethodDefinitionKind::Constructor => unreachable!(),
    };
    self.emit(Op::BuildObject, LINE);
    self.emit_byte(prop_count, LINE);
    self.emit(Op::Call, LINE);
    self.emit_byte(3, LINE);
    self.emit(Op::Discard, LINE);
    self.emit(Op::Discard, LINE);
    Ok(())
  }

  fn compile_static_field_initializer(
    &mut self,
    initializer: &oast::Expression<'_>,
  ) -> Result<FunctionProto, crate::errors::CompilerError> {
    let outer_bindings = self.visible_bindings_snapshot();
    let mut inner = Compiler {
      chunk: Chunk::new(Arc::clone(&self.source_text)),
      risc: super::risc::RiscBuilder::new(),
      source_text: Arc::clone(&self.source_text),
      locals: Vec::new(),
      local_types: Vec::new(),
      local_tdz: Vec::new(),
      local_immutable: Vec::new(),
      local_sloppy_immutable_silent: Vec::new(),
      local_mirror_globals: Vec::new(),
      scopes: vec![crate::compiler::Scope {
        kind: crate::compiler::ScopeKind::Function,
        bindings: HashMap::new(),
        lexical_bindings: HashSet::new(),
        block_var_names: HashSet::new(),
      }],
      globals: HashMap::new(),
      is_root: false,
      strict_context: true,
      loop_depth: 0,
      with_depth: 0,
      handler_depth: 0,
      continue_patches: Vec::new(),
      break_patches: Vec::new(),
      finally_patches: Vec::new(),
      iterator_close_stack: Vec::new(),
      label_patches: Vec::new(),
      current_module: self.current_module.clone(),
      pending_operand_bytes: 0,
      allow_new_target: false,
      allow_super: true,
      outer_bindings,
      upvalue_indices: HashMap::new(),
      captured_upvalues: Vec::new(),
      private_names: self.private_names.clone(),
      pending_instance_fields: Vec::new(),
      emission_error: None,
    };
    inner.chunk.allow_super = true;
    inner.compile_expr_oast(initializer)?;
    inner.emit(Op::Return, LINE);
    let chunk = super::risc::RiscBuilder::finish(
      std::mem::take(&mut inner.chunk),
      std::mem::take(&mut inner.risc),
    )?;
    let slot_names = inner.environment_slot_names();
    Ok(FunctionProto {
      name: "<static field>".to_string(),
      span: initializer.span(),
      arity: 0,
      length: 0,
      chunk: Arc::new(chunk),
      slot_types: inner.local_types,
      slot_names,
      lexical_tdz: inner.local_tdz,
      slot_immutable: inner.local_immutable,
      slot_sloppy_immutable_silent: inner.local_sloppy_immutable_silent,
      upvalues: inner.captured_upvalues,
      has_rest: false,
      has_prototype_property: false,
      captures_this: false,
      arguments_slot: None,
      self_binding_slot: None,
      has_simple_parameter_list: true,
      return_type: None,
      is_strict: true,
      is_constructor: false,
      is_derived_constructor: false,
      is_class_constructor: false,
      is_async: false,
      is_generator: false,
    })
  }

  fn compile_static_block_function(
    &mut self,
    block: &oast::StaticBlock<'_>,
  ) -> Result<FunctionProto, crate::errors::CompilerError> {
    let outer_bindings = self.visible_bindings_snapshot();
    let mut inner = Compiler {
      chunk: Chunk::new(Arc::clone(&self.source_text)),
      risc: super::risc::RiscBuilder::new(),
      source_text: Arc::clone(&self.source_text),
      locals: Vec::new(),
      local_types: Vec::new(),
      local_tdz: Vec::new(),
      local_immutable: Vec::new(),
      local_sloppy_immutable_silent: Vec::new(),
      local_mirror_globals: Vec::new(),
      scopes: vec![crate::compiler::Scope {
        kind: crate::compiler::ScopeKind::Function,
        bindings: HashMap::new(),
        lexical_bindings: HashSet::new(),
        block_var_names: HashSet::new(),
      }],
      globals: HashMap::new(),
      is_root: false,
      strict_context: true,
      loop_depth: 0,
      with_depth: 0,
      handler_depth: 0,
      continue_patches: Vec::new(),
      break_patches: Vec::new(),
      finally_patches: Vec::new(),
      iterator_close_stack: Vec::new(),
      label_patches: Vec::new(),
      current_module: self.current_module.clone(),
      pending_operand_bytes: 0,
      allow_new_target: false,
      allow_super: true,
      outer_bindings,
      upvalue_indices: HashMap::new(),
      captured_upvalues: Vec::new(),
      private_names: self.private_names.clone(),
      pending_instance_fields: Vec::new(),
      emission_error: None,
    };
    inner.chunk.allow_super = true;
    inner.pre_declare_variables_oast_stmts(&block.body);
    inner.pre_declare_functions_oast_stmts(&block.body);
    inner.emit_hoisted_function_decls_oast_stmts(&block.body)?;
    for statement in &block.body {
      if !matches!(statement, oast::Statement::FunctionDeclaration(_)) {
        inner.compile_statement_oast(statement)?;
      }
    }
    inner.emit(Op::Undefined, LINE);
    inner.emit(Op::Return, LINE);
    let chunk = super::risc::RiscBuilder::finish(
      std::mem::take(&mut inner.chunk),
      std::mem::take(&mut inner.risc),
    )?;
    let slot_names = inner.environment_slot_names();
    Ok(FunctionProto {
      name: "<static block>".to_string(),
      span: block.span,
      arity: 0,
      length: 0,
      chunk: Arc::new(chunk),
      slot_types: inner.local_types,
      slot_names,
      lexical_tdz: inner.local_tdz,
      slot_immutable: inner.local_immutable,
      slot_sloppy_immutable_silent: inner.local_sloppy_immutable_silent,
      upvalues: inner.captured_upvalues,
      has_rest: false,
      has_prototype_property: false,
      captures_this: false,
      arguments_slot: None,
      self_binding_slot: None,
      has_simple_parameter_list: true,
      return_type: None,
      is_strict: true,
      is_constructor: false,
      is_derived_constructor: false,
      is_class_constructor: false,
      is_async: false,
      is_generator: false,
    })
  }

  /// Emits a class declaration from an OAST class node.
  #[allow(clippy::too_many_lines)]
  pub(super) fn emit_class_from_oast(
    &mut self,
    c: &oast::Class<'_>,
    declaration_name: Option<&str>,
  ) -> Result<(), crate::errors::CompilerError> {
    let class_name = c
      .id
      .as_ref()
      .map(|id| id.name.to_string())
      .or_else(|| declaration_name.map(ToString::to_string))
      .unwrap_or_else(|| "<anonymous>".to_string());

    // Each class declaration creates fresh private-name identities while retaining
    // access to private names from enclosing classes.
    let enclosing_private_names = self.private_names.clone();
    let mut declared_private_names = HashSet::new();
    for element in &c.body.body {
      let key = match element {
        oast::ClassElement::MethodDefinition(method) => Some(&method.key),
        oast::ClassElement::PropertyDefinition(property) => Some(&property.key),
        oast::ClassElement::AccessorProperty(property) => Some(&property.key),
        _ => None,
      };
      if let Some(oast::PropertyKey::PrivateIdentifier(identifier)) = key {
        declared_private_names.insert(identifier.name.to_string());
      }
    }
    for name in declared_private_names {
      self.private_names.insert(
        name,
        Value::Symbol(crate::value::SymbolId::new_private(), None),
      );
    }

    let mut prepared_fields = Vec::new();
    for (index, element) in c.body.body.iter().enumerate() {
      let oast::ClassElement::PropertyDefinition(field) = element else {
        continue;
      };
      let key = if field.computed {
        let slot = self.alloc_temp_local(&format!("class_field_key_{index}"));
        crate::compiler::core::PreparedClassFieldKey::Computed(self.locals[slot].clone())
      } else {
        match &field.key {
          oast::PropertyKey::StaticIdentifier(identifier) => {
            crate::compiler::core::PreparedClassFieldKey::Named(identifier.name.to_string())
          }
          oast::PropertyKey::Identifier(identifier) => {
            crate::compiler::core::PreparedClassFieldKey::Named(identifier.name.to_string())
          }
          oast::PropertyKey::StringLiteral(literal) => {
            crate::compiler::core::PreparedClassFieldKey::Named(literal.value.to_string())
          }
          oast::PropertyKey::NumericLiteral(literal) => {
            crate::compiler::core::PreparedClassFieldKey::Named(super::numeric_property_key(
              literal.value,
            ))
          }
          oast::PropertyKey::PrivateIdentifier(identifier) => {
            crate::compiler::core::PreparedClassFieldKey::Private(
              self.ensure_private_name_constant(&identifier.name),
            )
          }
          _ => {
            return Err(crate::errors::CompilerError::from(
              "unsupported non-computed class field key",
            ));
          }
        }
      };
      let initializer_source = field.value.as_ref().map(|value| {
        let span = value.span();
        self.source_text[span.start as usize..span.end as usize].to_string()
      });
      prepared_fields.push(crate::compiler::core::PreparedClassField {
        key,
        initializer_source,
        is_static: field.r#static,
      });
    }

    let constructor_method = c.body.body.iter().find_map(|element| {
      if let oast::ClassElement::MethodDefinition(md) = element
        && matches!(md.kind, oast::MethodDefinitionKind::Constructor)
      {
        return Some(md);
      }
      None
    });
    let class_has_super = c.super_class.is_some();
    let proto = if let Some(md) = constructor_method {
      let outer_strict = self.strict_context;
      self.strict_context = true;
      let compiled = self.compile_function_with_instance_fields(
        &class_name,
        None,
        &md.value,
        None,
        class_has_super,
        &prepared_fields,
      );
      self.strict_context = outer_strict;
      let (mut proto, _env_len) = compiled?;
      proto.is_strict = true;
      proto.is_derived_constructor = class_has_super;
      proto.is_class_constructor = true;
      proto
    } else {
      let mut ctor_chunk = Chunk::new(Arc::clone(&self.source_text));
      if class_has_super {
        ctor_chunk.allow_new_target = true;
        ctor_chunk.allow_super = true;
        let mut inner = Compiler {
          chunk: ctor_chunk,
          risc: super::risc::RiscBuilder::new(),
          source_text: Arc::clone(&self.source_text),
          locals: Vec::new(),
          local_types: Vec::new(),
          local_tdz: Vec::new(),
          local_immutable: Vec::new(),
          local_sloppy_immutable_silent: Vec::new(),
          local_mirror_globals: Vec::new(),
          scopes: vec![crate::compiler::Scope {
            kind: crate::compiler::ScopeKind::Function,
            bindings: HashMap::new(),
            lexical_bindings: HashSet::new(),
            block_var_names: HashSet::new(),
          }],
          globals: HashMap::new(),
          is_root: false,
          strict_context: true,
          loop_depth: 0,
          with_depth: 0,
          handler_depth: 0,
          continue_patches: Vec::new(),
          break_patches: Vec::new(),
          finally_patches: Vec::new(),
          iterator_close_stack: Vec::new(),
          label_patches: Vec::new(),
          current_module: self.current_module.clone(),
          pending_operand_bytes: 0,
          allow_new_target: true,
          allow_super: true,
          outer_bindings: self.visible_bindings_snapshot(),
          upvalue_indices: HashMap::new(),
          captured_upvalues: Vec::new(),
          private_names: self.private_names.clone(),
          pending_instance_fields: prepared_fields.clone(),
          emission_error: None,
        };
        inner.add_local_with_mirror("arguments".to_string(), None, false);
        inner.emit(Op::GetGlobal, LINE);
        let global_idx = inner.global_name_index("Array");
        inner.emit_u16(inner.global_index_to_u16(global_idx)?, LINE);
        inner.emit_constant(Value::String("from".to_string().into()), LINE);
        inner.emit(Op::GetProperty, LINE);
        inner.emit(Op::LoadLocal, LINE);
        inner.emit_byte(0, LINE);
        inner.emit(Op::SuperConstruct, LINE);
        inner.emit_byte(1, LINE);
        inner.emit_pending_instance_field_initializers()?;
        inner.emit(Op::Discard, LINE);
        inner.emit(Op::GetThis, LINE);
        inner.emit(Op::Return, LINE);
        ctor_chunk = super::risc::RiscBuilder::finish(
          std::mem::take(&mut inner.chunk),
          std::mem::take(&mut inner.risc),
        )?;
        FunctionProto {
          name: class_name.clone(),
          span: c.span,
          arity: 0,
          length: 0,
          chunk: Arc::new(ctor_chunk),
          slot_types: inner.local_types,
          slot_names: inner.locals.into_iter().map(Some).collect(),
          lexical_tdz: inner.local_tdz,
          slot_immutable: inner.local_immutable,
          slot_sloppy_immutable_silent: inner.local_sloppy_immutable_silent,
          upvalues: inner.captured_upvalues,
          has_rest: false,
          has_prototype_property: true,
          captures_this: false,
          arguments_slot: Some(0),
          self_binding_slot: None,
          has_simple_parameter_list: true,
          return_type: None,
          is_strict: true,
          is_constructor: true,
          is_derived_constructor: true,
          is_class_constructor: true,
          is_async: false,
          is_generator: false,
        }
      } else {
        let mut inner = Compiler::new(Arc::clone(&self.source_text));
        inner.is_root = false;
        inner.strict_context = true;
        inner.current_module = self.current_module.clone();
        inner.outer_bindings = self.visible_bindings_snapshot();
        inner.private_names = self.private_names.clone();
        inner.pending_instance_fields = prepared_fields.clone();
        inner.emit_pending_instance_field_initializers()?;
        inner.emit(Op::GetThis, LINE);
        inner.emit(Op::Return, LINE);
        let ctor_chunk = super::risc::RiscBuilder::finish(
          std::mem::take(&mut inner.chunk),
          std::mem::take(&mut inner.risc),
        )?;
        FunctionProto {
          name: class_name.clone(),
          span: c.span,
          arity: 0,
          length: 0,
          chunk: Arc::new(ctor_chunk),
          slot_types: inner.local_types,
          slot_names: inner.locals.into_iter().map(Some).collect(),
          lexical_tdz: inner.local_tdz,
          slot_immutable: inner.local_immutable,
          slot_sloppy_immutable_silent: inner.local_sloppy_immutable_silent,
          upvalues: inner.captured_upvalues,
          has_rest: false,
          has_prototype_property: true,
          captures_this: false,
          arguments_slot: None,
          self_binding_slot: None,
          has_simple_parameter_list: true,
          return_type: None,
          is_strict: true,
          is_constructor: true,
          is_derived_constructor: false,
          is_class_constructor: true,
          is_async: false,
          is_generator: false,
        }
      }
    };

    let idx = self.chunk.add_function(proto);
    self.emit(Op::MakeClosure, LINE);
    self.emit_byte(self.function_index_to_u8(idx)?, LINE);
    if let Some(super_class) = &c.super_class {
      self.emit_get_builtin_name("classInitExtends");
      self.emit(Op::Exchange, LINE);
      self.compile_expr_oast(super_class)?;
      self.emit(Op::Call, LINE);
      self.emit_byte(2, LINE);
    }

    let ctor_slot = self.alloc_temp_local("class_ctor_tmp");
    self.emit(Op::Copy, LINE);
    self.emit(Op::StoreLocal, LINE);
    self.emit_byte(self.slot_to_u8(ctor_slot)?, LINE);

    let instance_home_slot = if c
      .body
      .body
      .iter()
      .any(|element| matches!(element, oast::ClassElement::MethodDefinition(md) if !md.r#static))
    {
      self.emit(Op::LoadLocal, LINE);
      self.emit_byte(self.slot_to_u8(ctor_slot)?, LINE);
      self.emit_constant(Value::String("prototype".to_string().into()), LINE);
      self.emit(Op::GetProperty, LINE);
      let home_slot = self.alloc_temp_local("class_home");
      self.emit(Op::StoreLocal, LINE);
      self.emit_byte(self.slot_to_u8(home_slot)?, LINE);
      Some(home_slot)
    } else {
      None
    };

    let mut prepared_field_iter = prepared_fields.iter();
    for element in &c.body.body {
      match element {
        oast::ClassElement::MethodDefinition(method) => {
          let target_slot = if method.r#static {
            ctor_slot
          } else {
            instance_home_slot.expect("instance home slot")
          };
          self.emit_class_member_installation(target_slot, method)?;
        }
        oast::ClassElement::PropertyDefinition(property) => {
          let field = prepared_field_iter
            .next()
            .expect("every class property has a prepared field");
          if property.computed {
            let crate::compiler::core::PreparedClassFieldKey::Computed(binding) = &field.key else {
              unreachable!("computed class fields have prepared key bindings")
            };
            let key = property
              .key
              .as_expression()
              .expect("computed class field key must be an expression");
            self.compile_expr_oast(key)?;
            let slot = self
              .resolve_local(binding)
              .expect("computed class field key binding must be local");
            self.emit(Op::StoreLocal, LINE);
            self.emit_byte(self.slot_to_u8(slot)?, LINE);
          }
          if field.is_static {
            self.emit_static_class_field(ctor_slot, field, property.value.as_ref())?;
          }
        }
        oast::ClassElement::StaticBlock(block) => {
          let proto = self.compile_static_block_function(block)?;
          let function_index = self.chunk.add_function(proto);
          self.emit(Op::LoadLocal, LINE);
          self.emit_byte(self.slot_to_u8(ctor_slot)?, LINE);
          self.emit(Op::MakeClosure, LINE);
          self.emit_byte(self.function_index_to_u8(function_index)?, LINE);
          self.emit_set_method_home_object(ctor_slot)?;
          self.emit(Op::CallBuiltin, LINE);
          self.emit_byte(0, LINE);
          self.emit(Op::Discard, LINE);
        }
        _ => {}
      }
    }

    self.emit(Op::LoadLocal, LINE);
    self.emit_byte(self.slot_to_u8(ctor_slot)?, LINE);
    self.private_names = enclosing_private_names;
    Ok(())
  }

  /// Compile a function declaration into a FunctionProto with its own scope.
  ///
  /// Creates an inner compiler scope, compiles the function body, and returns
  /// the compiled prototype with all metadata about parameters, locals, and closures.
  pub(super) fn compile_function(
    &mut self,
    name: &str,
    self_name: Option<&str>,
    func: &oast::Function,
    return_type: Option<Type>,
    allow_super: bool,
  ) -> Result<(FunctionProto, usize), crate::errors::CompilerError> {
    self.compile_function_with_instance_fields(name, self_name, func, return_type, allow_super, &[])
  }

  fn compile_function_with_instance_fields(
    &mut self,
    name: &str,
    self_name: Option<&str>,
    func: &oast::Function,
    return_type: Option<Type>,
    allow_super: bool,
    instance_fields: &[crate::compiler::core::PreparedClassField],
  ) -> Result<(FunctionProto, usize), crate::errors::CompilerError> {
    let _env_len = self.locals.len();
    let function_is_strict = self.strict_context
      || func
        .body
        .as_ref()
        .is_some_and(|body| Self::has_use_strict_directive(body));
    let body_has_use_strict = func
      .body
      .as_ref()
      .is_some_and(|body| Self::has_use_strict_directive(body));

    // `name` is function metadata and may be inferred from a property's
    // IdentifierName, which can legally be a reserved word. Validate only
    // actual function bindings from the AST.
    if let Some(binding) = &func.id
      && let Some(err) = Self::strict_binding_name_error(function_is_strict, &binding.name)
    {
      return Err(err);
    }
    if let Some(self_name) = self_name
      && let Some(err) = Self::strict_binding_name_error(function_is_strict, self_name)
    {
      return Err(err);
    }
    if function_is_strict {
      let mut seen = HashSet::new();
      for p in &func.params.items {
        if let oast::BindingPattern::BindingIdentifier(id) = &p.pattern
          && !seen.insert(id.name.to_string())
        {
          return Err(crate::errors::CompilerError::from(
            "SyntaxError: duplicate parameter name in strict mode",
          ));
        }
      }
      if let Some(rest) = &func.params.rest
        && let oast::BindingPattern::BindingIdentifier(id) = &rest.rest.argument
        && !seen.insert(id.name.to_string())
      {
        return Err(crate::errors::CompilerError::from(
          "SyntaxError: duplicate parameter name in strict mode",
        ));
      }
    }
    for p in &func.params.items {
      if let Some(err) = Self::strict_binding_pattern_name_error(function_is_strict, &p.pattern) {
        return Err(err);
      }
    }
    if let Some(rest) = &func.params.rest
      && let Some(err) =
        Self::strict_binding_pattern_name_error(function_is_strict, &rest.rest.argument)
    {
      return Err(err);
    }

    let has_simple_parameter_list = Self::has_simple_parameter_list(func);
    if !has_simple_parameter_list {
      if body_has_use_strict {
        return Err(crate::errors::CompilerError::from(
          "SyntaxError: non-simple parameter list in strict mode",
        ));
      }
      let mut seen = HashSet::new();
      for p in &func.params.items {
        let mut names = HashSet::new();
        Self::collect_binding_names(&p.pattern, &mut names);
        let mut names: Vec<_> = names.into_iter().collect();
        names.sort();
        for name in names {
          if !seen.insert(name) {
            return Err(crate::errors::CompilerError::from(
              "SyntaxError: duplicate parameter name in non-simple parameter list",
            ));
          }
        }
      }
      if let Some(rest) = &func.params.rest {
        let mut names = HashSet::new();
        Self::collect_binding_names(&rest.rest.argument, &mut names);
        let mut names: Vec<_> = names.into_iter().collect();
        names.sort();
        for name in names {
          if !seen.insert(name) {
            return Err(crate::errors::CompilerError::from(
              "SyntaxError: duplicate parameter name in non-simple parameter list",
            ));
          }
        }
      }
    }

    let mut inner = Compiler {
      chunk: Chunk::new(Arc::clone(&self.source_text)),
      risc: super::risc::RiscBuilder::new(),
      source_text: Arc::clone(&self.source_text),
      locals: Vec::new(),
      local_types: Vec::new(),
      local_tdz: Vec::new(),
      local_immutable: Vec::new(),
      local_sloppy_immutable_silent: Vec::new(),
      local_mirror_globals: Vec::new(),
      scopes: vec![crate::compiler::Scope {
        kind: crate::compiler::ScopeKind::Function,
        bindings: HashMap::new(),
        lexical_bindings: HashSet::new(),
        block_var_names: HashSet::new(),
      }],
      globals: HashMap::new(),
      is_root: false,
      strict_context: self.strict_context,
      loop_depth: 0,
      with_depth: 0,
      handler_depth: 0,
      continue_patches: Vec::new(),
      break_patches: Vec::new(),
      finally_patches: Vec::new(),
      iterator_close_stack: Vec::new(),
      label_patches: Vec::new(),
      current_module: self.current_module.clone(),
      pending_operand_bytes: 0,
      allow_new_target: true,
      allow_super,
      outer_bindings: self.visible_bindings_snapshot(),
      upvalue_indices: HashMap::new(),
      captured_upvalues: Vec::new(),
      private_names: self.private_names.clone(),
      pending_instance_fields: instance_fields.to_vec(),
      emission_error: None,
    };
    inner.strict_context = function_is_strict;
    inner.chunk.allow_new_target = inner.allow_new_target;
    inner.chunk.allow_super = inner.allow_super;

    let mut raw_param_slots = Vec::new();
    if has_simple_parameter_list {
      // Keep one positional slot per argument, but bind duplicate sloppy
      // parameter names to their last occurrence.
      let mut last_parameter_index = HashMap::new();
      for (index, parameter) in func.params.items.iter().enumerate() {
        if let oast::BindingPattern::BindingIdentifier(identifier) = &parameter.pattern {
          last_parameter_index.insert(identifier.name.to_string(), index);
        }
      }
      for (index, parameter) in func.params.items.iter().enumerate() {
        let pname = match &parameter.pattern {
          oast::BindingPattern::BindingIdentifier(identifier)
            if last_parameter_index.get(identifier.name.as_str()) == Some(&index) =>
          {
            identifier.name.to_string()
          }
          _ => format!("\0duplicate_param_{index}"),
        };
        inner.add_local_with_mirror(pname, None, false);
      }
    } else {
      for _ in &func.params.items {
        raw_param_slots.push(inner.add_local_with_mirror(
          format!("\0param_raw_{}", raw_param_slots.len()),
          None,
          false,
        ));
      }
      if func.params.rest.is_some() {
        raw_param_slots.push(inner.add_local_with_mirror(
          "\0param_raw_rest".to_string(),
          None,
          false,
        ));
      }
      let mut parameter_names = HashSet::new();
      for parameter in &func.params.items {
        Self::collect_binding_names(&parameter.pattern, &mut parameter_names);
      }
      if let Some(rest) = &func.params.rest {
        Self::collect_binding_names(&rest.rest.argument, &mut parameter_names);
      }
      let mut parameter_names: Vec<_> = parameter_names.into_iter().collect();
      parameter_names.sort();
      for parameter_name in parameter_names {
        let slot = inner.add_local_with_mirror(parameter_name, None, false);
        inner.mark_local_tdz(slot);
      }
    }
    let self_binding_slot =
      self_name.map(|self_name| inner.add_local_with_mirror(self_name.to_string(), None, false));
    if let Some(slot) = self_binding_slot
      && let Some(flag) = inner.local_immutable.get_mut(slot)
    {
      *flag = true;
    }
    if let Some(slot) = self_binding_slot
      && let Some(flag) = inner.local_sloppy_immutable_silent.get_mut(slot)
    {
      *flag = true;
    }
    let arguments_slot = inner.add_local_with_mirror("arguments".to_string(), None, false);

    // Non-simple parameter initializers run in a parameter environment where
    // declarations from the function body are not yet visible.
    if has_simple_parameter_list {
      inner.emit_parameter_initializers_from_current_slots(&func.params)?;
    } else {
      inner.emit_parameter_initializers_from_raw_slots(&func.params, &raw_param_slots)?;
    }

    // Instantiate body declarations only after parameter initialization.
    if let Some(body) = &func.body {
      inner.pre_declare_variables_oast_stmts(&body.statements);
      inner.pre_declare_functions_oast_stmts(&body.statements);
      inner.emit_hoisted_function_decls_oast_stmts(&body.statements)?;
    }
    if func.generator {
      inner.emit(Op::GeneratorInitDone, LINE);
    }
    if !allow_super {
      inner.emit_pending_instance_field_initializers()?;
    }

    // Compile function body statements into inner compiler.
    if let Some(body) = &func.body {
      for stmt in &body.statements {
        if matches!(stmt, oast::Statement::FunctionDeclaration(_)) {
          continue;
        }
        inner.compile_statement_oast(stmt)?;
      }
    }

    inner.emit(Op::Undefined, LINE);
    inner.emit(Op::Return, LINE);
    let mut chunk = super::risc::RiscBuilder::finish(
      std::mem::take(&mut inner.chunk),
      std::mem::take(&mut inner.risc),
    )?;
    chunk.allow_new_target = inner.allow_new_target;
    chunk.allow_super = inner.allow_super;
    let has_rest = func.params.rest.is_some();
    let has_simple_parameter_list = Self::has_simple_parameter_list(func);
    let slot_names = inner.environment_slot_names();
    let slot_types = inner.local_types;
    let lexical_tdz = inner.local_tdz;
    let has_prototype_property = !func.r#async;
    let is_constructor = !func.r#async && !func.generator;
    Ok((
      FunctionProto {
        name: name.to_string(),
        span: func.span,
        arity: func.params.items.len() + usize::from(has_rest),
        length: Self::expected_argument_count(&func.params),
        chunk: Arc::new(chunk),
        slot_types,
        slot_names,
        lexical_tdz,
        slot_immutable: inner.local_immutable,
        slot_sloppy_immutable_silent: inner.local_sloppy_immutable_silent,
        upvalues: inner.captured_upvalues,
        has_rest,
        has_prototype_property,
        captures_this: false,
        arguments_slot: Some(arguments_slot),
        self_binding_slot,
        has_simple_parameter_list,
        return_type,
        is_strict: function_is_strict,
        is_constructor,
        is_derived_constructor: false,
        is_class_constructor: false,
        is_async: func.r#async,
        is_generator: func.generator,
      },
      _env_len,
    ))
  }

  /// Compile an arrow function expression into a FunctionProto with its own scope.
  ///
  /// Similar to `compile_function` but with key differences:
  /// - No `arguments` binding
  /// - Captures `this` from enclosing scope
  /// - Not usable as a constructor
  /// - Supports implicit return for expression bodies
  pub(super) fn compile_arrow_function(
    &mut self,
    name: &str,
    func: &oast::ArrowFunctionExpression,
    return_type: Option<Type>,
  ) -> Result<(FunctionProto, usize), crate::errors::CompilerError> {
    let _env_len = self.locals.len();
    let function_is_strict = self.strict_context || Self::has_use_strict_directive(&func.body);
    let body_has_use_strict = Self::has_use_strict_directive(&func.body);
    let has_simple_parameter_list = func.params.rest.is_none()
      && func.params.items.iter().all(|param| {
        matches!(param.pattern, oast::BindingPattern::BindingIdentifier(_))
          && param.initializer.is_none()
      });

    // Arrow names are metadata inferred from their syntactic position, not
    // lexical bindings, so reserved IdentifierNames are valid here.
    if function_is_strict {
      let mut seen = HashSet::new();
      for p in &func.params.items {
        if let oast::BindingPattern::BindingIdentifier(id) = &p.pattern
          && !seen.insert(id.name.to_string())
        {
          return Err(crate::errors::CompilerError::from(
            "SyntaxError: duplicate parameter name in strict mode",
          ));
        }
      }
      if let Some(rest) = &func.params.rest
        && let oast::BindingPattern::BindingIdentifier(id) = &rest.rest.argument
        && !seen.insert(id.name.to_string())
      {
        return Err(crate::errors::CompilerError::from(
          "SyntaxError: duplicate parameter name in strict mode",
        ));
      }
    }
    for p in &func.params.items {
      if let Some(err) = Self::strict_binding_pattern_name_error(function_is_strict, &p.pattern) {
        return Err(err);
      }
    }
    if let Some(rest) = &func.params.rest
      && let Some(err) =
        Self::strict_binding_pattern_name_error(function_is_strict, &rest.rest.argument)
    {
      return Err(err);
    }

    if !has_simple_parameter_list {
      if body_has_use_strict {
        return Err(crate::errors::CompilerError::from(
          "SyntaxError: non-simple parameter list in strict mode",
        ));
      }
      let mut seen = HashSet::new();
      for p in &func.params.items {
        let mut names = HashSet::new();
        Self::collect_binding_names(&p.pattern, &mut names);
        let mut names: Vec<_> = names.into_iter().collect();
        names.sort();
        for name in names {
          if !seen.insert(name) {
            return Err(crate::errors::CompilerError::from(
              "SyntaxError: duplicate parameter name in non-simple parameter list",
            ));
          }
        }
      }
      if let Some(rest) = &func.params.rest {
        let mut names = HashSet::new();
        Self::collect_binding_names(&rest.rest.argument, &mut names);
        let mut names: Vec<_> = names.into_iter().collect();
        names.sort();
        for name in names {
          if !seen.insert(name) {
            return Err(crate::errors::CompilerError::from(
              "SyntaxError: duplicate parameter name in non-simple parameter list",
            ));
          }
        }
      }
    }

    let mut inner = Compiler {
      chunk: Chunk::new(Arc::clone(&self.source_text)),
      risc: super::risc::RiscBuilder::new(),
      source_text: Arc::clone(&self.source_text),
      locals: Vec::new(),
      local_types: Vec::new(),
      local_tdz: Vec::new(),
      local_immutable: Vec::new(),
      local_sloppy_immutable_silent: Vec::new(),
      local_mirror_globals: Vec::new(),
      scopes: vec![crate::compiler::Scope {
        kind: crate::compiler::ScopeKind::Function,
        bindings: HashMap::new(),
        lexical_bindings: HashSet::new(),
        block_var_names: HashSet::new(),
      }],
      globals: HashMap::new(),
      is_root: false,
      strict_context: self.strict_context,
      loop_depth: 0,
      with_depth: 0,
      handler_depth: 0,
      continue_patches: Vec::new(),
      break_patches: Vec::new(),
      finally_patches: Vec::new(),
      iterator_close_stack: Vec::new(),
      label_patches: Vec::new(),
      current_module: self.current_module.clone(),
      pending_operand_bytes: 0,
      allow_new_target: self.allow_new_target,
      allow_super: self.allow_super,
      outer_bindings: self.visible_bindings_snapshot(),
      upvalue_indices: HashMap::new(),
      captured_upvalues: Vec::new(),
      private_names: self.private_names.clone(),
      pending_instance_fields: Vec::new(),
      emission_error: None,
    };
    inner.strict_context = function_is_strict;
    inner.chunk.allow_new_target = inner.allow_new_target;
    inner.chunk.allow_super = inner.allow_super;

    let mut raw_param_slots = Vec::new();
    if has_simple_parameter_list {
      let mut seen = HashSet::new();
      for parameter in &func.params.items {
        if let oast::BindingPattern::BindingIdentifier(identifier) = &parameter.pattern
          && !seen.insert(identifier.name.to_string())
        {
          return Err(crate::errors::CompilerError::from(
            "SyntaxError: duplicate parameter name in arrow function",
          ));
        }
        let pname = match &parameter.pattern {
          oast::BindingPattern::BindingIdentifier(identifier) => identifier.name.to_string(),
          _ => "_".to_string(),
        };
        inner.add_local_with_mirror(pname, None, false);
      }
    } else {
      for _ in &func.params.items {
        raw_param_slots.push(inner.add_local_with_mirror(
          format!("\0param_raw_{}", raw_param_slots.len()),
          None,
          false,
        ));
      }
      if func.params.rest.is_some() {
        raw_param_slots.push(inner.add_local_with_mirror(
          "\0param_raw_rest".to_string(),
          None,
          false,
        ));
      }
      let mut parameter_names = HashSet::new();
      for parameter in &func.params.items {
        Self::collect_binding_names(&parameter.pattern, &mut parameter_names);
      }
      if let Some(rest) = &func.params.rest {
        Self::collect_binding_names(&rest.rest.argument, &mut parameter_names);
      }
      let mut parameter_names: Vec<_> = parameter_names.into_iter().collect();
      parameter_names.sort();
      for parameter_name in parameter_names {
        let slot = inner.add_local_with_mirror(parameter_name, None, false);
        inner.mark_local_tdz(slot);
      }
    }
    if !has_simple_parameter_list {
      inner.emit_parameter_initializers_from_raw_slots(&func.params, &raw_param_slots)?;
    }

    // Arrow body declarations are likewise outside the non-simple parameter
    // environment.
    inner.pre_declare_variables_oast_stmts(&func.body.statements);
    inner.pre_declare_functions_oast_stmts(&func.body.statements);
    inner.emit_hoisted_function_decls_oast_stmts(&func.body.statements)?;

    if has_simple_parameter_list {
      // Check if this is an implicit-return arrow function (expression: true).
      // In that case, the body contains a single ExpressionStatement that should be returned.
      if func.expression && func.body.statements.len() == 1 {
        if let oast::Statement::ExpressionStatement(expr_stmt) = &func.body.statements[0] {
          // Compile the expression in return position.
          inner.compile_expr_oast_tail(&expr_stmt.expression)?;
          inner.emit(Op::Return, LINE);
        } else {
          // Shouldn't happen for expression: true, but handle it safely
          for stmt in &func.body.statements {
            if !matches!(stmt, oast::Statement::FunctionDeclaration(_)) {
              inner.compile_statement_oast(stmt)?;
            }
          }
          inner.emit(Op::Undefined, LINE);
          inner.emit(Op::Return, LINE);
        }
      } else {
        // Regular function body with explicit statements
        for stmt in &func.body.statements {
          if matches!(stmt, oast::Statement::FunctionDeclaration(_)) {
            continue;
          }
          inner.compile_statement_oast(stmt)?;
        }
        inner.emit(Op::Undefined, LINE);
        inner.emit(Op::Return, LINE);
      }
    } else if func.expression && func.body.statements.len() == 1 {
      if let oast::Statement::ExpressionStatement(expr_stmt) = &func.body.statements[0] {
        inner.compile_expr_oast_tail(&expr_stmt.expression)?;
        inner.emit(Op::Return, LINE);
      } else {
        for stmt in &func.body.statements {
          if !matches!(stmt, oast::Statement::FunctionDeclaration(_)) {
            inner.compile_statement_oast(stmt)?;
          }
        }
        inner.emit(Op::Undefined, LINE);
        inner.emit(Op::Return, LINE);
      }
    } else {
      for stmt in &func.body.statements {
        if matches!(stmt, oast::Statement::FunctionDeclaration(_)) {
          continue;
        }
        inner.compile_statement_oast(stmt)?;
      }
      inner.emit(Op::Undefined, LINE);
      inner.emit(Op::Return, LINE);
    }

    let mut chunk = super::risc::RiscBuilder::finish(
      std::mem::take(&mut inner.chunk),
      std::mem::take(&mut inner.risc),
    )?;
    chunk.allow_new_target = inner.allow_new_target;
    chunk.allow_super = inner.allow_super;
    let has_rest = func.params.rest.is_some();
    let slot_names = inner.environment_slot_names();
    let slot_types = inner.local_types;
    let lexical_tdz = inner.local_tdz;
    Ok((
      FunctionProto {
        name: name.to_string(),
        span: func.span,
        arity: func.params.items.len() + usize::from(has_rest),
        length: Self::expected_argument_count(&func.params),
        chunk: Arc::new(chunk),
        slot_types,
        slot_names,
        lexical_tdz,
        slot_immutable: inner.local_immutable,
        slot_sloppy_immutable_silent: inner.local_sloppy_immutable_silent,
        upvalues: inner.captured_upvalues,
        has_rest,
        has_prototype_property: false,
        captures_this: true,
        // Arrow functions do not create their own `arguments`; it is
        // lexically resolved from the enclosing function/global scope.
        arguments_slot: None,
        self_binding_slot: None,
        has_simple_parameter_list,
        return_type,
        is_strict: function_is_strict,
        is_constructor: false,
        is_derived_constructor: false,
        is_class_constructor: false,
        is_async: func.r#async,
        is_generator: false,
      },
      _env_len,
    ))
  }
}
