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

use crate::Compiler;
use crate::bytecode::{Chunk, FunctionProto, Op};
use crate::typecheck::Type;
use crate::value::Value;
use oxc_ast::ast as oast;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

const LINE: u32 = 1;

impl Compiler {
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

            let (proto, _env_len) = self.compile_function(&fname, f, None)?;
            let idx = self.chunk.add_function(proto);
            self.emit(Op::MakeClosure, LINE);
            self.emit_byte(idx as u8, LINE);
            let slot = if let Some(s) = self.resolve_local(&fname) {
                s
            } else {
                self.add_local_with_mirror(fname.clone(), None, self.is_root)
            };

            if slot == self.locals.len() - 1 {
                self.add_lexical_binding(&fname);
            }

            if self.is_root {
                self.emit(Op::Dup, LINE);
            }
            self.emit(Op::SetLocal, LINE);
            self.emit_byte(slot as u8, LINE);
            if self.is_root {
                self.emit_set_global_name(&fname);
            }
            let is_exported = self
                .current_module
                .as_ref()
                .is_some_and(|c| c.exports.contains(&fname));
            if is_exported {
                self.emit(Op::GetLocal, LINE);
                self.emit_byte(slot as u8, LINE);
                self.emit_set_own_module_export(&fname)?;
            }
        }

        Ok(())
    }

    /// Compile an oxc AST statement into bytecode.
    ///
    /// Wrapper that delegates to `compile_statement_oast_with_mode` with `skip_pop` = false,
    /// meaning expressions will have their values popped from the stack.
    fn compile_statement_oast(
        &mut self,
        stmt: &oast::Statement,
    ) -> Result<(), crate::errors::CompilerError> {
        self.compile_statement_oast_with_mode(stmt, false)
    }

    /// Compile an oxc AST statement into bytecode with optional result preservation.
    ///
    /// # Arguments
    ///
    /// * `stmt` - The statement to compile
    /// * `skip_pop` - If true, the result of expression statements is preserved on the stack
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
    ) -> Result<(), crate::errors::CompilerError> {
        use oxc_ast::ast::{
            BindingPattern as OPat, ForStatementLeft as OForLeft, Statement as OStmt,
        };
        match stmt {
            OStmt::ImportDeclaration(_) => {}
            OStmt::ExportAllDeclaration(_) => {}
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
                        let Some(local_name) = Self::module_export_name_to_string(&spec.local)
                        else {
                            continue;
                        };
                        let Some(export_name) = Self::module_export_name_to_string(&spec.exported)
                        else {
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
                            let fname = "<default>".to_string();
                            let (proto, _env_len) = self.compile_function(&fname, f, None)?;
                            let idx = self.chunk.add_function(proto);
                            self.emit(Op::MakeClosure, LINE);
                            self.emit_byte(idx as u8, LINE);
                        }
                    }
                    oast::ExportDefaultDeclarationKind::ClassDeclaration(c) => {
                        if let Some(id) = &c.id {
                            self.compile_class_declaration_oast(c)?;
                            self.emit_get_binding(id.name.as_str());
                        } else {
                            return Err(crate::errors::CompilerError::from(
                                "unsupported anonymous export default class declaration",
                            ));
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
                if !skip_pop {
                    self.emit(Op::Pop, LINE);
                }
            }
            OStmt::LabeledStatement(l) => {
                let can_continue = matches!(
                    &l.body,
                    OStmt::WhileStatement(_) | OStmt::ForStatement(_) | OStmt::ForInStatement(_)
                );
                self.label_patches.push(crate::compiler::LabelPatchState {
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
                self.compile_statement_oast(&l.body)?;
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
                    let target = self.chunk.code.len();
                    self.patch_jump_list(label_state.break_patches, target);
                }
            }
            OStmt::WithStatement(w) => {
                // The VM currently models `with` as a normal scoped block after
                // evaluating the object expression for side effects.
                self.compile_expr_oast(&w.object)?;
                self.emit(Op::Pop, LINE);
                self.enter_block_scope();
                self.compile_statement_oast(&w.body)?;
                self.exit_scope();
            }
            OStmt::BlockStatement(b) => {
                self.enter_block_scope_with_vars(&b.body);
                for s in &b.body {
                    self.compile_statement_oast(s)?;
                }
                self.exit_scope();
            }
            OStmt::IfStatement(i) => {
                self.compile_expr_oast(&i.test)?;
                self.emit(Op::JumpIfFalse, LINE);
                let jump_over = self.chunk.code.len();
                self.emit_byte(0, LINE);
                self.emit_byte(0, LINE);
                self.compile_statement_oast(&i.consequent)?;
                if let Some(alt) = &i.alternate {
                    self.emit(Op::Jump, LINE);
                    let jump_else = self.chunk.code.len();
                    self.emit_byte(0, LINE);
                    self.emit_byte(0, LINE);
                    self.patch_jump(jump_over);
                    self.compile_statement_oast(alt)?;
                    self.patch_jump(jump_else);
                } else {
                    self.patch_jump(jump_over);
                }
            }
            OStmt::SwitchStatement(s) => {
                // Evaluate discriminant once and store in a local slot.
                self.compile_expr_oast(&s.discriminant)?;
                let discr_name = format!("__switch_discriminant_{}", self.locals.len());
                let discr_slot = self.add_local_with_mirror(discr_name, None, false);
                self.emit(Op::SetLocal, LINE);
                self.emit_byte(discr_slot as u8, LINE);

                // `break` inside a switch should jump to switch end.
                self.break_patches.push(Vec::new());

                // First phase: dispatch checks, each successful match jumps to
                // the corresponding case body.
                let mut case_body_jumps: Vec<(usize, usize)> = Vec::new();
                let mut default_index: Option<usize> = None;
                for (case_idx, case) in s.cases.iter().enumerate() {
                    if let Some(test) = &case.test {
                        self.emit(Op::GetLocal, LINE);
                        self.emit_byte(discr_slot as u8, LINE);
                        self.compile_expr_oast(test)?;
                        self.emit(Op::StrictEq, LINE);
                        self.emit(Op::JumpIfFalse, LINE);
                        let skip_jump = self.chunk.code.len();
                        self.emit_byte(0, LINE);
                        self.emit_byte(0, LINE);

                        self.emit(Op::Jump, LINE);
                        let body_jump = self.chunk.code.len();
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
                let no_match_jump = self.chunk.code.len();
                self.emit_byte(0, LINE);
                self.emit_byte(0, LINE);

                // Second phase: emit case bodies in source order. Fallthrough is
                // represented naturally by layout.
                let mut case_starts: Vec<usize> = vec![0; s.cases.len()];
                for (case_idx, case) in s.cases.iter().enumerate() {
                    case_starts[case_idx] = self.chunk.code.len();
                    for stmt in &case.consequent {
                        self.compile_statement_oast(stmt)?;
                    }
                }
                let switch_end = self.chunk.code.len();

                // Patch successful-case jumps to case body starts.
                for (case_idx, jump_pos) in case_body_jumps {
                    let target = case_starts[case_idx] as isize - (jump_pos as isize) - 2;
                    let v = target as i16;
                    self.chunk.code[jump_pos] = (v >> 8) as u8;
                    self.chunk.code[jump_pos + 1] = (v & 0xff) as u8;
                }

                // Patch no-match jump to default start or end.
                let default_target = default_index.map_or(switch_end, |idx| case_starts[idx]);
                let no_match_off = default_target as isize - (no_match_jump as isize) - 2;
                let no_match_v = no_match_off as i16;
                self.chunk.code[no_match_jump] = (no_match_v >> 8) as u8;
                self.chunk.code[no_match_jump + 1] = (no_match_v & 0xff) as u8;

                // Patch `break` statements emitted within this switch.
                if let Some(patches) = self.break_patches.pop() {
                    for patch_pos in patches {
                        let target = switch_end - patch_pos - 2;
                        self.chunk.code[patch_pos] = (target >> 8) as u8;
                        self.chunk.code[patch_pos + 1] = (target & 0xff) as u8;
                    }
                }
            }
            OStmt::WhileStatement(w) => {
                self.loop_depth += 1;
                self.continue_patches.push(Vec::new());
                self.break_patches.push(Vec::new());
                let loop_start = self.chunk.code.len();
                self.compile_expr_oast(&w.test)?;
                self.emit(Op::JumpIfFalse, LINE);
                let exit_jump = self.chunk.code.len();
                self.emit_byte(0, LINE);
                self.emit_byte(0, LINE);
                self.enter_block_scope();
                self.compile_statement_oast(&w.body)?;
                self.exit_scope();
                self.emit(Op::Jump, LINE);
                let back_jump_pos = self.chunk.code.len();
                let back_offset = loop_start as isize - (self.chunk.code.len() + 2) as isize;
                self.emit_short_signed(back_offset, LINE);
                if let Some(patches) = self.continue_patches.pop() {
                    for patch_pos in patches {
                        let target = back_jump_pos as isize - (patch_pos as isize) - 2;
                        let v = target as i16;
                        self.chunk.code[patch_pos] = (v >> 8) as u8;
                        self.chunk.code[patch_pos + 1] = (v & 0xff) as u8;
                    }
                }
                self.patch_jump(exit_jump);
                if let Some(patches) = self.break_patches.pop() {
                    for patch_pos in patches {
                        let target = self.chunk.code.len() - patch_pos - 2;
                        self.chunk.code[patch_pos] = (target >> 8) as u8;
                        self.chunk.code[patch_pos + 1] = (target & 0xff) as u8;
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
                    let loop_end = self.chunk.code.len();
                    self.patch_jump_list(continue_patches, loop_start);
                    self.patch_jump_list(break_patches, loop_end);
                }
                self.loop_depth = self.loop_depth.saturating_sub(1);
            }
            OStmt::DoWhileStatement(d) => {
                self.loop_depth += 1;
                self.continue_patches.push(Vec::new());
                self.break_patches.push(Vec::new());
                let loop_start = self.chunk.code.len();
                self.enter_block_scope();
                self.compile_statement_oast(&d.body)?;
                self.exit_scope();
                let test_start = self.chunk.code.len();
                self.compile_expr_oast(&d.test)?;
                self.emit(Op::JumpIfFalse, LINE);
                let exit_jump = self.chunk.code.len();
                self.emit_byte(0, LINE);
                self.emit_byte(0, LINE);
                self.emit(Op::Jump, LINE);
                let _back_jump_pos = self.chunk.code.len();
                let back_offset = loop_start as isize - (self.chunk.code.len() + 2) as isize;
                self.emit_short_signed(back_offset, LINE);
                if let Some(patches) = self.continue_patches.pop() {
                    for patch_pos in patches {
                        let target = test_start as isize - (patch_pos as isize) - 2;
                        let v = target as i16;
                        self.chunk.code[patch_pos] = (v >> 8) as u8;
                        self.chunk.code[patch_pos + 1] = (v & 0xff) as u8;
                    }
                }
                self.patch_jump(exit_jump);
                if let Some(patches) = self.break_patches.pop() {
                    for patch_pos in patches {
                        let target = self.chunk.code.len() - patch_pos - 2;
                        let v = target as i16;
                        self.chunk.code[patch_pos] = (v >> 8) as u8;
                        self.chunk.code[patch_pos + 1] = (v & 0xff) as u8;
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
                let loop_end = self.chunk.code.len();
                if let Some((continue_patches, break_patches)) = label_patches {
                    self.patch_jump_list(continue_patches, test_start);
                    self.patch_jump_list(break_patches, loop_end);
                }
                self.loop_depth = self.loop_depth.saturating_sub(1);
            }
            OStmt::ForStatement(f) => {
                use oxc_ast::ast::ForStatementInit as OForInit;
                self.loop_depth += 1;
                self.continue_patches.push(Vec::new());
                self.break_patches.push(Vec::new());
                let mut per_iteration_slots = Vec::new();

                if let Some(init) = &f.init {
                    match init {
                        OForInit::VariableDeclaration(v) => {
                            for dec in &v.declarations {
                                if let Some(init) = &dec.init {
                                    self.compile_expr_oast(init)?;
                                } else {
                                    self.emit(Op::Undefined, LINE);
                                }
                                let name = match &dec.id {
                                    OPat::BindingIdentifier(i) => i.name.to_string(),
                                    _ => {
                                        return Err(crate::errors::CompilerError::from(
                                            "unsupported var pattern",
                                        ));
                                    }
                                };
                                let mirror_global = self.is_root;
                                let slot =
                                    if matches!(v.kind, oxc_ast::ast::VariableDeclarationKind::Var)
                                    {
                                        self.resolve_local(&name).unwrap_or_else(|| {
                                            self.add_var_local(name.clone(), None, mirror_global)
                                        })
                                    } else {
                                        self.add_local_with_mirror(
                                            name.clone(),
                                            None,
                                            mirror_global,
                                        )
                                    };
                                if matches!(
                                    v.kind,
                                    oxc_ast::ast::VariableDeclarationKind::Let
                                        | oxc_ast::ast::VariableDeclarationKind::Const
                                ) {
                                    per_iteration_slots.push(slot);
                                }
                                if self.is_root {
                                    self.emit(Op::Dup, LINE);
                                }
                                self.emit(Op::SetLocal, LINE);
                                self.emit_byte(slot as u8, LINE);
                                if self.is_root && mirror_global {
                                    self.emit_set_global_name(&name);
                                }
                            }
                        }
                        other => {
                            let expr = other.as_expression().ok_or_else(|| {
                                crate::errors::CompilerError::from(
                                    "unsupported for-loop initializer",
                                )
                            })?;
                            self.compile_expr_oast(expr)?;
                            self.emit(Op::Pop, LINE);
                        }
                    }
                }

                for &slot in &per_iteration_slots {
                    self.emit(Op::CloneLocalCell, LINE);
                    self.emit_byte(slot as u8, LINE);
                }

                let loop_start = self.chunk.code.len();
                let exit_jump = if let Some(test) = &f.test {
                    self.compile_expr_oast(test)?;
                    self.emit(Op::JumpIfFalse, LINE);
                    let exit_jump = self.chunk.code.len();
                    self.emit_byte(0, LINE);
                    self.emit_byte(0, LINE);
                    Some(exit_jump)
                } else {
                    None
                };

                self.enter_block_scope();
                self.compile_statement_oast(&f.body)?;
                self.exit_scope();

                let update_start = self.chunk.code.len();
                if let Some(patches) = self.continue_patches.pop() {
                    for patch_pos in patches {
                        let target = update_start as isize - (patch_pos as isize) - 2;
                        let v = target as i16;
                        self.chunk.code[patch_pos] = (v >> 8) as u8;
                        self.chunk.code[patch_pos + 1] = (v & 0xff) as u8;
                    }
                }

                for &slot in &per_iteration_slots {
                    self.emit(Op::CloneLocalCell, LINE);
                    self.emit_byte(slot as u8, LINE);
                }

                if let Some(update) = &f.update {
                    self.compile_expr_oast(update)?;
                    self.emit(Op::Pop, LINE);
                }

                self.emit(Op::Jump, LINE);
                let back_offset = loop_start as isize - (self.chunk.code.len() + 2) as isize;
                self.emit_short_signed(back_offset, LINE);

                if let Some(exit_jump) = exit_jump {
                    self.patch_jump(exit_jump);
                }
                if let Some(patches) = self.break_patches.pop() {
                    for patch_pos in patches {
                        let target = self.chunk.code.len() - patch_pos - 2;
                        self.chunk.code[patch_pos] = (target >> 8) as u8;
                        self.chunk.code[patch_pos + 1] = (target & 0xff) as u8;
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
                    let loop_end = self.chunk.code.len();
                    self.patch_jump_list(continue_patches, update_start);
                    self.patch_jump_list(break_patches, loop_end);
                }
                self.loop_depth = self.loop_depth.saturating_sub(1);
            }
            OStmt::ForOfStatement(f) => {
                self.loop_depth += 1;
                self.continue_patches.push(Vec::new());
                self.break_patches.push(Vec::new());

                // iterable -> iterator via iterable[Symbol.iterator]()
                self.compile_expr_oast(&f.right)?;
                self.emit(Op::Dup, LINE);
                self.emit_get_global_name("Symbol");
                self.emit_constant(Value::String("iterator".to_string()), LINE);
                self.emit(Op::GetPropertyIC, LINE);
                let symbol_iterator_slot = self.alloc_ic_slot()?;
                self.emit_byte(symbol_iterator_slot, LINE);
                self.emit(Op::GetProperty, LINE);
                self.emit(Op::CallMethod, LINE);
                self.emit_byte(0, LINE);

                let iterator_slot = self.add_local_with_mirror(
                    format!("__forof_iter_{}", self.locals.len()),
                    None,
                    false,
                );
                self.emit(Op::SetLocal, LINE);
                self.emit_byte(iterator_slot as u8, LINE);

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
                    for name in names {
                        if self.resolve_local(&name).is_none() {
                            self.add_local_with_mirror(name.clone(), None, false);
                        }
                    }
                }

                let loop_start = self.chunk.code.len();

                self.emit(Op::GetLocal, LINE);
                self.emit_byte(iterator_slot as u8, LINE);
                self.emit(Op::IteratorNext, LINE);
                self.emit(Op::Dup, LINE);
                self.emit_constant(Value::String("done".to_string()), LINE);
                self.emit(Op::GetPropertyIC, LINE);
                let done_slot = self.alloc_ic_slot()?;
                self.emit_byte(done_slot, LINE);
                self.emit(Op::JumpIfTrue, LINE);
                let exit_jump = self.chunk.code.len();
                self.emit_byte(0, LINE);
                self.emit_byte(0, LINE);
                self.emit_constant(Value::String("value".to_string()), LINE);
                self.emit(Op::GetPropertyIC, LINE);
                let value_slot = self.alloc_ic_slot()?;
                self.emit_byte(value_slot, LINE);

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
                            for name in &names {
                                if let Some(slot) = self.resolve_local(name) {
                                    self.emit(Op::CloneLocalCell, LINE);
                                    self.emit_byte(slot as u8, LINE);
                                }
                            }
                        }
                        self.compile_binding_pattern_assignment_oast(&dec.id, v.kind)?;
                    }
                    oast::ForStatementLeft::AssignmentTargetIdentifier(id) => {
                        let name = id.name.as_str();
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
                    _ => {
                        return Err(crate::errors::CompilerError::from(
                            "unsupported for-of assignment target",
                        ));
                    }
                }

                self.enter_block_scope();
                self.compile_statement_oast(&f.body)?;
                self.exit_scope();

                let update_start = loop_start;
                if let Some(patches) = self.continue_patches.pop() {
                    for patch_pos in patches {
                        let target = update_start as isize - (patch_pos as isize) - 2;
                        let v = target as i16;
                        self.chunk.code[patch_pos] = (v >> 8) as u8;
                        self.chunk.code[patch_pos + 1] = (v & 0xff) as u8;
                    }
                }

                self.emit(Op::Jump, LINE);
                let back_offset = loop_start as isize - (self.chunk.code.len() + 2) as isize;
                self.emit_short_signed(back_offset, LINE);

                self.patch_jump(exit_jump);
                self.emit(Op::Jump, LINE);
                let end_jump = self.chunk.code.len();
                self.emit_byte(0, LINE);
                self.emit_byte(0, LINE);

                let break_cleanup = self.chunk.code.len();
                if let Some(patches) = self.break_patches.pop() {
                    for patch_pos in patches {
                        let target = break_cleanup as isize - (patch_pos as isize) - 2;
                        let v = target as i16;
                        self.chunk.code[patch_pos] = (v >> 8) as u8;
                        self.chunk.code[patch_pos + 1] = (v & 0xff) as u8;
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
                self.emit(Op::GetLocal, LINE);
                self.emit_byte(iterator_slot as u8, LINE);
                self.emit(Op::IteratorClose, LINE);
                self.emit(Op::Pop, LINE);
                self.patch_jump(end_jump);
                self.loop_depth = self.loop_depth.saturating_sub(1);
            }
            OStmt::ForInStatement(f) => {
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
                    if v.declarations
                        .first()
                        .and_then(|dec| dec.init.as_ref())
                        .is_some()
                    {
                        return Err(crate::errors::CompilerError::from(
                            "SyntaxError: for-in lexical declaration may not have an initializer",
                        ));
                    }
                }

                self.emit_get_global_name("__native_forInKeys");
                self.compile_expr_oast(&f.right)?;
                self.emit(Op::CallIC, LINE);
                self.emit_byte(1, LINE);

                let keys_slot = self.add_local_with_mirror(
                    format!("__forin_keys_{}", self.locals.len()),
                    None,
                    false,
                );
                self.emit(Op::SetLocal, LINE);
                self.emit_byte(keys_slot as u8, LINE);

                self.emit_constant(Value::Number(0.0), LINE);
                let index_slot = self.add_local_with_mirror(
                    format!("__forin_i_{}", self.locals.len()),
                    None,
                    false,
                );
                self.emit(Op::SetLocal, LINE);
                self.emit_byte(index_slot as u8, LINE);

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
                    for name in names {
                        if self.resolve_local(&name).is_none() {
                            self.add_local_with_mirror(name.clone(), None, false);
                        }
                    }
                }

                let loop_start = self.chunk.code.len();

                self.emit(Op::GetLocal, LINE);
                self.emit_byte(index_slot as u8, LINE);
                self.emit(Op::GetLocal, LINE);
                self.emit_byte(keys_slot as u8, LINE);
                self.emit_constant(Value::String("length".to_string()), LINE);
                self.emit(Op::GetProperty, LINE);
                self.emit(Op::Lt, LINE);
                self.emit(Op::JumpIfFalse, LINE);
                let exit_jump = self.chunk.code.len();
                self.emit_byte(0, LINE);
                self.emit_byte(0, LINE);

                self.emit(Op::GetLocal, LINE);
                self.emit_byte(keys_slot as u8, LINE);
                self.emit(Op::GetLocal, LINE);
                self.emit_byte(index_slot as u8, LINE);
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
                            for name in &names {
                                if let Some(slot) = self.resolve_local(name) {
                                    self.emit(Op::CloneLocalCell, LINE);
                                    self.emit_byte(slot as u8, LINE);
                                }
                            }
                        }
                        self.compile_binding_pattern_assignment_oast(&dec.id, v.kind)?;
                    }
                    OForLeft::AssignmentTargetIdentifier(id) => {
                        let name = id.name.as_str();
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
                    _ => {
                        return Err(crate::errors::CompilerError::from(
                            "unsupported for-in assignment target",
                        ));
                    }
                }

                self.enter_block_scope();
                self.compile_statement_oast(&f.body)?;
                self.exit_scope();

                let update_start = self.chunk.code.len();
                if let Some(patches) = self.continue_patches.pop() {
                    for patch_pos in patches {
                        let target = update_start as isize - (patch_pos as isize) - 2;
                        let v = target as i16;
                        self.chunk.code[patch_pos] = (v >> 8) as u8;
                        self.chunk.code[patch_pos + 1] = (v & 0xff) as u8;
                    }
                }

                self.emit(Op::GetLocal, LINE);
                self.emit_byte(index_slot as u8, LINE);
                self.emit_constant(Value::Number(1.0), LINE);
                self.emit(Op::Add, LINE);
                self.emit(Op::SetLocal, LINE);
                self.emit_byte(index_slot as u8, LINE);

                self.emit(Op::Jump, LINE);
                let back_offset = loop_start as isize - (self.chunk.code.len() + 2) as isize;
                self.emit_short_signed(back_offset, LINE);

                self.patch_jump(exit_jump);
                if let Some(patches) = self.break_patches.pop() {
                    for patch_pos in patches {
                        let target = self.chunk.code.len() - patch_pos - 2;
                        self.chunk.code[patch_pos] = (target >> 8) as u8;
                        self.chunk.code[patch_pos + 1] = (target & 0xff) as u8;
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
                    let loop_end = self.chunk.code.len();
                    self.patch_jump_list(continue_patches, update_start);
                    self.patch_jump_list(break_patches, loop_end);
                }
                self.loop_depth = self.loop_depth.saturating_sub(1);
            }
            OStmt::ReturnStatement(r) => {
                if let Some(arg) = &r.argument {
                    self.compile_expr_oast(arg)?;
                } else {
                    self.emit(Op::Undefined, LINE);
                }
                let return_target = self.finally_patches.last().and_then(|state| {
                    if state.in_finalizer {
                        None
                    } else {
                        Some((state.return_slot, state.did_return_slot))
                    }
                });
                if let Some((return_slot, did_return_slot)) = return_target {
                    self.emit(Op::Dup, LINE);
                    self.emit(Op::SetLocal, LINE);
                    self.emit_byte(return_slot as u8, LINE);
                    self.emit(Op::True, LINE);
                    self.emit(Op::SetLocal, LINE);
                    self.emit_byte(did_return_slot as u8, LINE);
                    self.emit(Op::Jump, LINE);
                    let patch_pos = self.chunk.code.len();
                    self.emit_byte(0, LINE);
                    self.emit_byte(0, LINE);
                    if let Some(state) = self.finally_patches.last_mut() {
                        state.return_jumps.push(patch_pos);
                    }
                    return Ok(());
                }
                self.emit(Op::Return, LINE);
            }
            OStmt::ContinueStatement(c) => {
                self.emit(Op::Jump, LINE);
                let patch_pos = self.chunk.code.len();
                self.emit_byte(0, LINE);
                self.emit_byte(0, LINE);
                if let Some(label) = &c.label {
                    let label_name = label.name.to_string();
                    let idx = self
                        .label_patches
                        .iter()
                        .rposition(|state| state.name == label_name)
                        .ok_or_else(|| {
                            crate::errors::CompilerError::from(format!(
                                "unknown label '{}'",
                                label_name
                            ))
                        })?;
                    if !self.label_patches[idx].can_continue {
                        return Err(crate::errors::CompilerError::from(
                            "'continue' used with non-iteration label",
                        ));
                    }
                    self.label_patches[idx].continue_patches.push(patch_pos);
                } else {
                    if self.continue_patches.is_empty() {
                        return Err(crate::errors::CompilerError::from(
                            "'continue' used outside of a loop",
                        ));
                    }
                    if let Some(patches) = self.continue_patches.last_mut() {
                        patches.push(patch_pos);
                    }
                }
            }
            OStmt::BreakStatement(b) => {
                self.emit(Op::Jump, LINE);
                let patch_pos = self.chunk.code.len();
                self.emit_byte(0, LINE);
                self.emit_byte(0, LINE);
                if let Some(label) = &b.label {
                    let label_name = label.name.to_string();
                    let idx = self
                        .label_patches
                        .iter()
                        .rposition(|state| state.name == label_name)
                        .ok_or_else(|| {
                            crate::errors::CompilerError::from(format!(
                                "unknown label '{}'",
                                label_name
                            ))
                        })?;
                    self.label_patches[idx].break_patches.push(patch_pos);
                } else {
                    if self.break_patches.is_empty() {
                        return Err(crate::errors::CompilerError::from(
                            "'break' used outside of a loop",
                        ));
                    }
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
                let handler = t.handler.as_ref();
                let finalizer = t.finalizer.as_ref();
                if finalizer.is_some() {
                    let return_slot = self.add_local_with_mirror(
                        format!("__finally_return_{}", self.locals.len()),
                        None,
                        false,
                    );
                    let did_return_slot = self.add_local_with_mirror(
                        format!("__finally_did_return_{}", self.locals.len() + 1),
                        None,
                        false,
                    );
                    self.emit(Op::Undefined, LINE);
                    self.emit(Op::SetLocal, LINE);
                    self.emit_byte(return_slot as u8, LINE);
                    self.emit(Op::False, LINE);
                    self.emit(Op::SetLocal, LINE);
                    self.emit_byte(did_return_slot as u8, LINE);
                    self.finally_patches
                        .push(crate::compiler::FinallyPatchState {
                            return_slot,
                            did_return_slot,
                            return_jumps: Vec::new(),
                            in_finalizer: false,
                        });
                }
                let finally_idx = finalizer
                    .as_ref()
                    .map(|_| self.finally_patches.len().saturating_sub(1));

                self.emit(Op::PushHandler, LINE);
                let handler_jump = self.chunk.code.len();
                self.emit_byte(0, LINE);
                self.emit_byte(0, LINE);

                self.enter_block_scope_with_vars(&t.block.body);
                for stmt in &t.block.body {
                    self.compile_statement_oast(stmt)?;
                }
                self.exit_scope();

                self.emit(Op::PopHandler, LINE);

                // Normal completion path: run finalizer (if present), then skip handler path.
                if let Some(fin) = finalizer {
                    let finally_idx = finally_idx.ok_or_else(|| {
                        crate::errors::CompilerError::from("finally state missing")
                    })?;
                    {
                        let state = self.finally_patches.get_mut(finally_idx).ok_or_else(|| {
                            crate::errors::CompilerError::from("finally state missing")
                        })?;
                        state.in_finalizer = true;
                    }
                    let body_start = self.chunk.code.len();
                    self.enter_block_scope_with_vars(&fin.body);
                    for stmt in &fin.body {
                        self.compile_statement_oast(stmt)?;
                    }
                    self.exit_scope();
                    let (did_return_slot, return_slot) = {
                        let state = self.finally_patches.get_mut(finally_idx).ok_or_else(|| {
                            crate::errors::CompilerError::from("finally state missing")
                        })?;
                        state.in_finalizer = false;
                        let return_jumps = std::mem::take(&mut state.return_jumps);
                        for patch_pos in return_jumps {
                            let target = body_start as isize - (patch_pos as isize) - 2;
                            let v = target as i16;
                            self.chunk.code[patch_pos] = (v >> 8) as u8;
                            self.chunk.code[patch_pos + 1] = (v & 0xff) as u8;
                        }
                        (state.did_return_slot, state.return_slot)
                    };
                    self.emit(Op::GetLocal, LINE);
                    self.emit_byte(did_return_slot as u8, LINE);
                    self.emit(Op::JumpIfFalse, LINE);
                    let skip_return = self.chunk.code.len();
                    self.emit_byte(0, LINE);
                    self.emit_byte(0, LINE);
                    self.emit(Op::GetLocal, LINE);
                    self.emit_byte(return_slot as u8, LINE);
                    self.emit(Op::Return, LINE);
                    self.patch_jump(skip_return);
                }
                self.emit(Op::Jump, LINE);
                let end_jump = self.chunk.code.len();
                self.emit_byte(0, LINE);
                self.emit_byte(0, LINE);

                // Exception path starts here.
                self.patch_jump(handler_jump);

                match handler {
                    Some(handler) => {
                        if let Some(param) = &handler.param {
                            let name = match &param.pattern {
                                OPat::BindingIdentifier(i) => i.name.to_string(),
                                _ => "__catch_pattern".to_string(),
                            };
                            self.enter_block_scope_with_vars(&handler.body.body);
                            let slot = self.add_local_with_mirror(name, None, false);
                            self.emit(Op::SetLocal, LINE);
                            self.emit_byte(slot as u8, LINE);
                            self.emit(Op::GetLocal, LINE);
                            self.emit_byte(slot as u8, LINE);
                            self.compile_binding_pattern_assignment_oast(
                                &param.pattern,
                                oxc_ast::ast::VariableDeclarationKind::Let,
                            )?;
                            for stmt in &handler.body.body {
                                self.compile_statement_oast(stmt)?;
                            }
                            self.exit_scope();
                        } else {
                            self.emit(Op::Pop, LINE);
                            self.enter_block_scope_with_vars(&handler.body.body);
                            for stmt in &handler.body.body {
                                self.compile_statement_oast(stmt)?;
                            }
                            self.exit_scope();
                        }

                        if let Some(fin) = finalizer {
                            let finally_idx = finally_idx.ok_or_else(|| {
                                crate::errors::CompilerError::from("finally state missing")
                            })?;
                            {
                                let state =
                                    self.finally_patches.get_mut(finally_idx).ok_or_else(|| {
                                        crate::errors::CompilerError::from("finally state missing")
                                    })?;
                                state.in_finalizer = true;
                            }
                            self.enter_block_scope_with_vars(&fin.body);
                            for stmt in &fin.body {
                                self.compile_statement_oast(stmt)?;
                            }
                            self.exit_scope();
                            let state =
                                self.finally_patches.get_mut(finally_idx).ok_or_else(|| {
                                    crate::errors::CompilerError::from("finally state missing")
                                })?;
                            state.in_finalizer = false;
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
                        let slot =
                            self.add_local_with_mirror("__finally_thrown".to_string(), None, false);
                        self.emit(Op::SetLocal, LINE);
                        self.emit_byte(slot as u8, LINE);
                        if let Some(fin) = finalizer {
                            let finally_idx = finally_idx.ok_or_else(|| {
                                crate::errors::CompilerError::from("finally state missing")
                            })?;
                            {
                                let state =
                                    self.finally_patches.get_mut(finally_idx).ok_or_else(|| {
                                        crate::errors::CompilerError::from("finally state missing")
                                    })?;
                                state.in_finalizer = true;
                            }
                            for stmt in &fin.body {
                                self.compile_statement_oast(stmt)?;
                            }
                            let state =
                                self.finally_patches.get_mut(finally_idx).ok_or_else(|| {
                                    crate::errors::CompilerError::from("finally state missing")
                                })?;
                            state.in_finalizer = false;
                        }
                        self.emit(Op::GetLocal, LINE);
                        self.emit_byte(slot as u8, LINE);
                        self.emit(Op::Throw, LINE);
                        self.exit_scope();
                    }
                }

                self.patch_jump(end_jump);
                if finalizer.is_some() {
                    self.finally_patches.pop();
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
        let name =
            c.id.as_ref()
                .map(|id| id.name.to_string())
                .ok_or_else(|| crate::errors::CompilerError::from("anonymous class declaration"))?;

        if let Some(err) = self.check_var_lexical_conflict(&name) {
            return Err(crate::errors::CompilerError::from(err));
        }

        let mut ctor_chunk = Chunk::new(Rc::clone(&self.source_text));
        ctor_chunk.write(Op::Return as u8, LINE);
        let proto = FunctionProto {
            name: name.clone(),
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
        let slot = self
            .resolve_local(&name)
            .unwrap_or_else(|| self.add_local_with_mirror(name.clone(), None, self.is_root));

        if slot == self.locals.len() - 1 {
            self.add_lexical_binding(&name);
        }

        if self.is_root {
            self.emit(Op::Dup, LINE);
        }
        self.emit(Op::SetLocal, LINE);
        self.emit_byte(slot as u8, LINE);
        if self.is_root {
            self.emit_set_global_name(&name);
        }
        // Emit class elements (methods/fields). Minimal implementation:
        // Create closures for methods and install them on the constructor
        // (static) or constructor.prototype (instance). Private identifiers
        // are lowered to synthetic string keys recorded in `private_names`.
        for element in &c.body.body {
            if let oast::ClassElement::MethodDefinition(md) = element {
                // Only handle method definitions for now.
                // Compile the method to a FunctionProto and create a closure.
                let method_name = match &md.key {
                    oast::PropertyKey::StaticIdentifier(ident) => Some(ident.name.to_string()),
                    oast::PropertyKey::Identifier(ident) => Some(ident.name.to_string()),
                    oast::PropertyKey::StringLiteral(s) => Some(s.value.to_string()),
                    oast::PropertyKey::NumericLiteral(n) => Some(n.value.to_string()),
                    oast::PropertyKey::PrivateIdentifier(pid) => Some(pid.name.to_string()),
                    _ => None,
                };
                // Only support non-computed simple keys here.
                let (proto, _env_len) = self.compile_function(
                    method_name.as_deref().unwrap_or("<method>"),
                    &md.value,
                    None,
                )?;
                let fidx = self.chunk.add_function(proto);

                if md.r#static {
                    // static method: install on constructor object
                    // push constructor
                    self.emit(Op::GetLocal, LINE);
                    self.emit_byte(slot as u8, LINE);

                    // push key
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

                    // push closure
                    self.emit(Op::MakeClosure, LINE);
                    self.emit_byte(fidx as u8, LINE);

                    // set property: expects [obj, key, value]
                    self.emit(Op::SetPropertyIC, LINE);
                    let ic_slot = self.alloc_ic_slot()?;
                    self.emit_byte(ic_slot, LINE);
                } else {
                    // instance method: install on constructor.prototype
                    // push constructor
                    self.emit(Op::GetLocal, LINE);
                    self.emit_byte(slot as u8, LINE);
                    // push "prototype" key and getProperty
                    self.emit_constant(Value::String("prototype".to_string()), LINE);
                    self.emit(Op::GetProperty, LINE);

                    // push key
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

                    // push closure
                    self.emit(Op::MakeClosure, LINE);
                    self.emit_byte(fidx as u8, LINE);

                    // set property on prototype
                    self.emit(Op::SetPropertyIC, LINE);
                    let ic_slot = self.alloc_ic_slot()?;
                    self.emit_byte(ic_slot, LINE);
                }
            }
        }

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

    /// Emit bytecode to initialize default parameter values.
    ///
    /// For each parameter with an initializer, checks if the parameter is undefined
    /// and if so, replaces it with the initializer expression's value.
    fn emit_parameter_initializers(
        &mut self,
        params: &oast::FormalParameters<'_>,
    ) -> Result<(), crate::errors::CompilerError> {
        for (idx, param) in params.items.iter().enumerate() {
            let Some(init) = &param.initializer else {
                continue;
            };
            self.emit(Op::GetLocal, LINE);
            self.emit_byte(idx as u8, LINE);
            self.emit(Op::Undefined, LINE);
            self.emit(Op::StrictEq, LINE);
            self.emit(Op::JumpIfFalse, LINE);
            let skip_jump = self.chunk.code.len();
            self.emit_byte(0, LINE);
            self.emit_byte(0, LINE);
            self.compile_expr_oast(init)?;
            self.emit(Op::SetLocal, LINE);
            self.emit_byte(idx as u8, LINE);
            self.patch_jump(skip_jump);
        }
        Ok(())
    }

    /// Compile a function declaration into a FunctionProto with its own scope.
    ///
    /// Creates an inner compiler scope, compiles the function body, and returns
    /// the compiled prototype with all metadata about parameters, locals, and closures.
    pub(super) fn compile_function(
        &mut self,
        name: &str,
        func: &oast::Function,
        return_type: Option<Type>,
    ) -> Result<(FunctionProto, usize), crate::errors::CompilerError> {
        let _env_len = self.locals.len();
        let mut inner = Compiler {
            chunk: Chunk::new(Rc::clone(&self.source_text)),
            source_text: Rc::clone(&self.source_text),
            locals: Vec::new(),
            local_types: Vec::new(),
            local_tdz: Vec::new(),
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
            continue_patches: Vec::new(),
            break_patches: Vec::new(),
            finally_patches: Vec::new(),
            label_patches: Vec::new(),
            current_module: None,
            next_ic_slot: 0,
            outer_bindings: self.visible_bindings_snapshot(),
            upvalue_indices: HashMap::new(),
            captured_upvalues: Vec::new(),
            private_names: self.private_names.clone(),
            object_expr_had_set_property: false,
        };

        // Parameter placeholders (no type information available here).
        for p in &func.params.items {
            let pname = match &p.pattern {
                oast::BindingPattern::BindingIdentifier(i) => i.name.to_string(),
                _ => "_".to_string(),
            };
            inner.add_local_with_mirror(pname, None, false);
        }
        if let Some(rest) = &func.params.rest {
            let pname = match &rest.rest.argument {
                oast::BindingPattern::BindingIdentifier(i) => i.name.to_string(),
                _ => "_".to_string(),
            };
            inner.add_local_with_mirror(pname, None, false);
        }
        let arguments_slot = inner.add_local_with_mirror("arguments".to_string(), None, false);

        // Pre-declare nested functions inside this function body.
        if let Some(body) = &func.body {
            inner.pre_declare_variables_oast_stmts(&body.statements);
            inner.pre_declare_functions_oast_stmts(&body.statements);
            inner.emit_hoisted_function_decls_oast_stmts(&body.statements)?;
        }

        inner.emit_parameter_initializers(&func.params)?;

        // Compile function body statements into inner compiler.
        if let Some(body) = &func.body {
            for stmt in &body.statements {
                if matches!(stmt, oast::Statement::FunctionDeclaration(_)) {
                    continue;
                }
                inner.compile_statement_oast(stmt)?;
            }
        }

        inner.emit(Op::Return, LINE);
        let chunk = std::mem::take(&mut inner.chunk);
        let has_rest = func.params.rest.is_some();
        let has_simple_parameter_list = Self::has_simple_parameter_list(func);
        let slot_types = inner.local_types;
        let slot_names: Vec<Option<String>> = inner.locals.into_iter().map(Some).collect();
        let lexical_tdz = inner.local_tdz;
        let has_prototype_property = !func.r#async;
        let is_constructor = !func.r#async && !func.generator;
        let is_strict = self.strict_context
            || func
                .body
                .as_ref()
                .is_some_and(|body| Self::has_use_strict_directive(body));
        Ok((
            FunctionProto {
                name: name.to_string(),
                span: func.span,
                arity: func.params.items.len() + if has_rest { 1 } else { 0 },
                chunk: Rc::new(chunk),
                slot_types,
                slot_names,
                lexical_tdz,
                upvalues: inner.captured_upvalues,
                has_rest,
                has_prototype_property,
                captures_this: false,
                arguments_slot: Some(arguments_slot),
                has_simple_parameter_list,
                return_type,
                is_strict,
                is_constructor,
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
        let mut inner = Compiler {
            chunk: Chunk::new(Rc::clone(&self.source_text)),
            source_text: Rc::clone(&self.source_text),
            locals: Vec::new(),
            local_types: Vec::new(),
            local_tdz: Vec::new(),
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
            continue_patches: Vec::new(),
            break_patches: Vec::new(),
            finally_patches: Vec::new(),
            label_patches: Vec::new(),
            current_module: None,
            next_ic_slot: 0,
            outer_bindings: self.visible_bindings_snapshot(),
            upvalue_indices: HashMap::new(),
            captured_upvalues: Vec::new(),
            private_names: self.private_names.clone(),
            object_expr_had_set_property: false,
        };

        for p in &func.params.items {
            let pname = match &p.pattern {
                oast::BindingPattern::BindingIdentifier(i) => i.name.to_string(),
                _ => "_".to_string(),
            };
            inner.add_local_with_mirror(pname, None, false);
        }
        if let Some(rest) = &func.params.rest {
            let pname = match &rest.rest.argument {
                oast::BindingPattern::BindingIdentifier(i) => i.name.to_string(),
                _ => "_".to_string(),
            };
            inner.add_local_with_mirror(pname, None, false);
        }
        // Pre-declare nested functions inside this function body.
        inner.pre_declare_variables_oast_stmts(&func.body.statements);
        inner.pre_declare_functions_oast_stmts(&func.body.statements);

        inner.emit_hoisted_function_decls_oast_stmts(&func.body.statements)?;

        // Check if this is an implicit-return arrow function (expression: true).
        // In that case, the body contains a single ExpressionStatement that should be returned.
        if func.expression && func.body.statements.len() == 1 {
            if let oast::Statement::ExpressionStatement(expr_stmt) = &func.body.statements[0] {
                // Compile the expression and return its value
                inner.compile_expr_oast(&expr_stmt.expression)?;
                inner.emit(Op::Return, LINE);
            } else {
                // Shouldn't happen for expression: true, but handle it safely
                for stmt in &func.body.statements {
                    if !matches!(stmt, oast::Statement::FunctionDeclaration(_)) {
                        inner.compile_statement_oast(stmt)?;
                    }
                }
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
            inner.emit(Op::Return, LINE);
        }

        let chunk = std::mem::take(&mut inner.chunk);
        let has_rest = func.params.rest.is_some();
        let has_simple_parameter_list = func.params.rest.is_none()
            && func.params.items.iter().all(|param| {
                matches!(param.pattern, oast::BindingPattern::BindingIdentifier(_))
                    && param.initializer.is_none()
            });
        let slot_types = inner.local_types;
        let slot_names: Vec<Option<String>> = inner.locals.into_iter().map(Some).collect();
        let lexical_tdz = inner.local_tdz;
        let is_strict = self.strict_context || Self::has_use_strict_directive(&func.body);
        Ok((
            FunctionProto {
                name: name.to_string(),
                span: func.span,
                arity: func.params.items.len() + if has_rest { 1 } else { 0 },
                chunk: Rc::new(chunk),
                slot_types,
                slot_names,
                lexical_tdz,
                upvalues: inner.captured_upvalues,
                has_rest,
                has_prototype_property: false,
                captures_this: true,
                // Arrow functions do not create their own `arguments`; it is
                // lexically resolved from the enclosing function/global scope.
                arguments_slot: None,
                has_simple_parameter_list,
                return_type,
                is_strict,
                is_constructor: false,
                is_async: false,
                is_generator: false,
            },
            _env_len,
        ))
    }
}
