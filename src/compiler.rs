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
/// Returns an error string if compilation fails
// Public API: compile an oxc AST Program. Internally we convert to the
// legacy owned AST and delegate to the existing compiler implementation.
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

    /// (legacy-owned AST entrypoints removed) The compiler exposes `compile_oxc`
    /// which consumes `oast::Program` directly. Previous owned-ast entrypoints
    /// were intentionally removed as part of the migration.

    /// Transitional compiler entry that accepts an `oast::Program` reference.
    ///
    /// Currently this converts to the legacy `local_ast::Program` and invokes
    /// the existing `compile(&Program)` implementation. We'll replace the
    /// body with a direct walker over `oast` in subsequent patches.
    pub fn compile_oxc(
        &mut self,
        program: &oast::Program<'_>,
    ) -> Result<Chunk, crate::errors::CompilerError> {
        // Start direct walking of the oxc program body. We iterate over
        // ModuleItems and compile their statement content where present.
        // This is a minimal but complete walker for the node kinds we need.
        // Note: ModuleDecl items (imports/exports) are treated as no-ops.
        // Use fully-qualified oxc ast alias `oast::ModuleItem` when matching.

        // Pre-declare functions for hoisting by scanning module items.
        self.pre_declare_functions_oast(program);

        for item in &program.body {
            match item {
                oast::ModuleItem::Stmt(stmt) => {
                    self.compile_statement_oast(stmt)?;
                }
                oast::ModuleItem::ModuleDecl(_) => {
                    // imports/exports skipped at compile-time
                }
            }
        }

        // After walking module, finish with Return
        self.emit(Op::Return, LINE);
        Ok(std::mem::take(&mut self.chunk))
    }

    /// Scan an oxc Program and reserve local slots for top-level function
    /// declarations to preserve hoisting semantics during the migration.
    fn pre_declare_functions_oast(&mut self, program: &oast::Program<'_>) {
        for item in &program.body {
            if let oast::ModuleItem::Stmt(stmt) = item {
                if let oast::Stmt::Decl(oast::Decl::Fn(fn_decl)) = stmt {
                    if let Some(ident) = &fn_decl.ident {
                        let name = ident.name.clone();
                        if self.resolve_local(&name).is_none() {
                            self.add_local(name, None);
                        }
                    }
                }
            }
        }
    }

    // Direct oxc AST statement/epression walkers
    fn compile_statement_oast(
        &mut self,
        stmt: &oast::Stmt,
    ) -> Result<(), crate::errors::CompilerError> {
        use oxc_ast::ast::{Decl as ODecl, Pat as OPat, Stmt as OStmt};
        match stmt {
            OStmt::Decl(d) => match d {
                ODecl::Var(v) => {
                    for dec in &v.decls {
                        if let Some(init) = &dec.init {
                            self.compile_expr_oast(init)?;
                        } else {
                            self.emit(Op::Null, LINE);
                        }
                        let name = match &dec.id {
                            OPat::Ident(i) => i.id.name.clone(),
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
                ODecl::Fn(f) => {
                    if let Some(ident) = &f.ident {
                        let fname = ident.name.clone();
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
                _ => {}
            },
            OStmt::Expr(e) => {
                self.compile_expr_oast(&e.expr)?;
                self.emit(Op::Pop, LINE);
            }
            OStmt::If(i) => {
                self.compile_expr_oast(&i.test)?;
                self.emit(Op::JumpIfFalse, LINE);
                let jump_over = self.chunk.code.len();
                self.emit_byte(0, LINE);
                self.emit_byte(0, LINE);
                self.compile_statement_oast(&i.cons)?;
                if let Some(alt) = &i.alt {
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
            OStmt::While(w) => {
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
            OStmt::Return(r) => {
                if let Some(arg) = &r.arg {
                    self.compile_expr_oast(arg)?;
                } else {
                    self.emit(Op::Null, LINE);
                }
                self.emit(Op::Return, LINE);
            }
            _ => {}
        }
        Ok(())
    }

    fn compile_expr_oast(&mut self, expr: &oast::Expr) -> Result<(), crate::errors::CompilerError> {
        use oxc_ast::ast::Expr as OExpr;
        match expr {
            OExpr::Lit(l) => match &l.kind {
                oast::LitKind::Number(n) => self.emit_constant(Value::Number(*n), LINE),
                oast::LitKind::String(s) => self.emit_constant(Value::String(s.clone()), LINE),
                oast::LitKind::Bool(b) => {
                    if *b {
                        self.emit(Op::True, LINE)
                    } else {
                        self.emit(Op::False, LINE)
                    }
                }
                oast::LitKind::Null => self.emit(Op::Null, LINE),
                _ => {}
            },
            OExpr::Ident(id) => {
                let name = &id.name;
                if let Some(slot) = self.resolve_local(name) {
                    self.emit(Op::GetLocal, LINE);
                    self.emit_byte(slot as u8, LINE);
                } else {
                    let idx = self.global_name_index(name);
                    self.emit(Op::GetGlobal, LINE);
                    self.emit_byte(idx as u8, LINE);
                }
            }
            OExpr::Unary(u) => {
                self.compile_expr_oast(&u.arg)?;
                match u.op {
                    oast::UnaryOp::Not => self.emit(Op::Not, LINE),
                    oast::UnaryOp::Neg => self.emit(Op::Neg, LINE),
                    _ => {}
                }
            }
            OExpr::Bin(b) => {
                self.compile_expr_oast(&b.lhs)?;
                self.compile_expr_oast(&b.rhs)?;
                match b.op {
                    oast::BinaryOp::Add => self.emit(Op::Add, LINE),
                    oast::BinaryOp::Sub => self.emit(Op::Sub, LINE),
                    oast::BinaryOp::Mul => self.emit(Op::Mul, LINE),
                    oast::BinaryOp::Div => self.emit(Op::Div, LINE),
                    _ => self.emit(Op::Add, LINE),
                }
            }
            _ => {}
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

    fn pre_declare_functions_oast_stmts(&mut self, stmts: &[oast::Stmt]) {
        for stmt in stmts {
            if let oast::Stmt::Decl(oast::Decl::Fn(f)) = stmt {
                if let Some(ident) = &f.ident {
                    let name = ident.name.clone();
                    if self.resolve_local(&name).is_none() {
                        self.add_local(name, None);
                    }
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
        for p in &func.params {
            let pname = match &p.pat {
                oast::Pat::Ident(i) => i.id.name.clone(),
                _ => "_".to_string(),
            };
            param_type_opts.push(None);
            inner.add_local(pname, None);
        }

        // Pre-declare nested functions inside this function body.
        if let Some(body) = &func.body {
            self.pre_declare_functions_oast_stmts(&body.body);
        }

        // Compile function body statements into inner compiler.
        if let Some(body) = &func.body {
            for stmt in &body.body {
                inner.compile_statement_oast(stmt)?;
            }
        }

        inner.emit(Op::Return, LINE);
        let chunk = std::mem::take(&mut inner.chunk);
        let slot_types: Vec<Option<Type>> = param_type_opts
            .into_iter()
            .chain(inner.local_types)
            .collect();
        let has_rest = func.params.iter().any(|p| p.rest);
        Ok((
            FunctionProto {
                name: name.to_string(),
                arity: func.params.len(),
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
