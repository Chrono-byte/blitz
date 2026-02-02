//! Compile AST to bytecode.

use crate::ast::*;
use crate::bytecode::{Chunk, FunctionProto, Op};
use crate::value::Value;
use std::collections::HashMap;
use std::rc::Rc;

const LINE: u32 = 1;

/// Compile a parsed program to bytecode.
///
/// # Errors
///
/// Returns an error string if compilation fails
pub fn compile(program: &Program) -> Result<Chunk, String> {
    let mut c = Compiler::new();
    c.compile(program)
}

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

    /// Compiles a program to bytecode
    ///
    /// # Errors
    ///
    /// Returns an error string if compilation fails
    pub fn compile(&mut self, program: &Program) -> Result<Chunk, String> {
        for stmt in &program.body {
            self.compile_statement(stmt)?;
        }
        self.emit(Op::Return, LINE);
        Ok(std::mem::take(&mut self.chunk))
    }

    fn compile_statement(&mut self, stmt: &Statement) -> Result<(), String> {
        match stmt {
            Statement::VarDecl(v) => {
                if let Some(init) = &v.init {
                    self.compile_expr(init)?;
                } else {
                    self.emit(Op::Null, LINE);
                }
                let slot = self.add_local(v.name.clone(), v.type_ann.clone());
                if self.is_root {
                    self.chunk
                        .global_types
                        .insert(v.name.clone(), v.type_ann.clone());
                    self.emit(Op::Dup, LINE);
                }
                self.emit(Op::SetLocal, LINE);
                self.emit_byte(slot as u8, LINE);
                if self.is_root {
                    let idx = self.global_name_index(&v.name);
                    self.emit(Op::SetGlobal, LINE);
                    self.emit_byte(idx as u8, LINE);
                }
            }
            Statement::Declare(_d) => {
                // Ambient declaration: do not emit runtime initialization.
                // Type information is not needed at runtime here; the parser
                // records declarations so the typechecker can still see them.
            }
            Statement::Block(b) => {
                let start = self.locals.len();
                let start_types = self.local_types.len();
                for s in &b.body {
                    self.compile_statement(s)?;
                }
                self.locals.truncate(start);
                self.local_types.truncate(start_types);
            }
            Statement::If(i) => {
                self.compile_expr(&i.test)?;
                self.emit(Op::JumpIfFalse, LINE);
                let jump_over = self.chunk.code.len();
                self.emit_byte(0, LINE);
                self.emit_byte(0, LINE);
                self.compile_statement(&i.consequent)?;
                if let Some(alt) = &i.alternate {
                    self.emit(Op::Jump, LINE);
                    let jump_else = self.chunk.code.len();
                    self.emit_byte(0, LINE);
                    self.emit_byte(0, LINE);
                    self.patch_jump(jump_over);
                    self.compile_statement(alt)?;
                    self.patch_jump(jump_else);
                } else {
                    self.patch_jump(jump_over);
                }
            }
            Statement::Try(t) => {
                // Compile try block. Our VM uses a simple model: we emit the try
                // body followed by an unconditional jump past the catch. If a
                // catch handler is present, compile it into a new block that is
                // invoked when an exception is thrown. For now, exceptions are
                // represented by returning an error string from runtime calls and
                // we don't have full unwinding; implement a simplified try that
                // executes try block and then the finally block (if present).
                // Note: proper exception handling requires VM support; here we
                // compile the try body and simply compile catch/finally bodies
                // after it so source order is preserved. This is a placeholder
                // to enable parsing/compilation and will be expanded later.
                // Emit a handler: push handler with offset to catch block (if present)
                if let Some((catch_name, catch_block)) = &t.handler {
                    // Reserve PushHandler with offset to be emitted now
                    // We compute handler start as current code len + placeholder; emit PushHandler
                    self.emit(Op::PushHandler, LINE);
                    // placeholder for offset (2 bytes)
                    let handler_offset_pos = self.chunk.code.len();
                    self.emit_byte(0, LINE);
                    self.emit_byte(0, LINE);

                    // Compile try block
                    self.compile_statement(&Statement::Block(t.block.clone()))?;

                    // After try block, emit PopHandler and jump over catch
                    self.emit(Op::PopHandler, LINE);
                    self.emit(Op::Jump, LINE);
                    let jump_past_catch = self.chunk.code.len();
                    self.emit_byte(0, LINE);
                    self.emit_byte(0, LINE);

                    // Patch PushHandler offset to point to start of catch handler (next byte)
                    let handler_start =
                        self.chunk.code.len() as isize - (handler_offset_pos as isize) - 2;
                    let v = handler_start as i16;
                    self.chunk.code[handler_offset_pos] = (v >> 8) as u8;
                    self.chunk.code[handler_offset_pos + 1] = (v & 0xff) as u8;

                    // Compile catch block; the thrown value is left on the stack as the
                    // catch parameter by the VM. We must allocate a local slot for the
                    // catch parameter and emit a SetLocal to consume the thrown value
                    // into that local before compiling the catch body.
                    let start_locals = self.locals.len();
                    let start_types = self.local_types.len();
                    // create a local with the catch parameter name and Void type
                    let slot = self.add_local(catch_name.clone(), Some(Type::Void));
                    // Emit SetLocal to move thrown value (on stack) into the local slot
                    self.emit(Op::SetLocal, LINE);
                    self.emit_byte(slot as u8, LINE);
                    // Now compile the catch body; the catch param is available as a local
                    self.compile_statement(&Statement::Block(catch_block.clone()))?;
                    // restore locals after compiling catch
                    self.locals.truncate(start_locals);
                    self.local_types.truncate(start_types);

                    // Patch jump past catch to after catch
                    self.patch_jump(jump_past_catch);
                } else {
                    // No catch: just compile try body
                    self.compile_statement(&Statement::Block(t.block.clone()))?;
                }

                // Finally block (always runs after try/catch)
                if let Some(fin) = &t.finalizer {
                    self.compile_statement(&Statement::Block(fin.clone()))?;
                }
            }
            Statement::While(w) => {
                let loop_start = self.chunk.code.len();
                self.compile_expr(&w.test)?;
                self.emit(Op::JumpIfFalse, LINE);
                let exit_jump = self.chunk.code.len();
                self.emit_byte(0, LINE);
                self.emit_byte(0, LINE);
                self.compile_statement(&w.body)?;
                self.emit(Op::Jump, LINE);
                let back_offset = loop_start as isize - (self.chunk.code.len() + 3) as isize;
                self.emit_short_signed(back_offset, LINE);
                self.patch_jump(exit_jump);
            }
            Statement::FunctionDecl(f) => {
                let (proto, _env_len) =
                    self.compile_function(&f.name, &f.params, &f.body, f.return_type.clone())?;
                let idx = self.chunk.add_function(proto);
                self.emit(Op::MakeClosure, LINE);
                self.emit_byte(idx as u8, LINE);
                let fn_type = Type::Function(
                    f.params
                        .iter()
                        .map(|p| p.type_ann.clone().unwrap_or(Type::Void))
                        .collect(),
                    Box::new(f.return_type.clone().unwrap_or(Type::Void)),
                );
                let slot = self.add_local(f.name.clone(), Some(fn_type));
                self.emit(Op::SetLocal, LINE);
                self.emit_byte(slot as u8, LINE);
            }
            Statement::Return(r) => {
                if let Some(arg) = &r.argument {
                    self.compile_expr(arg)?;
                } else {
                    self.emit(Op::Null, LINE);
                }
                self.emit(Op::Return, LINE);
            }
            Statement::ExpressionStatement(e) => {
                self.compile_expr(&e.expression)?;
                // Assignment and similar leave nothing on stack; only pop when a value remains.
                if !matches!(&e.expression, Expr::Assignment(_)) {
                    self.emit(Op::Pop, LINE);
                }
            }
            Statement::Throw(t) => {
                // Compile throw: evaluate argument then emit Throw opcode
                self.compile_expr(&t.argument)?;
                self.emit(Op::Throw, LINE);
            }
        }
        Ok(())
    }

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

    fn compile_function(
        &mut self,
        name: &str,
        params: &[Param],
        body: &Block,
        return_type: Option<Type>,
    ) -> Result<(FunctionProto, usize), String> {
        let _env_len = self.locals.len();
        let mut inner = Compiler {
            chunk: Chunk::new(),
            locals: Vec::new(),
            local_types: Vec::new(),
            globals: HashMap::new(),
            is_root: false,
        };
        // Keep parameter type *options* so missing annotations are represented as None.
        let param_type_opts: Vec<Option<Type>> =
            params.iter().map(|p| p.type_ann.clone()).collect();
        for p in params {
            inner.add_local(p.name.clone(), p.type_ann.clone());
        }
        for stmt in &body.body {
            inner.compile_statement(stmt)?;
        }
        inner.emit(Op::Return, LINE);
        let chunk = std::mem::take(&mut inner.chunk);
        let slot_types: Vec<Option<Type>> = param_type_opts
            .into_iter()
            .chain(inner.local_types)
            .collect();
        let has_rest = params.iter().any(|p| p.rest);
        Ok((
            FunctionProto {
                name: name.to_string(),
                arity: params.len(),
                chunk: Rc::new(chunk),
                slot_types,
                has_rest,
                return_type,
            },
            _env_len,
        ))
    }

    fn compile_expr(&mut self, expr: &Expr) -> Result<(), String> {
        match expr {
            Expr::Literal(l) => {
                if let LiteralValue::String(s) = &l.value {
                    crate::trace::string("compiler LiteralValue::String", s);
                }
                let v = match &l.value {
                    LiteralValue::Number(n) => Value::Number(*n),
                    LiteralValue::String(s) => Value::String(s.clone()),
                    LiteralValue::Bool(b) => Value::Bool(*b),
                    LiteralValue::Null => Value::Null,
                };
                match &l.value {
                    LiteralValue::Null => self.emit(Op::Null, LINE),
                    LiteralValue::Bool(true) => self.emit(Op::True, LINE),
                    LiteralValue::Bool(false) => self.emit(Op::False, LINE),
                    _ => self.emit_constant(v, LINE),
                }
            }
            Expr::Identifier(ident) => {
                let name = &ident.name;
                if let Some(slot) = self.resolve_local(name) {
                    self.emit(Op::GetLocal, LINE);
                    self.emit_byte(slot as u8, LINE);
                } else {
                    let idx = self.global_name_index(name);
                    self.emit(Op::GetGlobal, LINE);
                    self.emit_byte(idx as u8, LINE);
                }
            }
            Expr::ThisExpression => {
                // Placeholder: evaluate `this` as `null` until `this` binding is implemented.
                self.emit(Op::Null, LINE);
            }
            Expr::BinaryExpr(b) => {
                self.compile_expr(&b.left)?;
                self.compile_expr(&b.right)?;
                let op = match b.operator {
                    BinaryOperator::Plus => {
                        // Heuristic: if both operands are numeric literals, use AddNumber.
                        if matches!(
                            &*b.left,
                            Expr::Literal(Literal {
                                value: LiteralValue::Number(_)
                            })
                        ) && matches!(
                            &*b.right,
                            Expr::Literal(Literal {
                                value: LiteralValue::Number(_)
                            })
                        ) {
                            Op::AddNumber
                        } else {
                            Op::Add
                        }
                    }
                    BinaryOperator::Minus => Op::Sub,
                    BinaryOperator::Star => Op::Mul,
                    BinaryOperator::Slash => Op::Div,
                    BinaryOperator::Percent => Op::Mod,
                    BinaryOperator::EqEq | BinaryOperator::EqEqEq => Op::Eq,
                    BinaryOperator::BangEq | BinaryOperator::BangEqEq => Op::Neq,
                    BinaryOperator::Lt => Op::Lt,
                    BinaryOperator::Le => Op::Le,
                    BinaryOperator::Gt => Op::Gt,
                    BinaryOperator::Ge => Op::Ge,
                    BinaryOperator::AndAnd => Op::And,
                    BinaryOperator::OrOr => Op::Or,
                };
                self.emit(op, LINE);
            }
            Expr::UnaryExpr(u) => {
                self.compile_expr(&u.argument)?;
                match u.operator {
                    UnaryOperator::Bang => self.emit(Op::Not, LINE),
                    UnaryOperator::Minus => self.emit(Op::Neg, LINE),
                    UnaryOperator::Typeof => self.emit(Op::Typeof, LINE),
                }
            }
            Expr::CallExpr(c) => {
                self.compile_expr(&c.callee)?;
                for arg in &c.args {
                    self.compile_expr(arg)?;
                }
                self.emit(Op::Call, LINE);
                self.emit_byte(c.args.len() as u8, LINE);
            }
            Expr::MemberExpr(m) => {
                // Evaluate the object first.
                self.compile_expr(&m.object)?;
                // For non-computed member access with an identifier property (e.g. `obj.prop`),
                // push the property name as a string constant instead of evaluating the
                // identifier expression (which would load a variable named `prop`).
                if !m.computed {
                    if let Expr::Identifier(ident) = &*m.property {
                        // Emit constant property key then use the inline-cache-aware opcode.
                        // For now use a single global IC slot based on the order of GetPropertyIC
                        // occurrences in the chunk. This simple scheme keeps the patch small and
                        // provides measurable benefit for repeated accesses.
                        self.emit_constant(Value::String(ident.name.clone()), LINE);
                        self.emit(Op::GetPropertyIC, LINE);
                        // Use a single-byte ic slot index equal to the current constant pool
                        // tail index modulo 256. This is a pragmatic choice: it keeps the
                        // representation byte-sized and avoids adding new compiler state for
                        // IC allocation in this patch. A later iteration should maintain a
                        // proper IC table in Chunk with allocation.
                        let ic_slot = (self.chunk.constants.len().saturating_sub(1) % 256) as u8;
                        self.emit_byte(ic_slot, LINE);
                    } else {
                        self.compile_expr(&m.property)?;
                        self.emit(Op::GetProperty, LINE);
                    }
                } else {
                    self.compile_expr(&m.property)?;
                    self.emit(Op::GetProperty, LINE);
                }
            }
            Expr::Assignment(a) => match &a.target {
                AssignmentTarget::Identifier(ident) => {
                    let name = &ident.name;
                    self.compile_expr(&a.value)?;
                    if let Some(slot) = self.resolve_local(name) {
                        self.emit(Op::SetLocal, LINE);
                        self.emit_byte(slot as u8, LINE);
                    } else {
                        let idx = self.global_name_index(name);
                        self.emit(Op::SetGlobal, LINE);
                        self.emit_byte(idx as u8, LINE);
                    }
                }
                AssignmentTarget::MemberExpr(m) => {
                    // For setting a property, evaluate object first, then property
                    // (as a constant string for non-computed identifier), then value.
                    self.compile_expr(&m.object)?;
                    if !m.computed {
                        if let Expr::Identifier(ident) = &*m.property {
                            self.emit_constant(Value::String(ident.name.clone()), LINE);
                        } else {
                            self.compile_expr(&m.property)?;
                        }
                    } else {
                        self.compile_expr(&m.property)?;
                    }
                    self.compile_expr(&a.value)?;
                    self.emit(Op::SetProperty, LINE);
                }
            },
            Expr::ArrayExpr(arr) => {
                for e in &arr.elements {
                    self.compile_expr(e)?;
                }
                self.emit(Op::BuildArray, LINE);
                self.emit_byte(arr.elements.len() as u8, LINE);
            }
            Expr::ObjectExpr(obj) => {
                for prop in &obj.properties {
                    self.emit_constant(Value::String(prop.key.clone()), LINE);
                    self.compile_expr(&prop.value)?;
                }
                self.emit(Op::BuildObject, LINE);
                self.emit_byte(obj.properties.len() as u8, LINE);
            }
            Expr::FunctionExpr(f) => {
                let (proto, _) =
                    self.compile_function("anonymous", &f.params, &f.body, f.return_type.clone())?;
                let idx = self.chunk.add_function(proto);
                self.emit(Op::MakeClosure, LINE);
                self.emit_byte(idx as u8, LINE);
            }
        }
        Ok(())
    }
}

impl Default for Compiler {
    fn default() -> Self {
        Self::new()
    }
}
