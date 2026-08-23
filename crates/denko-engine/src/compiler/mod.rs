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
//! 4. Finalize the virtual instructions into RISC v2 words in a [`Chunk`]
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
use emit::FrontendOp as Op;
use oxc_ast::ast as oast;
use std::collections::HashSet;
use std::sync::Arc;

pub mod core;
pub(crate) use core::{
  Compiler, FinallyPatchState, IteratorCloseState, LabelPatchState, ModuleContext, Scope,
  ScopeKind, with_compile_context_override,
};
mod binding;
mod declaration;
mod emit;
mod expression;
mod pre_declare;
mod risc;
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
// Public API: compile an oxc AST Program directly.
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
  fn lexical_declared_names_of_statement(
    stmt: &oast::Statement<'_>,
    out: &mut HashSet<String>,
  ) {
    match stmt {
      oast::Statement::VariableDeclaration(v)
        if !matches!(v.kind, oxc_ast::ast::VariableDeclarationKind::Var) =>
      {
        for decl in &v.declarations {
          Self::collect_binding_names(&decl.id, out);
        }
      }
      oast::Statement::FunctionDeclaration(f) => {
        if let Some(id) = &f.id {
          out.insert(id.name.to_string());
        }
      }
      oast::Statement::ClassDeclaration(c) => {
        if let Some(id) = &c.id {
          out.insert(id.name.to_string());
        }
      }
      _ => {}
    }
  }

  fn collect_binding_names(
    pattern: &oast::BindingPattern<'_>,
    out: &mut HashSet<String>,
  ) {
    match pattern {
      oast::BindingPattern::BindingIdentifier(i) => {
        out.insert(i.name.to_string());
      }
      oast::BindingPattern::AssignmentPattern(a) => Self::collect_binding_names(&a.left, out),
      oast::BindingPattern::ObjectPattern(o) => {
        for prop in &o.properties {
          Self::collect_binding_names(&prop.value, out);
        }
        if let Some(rest) = &o.rest {
          Self::collect_binding_names(&rest.argument, out);
        }
      }
      oast::BindingPattern::ArrayPattern(a) => {
        for elem in a.elements.iter().flatten() {
          Self::collect_binding_names(elem, out);
        }
        if let Some(rest) = &a.rest {
          Self::collect_binding_names(&rest.argument, out);
        }
      }
    }
  }

  fn var_declared_names_of_statement(
    &self,
    stmt: &oast::Statement<'_>,
    out: &mut HashSet<String>,
  ) {
    match stmt {
      oast::Statement::VariableDeclaration(v)
        if matches!(v.kind, oxc_ast::ast::VariableDeclarationKind::Var) =>
      {
        for decl in &v.declarations {
          Self::collect_binding_names(&decl.id, out);
        }
      }
      oast::Statement::BlockStatement(b) => {
        for stmt in &b.body {
          self.var_declared_names_of_statement(stmt, out);
        }
      }
      oast::Statement::IfStatement(i) => {
        self.var_declared_names_of_statement(&i.consequent, out);
        if let Some(alt) = &i.alternate {
          self.var_declared_names_of_statement(alt, out);
        }
      }
      oast::Statement::WhileStatement(w) => self.var_declared_names_of_statement(&w.body, out),
      oast::Statement::DoWhileStatement(d) => self.var_declared_names_of_statement(&d.body, out),
      oast::Statement::ForStatement(f) => {
        if let Some(init) = &f.init
          && let oxc_ast::ast::ForStatementInit::VariableDeclaration(v) = init
          && matches!(v.kind, oxc_ast::ast::VariableDeclarationKind::Var)
        {
          for decl in &v.declarations {
            Self::collect_binding_names(&decl.id, out);
          }
        }
        self.var_declared_names_of_statement(&f.body, out);
      }
      oast::Statement::ForInStatement(f) => {
        if let oxc_ast::ast::ForStatementLeft::VariableDeclaration(v) = &f.left
          && matches!(v.kind, oxc_ast::ast::VariableDeclarationKind::Var)
        {
          for decl in &v.declarations {
            Self::collect_binding_names(&decl.id, out);
          }
        }
        self.var_declared_names_of_statement(&f.body, out);
      }
      oast::Statement::ForOfStatement(f) => {
        if let oxc_ast::ast::ForStatementLeft::VariableDeclaration(v) = &f.left
          && matches!(v.kind, oxc_ast::ast::VariableDeclarationKind::Var)
        {
          for decl in &v.declarations {
            Self::collect_binding_names(&decl.id, out);
          }
        }
        self.var_declared_names_of_statement(&f.body, out);
      }
      oast::Statement::LabeledStatement(l) => self.var_declared_names_of_statement(&l.body, out),
      oast::Statement::WithStatement(w) => self.var_declared_names_of_statement(&w.body, out),
      oast::Statement::TryStatement(t) => {
        for stmt in &t.block.body {
          self.var_declared_names_of_statement(stmt, out);
        }
        if let Some(handler) = &t.handler {
          for stmt in &handler.body.body {
            self.var_declared_names_of_statement(stmt, out);
          }
        }
        if let Some(finalizer) = &t.finalizer {
          for stmt in &finalizer.body {
            self.var_declared_names_of_statement(stmt, out);
          }
        }
      }
      _ => {}
    }
  }

  fn validate_statement_position_function(
    &self,
    stmt: &oast::Statement<'_>,
  ) -> Result<(), crate::errors::CompilerError> {
    if matches!(stmt, oast::Statement::FunctionDeclaration(_)) {
      return Err(crate::errors::CompilerError::from(
        "SyntaxError: function declaration in statement position",
      ));
    }
    Ok(())
  }

  fn validate_early_errors_in_expression(
    &self,
    expr: &oast::Expression<'_>,
  ) -> Result<(), crate::errors::CompilerError> {
    let mut expr = expr;
    while let oast::Expression::ParenthesizedExpression(p) = expr {
      expr = &p.expression;
    }
    match expr {
      oast::Expression::FunctionExpression(f) => {
        if let Some(body) = &f.body {
          self.validate_early_errors_in_statements(&body.statements, false)?;
        }
      }
      oast::Expression::ArrowFunctionExpression(a) => {
        self.validate_early_errors_in_statements(&a.body.statements, false)?;
      }
      oast::Expression::ClassExpression(c) => {
        for element in &c.body.body {
          match element {
            oast::ClassElement::StaticBlock(block) => {
              self.validate_early_errors_in_statements(&block.body, false)?;
            }
            oast::ClassElement::MethodDefinition(md) => {
              if let Some(body) = &md.value.body {
                self.validate_early_errors_in_statements(&body.statements, false)?;
              }
            }
            oast::ClassElement::PropertyDefinition(prop) => {
              if let Some(value) = &prop.value {
                self.validate_early_errors_in_expression(value)?;
              }
            }
            oast::ClassElement::AccessorProperty(prop) => {
              if let Some(value) = &prop.value {
                self.validate_early_errors_in_expression(value)?;
              }
            }
            oast::ClassElement::TSIndexSignature(_) => {}
          }
        }
      }
      _ => {}
    }
    Ok(())
  }

  fn validate_early_errors_in_program(
    &self,
    stmts: &[oast::Statement<'_>],
    allow_module_declarations: bool,
  ) -> Result<(), crate::errors::CompilerError> {
    self.validate_early_errors_in_statements(stmts, allow_module_declarations)
  }

  fn validate_early_errors_in_statements(
    &self,
    stmts: &[oast::Statement<'_>],
    allow_module_declarations: bool,
  ) -> Result<(), crate::errors::CompilerError> {
    let mut lexical = HashSet::new();
    let mut vars = HashSet::new();
    for stmt in stmts {
      Self::lexical_declared_names_of_statement(stmt, &mut lexical);
      self.var_declared_names_of_statement(stmt, &mut vars);
    }
    let mut seen_lexical = HashSet::new();
    for stmt in stmts {
      if matches!(stmt, oast::Statement::FunctionDeclaration(_)) {
        continue;
      }
      let mut names = HashSet::new();
      Self::lexical_declared_names_of_statement(stmt, &mut names);
      let mut names: Vec<_> = names.into_iter().collect();
      names.sort();
      for name in names {
        if !seen_lexical.insert(name.clone()) {
          return Err(crate::errors::CompilerError::from(format!(
            "SyntaxError: redeclaration of lexical binding `{}`",
            name
          )));
        }
      }
    }
    if let Some(name) = lexical.iter().find(|name| vars.contains(*name)) {
      return Err(crate::errors::CompilerError::from(format!(
        "SyntaxError: redeclaration of lexical binding `{}`",
        name
      )));
    }

    for stmt in stmts {
      if matches!(
        stmt,
        oast::Statement::ImportDeclaration(_)
          | oast::Statement::ExportAllDeclaration(_)
          | oast::Statement::ExportDefaultDeclaration(_)
          | oast::Statement::ExportNamedDeclaration(_)
      ) && !allow_module_declarations
      {
        return Err(crate::errors::CompilerError::from(
          "SyntaxError: module declaration in statement position",
        ));
      }
      match stmt {
        oast::Statement::BlockStatement(b) => {
          self.validate_early_errors_in_statements(&b.body, false)?
        }
        oast::Statement::IfStatement(i) => {
          self.validate_statement_position_function(&i.consequent)?;
          self.validate_early_errors_in_statements(std::slice::from_ref(&i.consequent), false)?;
          if let Some(alt) = &i.alternate {
            self.validate_statement_position_function(alt)?;
            self.validate_early_errors_in_statements(std::slice::from_ref(alt), false)?;
          }
        }
        oast::Statement::WhileStatement(w) => {
          self.validate_statement_position_function(&w.body)?;
          self.validate_early_errors_in_statements(std::slice::from_ref(&w.body), false)?;
        }
        oast::Statement::DoWhileStatement(d) => {
          self.validate_statement_position_function(&d.body)?;
          self.validate_early_errors_in_statements(std::slice::from_ref(&d.body), false)?;
        }
        oast::Statement::ForStatement(f) => {
          self.validate_statement_position_function(&f.body)?;
          self.validate_early_errors_in_statements(std::slice::from_ref(&f.body), false)?;
        }
        oast::Statement::ForInStatement(f) => {
          self.validate_statement_position_function(&f.body)?;
          self.validate_early_errors_in_statements(std::slice::from_ref(&f.body), false)?;
        }
        oast::Statement::ForOfStatement(f) => {
          self.validate_statement_position_function(&f.body)?;
          self.validate_early_errors_in_statements(std::slice::from_ref(&f.body), false)?;
        }
        oast::Statement::LabeledStatement(l) => {
          self.validate_statement_position_function(&l.body)?;
          self.validate_early_errors_in_statements(std::slice::from_ref(&l.body), false)?;
        }
        oast::Statement::WithStatement(w) => {
          self.validate_early_errors_in_statements(std::slice::from_ref(&w.body), false)?;
        }
        oast::Statement::TryStatement(t) => {
          self.validate_early_errors_in_statements(&t.block.body, false)?;
          if let Some(handler) = &t.handler {
            self.validate_early_errors_in_statements(&handler.body.body, false)?;
          }
          if let Some(finalizer) = &t.finalizer {
            self.validate_early_errors_in_statements(&finalizer.body, false)?;
          }
        }
        oast::Statement::FunctionDeclaration(f) => {
          if let Some(body) = &f.body {
            self.validate_early_errors_in_statements(&body.statements, false)?;
          }
        }
        oast::Statement::ClassDeclaration(c) => {
          for element in &c.body.body {
            match element {
              oast::ClassElement::StaticBlock(block) => {
                self.validate_early_errors_in_statements(&block.body, false)?;
              }
              oast::ClassElement::MethodDefinition(md) => {
                if let Some(body) = &md.value.body {
                  self.validate_early_errors_in_statements(&body.statements, false)?;
                }
              }
              oast::ClassElement::PropertyDefinition(prop) => {
                if let Some(expr) = &prop.value {
                  self.validate_early_errors_in_expression(expr)?;
                }
              }
              oast::ClassElement::AccessorProperty(prop) => {
                if let Some(expr) = &prop.value {
                  self.validate_early_errors_in_expression(expr)?;
                }
              }
              oast::ClassElement::TSIndexSignature(_) => {}
            }
          }
        }
        oast::Statement::ExpressionStatement(e) => {
          self.validate_early_errors_in_expression(&e.expression)?;
        }
        _ => {}
      }
    }
    Ok(())
  }

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

    if repl_mode
      && stmts.is_empty()
      && let Some(directive) = program.directives.last()
    {
      self.emit_constant(
        Value::String(directive.expression.value.to_string().into()),
        LINE,
      );
    }

    for (i, stmt) in stmts.iter().enumerate() {
      let is_last_expr_stmt = repl_mode
        && i == stmts.len() - 1
        && matches!(stmt, oast::Statement::ExpressionStatement(_));
      self.compile_statement_oast_with_mode(stmt, is_last_expr_stmt, None)?;
    }

    if let Some(error) = self.emission_error.take() {
      return Err(error);
    }

    self.emit(Op::Return, LINE);
    if self.is_root {
      self.chunk.root_slot_types = self.local_types.clone();
      self.chunk.root_slot_names = self.environment_slot_names();
      self.chunk.root_lexical_tdz = self.local_tdz.clone();
      self.chunk.root_slot_immutable = self.local_immutable.clone();
      self.chunk.root_strict = self.strict_context;
    }
    let chunk = std::mem::take(&mut self.chunk);
    let builder = std::mem::take(&mut self.risc);
    risc::RiscBuilder::finish(chunk, builder)
  }
}

#[cfg(test)]
mod test {
  #![allow(clippy::as_conversions)]
  #![allow(clippy::unwrap_used)]
  use std::cell::RefCell;
  use std::collections::{HashMap, HashSet};
  use std::rc::Rc;

  use crate::bytecode::{decode_compare_jump, decode_rd, decode_rs};
  use crate::compiler::compile;
  use crate::runtime_utils;
  use crate::{Chunk, ModuleContext, ObjectData, Op, Value, Vm, compile_oxc_with_ctx};

  #[cfg(test)]
  fn walk_chunk_ops(chunk: &Chunk) -> Vec<(usize, Op)> {
    let mut ops = Vec::new();
    let mut ip = 0usize;
    while ip < chunk.code.len() {
      if let Some(op) = Op::from_byte((chunk.code[ip] & 0xFF) as u8) {
        ops.push((ip, op));
        // fixed-width model: one u32 per instruction
        ip += 1;
      } else {
        ip += 1;
      }
    }
    ops
  }

  #[test]
  fn compile_session_uses_its_own_source_text() {
    let source = crate::js!("let answer = 42;");
    let session = crate::parser::Parser::parse_source(source).expect("parse");

    let chunk = super::compile_session(&session).expect("compile session");

    assert_eq!(chunk.source_text.as_str(), source);
  }

  #[test]
  fn anonymous_default_class_compiles_as_a_named_default_export() {
    let source = crate::js!("export default class {}");
    let session =
      crate::parser::Parser::parse_source_with_type(source, oxc_span::SourceType::mjs())
        .expect("parse module");
    let context = ModuleContext {
      module_index: 0,
      imports: HashMap::new(),
      exports: HashSet::from(["default".to_string()]),
      namespace_exports: HashMap::new(),
    };

    let chunk = super::compile_session_with_context(&session, &context)
      .expect("compile anonymous default class");

    assert!(!chunk.code.is_empty());
  }

  #[test]
  fn numeric_property_keys_use_ecmascript_number_formatting() {
    assert_eq!(super::numeric_property_key(0.000_000_1), "1e-7");
    assert_eq!(
      super::numeric_property_key(1_000_000_000_000_000_000_000.0),
      "1e+21"
    );
    assert_eq!(super::numeric_property_key(-0.0), "0");
  }

  #[test]
  fn compile_simple_for_loop() {
    let src = crate::js!("for (var i = 0; i < 3; i = i + 1) { console.log(i); }");
    let session = crate::parser::Parser::parse_source(src).expect("parse ok");
    let program = session.program();
    let chunk = compile(program, session.source()).expect("compile ok");

    // Ensure we emitted a conditional branch for the test and at least one
    // unconditional Jump (back-jump / jump-over).
    let mut saw_conditional_branch = false;
    let mut saw_jump = false;
    for (_, op) in walk_chunk_ops(&chunk) {
      match op {
        Op::JumpIfFalse | Op::CompareJump => saw_conditional_branch = true,
        Op::Jump => saw_jump = true,
        _ => {}
      }
    }

    assert!(
      saw_conditional_branch,
      "expected conditional branch in compiled for-loop"
    );
    assert!(saw_jump, "expected Jump in compiled for-loop");
  }

  #[test]
  fn compile_for_with_continue() {
    let src =
      crate::js!("for (var i = 0; i < 3; i = i + 1) { if (i == 1) { continue; } console.log(i); }");
    let session = crate::parser::Parser::parse_source(src).expect("parse ok");
    let program = session.program();
    let chunk = compile(program, session.source()).expect("compile ok");

    // Expect multiple control-flow ops: one for the test exit, one for the
    // back-jump, and at least one emitted for 'continue' which should be
    // patched to jump to update.
    let mut conditional_branch = 0usize;
    let mut jump = 0usize;
    for (_, op) in walk_chunk_ops(&chunk) {
      match op {
        Op::JumpIfFalse | Op::CompareJump => conditional_branch += 1,
        Op::Jump => jump += 1,
        _ => {}
      }
    }

    assert!(
      conditional_branch >= 1,
      "expected at least one conditional branch"
    );
    assert!(
      jump >= 2,
      "expected at least two Jump opcodes (back-jump and continue)"
    );
  }

  #[cfg(test)]
  fn opcode_width(op: Op) -> usize {
    let _ = op;
    1
  }

  #[test]
  fn static_member_cache_sites_have_distinct_risc_instructions() {
    let src = crate::js!("a.x; a.y; a.x;");
    let session = crate::parser::Parser::parse_source(src).expect("parse ok");
    let program = session.program();
    let chunk = compile(program, session.source()).expect("compile ok");

    let sites = walk_chunk_ops(&chunk)
      .into_iter()
      .filter(|(_, op)| *op == Op::GetProperty)
      .map(|(ip, _)| {
        let word = chunk.code[ip];
        (ip, decode_rd(word), decode_rs(word))
      })
      .collect::<Vec<_>>();

    assert_eq!(
      sites.len(),
      3,
      "expected one property access per source site"
    );
    assert!(
      sites.windows(2).all(|pair| pair[0].0 < pair[1].0),
      "inline caches are keyed by distinct RISC instruction addresses"
    );
    assert!(
      sites.iter().all(|(_, object, key)| object != key),
      "GetProperty must decode distinct object/result and key registers"
    );
  }

  #[test]
  fn property_sites_are_not_limited_by_unused_u8_cache_slots() {
    let source = (0..300)
      .map(|index| format!("value.property{index};"))
      .collect::<String>();
    let session = crate::parser::Parser::parse_source(&source).expect("parse ok");
    let chunk = compile(session.program(), session.source()).expect("compile ok");
    let property_sites = walk_chunk_ops(&chunk)
      .into_iter()
      .filter(|(_, op)| *op == Op::GetProperty)
      .count();
    assert_eq!(property_sites, 300);
  }

  #[test]
  fn compile_for_loop_back_jump_targets_loop_start() {
    let src = crate::js!("for (var i = 0; i < 3; i = i + 1) {}");
    let session = crate::parser::Parser::parse_source(src).expect("parse ok");
    let program = session.program();
    let chunk = compile(program, session.source()).expect("compile ok");

    let mut loop_start = None;
    let mut back_target = None;
    for (ip, op) in walk_chunk_ops(&chunk) {
      match op {
        Op::GetGlobal if loop_start.is_none() => {
          loop_start = Some(ip);
        }
        Op::Jump => {
          let target = chunk
            .jump_target_from_ip(ip)
            .map(|target| target as isize);
          if target == loop_start.map(|v| v as isize) {
            back_target = target;
          }
        }
        _ => {}
      }
    }

    assert_eq!(back_target, loop_start.map(|v| v as isize));
  }

  #[test]
  fn compile_new_expression_lowers_to_construct() {
    let src = crate::js!("new Array(1, 2, 3);");
    let session = crate::parser::Parser::parse_source(src).expect("parse ok");
    let program = session.program();
    let chunk = compile(program, session.source()).expect("compile ok");

    let saw_construct = chunk
      .code
      .iter()
      .filter_map(|b| Op::from_byte((*b & 0xFF) as u8))
      .any(|op| matches!(op, Op::Construct));

    assert!(
      saw_construct,
      "expected Construct opcode for lowered new expression"
    );
  }

  #[test]
  fn compile_update_expression_emits_store() {
    let src = crate::js!("for (var i = 0; i < 3; i++) {}");
    let session = crate::parser::Parser::parse_source(src).expect("parse ok");
    let program = session.program();
    let chunk = compile(program, session.source()).expect("compile ok");

    let saw_add = chunk
      .code
      .iter()
      .filter_map(|b| Op::from_byte((*b & 0xFF) as u8))
      .any(|op| matches!(op, Op::Add));
    let saw_set_local = chunk
      .code
      .iter()
      .filter_map(|b| Op::from_byte((*b & 0xFF) as u8))
      .any(|op| matches!(op, Op::StoreLocal));

    assert!(saw_add, "expected i++ to emit Add");
    assert!(saw_set_local, "expected i++ to emit StoreLocal");
  }

  #[test]
  fn compile_member_call_uses_call_method() {
    let src = crate::js!("obj.method(1);");
    let session = crate::parser::Parser::parse_source(src).expect("parse ok");
    let program = session.program();
    let chunk = compile(program, session.source()).expect("compile ok");

    let saw_call_method = chunk
      .code
      .iter()
      .filter_map(|b| Op::from_byte((*b & 0xFF) as u8))
      .any(|op| matches!(op, Op::Call | Op::CallBuiltin));

    assert!(saw_call_method, "expected member call to emit CallMethod");
  }

  #[test]
  fn compile_for_in_uses_get_builtin() {
    let src = crate::js!("for (var k in obj) {}");
    let session = crate::parser::Parser::parse_source(src).expect("parse ok");
    let program = session.program();
    let chunk = compile(program, session.source()).expect("compile ok");

    let saw_get_builtin = chunk
      .code
      .iter()
      .filter_map(|b| Op::from_byte((*b & 0xFF) as u8))
      .any(|op| matches!(op, Op::GetGlobal));

    assert!(
      saw_get_builtin,
      "expected for-in lowering to emit GetBuiltin"
    );
  }

  #[test]
  fn compile_rejects_calls_with_more_than_u8_arguments() {
    let args = (0_i32..256_i32)
      .map(|i| i.to_string())
      .collect::<Vec<_>>()
      .join(", ");
    let src = crate::js!(format(script), "f({args});");
    let session = crate::parser::Parser::parse_source(&src).expect("parse ok");
    let program = session.program();
    let err = compile(program, session.source()).expect_err("compile should fail");

    assert!(
      err
        .to_string()
        .contains("too many arguments in call"),
      "unexpected error: {err}"
    );
  }

  #[test]
  fn compile_generator_non_simple_params_keep_later_params_tdz() {
    let src = crate::js!("function* f(x = y, y) {}");
    let session = crate::parser::Parser::parse_source(src).expect("parse ok");
    let program = session.program();
    let chunk = compile(program, session.source()).expect("compile ok");

    let proto = chunk
      .functions
      .iter()
      .find(|p| p.name == "f")
      .expect("generator proto exists");

    assert!(
      proto
        .slot_names
        .iter()
        .any(|name| name.as_deref() == Some("y")),
      "expected generator parameter y to have a local slot"
    );
    let y_slot = proto
      .slot_names
      .iter()
      .position(|name| name.as_deref() == Some("y"))
      .expect("y slot");
    assert!(
      proto
        .lexical_tdz
        .get(y_slot)
        .copied()
        .unwrap_or(false),
      "later generator parameter y should start in TDZ"
    );

    let mut ops = Vec::new();
    let mut ip = 0usize;
    while ip < proto.chunk.code.len() && ops.len() < 16 {
      let op = Op::from_byte((proto.chunk.code[ip] & 0xFF) as u8).expect("valid opcode");
      let width = opcode_width(op);
      let operand = if matches!(op, Op::LoadLocal | Op::StoreLocal | Op::CloneLocalCell) {
        proto.chunk.code.get(ip + 1).copied()
      } else {
        None
      };
      ops.push((op.name(), operand));
      ip += width;
    }
    eprintln!("generator proto ops: {:?}", ops);
  }

  #[test]
  fn compile_object_generator_method_emits_body() {
    let src = crate::js!("var o = { *m() { let x; eval(\"var x;\"); } };");
    let session = crate::parser::Parser::parse_source(src).expect("parse ok");
    let program = session.program();
    let chunk = compile(program, session.source()).expect("compile ok");

    eprintln!(
      "object generator function names: {:?}",
      chunk
        .functions
        .iter()
        .map(|p| (p.name.clone(), p.chunk.code.len(), p.is_generator))
        .collect::<Vec<_>>()
    );
    let proto = chunk
      .functions
      .iter()
      .find(|p| p.is_generator)
      .expect("object generator proto exists");

    eprintln!(
      "object generator proto: code_len={} consts={} slots={} tdz={:?}",
      proto.chunk.code.len(),
      proto.chunk.constants.len(),
      proto.slot_names.len(),
      proto.lexical_tdz
    );
    let mut ops = Vec::new();
    let mut ip = 0usize;
    while ip < proto.chunk.code.len() && ops.len() < 24 {
      let op = Op::from_byte((proto.chunk.code[ip] & 0xFF) as u8).expect("valid opcode");
      let width = opcode_width(op);
      ops.push(op.name());
      ip += width;
    }
    eprintln!("object generator proto ops: {:?}", ops);

    assert!(
      !proto.chunk.code.is_empty(),
      "object generator method should emit bytecode"
    );
  }

  #[test]
  fn strict_direct_return_calls_emit_tail_call() {
    let src = crate::js!(r#""use strict"; function recurse(value) { return recurse(value - 1); }"#);
    let session = crate::parser::Parser::parse_source(src).expect("parse ok");
    let chunk = compile(session.program(), session.source()).expect("compile ok");
    let proto = chunk
      .functions
      .iter()
      .find(|proto| proto.name == "recurse")
      .expect("function proto");

    assert!(
      proto
        .chunk
        .code
        .iter()
        .any(|word| { Op::from_byte((word & 0xFF) as u8) == Some(Op::TailCall) })
    );
  }

  #[test]
  fn compile_dynamic_import_uses_get_builtin() {
    let src = crate::js!("import('./dep.js');");
    let session = crate::parser::Parser::parse_source(src).expect("parse ok");
    let program = session.program();
    let chunk = compile(program, session.source()).expect("compile ok");

    let saw_get_builtin = chunk
      .code
      .iter()
      .filter_map(|b| Op::from_byte((*b & 0xFF) as u8))
      .any(|op| matches!(op, Op::GetBuiltin));

    assert!(
      saw_get_builtin,
      "expected dynamic import to emit GetBuiltin"
    );
  }

  #[test]
  fn compile_sequence_expression_keeps_last_value() {
    let src = crate::js!("var x = (a = 1, 2);");
    let session = crate::parser::Parser::parse_source(src).expect("parse_source failed");
    let program = session.program();
    let chunk = compile(program, session.source()).expect("compile ok");

    chunk
      .verify()
      .expect("sequence expression emits valid RISC");
  }

  #[test]
  fn imported_binding_assignment_is_rejected() {
    let src = crate::js!("a = 1;");
    let session = crate::parser::Parser::parse_source(src).expect("parse ok");
    let program = session.program();
    let ctx = ModuleContext {
      module_index: 0,
      imports: HashMap::from([(String::from("a"), (1usize, String::from("value")))]),
      exports: HashSet::new(),
      namespace_exports: HashMap::new(),
    };

    let err =
      compile_oxc_with_ctx(program, Some(&ctx), session.source()).expect_err("compile should fail");
    assert!(
      err.to_string().contains("imported binding"),
      "unexpected error: {err}"
    );
  }

  #[test]
  fn strict_assignment_to_arguments_is_rejected() {
    let src = crate::js!(
      r#"
        function testcase() {
            'use strict';
            arguments = 7;
        }
    "#
    );
    let session = crate::parser::Parser::parse_source(src).expect("parse ok");
    let program = session.program();

    let err = compile(program, session.source()).expect_err("compile should fail");
    assert!(
      err
        .to_string()
        .contains("not allowed in strict mode"),
      "unexpected error: {err}"
    );
  }

  #[test]
  fn nullish_coalescing_is_supported() {
    let src = crate::js!("1 ?? 2;");
    let session = crate::parser::Parser::parse_source(src).expect("parse ok");
    let program = session.program();

    let chunk = compile(program, session.source()).expect("compile ok");
    let saw_coalesce = chunk
      .code
      .iter()
      .filter_map(|b| Op::from_byte((*b & 0xFF) as u8))
      .any(|op| matches!(op, Op::JumpIfNullish));
    assert!(
      saw_coalesce,
      "expected nullish coalescing lowering to emit JumpIfNullish"
    );
  }

  #[test]
  fn comparison_branches_use_compare_jump() {
    let src = crate::js!("if (a === b) { 1; } else { 2; } a === b ? 1 : 2;");
    let session = crate::parser::Parser::parse_source(src).expect("parse ok");
    let program = session.program();

    let chunk = compile(program, session.source()).expect("compile ok");
    let saw_compare_jump = chunk
      .code
      .iter()
      .filter_map(|b| Op::from_byte((*b & 0xFF) as u8))
      .any(|op| matches!(op, Op::CompareJump));
    assert!(
      saw_compare_jump,
      "expected comparison branching to emit CompareJump"
    );
  }

  #[test]
  fn while_compare_jump_preserves_compare_opcode_payload() {
    let src = crate::js!("var s = \"\"; while (s.length < 4) { s = \"0\" + s; }");
    let session = crate::parser::Parser::parse_source(src).expect("parse ok");
    let program = session.program();

    let chunk = compile(program, session.source()).expect("compile ok");
    let (condition, left, right, offset) = chunk
      .code
      .iter()
      .find_map(|word| {
        (Op::from_byte((*word & 0xFF) as u8) == Some(Op::CompareJump))
          .then(|| decode_compare_jump(*word))
      })
      .expect("expected CompareJump in while loop");
    assert_eq!(
      condition, 2,
      "while condition should preserve LessThan compare op"
    );
    assert_ne!(left, right, "comparison inputs must use distinct registers");
    assert!(offset > 0, "false comparison must branch out of the loop");
  }

  #[test]
  fn compile_for_of_emits_iterator_ops() {
    let src = crate::js!("for (var x of [1, 2, 3]) {}");
    let session = crate::parser::Parser::parse_source(src).expect("parse ok");
    let program = session.program();
    let chunk = compile(program, session.source()).expect("compile ok");

    let ops: Vec<_> = chunk
      .code
      .iter()
      .filter_map(|b| Op::from_byte((*b & 0xFF) as u8))
      .collect();
    assert!(
      ops
        .iter()
        .any(|op| matches!(op, Op::IteratorNext))
    );
    assert!(ops.iter().any(|op| matches!(op, Op::GetProperty)));
  }

  #[test]
  fn compile_for_await_uses_async_iterator_acquisition() {
    let src = crate::js!("async function f(xs) { for await (const x of xs) {} }");
    let session = crate::parser::Parser::parse_source(src).expect("parse ok");
    let program = session.program();
    let chunk = compile(program, session.source()).expect("compile ok");

    let ops: Vec<_> = chunk
      .functions
      .iter()
      .flat_map(|function| function.chunk.code.iter())
      .filter_map(|word| Op::from_byte((*word & 0xFF) as u8))
      .collect();
    assert!(
      ops
        .iter()
        .any(|op| matches!(op, Op::GetAsyncIterator)),
      "for-await must acquire an async iterator record"
    );
  }

  #[test]
  fn for_of_over_array_executes() {
    let src = crate::js!("var sum = 0; for (var x of [1, 2, 3]) { sum = sum + x; } sum;");
    let session = crate::parser::Parser::parse_source(src).expect("parse ok");
    let program = session.program();
    let chunk = compile(program, session.source()).expect("compile ok");

    let globals = Rc::new(RefCell::new(ObjectData::new()));
    globals.borrow_mut().properties.insert(
      crate::value::PropertyKey::String("globalThis".to_string().into()),
      crate::value::PropertyDescriptor::ordinary_data(Value::Object(Rc::clone(&globals))),
    );
    let mut vm = Vm::new(globals);
    drop(vm.run(chunk).expect("run ok"));

    assert!(
      matches!(vm.get_global("sum"), Some(Value::Number(n)) if (n - 6.0).abs() < f64::EPSILON)
    );
  }

  #[test]
  fn computed_member_compound_assignment_evaluates_reference_once() {
    let src = crate::js!(
      r#"
        globalThis.hits = 0;
        var obj = { value: 1 };
        function key() { globalThis.hits = globalThis.hits + 1; return "value"; }
        obj[key()] += 2;
    "#
    );
    let session = crate::parser::Parser::parse_source(src).expect("parse ok");
    let program = session.program();
    let chunk = compile(program, session.source()).expect("compile ok");

    let globals = Rc::new(RefCell::new(ObjectData::new()));
    globals.borrow_mut().properties.insert(
      crate::value::PropertyKey::String("globalThis".to_string().into()),
      crate::value::PropertyDescriptor::ordinary_data(Value::Object(Rc::clone(&globals))),
    );
    let mut vm = Vm::new(globals);
    drop(vm.run(chunk).expect("run ok"));

    assert!(
      matches!(vm.get_global("hits"), Some(Value::Number(n)) if (n - 1.0).abs() < f64::EPSILON),
      "unexpected hits value: {:?}",
      vm.get_global("hits")
    );
  }

  fn run_compiled(src: &str) -> Vm {
    let session = crate::parser::Parser::parse_source(src).expect("parse ok");
    let chunk = compile(session.program(), session.source()).expect("compile ok");
    let globals = Rc::new(RefCell::new(ObjectData::new()));
    globals.borrow_mut().properties.insert(
      crate::value::PropertyKey::String("globalThis".to_string().into()),
      crate::value::PropertyDescriptor::ordinary_data(Value::Object(Rc::clone(&globals))),
    );
    runtime_utils::install_minimal_kernel_constructors(&globals);
    let mut vm = Vm::new(globals);
    vm.run(chunk).expect("run ok");
    vm
  }

  #[test]
  fn object_literal_evaluates_mixed_properties_in_source_order() {
    let vm = run_compiled(crate::js!(
      r#"
        let order = "";
        function mark(label, value) { order += label; return value; }
        const source = { get copied() { order += "g"; return 1; } };
        const result = {
          first: mark("a", 1),
          get accessor() { return 2; },
          [mark("k", "computed")]: mark("v", 3),
          ...mark("s", source),
          last: mark("z", 4)
        };
        globalThis.object_order = order;
        globalThis.object_result = result.first + result.computed + result.copied + result.last;
        globalThis.object_accessor = Object.getOwnPropertyDescriptor(result, "accessor").get !== undefined;
      "#
    ));

    assert_eq!(
      vm.get_global("object_order"),
      Some(Value::String("akvsgz".into()))
    );
    assert_eq!(vm.get_global("object_result"), Some(Value::Number(9.0)));
    assert_eq!(vm.get_global("object_accessor"), Some(Value::Bool(true)));
  }

  #[test]
  fn template_interpolations_use_to_string_and_unary_plus_rejects_bigint() {
    let vm = run_compiled(crate::js!(
      r#"
        const template_result = `${1n}:${true}:${null}:${undefined}`;
        let unary_plus_threw = false;
        try { +1n; } catch (error) { unary_plus_threw = error instanceof TypeError; }
        globalThis.template_result = template_result;
        globalThis.unary_plus_threw = unary_plus_threw;
      "#
    ));

    assert_eq!(
      vm.get_global("template_result"),
      Some(Value::String("1:true:null:undefined".into()))
    );
    assert_eq!(vm.get_global("unary_plus_threw"), Some(Value::Bool(true)));
  }

  #[test]
  fn optional_calls_return_undefined_and_call_present_values() {
    let vm = run_compiled(crate::js!(
      r#"
        let calls = 0;
        function present() { calls += 1; return 7; }
        let absent;
        const direct = present?.();
        const missing = absent?.();
        const receiver = { value: 9, method() { return this.value; } };
        const member = receiver?.method();
        const missingMember = receiver?.missing?.();
        const missingComputed = receiver?.["missing"]?.();
        globalThis.optional_direct = direct;
        globalThis.optional_missing = missing;
        globalThis.optional_member = member;
        globalThis.optional_missing_member = missingMember;
        globalThis.optional_missing_computed = missingComputed;
        globalThis.optional_calls = calls;
      "#
    ));

    assert_eq!(vm.get_global("optional_direct"), Some(Value::Number(7.0)));
    assert_eq!(vm.get_global("optional_missing"), Some(Value::Undefined));
    assert_eq!(vm.get_global("optional_member"), Some(Value::Number(9.0)));
    assert_eq!(
      vm.get_global("optional_missing_member"),
      Some(Value::Undefined)
    );
    assert_eq!(
      vm.get_global("optional_missing_computed"),
      Some(Value::Undefined)
    );
    assert_eq!(vm.get_global("optional_calls"), Some(Value::Number(1.0)));
  }

  #[test]
  fn optional_spread_calls_expand_arguments_after_nullish_check() {
    let vm = run_compiled(crate::js!(
      r#"
        let evaluated = 0;
        function args() { evaluated += 1; return [10, 20, 40]; }
        function sum(a, b, c) { return a + b + c; }
        let absent;
        globalThis.optional_spread_sum = sum?.(...args());
        globalThis.optional_spread_missing = absent?.(...args());
        globalThis.optional_spread_evaluated = evaluated;
        globalThis.named_function_expression = (function named() {}?.name);
      "#
    ));

    assert_eq!(
      vm.get_global("optional_spread_sum"),
      Some(Value::Number(70.0))
    );
    assert_eq!(
      vm.get_global("optional_spread_missing"),
      Some(Value::Undefined)
    );
    assert_eq!(
      vm.get_global("optional_spread_evaluated"),
      Some(Value::Number(1.0))
    );
    assert_eq!(
      vm.get_global("named_function_expression"),
      Some(Value::String("named".into()))
    );
  }

  #[test]
  fn lexical_scopes_parameters_and_compiler_temporaries_are_not_leaked() {
    let vm = run_compiled(crate::js!(
      r#"
        let outer = 1;
        let block_tdz = false;
        try { { outer; let outer = 2; } } catch (_) { block_tdz = true; }
        function blockScope() { { let hidden = 1; } return typeof hidden; }
        for (let loopName = 0; loopName < 1; loopName++) {}
        switch (1) { case 1: break; }
        var parameterOuter = 42;
        function parameterScope(value = parameterOuter) {
          var parameterOuter = 1;
          return value;
        }
        let laterParameterTdz = false;
        try {
          function later(value = nested, { nested }) { return value; }
          later(undefined, { nested: 7 });
        } catch (_) { laterParameterTdz = true; }
        globalThis.block_tdz = block_tdz;
        globalThis.block_scope = blockScope();
        globalThis.loop_scope = typeof loopName;
        var __user_binding = Array.isArray;
        globalThis.temporary_scope = typeof __switch_discriminant_0;
        globalThis.double_underscore_binding = __user_binding([]);
        globalThis.parameter_scope = parameterScope();
        globalThis.later_parameter_tdz = laterParameterTdz;
      "#
    ));

    assert_eq!(vm.get_global("block_tdz"), Some(Value::Bool(true)));
    assert_eq!(
      vm.get_global("block_scope"),
      Some(Value::String("undefined".into()))
    );
    assert_eq!(
      vm.get_global("loop_scope"),
      Some(Value::String("undefined".into()))
    );
    assert_eq!(
      vm.get_global("temporary_scope"),
      Some(Value::String("undefined".into()))
    );
    assert_eq!(
      vm.get_global("double_underscore_binding"),
      Some(Value::Bool(true))
    );
    assert_eq!(vm.get_global("parameter_scope"), Some(Value::Number(42.0)));
    assert_eq!(
      vm.get_global("later_parameter_tdz"),
      Some(Value::Bool(true))
    );
  }

  #[test]
  fn strict_early_errors_cover_generator_parameters_and_catch_bindings() {
    for source in [
      crate::js!(r#"function* invalid(value = 1) { "use strict"; }"#),
      crate::js!(r#""use strict"; try { throw 1; } catch (eval) {}"#),
    ] {
      let session = crate::parser::Parser::parse_source(source).expect("parse ok");
      assert!(
        compile(session.program(), session.source()).is_err(),
        "{source}"
      );
    }
  }

  #[test]
  fn member_postfix_update_returns_old_value_and_stores_new_value() {
    let src = crate::js!(
      r#"
        var obj = { value: 1 };
        var old = obj.value++;
    "#
    );
    let session = crate::parser::Parser::parse_source(src).expect("parse ok");
    let program = session.program();
    let chunk = compile(program, session.source()).expect("compile ok");

    let globals = Rc::new(RefCell::new(ObjectData::new()));
    globals.borrow_mut().properties.insert(
      crate::value::PropertyKey::String("globalThis".to_string().into()),
      crate::value::PropertyDescriptor::ordinary_data(Value::Object(Rc::clone(&globals))),
    );
    let mut vm = Vm::new(globals);
    drop(vm.run(chunk).expect("run ok"));

    assert!(
      matches!(vm.get_global("old"), Some(Value::Number(n)) if (n - 1.0).abs() < f64::EPSILON)
    );
  }

  #[test]
  fn block_scoped_let_does_not_escape_block() {
    let src = crate::js!("{ let x = 1; } x = 2;");
    let session = crate::parser::Parser::parse_source(src).expect("parse ok");
    let program = session.program();
    let chunk = compile(program, session.source()).expect("compile ok");

    let globals = Rc::new(RefCell::new(ObjectData::new()));
    let mut vm = Vm::new(globals);
    vm.run(chunk).expect("run ok");

    assert!(
      matches!(vm.get_global("x"), Some(Value::Number(n)) if (n - 2.0).abs() < f64::EPSILON),
      "block-local binding should not leak past its block"
    );
  }
}
