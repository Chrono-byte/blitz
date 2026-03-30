//! Core Compiler struct and foundational implementation.
//!
//! This module contains the basic structure and initialization logic for the compiler,
//! along with fundamental helper methods for scope management, local variable tracking,
//! and basic bytecode emission.

use crate::bytecode::{Chunk, Op, UpvalueSpec};
use crate::typecheck::Type;
use crate::value::Value;
use oxc_ast::ast as oast;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

const LINE: u32 = 1;

/// Module compilation context describing imports/exports for the module being compiled.
#[derive(Clone, Debug)]
pub struct ModuleContext {
    /// Numeric module index assigned during instantiation
    pub module_index: usize,
    /// Imports: local name -> (module_index, exported_name)
    pub imports: HashMap<String, (usize, String)>,
    /// Exported local names (set)
    pub exports: HashSet<String>,
}

/// Compiles AST to bytecode
pub struct Compiler {
    /// Current chunk being compiled
    pub chunk: Chunk,
    /// Original source text for this compilation unit
    pub source_text: Rc<String>,
    /// Local variable names
    pub locals: Vec<String>,
    /// Type annotations for local variables
    pub local_types: Vec<Option<Type>>,
    /// Per-local TDZ marker for lexical bindings.
    pub local_tdz: Vec<bool>,
    /// Whether writes to this local should mirror to the global object.
    pub local_mirror_globals: Vec<bool>,
    /// Lexical scope stack for name visibility.
    pub scopes: Vec<Scope>,
    /// Global variable name to constant index mapping
    pub globals: HashMap<String, usize>,
    /// True only for the top-level compiler (so we can record global_types).
    pub is_root: bool,
    /// Whether the current compilation context is strict.
    pub strict_context: bool,
    /// Current nested loop depth.
    pub loop_depth: usize,
    /// Stack of continue patch sites for nested loops. Each entry holds a list
    /// of code offsets (index of the first placeholder byte) that must be
    /// patched to jump to the loop's continue target when the loop is closed.
    pub continue_patches: Vec<Vec<usize>>,
    /// Stack of break patch sites for nested loops. Each entry holds a list of
    /// jump operands that must be patched to the loop exit target.
    pub break_patches: Vec<Vec<usize>>,
    /// Stack of active `try {}` blocks with `finally` clauses so `return`
    /// statements in the protected region can run the finalizer before
    /// unwinding.
    pub finally_patches: Vec<FinallyPatchState>,
    /// Stack of active labels for `break`/`continue` resolution.
    pub label_patches: Vec<LabelPatchState>,
    /// Optional module context when compiling a module
    pub current_module: Option<ModuleContext>,
    /// Monotonic inline-cache slot allocator for the current chunk.
    pub next_ic_slot: u8,
    /// Bindings visible from the immediately enclosing function.
    pub outer_bindings: HashMap<String, CaptureSource>,
    /// Upvalue indices allocated in this compiler.
    pub upvalue_indices: HashMap<String, u8>,
    /// Upvalue capture specs for the current function.
    pub captured_upvalues: Vec<UpvalueSpec>,
    /// Synthetic private-name mapping: private identifier -> synthetic key string
    /// The string is used as the constant value stored in each chunk that
    /// represents the hidden private key. We store the string so inner
    /// compilers can add the same constant into their own chunks.
    pub private_names: HashMap<String, String>,
    /// Tracks whether SetProperty was emitted in the current object literal.
    /// Used to determine if we need to emit a Pop after variable declaration.
    pub object_expr_had_set_property: bool,
}

/// Source of a captured variable in a closure.
#[derive(Clone, Copy, Debug)]
pub enum CaptureSource {
    /// Local variable captured from enclosing function scope.
    Local(u8),
    /// Upvalue captured from outer closure.
    Upvalue(u8),
}

/// Kind of lexical scope in the compiler.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScopeKind {
    /// Function scope.
    Function,
    /// Block scope.
    Block,
}

/// Lexical scope tracking bindings and visibility.
#[derive(Debug)]
pub struct Scope {
    /// Kind of this scope (function or block).
    pub kind: ScopeKind,
    /// Variable bindings in this scope: name -> local slot index.
    pub bindings: HashMap<String, usize>,
    /// Lexically-bound variable names (let/const) in this scope.
    pub lexical_bindings: HashSet<String>,
    /// Block-scoped var names collected from statements in this block.
    pub block_var_names: HashSet<String>,
}

/// State for patching return statements in finally blocks.
#[derive(Debug)]
pub struct FinallyPatchState {
    /// Local slot holding the return value.
    pub return_slot: usize,
    /// Local slot indicating whether a return was executed.
    pub did_return_slot: usize,
    /// List of jump offsets to patch for returns in the finally.
    pub return_jumps: Vec<usize>,
    /// Whether we are currently inside the finalizer.
    pub in_finalizer: bool,
}

/// State for patching labeled break/continue statements.
#[derive(Debug)]
pub struct LabelPatchState {
    /// Label name.
    pub name: String,
    /// Whether continue is allowed for this label.
    pub can_continue: bool,
    /// Loop depth when this label was created.
    pub owner_loop_depth: usize,
    /// List of continue jump offsets to patch.
    pub continue_patches: Vec<usize>,
    /// List of break jump offsets to patch.
    pub break_patches: Vec<usize>,
}

impl Compiler {
    /// Creates a new compiler instance
    pub fn new(source_text: Rc<String>) -> Self {
        Self {
            chunk: Chunk::new(Rc::clone(&source_text)),
            source_text,
            locals: Vec::new(),
            local_types: Vec::new(),
            local_tdz: Vec::new(),
            local_mirror_globals: Vec::new(),
            scopes: vec![Scope {
                kind: ScopeKind::Function,
                bindings: HashMap::new(),
                lexical_bindings: HashSet::new(),
                block_var_names: HashSet::new(),
            }],
            globals: HashMap::new(),
            is_root: true,
            strict_context: false,
            loop_depth: 0,
            continue_patches: Vec::new(),
            break_patches: Vec::new(),
            finally_patches: Vec::new(),
            label_patches: Vec::new(),
            current_module: None,
            next_ic_slot: 0,
            outer_bindings: HashMap::new(),
            upvalue_indices: HashMap::new(),
            captured_upvalues: Vec::new(),
            private_names: HashMap::new(),
            object_expr_had_set_property: false,
        }
    }

    pub(crate) fn emit(&mut self, op: Op, line: u32) {
        self.chunk.write(op as u8, line);
    }

    pub(crate) fn emit_byte(&mut self, byte: u8, line: u32) {
        self.chunk.write(byte, line);
    }

    pub(crate) fn emit_u16(&mut self, value: u16, line: u32) {
        self.emit_byte((value >> 8) as u8, line);
        self.emit_byte((value & 0xff) as u8, line);
    }

    pub(crate) fn emit_constant(&mut self, value: Value, line: u32) {
        let idx = self.chunk.add_constant(value);
        self.emit(Op::Const, line);
        let idx = u16::try_from(idx).expect("too many constants in chunk");
        self.emit_u16(idx, line);
    }

    /// Allocates a new inline cache slot for the current compilation chunk.
    ///
    /// # Errors
    ///
    /// Returns an error if the maximum number of inline cache sites has been exceeded.
    pub fn alloc_ic_slot(&mut self) -> Result<u8, crate::errors::CompilerError> {
        let slot = self.next_ic_slot;
        self.next_ic_slot = self
            .next_ic_slot
            .checked_add(1)
            .ok_or_else(|| crate::errors::CompilerError::from("Too many inline cache sites"))?;
        Ok(slot)
    }

    pub(crate) fn resolve_local(&self, name: &str) -> Option<usize> {
        self.scopes
            .iter()
            .rev()
            .find_map(|scope| scope.bindings.get(name).copied())
    }

    pub(crate) fn current_function_scope_index(&self) -> usize {
        self.scopes
            .iter()
            .rposition(|scope| scope.kind == ScopeKind::Function)
            .expect("compiler must always have a function scope")
    }

    pub(crate) fn add_local_in_scope(
        &mut self,
        scope_index: usize,
        name: String,
        type_ann: Option<Type>,
        mirror_global: bool,
    ) -> usize {
        if let Some(slot) = self.scopes[scope_index].bindings.get(&name).copied() {
            return slot;
        }
        let slot = self.locals.len();
        self.locals.push(name.clone());
        self.local_types.push(type_ann);
        self.local_tdz.push(false);
        self.local_mirror_globals.push(mirror_global);
        self.scopes[scope_index].bindings.insert(name, slot);
        slot
    }

    pub(crate) fn mark_local_tdz(&mut self, slot: usize) {
        if let Some(flag) = self.local_tdz.get_mut(slot) {
            *flag = true;
        }
    }

    pub(crate) fn add_lexical_binding(&mut self, name: &str) {
        let scope_index = self.scopes.len().saturating_sub(1);
        self.scopes[scope_index]
            .lexical_bindings
            .insert(name.to_string());
    }

    pub(crate) fn add_local_with_mirror(
        &mut self,
        name: String,
        type_ann: Option<Type>,
        mirror_global: bool,
    ) -> usize {
        let scope_index = self.scopes.len().saturating_sub(1);
        self.add_local_in_scope(scope_index, name, type_ann, mirror_global)
    }

    pub(crate) fn add_var_local(
        &mut self,
        name: String,
        type_ann: Option<Type>,
        mirror_global: bool,
    ) -> usize {
        let scope_index = self.current_function_scope_index();
        self.add_local_in_scope(scope_index, name, type_ann, mirror_global)
    }

    pub(crate) fn check_var_lexical_conflict(&self, name: &str) -> Option<String> {
        for scope in self.scopes.iter().rev() {
            if matches!(scope.kind, ScopeKind::Function) {
                break;
            }
            if scope.lexical_bindings.contains(name) {
                return Some(format!(
                    "SyntaxError: redeclaration of lexical binding `{}`",
                    name
                ));
            }
        }
        None
    }

    #[allow(dead_code)]
    pub(crate) fn check_var_or_lexical_against_block_vars(&self, name: &str) -> Option<String> {
        for scope in self.scopes.iter().rev() {
            if matches!(scope.kind, ScopeKind::Function) {
                break;
            }
            if scope.block_var_names.contains(name) {
                return Some(format!(
                    "SyntaxError: redeclaration of lexical binding `{}`",
                    name
                ));
            }
        }
        None
    }

    pub(crate) fn collect_var_names_from_statement(
        &self,
        stmt: &oast::Statement<'_>,
    ) -> HashSet<String> {
        let mut names = HashSet::new();
        self.add_var_names_from_statement(stmt, &mut names);
        names
    }

    pub(crate) fn add_var_names_from_statement(
        &self,
        stmt: &oast::Statement<'_>,
        names: &mut HashSet<String>,
    ) {
        match stmt {
            oast::Statement::VariableDeclaration(v) => {
                if matches!(v.kind, oxc_ast::ast::VariableDeclarationKind::Var) {
                    for decl in &v.declarations {
                        self.add_var_names_from_binding(&decl.id, names);
                    }
                }
            }
            oast::Statement::FunctionDeclaration(f) => {
                if let Some(id) = &f.id {
                    names.insert(id.name.to_string());
                }
            }
            oast::Statement::ClassDeclaration(c) => {
                if let Some(id) = &c.id {
                    names.insert(id.name.to_string());
                }
            }
            oast::Statement::BlockStatement(b) => {
                for s in &b.body {
                    self.add_var_names_from_statement(s, names);
                }
            }
            oast::Statement::IfStatement(i) => {
                self.add_var_names_from_statement(&i.consequent, names);
                if let Some(alt) = &i.alternate {
                    self.add_var_names_from_statement(alt, names);
                }
            }
            oast::Statement::LabeledStatement(l) => {
                self.add_var_names_from_statement(&l.body, names);
            }
            oast::Statement::WithStatement(w) => {
                self.add_var_names_from_statement(&w.body, names);
            }
            _ => {}
        }
    }

    pub(crate) fn add_var_names_from_binding(
        &self,
        pattern: &oast::BindingPattern<'_>,
        names: &mut HashSet<String>,
    ) {
        match pattern {
            oast::BindingPattern::BindingIdentifier(i) => {
                names.insert(i.name.to_string());
            }
            oast::BindingPattern::AssignmentPattern(a) => {
                self.add_var_names_from_binding(&a.left, names);
            }
            oast::BindingPattern::ObjectPattern(o) => {
                for prop in &o.properties {
                    self.add_var_names_from_binding(&prop.value, names);
                }
            }
            oast::BindingPattern::ArrayPattern(a) => {
                for elem in (&a.elements).into_iter().flatten() {
                    self.add_var_names_from_binding(elem, names);
                }
            }
        }
    }

    pub(crate) fn enter_block_scope(&mut self) {
        self.scopes.push(Scope {
            kind: ScopeKind::Block,
            bindings: HashMap::new(),
            lexical_bindings: HashSet::new(),
            block_var_names: HashSet::new(),
        });
    }

    pub(crate) fn enter_block_scope_with_vars(&mut self, stmts: &[oast::Statement<'_>]) {
        let mut block_var_names = HashSet::new();
        for stmt in stmts {
            let stmt_vars = self.collect_var_names_from_statement(stmt);
            block_var_names.extend(stmt_vars);
        }
        self.scopes.push(Scope {
            kind: ScopeKind::Block,
            bindings: HashMap::new(),
            lexical_bindings: HashSet::new(),
            block_var_names,
        });
    }

    pub(crate) fn exit_scope(&mut self) {
        let scope = self
            .scopes
            .pop()
            .expect("compiler must always have an active scope");
        debug_assert!(
            !matches!(scope.kind, ScopeKind::Function) || !self.scopes.is_empty(),
            "cannot pop the last function scope"
        );
    }

    pub(crate) fn visible_bindings_snapshot(&mut self) -> HashMap<String, CaptureSource> {
        let mut bindings = HashMap::new();
        for scope in &self.scopes {
            for (name, slot) in &scope.bindings {
                bindings.insert(name.clone(), CaptureSource::Local(*slot as u8));
            }
        }
        for (name, idx) in &self.upvalue_indices {
            bindings
                .entry(name.clone())
                .or_insert(CaptureSource::Upvalue(*idx));
        }
        // Allow pass-through capture for grandparent bindings needed by
        // deeper nested functions. Root bindings must remain capturable so
        // callbacks created inside a top-level function can still see the
        // surrounding module scope after that function returns.
        let outer_names: Vec<String> = self.outer_bindings.keys().cloned().collect();
        for name in outer_names {
            if bindings.contains_key(&name) {
                continue;
            }
            if let Some(idx) = self.resolve_upvalue(&name) {
                bindings.insert(name, CaptureSource::Upvalue(idx));
            }
        }
        bindings
    }

    pub(crate) fn local_mirrors_global(&self, slot: usize) -> bool {
        self.local_mirror_globals
            .get(slot)
            .copied()
            .unwrap_or(false)
    }

    pub(crate) fn emit_get_binding(&mut self, name: &str) {
        if let Some(slot) = self.resolve_local(name) {
            self.emit(Op::GetLocal, LINE);
            self.emit_byte(slot as u8, LINE);
        } else if let Some(upvalue) = self.resolve_upvalue(name) {
            self.emit(Op::GetUpvalue, LINE);
            self.emit_byte(upvalue, LINE);
        } else if let Some((src_idx, exported_name)) = self
            .current_module
            .as_ref()
            .and_then(|c| c.imports.get(name).map(|(i, n)| (*i, n.clone())))
        {
            let cname_idx = self.chunk.add_constant(Value::String(exported_name));
            self.emit(Op::GetModuleExport, LINE);
            self.emit_byte(src_idx as u8, LINE);
            self.emit_byte(cname_idx as u8, LINE);
        } else {
            self.emit_get_global_name(name);
        }
    }

    pub(crate) fn emit_set_own_module_export(
        &mut self,
        export_name: &str,
    ) -> Result<(), crate::errors::CompilerError> {
        let module_index = self
            .current_module
            .as_ref()
            .map(|c| c.module_index)
            .ok_or_else(|| crate::errors::CompilerError::from("module_index expected"))?;
        let cname_idx = self
            .chunk
            .add_constant(Value::String(export_name.to_string()));
        self.emit(Op::SetModuleExport, LINE);
        self.emit_byte(module_index as u8, LINE);
        self.emit_byte(cname_idx as u8, LINE);
        Ok(())
    }

    pub(crate) fn module_export_name_to_string(
        name: &oast::ModuleExportName<'_>,
    ) -> Option<String> {
        match name {
            oast::ModuleExportName::IdentifierName(id) => Some(id.name.to_string()),
            oast::ModuleExportName::IdentifierReference(id) => Some(id.name.to_string()),
            oast::ModuleExportName::StringLiteral(s) => Some(s.value.to_string()),
        }
    }

    pub(crate) fn resolve_upvalue(&mut self, name: &str) -> Option<u8> {
        if let Some(&idx) = self.upvalue_indices.get(name) {
            return Some(idx);
        }
        let source = *self.outer_bindings.get(name)?;
        let idx = u8::try_from(self.captured_upvalues.len()).ok()?;
        let spec = match source {
            CaptureSource::Local(slot) => UpvalueSpec {
                is_local: true,
                index: slot,
            },
            CaptureSource::Upvalue(upvalue) => UpvalueSpec {
                is_local: false,
                index: upvalue,
            },
        };
        self.captured_upvalues.push(spec);
        self.upvalue_indices.insert(name.to_string(), idx);
        Some(idx)
    }

    pub(crate) fn global_name_index(&mut self, name: &str) -> usize {
        if let Some(&idx) = self.globals.get(name) {
            return idx;
        }
        let idx = self.chunk.add_constant(Value::String(name.to_string()));
        self.globals.insert(name.to_string(), idx);
        idx
    }

    /// Ensure a synthetic private key exists for `name` and return it as a
    /// constant Value::String. The mapping is kept in `self.private_names` so
    /// repeated references to the same private identifier use the same key.
    pub(crate) fn ensure_private_name_constant(&mut self, name: &str) -> Value {
        if let Some(s) = self.private_names.get(name) {
            return Value::String(s.clone());
        }
        // Create a synthetic key that is unlikely to collide with public names.
        let key = format!("\u{1F}private#{}", name);
        self.private_names.insert(name.to_string(), key.clone());
        Value::String(key)
    }

    pub(crate) fn emit_get_global_name(&mut self, name: &str) {
        let idx = self.global_name_index(name);
        self.emit(Op::GetGlobal, LINE);
        self.emit_u16(idx as u16, LINE);
    }

    pub(crate) fn emit_set_global_name(&mut self, name: &str) {
        let idx = self.global_name_index(name);
        self.emit(Op::SetGlobal, LINE);
        self.emit_u16(idx as u16, LINE);
    }

    pub(crate) fn patch_jump(&mut self, offset: usize) {
        let jump = self.chunk.code.len() - offset - 2;
        self.chunk.code[offset] = (jump >> 8) as u8;
        self.chunk.code[offset + 1] = (jump & 0xff) as u8;
    }

    pub(crate) fn emit_short_signed(&mut self, val: isize, line: u32) {
        let v = val as i16;
        self.emit_byte((v >> 8) as u8, line);
        self.emit_byte((v & 0xff) as u8, line);
    }

    pub(crate) fn patch_jump_list(&mut self, patches: Vec<usize>, target: usize) {
        for patch_pos in patches {
            let offset = target as isize - (patch_pos as isize) - 2;
            let v = offset as i16;
            self.chunk.code[patch_pos] = (v >> 8) as u8;
            self.chunk.code[patch_pos + 1] = (v & 0xff) as u8;
        }
    }
}

impl Default for Compiler {
    fn default() -> Self {
        Self::new(Rc::new(String::new()))
    }
}
