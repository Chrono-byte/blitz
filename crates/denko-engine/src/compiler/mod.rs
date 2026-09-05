// SPDX-License-Identifier: MPL-2.0
//! AST-to-bytecode compiler.
//!
//! The compiler transforms parsed TypeScript/JavaScript AST into executable
//! bytecode for the VM. It performs:
//! - Variable resolution (locals vs globals)
//! - Type annotation tracking for metadata and tooling
//! - Control flow compilation (if/else, while, try/catch, etc.)
//! - Function compilation with closure support
//!
//! # Architecture
//!
//! Compilation is single-pass over the AST:
//! 1. Pre-declare function names for hoisting support
//! 2. Compile statements sequentially
//! 3. Emit compiler-owned virtual instructions
//! 4. Finalize the virtual instructions into bytecode v2 words in a [`Chunk`]
//! 5. Track local variables and their types for metadata and tooling
//!
//! # Future Work
//!
//! - JIT compilation with multiple optimization passes
//! - Loop identification and optimization
//! - Dead code elimination
//! - Constant folding

use crate::bytecode::Chunk;
use crate::parser::ParserSession;
use crate::value::Value;
use emit::LoweringOpcode;
use oxc_ast::ast as oast;
use std::collections::HashSet;
use std::sync::Arc;

pub mod core;
pub(crate) use core::{
  Compiler, FinallyPatchState, IteratorCloseState, LabelPatchState, ModuleContext, Scope,
  ScopeKind, with_compile_context_override,
};
mod binding;
mod bytecode;
mod class;
mod early_errors;
mod emit;
mod expression;
mod function;
mod optimize;
mod optional_chain;
mod pre_declare;
mod reference;
mod statement;
mod tail;

const LINE: u32 = 1;

fn numeric_property_key(value: f64) -> String {
  crate::runtime_utils::js_number_to_string(value)
}

/// Compile a parsed program to bytecode.
///
/// # Errors
///
/// Returns an error string if compilation fails.
pub fn compile(
  program: &oast::Program<'_>,
  source: &str,
) -> Result<Chunk, crate::errors::CompilerError> {
  let source_text = Arc::new(source.to_string());
  let mut c = Compiler::new(source_text);
  c.compile_oxc(program, None)
}

/// Compile a parser session to bytecode.
///
/// The session owns both the AST and its backing source text, so callers
/// cannot accidentally compile an AST with unrelated source metadata.
pub fn compile_session(session: &ParserSession) -> Result<Chunk, crate::errors::CompilerError> {
  compile_oxc(session.program(), session.source())
}

/// Compile a parser session with module-linking metadata.
pub fn compile_session_with_context(
  session: &ParserSession,
  context: &ModuleContext,
) -> Result<Chunk, crate::errors::CompilerError> {
  compile_oxc_with_ctx(session.program(), Some(context), session.source())
}

/// Compile an oxc AST Program.
///
/// # Errors
///
/// Returns an error if compilation fails.
pub(crate) fn compile_oxc(
  program: &oast::Program<'_>,
  source: &str,
) -> Result<Chunk, crate::errors::CompilerError> {
  let source_text = Arc::new(source.to_string());
  let mut c = Compiler::new(source_text);
  c.compile_oxc(program, None)
}

/// Compile an oxc AST Program with an optional ModuleContext.
///
/// # Errors
///
/// Returns an error if compilation fails.
pub(crate) fn compile_oxc_with_ctx(
  program: &oast::Program<'_>,
  ctx: Option<&ModuleContext>,
  source: &str,
) -> Result<Chunk, crate::errors::CompilerError> {
  let source_text = Arc::new(source.to_string());
  let mut c = Compiler::new(source_text);
  c.compile_oxc(program, ctx)
}

/// Compile an oxc AST Program in REPL mode.
/// In REPL mode, the last expression statement does not emit Pop,
/// allowing the result to be returned for printing.
///
/// # Errors
///
/// Returns an error if compilation fails.
pub(crate) fn compile_oxc_repl(
  program: &oast::Program<'_>,
  source: &str,
) -> Result<Chunk, crate::errors::CompilerError> {
  let source_text = Arc::new(source.to_string());
  let mut c = Compiler::new(source_text);
  c.compile_oxc_with_mode(program, None, true)
}

/// Compile a parser session in persistent REPL mode.
///
/// The final expression remains on the stack as the submission result.
pub fn compile_repl_session(
  session: &ParserSession
) -> Result<Chunk, crate::errors::CompilerError> {
  compile_oxc_repl(session.program(), session.source())
}

impl Compiler {
  /// Compile an `oast::Program` with the direct AST walker.
  ///
  /// # Errors
  /// Returns a `CompilerError` if compilation fails.
  pub(crate) fn compile_oxc(
    &mut self,
    program: &oast::Program<'_>,
    ctx: Option<&ModuleContext>,
  ) -> Result<Chunk, crate::errors::CompilerError> {
    self.compile_oxc_with_mode(program, ctx, false)
  }

  /// Compile with explicit mode control.
  /// In REPL mode, the last expression statement does not emit Pop,
  /// allowing the result to be returned.
  ///
  /// # Errors
  /// Returns a `CompilerError` if compilation fails.
  pub(crate) fn compile_oxc_with_mode(
    &mut self,
    program: &oast::Program<'_>,
    ctx: Option<&ModuleContext>,
    repl_mode: bool,
  ) -> Result<Chunk, crate::errors::CompilerError> {
    let semantic = oxc_semantic::SemanticBuilder::new()
      .build(program)
      .semantic;
    let scoping = semantic.scoping();
    self.eval_scopes = Arc::new(
      scoping
        .scope_descendants_from_root()
        .filter(|id| scoping.scope_flags(*id).contains_direct_eval())
        .collect(),
    );
    if let Some(cctx) = ctx {
      self.current_module = Some(cctx.clone());
    } else {
      self.current_module = None;
    }
    self.strict_context = program.source_type.is_strict()
      || program
        .directives
        .iter()
        .any(|directive| directive.directive.as_str() == "use strict");
    self.chunk.allow_new_target = self.allow_new_target;
    self.chunk.allow_super = self.allow_super;

    self.validate_early_errors_in_program(&program.body, program.source_type.is_module())?;

    self.pre_declare_functions_oast(program);
    self.pre_declare_variables_oast(program);

    self.emit_hoisted_function_decls_oast_stmts(&program.body)?;

    let stmts: Vec<_> = program
      .body
      .iter()
      .filter(|s| !matches!(s, oast::Statement::FunctionDeclaration(_)))
      .collect();

    let completion_slot = if repl_mode {
      let slot = self.alloc_temp_local("statement_list_completion");
      if let Some(directive) = program.directives.last() {
        self.emit_constant(
          Value::String(directive.expression.value.to_string().into()),
          LINE,
        );
      } else {
        self.emit(LoweringOpcode::Undefined, LINE);
      }
      self.emit(LoweringOpcode::StoreLocal, LINE);
      self.emit_slot(slot, LINE);
      Some(slot)
    } else {
      None
    };

    for stmt in &stmts {
      self.compile_statement_oast_with_mode(stmt, false, completion_slot)?;
    }

    if let Some(slot) = completion_slot {
      self.emit(LoweringOpcode::LoadLocal, LINE);
      self.emit_slot(slot, LINE);
    }

    if let Some(error) = self.emission_error.take() {
      return Err(error);
    }

    self.emit(LoweringOpcode::Return, LINE);
    if self.is_root {
      self.chunk.root_slot_types = self.local_types.clone();
      self.chunk.root_slot_names = self.environment_slot_names();
      self.chunk.root_lexical_tdz = self.local_tdz.clone();
      self.chunk.root_slot_immutable = self.local_immutable.clone();
      self.chunk.root_function_declarations = program
        .body
        .iter()
        .filter_map(|statement| match statement {
          oast::Statement::FunctionDeclaration(function) => function
            .id
            .as_ref()
            .map(|identifier| identifier.name.to_string()),
          _ => None,
        })
        .collect();
      self.chunk.root_strict = self.strict_context;
    }
    // Read the root AST emitter's operations before optimization can remove
    // unreachable awaits. Nested functions have independent lowering builders.
    self.chunk.has_top_level_await = program.source_type.is_module()
      && self
        .lowering
        .instructions
        .iter()
        .any(|instruction| {
          matches!(
            instruction.opcode,
            LoweringOpcode::AwaitPromise | LoweringOpcode::GetAsyncIterator
          )
        });
    let code = std::mem::take(&mut self.chunk);
    let builder = std::mem::take(&mut self.lowering);
    bytecode::Lowering::finish(code, builder)
  }
}

#[cfg(test)]
mod tests;
