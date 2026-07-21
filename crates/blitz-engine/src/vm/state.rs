//! Stack-based bytecode interpreter with runtime safety helpers.
//!
//! This module implements the VM core: a fetch-decode-execute loop that
//! evaluates compiled bytecode in a stack-based environment. It contains
//! the Vm struct, frame/handler implementations, and tuning constants used
//! by the runtime.
//!
//! Key responsibilities:
//! - Manage the operand stack and locals
//! - Maintain call frames, exception handlers, and generator state
//! - Provide property access support with generation-guarded inline
//!   caching to accelerate repeated lookups
//! - Integrate with a background runtime for blocking/async work
//!
//! Runtime safety:
//! When type annotations are available the VM performs runtime checks on
//! local and global assignments as well as function arguments. These checks
//! are defensive and intended to surface type errors during development.
//!
//! Example
//!
//! ```
//! use blitz::vm::Vm;
//! use blitz::bytecode::Chunk;
//! use std::cell::RefCell;
//! use std::rc::Rc;
//! use blitz::value::ObjectData;
//!
//! let globals = Rc::new(RefCell::new(ObjectData::new()));
//! let mut vm = Vm::new(globals);
//! // vm.run(chunk)?;
//! ```

use crate::bytecode::{Chunk, OP_COUNT};
use crate::environment::EnvironmentRecord;
use crate::module::ModuleLoader;
use crate::realm::Realm;
use crate::typecheck::Type;
use crate::value::{ObjectData, SharedValue, Value};
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::env;
use std::rc::{Rc, Weak};
use std::sync::Mutex;
use std::sync::{Arc, OnceLock};
use std::time::Instant;
use tokio::runtime::Runtime;

mod frame;
mod generator_runner;
mod handler;
mod partial_descriptor;
mod stack;
mod yield_star;

pub(crate) use frame::Frame;
pub(crate) use generator_runner::GeneratorRunner;
pub(crate) use handler::{FinallyHandlerRecord, HandlerRecord, RiscExecutionState, RiscRunOutcome};
pub(crate) use partial_descriptor::PartialDescriptor;
pub(crate) use stack::{CallStack, HandlerStack};
pub(crate) use yield_star::{YieldStarResume, YieldStarState};

// ---------------------------------------------------------------------------
// Tuning constants
// ---------------------------------------------------------------------------

/// Initial operand stack capacity. Avoids reallocation for most programs.
pub(super) const STACK_INITIAL_CAPACITY: usize = 512;

/// Initial locals vector capacity per call frame.
pub(super) const LOCALS_INITIAL_CAPACITY: usize = 64;

/// Initial call-frame stack capacity.
pub(super) const FRAMES_INITIAL_CAPACITY: usize = 64;

/// Maximum prototype chain traversal depth before we assume a cycle.
pub(super) const MAX_PROTOTYPE_DEPTH: usize = 128;

/// Maximum gap we materialize into the dense backing store before treating an
/// array write as sparse and storing it on the property bag instead.
pub(super) const MAX_DENSE_ARRAY_GROWTH: usize = 1024;

/// Number of tracked heap allocations between opportunistic GC attempts.
///
/// The prototype collector is intentionally pressure-driven instead of
/// step-driven so tight numeric loops do not pay GC overhead unless they are
/// actually creating heap garbage.
pub(super) const GC_ALLOCATION_PRESSURE_THRESHOLD: usize = 128;

/// Runtime GC pressure threshold, overrideable via `TSBLITZ_GC_PRESSURE_THRESHOLD`.
pub(super) fn gc_allocation_pressure_threshold() -> usize {
  static THRESHOLD: OnceLock<usize> = OnceLock::new();
  *THRESHOLD.get_or_init(|| {
    env::var("TSBLITZ_GC_PRESSURE_THRESHOLD")
      .ok()
      .and_then(|s| s.parse::<usize>().ok())
      .filter(|n| *n > 0)
      .unwrap_or(GC_ALLOCATION_PRESSURE_THRESHOLD)
  })
}

type Microtask = Box<dyn FnMut(&mut Vm) -> Result<(), crate::errors::RuntimeError>>;
type DynamicImportJob = Box<dyn FnMut(&mut Vm) -> Result<(), crate::errors::RuntimeError>>;

#[derive(Clone)]
pub(super) struct PropertyGetIcEntry {
  pub(super) receiver_identity: usize,
  pub(super) receiver_generation: u64,
  pub(super) key: crate::value::PropertyKey,
  pub(super) value: Value,
}

// Factor complex hook type into aliases to satisfy clippy's `type_complexity` suggestions.
//
// - `UnhandledRejectionHookInner` captures the underlying FnMut signature.
// - `UnhandledRejectionHook` wraps the inner type in the `Rc<RefCell<...>>` used throughout the VM.
type UnhandledRejectionHookInner = dyn FnMut(&mut Vm, Value, Value);
type UnhandledRejectionHook = Rc<RefCell<UnhandledRejectionHookInner>>;

#[derive(Default)]
/// A stack of values used for operand storage in the VM.
pub(crate) struct OperandStack {
  /// The main stack of values.
  inner: Vec<Value>,
  /// A register holding the top-of-stack value for performance.
  register: Option<Value>,
}

impl OperandStack {
  /// Creates a new operand stack with the specified capacity.
  pub(crate) fn with_capacity(cap: usize) -> Self {
    Self {
      inner: Vec::with_capacity(cap),
      register: None,
    }
  }

  /// Clears all values from the stack.
  pub(crate) fn clear(&mut self) {
    self.inner.clear();
    self.register = None;
  }

  /// Returns the total number of values on the stack.
  pub(crate) fn len(&self) -> usize {
    self.inner.len() + self.register.is_some() as usize
  }

  /// Pushes a value onto the stack.
  pub(crate) fn push(
    &mut self,
    value: Value,
  ) {
    if let Some(top) = self.register.replace(value) {
      self.inner.push(top);
    }
  }

  /// Pops and returns the top value from the stack.
  pub(crate) fn pop(&mut self) -> Option<Value> {
    if let Some(top) = self.register.take() {
      Some(top)
    } else {
      self.inner.pop()
    }
  }

  /// Returns a reference to the top value without removing it.
  pub(crate) fn last(&self) -> Option<&Value> {
    self
      .register
      .as_ref()
      .or_else(|| self.inner.last())
  }

  /// Returns an iterator over all values in the stack.
  pub(crate) fn iter(&self) -> OperandStackIter<'_> {
    OperandStackIter {
      inner: self.inner.iter(),
      register: self.register.as_ref(),
      register_emitted: false,
    }
  }

  /// Returns a reverse iterator over all values in the stack.
  pub(crate) fn iter_rev(&self) -> OperandStackRevIter<'_> {
    OperandStackRevIter {
      inner: self.inner.iter().rev(),
      register: self.register.as_ref(),
      register_emitted: false,
    }
  }

  /// Returns a copy of all values currently on the stack.
  pub(crate) fn snapshot(&self) -> Vec<Value> {
    let mut values = self.inner.clone();
    if let Some(top) = self.register.as_ref() {
      values.push(top.clone());
    }
    values
  }

  /// Takes all values from the stack, leaving it empty.
  pub(crate) fn take_all(&mut self) -> Vec<Value> {
    let mut values = std::mem::take(&mut self.inner);
    if let Some(top) = self.register.take() {
      values.push(top);
    }
    values
  }

  /// Restores the stack to a previously saved state.
  pub(crate) fn restore_from(
    &mut self,
    mut values: Vec<Value>,
  ) {
    if let Some(last) = values.pop() {
      self.inner = values;
      self.register = Some(last);
    } else {
      self.inner = Vec::new();
      self.register = None;
    }
  }

  /// Truncates the stack to the specified length.
  pub(crate) fn truncate(
    &mut self,
    new_len: usize,
  ) {
    let total = self.len();
    if new_len >= total {
      return;
    }
    if new_len == 0 {
      self.clear();
      return;
    }
    if new_len <= self.inner.len() {
      self.inner.truncate(new_len);
      self.register = None;
    } else if new_len == self.inner.len() {
      self.register = None;
    }
  }
}

/// Forward iterator over operand stack values.
pub(crate) struct OperandStackIter<'a> {
  /// Inner iterator over the stack vector.
  inner: std::slice::Iter<'a, Value>,
  /// Reference to the register (top-of-stack).
  register: Option<&'a Value>,
  /// Whether the register has been emitted by the iterator.
  register_emitted: bool,
}

impl<'a> Iterator for OperandStackIter<'a> {
  type Item = &'a Value;

  fn next(&mut self) -> Option<Self::Item> {
    if let Some(value) = self.inner.next() {
      Some(value)
    } else if !self.register_emitted {
      self.register_emitted = true;
      self.register
    } else {
      None
    }
  }
}

/// Reverse iterator over operand stack values.
pub(crate) struct OperandStackRevIter<'a> {
  /// Inner reverse iterator over the stack vector.
  inner: std::iter::Rev<std::slice::Iter<'a, Value>>,
  /// Reference to the register (top-of-stack).
  register: Option<&'a Value>,
  /// Whether the register has been emitted by the iterator.
  register_emitted: bool,
}

impl<'a> Iterator for OperandStackRevIter<'a> {
  type Item = &'a Value;

  fn next(&mut self) -> Option<Self::Item> {
    if let Some(value) = self.inner.next() {
      Some(value)
    } else if !self.register_emitted {
      self.register_emitted = true;
      self.register
    } else {
      None
    }
  }
}

// ---------------------------------------------------------------------------
// Global background Tokio runtime (lazy, shared across all Vm instances)
// ---------------------------------------------------------------------------

static GLOBAL_BG_RUNTIME: OnceLock<Arc<Runtime>> = OnceLock::new();
pub(super) fn get_global_bg_runtime() -> Arc<Runtime> {
  Arc::<Runtime>::clone(GLOBAL_BG_RUNTIME.get_or_init(|| {
    let threads_from_env = env::var("TSBLITZ_BG_THREADS")
      .ok()
      .and_then(|s| s.parse::<usize>().ok())
      .filter(|&n| n > 0 && n <= 1024); // upper bound for sanity

    let cpus = std::thread::available_parallelism().map_or(4, std::num::NonZero::get);

    // Reserve 1 core for main thread, but at least 1 worker
    let threads = threads_from_env.unwrap_or_else(|| cpus.saturating_sub(1).max(1));

    let rt = tokio::runtime::Builder::new_multi_thread()
      .worker_threads(threads)
      .thread_name("blitz-bg")
      .build()
      .expect("Failed to build Tokio runtime for VM background tasks");

    Arc::new(rt)
  }))
}

// ---------------------------------------------------------------------------
// VM
// ---------------------------------------------------------------------------

/// Virtual machine for executing RISC bytecode.
pub struct Vm {
  /// The ECMAScript Realm this VM operates in.
  pub(super) realm: Realm,
  /// Chunk being executed.
  pub(super) chunk: Rc<Chunk>,
  /// Instruction pointer within the chunk.
  pub(super) ip: usize,
  /// Operand stack. Shared across all frames; each frame records its base
  /// offset so the stack is truncated correctly on return.
  pub(super) stack: OperandStack,
  /// Local variables for the current frame.
  pub(super) locals: Vec<SharedValue>,
  /// Current `this` binding.
  pub(super) this_value: Value,
  /// Whether the current builtin call received an explicit receiver in args[0].
  pub(super) builtin_this_arg_prepended: bool,
  /// Upvalue environment for the currently executing closure, if any.
  pub(super) current_closure_env: Option<Rc<Mutex<Vec<SharedValue>>>>,
  /// Temporary root for a callee closure environment while a call frame is
  /// being prepared.
  pub(super) pending_closure_env: Option<Rc<Mutex<Vec<SharedValue>>>>,
  /// Temporary roots for values that are in flight through host helpers or
  /// call setup and are not yet anchored in the VM state.
  pub(super) gc_temp_roots: Vec<Value>,
  /// Global variables (derived from realm.global_object for convenience).
  pub(super) globals: Rc<RefCell<ObjectData>>,
  /// Call-frame stack component.
  pub(super) call_stack: CallStack,
  /// Root chunk (kept alive for global type metadata).
  pub(super) root_chunk: Option<Rc<Chunk>>,
  /// Slot-type annotations for the current function (formerly used by TS checks).
  pub(super) current_slot_types: Option<Vec<Option<Type>>>,
  /// Slot names for the current function.
  pub(super) current_slot_names: Option<Vec<Option<String>>>,
  /// Per-slot immutability flags for the current function.
  pub(super) current_slot_immutable: Option<Vec<bool>>,
  /// Per-slot flag for immutable bindings that ignore sloppy writes.
  pub(super) current_slot_sloppy_immutable_silent: Option<Vec<bool>>,
  /// Caller slot names visible while executing direct eval.
  pub(super) direct_eval_outer_slot_names: Option<Vec<Option<String>>>,
  /// Caller slot immutability visible while executing direct eval.
  pub(super) direct_eval_outer_slot_immutable: Option<Vec<bool>>,
  /// Caller sloppy-write-ignore immutability flags for direct eval.
  pub(super) direct_eval_outer_slot_sloppy_immutable_silent: Option<Vec<bool>>,
  /// Per-object mutation generation counter, keyed by `Rc` pointer address.
  /// Incremented on every `SetProperty` so that stale IC entries are
  /// detected and evicted on the next lookup.
  pub(super) obj_generations: HashMap<usize, u64>,
  /// Monomorphic per-instruction cache for own data-property reads.
  pub(super) property_get_ic: HashMap<(usize, usize), PropertyGetIcEntry>,
  /// Background Tokio runtime handle.
  pub(super) bg_runtime: Arc<Runtime>,
  /// Exception-handler stack.
  pub(super) handlers: HandlerStack,
  /// Flag set when a JS exception was unwound to a handler.
  pub(super) pending_exception: bool,
  /// Shared module loader for this realm, reused across VMs with the same globals.
  pub(super) module_loader: Rc<RefCell<ModuleLoader>>,
  /// Per-VM module table: vector indexed by module index, each mapping export name -> shared slot
  pub(super) module_table: Vec<HashMap<String, SharedValue>>,
  /// Current [[NewTarget]] while evaluating a constructor call.
  pub(super) construct_new_target: Option<Value>,
  /// Recently returned heap value, kept alive across the return boundary
  /// until the caller has a chance to store it.
  pub(super) recent_return_value: Option<Value>,
  /// Last uncaught thrown JS value observed during unwinding.
  pub(crate) last_uncaught_exception: Option<Value>,
  /// ECMAScript microtask queue.
  pub(super) microtasks: VecDeque<Microtask>,
  /// Queue for pending dynamic import jobs.
  pub(super) dynamic_import_jobs: VecDeque<DynamicImportJob>,
  /// Optional hook invoked when an unhandled rejection or microtask exception
  /// is observed. The hook receives two Values: the first is the promise
  /// object (or Undefined for non-promise microtask exceptions) and the
  /// second is the rejection reason / exception value.
  pub(super) unhandled_rejection_hook: Option<UnhandledRejectionHook>,
  /// Store of lexical environment records for the current execution context.
  pub(super) lexical_env: Rc<RefCell<EnvironmentRecord>>,
  /// Store of global environment records for the current execution context.
  pub(super) global_env: Rc<RefCell<EnvironmentRecord>>,
  /// Iterator delegation state for the current `yield*` expression, if any.
  pub(super) yield_star_state: Option<YieldStarState>,
  /// Frame depth of a generator return completion currently crossing finalizers.
  pub(super) generator_return_frame_count: Option<usize>,
  /// Live arguments object for the currently executing frame, if any.
  pub(super) current_arguments_object: Option<Value>,
  /// Number of leading arguments currently mapped to local slots.
  pub(super) current_arguments_mapped_count: usize,
  /// Current strict-mode state for the active call context.
  pub(super) current_strict_mode: bool,
  /// Whether runtime opcode profiling is enabled for this VM instance.
  pub(super) opcode_profiling_enabled: bool,
  /// Per-opcode execution counters for the current run.
  pub(super) opcode_counts: [u64; OP_COUNT],
  /// Run start timestamp for the current profiling window.
  pub(super) run_started_at: Option<Instant>,
}

thread_local! {
    static SHARED_MODULE_LOADERS: RefCell<HashMap<usize, Weak<RefCell<ModuleLoader>>>> =
        RefCell::new(HashMap::new());
}

pub(super) fn shared_module_loader_for(
  globals: &Rc<RefCell<ObjectData>>
) -> Rc<RefCell<ModuleLoader>> {
  let key = Rc::as_ptr(globals) as usize;
  SHARED_MODULE_LOADERS.with(|cache| {
    let mut cache = cache.borrow_mut();
    if let Some(loader) = cache.get(&key).and_then(Weak::upgrade) {
      return loader;
    }

    let loader = Rc::new(RefCell::new(ModuleLoader::new()));
    cache.insert(key, Rc::downgrade(&loader));
    loader
  })
}
