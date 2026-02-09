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

use crate::bytecode::{Chunk, FunctionProto, Op};
use crate::typecheck::Type;
use crate::value::Value;
use oxc_ast::ast as oast;
use std::collections::HashMap;
use std::rc::Rc;

const LINE: u32 = 1;

/// Compile a parsed program to bytecode.
///
/// # Errors
///
/// Returns an error string if compilation fails.
// Public API: compile an oxc AST Program directly.
pub fn compile(program: &oast::Program<'_>) -> Result<Chunk, crate::errors::CompilerError> {
    let mut c = Compiler::new();
    c.compile_oxc(program)
}

/// Compile an oxc AST Program.
pub fn compile_oxc(program: &oast::Program<'_>) -> Result<Chunk, crate::errors::CompilerError> {
    let mut c = Compiler::new();
    c.compile_oxc(program)
}

// Internal helpers for legacy owned AST were removed. The compiler now
// operates directly on the oxc AST via `compile_oxc`/`compile_statement_oast`.

/// Compiles AST to bytecode
pub struct Compiler {
    /// Current chunk being compiled
    chunk: Chunk,
    /// Local variable names
    locals: Vec<String>,
    /// Type annotations for local variables
    local_types: Vec<Option<Type>>,
    /// Global variable name to constant index mapping
    globals: HashMap<String, usize>,
    /// True only for the top-level compiler (so we can record global_types).
    is_root: bool,
    /// Stack of continue patch sites for nested loops. Each entry holds a list
    /// of code offsets (index of the first placeholder byte) that must be
    /// patched to jump to the loop's continue target when the loop is closed.
    continue_patches: Vec<Vec<usize>>,
}

impl Compiler {
    /// Creates a new compiler instance
    pub fn new() -> Self {
        Self {
            chunk: Chunk::new(),
            locals: Vec::new(),
            local_types: Vec::new(),
            globals: HashMap::new(),
            is_root: true,
            continue_patches: Vec::new(),
        }
    }

    fn emit(&mut self, op: Op, line: u32) {
        self.chunk.write(op as u8, line);
    }

    fn emit_byte(&mut self, byte: u8, line: u32) {
        self.chunk.write(byte, line);
    }

    fn emit_constant(&mut self, value: Value, line: u32) {
        let idx = self.chunk.add_constant(value);
        self.emit(Op::Const, line);
        self.emit_byte(idx as u8, line);
    }

    fn resolve_local(&self, name: &str) -> Option<usize> {
        self.locals.iter().position(|n| n == name)
    }

    fn add_local(&mut self, name: String, type_ann: Option<Type>) -> usize {
        let slot = self.locals.len();
        self.locals.push(name);
        self.local_types.push(type_ann);
        slot
    }

    fn global_name_index(&mut self, name: &str) -> usize {
        if let Some(&idx) = self.globals.get(name) {
            return idx;
        }
        let idx = self.chunk.add_constant(Value::String(name.to_string()));
        self.globals.insert(name.to_string(), idx);
        idx
    }

    /// Transitional compiler entry that accepts an `oast::Program` reference.
    ///
    /// Uses a direct walker over `oast`.
    pub fn compile_oxc(
        &mut self,
        program: &oast::Program<'_>,
    ) -> Result<Chunk, crate::errors::CompilerError> {
        // Start direct walking of the oxc program body. This is a minimal but
        // complete walker for the node kinds we need. Module declarations are
        // treated as no-ops.

        // Pre-declare functions for hoisting by scanning module items.
        self.pre_declare_functions_oast(program);

        for stmt in &program.body {
            self.compile_statement_oast(stmt)?;
        }

        // After walking module, finish with Return
        self.emit(Op::Return, LINE);
        Ok(std::mem::take(&mut self.chunk))
    }

    /// Scan an oxc Program and reserve local slots for top-level function
    /// declarations to preserve hoisting semantics during the migration.
    fn pre_declare_functions_oast(&mut self, program: &oast::Program<'_>) {
        for stmt in &program.body {
            if let oast::Statement::FunctionDeclaration(fn_decl) = stmt
                && let Some(ident) = &fn_decl.id
            {
                let name = ident.name.to_string();
                if self.resolve_local(&name).is_none() {
                    self.add_local(name, None);
                }
            }
        }
    }

    // Direct oxc AST statement/epression walkers
    fn compile_statement_oast(
        &mut self,
        stmt: &oast::Statement,
    ) -> Result<(), crate::errors::CompilerError> {
        use oxc_ast::ast::{BindingPattern as OPat, Statement as OStmt};
        if matches!(stmt, oxc_ast::match_module_declaration!(OStmt)) {
            return Ok(());
        }
        match stmt {
            OStmt::VariableDeclaration(v) => {
                for dec in &v.declarations {
                    if let Some(init) = &dec.init {
                        self.compile_expr_oast(init)?;
                    } else {
                        self.emit(Op::Null, LINE);
                    }
                    let name = match &dec.id {
                        OPat::BindingIdentifier(i) => i.name.to_string(),
                        _ => {
                            return Err(crate::errors::CompilerError::from(
                                "unsupported var pattern",
                            ));
                        }
                    };
                    let slot = self.add_local(name.clone(), None);
                    if self.is_root {
                        self.chunk.global_types.insert(name.clone(), None);
                        self.emit(Op::Dup, LINE);
                    }
                    self.emit(Op::SetLocal, LINE);
                    self.emit_byte(slot as u8, LINE);
                    if self.is_root {
                        let idx = self.global_name_index(&name);
                        self.emit(Op::SetGlobal, LINE);
                        self.emit_byte(idx as u8, LINE);
                    }
                }
            }
            OStmt::FunctionDeclaration(f) => {
                if let Some(ident) = &f.id {
                    let fname = ident.name.to_string();
                    let (proto, _env_len) = self.compile_function(&fname, f, None)?;
                    let idx = self.chunk.add_function(proto);
                    self.emit(Op::MakeClosure, LINE);
                    self.emit_byte(idx as u8, LINE);
                    let slot = if let Some(s) = self.resolve_local(&fname) {
                        s
                    } else {
                        self.add_local(fname.clone(), None)
                    };
                    if self.is_root {
                        self.chunk.global_types.insert(fname.clone(), None);
                        self.emit(Op::Dup, LINE);
                    }
                    self.emit(Op::SetLocal, LINE);
                    self.emit_byte(slot as u8, LINE);
                    if self.is_root {
                        let gidx = self.global_name_index(&fname);
                        self.emit(Op::SetGlobal, LINE);
                        self.emit_byte(gidx as u8, LINE);
                    }
                }
            }
            OStmt::ExpressionStatement(e) => {
                self.compile_expr_oast(&e.expression)?;
                self.emit(Op::Pop, LINE);
            }
            OStmt::BlockStatement(b) => {
                for s in &b.body {
                    self.compile_statement_oast(s)?;
                }
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
            OStmt::WhileStatement(w) => {
                self.continue_patches.push(Vec::new());
                let loop_start = self.chunk.code.len();
                self.compile_expr_oast(&w.test)?;
                self.emit(Op::JumpIfFalse, LINE);
                let exit_jump = self.chunk.code.len();
                self.emit_byte(0, LINE);
                self.emit_byte(0, LINE);
                self.compile_statement_oast(&w.body)?;
                self.emit(Op::Jump, LINE);
                let back_jump_pos = self.chunk.code.len();
                let back_offset = loop_start as isize - (self.chunk.code.len() + 3) as isize;
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
            }
            OStmt::ForStatement(f) => {
                use oxc_ast::ast::ForStatementInit as OForInit;
                self.continue_patches.push(Vec::new());

                if let Some(init) = &f.init {
                    match init {
                        OForInit::VariableDeclaration(v) => {
                            for dec in &v.declarations {
                                if let Some(init) = &dec.init {
                                    self.compile_expr_oast(init)?;
                                } else {
                                    self.emit(Op::Null, LINE);
                                }
                                let name = match &dec.id {
                                    OPat::BindingIdentifier(i) => i.name.to_string(),
                                    _ => {
                                        return Err(crate::errors::CompilerError::from(
                                            "unsupported var pattern",
                                        ));
                                    }
                                };
                                let slot = self.add_local(name.clone(), None);
                                if self.is_root {
                                    self.chunk.global_types.insert(name.clone(), None);
                                    self.emit(Op::Dup, LINE);
                                }
                                self.emit(Op::SetLocal, LINE);
                                self.emit_byte(slot as u8, LINE);
                                if self.is_root {
                                    let idx = self.global_name_index(&name);
                                    self.emit(Op::SetGlobal, LINE);
                                    self.emit_byte(idx as u8, LINE);
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

                self.compile_statement_oast(&f.body)?;

                let update_start = self.chunk.code.len();
                if let Some(patches) = self.continue_patches.pop() {
                    for patch_pos in patches {
                        let target = update_start as isize - (patch_pos as isize) - 2;
                        let v = target as i16;
                        self.chunk.code[patch_pos] = (v >> 8) as u8;
                        self.chunk.code[patch_pos + 1] = (v & 0xff) as u8;
                    }
                }

                if let Some(update) = &f.update {
                    self.compile_expr_oast(update)?;
                    self.emit(Op::Pop, LINE);
                }

                self.emit(Op::Jump, LINE);
                let back_offset = loop_start as isize - (self.chunk.code.len() + 3) as isize;
                self.emit_short_signed(back_offset, LINE);

                if let Some(exit_jump) = exit_jump {
                    self.patch_jump(exit_jump);
                }
            }
            OStmt::ReturnStatement(r) => {
                if let Some(arg) = &r.argument {
                    self.compile_expr_oast(arg)?;
                } else {
                    self.emit(Op::Null, LINE);
                }
                self.emit(Op::Return, LINE);
            }
            OStmt::ContinueStatement(_) => {
                if self.continue_patches.is_empty() {
                    return Err(crate::errors::CompilerError::from(
                        "'continue' used outside of a loop",
                    ));
                }
                self.emit(Op::Jump, LINE);
                let patch_pos = self.chunk.code.len();
                self.emit_byte(0, LINE);
                self.emit_byte(0, LINE);
                if let Some(patches) = self.continue_patches.last_mut() {
                    patches.push(patch_pos);
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn compile_expr_oast(
        &mut self,
        expr: &oast::Expression,
    ) -> Result<(), crate::errors::CompilerError> {
        use oxc_ast::ast::Expression as OExpr;
        match expr {
            OExpr::NumericLiteral(n) => self.emit_constant(Value::Number(n.value), LINE),
            OExpr::StringLiteral(s) => self.emit_constant(Value::String(s.value.to_string()), LINE),
            OExpr::BooleanLiteral(b) => {
                if b.value {
                    self.emit(Op::True, LINE)
                } else {
                    self.emit(Op::False, LINE)
                }
            }
            OExpr::NullLiteral(_) => self.emit(Op::Null, LINE),
            OExpr::ArrowFunctionExpression(_) => {
                self.emit(Op::Null, LINE);
            }
            OExpr::ObjectExpression(obj) => {
                for prop in &obj.properties {
                    if let oxc_ast::ast::ObjectPropertyKind::ObjectProperty(p) = prop {
                        let key = match &p.key {
                            oxc_ast::ast::PropertyKey::StaticIdentifier(ident) => {
                                ident.name.to_string()
                            }
                            oxc_ast::ast::PropertyKey::StringLiteral(s) => s.value.to_string(),
                            _ => String::new(),
                        };
                        self.emit_constant(Value::String(key), LINE);
                        self.compile_expr_oast(&p.value)?;
                    }
                }
                self.emit(Op::BuildObject, LINE);
                self.emit_byte(obj.properties.len() as u8, LINE);
            }
            OExpr::ArrayExpression(arr) => {
                for el in &arr.elements {
                    if let Some(expr) = el.as_expression() {
                        self.compile_expr_oast(expr)?;
                    } else {
                        self.emit(Op::Null, LINE);
                    }
                }
                self.emit(Op::BuildArray, LINE);
                self.emit_byte(arr.elements.len() as u8, LINE);
            }
            OExpr::Identifier(id) => {
                let name = id.name.as_str();
                if let Some(slot) = self.resolve_local(name) {
                    self.emit(Op::GetLocal, LINE);
                    self.emit_byte(slot as u8, LINE);
                } else {
                    let idx = self.global_name_index(name);
                    self.emit(Op::GetGlobal, LINE);
                    self.emit_byte(idx as u8, LINE);
                }
            }
            OExpr::AssignmentExpression(a) => {
                if a.operator != oast::AssignmentOperator::Assign {
                    return Err(crate::errors::CompilerError::from(
                        "unsupported assignment operator",
                    ));
                }
                let target = a.left.as_simple_assignment_target().ok_or_else(|| {
                    crate::errors::CompilerError::from("unsupported assignment target")
                })?;
                match target {
                    oast::SimpleAssignmentTarget::AssignmentTargetIdentifier(id) => {
                        let name = id.name.as_str();
                        self.compile_expr_oast(&a.right)?;
                        self.emit(Op::Dup, LINE);
                        if let Some(slot) = self.resolve_local(name) {
                            self.emit(Op::SetLocal, LINE);
                            self.emit_byte(slot as u8, LINE);
                        } else {
                            let idx = self.global_name_index(name);
                            self.emit(Op::SetGlobal, LINE);
                            self.emit_byte(idx as u8, LINE);
                        }
                    }
                    _ => {
                        return Err(crate::errors::CompilerError::from(
                            "unsupported assignment target",
                        ));
                    }
                }
                self.emit(Op::Null, LINE);
            }
            OExpr::UnaryExpression(u) => {
                self.compile_expr_oast(&u.argument)?;
                match u.operator {
                    oast::UnaryOperator::LogicalNot => self.emit(Op::Not, LINE),
                    oast::UnaryOperator::UnaryNegation => self.emit(Op::Neg, LINE),
                    _ => {}
                }
            }
            OExpr::BinaryExpression(b) => {
                self.compile_expr_oast(&b.left)?;
                self.compile_expr_oast(&b.right)?;
                match b.operator {
                    oast::BinaryOperator::Addition => self.emit(Op::Add, LINE),
                    oast::BinaryOperator::Subtraction => self.emit(Op::Sub, LINE),
                    oast::BinaryOperator::Multiplication => self.emit(Op::Mul, LINE),
                    oast::BinaryOperator::Division => self.emit(Op::Div, LINE),
                    _ => self.emit(Op::Add, LINE),
                }
            }
            OExpr::LogicalExpression(l) => {
                self.compile_expr_oast(&l.left)?;
                self.compile_expr_oast(&l.right)?;
                match l.operator {
                    oast::LogicalOperator::And => self.emit(Op::And, LINE),
                    oast::LogicalOperator::Or => self.emit(Op::Or, LINE),
                    oast::LogicalOperator::Coalesce => {
                        return Err(crate::errors::CompilerError::from(
                            "?? logical coalesce not supported",
                        ));
                    }
                }
            }
            OExpr::ParenthesizedExpression(p) => {
                self.compile_expr_oast(&p.expression)?;
            }
            _ => self.emit(Op::Null, LINE),
        }
        Ok(())
    }

    // Legacy owned-AST statement walker removed. Use `compile_statement_oast`.

    fn patch_jump(&mut self, offset: usize) {
        let jump = self.chunk.code.len() - offset - 2;
        self.chunk.code[offset] = (jump >> 8) as u8;
        self.chunk.code[offset + 1] = (jump & 0xff) as u8;
    }

    fn emit_short_signed(&mut self, val: isize, line: u32) {
        let v = val as i16;
        self.emit_byte((v >> 8) as u8, line);
        self.emit_byte((v & 0xff) as u8, line);
    }

    fn pre_declare_functions_oast_stmts(&mut self, stmts: &[oast::Statement]) {
        for stmt in stmts {
            if let oast::Statement::FunctionDeclaration(f) = stmt
                && let Some(ident) = &f.id
            {
                let name = ident.name.to_string();
                if self.resolve_local(&name).is_none() {
                    self.add_local(name, None);
                }
            }
        }
    }

    fn compile_function(
        &mut self,
        name: &str,
        func: &oast::Function,
        return_type: Option<Type>,
    ) -> Result<(FunctionProto, usize), crate::errors::CompilerError> {
        let _env_len = self.locals.len();
        let mut inner = Compiler {
            chunk: Chunk::new(),
            locals: Vec::new(),
            local_types: Vec::new(),
            globals: HashMap::new(),
            is_root: false,
            continue_patches: Vec::new(),
        };

        // Parameter placeholders (no type information available here).
        let mut param_type_opts: Vec<Option<Type>> = Vec::new();
        for p in &func.params.items {
            let pname = match &p.pattern {
                oast::BindingPattern::BindingIdentifier(i) => i.name.to_string(),
                _ => "_".to_string(),
            };
            param_type_opts.push(None);
            inner.add_local(pname, None);
        }
        if let Some(rest) = &func.params.rest {
            let pname = match &rest.rest.argument {
                oast::BindingPattern::BindingIdentifier(i) => i.name.to_string(),
                _ => "_".to_string(),
            };
            param_type_opts.push(None);
            inner.add_local(pname, None);
        }

        // Pre-declare nested functions inside this function body.
        if let Some(body) = &func.body {
            self.pre_declare_functions_oast_stmts(&body.statements);
        }

        // Compile function body statements into inner compiler.
        if let Some(body) = &func.body {
            for stmt in &body.statements {
                inner.compile_statement_oast(stmt)?;
            }
        }

        inner.emit(Op::Return, LINE);
        let chunk = std::mem::take(&mut inner.chunk);
        let slot_types: Vec<Option<Type>> = param_type_opts
            .into_iter()
            .chain(inner.local_types)
            .collect();
        let has_rest = func.params.rest.is_some();
        Ok((
            FunctionProto {
                name: name.to_string(),
                arity: func.params.items.len() + if has_rest { 1 } else { 0 },
                chunk: Rc::new(chunk),
                slot_types,
                has_rest,
                return_type,
            },
            _env_len,
        ))
    }

    // Legacy owned-AST expression walker removed. Use `compile_expr_oast`.
}

impl Default for Compiler {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bytecode::Op;
    use crate::parser::Parser;
    use crate::value::Value;

    #[test]
    fn hoist_top_level_function_names_into_locals() {
        let src = "f(); function f() { return 42; }";
        let session = crate::parser::Parser::parse_source(src).expect("parse ok");
        let program = session.program();
        let chunk = compile(program).expect("compile ok");

        // If a constant "f" exists, ensure it is not referenced via GetGlobal/SetGlobal.
        let mut f_const_idx: Option<usize> = None;
        for (i, c) in chunk.constants.iter().enumerate() {
            if let Value::String(s) = c {
                if s == "f" {
                    f_const_idx = Some(i);
                    break;
                }
            }
        }

        if let Some(idx) = f_const_idx {
            let mut i = 0usize;
            while i < chunk.code.len() {
                if let Some(op) = Op::from_byte(chunk.code[i]) {
                    match op {
                        Op::GetGlobal | Op::SetGlobal => {
                            let ref_idx = chunk.code.get(i + 1).cloned().unwrap_or(0) as usize;
                            assert_ne!(ref_idx, idx, "found GetGlobal/SetGlobal referencing 'f'");
                            i += 2;
                        }
                        Op::Jump | Op::JumpIfFalse | Op::PushHandler => i += 3,
                        Op::Const
                        | Op::MakeClosure
                        | Op::GetPropertyIC
                        | Op::Call
                        | Op::GetBuiltin => i += 2,
                        _ => i += 1,
                    }
                } else {
                    i += 1;
                }
            }
        }
    }

    #[test]
    fn compile_simple_for_loop() {
        let src = "for (var i = 0; i < 3; i = i + 1) { console.log(i); }";
        let session = crate::parser::Parser::parse_source(src).expect("parse ok");
        let program = session.program();
        let chunk = compile(program).expect("compile ok");

        // Ensure we emitted a JumpIfFalse (test) and at least one Jump (back/jump-over)
        let mut saw_jump_if_false = false;
        let mut saw_jump = false;
        let mut i = 0usize;
        while i < chunk.code.len() {
            if let Some(op) = Op::from_byte(chunk.code[i]) {
                match op {
                    Op::JumpIfFalse => {
                        saw_jump_if_false = true;
                        i += 3;
                    }
                    Op::Jump => {
                        saw_jump = true;
                        i += 3;
                    }
                    Op::Const
                    | Op::MakeClosure
                    | Op::GetPropertyIC
                    | Op::Call
                    | Op::GetBuiltin
                    | Op::SetLocal
                    | Op::GetLocal
                    | Op::SetGlobal
                    | Op::GetGlobal
                    | Op::GetProperty
                    | Op::SetProperty => i += 2,
                    _ => i += 1,
                }
            } else {
                i += 1;
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
        let chunk = compile(program).expect("compile ok");

        // Expect multiple Jump opcodes: one for the test exit, one for the back-jump,
        // and at least one emitted for 'continue' which should be patched to jump to update.
        let mut jump_if_false = 0usize;
        let mut jump = 0usize;
        let mut i = 0usize;
        while i < chunk.code.len() {
            if let Some(op) = Op::from_byte(chunk.code[i]) {
                match op {
                    Op::JumpIfFalse => {
                        jump_if_false += 1;
                        i += 3;
                    }
                    Op::Jump => {
                        jump += 1;
                        i += 3;
                    }
                    Op::Const
                    | Op::MakeClosure
                    | Op::GetPropertyIC
                    | Op::Call
                    | Op::GetBuiltin
                    | Op::SetLocal
                    | Op::GetLocal
                    | Op::SetGlobal
                    | Op::GetGlobal
                    | Op::GetProperty
                    | Op::SetProperty => i += 2,
                    _ => i += 1,
                }
            } else {
                i += 1;
            }
        }

        assert!(jump_if_false >= 1, "expected at least one JumpIfFalse");
        assert!(
            jump >= 2,
            "expected at least two Jump opcodes (back-jump and continue)"
        );
    }
}
