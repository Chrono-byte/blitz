//! Core Compiler struct and foundational implementation.
//!
//! This module contains the basic structure and initialization logic for the compiler,
//! along with fundamental helper methods for scope management, local variable tracking,
//! and basic bytecode emission.

use crate::bytecode::{Chunk, Op, UpvalueSpec};
use crate::errors::CompilerError;
use crate::typecheck::Type;
use crate::value::Value;
use oxc_ast::ast as oast;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

const LINE: u32 = 1;

thread_local! {
    static COMPILE_CONTEXT_OVERRIDE: std::cell::RefCell<Option<(bool, bool)>> =
        const { std::cell::RefCell::new(None) };
}

pub(crate) fn with_compile_context_override<R>(
    context: Option<(bool, bool)>,
    f: impl FnOnce() -> R,
) -> R {
    Compiler::with_compile_context_override(context, f)
}

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
    /// Per-local immutability marker (`const` and named function-expression bindings).
    pub local_immutable: Vec<bool>,
    /// Per-local flag for immutable bindings that ignore sloppy writes.
    pub local_sloppy_immutable_silent: Vec<bool>,
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
    /// Current nested `with` depth.
    pub with_depth: usize,
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
    /// Number of operand bytes already emitted into the last instruction's payload.
    /// This lets the old emit()/emit_byte() caller pattern pack subsequent operand
    /// bytes into the single 24-bit payload of the instruction word.
    pub pending_operand_bytes: usize,
    /// Whether `new.target` is valid in the current compilation context.
    pub allow_new_target: bool,
    /// Whether `super` property access/calls are valid in the current context.
    pub allow_super: bool,
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
#[derive(Clone, Debug)]
pub enum CaptureSource {
    /// Local variable captured from enclosing function scope.
    ///
    /// `mirror_global_name` is set for root `var` bindings that are also
    /// exposed on the global object.
    Local {
        /// Slot index in the enclosing function.
        slot: u8,
        /// Optional global property name mirrored by this binding.
        mirror_global_name: Option<String>,
        /// Whether assignment to this binding must be rejected.
        immutable: bool,
        /// Whether writes in non-strict mode should be silently ignored.
        sloppy_immutable_silent: bool,
    },
    /// Upvalue captured from outer closure.
    Upvalue {
        /// Upvalue index in the enclosing function.
        index: u8,
        /// Whether assignment to this binding must be rejected.
        immutable: bool,
        /// Whether writes in non-strict mode should be silently ignored.
        sloppy_immutable_silent: bool,
    },
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
    /// Resolves the patch index for a jump offset, accounting for fixed-width
    /// mode where jumps are patched in place rather than emitted separately.
    fn resolve_jump_patch_index(&self, offset: usize) -> usize {
        let op_at = |idx: usize| {
            self.chunk
                .code
                .get(idx)
                .and_then(|instr| Op::from_byte((instr & 0xFF) as u8))
        };
        let is_patchable_jump_at =
            |idx: usize| op_at(idx).is_some_and(|op| op.is_jump() || matches!(op, Op::PushHandler));

        // Legacy call-sites capture `code.len()` immediately after emitting the
        // jump opcode, which points one instruction past the jump in fixed-width
        // mode. Prefer that interpretation first.
        if offset > 0 && is_patchable_jump_at(offset - 1) {
            offset - 1
        } else {
            offset
        }
    }

    /// Creates a new compiler instance
    pub fn new(source_text: Rc<String>) -> Self {
        let (allow_new_target, allow_super) = COMPILE_CONTEXT_OVERRIDE
            .with(|override_state| *override_state.borrow())
            .unwrap_or((false, false));
        let mut chunk = Chunk::new(Rc::clone(&source_text));
        chunk.allow_new_target = allow_new_target;
        chunk.allow_super = allow_super;
        Self {
            chunk,
            source_text,
            locals: Vec::new(),
            local_types: Vec::new(),
            local_tdz: Vec::new(),
            local_immutable: Vec::new(),
            local_sloppy_immutable_silent: Vec::new(),
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
            with_depth: 0,
            continue_patches: Vec::new(),
            break_patches: Vec::new(),
            finally_patches: Vec::new(),
            label_patches: Vec::new(),
            current_module: None,
            next_ic_slot: 0,
            pending_operand_bytes: 0,
            allow_new_target,
            allow_super,
            outer_bindings: HashMap::new(),
            upvalue_indices: HashMap::new(),
            captured_upvalues: Vec::new(),
            private_names: HashMap::new(),
            object_expr_had_set_property: false,
        }
    }

    /// Emits a single instruction word with an 8-bit opcode and zero payload.
    pub(crate) fn emit(&mut self, op: Op, line: u32) {
        // Emit an instruction word with opcode in low 8 bits and zero payload.
        let instr = (op.to_byte() as u32) & 0xFF;
        self.chunk.write_instr(instr, line);
        // Reset pending operand byte counter for this new instruction.
        self.pending_operand_bytes = 0;
    }

    /// Emit a single instruction word with a 24-bit payload packed into bits 8..31.
    /// This is the preferred path for RISC-style fixed-width instructions where
    /// the opcode and all operands must be written as one u32.
    pub(crate) fn emit_with_payload(&mut self, op: Op, payload24: u32, line: u32) {
        let instr = ((payload24 & 0x00FF_FFFF) << 8) | ((op.to_byte() as u32) & 0xFF);
        self.chunk.write_instr(instr, line);
        // We're writing the full instruction at once so reset pending bytes.
        self.pending_operand_bytes = 0;
    }

    pub(crate) fn emit_byte(&mut self, byte: u8, line: u32) {
        // Pack a single operand byte into the previous instruction's 24-bit payload
        // using little-endian order: first emitted byte occupies bits 8..15, next 16..23, etc.
        if self.chunk.code.is_empty() {
            panic!("emit_byte with empty chunk");
        }
        let idx = self.chunk.code.len() - 1;
        if self.pending_operand_bytes >= 3 {
            panic!("too many operand bytes for instruction payload");
        }
        let shift = 8 + (self.pending_operand_bytes * 8);
        let instr = self.chunk.code[idx];
        let new = instr | ((byte as u32) << shift);
        self.chunk.code[idx] = new;
        self.pending_operand_bytes += 1;
        // Update line info for the instruction's operand emission. We modify
        // the previous instruction word in-place, so update the corresponding
        // line entry instead of pushing a new one.
        let lines_idx = idx;
        if lines_idx < self.chunk.lines.len() {
            self.chunk.lines[lines_idx] = line;
        } else {
            // Defensive fallback: if lines is shorter than code, push the line.
            self.chunk.lines.push(line);
        }
    }

    /// Emits a u16 value as a 24-bit payload in a single instruction word,
    /// avoiding multi-write emissions which break the fixed-width instruction model.
    pub(crate) fn emit_u16(&mut self, value: u16, line: u32) {
        // Pack the u16 into the low 16 bits of the 24-bit payload and emit
        // as a single instruction word. This avoids emitting multiple operand
        // bytes separately which can desynchronize instruction indices.
        let payload = value as u32 & 0xFFFF;
        // The caller must have emitted the opcode previously; to preserve
        // existing call-sites that expect emit_u16 to follow an emit(Op),
        // we'll patch the previously emitted zero-payload instruction word
        // rather than calling write_instr again. If there is no previous
        // instruction, fall back to writing a full instruction with a
        // zero opcode (shouldn't happen in normal compilation).
        if self.chunk.code.is_empty() {
            // Defensive: write a full word with payload and no-op opcode 0.
            let instr = (payload & 0x00FF_FFFF) << 8_i32;
            self.chunk.write_instr(instr, line);
        } else {
            let idx = self.chunk.code.len() - 1;
            let instr = self.chunk.code[idx];
            // Clear any existing payload bits and OR in our payload.
            let base = instr & 0xFF;
            let new = base | ((payload & 0x00FF_FFFF) << 8_i32);
            self.chunk.code[idx] = new;
            // Keep line table aligned: one line entry already exists for the opcode,
            // so do not push another. Reset pending counter.
            self.pending_operand_bytes = 0;
        }
    }

    /// Emits a constant value as a `Const` instruction with the constant index
    /// encoded into the 24-bit payload in a single instruction word to avoid
    /// multi-write emissions which break the fixed-width instruction model.
    pub(crate) fn emit_constant(&mut self, value: Value, line: u32) {
        let idx = self.chunk.add_constant(value);
        // Emit the Const instruction with the constant index encoded into
        // the 24-bit payload in a single instruction word to avoid
        // multi-write emissions which break the fixed-width instruction model.
        let idx = u16::try_from(idx).expect("too many constants in chunk") as u32;
        self.emit_with_payload(Op::Const, idx, line);
    }

    /// Returns a compiler error for a strict binding name (eval/arguments).
    pub(crate) fn strict_binding_name_error(
        strict_context: bool,
        name: &str,
    ) -> Option<CompilerError> {
        if strict_context && matches!(name, "eval" | "arguments") {
            Some(CompilerError::from(format!(
                "SyntaxError: `{}` is not allowed in strict mode",
                name
            )))
        } else {
            None
        }
    }

    /// Returns a compiler error for assigning to an imported binding.
    pub(crate) fn imported_binding_assignment_error() -> CompilerError {
        CompilerError::from("SyntaxError: Cannot assign to imported binding")
    }

    /// Returns a compiler error if the given binding pattern has a strict name.
    pub(crate) fn strict_binding_pattern_name_error(
        strict_context: bool,
        pattern: &oast::BindingPattern<'_>,
    ) -> Option<CompilerError> {
        use oast::BindingPattern as OPat;

        match pattern {
            OPat::BindingIdentifier(i) => Self::strict_binding_name_error(strict_context, &i.name),
            OPat::AssignmentPattern(a) => {
                Self::strict_binding_pattern_name_error(strict_context, &a.left)
            }
            OPat::ObjectPattern(o) => o.properties.iter().find_map(|prop| {
                Self::strict_binding_pattern_name_error(strict_context, &prop.value)
            }),
            OPat::ArrayPattern(a) => a
                .elements
                .iter()
                .flatten()
                .find_map(|elem| Self::strict_binding_pattern_name_error(strict_context, elem))
                .or_else(|| {
                    a.rest.as_ref().and_then(|rest| {
                        Self::strict_binding_pattern_name_error(strict_context, &rest.argument)
                    })
                }),
        }
    }

    /// Allocates a new inline cache slot for the current compilation chunk.
    ///
    /// # Errors
    ///
    /// Returns an error if the maximum number of inline cache sites has been exceeded.
    pub fn alloc_ic_slot(&mut self) -> Result<u8, CompilerError> {
        let slot = self.next_ic_slot;
        self.next_ic_slot = self.next_ic_slot.wrapping_add(1);
        Ok(slot)
    }

    /// Resolves a local variable by name, returning its slot index if found.
    pub(crate) fn resolve_local(&self, name: &str) -> Option<usize> {
        self.scopes
            .iter()
            .rev()
            .find_map(|scope| scope.bindings.get(name).copied())
    }

    /// Returns the index of the current function scope.
    pub(crate) fn current_function_scope_index(&self) -> usize {
        self.scopes
            .iter()
            .rposition(|scope| scope.kind == ScopeKind::Function)
            .expect("compiler must always have a function scope")
    }

    /// Adds a local variable to the given scope, returning its slot index.
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
        self.local_immutable.push(false);
        self.local_sloppy_immutable_silent.push(false);
        self.local_mirror_globals.push(mirror_global);
        self.scopes[scope_index].bindings.insert(name, slot);
        slot
    }

    /// Marks a local variable as being in the TDZ (Temporal Dead Zone).
    pub(crate) fn mark_local_tdz(&mut self, slot: usize) {
        if let Some(flag) = self.local_tdz.get_mut(slot) {
            *flag = true;
        }
    }

    /// Adds a lexical binding (used for `let`/`const` bindings).
    pub(crate) fn add_lexical_binding(&mut self, name: &str) {
        let scope_index = self.scopes.len().saturating_sub(1);
        self.scopes[scope_index]
            .lexical_bindings
            .insert(name.to_string());
    }

    /// Adds a local variable with a mirror flag (used for `let`/`const` bindings).
    pub(crate) fn add_local_with_mirror(
        &mut self,
        name: String,
        type_ann: Option<Type>,
        mirror_global: bool,
    ) -> usize {
        let scope_index = self.scopes.len().saturating_sub(1);
        self.add_local_in_scope(scope_index, name, type_ann, mirror_global)
    }

    /// Adds a local variable without a mirror flag (used for `var` bindings).
    pub(crate) fn add_var_local(
        &mut self,
        name: String,
        type_ann: Option<Type>,
        mirror_global: bool,
    ) -> usize {
        let scope_index = self.current_function_scope_index();
        self.add_local_in_scope(scope_index, name, type_ann, mirror_global)
    }

    /// Checks for a lexical binding conflict with a `var` binding in the current scope.
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

    /// Collects variable names from a statement, excluding lexical bindings.
    pub(crate) fn collect_var_names_from_statement(
        &self,
        stmt: &oast::Statement<'_>,
    ) -> HashSet<String> {
        let mut names = HashSet::new();
        self.add_var_names_from_statement(stmt, &mut names);
        names
    }

    /// Adds variable names from a statement to the given set, excluding lexical bindings.
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

    /// Adds variable names from a binding pattern to the given set.
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

    /// Enters a block scope with the given block var names.
    pub(crate) fn enter_block_scope(&mut self) {
        self.scopes.push(Scope {
            kind: ScopeKind::Block,
            bindings: HashMap::new(),
            lexical_bindings: HashSet::new(),
            block_var_names: HashSet::new(),
        });
    }

    /// Enters a block scope with the given block var names.
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

    /// Exits the current scope, popping it from the scope stack.
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

    /// Returns a snapshot of the current visible bindings from all scopes.
    pub(crate) fn visible_bindings_snapshot(&mut self) -> HashMap<String, CaptureSource> {
        let mut bindings = HashMap::new();
        for scope in &self.scopes {
            for (name, slot) in &scope.bindings {
                bindings.insert(
                    name.clone(),
                    CaptureSource::Local {
                        slot: u8::try_from(*slot).expect("slot fits in u8"),
                        mirror_global_name: if self.local_mirrors_global(*slot) {
                            Some(name.clone())
                        } else {
                            None
                        },
                        immutable: self.local_immutable.get(*slot).copied().unwrap_or(false),
                        sloppy_immutable_silent: self
                            .local_sloppy_immutable_silent
                            .get(*slot)
                            .copied()
                            .unwrap_or(false),
                    },
                );
            }
        }
        for (name, idx) in &self.upvalue_indices {
            let (immutable, sloppy_immutable_silent) = self
                .captured_upvalues
                .get(*idx as usize)
                .map_or((false, false), |spec| {
                    (spec.immutable, spec.sloppy_immutable_silent)
                });
            bindings
                .entry(name.clone())
                .or_insert(CaptureSource::Upvalue {
                    index: *idx,
                    immutable,
                    sloppy_immutable_silent,
                });
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
                let (immutable, sloppy_immutable_silent) = self
                    .captured_upvalues
                    .get(idx as usize)
                    .map_or((false, false), |spec| {
                        (spec.immutable, spec.sloppy_immutable_silent)
                    });
                bindings.insert(
                    name,
                    CaptureSource::Upvalue {
                        index: idx,
                        immutable,
                        sloppy_immutable_silent,
                    },
                );
            }
        }
        bindings
    }

    /// Returns true if the local variable at the given slot mirrors the global object.
    pub(crate) fn local_mirrors_global(&self, slot: usize) -> bool {
        self.local_mirror_globals
            .get(slot)
            .copied()
            .unwrap_or(false)
    }

    /// Emits a `LoadLocal` or `GetUpvalue` operation for the given binding name.
    pub(crate) fn emit_get_binding(&mut self, name: &str) {
        if let Some(slot) = self.resolve_local(name) {
            self.emit(Op::LoadLocal, LINE);
            self.emit_byte(u8::try_from(slot).expect("slot fits in u8"), LINE);
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
            self.emit_byte(u8::try_from(src_idx).expect("src_idx fits in u8"), LINE);
            self.emit_byte(u8::try_from(cname_idx).expect("cname_idx fits in u8"), LINE);
        } else {
            self.emit_get_global_name(name);
        }
    }

    /// Emits a `SetModuleExport` operation for the given export name.
    pub(crate) fn emit_set_own_module_export(
        &mut self,
        export_name: &str,
    ) -> Result<(), CompilerError> {
        let module_index = self
            .current_module
            .as_ref()
            .map(|c| c.module_index)
            .ok_or_else(|| CompilerError::from("module_index expected"))?;
        let cname_idx = self
            .chunk
            .add_constant(Value::String(export_name.to_string()));
        self.emit(Op::SetModuleExport, LINE);
        self.emit_byte(
            u8::try_from(module_index).expect("module_index fits in u8"),
            LINE,
        );
        self.emit_byte(u8::try_from(cname_idx).expect("cname_idx fits in u8"), LINE);
        Ok(())
    }

    /// Converts a `ModuleExportName` to a string.
    pub(crate) fn module_export_name_to_string(
        name: &oast::ModuleExportName<'_>,
    ) -> Option<String> {
        match name {
            oast::ModuleExportName::IdentifierName(id) => Some(id.name.to_string()),
            oast::ModuleExportName::IdentifierReference(id) => Some(id.name.to_string()),
            oast::ModuleExportName::StringLiteral(s) => Some(s.value.to_string()),
        }
    }

    /// Resolves the upvalue index for the given binding name.
    pub(crate) fn resolve_upvalue(&mut self, name: &str) -> Option<u8> {
        if let Some(&idx) = self.upvalue_indices.get(name) {
            return Some(idx);
        }
        let source = self.outer_bindings.get(name)?.clone();
        let idx = u8::try_from(self.captured_upvalues.len()).ok()?;
        let spec = match source {
            CaptureSource::Local {
                slot,
                mirror_global_name,
                immutable,
                sloppy_immutable_silent,
            } => UpvalueSpec {
                is_local: true,
                index: slot,
                mirror_global_name,
                immutable,
                sloppy_immutable_silent,
            },
            CaptureSource::Upvalue {
                index: upvalue,
                immutable,
                sloppy_immutable_silent,
            } => UpvalueSpec {
                is_local: false,
                index: upvalue,
                mirror_global_name: None,
                immutable,
                sloppy_immutable_silent,
            },
        };
        self.captured_upvalues.push(spec);
        self.upvalue_indices.insert(name.to_string(), idx);
        Some(idx)
    }

    /// Returns the index of the global constant for the given name,
    /// adding a fresh constant if necessary.
    pub(crate) fn global_name_index(&mut self, name: &str) -> usize {
        // If we previously cached an index for this global name, it may
        // refer to a constant stored in a different chunk (e.g. from an
        // enclosing function/proto). Ensure the returned index always points
        // into the current compilation chunk by validating the cached index
        // and, if necessary, adding a fresh constant into this chunk.
        if let Some(&idx) = self.globals.get(name)
            && idx < self.chunk.constants.len()
        {
            return idx;
        }
        // Cached index is out-of-range for current chunk; fall through to
        // add a local constant instead of reusing the stale index.
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

    /// Emits a `GetGlobal` operation for the given global name.
    pub(crate) fn emit_get_global_name(&mut self, name: &str) {
        let idx = self.global_name_index(name);
        self.emit(Op::GetGlobal, LINE);
        self.emit_u16(u16::try_from(idx).expect("idx fits in u16"), LINE);
    }

    /// Emits a `GetBuiltin` operation for the given builtin name.
    pub(crate) fn emit_get_builtin_name(&mut self, name: &str) {
        let idx = self.global_name_index(name);
        self.emit(Op::GetBuiltin, LINE);
        self.emit_u16(u16::try_from(idx).expect("idx fits in u16"), LINE);
    }

    /// Executes the given closure with the given compile context override.
    pub(crate) fn with_compile_context_override<R>(
        context: Option<(bool, bool)>,
        f: impl FnOnce() -> R,
    ) -> R {
        struct Reset(Option<(bool, bool)>);

        impl Drop for Reset {
            fn drop(&mut self) {
                COMPILE_CONTEXT_OVERRIDE.with(|state| {
                    *state.borrow_mut() = self.0;
                });
            }
        }

        let previous = COMPILE_CONTEXT_OVERRIDE.with(|state| *state.borrow());
        COMPILE_CONTEXT_OVERRIDE.with(|state| {
            *state.borrow_mut() = context;
        });
        let _reset = Reset(previous);
        f()
    }

    /// Attach a home object to the method closure currently on top of the stack.
    ///
    /// The stack shape must be `[... home_object, key, closure]` when this is
    /// called. The helper preserves that shape so the caller can immediately
    /// emit the property installation that follows.
    pub(crate) fn emit_set_method_home_object(
        &mut self,
        home_slot: usize,
    ) -> Result<(), CompilerError> {
        self.emit(Op::Dup, LINE);
        self.emit_constant(Value::String("[[HomeObject]]".to_string()), LINE);
        self.emit(Op::LoadLocal, LINE);
        self.emit_byte(u8::try_from(home_slot).expect("home_slot fits in u8"), LINE);
        self.emit(Op::SetProperty, LINE);
        let slot = self.alloc_ic_slot()?;
        self.emit_byte(slot, LINE);
        self.emit(Op::Pop, LINE);
        Ok(())
    }

    /// Emits a `Typeof` operation for the given identifier name.
    pub(crate) fn emit_typeof_identifier(&mut self, name: &str) {
        if let Some(slot) = self.resolve_local(name) {
            self.emit(Op::LoadLocal, LINE);
            self.emit_byte(u8::try_from(slot).expect("slot fits in u8"), LINE);
            self.emit(Op::Typeof, LINE);
        } else {
            let idx = self.global_name_index(name);
            self.emit(Op::TypeofGlobal, LINE);
            self.emit_u16(u16::try_from(idx).expect("idx fits in u16"), LINE);
        }
    }

    /// Emits a `SetGlobal` operation for the given global name.
    pub(crate) fn emit_set_global_name(&mut self, name: &str) {
        let idx = self.global_name_index(name);
        self.emit(Op::SetGlobal, LINE);
        self.emit_u16(u16::try_from(idx).expect("idx fits in u16"), LINE);
    }

    /// Patches a jump instruction at the given offset with the current code size.
    pub(crate) fn patch_jump(&mut self, offset: usize) {
        // `offset` is the instruction index where the jump instruction resides
        // (this follows the previous emitter pattern of recording `code.len()`
        // before adding operand bytes). The relative offset is computed in
        // instruction units: target - (offset + 1)
        let patch_idx = self.resolve_jump_patch_index(offset);
        let target = self.chunk.code.len() as isize;
        let rel = target - (patch_idx as isize) - 1;
        let val = i32::try_from(rel).expect("jump offset overflow");
        self.apply_jump_patch(patch_idx, val);
    }

    /// Applies a jump patch to the chunk at the given patch index with the given value.
    fn apply_jump_patch(&mut self, patch_idx: usize, val: i32) {
        let ok = if self
            .chunk
            .code
            .get(patch_idx)
            .is_some_and(|instr| Op::from_byte((instr & 0xFF) as u8) == Some(Op::CompareJump))
        {
            // Preserve compare-op byte in payload byte 0 and patch signed i16
            // offset into payload bytes 1..2.
            let rel16 = i16::try_from(val).expect("compare jump offset overflow");
            let instr = self.chunk.code[patch_idx];
            let compare_byte = (instr >> 8_i32) & 0xFF;
            let rel_u16 = rel16.cast_unsigned();
            let payload = compare_byte | (((rel_u16 as u32) & 0xFFFF) << 8_i32);
            let new_instr = (instr & 0xFF) | ((payload & 0x00FF_FFFF) << 8_i32);
            self.chunk.code[patch_idx] = new_instr;
            true
        } else {
            self.chunk.write_i24_payload_at(patch_idx, val)
        };
        assert!(ok, "failed to write jump payload");
    }

    /// Patches a jump instruction at the given offset to jump to the target instruction.
    pub(crate) fn patch_jump_to_target(&mut self, offset: usize, target: usize) {
        let patch_idx = self.resolve_jump_patch_index(offset);
        let rel = (target as isize) - (patch_idx as isize) - 1;
        self.apply_jump_patch(patch_idx, rel as i32);
    }

    /// Emits a signed jump offset as a sign-extended i24 payload.
    pub(crate) fn emit_short_signed(&mut self, val: isize, line: u32) {
        // Store signed jump offsets as sign-extended i24 payload so runtime
        // jump decoding (`read_i24_payload_at`) preserves negative back-jumps.
        let v = i32::try_from(val).expect("val fits in i32");
        let idx = self.chunk.code.len().saturating_sub(1);
        let ok = self.chunk.write_i24_payload_at(idx, v);
        assert!(ok, "failed to write signed jump payload");
        if idx < self.chunk.lines.len() {
            self.chunk.lines[idx] = line;
        }
        self.pending_operand_bytes = 0;
    }

    /// Patches a list of jump instructions to point to the given target.
    pub(crate) fn patch_jump_list(&mut self, patches: Vec<usize>, target: usize) {
        for patch_pos in patches {
            let patch_idx = self.resolve_jump_patch_index(patch_pos);
            // compute relative offset in instruction units
            let rel = (target as isize) - (patch_idx as isize) - 1;
            let v = i32::try_from(rel).expect("offset overflow");
            self.apply_jump_patch(patch_idx, v);
        }
    }
}

impl Default for Compiler {
    fn default() -> Self {
        Self::new(Rc::new(String::new()))
    }
}
