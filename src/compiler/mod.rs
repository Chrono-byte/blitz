//! AST-to-bytecode compiler.
//!
//! The compiler transforms parsed TypeScript/JavaScript AST into executable
//! bytecode for the VM. It performs:
//! - Variable resolution (locals vs globals)
//! - Type annotation tracking for runtime checks
//! - Control flow compilation (if/else, while, try/catch, etc.)
//! - Function compilation with closure support
//!
//! # Architecture
//!
//! Compilation is single-pass over the AST:
//! 1. Pre-declare function names for hoisting support
//! 2. Compile statements sequentially
//! 3. Emit bytecode directly into a [`Chunk`]
//! 4. Track local variables and their types for runtime enforcement
//!
//! # Future Work
//!
//! - JIT compilation with multiple optimization passes
//! - Loop identification and optimization
//! - Dead code elimination
//! - Constant folding

use crate::bytecode::{Chunk, Op};
use oxc_ast::ast as oast;
use std::collections::HashSet;
use std::rc::Rc;

#[cfg(test)]
use {
    crate::{ObjectData, Value, Vm},
    std::cell::RefCell,
    std::collections::HashMap,
};

pub mod core;
pub(crate) use core::{
    Compiler, FinallyPatchState, LabelPatchState, ModuleContext, Scope, ScopeKind,
    with_compile_context_override,
};
mod binding;
mod declaration;
mod expression;
mod pre_declare;
mod tail;

const LINE: u32 = 1;

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
    let source_text = Rc::new(source.to_string());
    let mut c = Compiler::new(source_text.clone());
    c.compile_oxc(program, None)
}

/// Compile an oxc AST Program.
///
/// # Errors
///
/// Returns an error if compilation fails.
pub fn compile_oxc(
    program: &oast::Program<'_>,
    source: &str,
) -> Result<Chunk, crate::errors::CompilerError> {
    let source_text = Rc::new(source.to_string());
    let mut c = Compiler::new(source_text.clone());
    c.compile_oxc(program, None)
}

/// Compile an oxc AST Program with an optional ModuleContext.
///
/// # Errors
///
/// Returns an error if compilation fails.
pub fn compile_oxc_with_ctx(
    program: &oast::Program<'_>,
    ctx: Option<&ModuleContext>,
    source: &str,
) -> Result<Chunk, crate::errors::CompilerError> {
    let source_text = Rc::new(source.to_string());
    let mut c = Compiler::new(source_text.clone());
    c.compile_oxc(program, ctx)
}

/// Compile an oxc AST Program in REPL mode.
/// In REPL mode, the last expression statement does not emit Pop,
/// allowing the result to be returned for printing.
///
/// # Errors
///
/// Returns an error if compilation fails.
pub fn compile_oxc_repl(
    program: &oast::Program<'_>,
    source: &str,
) -> Result<Chunk, crate::errors::CompilerError> {
    let source_text = Rc::new(source.to_string());
    let mut c = Compiler::new(source_text.clone());
    c.compile_oxc_with_mode(program, None, true)
}

impl Compiler {
    fn lexical_declared_names_of_statement(stmt: &oast::Statement<'_>, out: &mut HashSet<String>) {
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

    fn collect_binding_names(pattern: &oast::BindingPattern<'_>, out: &mut HashSet<String>) {
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
            oast::Statement::WhileStatement(w) => {
                self.var_declared_names_of_statement(&w.body, out)
            }
            oast::Statement::DoWhileStatement(d) => {
                self.var_declared_names_of_statement(&d.body, out)
            }
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
            oast::Statement::LabeledStatement(l) => {
                self.var_declared_names_of_statement(&l.body, out)
            }
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
                    self.validate_early_errors_in_statements(
                        std::slice::from_ref(&i.consequent),
                        false,
                    )?;
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
                                    self.validate_early_errors_in_statements(
                                        &body.statements,
                                        false,
                                    )?;
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

    /// Transitional compiler entry that accepts an `oast::Program` reference.
    ///
    /// Uses a direct walker over `oast`.
    pub fn compile_oxc(
        &mut self,
        program: &oast::Program<'_>,
        ctx: Option<&ModuleContext>,
    ) -> Result<Chunk, crate::errors::CompilerError> {
        self.compile_oxc_with_mode(program, ctx, false)
    }

    /// Compile with explicit mode control.
    /// In REPL mode, the last expression statement does not emit Pop,
    /// allowing the result to be returned.
    pub fn compile_oxc_with_mode(
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

        for (i, stmt) in stmts.iter().enumerate() {
            let is_last_expr_stmt = repl_mode
                && i == stmts.len() - 1
                && matches!(stmt, oast::Statement::ExpressionStatement(_));
            self.compile_statement_oast_with_mode(stmt, is_last_expr_stmt, None)?;
        }

        self.emit(Op::Return, LINE);
        if self.is_root {
            self.chunk.root_slot_types = self.local_types.clone();
            self.chunk.root_slot_names = self.locals.iter().cloned().map(Some).collect();
            self.chunk.root_lexical_tdz = self.local_tdz.clone();
            self.chunk.root_strict = self.strict_context;
        }
        Ok(std::mem::take(&mut self.chunk))
    }
}

#[cfg(test)]
fn walk_chunk_ops(chunk: &Chunk) -> Vec<(usize, Op)> {
    let mut ops = Vec::new();
    let mut ip = 0usize;
    while ip < chunk.code.len() {
        if let Some(op) = Op::from_byte((chunk.code[ip] & 0xFF) as u8) {
            ops.push((ip, op));
            ip += opcode_width(op);
        } else {
            ip += 1;
        }
    }
    ops
}

#[test]
fn compile_simple_for_loop() {
    let src = "for (var i = 0; i < 3; i = i + 1) { console.log(i); }";
    let session = crate::parser::Parser::parse_source(src).expect("parse ok");
    let program = session.program();
    let chunk = compile(program, session.source()).expect("compile ok");

    // Ensure we emitted a JumpIfFalse (test) and at least one Jump (back/jump-over)
    let mut saw_jump_if_false = false;
    let mut saw_jump = false;
    for (_, op) in walk_chunk_ops(&chunk) {
        match op {
            Op::JumpIfFalse => saw_jump_if_false = true,
            Op::Jump => saw_jump = true,
            _ => {}
        }
    }

    assert!(
        saw_jump_if_false,
        "expected JumpIfFalse in compiled for-loop"
    );
    assert!(saw_jump, "expected Jump in compiled for-loop");
}

#[test]
fn compile_for_with_continue() {
    let src = "for (var i = 0; i < 3; i = i + 1) { if (i == 1) { continue; } console.log(i); }";
    let session = crate::parser::Parser::parse_source(src).expect("parse ok");
    let program = session.program();
    let chunk = compile(program, session.source()).expect("compile ok");

    // Expect multiple Jump opcodes: one for the test exit, one for the back-jump,
    // and at least one emitted for 'continue' which should be patched to jump to update.
    let mut jump_if_false = 0usize;
    let mut jump = 0usize;
    for (_, op) in walk_chunk_ops(&chunk) {
        match op {
            Op::JumpIfFalse => jump_if_false += 1,
            Op::Jump => jump += 1,
            _ => {}
        }
    }

    assert!(jump_if_false >= 1, "expected at least one JumpIfFalse");
    assert!(
        jump >= 2,
        "expected at least two Jump opcodes (back-jump and continue)"
    );
}

#[cfg(test)]
fn opcode_width(op: Op) -> usize {
    match op {
        Op::Const => 3,
        Op::GetGlobal | Op::SetGlobal | Op::GetGlobalIC => 3,
        Op::GetLocal
        | Op::SetLocal
        | Op::CloneLocalCell
        | Op::Call
        | Op::CallMethod
        | Op::CallIC
        | Op::TailCallIC
        | Op::TailCallMethod
        | Op::Construct
        | Op::PopN
        | Op::MakeClosure
        | Op::GetUpvalue
        | Op::SetUpvalue
        | Op::GetPropertyIC
        | Op::SetPropertyIC
        | Op::GetBuiltin
        | Op::BuildArray
        | Op::BuildObject
        | Op::Spread
        | Op::ArrayFromSpread => 2,
        Op::Jump | Op::JumpIfFalse | Op::JumpIfTrue | Op::JumpIfNullish | Op::PushHandler => 3,
        Op::CompareJumpIfFalse => 4,
        _ => 1,
    }
}

#[test]
fn static_member_ic_slots_are_unique_per_site() {
    let src = "a.x; a.y; a.x;";
    let session = crate::parser::Parser::parse_source(src).expect("parse ok");
    let program = session.program();
    let chunk = compile(program, session.source()).expect("compile ok");

    let mut slots = Vec::new();
    let mut i = 0usize;
    while i < chunk.code.len() {
        if let Some(op) = Op::from_byte((chunk.code[i] & 0xFF) as u8) {
            if matches!(op, Op::GetPropertyIC) {
                slots.push(chunk.code.get(i + 1).copied().expect("slot byte"));
            }
            i += opcode_width(op);
        } else {
            i += 1;
        }
    }

    assert_eq!(slots, vec![0, 1, 2]);
}

#[test]
fn compile_less_than_uses_lt_opcode() {
    let src = "for (var i = 0; i < 100; i = i + 1) {}";
    let session = crate::parser::Parser::parse_source(src).expect("parse ok");
    let program = session.program();
    let chunk = compile(program, session.source()).expect("compile ok");

    let saw_lt = chunk
        .code
        .iter()
        .filter_map(|b| Op::from_byte((*b & 0xFF) as u8))
        .any(|op| matches!(op, Op::Lt));

    assert!(
        saw_lt,
        "expected Lt opcode in compiled less-than expression"
    );
}

#[test]
fn compile_for_loop_back_jump_targets_loop_start() {
    let src = "for (var i = 0; i < 3; i = i + 1) {}";
    let session = crate::parser::Parser::parse_source(src).expect("parse ok");
    let program = session.program();
    let chunk = compile(program, session.source()).expect("compile ok");

    let mut loop_start = None;
    let mut back_target = None;
    for (ip, op) in walk_chunk_ops(&chunk) {
        match op {
            Op::GetGlobalIC if loop_start.is_none() => {
                loop_start = Some(ip);
            }
            Op::Jump => {
                let target = chunk.jump_target_from_ip(ip).map(|target| target as isize);
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
    let src = "new Array(1, 2, 3);";
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
    let src = "for (var i = 0; i < 3; i++) {}";
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
        .any(|op| matches!(op, Op::SetLocal));

    assert!(saw_add, "expected i++ to emit Add");
    assert!(saw_set_local, "expected i++ to emit SetLocal");
}

#[test]
fn compile_member_call_uses_call_method() {
    let src = "obj.method(1);";
    let session = match crate::parser::Parser::parse_source(src) {
        Ok(s) => s,
        // We allow panics here because the compiler error is expected, we can't recover if the user's code is invalid
        Err(e) => panic!("parse_source failed: {:?}", e),
    };
    let program = session.program();
    let chunk = match compile(program, session.source()) {
        Ok(c) => c,
        // We allow panics here because the compiler error is expected, we can't recover if the user's code is invalid
        Err(e) => panic!("compile failed: {:?}", e),
    };

    let saw_call_method = chunk
        .code
        .iter()
        .filter_map(|b| Op::from_byte((*b & 0xFF) as u8))
        .any(|op| matches!(op, Op::CallMethod));

    assert!(saw_call_method, "expected member call to emit CallMethod");
}

#[test]
fn compile_for_in_uses_get_builtin() {
    let src = "for (var k in obj) {}";
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
        "expected for-in lowering to emit GetBuiltin"
    );
}

#[test]
fn compile_generator_non_simple_params_keep_later_params_tdz() {
    let src = "function* f(x = y, y) {}";
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
        proto.lexical_tdz.get(y_slot).copied().unwrap_or(false),
        "later generator parameter y should start in TDZ"
    );

    let mut ops = Vec::new();
    let mut ip = 0usize;
    while ip < proto.chunk.code.len() && ops.len() < 16 {
        let op = Op::from_byte((proto.chunk.code[ip] & 0xFF) as u8).expect("valid opcode");
        let width = opcode_width(op);
        let operand = if matches!(op, Op::GetLocal | Op::SetLocal | Op::CloneLocalCell) {
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
    let src = "var o = { *m() { let x; eval(\"var x;\"); } };";
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
fn compile_dynamic_import_uses_get_builtin() {
    let src = "import('./dep.js');";
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
    let src = "var x = (a = 1, 2);";
    let session = crate::parser::Parser::parse_source(src)
        .unwrap_or_else(|e| panic!("parse_source failed: {:?}", e));
    let program = session.program();
    let chunk = compile(program, session.source()).expect("compile ok");

    let saw_pop = chunk
        .code
        .iter()
        .filter_map(|b| Op::from_byte((*b & 0xFF) as u8))
        .any(|op| matches!(op, Op::Pop));

    assert!(
        saw_pop,
        "expected sequence expression to emit Pop for intermediate values"
    );
}

#[test]
fn imported_binding_assignment_is_rejected() {
    let src = "a = 1;";
    let session = crate::parser::Parser::parse_source(src).expect("parse ok");
    let program = session.program();
    let ctx = ModuleContext {
        module_index: 0,
        imports: HashMap::from([(String::from("a"), (1usize, String::from("value")))]),
        exports: HashSet::new(),
    };

    let err = compile_oxc_with_ctx(program, Some(&ctx), session.source())
        .expect_err("compile should fail");
    assert!(
        err.to_string().contains("imported binding"),
        "unexpected error: {err}"
    );
}

#[test]
fn strict_assignment_to_arguments_is_rejected() {
    let src = r#"
        function testcase() {
            'use strict';
            arguments = 7;
        }
    "#;
    let session = crate::parser::Parser::parse_source(src).expect("parse ok");
    let program = session.program();

    let err = compile(program, session.source()).expect_err("compile should fail");
    assert!(
        err.to_string().contains("not allowed in strict mode"),
        "unexpected error: {err}"
    );
}

#[test]
fn nullish_coalescing_is_supported() {
    let src = "1 ?? 2;";
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
    let src = "if (a === b) { 1; } else { 2; } a === b ? 1 : 2;";
    let session = crate::parser::Parser::parse_source(src).expect("parse ok");
    let program = session.program();

    let chunk = compile(program, session.source()).expect("compile ok");
    let saw_compare_jump = chunk
        .code
        .iter()
        .filter_map(|b| Op::from_byte((*b & 0xFF) as u8))
        .any(|op| matches!(op, Op::CompareJumpIfFalse));
    assert!(
        saw_compare_jump,
        "expected comparison branching to emit CompareJumpIfFalse"
    );
}

#[test]
fn compile_for_of_emits_iterator_ops() {
    let src = "for (var x of [1, 2, 3]) {}";
    let session = crate::parser::Parser::parse_source(src).expect("parse ok");
    let program = session.program();
    let chunk = compile(program, session.source()).expect("compile ok");

    let ops: Vec<_> = chunk
        .code
        .iter()
        .filter_map(|b| Op::from_byte((*b & 0xFF) as u8))
        .collect();
    assert!(ops.iter().any(|op| matches!(op, Op::IteratorNext)));
    assert!(ops.iter().any(|op| matches!(op, Op::GetPropertyIC)));
}

#[test]
fn for_of_over_array_executes() {
    let src = "var sum = 0; for (var x of [1, 2, 3]) { sum = sum + x; } sum;";
    let session = crate::parser::Parser::parse_source(src).expect("parse ok");
    let program = session.program();
    let chunk = compile(program, session.source()).expect("compile ok");

    let globals = Rc::new(RefCell::new(ObjectData::new()));
    globals.borrow_mut().properties.insert(
        crate::value::PropertyKey::String("globalThis".to_string()),
        crate::value::PropertyDescriptor::data(Value::Object(globals.clone())),
    );
    let mut vm = Vm::new(globals.clone());
    let _ = vm.run(chunk).expect("run ok");

    assert!(
        matches!(vm.get_global("sum"), Some(Value::Number(n)) if (n - 6.0).abs() < f64::EPSILON)
    );
}

#[test]
fn computed_member_compound_assignment_evaluates_reference_once() {
    let src = r#"
        globalThis.hits = 0;
        var obj = { value: 1 };
        function key() { globalThis.hits = globalThis.hits + 1; return "value"; }
        obj[key()] += 2;
    "#;
    let session = crate::parser::Parser::parse_source(src).expect("parse ok");
    let program = session.program();
    let chunk = compile(program, session.source()).expect("compile ok");

    let globals = Rc::new(RefCell::new(ObjectData::new()));
    globals.borrow_mut().properties.insert(
        crate::value::PropertyKey::String("globalThis".to_string()),
        crate::value::PropertyDescriptor::data(Value::Object(globals.clone())),
    );
    let mut vm = Vm::new(globals.clone());
    let _ = vm.run(chunk).expect("run ok");

    assert!(
        matches!(vm.get_global("hits"), Some(Value::Number(n)) if (n - 1.0).abs() < f64::EPSILON),
        "unexpected hits value: {:?}",
        vm.get_global("hits")
    );
}

#[test]
fn member_postfix_update_returns_old_value_and_stores_new_value() {
    let src = r#"
        var obj = { value: 1 };
        var old = obj.value++;
    "#;
    let session = crate::parser::Parser::parse_source(src).expect("parse ok");
    let program = session.program();
    let chunk = compile(program, session.source()).expect("compile ok");

    let globals = Rc::new(RefCell::new(ObjectData::new()));
    globals.borrow_mut().properties.insert(
        crate::value::PropertyKey::String("globalThis".to_string()),
        crate::value::PropertyDescriptor::data(Value::Object(globals.clone())),
    );
    let mut vm = Vm::new(globals.clone());
    let _ = vm.run(chunk).expect("run ok");

    assert!(
        matches!(vm.get_global("old"), Some(Value::Number(n)) if (n - 1.0).abs() < f64::EPSILON)
    );
}

#[test]
fn block_scoped_let_does_not_escape_block() {
    let src = "{ let x = 1; } x = 2;";
    let session = crate::parser::Parser::parse_source(src).expect("parse ok");
    let program = session.program();
    let chunk = compile(program, session.source()).expect("compile ok");

    let globals = Rc::new(RefCell::new(ObjectData::new()));
    let mut vm = Vm::new(globals.clone());
    vm.run(chunk).expect("run ok");

    assert!(
        matches!(vm.get_global("x"), Some(Value::Number(n)) if (n - 2.0).abs() < f64::EPSILON),
        "block-local binding should not leak past its block"
    );
}
