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

use crate::bytecode::{Chunk, FunctionProto, Op, UpvalueSpec};
use crate::typecheck::Type;
use crate::value::Value;
use oxc_ast::ast as oast;
use std::collections::HashMap;
use std::rc::Rc;

/// Module compilation context describing imports/exports for the module being compiled.
#[derive(Clone, Debug)]
pub struct ModuleContext {
    /// Numeric module index assigned during instantiation
    pub module_index: usize,
    /// Imports: local name -> (module_index, exported_name)
    pub imports: HashMap<String, (usize, String)>,
    /// Exported local names (set)
    pub exports: std::collections::HashSet<String>,
}

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
    let mut c = Compiler::new(Rc::clone(&source_text));
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
    let mut c = Compiler::new(Rc::clone(&source_text));
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
    let mut c = Compiler::new(Rc::clone(&source_text));
    c.compile_oxc(program, ctx)
}

// Internal helpers for legacy owned AST were removed. The compiler now
// operates directly on the oxc AST via `compile_oxc`/`compile_statement_oast`.

/// Compiles AST to bytecode
pub struct Compiler {
    /// Current chunk being compiled
    chunk: Chunk,
    /// Original source text for this compilation unit
    source_text: Rc<String>,
    /// Local variable names
    locals: Vec<String>,
    /// Type annotations for local variables
    local_types: Vec<Option<Type>>,
    /// Per-local TDZ marker for lexical bindings.
    local_tdz: Vec<bool>,
    /// Whether writes to this local should mirror to the global object.
    local_mirror_globals: Vec<bool>,
    /// Lexical scope stack for name visibility.
    scopes: Vec<Scope>,
    /// Global variable name to constant index mapping
    globals: HashMap<String, usize>,
    /// True only for the top-level compiler (so we can record global_types).
    is_root: bool,
    /// Whether the current compilation context is strict.
    strict_context: bool,
    /// Current nested loop depth.
    loop_depth: usize,
    /// Stack of continue patch sites for nested loops. Each entry holds a list
    /// of code offsets (index of the first placeholder byte) that must be
    /// patched to jump to the loop's continue target when the loop is closed.
    continue_patches: Vec<Vec<usize>>,
    /// Stack of break patch sites for nested loops. Each entry holds a list of
    /// jump operands that must be patched to the loop exit target.
    break_patches: Vec<Vec<usize>>,
    /// Stack of active `try {}` blocks with `finally` clauses so `return`
    /// statements in the protected region can run the finalizer before
    /// unwinding.
    finally_patches: Vec<FinallyPatchState>,
    /// Stack of active labels for `break`/`continue` resolution.
    label_patches: Vec<LabelPatchState>,
    /// Optional module context when compiling a module
    current_module: Option<ModuleContext>,
    /// Monotonic inline-cache slot allocator for the current chunk.
    next_ic_slot: u8,
    /// Bindings visible from the immediately enclosing function.
    outer_bindings: HashMap<String, CaptureSource>,
    /// Upvalue indices allocated in this compiler.
    upvalue_indices: HashMap<String, u8>,
    /// Upvalue capture specs for the current function.
    captured_upvalues: Vec<UpvalueSpec>,
    /// Synthetic private-name mapping: private identifier -> synthetic key string
    /// The string is used as the constant value stored in each chunk that
    /// represents the hidden private key. We store the string so inner
    /// compilers can add the same constant into their own chunks.
    private_names: HashMap<String, String>,
}

#[derive(Clone, Copy, Debug)]
enum CaptureSource {
    Local(u8),
    Upvalue(u8),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ScopeKind {
    Function,
    Block,
}

#[derive(Debug)]
struct Scope {
    kind: ScopeKind,
    bindings: HashMap<String, usize>,
}

#[derive(Debug)]
struct FinallyPatchState {
    return_slot: usize,
    did_return_slot: usize,
    return_jumps: Vec<usize>,
    in_finalizer: bool,
}

#[derive(Debug)]
struct LabelPatchState {
    name: String,
    can_continue: bool,
    owner_loop_depth: usize,
    continue_patches: Vec<usize>,
    break_patches: Vec<usize>,
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
        }
    }

    fn emit(&mut self, op: Op, line: u32) {
        self.chunk.write(op as u8, line);
    }

    fn emit_byte(&mut self, byte: u8, line: u32) {
        self.chunk.write(byte, line);
    }

    fn emit_u16(&mut self, value: u16, line: u32) {
        self.emit_byte((value >> 8) as u8, line);
        self.emit_byte((value & 0xff) as u8, line);
    }

    fn emit_constant(&mut self, value: Value, line: u32) {
        let idx = self.chunk.add_constant(value);
        self.emit(Op::Const, line);
        let idx = u16::try_from(idx).expect("too many constants in chunk");
        self.emit_u16(idx, line);
    }

    fn alloc_ic_slot(&mut self) -> Result<u8, crate::errors::CompilerError> {
        let slot = self.next_ic_slot;
        self.next_ic_slot = self
            .next_ic_slot
            .checked_add(1)
            .ok_or_else(|| crate::errors::CompilerError::from("Too many inline cache sites"))?;
        Ok(slot)
    }

    fn resolve_local(&self, name: &str) -> Option<usize> {
        self.scopes
            .iter()
            .rev()
            .find_map(|scope| scope.bindings.get(name).copied())
    }

    fn current_function_scope_index(&self) -> usize {
        self.scopes
            .iter()
            .rposition(|scope| scope.kind == ScopeKind::Function)
            .expect("compiler must always have a function scope")
    }

    fn add_local_in_scope(
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

    fn mark_local_tdz(&mut self, slot: usize) {
        if let Some(flag) = self.local_tdz.get_mut(slot) {
            *flag = true;
        }
    }

    fn add_local_with_mirror(
        &mut self,
        name: String,
        type_ann: Option<Type>,
        mirror_global: bool,
    ) -> usize {
        let scope_index = self.scopes.len().saturating_sub(1);
        self.add_local_in_scope(scope_index, name, type_ann, mirror_global)
    }

    fn add_var_local(
        &mut self,
        name: String,
        type_ann: Option<Type>,
        mirror_global: bool,
    ) -> usize {
        let scope_index = self.current_function_scope_index();
        self.add_local_in_scope(scope_index, name, type_ann, mirror_global)
    }

    fn enter_block_scope(&mut self) {
        self.scopes.push(Scope {
            kind: ScopeKind::Block,
            bindings: HashMap::new(),
        });
    }

    fn exit_scope(&mut self) {
        let scope = self
            .scopes
            .pop()
            .expect("compiler must always have an active scope");
        debug_assert!(
            !matches!(scope.kind, ScopeKind::Function) || !self.scopes.is_empty(),
            "cannot pop the last function scope"
        );
    }

    fn visible_bindings_snapshot(&mut self) -> HashMap<String, CaptureSource> {
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

    fn local_mirrors_global(&self, slot: usize) -> bool {
        self.local_mirror_globals
            .get(slot)
            .copied()
            .unwrap_or(false)
    }

    fn emit_get_binding(&mut self, name: &str) {
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

    fn emit_set_own_module_export(
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

    fn module_export_name_to_string(name: &oast::ModuleExportName<'_>) -> Option<String> {
        match name {
            oast::ModuleExportName::IdentifierName(id) => Some(id.name.to_string()),
            oast::ModuleExportName::IdentifierReference(id) => Some(id.name.to_string()),
            oast::ModuleExportName::StringLiteral(s) => Some(s.value.to_string()),
        }
    }

    fn resolve_upvalue(&mut self, name: &str) -> Option<u8> {
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

    fn global_name_index(&mut self, name: &str) -> usize {
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
    fn ensure_private_name_constant(&mut self, name: &str) -> Value {
        if let Some(s) = self.private_names.get(name) {
            return Value::String(s.clone());
        }
        // Create a synthetic key that is unlikely to collide with public names.
        let key = format!("\u{1F}private#{}", name);
        self.private_names.insert(name.to_string(), key.clone());
        Value::String(key)
    }

    fn emit_get_global_name(&mut self, name: &str) {
        let idx = self.global_name_index(name);
        self.emit(Op::GetGlobal, LINE);
        self.emit_u16(idx as u16, LINE);
    }

    fn emit_set_global_name(&mut self, name: &str) {
        let idx = self.global_name_index(name);
        self.emit(Op::SetGlobal, LINE);
        self.emit_u16(idx as u16, LINE);
    }

    /// Transitional compiler entry that accepts an `oast::Program` reference.
    ///
    /// Uses a direct walker over `oast`.
    pub fn compile_oxc(
        &mut self,
        program: &oast::Program<'_>,
        ctx: Option<&ModuleContext>,
    ) -> Result<Chunk, crate::errors::CompilerError> {
        // Start direct walking of the oxc program body. This is a minimal but
        // complete walker for the node kinds we need. Module declarations are
        // treated as no-ops.

        // Pre-declare bindings before hoisted function compilation so function
        // bodies can capture later lexical declarations.
        self.pre_declare_functions_oast(program);
        self.pre_declare_variables_oast(program);

        // Attach ModuleContext for compilation to consult when resolving
        // imports/exports. We store a clone so the compiler can borrow it
        // throughout the compile pass.
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

        self.emit_hoisted_function_decls_oast_stmts(&program.body)?;

        for stmt in &program.body {
            if matches!(stmt, oast::Statement::FunctionDeclaration(_)) {
                continue;
            }
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
                    self.add_local_in_scope(self.current_function_scope_index(), name, None, false);
                }
            }
        }
    }

    fn pre_declare_variables_oast(&mut self, program: &oast::Program<'_>) {
        self.pre_declare_variables_oast_stmts(&program.body);
    }

    fn pre_declare_variables_oast_stmts(&mut self, stmts: &[oast::Statement]) {
        use oxc_ast::ast::BindingPattern as OPat;
        for stmt in stmts {
            if let oast::Statement::VariableDeclaration(v) = stmt {
                for dec in &v.declarations {
                    let name = match &dec.id {
                        OPat::BindingIdentifier(i) => i.name.to_string(),
                        _ => continue,
                    };
                    if self.resolve_local(&name).is_none() {
                        let mirror_global = self.is_root;
                        if matches!(v.kind, oxc_ast::ast::VariableDeclarationKind::Var) {
                            self.add_var_local(name, None, mirror_global);
                        } else {
                            let slot = self.add_local_with_mirror(name, None, mirror_global);
                            self.mark_local_tdz(slot);
                        }
                    }
                }
            }
        }
    }

    fn compile_variable_declaration_oast(
        &mut self,
        v: &oast::VariableDeclaration<'_>,
    ) -> Result<(), crate::errors::CompilerError> {
        for dec in &v.declarations {
            if let Some(init) = &dec.init {
                self.compile_expr_oast(init)?;
            } else {
                self.emit(Op::Undefined, LINE);
            }
            self.compile_binding_pattern_assignment_oast(&dec.id, v.kind)?;
        }

        Ok(())
    }

    fn binding_property_key_to_value(key: &oast::PropertyKey<'_>) -> Option<Value> {
        match key {
            oast::PropertyKey::StaticIdentifier(i) => Some(Value::String(i.name.to_string())),
            oast::PropertyKey::Identifier(i) => Some(Value::String(i.name.to_string())),
            oast::PropertyKey::StringLiteral(s) => Some(Value::String(s.value.to_string())),
            oast::PropertyKey::NumericLiteral(n) => Some(Value::String(n.value.to_string())),
            _ => None,
        }
    }

    fn store_binding_identifier(
        &mut self,
        name: &str,
        kind: oxc_ast::ast::VariableDeclarationKind,
    ) -> Result<(), crate::errors::CompilerError> {
        let mirror_global = self.is_root;
        let slot = if matches!(kind, oxc_ast::ast::VariableDeclarationKind::Var) {
            self.resolve_local(name)
                .unwrap_or_else(|| self.add_var_local(name.to_string(), None, mirror_global))
        } else if let Some(slot) = self.scopes[self.scopes.len() - 1]
            .bindings
            .get(name)
            .copied()
        {
            slot
        } else {
            let slot = self.add_local_with_mirror(name.to_string(), None, mirror_global);
            self.mark_local_tdz(slot);
            slot
        };
        if self.is_root {
            self.emit(Op::Dup, LINE);
        }
        self.emit(Op::SetLocal, LINE);
        self.emit_byte(slot as u8, LINE);
        if self.is_root && mirror_global {
            self.emit_set_global_name(name);
        }
        let is_exported = self
            .current_module
            .as_ref()
            .is_some_and(|c| c.exports.contains(name));
        if is_exported {
            self.emit(Op::GetLocal, LINE);
            self.emit_byte(slot as u8, LINE);
            self.emit_set_own_module_export(name)?;
        }
        Ok(())
    }

    fn compile_binding_pattern_assignment_oast(
        &mut self,
        pattern: &oast::BindingPattern<'_>,
        kind: oxc_ast::ast::VariableDeclarationKind,
    ) -> Result<(), crate::errors::CompilerError> {
        match pattern {
            oast::BindingPattern::BindingIdentifier(i) => {
                self.store_binding_identifier(i.name.as_str(), kind)?;
            }
            oast::BindingPattern::AssignmentPattern(a) => {
                self.compile_binding_pattern_assignment_oast(&a.left, kind)?;
            }
            oast::BindingPattern::ArrayPattern(a) => {
                for (idx, el) in a.elements.iter().enumerate() {
                    let Some(el) = el else { continue };
                    self.emit(Op::Dup, LINE);
                    self.emit_constant(Value::String(idx.to_string()), LINE);
                    self.emit(Op::GetProperty, LINE);
                    self.compile_binding_pattern_assignment_oast(el, kind)?;
                }
                if let Some(rest) = &a.rest {
                    self.emit(Op::Dup, LINE);
                    self.compile_binding_pattern_assignment_oast(&rest.argument, kind)?;
                }
                self.emit(Op::Pop, LINE);
            }
            oast::BindingPattern::ObjectPattern(o) => {
                for prop in &o.properties {
                    let key = Self::binding_property_key_to_value(&prop.key).ok_or_else(|| {
                        crate::errors::CompilerError::from("unsupported object binding key")
                    })?;
                    self.emit(Op::Dup, LINE);
                    self.emit_constant(key, LINE);
                    self.emit(Op::GetProperty, LINE);
                    self.compile_binding_pattern_assignment_oast(&prop.value, kind)?;
                }
                if let Some(rest) = &o.rest {
                    self.emit(Op::Dup, LINE);
                    self.compile_binding_pattern_assignment_oast(&rest.argument, kind)?;
                }
                self.emit(Op::Pop, LINE);
            }
        }
        Ok(())
    }

    fn compile_function_declaration_oast(
        &mut self,
        f: &oast::Function<'_>,
    ) -> Result<(), crate::errors::CompilerError> {
        if let Some(ident) = &f.id {
            let fname = ident.name.to_string();
            let (proto, _env_len) = self.compile_function(&fname, f, None)?;
            let idx = self.chunk.add_function(proto);
            self.emit(Op::MakeClosure, LINE);
            self.emit_byte(idx as u8, LINE);
            let slot = if let Some(s) = self.resolve_local(&fname) {
                s
            } else {
                self.add_local_with_mirror(fname.clone(), None, self.is_root)
            };
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

    // Direct oxc AST statement/epression walkers
    fn compile_statement_oast(
        &mut self,
        stmt: &oast::Statement,
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
                    other => match other {
                        oast::ExportDefaultDeclarationKind::NumericLiteral(n) => {
                            self.emit_constant(Value::Number(n.value), LINE)
                        }
                        oast::ExportDefaultDeclarationKind::StringLiteral(s) => {
                            self.emit_constant(Value::String(s.value.to_string()), LINE)
                        }
                        oast::ExportDefaultDeclarationKind::BooleanLiteral(b) => {
                            if b.value {
                                self.emit(Op::True, LINE);
                            } else {
                                self.emit(Op::False, LINE);
                            }
                        }
                        oast::ExportDefaultDeclarationKind::NullLiteral(_) => {
                            self.emit(Op::Null, LINE)
                        }
                        oast::ExportDefaultDeclarationKind::BigIntLiteral(n) => {
                            let raw =
                                n.raw.as_ref().map(oxc_span::Atom::as_str).ok_or_else(|| {
                                    crate::errors::CompilerError::from("invalid bigint literal")
                                })?;
                            let digits = raw.strip_suffix('n').unwrap_or(raw).replace('_', "");
                            let parsed = if let Some(hex) = digits.strip_prefix("0x") {
                                num_bigint::BigInt::parse_bytes(hex.as_bytes(), 16)
                            } else if let Some(bin) = digits.strip_prefix("0b") {
                                num_bigint::BigInt::parse_bytes(bin.as_bytes(), 2)
                            } else if let Some(oct) = digits.strip_prefix("0o") {
                                num_bigint::BigInt::parse_bytes(oct.as_bytes(), 8)
                            } else {
                                num_bigint::BigInt::parse_bytes(digits.as_bytes(), 10)
                            }
                            .ok_or_else(|| {
                                crate::errors::CompilerError::from("invalid bigint literal")
                            })?;
                            self.emit_constant(Value::BigInt(parsed), LINE);
                        }
                        _ => {
                            return Err(crate::errors::CompilerError::from(
                                "unsupported export default declaration",
                            ));
                        }
                    },
                }
                self.emit_set_own_module_export("default")?;
            }
            OStmt::VariableDeclaration(v) => self.compile_variable_declaration_oast(v)?,
            OStmt::FunctionDeclaration(f) => self.compile_function_declaration_oast(f)?,
            OStmt::ClassDeclaration(c) => self.compile_class_declaration_oast(c)?,
            OStmt::ExpressionStatement(e) => {
                self.compile_expr_oast(&e.expression)?;
                self.emit(Op::Pop, LINE);
            }
            OStmt::LabeledStatement(l) => {
                let can_continue = matches!(
                    &l.body,
                    OStmt::WhileStatement(_) | OStmt::ForStatement(_) | OStmt::ForInStatement(_)
                );
                self.label_patches.push(LabelPatchState {
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
                self.enter_block_scope();
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
            OStmt::ForInStatement(f) => {
                self.loop_depth += 1;
                self.continue_patches.push(Vec::new());
                self.break_patches.push(Vec::new());

                self.emit_get_global_name("__native_forInKeys");
                self.compile_expr_oast(&f.right)?;
                self.emit(Op::Call, LINE);
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
                    self.finally_patches.push(FinallyPatchState {
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

                for stmt in &t.block.body {
                    self.compile_statement_oast(stmt)?;
                }

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
                    for stmt in &fin.body {
                        self.compile_statement_oast(stmt)?;
                    }
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
                            self.enter_block_scope();
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
                            self.enter_block_scope();
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
                            for stmt in &fin.body {
                                self.compile_statement_oast(stmt)?;
                            }
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

    fn compile_class_declaration_oast(
        &mut self,
        c: &oast::Class<'_>,
    ) -> Result<(), crate::errors::CompilerError> {
        let name =
            c.id.as_ref()
                .map(|id| id.name.to_string())
                .ok_or_else(|| crate::errors::CompilerError::from("anonymous class declaration"))?;
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
                    self.emit(Op::SetProperty, LINE);
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
                    self.emit(Op::SetProperty, LINE);
                }
            }
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
            OExpr::BigIntLiteral(n) => {
                let raw =
                    n.raw.as_ref().map(oxc_span::Atom::as_str).ok_or_else(|| {
                        crate::errors::CompilerError::from("invalid bigint literal")
                    })?;
                let digits = raw.strip_suffix('n').unwrap_or(raw).replace('_', "");
                let parsed = if let Some(hex) = digits.strip_prefix("0x") {
                    num_bigint::BigInt::parse_bytes(hex.as_bytes(), 16)
                } else if let Some(bin) = digits.strip_prefix("0b") {
                    num_bigint::BigInt::parse_bytes(bin.as_bytes(), 2)
                } else if let Some(oct) = digits.strip_prefix("0o") {
                    num_bigint::BigInt::parse_bytes(oct.as_bytes(), 8)
                } else {
                    num_bigint::BigInt::parse_bytes(digits.as_bytes(), 10)
                }
                .ok_or_else(|| crate::errors::CompilerError::from("invalid bigint literal"))?;
                self.emit_constant(Value::BigInt(parsed), LINE);
            }
            OExpr::StringLiteral(s) => self.emit_constant(Value::String(s.value.to_string()), LINE),
            OExpr::TemplateLiteral(t) => {
                let first = t
                    .quasis
                    .first()
                    .and_then(|q| q.value.cooked.as_ref())
                    .map(ToString::to_string)
                    .unwrap_or_default();
                self.emit_constant(Value::String(first), LINE);
                for (i, expr) in t.expressions.iter().enumerate() {
                    self.compile_expr_oast(expr)?;
                    self.emit(Op::Add, LINE);
                    let tail = t
                        .quasis
                        .get(i + 1)
                        .and_then(|q| q.value.cooked.as_ref())
                        .map(ToString::to_string)
                        .unwrap_or_default();
                    self.emit_constant(Value::String(tail), LINE);
                    self.emit(Op::Add, LINE);
                }
            }
            OExpr::TaggedTemplateExpression(t) => {
                // Minimal tagged template support: `tag` receives
                // `(stringsArray, ...substitutions)`.
                // We currently pass only cooked strings and omit template
                // object caching/raw metadata.
                self.compile_expr_oast(&t.tag)?;
                for quasi in &t.quasi.quasis {
                    let cooked = quasi
                        .value
                        .cooked
                        .as_ref()
                        .map(ToString::to_string)
                        .unwrap_or_default();
                    self.emit_constant(Value::String(cooked), LINE);
                }
                self.emit(Op::BuildArray, LINE);
                self.emit_byte(t.quasi.quasis.len() as u8, LINE);
                let mut arg_count = 1usize;
                for sub in &t.quasi.expressions {
                    self.compile_expr_oast(sub)?;
                    arg_count += 1;
                }
                self.emit(Op::Call, LINE);
                self.emit_byte(arg_count as u8, LINE);
            }
            OExpr::BooleanLiteral(b) => {
                if b.value {
                    self.emit(Op::True, LINE)
                } else {
                    self.emit(Op::False, LINE);
                }
            }
            OExpr::NullLiteral(_) => self.emit(Op::Null, LINE),
            OExpr::ThisExpression(_) => {
                // Emit GetThis to push the current this binding for the running frame.
                self.emit(Op::GetThis, LINE);
            }
            OExpr::ArrowFunctionExpression(a) => {
                let name = "<arrow>";
                let (proto, _env_len) = self.compile_arrow_function(name, a, None)?;
                let idx = self.chunk.add_function(proto);
                self.emit(Op::MakeClosure, LINE);
                self.emit_byte(idx as u8, LINE);
            }
            OExpr::FunctionExpression(f) => {
                let name = "<anonymous>";
                let (proto, _env_len) = self.compile_function(name, f, None)?;
                let idx = self.chunk.add_function(proto);
                self.emit(Op::MakeClosure, LINE);
                self.emit_byte(idx as u8, LINE);
            }
            OExpr::ObjectExpression(obj) => {
                let mut count = 0usize;
                for prop in &obj.properties {
                    if let oast::ObjectPropertyKind::ObjectProperty(p) = prop {
                        if p.computed {
                            continue;
                        }
                        let key = match &p.key {
                            oast::PropertyKey::StaticIdentifier(ident) => ident.name.to_string(),
                            oast::PropertyKey::Identifier(ident) => ident.name.to_string(),
                            oast::PropertyKey::StringLiteral(s) => s.value.to_string(),
                            oast::PropertyKey::NumericLiteral(n) => n.value.to_string(),
                            _ => continue,
                        };
                        self.emit_constant(Value::String(key), LINE);
                        if p.shorthand {
                            match &p.key {
                                oast::PropertyKey::StaticIdentifier(ident) => {
                                    self.emit_get_binding(ident.name.as_str());
                                }
                                oast::PropertyKey::Identifier(ident) => {
                                    self.emit_get_binding(ident.name.as_str());
                                }
                                _ => {
                                    self.emit(Op::Undefined, LINE);
                                }
                            }
                        } else {
                            self.compile_expr_oast(&p.value)?;
                        }
                        count += 1;
                    }
                }
                self.emit(Op::BuildObject, LINE);
                self.emit_byte(count as u8, LINE);
            }
            OExpr::ArrayExpression(arr) => {
                for el in &arr.elements {
                    if let Some(expr) = el.as_expression() {
                        self.compile_expr_oast(expr)?;
                    } else {
                        self.emit(Op::Undefined, LINE);
                    }
                }
                self.emit(Op::BuildArray, LINE);
                self.emit_byte(arr.elements.len() as u8, LINE);
            }
            OExpr::Identifier(id) => {
                let name = id.name.as_str();
                self.emit_get_binding(name);
            }
            OExpr::AssignmentExpression(a) => {
                let target = a.left.as_simple_assignment_target().ok_or_else(|| {
                    crate::errors::CompilerError::from("unsupported assignment target")
                })?;
                match a.operator {
                    oast::AssignmentOperator::Assign => match target {
                        oast::SimpleAssignmentTarget::AssignmentTargetIdentifier(id) => {
                            let name = id.name.as_str();
                            if self
                                .current_module
                                .as_ref()
                                .is_some_and(|c| c.imports.contains_key(name))
                            {
                                return Err(crate::errors::CompilerError::from(
                                    "cannot assign to imported binding",
                                ));
                            }
                            self.compile_expr_oast(&a.right)?;
                            self.emit(Op::Dup, LINE);
                            if let Some(slot) = self.resolve_local(name) {
                                self.emit(Op::SetLocal, LINE);
                                self.emit_byte(slot as u8, LINE);
                                if self.local_mirrors_global(slot) {
                                    self.emit(Op::GetLocal, LINE);
                                    self.emit_byte(slot as u8, LINE);
                                    self.emit_set_global_name(name);
                                }
                                // Check export membership without holding a borrow
                                let is_exported = self
                                    .current_module
                                    .as_ref()
                                    .is_some_and(|c| c.exports.contains(name));
                                if is_exported {
                                    let module_index = self
                                        .current_module
                                        .as_ref()
                                        .map(|c| c.module_index)
                                        .expect("module_index expected");
                                    let cname_idx =
                                        self.chunk.add_constant(Value::String(name.to_string()));
                                    self.emit(Op::SetModuleExport, LINE);
                                    self.emit_byte(module_index as u8, LINE);
                                    self.emit_byte(cname_idx as u8, LINE);
                                }
                            } else if let Some(upvalue) = self.resolve_upvalue(name) {
                                self.emit(Op::SetUpvalue, LINE);
                                self.emit_byte(upvalue, LINE);
                            } else {
                                // Assignment to a non-local — could be an import binding
                                if let Some((src_idx, exported_name)) = self
                                    .current_module
                                    .as_ref()
                                    .and_then(|c| c.imports.get(name).map(|(i, n)| (*i, n.clone())))
                                {
                                    let cname_idx =
                                        self.chunk.add_constant(Value::String(exported_name));
                                    self.emit(Op::SetModuleExport, LINE);
                                    self.emit_byte(src_idx as u8, LINE);
                                    self.emit_byte(cname_idx as u8, LINE);
                                } else {
                                    self.emit_set_global_name(name);
                                }
                            }
                        }
                        oast::SimpleAssignmentTarget::StaticMemberExpression(m) => {
                            self.compile_expr_oast(&m.object)?;
                            self.emit_constant(Value::String(m.property.name.to_string()), LINE);
                            self.compile_expr_oast(&a.right)?;
                            self.emit(Op::SetProperty, LINE);
                        }
                        oast::SimpleAssignmentTarget::ComputedMemberExpression(m) => {
                            self.compile_expr_oast(&m.object)?;
                            self.compile_expr_oast(&m.expression)?;
                            self.compile_expr_oast(&a.right)?;
                            self.emit(Op::SetProperty, LINE);
                        }
                        _ => {
                            return Err(crate::errors::CompilerError::from(
                                "unsupported assignment target",
                            ));
                        }
                    },
                    oast::AssignmentOperator::Addition
                    | oast::AssignmentOperator::Subtraction
                    | oast::AssignmentOperator::Multiplication
                    | oast::AssignmentOperator::Division
                    | oast::AssignmentOperator::Remainder
                    | oast::AssignmentOperator::Exponential
                    | oast::AssignmentOperator::BitwiseAnd
                    | oast::AssignmentOperator::BitwiseOR
                    | oast::AssignmentOperator::BitwiseXOR
                    | oast::AssignmentOperator::ShiftLeft
                    | oast::AssignmentOperator::ShiftRight
                    | oast::AssignmentOperator::ShiftRightZeroFill => match target {
                        oast::SimpleAssignmentTarget::AssignmentTargetIdentifier(id) => {
                            let name = id.name.as_str();
                            if let Some(slot) = self.resolve_local(name) {
                                self.emit(Op::GetLocal, LINE);
                                self.emit_byte(slot as u8, LINE);
                            } else if let Some(upvalue) = self.resolve_upvalue(name) {
                                self.emit(Op::GetUpvalue, LINE);
                                self.emit_byte(upvalue, LINE);
                            } else if self
                                .current_module
                                .as_ref()
                                .is_some_and(|c| c.imports.contains_key(name))
                            {
                                return Err(crate::errors::CompilerError::from(
                                    "cannot assign to imported binding",
                                ));
                            } else {
                                self.emit_get_global_name(name);
                            }
                            self.compile_expr_oast(&a.right)?;
                            match a.operator {
                                oast::AssignmentOperator::Addition => self.emit(Op::Add, LINE),
                                oast::AssignmentOperator::Subtraction => self.emit(Op::Sub, LINE),
                                oast::AssignmentOperator::Multiplication => {
                                    self.emit(Op::Mul, LINE)
                                }
                                oast::AssignmentOperator::Division => self.emit(Op::Div, LINE),
                                oast::AssignmentOperator::Remainder => self.emit(Op::Mod, LINE),
                                oast::AssignmentOperator::Exponential => self.emit(Op::Pow, LINE),
                                oast::AssignmentOperator::BitwiseAnd => self.emit(Op::BitAnd, LINE),
                                oast::AssignmentOperator::BitwiseOR => self.emit(Op::BitOr, LINE),
                                oast::AssignmentOperator::BitwiseXOR => self.emit(Op::BitXor, LINE),
                                oast::AssignmentOperator::ShiftLeft => self.emit(Op::Shl, LINE),
                                oast::AssignmentOperator::ShiftRight => self.emit(Op::Shr, LINE),
                                oast::AssignmentOperator::ShiftRightZeroFill => {
                                    self.emit(Op::Ushr, LINE)
                                }
                                _ => {}
                            }
                            self.emit(Op::Dup, LINE);
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
                        oast::SimpleAssignmentTarget::StaticMemberExpression(m) => {
                            self.emit_get_global_name("__native_compoundAssign");
                            self.compile_expr_oast(&m.object)?;
                            self.emit_constant(Value::String(m.property.name.to_string()), LINE);
                            self.compile_expr_oast(&a.right)?;
                            let op = match a.operator {
                                oast::AssignmentOperator::Addition => "+",
                                oast::AssignmentOperator::Subtraction => "-",
                                oast::AssignmentOperator::Multiplication => "*",
                                oast::AssignmentOperator::Division => "/",
                                oast::AssignmentOperator::Remainder => "%",
                                oast::AssignmentOperator::Exponential => "**",
                                oast::AssignmentOperator::BitwiseAnd => "&",
                                oast::AssignmentOperator::BitwiseOR => "|",
                                oast::AssignmentOperator::BitwiseXOR => "^",
                                oast::AssignmentOperator::ShiftLeft => "<<",
                                oast::AssignmentOperator::ShiftRight => ">>",
                                oast::AssignmentOperator::ShiftRightZeroFill => ">>>",
                                _ => unreachable!(),
                            };
                            self.emit_constant(Value::String(op.to_string()), LINE);
                            self.emit(Op::Call, LINE);
                            self.emit_byte(4, LINE);
                        }
                        oast::SimpleAssignmentTarget::ComputedMemberExpression(m) => {
                            self.emit_get_global_name("__native_compoundAssign");
                            self.compile_expr_oast(&m.object)?;
                            self.compile_expr_oast(&m.expression)?;
                            self.compile_expr_oast(&a.right)?;
                            let op = match a.operator {
                                oast::AssignmentOperator::Addition => "+",
                                oast::AssignmentOperator::Subtraction => "-",
                                oast::AssignmentOperator::Multiplication => "*",
                                oast::AssignmentOperator::Division => "/",
                                oast::AssignmentOperator::Remainder => "%",
                                oast::AssignmentOperator::Exponential => "**",
                                oast::AssignmentOperator::BitwiseAnd => "&",
                                oast::AssignmentOperator::BitwiseOR => "|",
                                oast::AssignmentOperator::BitwiseXOR => "^",
                                oast::AssignmentOperator::ShiftLeft => "<<",
                                oast::AssignmentOperator::ShiftRight => ">>",
                                oast::AssignmentOperator::ShiftRightZeroFill => ">>>",
                                _ => unreachable!(),
                            };
                            self.emit_constant(Value::String(op.to_string()), LINE);
                            self.emit(Op::Call, LINE);
                            self.emit_byte(4, LINE);
                        }
                        _ => {
                            return Err(crate::errors::CompilerError::from(
                                "unsupported assignment target",
                            ));
                        }
                    },
                    _ => {
                        return Err(crate::errors::CompilerError::from(
                            "unsupported assignment operator",
                        ));
                    }
                }
            }
            OExpr::UnaryExpression(u) => match u.operator {
                oast::UnaryOperator::Typeof => {
                    if let OExpr::Identifier(id) = &u.argument {
                        // Preserve `typeof undeclaredIdentifier` semantics.
                        // Normal global lookup now throws ReferenceError for
                        // missing bindings, so use a name-based helper for the
                        // identifier case.
                        let name = id.name.to_string();
                        if self.resolve_local(&name).is_some()
                            || self.resolve_upvalue(&name).is_some()
                        {
                            self.emit_get_global_name("__native_typeof");
                            self.compile_expr_oast(&u.argument)?;
                            self.emit(Op::Call, LINE);
                            self.emit_byte(1, LINE);
                        } else {
                            self.emit_get_global_name("__native_typeof_name");
                            self.emit_constant(Value::String(name), LINE);
                            self.emit(Op::Call, LINE);
                            self.emit_byte(1, LINE);
                        }
                    } else {
                        self.emit_get_global_name("__native_typeof");
                        self.compile_expr_oast(&u.argument)?;
                        self.emit(Op::Call, LINE);
                        self.emit_byte(1, LINE);
                    }
                }
                oast::UnaryOperator::Delete => {
                    if let OExpr::StaticMemberExpression(m) = &u.argument {
                        self.emit_get_global_name("__native_deleteProperty");
                        self.compile_expr_oast(&m.object)?;
                        self.emit_constant(Value::String(m.property.name.to_string()), LINE);
                        self.emit(Op::Call, LINE);
                        self.emit_byte(2, LINE);
                    } else if let OExpr::ComputedMemberExpression(m) = &u.argument {
                        self.emit_get_global_name("__native_deleteProperty");
                        self.compile_expr_oast(&m.object)?;
                        self.compile_expr_oast(&m.expression)?;
                        self.emit(Op::Call, LINE);
                        self.emit_byte(2, LINE);
                    } else {
                        self.emit_constant(Value::Bool(true), LINE);
                    }
                }
                _ => {
                    self.compile_expr_oast(&u.argument)?;
                    match u.operator {
                        oast::UnaryOperator::LogicalNot => self.emit(Op::Not, LINE),
                        oast::UnaryOperator::UnaryNegation => self.emit(Op::Neg, LINE),
                        oast::UnaryOperator::Void => {
                            self.emit(Op::Pop, LINE);
                            self.emit(Op::Undefined, LINE);
                        }
                        _ => {}
                    }
                }
            },
            OExpr::UpdateExpression(u) => {
                use oxc_ast::ast::SimpleAssignmentTarget as OTarget;

                match &u.argument {
                    OTarget::AssignmentTargetIdentifier(id) => {
                        let name = id.name.as_str();
                        if let Some(slot) = self.resolve_local(name) {
                            self.emit(Op::GetLocal, LINE);
                            self.emit_byte(slot as u8, LINE);
                            if !u.prefix {
                                self.emit(Op::Dup, LINE);
                            }
                            self.emit_constant(Value::Number(1.0), LINE);
                            match u.operator {
                                oast::UpdateOperator::Increment => self.emit(Op::Add, LINE),
                                oast::UpdateOperator::Decrement => self.emit(Op::Sub, LINE),
                            }
                            if u.prefix {
                                self.emit(Op::Dup, LINE);
                            }
                            self.emit(Op::SetLocal, LINE);
                            self.emit_byte(slot as u8, LINE);
                            if self.local_mirrors_global(slot) {
                                self.emit(Op::GetLocal, LINE);
                                self.emit_byte(slot as u8, LINE);
                                self.emit_set_global_name(name);
                            }
                        } else if let Some(upvalue) = self.resolve_upvalue(name) {
                            self.emit(Op::GetUpvalue, LINE);
                            self.emit_byte(upvalue, LINE);
                            if !u.prefix {
                                self.emit(Op::Dup, LINE);
                            }
                            self.emit_constant(Value::Number(1.0), LINE);
                            match u.operator {
                                oast::UpdateOperator::Increment => self.emit(Op::Add, LINE),
                                oast::UpdateOperator::Decrement => self.emit(Op::Sub, LINE),
                            }
                            if u.prefix {
                                self.emit(Op::Dup, LINE);
                            }
                            self.emit(Op::SetUpvalue, LINE);
                            self.emit_byte(upvalue, LINE);
                        } else {
                            self.emit_get_global_name(name);
                            if !u.prefix {
                                self.emit(Op::Dup, LINE);
                            }
                            self.emit_constant(Value::Number(1.0), LINE);
                            match u.operator {
                                oast::UpdateOperator::Increment => self.emit(Op::Add, LINE),
                                oast::UpdateOperator::Decrement => self.emit(Op::Sub, LINE),
                            }
                            if u.prefix {
                                self.emit(Op::Dup, LINE);
                            }
                            self.emit_set_global_name(name);
                        }
                    }
                    OTarget::StaticMemberExpression(m) => {
                        self.emit_get_global_name("__native_updateProperty");
                        self.compile_expr_oast(&m.object)?;
                        self.emit_constant(Value::String(m.property.name.to_string()), LINE);
                        self.emit_constant(
                            Value::Number(match u.operator {
                                oast::UpdateOperator::Increment => 1.0,
                                oast::UpdateOperator::Decrement => -1.0,
                            }),
                            LINE,
                        );
                        self.emit_constant(Value::Bool(u.prefix), LINE);
                        self.emit(Op::Call, LINE);
                        self.emit_byte(4, LINE);
                    }
                    OTarget::ComputedMemberExpression(m) => {
                        self.emit_get_global_name("__native_updateProperty");
                        self.compile_expr_oast(&m.object)?;
                        self.compile_expr_oast(&m.expression)?;
                        self.emit_constant(
                            Value::Number(match u.operator {
                                oast::UpdateOperator::Increment => 1.0,
                                oast::UpdateOperator::Decrement => -1.0,
                            }),
                            LINE,
                        );
                        self.emit_constant(Value::Bool(u.prefix), LINE);
                        self.emit(Op::Call, LINE);
                        self.emit_byte(4, LINE);
                    }
                    _ => {
                        return Err(crate::errors::CompilerError::from(
                            "unsupported update target",
                        ));
                    }
                }
            }
            OExpr::YieldExpression(y) => {
                if let Some(arg) = &y.argument {
                    self.compile_expr_oast(arg)?;
                } else {
                    self.emit(Op::Undefined, LINE);
                }
                if y.delegate {
                    self.emit(Op::YieldStar, LINE);
                } else {
                    self.emit(Op::Yield, LINE);
                }
            }
            OExpr::BinaryExpression(b) => {
                if matches!(
                    b.operator,
                    oast::BinaryOperator::Instanceof | oast::BinaryOperator::In
                ) {
                    let helper = if matches!(b.operator, oast::BinaryOperator::Instanceof) {
                        "__native_instanceof"
                    } else {
                        "__native_in"
                    };
                    self.emit_get_global_name(helper);
                    self.compile_expr_oast(&b.left)?;
                    self.compile_expr_oast(&b.right)?;
                    self.emit(Op::Call, LINE);
                    self.emit_byte(2, LINE);
                    return Ok(());
                }
                self.compile_expr_oast(&b.left)?;
                self.compile_expr_oast(&b.right)?;
                match b.operator {
                    oast::BinaryOperator::Equality => self.emit(Op::Eq, LINE),
                    oast::BinaryOperator::StrictEquality => self.emit(Op::StrictEq, LINE),
                    oast::BinaryOperator::Inequality => self.emit(Op::Neq, LINE),
                    oast::BinaryOperator::StrictInequality => self.emit(Op::StrictNeq, LINE),
                    oast::BinaryOperator::LessThan => self.emit(Op::Lt, LINE),
                    oast::BinaryOperator::LessEqualThan => self.emit(Op::Le, LINE),
                    oast::BinaryOperator::GreaterThan => self.emit(Op::Gt, LINE),
                    oast::BinaryOperator::GreaterEqualThan => self.emit(Op::Ge, LINE),
                    oast::BinaryOperator::Addition => self.emit(Op::Add, LINE),
                    oast::BinaryOperator::Subtraction => self.emit(Op::Sub, LINE),
                    oast::BinaryOperator::Multiplication => self.emit(Op::Mul, LINE),
                    oast::BinaryOperator::Division => self.emit(Op::Div, LINE),
                    oast::BinaryOperator::Remainder => self.emit(Op::Mod, LINE),
                    oast::BinaryOperator::Exponential => {
                        // Lower exponentiation to a dedicated opcode
                        self.emit(Op::Pow, LINE)
                    }
                    oast::BinaryOperator::BitwiseAnd => self.emit(Op::BitAnd, LINE),
                    oast::BinaryOperator::BitwiseOR => self.emit(Op::BitOr, LINE),
                    oast::BinaryOperator::BitwiseXOR => self.emit(Op::BitXor, LINE),
                    oast::BinaryOperator::ShiftLeft => self.emit(Op::Shl, LINE),
                    oast::BinaryOperator::ShiftRight => self.emit(Op::Shr, LINE),
                    oast::BinaryOperator::ShiftRightZeroFill => self.emit(Op::Ushr, LINE),
                    _ => {
                        return Err(crate::errors::CompilerError::from(
                            "unsupported binary operator",
                        ));
                    }
                }
            }
            OExpr::LogicalExpression(l) => match l.operator {
                oast::LogicalOperator::And => {
                    self.compile_expr_oast(&l.left)?;
                    self.emit(Op::Dup, LINE);
                    self.emit(Op::JumpIfFalse, LINE);
                    let jump_end = self.chunk.code.len();
                    self.emit_byte(0, LINE);
                    self.emit_byte(0, LINE);
                    self.emit(Op::Pop, LINE);
                    self.compile_expr_oast(&l.right)?;
                    self.patch_jump(jump_end);
                }
                oast::LogicalOperator::Or => {
                    self.compile_expr_oast(&l.left)?;
                    self.emit(Op::Dup, LINE);
                    self.emit(Op::JumpIfFalse, LINE);
                    let jump_right = self.chunk.code.len();
                    self.emit_byte(0, LINE);
                    self.emit_byte(0, LINE);
                    self.emit(Op::Jump, LINE);
                    let jump_end = self.chunk.code.len();
                    self.emit_byte(0, LINE);
                    self.emit_byte(0, LINE);
                    self.patch_jump(jump_right);
                    self.emit(Op::Pop, LINE);
                    self.compile_expr_oast(&l.right)?;
                    self.patch_jump(jump_end);
                }
                oast::LogicalOperator::Coalesce => {
                    self.compile_expr_oast(&l.left)?;
                    self.emit(Op::Dup, LINE);
                    self.emit(Op::Undefined, LINE);
                    self.emit(Op::Eq, LINE);
                    self.emit(Op::JumpIfFalse, LINE);
                    let jump_end = self.chunk.code.len();
                    self.emit_byte(0, LINE);
                    self.emit_byte(0, LINE);
                    self.emit(Op::Pop, LINE);
                    self.compile_expr_oast(&l.right)?;
                    self.patch_jump(jump_end);
                }
            },
            OExpr::ConditionalExpression(c) => {
                self.compile_expr_oast(&c.test)?;
                self.emit(Op::JumpIfFalse, LINE);
                let jump_else = self.chunk.code.len();
                self.emit_byte(0, LINE);
                self.emit_byte(0, LINE);
                self.compile_expr_oast(&c.consequent)?;
                self.emit(Op::Jump, LINE);
                let jump_end = self.chunk.code.len();
                self.emit_byte(0, LINE);
                self.emit_byte(0, LINE);
                self.patch_jump(jump_else);
                self.compile_expr_oast(&c.alternate)?;
                self.patch_jump(jump_end);
            }
            OExpr::SequenceExpression(seq) => {
                let len = seq.expressions.len();
                for (idx, expr) in seq.expressions.iter().enumerate() {
                    self.compile_expr_oast(expr)?;
                    if idx + 1 != len {
                        self.emit(Op::Pop, LINE);
                    }
                }
                if len == 0 {
                    self.emit(Op::Undefined, LINE);
                }
            }
            OExpr::StaticMemberExpression(m) => {
                self.compile_expr_oast(&m.object)?;
                self.emit_constant(Value::String(m.property.name.to_string()), LINE);
                self.emit(Op::GetPropertyIC, LINE);
                let slot = self.alloc_ic_slot()?;
                self.emit_byte(slot, LINE);
            }
            OExpr::ComputedMemberExpression(m) => {
                self.compile_expr_oast(&m.object)?;
                self.compile_expr_oast(&m.expression)?;
                self.emit(Op::GetProperty, LINE);
            }
            OExpr::CallExpression(c) => {
                if let OExpr::StaticMemberExpression(m) = &c.callee
                    && let OExpr::Identifier(obj_ident) = &m.object
                    && obj_ident.name.as_str() == "console"
                    && m.property.name.as_str() == "log"
                {
                    self.emit_get_global_name("__native_print");
                    let mut arg_count = 0usize;
                    for arg in &c.arguments {
                        if let Some(expr) = arg.as_expression() {
                            self.compile_expr_oast(expr)?;
                            arg_count += 1;
                        } else {
                            self.emit(Op::Undefined, LINE);
                            arg_count += 1;
                        }
                    }
                    self.emit(Op::Call, LINE);
                    self.emit_byte(arg_count as u8, LINE);
                    return Ok(());
                }

                if let OExpr::StaticMemberExpression(m) = &c.callee {
                    self.compile_expr_oast(&m.object)?;
                    self.emit(Op::Dup, LINE);
                    self.emit_constant(Value::String(m.property.name.to_string()), LINE);
                    self.emit(Op::GetPropertyIC, LINE);
                    let slot = self.alloc_ic_slot()?;
                    self.emit_byte(slot, LINE);
                    let mut arg_count = 0usize;
                    for arg in &c.arguments {
                        if let Some(expr) = arg.as_expression() {
                            self.compile_expr_oast(expr)?;
                            arg_count += 1;
                        } else {
                            self.emit(Op::Undefined, LINE);
                            arg_count += 1;
                        }
                    }
                    self.emit(Op::CallMethod, LINE);
                    self.emit_byte(arg_count as u8, LINE);
                    return Ok(());
                }

                if let OExpr::ComputedMemberExpression(m) = &c.callee {
                    self.compile_expr_oast(&m.object)?;
                    self.emit(Op::Dup, LINE);
                    self.compile_expr_oast(&m.expression)?;
                    self.emit(Op::GetProperty, LINE);
                    let mut arg_count = 0usize;
                    for arg in &c.arguments {
                        if let Some(expr) = arg.as_expression() {
                            self.compile_expr_oast(expr)?;
                            arg_count += 1;
                        } else {
                            self.emit(Op::Undefined, LINE);
                            arg_count += 1;
                        }
                    }
                    self.emit(Op::CallMethod, LINE);
                    self.emit_byte(arg_count as u8, LINE);
                    return Ok(());
                }

                if let OExpr::PrivateFieldExpression(m) = &c.callee {
                    // Lower this.#private() style calls to preserve receiver.
                    self.compile_expr_oast(&m.object)?;
                    self.emit(Op::Dup, LINE);
                    // private identifier -> synthetic constant
                    let oast::PrivateIdentifier { name, .. } = &m.field;
                    let key = self.ensure_private_name_constant(name);
                    self.emit_constant(key, LINE);
                    self.emit(Op::GetProperty, LINE);
                    let mut arg_count = 0usize;
                    for arg in &c.arguments {
                        if let Some(expr) = arg.as_expression() {
                            self.compile_expr_oast(expr)?;
                            arg_count += 1;
                        } else {
                            self.emit(Op::Undefined, LINE);
                            arg_count += 1;
                        }
                    }
                    self.emit(Op::CallMethod, LINE);
                    self.emit_byte(arg_count as u8, LINE);
                    return Ok(());
                }

                let has_spread = c
                    .arguments
                    .iter()
                    .any(|arg| matches!(arg, oast::Argument::SpreadElement(_)));
                if has_spread {
                    self.emit_get_global_name("__native_call_spread");
                    self.compile_expr_oast(&c.callee)?;
                    self.emit(Op::Undefined, LINE);
                    for arg in &c.arguments {
                        match arg {
                            oast::Argument::SpreadElement(s) => {
                                self.compile_expr_oast(&s.argument)?;
                            }
                            _ => {
                                if let Some(expr) = arg.as_expression() {
                                    self.compile_expr_oast(expr)?;
                                } else {
                                    self.emit(Op::Undefined, LINE);
                                }
                            }
                        }
                    }
                    self.emit(Op::BuildArray, LINE);
                    self.emit_byte(c.arguments.len() as u8, LINE);
                    for arg in &c.arguments {
                        if matches!(arg, oast::Argument::SpreadElement(_)) {
                            self.emit(Op::True, LINE);
                        } else {
                            self.emit(Op::False, LINE);
                        }
                    }
                    self.emit(Op::BuildArray, LINE);
                    self.emit_byte(c.arguments.len() as u8, LINE);
                    self.emit(Op::Call, LINE);
                    self.emit_byte(4, LINE);
                } else {
                    self.compile_expr_oast(&c.callee)?;
                    let mut arg_count = 0usize;
                    for arg in &c.arguments {
                        if let Some(expr) = arg.as_expression() {
                            self.compile_expr_oast(expr)?;
                            arg_count += 1;
                        } else {
                            self.emit(Op::Undefined, LINE);
                            arg_count += 1;
                        }
                    }
                    self.emit(Op::Call, LINE);
                    self.emit_byte(arg_count as u8, LINE);
                }
            }
            OExpr::NewExpression(n) => {
                self.emit_get_global_name("__native_construct");
                self.compile_expr_oast(&n.callee)?;
                let mut arg_count = 0usize;
                for arg in &n.arguments {
                    if let Some(expr) = arg.as_expression() {
                        self.compile_expr_oast(expr)?;
                        arg_count += 1;
                    } else {
                        self.emit(Op::Undefined, LINE);
                        arg_count += 1;
                    }
                }
                self.emit(Op::Call, LINE);
                self.emit_byte((arg_count + 1) as u8, LINE);
            }
            OExpr::ImportExpression(i) => {
                self.emit_get_global_name("__native_dynamic_import");
                self.compile_expr_oast(&i.source)?;
                if let Some(options) = &i.options {
                    self.compile_expr_oast(options)?;
                } else {
                    self.emit(Op::Undefined, LINE);
                }
                self.emit(Op::Call, LINE);
                self.emit_byte(2, LINE);
            }
            OExpr::RegExpLiteral(re) => {
                self.emit_get_global_name("RegExp");
                self.emit_constant(Value::String(re.regex.pattern.text.to_string()), LINE);
                self.emit_constant(Value::String(re.regex.flags.to_string()), LINE);
                self.emit(Op::Call, LINE);
                self.emit_byte(2, LINE);
            }
            OExpr::ParenthesizedExpression(p) => {
                self.compile_expr_oast(&p.expression)?;
            }
            OExpr::ClassExpression(c) => {
                // Minimal class expression support: emit a trivial constructor
                // function and push it on the stack. Full class element
                // initialization is not implemented yet; this prevents
                // class-expressions from lowering to `Undefined` which breaks
                // usages like `var C = class {}` followed by `new C()`.
                let name =
                    c.id.as_ref()
                        .map_or_else(|| "<anonymous>".to_string(), |id| id.name.to_string());
                let mut ctor_chunk = Chunk::new(Rc::clone(&self.source_text));
                ctor_chunk.write(Op::Return as u8, LINE);
                let proto = FunctionProto {
                    name,
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
                // To initialize class elements we need a local slot to hold the
                // constructor while we install methods. Duplicate the ctor and
                // store into a temp local so the original ctor remains on the
                // stack as the expression result.
                let temp_name = format!("__class_ctor_{}", self.locals.len());
                let temp_slot = self.add_local_with_mirror(temp_name, None, false);
                // dup ctor -> [ctor, ctor]
                self.emit(Op::Dup, LINE);
                // set local consumes one ctor -> [ctor]
                self.emit(Op::SetLocal, LINE);
                self.emit_byte(temp_slot as u8, LINE);

                // Emit class elements (methods) similar to declaration lowering.
                for element in &c.body.body {
                    if let oast::ClassElement::MethodDefinition(md) = element {
                        let method_name = match &md.key {
                            oast::PropertyKey::StaticIdentifier(ident) => {
                                Some(ident.name.to_string())
                            }
                            oast::PropertyKey::Identifier(ident) => Some(ident.name.to_string()),
                            oast::PropertyKey::StringLiteral(s) => Some(s.value.to_string()),
                            oast::PropertyKey::NumericLiteral(n) => Some(n.value.to_string()),
                            oast::PropertyKey::PrivateIdentifier(pid) => Some(pid.name.to_string()),
                            _ => None,
                        };
                        let (proto, _env_len) = self.compile_function(
                            method_name.as_deref().unwrap_or("<method>"),
                            &md.value,
                            None,
                        )?;
                        let fidx = self.chunk.add_function(proto);

                        if md.r#static {
                            // static: install on constructor
                            self.emit(Op::GetLocal, LINE);
                            self.emit_byte(temp_slot as u8, LINE);
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
                            self.emit(Op::MakeClosure, LINE);
                            self.emit_byte(fidx as u8, LINE);
                            self.emit(Op::SetProperty, LINE);
                        } else {
                            // instance: install on constructor.prototype
                            self.emit(Op::GetLocal, LINE);
                            self.emit_byte(temp_slot as u8, LINE);
                            self.emit_constant(Value::String("prototype".to_string()), LINE);
                            self.emit(Op::GetProperty, LINE);
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
                            self.emit(Op::MakeClosure, LINE);
                            self.emit_byte(fidx as u8, LINE);
                            self.emit(Op::SetProperty, LINE);
                        }
                    }
                }
            }
            OExpr::AwaitExpression(a) => {
                self.compile_expr_oast(&a.argument)?;
                self.emit(Op::AwaitPromise, LINE);
            }
            OExpr::TSAsExpression(a) => {
                self.compile_expr_oast(&a.expression)?;
            }
            OExpr::TSNonNullExpression(n) => {
                self.compile_expr_oast(&n.expression)?;
            }
            _ => self.emit(Op::Undefined, LINE),
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
                    self.add_local_with_mirror(name, None, self.is_root);
                }
            }
        }
    }

    fn emit_hoisted_function_decls_oast_stmts(
        &mut self,
        stmts: &[oast::Statement],
    ) -> Result<(), crate::errors::CompilerError> {
        for stmt in stmts {
            if let oast::Statement::FunctionDeclaration(f) = stmt
                && let Some(ident) = &f.id
            {
                let fname = ident.name.to_string();
                let (proto, _env_len) = self.compile_function(&fname, f, None)?;
                let idx = self.chunk.add_function(proto);
                self.emit(Op::MakeClosure, LINE);
                self.emit_byte(idx as u8, LINE);
                let slot = if let Some(s) = self.resolve_local(&fname) {
                    s
                } else {
                    self.add_local_with_mirror(fname.clone(), None, self.is_root)
                };
                if self.is_root {
                    self.emit(Op::Dup, LINE);
                }
                self.emit(Op::SetLocal, LINE);
                self.emit_byte(slot as u8, LINE);
                if self.is_root {
                    self.emit_set_global_name(&fname);
                }
            }
        }
        Ok(())
    }

    /// Check whether a function body contains a "use strict" directive
    /// prologue. Per ECMAScript, directive prologue consists of leading
    /// string-literal expression statements appearing before any other
    /// statements.
    fn has_use_strict_directive(body: &oast::FunctionBody<'_>) -> bool {
        body.directives
            .iter()
            .any(|directive| directive.directive.as_str() == "use strict")
    }

    fn has_simple_parameter_list(func: &oast::Function<'_>) -> bool {
        func.params.rest.is_none()
            && func.params.items.iter().all(|param| {
                matches!(param.pattern, oast::BindingPattern::BindingIdentifier(_))
                    && param.initializer.is_none()
            })
    }

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

    fn patch_jump_list(&mut self, patches: Vec<usize>, target: usize) {
        for patch_pos in patches {
            let offset = target as isize - (patch_pos as isize) - 2;
            let v = offset as i16;
            self.chunk.code[patch_pos] = (v >> 8) as u8;
            self.chunk.code[patch_pos + 1] = (v & 0xff) as u8;
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
            chunk: Chunk::new(Rc::clone(&self.source_text)),
            source_text: Rc::clone(&self.source_text),
            locals: Vec::new(),
            local_types: Vec::new(),
            local_tdz: Vec::new(),
            local_mirror_globals: Vec::new(),
            scopes: vec![Scope {
                kind: ScopeKind::Function,
                bindings: HashMap::new(),
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

    fn compile_arrow_function(
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
            scopes: vec![Scope {
                kind: ScopeKind::Function,
                bindings: HashMap::new(),
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

    // Legacy owned-AST expression walker removed. Use `compile_expr_oast`.
}

impl Default for Compiler {
    fn default() -> Self {
        Self::new(Rc::new(String::new()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bytecode::Op;
    use crate::compiler::ModuleContext;
    use crate::value::ObjectData;
    use crate::vm::Vm;

    use crate::value::Value;
    use std::cell::RefCell;
    use std::collections::{HashMap, HashSet};
    use std::rc::Rc;

    #[test]
    fn hoist_top_level_function_names_into_locals() {
        let src = "f(); function f() { return 42; }";
        let session = crate::parser::Parser::parse_source(src).expect("parse ok");
        let program = session.program();
        let chunk = compile(program, session.source()).expect("compile ok");

        // If a constant "f" exists, ensure it is not referenced via GetGlobal/SetGlobal.
        let mut f_const_idx: Option<usize> = None;
        for (i, c) in chunk.constants.iter().enumerate() {
            if let Value::String(s) = c
                && s == "f"
            {
                f_const_idx = Some(i);
                break;
            }
        }

        if let Some(idx) = f_const_idx {
            let mut i = 0usize;
            while i < chunk.code.len() {
                if let Some(op) = Op::from_byte(chunk.code[i]) {
                    match op {
                        Op::GetGlobal | Op::SetGlobal => {
                            let hi = chunk.code.get(i + 1).cloned().unwrap_or(0) as usize;
                            let lo = chunk.code.get(i + 2).cloned().unwrap_or(0) as usize;
                            let ref_idx = (hi << 8) | lo;
                            assert_ne!(ref_idx, idx, "found GetGlobal/SetGlobal referencing 'f'");
                            i += 3;
                        }
                        Op::Jump | Op::JumpIfFalse | Op::PushHandler => i += 3,
                        Op::Const
                        | Op::MakeClosure
                        | Op::GetPropertyIC
                        | Op::CallMethod
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
        let chunk = compile(program, session.source()).expect("compile ok");

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
                    | Op::CallMethod
                    | Op::Call
                    | Op::GetBuiltin
                    | Op::SetLocal
                    | Op::GetLocal
                    | Op::GetProperty
                    | Op::SetProperty => i += 2,
                    Op::SetGlobal | Op::GetGlobal => i += 3,
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
        let chunk = compile(program, session.source()).expect("compile ok");

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
                    | Op::CallMethod
                    | Op::Call
                    | Op::GetBuiltin
                    | Op::SetLocal
                    | Op::GetLocal
                    | Op::GetProperty
                    | Op::SetProperty => i += 2,
                    Op::SetGlobal | Op::GetGlobal => i += 3,
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

    #[test]
    fn static_member_ic_slots_are_unique_per_site() {
        let src = "a.x; a.y; a.x;";
        let session = crate::parser::Parser::parse_source(src).expect("parse ok");
        let program = session.program();
        let chunk = compile(program, session.source()).expect("compile ok");

        let mut slots = Vec::new();
        let mut i = 0usize;
        while i < chunk.code.len() {
            if let Some(op) = Op::from_byte(chunk.code[i]) {
                match op {
                    Op::GetPropertyIC => {
                        slots.push(chunk.code.get(i + 1).copied().expect("slot byte"));
                        i += 2;
                    }
                    Op::Jump | Op::JumpIfFalse | Op::PushHandler => i += 3,
                    Op::Const
                    | Op::MakeClosure
                    | Op::CallMethod
                    | Op::Call
                    | Op::GetBuiltin
                    | Op::SetLocal
                    | Op::GetLocal
                    | Op::GetProperty
                    | Op::SetProperty
                    | Op::BuildArray
                    | Op::BuildObject => i += 2,
                    Op::SetGlobal | Op::GetGlobal => i += 3,
                    _ => i += 1,
                }
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
            .filter_map(|b| Op::from_byte(*b))
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

        let mut i = 0usize;
        let mut loop_start = None;
        let mut back_target = None;
        while i < chunk.code.len() {
            if let Some(op) = Op::from_byte(chunk.code[i]) {
                match op {
                    Op::GetLocal if loop_start.is_none() => {
                        loop_start = Some(i);
                        i += 2;
                    }
                    Op::Jump => {
                        let hi = chunk.code.get(i + 1).copied().unwrap_or(0) as i16;
                        let lo = chunk.code.get(i + 2).copied().unwrap_or(0) as i16;
                        let offset = i16::from_be_bytes([hi as u8, lo as u8]) as isize;
                        back_target = Some((i + 3) as isize + offset);
                        i += 3;
                    }
                    Op::JumpIfFalse | Op::PushHandler => i += 3,
                    Op::Const
                    | Op::MakeClosure
                    | Op::GetPropertyIC
                    | Op::CallMethod
                    | Op::Call
                    | Op::GetBuiltin
                    | Op::SetLocal
                    | Op::GetLocal
                    | Op::GetProperty
                    | Op::SetProperty
                    | Op::BuildArray
                    | Op::BuildObject => i += 2,
                    Op::SetGlobal | Op::GetGlobal => i += 3,
                    _ => i += 1,
                }
            } else {
                i += 1;
            }
        }

        assert_eq!(back_target, loop_start.map(|v| v as isize));
    }

    #[test]
    fn compile_new_expression_lowers_to_call() {
        let src = "new Array(1, 2, 3);";
        let session = crate::parser::Parser::parse_source(src).expect("parse ok");
        let program = session.program();
        let chunk = compile(program, session.source()).expect("compile ok");

        let saw_call = chunk
            .code
            .iter()
            .filter_map(|b| Op::from_byte(*b))
            .any(|op| matches!(op, Op::Call));

        assert!(saw_call, "expected Call opcode for lowered new expression");
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
            .filter_map(|b| Op::from_byte(*b))
            .any(|op| matches!(op, Op::Add));
        let saw_set_local = chunk
            .code
            .iter()
            .filter_map(|b| Op::from_byte(*b))
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
            #[allow(clippy::panic)]
            Err(e) => panic!("parse_source failed: {:?}", e),
        };
        let program = session.program();
        let chunk = match compile(program, session.source()) {
            Ok(c) => c,
            // We allow panics here because the compiler error is expected, we can't recover if the user's code is invalid
            #[allow(clippy::panic)]
            Err(e) => panic!("compile failed: {:?}", e),
        };

        let saw_call_method = chunk
            .code
            .iter()
            .filter_map(|b| Op::from_byte(*b))
            .any(|op| matches!(op, Op::CallMethod));

        assert!(saw_call_method, "expected member call to emit CallMethod");
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
            .filter_map(|b| Op::from_byte(*b))
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
    fn nullish_coalescing_is_supported() {
        let src = "1 ?? 2;";
        let session = crate::parser::Parser::parse_source(src).expect("parse ok");
        let program = session.program();

        let chunk = compile(program, session.source()).expect("compile ok");
        let saw_coalesce = chunk
            .code
            .iter()
            .filter_map(|b| Op::from_byte(*b))
            .any(|op| matches!(op, Op::Eq | Op::JumpIfFalse));
        assert!(
            saw_coalesce,
            "expected nullish coalescing lowering to emit Eq/JumpIfFalse"
        );
    }

    #[test]
    fn block_scoped_let_does_not_escape_block() {
        let src = "{ let x = 1; } x = 2;";
        let session = crate::parser::Parser::parse_source(src).expect("parse ok");
        let program = session.program();
        let chunk = compile(program, session.source()).expect("compile ok");

        let globals = Rc::new(RefCell::new(ObjectData::new()));
        let mut vm = Vm::new(Rc::clone(&globals));
        vm.run(chunk).expect("run ok");

        assert!(
            matches!(vm.get_global("x"), Some(Value::Number(n)) if (n - 2.0).abs() < f64::EPSILON),
            "block-local binding should not leak past its block"
        );
    }
}
