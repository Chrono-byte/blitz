// SPDX-License-Identifier: MPL-2.0
//! Core Compiler struct and foundational implementation.
//!
//! This module contains the basic structure and initialization logic for the compiler,
//! along with fundamental helper methods for scope management, local variable tracking,
//! and basic bytecode emission.

use super::bytecode::Lowering;
use super::emit::LoweringOpcode;
use crate::bytecode::{Chunk, Constant, UpvalueSpec};
use crate::errors::CompilerError;
use crate::typecheck::Type;
use crate::value::Value;
use oxc_ast::ast as oast;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

const LINE: u32 = 1;

thread_local! {
    static COMPILE_CONTEXT_OVERRIDE: std::cell::RefCell<Option<(bool, bool)>> =
        const { std::cell::RefCell::new(None) };
}

/// Compiles a given function with an overridden compile context.
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
  /// Namespace re-exports: exported name -> dependency module index.
  pub namespace_exports: HashMap<String, usize>,
}

/// Compiles AST to bytecode
pub(crate) struct Compiler {
  /// Current chunk being compiled
  pub chunk: Chunk,
  /// Compiler-owned virtual instruction stream. Frontend operations never
  /// enter `Chunk::words`; only finalized bytecode v2 words are stored there.
  pub(crate) lowering: Lowering,
  /// Original source text for this compilation unit
  pub source_text: Arc<String>,
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
  /// Whether this compilation unit can close over an outer `with` environment.
  pub may_capture_with: bool,
  /// Scopes whose sloppy eval may introduce bindings at runtime.
  pub eval_scopes: Arc<HashSet<oxc_syntax::scope::ScopeId>>,
  pub dynamic_outer_bindings: bool,
  /// Active bytecode exception handlers that must survive a call.
  pub handler_depth: usize,
  /// Stack of continue patch sites for nested loops. Each entry holds a list
  /// of instruction positions whose branch payloads must be patched to jump
  /// to the loop's continue target when the loop is closed.
  pub continue_patches: Vec<Vec<usize>>,
  /// Stack of break patch sites for nested loops. Each entry holds a list of
  /// jump operands that must be patched to the loop exit target.
  pub break_patches: Vec<Vec<usize>>,
  /// Stack of active `try {}` blocks with `finally` clauses so `return`
  /// statements in the protected region can run the finalizer before
  /// unwinding.
  pub finally_patches: Vec<FinallyPatchState>,
  /// Active `for...of` iterators that abrupt control flow must close.
  pub iterator_close_stack: Vec<IteratorCloseState>,
  /// Stack of active labels for `break`/`continue` resolution.
  pub label_patches: Vec<LabelPatchState>,
  /// Optional module context when compiling a module
  pub current_module: Option<ModuleContext>,
  /// Number of operand bytes already emitted into the last instruction's payload.
  /// This lets the emit()/emit_byte() caller pattern pack subsequent operand
  /// bytes into the single 24-bit payload of the instruction word.
  pub pending_operand_bytes: usize,
  /// Whether `new.target` is valid in the current compilation context.
  pub allow_new_target: bool,
  /// Whether `super` property access/calls are valid in the current context.
  pub allow_super: bool,
  /// Whether this compiler is for an async function.
  pub is_async: bool,
  /// Whether this compiler is for a generator function.
  pub is_generator: bool,
  /// Bindings visible from the immediately enclosing function.
  pub outer_bindings: HashMap<String, CaptureSource>,
  /// Upvalue indices allocated in this compiler.
  pub upvalue_indices: HashMap<String, u16>,
  /// Upvalue capture specs for the current function.
  pub captured_upvalues: Vec<UpvalueSpec>,
  /// Private identifiers visible in the current class lexical environment.
  /// Values are unforgeable engine-private symbols copied into nested method compilers.
  pub private_names: HashMap<String, Value>,
  /// Instance fields awaiting initialization after `super()` in a derived constructor.
  pub pending_instance_fields: Vec<PreparedClassField>,
  /// First deferred emission failure. Emission helpers cannot return errors
  /// through every AST visitor, so compilation reports it before lowering.
  pub(crate) emission_error: Option<CompilerError>,
}

/// A class field prepared for constructor or static initialization.
#[derive(Clone)]
pub struct PreparedClassField {
  /// Field name, already reduced to a stable constructor-visible form.
  pub key: PreparedClassFieldKey,
  /// Initializer expression; absent fields initialize to `undefined`.
  pub initializer_source: Option<String>,
  /// Whether this field is installed on the constructor rather than instances.
  pub is_static: bool,
}

/// Stable representation of a class field key.
#[derive(Clone)]
pub enum PreparedClassFieldKey {
  /// Ordinary identifier property name.
  Named(String),
  /// Private field's unforgeable key.
  Private(Value),
  /// Outer local holding a computed property key evaluated during class definition.
  Computed(String),
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
    slot: u16,
    /// Optional global property name mirrored by this binding.
    mirror_global_name: Option<String>,
    /// Export name owned by the captured binding, if any.
    module_export_name: Option<String>,
    /// Whether assignment to this binding must be rejected.
    immutable: bool,
    /// Whether writes in non-strict mode should be silently ignored.
    sloppy_immutable_silent: bool,
  },
  /// Upvalue captured from outer closure.
  Upvalue {
    /// Upvalue index in the enclosing function.
    index: u16,
    /// Export name owned by the captured binding, if any.
    module_export_name: Option<String>,
    /// Whether assignment to this binding must be rejected.
    immutable: bool,
    /// Whether writes in non-strict mode should be silently ignored.
    sloppy_immutable_silent: bool,
  },
}

impl CaptureSource {
  fn module_export_name(&self) -> Option<&str> {
    match self {
      Self::Local {
        module_export_name, ..
      }
      | Self::Upvalue {
        module_export_name, ..
      } => module_export_name.as_deref(),
    }
  }
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
  /// Number of declarative environment records emitted for this scope.
  pub runtime_env_count: usize,
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
  /// Return jumps emitted from the catch clause, which use the catch-side
  /// finalizer copy rather than the normal path.
  pub catch_return_jumps: Vec<usize>,
  /// Jumps from an inner finalizer that must enter this finalizer.
  pub entry_jumps: Vec<usize>,
  /// Abrupt loop completions that must pass through this finalizer.
  pub control_actions: Vec<FinallyControlAction>,
  /// Handler depth before this try installed its exception handler.
  pub handler_depth: usize,
  /// Loop depth at which this try was entered.
  pub loop_depth: usize,
  /// Whether we are currently inside the finalizer.
  pub in_finalizer: bool,
  /// Whether statements are currently compiled in the catch clause.
  pub in_catch: bool,
}

#[derive(Clone, Debug)]
pub struct FinallyControlAction {
  /// Boolean local set by the abrupt statement.
  pub flag_slot: usize,
  /// Whether this is a continue (otherwise it is a break).
  pub is_continue: bool,
  /// Target loop patch list.
  pub loop_index: usize,
  /// Optional label patch list; labels take precedence over loop_index.
  pub label_index: Option<usize>,
  /// Outer finalizer to enter before dispatching the control target.
  pub next_finally: Option<usize>,
}

#[derive(Debug)]
/// An active `for...of` iterator and the loop nesting level that owns it.
pub struct IteratorCloseState {
  /// Local slot containing the iterator object.
  pub iterator_slot: usize,
  /// Loop depth of the owning `for...of` statement.
  pub loop_depth: usize,
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
  /// Current frontend instruction position. Labels and metadata are expressed
  /// in this compiler-owned coordinate space until bytecode finalization.
  pub(crate) fn current_pc(&mut self) -> usize {
    self.lowering.len()
  }

  /// Resolves the patch index for a jump offset, accounting for fixed-width
  /// mode where jumps are patched in place rather than emitted separately.
  fn resolve_jump_patch_index(
    &self,
    offset: usize,
  ) -> usize {
    let op_at = |idx: usize| self.lowering.op_at(idx);
    let is_patchable_jump_at = |idx: usize| {
      op_at(idx).is_some_and(|op| op.is_jump() || matches!(op, LoweringOpcode::PushHandler))
    };

    if offset > 0 && is_patchable_jump_at(offset - 1) {
      offset - 1
    } else {
      offset
    }
  }

  /// Creates a new compiler instance
  pub(crate) fn new(source_text: Arc<String>) -> Self {
    let (allow_new_target, allow_super) = COMPILE_CONTEXT_OVERRIDE
      .with(|override_state| *override_state.borrow())
      .unwrap_or((false, false));
    let mut code = Chunk::new(Arc::clone(&source_text));
    code.allow_new_target = allow_new_target;
    code.allow_super = allow_super;
    Self {
      chunk: code,
      lowering: Lowering::new(),
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
        runtime_env_count: 0,
      }],
      globals: HashMap::new(),
      is_root: true,
      strict_context: false,
      loop_depth: 0,
      with_depth: 0,
      may_capture_with: false,
      eval_scopes: Arc::new(HashSet::new()),
      dynamic_outer_bindings: false,
      handler_depth: 0,
      continue_patches: Vec::new(),
      break_patches: Vec::new(),
      finally_patches: Vec::new(),
      iterator_close_stack: Vec::new(),
      label_patches: Vec::new(),
      current_module: None,
      pending_operand_bytes: 0,
      allow_new_target,
      allow_super,
      is_async: false,
      is_generator: false,
      outer_bindings: HashMap::new(),
      upvalue_indices: HashMap::new(),
      captured_upvalues: Vec::new(),
      private_names: HashMap::new(),
      pending_instance_fields: Vec::new(),
      emission_error: None,
    }
  }

  /// Returns true if a tail call can be emitted in the current context.
  pub(crate) fn can_emit_tail_call(&self) -> bool {
    self.strict_context
      && self.finally_patches.is_empty()
      && self.handler_depth == 0
      && self.iterator_close_stack.is_empty()
      && self.with_depth == 0
      && !self.is_async
      && !self.is_generator
  }

  /// Compute a 1-based source line for a span. Falls back to 1 when out of bounds.
  #[allow(dead_code)]
  pub(crate) fn line_for_span(
    &self,
    span: oxc_span::Span,
  ) -> u32 {
    let source = self.source_text.as_str();
    let offset = span.start as usize;
    if offset > source.len() {
      return 1;
    }
    source[..offset]
      .bytes()
      .filter(|&b| b == b'\n')
      .count() as u32
      + 1
  }

  /// Emits a single instruction word with an 8-bit opcode and zero payload.
  pub(crate) fn emit(
    &mut self,
    op: LoweringOpcode,
    line: u32,
  ) {
    self.lowering.emit(op, 0, line);
    self.pending_operand_bytes = 0;
  }

  pub(crate) fn emit_fallthrough_return(
    &mut self,
    line: u32,
  ) {
    if self.lowering.is_reachable() {
      self.emit(LoweringOpcode::Undefined, line);
      self.emit(LoweringOpcode::Return, line);
    }
  }

  /// Emit a single instruction word with a 24-bit payload packed into bits 8..31.
  /// This is the preferred path for fixed-width fixed-width instructions where
  /// the opcode and all operands must be written as one u32.
  pub(crate) fn emit_with_payload(
    &mut self,
    op: LoweringOpcode,
    payload24: u32,
    line: u32,
  ) {
    self.lowering.emit(op, payload24, line);
    self.pending_operand_bytes = 0;
  }

  /// Emit a single operand byte packed into the pending instruction's 24-bit payload.
  ///
  /// This appends an operand to a fixed-width instruction's pending 24-bit
  /// payload. The opcode is emitted first and subsequent operands occupy the
  /// payload bytes of that same instruction word.
  pub(crate) fn emit_byte(
    &mut self,
    byte: u8,
    line: u32,
  ) {
    if self.lowering.is_empty() {
      self
        .emission_error
        .get_or_insert_with(|| CompilerError::from("compiler emitted operand without instruction"));
      return;
    }
    if self.pending_operand_bytes >= 3 {
      self.emission_error.get_or_insert_with(|| {
        CompilerError::from("too many operand bytes for instruction payload")
      });
      return;
    }
    self
      .lowering
      .append_operand_byte(self.pending_operand_bytes, byte, line);
    self.pending_operand_bytes += 1;
  }

  /// Encode a local-frame slot in the 16-bit auxiliary field used by bytecode v2.
  pub(crate) fn emit_slot(
    &mut self,
    slot: usize,
    line: u32,
  ) {
    let Ok(slot) = u16::try_from(slot) else {
      self.emission_error.get_or_insert_with(|| {
        CompilerError::from(format!("Too many local variables (max 65535, got {slot})"))
      });
      return;
    };
    self
      .lowering
      .set_last_payload(u32::from(slot), line);
    self.pending_operand_bytes = 2;
  }

  /// Emits a u16 value as a 24-bit payload in a single instruction word,
  /// avoiding multi-write emissions which break the fixed-width instruction model.
  pub(crate) fn emit_u16(
    &mut self,
    value: u16,
    line: u32,
  ) {
    let payload = value as u32 & 0xFFFF;
    self.lowering.set_last_payload(payload, line);
    self.pending_operand_bytes = 0;
  }

  /// Emits a constant value as a `Const` instruction with the constant index
  /// encoded into the 24-bit payload in a single instruction word to avoid
  /// multi-write emissions which break the fixed-width instruction model.
  pub(crate) fn emit_constant(
    &mut self,
    value: Value,
    line: u32,
  ) {
    let constant = match Constant::try_from(value) {
      Ok(constant) => constant,
      Err(_) => {
        self
          .emission_error
          .get_or_insert_with(|| CompilerError::from("compiler emitted a runtime-owned constant"));
        return;
      }
    };
    let idx = self.chunk.add_constant(constant);
    let Ok(idx) = u16::try_from(idx) else {
      self
        .emission_error
        .get_or_insert_with(|| CompilerError::from("constant pool exceeds 65536 entries"));
      return;
    };
    let idx = u32::from(idx);
    self.emit_with_payload(LoweringOpcode::Const, idx, line);
  }

  /// Encode a nested-function index in the 16-bit auxiliary field used by bytecode v2.
  pub(crate) fn emit_function_index(
    &mut self,
    idx: usize,
    line: u32,
  ) {
    let Ok(idx) = u16::try_from(idx) else {
      self.emission_error.get_or_insert_with(|| {
        CompilerError::from(format!("Too many functions (max 65535, got {idx})"))
      });
      return;
    };
    self
      .lowering
      .set_last_payload(u32::from(idx), line);
    self.pending_operand_bytes = 2;
  }

  /// Add a compact module-index/export-name descriptor to the constant pool.
  ///
  /// Module instructions dedicate their sixteen auxiliary bits to this
  /// descriptor, avoiding separate eight-bit limits for module and export
  /// indices while retaining the fixed-width encoding.
  pub(crate) fn module_export_descriptor_index(
    &mut self,
    module_index: usize,
    name: &str,
  ) -> Result<u16, CompilerError> {
    let descriptor = Constant::String(format!("{module_index}\0{name}").into());
    let index = self
      .chunk
      .constants
      .iter()
      .position(|value| value == &descriptor)
      .unwrap_or_else(|| self.chunk.add_constant(descriptor));
    u16::try_from(index)
      .map_err(|_| CompilerError::from(format!("Too many constants (max 65535, got {index})")))
  }

  /// Safely converts a constant index (usize) to u8, checking that it doesn't exceed 255.
  pub(crate) fn constant_index_to_u8(
    &self,
    idx: usize,
  ) -> Result<u8, CompilerError> {
    u8::try_from(idx)
      .map_err(|_| CompilerError::from(format!("Too many constants (max 255, got {})", idx)))
  }

  /// Safely converts a global index (usize) to u16, checking that it doesn't exceed 65535.
  pub(crate) fn global_index_to_u16(
    &self,
    idx: usize,
  ) -> Result<u16, CompilerError> {
    u16::try_from(idx).map_err(|_| {
      CompilerError::from(format!(
        "Too many global constants (max 65535, got {})",
        idx
      ))
    })
  }

  /// Returns a compiler error for a strict binding name (eval/arguments).
  pub(crate) fn strict_binding_name_error(
    strict_context: bool,
    name: &str,
  ) -> Option<CompilerError> {
    if strict_context
      && matches!(
        name,
        "eval"
          | "arguments"
          | "implements"
          | "interface"
          | "let"
          | "package"
          | "private"
          | "protected"
          | "public"
          | "static"
          | "yield"
      )
    {
      Some(CompilerError::from(format!(
        "SyntaxError: `{}` is not allowed in strict mode",
        name
      )))
    } else {
      None
    }
  }

  /// Returns a compiler error for a strict-mode reserved identifier reference.
  pub(crate) fn strict_identifier_reference_error(
    strict_context: bool,
    name: &str,
  ) -> Option<CompilerError> {
    if strict_context
      && matches!(
        name,
        "implements"
          | "interface"
          | "let"
          | "package"
          | "private"
          | "protected"
          | "public"
          | "static"
          | "yield"
      )
    {
      Some(CompilerError::from(format!(
        "SyntaxError: `{name}` is not allowed in strict mode"
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
      OPat::ObjectPattern(o) => o
        .properties
        .iter()
        .find_map(|prop| Self::strict_binding_pattern_name_error(strict_context, &prop.value)),
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

  /// Resolves a local variable by name, returning its slot index if found.
  pub(crate) fn resolve_local(
    &self,
    name: &str,
  ) -> Option<usize> {
    self
      .scopes
      .iter()
      .rev()
      .find_map(|scope| scope.bindings.get(name).copied())
  }

  /// Returns the index of the current function scope.
  pub(crate) fn current_function_scope_index(&self) -> usize {
    self
      .scopes
      .iter()
      .rposition(|scope| scope.kind == ScopeKind::Function)
      .unwrap_or(0)
  }

  /// Adds a local variable to the given scope, returning its slot index.
  pub(crate) fn add_local_in_scope(
    &mut self,
    scope_index: usize,
    name: String,
    type_ann: Option<Type>,
    mirror_global: bool,
  ) -> usize {
    if let Some(slot) = self.scopes[scope_index]
      .bindings
      .get(&name)
      .copied()
    {
      return slot;
    }
    let slot = self.locals.len();
    self.locals.push(name.clone());
    self.local_types.push(type_ann);
    self.local_tdz.push(false);
    self.local_immutable.push(false);
    self.local_sloppy_immutable_silent.push(false);
    self.local_mirror_globals.push(mirror_global);
    self.scopes[scope_index]
      .bindings
      .insert(name, slot);
    slot
  }

  /// Marks a local variable as being in the TDZ (Temporal Dead Zone).
  pub(crate) fn mark_local_tdz(
    &mut self,
    slot: usize,
  ) {
    if let Some(flag) = self.local_tdz.get_mut(slot) {
      *flag = true;
    }
  }

  /// Marks a local variable as immutable after its first initialization.
  pub(crate) fn mark_local_immutable(
    &mut self,
    slot: usize,
  ) {
    if let Some(flag) = self.local_immutable.get_mut(slot) {
      *flag = true;
    }
  }

  /// Adds a lexical binding (used for `let`/`const` bindings).
  pub(crate) fn add_lexical_binding(
    &mut self,
    name: &str,
  ) {
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
  pub(crate) fn check_var_lexical_conflict(
    &self,
    name: &str,
  ) -> Option<String> {
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
      oast::Statement::WhileStatement(w) => {
        self.add_var_names_from_statement(&w.body, names);
      }
      oast::Statement::DoWhileStatement(d) => {
        self.add_var_names_from_statement(&d.body, names);
      }
      oast::Statement::ForStatement(f) => {
        if let Some(oast::ForStatementInit::VariableDeclaration(v)) = &f.init
          && matches!(v.kind, oxc_ast::ast::VariableDeclarationKind::Var)
        {
          for declaration in &v.declarations {
            self.add_var_names_from_binding(&declaration.id, names);
          }
        }
        self.add_var_names_from_statement(&f.body, names);
      }
      oast::Statement::ForInStatement(f) => {
        if let oast::ForStatementLeft::VariableDeclaration(v) = &f.left
          && matches!(v.kind, oxc_ast::ast::VariableDeclarationKind::Var)
        {
          for declaration in &v.declarations {
            self.add_var_names_from_binding(&declaration.id, names);
          }
        }
        self.add_var_names_from_statement(&f.body, names);
      }
      oast::Statement::ForOfStatement(f) => {
        if let oast::ForStatementLeft::VariableDeclaration(v) = &f.left
          && matches!(v.kind, oxc_ast::ast::VariableDeclarationKind::Var)
        {
          for declaration in &v.declarations {
            self.add_var_names_from_binding(&declaration.id, names);
          }
        }
        self.add_var_names_from_statement(&f.body, names);
      }
      oast::Statement::SwitchStatement(s) => {
        for case in &s.cases {
          for statement in &case.consequent {
            self.add_var_names_from_statement(statement, names);
          }
        }
      }
      oast::Statement::TryStatement(t) => {
        for statement in &t.block.body {
          self.add_var_names_from_statement(statement, names);
        }
        if let Some(handler) = &t.handler {
          for statement in &handler.body.body {
            self.add_var_names_from_statement(statement, names);
          }
        }
        if let Some(finalizer) = &t.finalizer {
          for statement in &finalizer.body {
            self.add_var_names_from_statement(statement, names);
          }
        }
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
      runtime_env_count: 0,
    });
  }

  pub(crate) fn mirrors_root_bindings(&self) -> bool {
    self.is_root && self.current_module.is_none()
  }

  /// Enters a block scope with the given block var names.
  pub(crate) fn enter_block_scope_with_vars(
    &mut self,
    stmts: &[oast::Statement<'_>],
  ) {
    self.enter_block_scope_with_vars_mode(stmts, false);
  }

  pub(crate) fn enter_function_body_scope_with_vars(
    &mut self,
    stmts: &[oast::Statement<'_>],
    separate_var_environment: bool,
  ) {
    self.enter_block_scope_with_vars_mode(stmts, separate_var_environment);
  }

  fn enter_block_scope_with_vars_mode(
    &mut self,
    stmts: &[oast::Statement<'_>],
    separate_var_environment: bool,
  ) {
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
      runtime_env_count: 0,
    });
    self.pre_declare_variables_oast_stmts(stmts);
    if separate_var_environment {
      self.activate_current_scope_var_bindings();
    }
    self.activate_current_scope_lexical_bindings();
  }

  fn activate_current_scope_var_bindings(&mut self) {
    let mut bindings: Vec<_> = self
      .scopes
      .last()
      .into_iter()
      .flat_map(|scope| scope.block_var_names.iter())
      .filter_map(|name| {
        self
          .resolve_local(name)
          .map(|slot| (slot, name.clone()))
      })
      .collect();
    bindings.sort_by_key(|(slot, _)| *slot);
    for (slot, name) in &bindings {
      self.emit_constant(crate::value::Value::String(name.clone().into()), 1);
      self.emit_with_payload(LoweringOpcode::PushLexicalEnv, *slot as u32, 1);
    }
    if let Some(scope) = self.scopes.last_mut() {
      scope.runtime_env_count += bindings.len();
    }
  }

  pub(crate) fn activate_current_scope_lexical_bindings(&mut self) {
    let mut bindings: Vec<_> = self
      .scopes
      .last()
      .into_iter()
      .flat_map(|scope| scope.lexical_bindings.iter())
      .filter_map(|name| {
        self
          .scopes
          .last()?
          .bindings
          .get(name)
          .copied()
          .map(|slot| (slot, name.clone()))
      })
      .collect();
    bindings.sort_by_key(|(slot, _)| *slot);
    for (slot, name) in &bindings {
      self.emit_constant(crate::value::Value::String(name.clone().into()), 1);
      self.emit_with_payload(LoweringOpcode::PushLexicalEnv, *slot as u32, 1);
    }
    if let Some(scope) = self.scopes.last_mut() {
      scope.runtime_env_count += bindings.len();
    }
  }

  /// Names that belong to the function/root environment. Block bindings and
  /// compiler-generated temporaries still use frame slots, but must not become
  /// observable bindings in the surrounding environment.
  pub(crate) fn environment_slot_names(&self) -> Vec<Option<String>> {
    let mut names = vec![None; self.locals.len()];
    if let Some(function_scope) = self.scopes.first() {
      for (name, &slot) in &function_scope.bindings {
        if slot < names.len() && !name.starts_with('\0') {
          names[slot] = Some(name.clone());
        }
      }
    }
    names
  }

  /// Exits the current scope, popping it from the scope stack.
  pub(crate) fn exit_scope(&mut self) {
    let Some(scope) = self.scopes.pop() else {
      self
        .emission_error
        .get_or_insert_with(|| CompilerError::from("compiler scope error: missing active scope"));
      return;
    };
    for _ in 0..scope.runtime_env_count {
      self.emit(LoweringOpcode::PopEnv, 1);
    }
    if matches!(scope.kind, ScopeKind::Function) && self.scopes.is_empty() {
      self.emission_error.get_or_insert_with(|| {
        CompilerError::from("compiler scope error: popped last function scope")
      });
    }
  }

  /// Returns a snapshot of the current visible bindings from all scopes.
  pub(crate) fn visible_bindings_snapshot(&mut self) -> HashMap<String, CaptureSource> {
    let mut bindings = HashMap::new();
    let mut overflowing_slot = None;
    for (scope_index, scope) in self.scopes.iter().enumerate() {
      for (name, slot) in &scope.bindings {
        let Ok(capture_slot) = u16::try_from(*slot) else {
          overflowing_slot.get_or_insert(*slot);
          continue;
        };
        let module_export_name = (self.is_root
          && scope_index == 0
          && self
            .current_module
            .as_ref()
            .is_some_and(|module| module.exports.contains(name)))
        .then(|| name.clone());
        bindings.insert(
          name.clone(),
          CaptureSource::Local {
            slot: capture_slot,
            mirror_global_name: if self.local_mirrors_global(*slot) {
              Some(name.clone())
            } else {
              None
            },
            module_export_name,
            immutable: self
              .local_immutable
              .get(*slot)
              .copied()
              .unwrap_or(false),
            sloppy_immutable_silent: self
              .local_sloppy_immutable_silent
              .get(*slot)
              .copied()
              .unwrap_or(false),
          },
        );
      }
    }
    if let Some(slot) = overflowing_slot {
      self.emission_error.get_or_insert_with(|| {
        CompilerError::from(format!("Too many local variables (max 65535, got {slot})"))
      });
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
          module_export_name: self
            .outer_bindings
            .get(name)
            .and_then(CaptureSource::module_export_name)
            .map(str::to_owned),
          immutable,
          sloppy_immutable_silent,
        });
    }
    let mut outer_names: Vec<String> = self.outer_bindings.keys().cloned().collect();
    outer_names.sort();
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
          name.clone(),
          CaptureSource::Upvalue {
            index: idx,
            module_export_name: self
              .outer_bindings
              .get(&name)
              .and_then(CaptureSource::module_export_name)
              .map(str::to_owned),
            immutable,
            sloppy_immutable_silent,
          },
        );
      }
    }
    bindings
  }

  /// Returns true if the local variable at the given slot mirrors the global object.
  pub(crate) fn local_mirrors_global(
    &self,
    slot: usize,
  ) -> bool {
    self
      .local_mirror_globals
      .get(slot)
      .copied()
      .unwrap_or(false)
  }

  /// Returns whether this root-scope slot is the binding that owns `name`'s
  /// module export. Export membership alone is not sufficient: a nested scope
  /// may legally contain a different binding with the same spelling.
  pub(crate) fn owns_local_module_export(
    &self,
    name: &str,
    slot: usize,
  ) -> bool {
    self.is_root
      && self
        .scopes
        .first()
        .and_then(|scope| scope.bindings.get(name))
        == Some(&slot)
      && self
        .current_module
        .as_ref()
        .is_some_and(|module| module.exports.contains(name))
  }

  /// Returns whether this compiler may publish an update for `name`.
  ///
  /// The root compiler owns declarations and aliases. Nested compilers may
  /// publish only when their captured source was identified as that root
  /// module binding.
  fn owns_module_export(
    &self,
    name: &str,
  ) -> bool {
    self.is_root
      || self
        .outer_bindings
        .get(name)
        .and_then(CaptureSource::module_export_name)
        .is_some_and(|export_name| export_name == name)
  }

  pub(crate) fn emit_sync_local_module_export(
    &mut self,
    name: &str,
    slot: usize,
  ) -> Result<(), CompilerError> {
    if self.owns_local_module_export(name, slot) {
      self.emit(LoweringOpcode::LoadLocal, LINE);
      self.emit_slot(slot, LINE);
      self.emit_set_own_module_export(name)?;
    }
    Ok(())
  }

  pub(crate) fn emit_sync_upvalue_module_export(
    &mut self,
    upvalue: u16,
  ) -> Result<(), CompilerError> {
    if let Some((upvalue, export_name)) = self.module_export_for_upvalue(upvalue) {
      self.emit(LoweringOpcode::GetUpvalue, LINE);
      self.emit_u16(upvalue, LINE);
      self.emit_set_own_module_export(&export_name)?;
    }
    Ok(())
  }

  fn module_export_for_upvalue(
    &self,
    upvalue: u16,
  ) -> Option<(u16, String)> {
    self
      .upvalue_indices
      .iter()
      .find_map(|(name, index)| {
        (*index == upvalue)
          .then(|| {
            self
              .outer_bindings
              .get(name)
              .and_then(CaptureSource::module_export_name)
              .map(|export_name| (upvalue, export_name.to_owned()))
          })
          .flatten()
      })
  }

  /// Emits a `LoadLocal` or `GetUpvalue` operation for the given binding name.
  pub(crate) fn emit_get_binding(
    &mut self,
    name: &str,
  ) {
    if self.with_depth > 0 || self.may_capture_with {
      let reference_slot = self.alloc_temp_local("with_identifier_reference");
      self.emit_get_builtin_name("resolveWithBindingObject");
      self.emit_constant(Value::String(name.into()), LINE);
      self.emit(LoweringOpcode::Call, LINE);
      self.emit_byte(1, LINE);
      self.emit(LoweringOpcode::StoreLocal, LINE);
      self.emit_slot(reference_slot, LINE);
      self.emit_get_builtin_name("getResolvedIdentifier");
      self.emit(LoweringOpcode::LoadLocal, LINE);
      self.emit_slot(reference_slot, LINE);
      self.emit_constant(Value::String(name.into()), LINE);
      self.emit(LoweringOpcode::Call, LINE);
      self.emit_byte(2, LINE);
    } else if let Some(slot) = self.resolve_local(name) {
      self.emit(LoweringOpcode::LoadLocal, LINE);
      self.emit_slot(slot, LINE);
    } else if let Some(upvalue) = self.resolve_upvalue(name) {
      self.emit(LoweringOpcode::GetUpvalue, LINE);
      self.emit_u16(upvalue, LINE);
    } else if let Some((src_idx, exported_name)) = self
      .current_module
      .as_ref()
      .and_then(|c| c.imports.get(name).map(|(i, n)| (*i, n.clone())))
    {
      let descriptor = self
        .module_export_descriptor_index(src_idx, &exported_name)
        .expect("module export descriptor fits in the constant pool");
      self.emit(LoweringOpcode::GetModuleExport, LINE);
      self.emit_byte(descriptor.to_le_bytes()[0], LINE);
      self.emit_byte(descriptor.to_le_bytes()[1], LINE);
    } else {
      self.emit_get_global_name(name);
    }
  }

  /// Emits a `SetModuleExport` operation for the given export name.
  pub(crate) fn emit_set_own_module_export(
    &mut self,
    export_name: &str,
  ) -> Result<(), CompilerError> {
    if !self.owns_module_export(export_name) {
      self.emit(LoweringOpcode::Discard, LINE);
      return Ok(());
    }
    let module_index = self
      .current_module
      .as_ref()
      .map(|c| c.module_index)
      .ok_or_else(|| CompilerError::from("module_index expected"))?;
    let descriptor = self.module_export_descriptor_index(module_index, export_name)?;
    self.emit(LoweringOpcode::SetModuleExport, LINE);
    self.emit_byte(descriptor.to_le_bytes()[0], LINE);
    self.emit_byte(descriptor.to_le_bytes()[1], LINE);
    Ok(())
  }

  /// Converts a `ModuleExportName` to a string.
  pub(crate) fn module_export_name_to_string(name: &oast::ModuleExportName<'_>) -> Option<String> {
    match name {
      oast::ModuleExportName::IdentifierName(id) => Some(id.name.to_string()),
      oast::ModuleExportName::IdentifierReference(id) => Some(id.name.to_string()),
      oast::ModuleExportName::StringLiteral(s) => Some(s.value.to_string()),
    }
  }

  /// Resolves the upvalue index for the given binding name.
  pub(crate) fn resolve_upvalue(
    &mut self,
    name: &str,
  ) -> Option<u16> {
    if self.dynamic_outer_bindings {
      return None;
    }
    if let Some(&idx) = self.upvalue_indices.get(name) {
      return Some(idx);
    }
    let source = self.outer_bindings.get(name)?.clone();
    let idx = u16::try_from(self.captured_upvalues.len()).ok()?;
    let spec = match source {
      CaptureSource::Local {
        slot,
        mirror_global_name,
        immutable,
        sloppy_immutable_silent,
        ..
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
        ..
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
  pub(crate) fn global_name_index(
    &mut self,
    name: &str,
  ) -> usize {
    if let Some(&idx) = self.globals.get(name)
      && idx < self.chunk.constants.len()
    {
      return idx;
    }
    let idx = self
      .chunk
      .add_constant(Constant::String(name.to_string().into()));
    self.globals.insert(name.to_string(), idx);
    idx
  }

  /// Ensure an unforgeable private key exists for `name` in the active class scope.
  pub(crate) fn ensure_private_name_constant(
    &mut self,
    name: &str,
  ) -> Value {
    if let Some(value) = self.private_names.get(name) {
      return value.clone();
    }
    let value = Value::Symbol(crate::value::SymbolId::new_private(), None);
    self
      .private_names
      .insert(name.to_string(), value.clone());
    value
  }

  /// Emits a `GetGlobal` operation for the given global name.
  pub(crate) fn emit_get_global_name(
    &mut self,
    name: &str,
  ) {
    let idx = self.global_name_index(name);
    let idx_u16 = match u16::try_from(idx) {
      Ok(v) => v,
      Err(_) => {
        self.emission_error.get_or_insert_with(|| {
          CompilerError::from(format!("Too many constants (max 65535, got {idx})"))
        });
        return;
      }
    };
    self.emit(LoweringOpcode::GetGlobal, LINE);
    self.emit_u16(idx_u16, LINE);
  }

  /// Emits a `GetBuiltin` operation for the given builtin name.
  pub(crate) fn emit_get_builtin_name(
    &mut self,
    name: &str,
  ) {
    let idx = self.global_name_index(name);
    let idx_u16 = match u16::try_from(idx) {
      Ok(v) => v,
      Err(_) => {
        self.emission_error.get_or_insert_with(|| {
          CompilerError::from(format!("Too many constants (max 65535, got {idx})"))
        });
        return;
      }
    };
    self.emit(LoweringOpcode::GetBuiltin, LINE);
    self.emit_u16(idx_u16, LINE);
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

  /// Attach a home object to the method closure currently on top of the
  /// compiler's virtual value stack.
  ///
  /// The virtual value stack shape must be `[... home_object, key, closure]` when this is
  /// called. The helper preserves that shape so the caller can immediately
  /// emit the property installation that follows.
  pub(crate) fn emit_set_method_home_object(
    &mut self,
    home_slot: usize,
  ) -> Result<(), CompilerError> {
    self.emit(LoweringOpcode::Copy, LINE);
    self.emit_constant(Value::String("[[HomeObject]]".to_string().into()), LINE);
    self.emit(LoweringOpcode::LoadLocal, LINE);
    self.emit_slot(home_slot, LINE);
    self.emit(LoweringOpcode::SetProperty, LINE);
    self.emit(LoweringOpcode::Discard, LINE);
    Ok(())
  }

  /// Emits a `Typeof` operation for the given identifier name.
  pub(crate) fn emit_typeof_identifier(
    &mut self,
    name: &str,
  ) {
    if let Some(slot) = self.resolve_local(name) {
      self.emit(LoweringOpcode::LoadLocal, LINE);
      self.emit_slot(slot, LINE);
      self.emit(LoweringOpcode::Typeof, LINE);
    } else {
      let idx = self.global_name_index(name);
      let idx_u16 = match u16::try_from(idx) {
        Ok(v) => v,
        Err(_) => {
          self.emission_error.get_or_insert_with(|| {
            CompilerError::from(format!("Too many constants (max 65535, got {idx})"))
          });
          return;
        }
      };
      self.emit(LoweringOpcode::TypeofGlobal, LINE);
      self.emit_u16(idx_u16, LINE);
    }
  }

  /// Emits a `SetGlobal` operation for the given global name.
  pub(crate) fn emit_set_global_name(
    &mut self,
    name: &str,
  ) {
    let idx = self.global_name_index(name);
    let idx_u16 = match u16::try_from(idx) {
      Ok(v) => v,
      Err(_) => {
        self.emission_error.get_or_insert_with(|| {
          CompilerError::from(format!("Too many constants (max 65535, got {idx})"))
        });
        return;
      }
    };
    self.emit(LoweringOpcode::SetGlobal, LINE);
    self.emit_u16(idx_u16, LINE);
  }

  /// Patches a jump instruction at the given offset with the current code size.
  pub(crate) fn patch_jump(
    &mut self,
    offset: usize,
  ) {
    let patch_idx = self.resolve_jump_patch_index(offset);
    let target = self.current_pc() as isize;
    let rel = target - (patch_idx as isize) - 1;
    let Ok(val) = i32::try_from(rel) else {
      self
        .emission_error
        .get_or_insert_with(|| CompilerError::from("jump offset overflow"));
      return;
    };
    self.apply_jump_patch(patch_idx, val);
  }

  /// Applies a jump patch to the code at the given patch index with the given value.
  fn apply_jump_patch(
    &mut self,
    patch_idx: usize,
    val: i32,
  ) {
    self.lowering.patch_branch(patch_idx, val);
  }

  /// Patches a jump instruction at the given offset to jump to the target instruction.
  pub(crate) fn patch_jump_to_target(
    &mut self,
    offset: usize,
    target: usize,
  ) {
    let patch_idx = self.resolve_jump_patch_index(offset);
    let rel = (target as isize) - (patch_idx as isize) - 1;
    let Ok(rel_i32) = i32::try_from(rel) else {
      self
        .emission_error
        .get_or_insert_with(|| CompilerError::from("jump offset exceeds i32 range"));
      return;
    };
    self.apply_jump_patch(patch_idx, rel_i32);
  }

  /// Emits a signed jump offset as a sign-extended i24 payload.
  pub(crate) fn emit_short_signed(
    &mut self,
    val: isize,
    line: u32,
  ) {
    let Ok(v) = i32::try_from(val) else {
      self
        .emission_error
        .get_or_insert_with(|| CompilerError::from("jump offset exceeds i32 range"));
      return;
    };
    let idx = self.current_pc().saturating_sub(1);
    self.lowering.patch_branch_line(idx, v, line);
    self.pending_operand_bytes = 0;
  }

  /// Patches a list of jump instructions to point to the given target.
  pub(crate) fn patch_jump_list(
    &mut self,
    patches: Vec<usize>,
    target: usize,
  ) {
    for patch_pos in patches {
      let patch_idx = self.resolve_jump_patch_index(patch_pos);
      let rel = (target as isize) - (patch_idx as isize) - 1;
      let Ok(v) = i32::try_from(rel) else {
        self
          .emission_error
          .get_or_insert_with(|| CompilerError::from("jump offset overflow"));
        return;
      };
      self.apply_jump_patch(patch_idx, v);
    }
  }
}

impl Default for Compiler {
  fn default() -> Self {
    Self::new(Arc::new(String::new()))
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn constant_pool_overflow_records_a_compiler_error() {
    let mut compiler = Compiler::default();
    for value in 0..=u16::MAX {
      compiler.emit_constant(Value::Number(f64::from(value)), LINE);
    }
    assert!(compiler.emission_error.is_none());

    compiler.emit_constant(Value::Number(65_536.0), LINE);

    let error = compiler
      .emission_error
      .expect("constant pool overflow must be reported");
    assert!(error.message.contains("constant pool exceeds"));
  }

  #[test]
  fn module_export_ownership_follows_the_binding_slot() {
    let mut compiler = Compiler {
      current_module: Some(ModuleContext {
        module_index: 0,
        imports: HashMap::new(),
        exports: HashSet::from(["x".to_string()]),
        namespace_exports: HashMap::new(),
      }),
      ..Compiler::default()
    };
    let root_slot = compiler.add_local_with_mirror("x".to_string(), None, false);
    assert!(compiler.owns_local_module_export("x", root_slot));

    compiler.enter_block_scope();
    let shadow_slot = compiler.add_local_with_mirror("x".to_string(), None, false);
    assert!(!compiler.owns_local_module_export("x", shadow_slot));

    let mut nested = Compiler {
      is_root: false,
      current_module: compiler.current_module.clone(),
      ..Compiler::default()
    };
    nested.outer_bindings.insert(
      "x".to_string(),
      CaptureSource::Local {
        slot: u16::try_from(root_slot).expect("test slot fits"),
        mirror_global_name: None,
        module_export_name: Some("x".to_string()),
        immutable: false,
        sloppy_immutable_silent: false,
      },
    );
    assert!(nested.owns_module_export("x"));
  }
}
