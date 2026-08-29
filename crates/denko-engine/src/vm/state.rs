// SPDX-License-Identifier: MPL-2.0
//! bytecode interpreter.
//!
//! This module implements the VM core: a fetch-decode-execute loop that
//! evaluates compiled bytecode using registers and spill storage. It
//! contains the Vm struct, frame/handler implementations, and tuning constants
//! used by the runtime.
//!
//! Key responsibilities:
//! - Manage registers, spill storage, and locals
//! - Maintain call frames, exception handlers, and generator state
//! - Provide property access support with generation-guarded inline
//!   caching to accelerate repeated lookups
//! - Integrate with a background runtime for blocking/async work
//!
//! Example
//!
//! ```
//! use denko::vm::Vm;
//! use denko::bytecode::Chunk;
//! use std::cell::RefCell;
//! use std::rc::Rc;
//! use denko::value::ObjectData;
//!
//! let globals = Rc::new(RefCell::new(ObjectData::new()));
//! let mut vm = Vm::new(globals);
//! // vm.run(code)?;
//! ```

use crate::binding::BindingCell;
use crate::bytecode::{Chunk, Function, InstructionIndex, OP_COUNT};
use crate::environment::EnvironmentRecord;
use crate::gc;
use crate::module::ModuleLoader;
use crate::realm::Realm;
use crate::value::{ObjectData, Value};
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::env;
use std::rc::{Rc, Weak};
use std::sync::{Arc, OnceLock};
use std::time::Instant;
use tokio::runtime::Runtime;

pub(crate) mod arena;
mod frame;
mod generator_runner;
mod handler;
mod memory;
mod partial_descriptor;
mod stack;
mod yield_star;

pub(crate) use frame::CallFrame;
pub(crate) use generator_runner::{GeneratorRunner, GeneratorStatus};
pub(crate) use handler::{
  AwaitSettlement, FinallyHandlerRecord, HandlerRecord, PendingOperation, PendingOperationStep,
  RegisterState, RunOutcome,
};
pub(crate) use memory::VmMemory;
pub use memory::{HostRoot, VmMemorySnapshot, WeakHostRoot};
pub(crate) use partial_descriptor::PartialDescriptor;
pub(crate) use stack::{CallStack, HandlerStack};
pub(crate) use yield_star::{YieldStarResume, YieldStarState};

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

pub(super) type Microtask = Box<dyn FnMut(&mut Vm) -> Result<(), crate::errors::RuntimeError>>;
pub(super) type DynamicImportJob =
  Box<dyn FnMut(&mut Vm) -> Result<(), crate::errors::RuntimeError>>;

#[derive(Clone)]
pub(super) struct PropertyGetIcEntry {
  pub(super) epoch: u64,
  pub(super) receiver_identity: usize,
  pub(super) receiver_generation: u64,
  pub(super) key: crate::value::PropertyKey,
  pub(super) value: Value,
}

pub(super) type UnhandledRejectionHookInner = dyn FnMut(&mut Vm, Value, Value);
pub(super) type UnhandledRejectionHook = Rc<RefCell<UnhandledRejectionHookInner>>;

static GLOBAL_BG_RUNTIME: OnceLock<Arc<Runtime>> = OnceLock::new();
pub(super) fn get_global_bg_runtime() -> Arc<Runtime> {
  Arc::<Runtime>::clone(GLOBAL_BG_RUNTIME.get_or_init(|| {
    let threads_from_env = env::var("TSBLITZ_BG_THREADS")
      .ok()
      .and_then(|s| s.parse::<usize>().ok())
      .filter(|&n| n > 0 && n <= 1024);

    let cpus = std::thread::available_parallelism().map_or(4, std::num::NonZero::get);

    let threads = threads_from_env.unwrap_or_else(|| cpus.saturating_sub(1).max(1));

    let rt = tokio::runtime::Builder::new_multi_thread()
      .worker_threads(threads)
      .thread_name("blitz-bg")
      .enable_all()
      .build()
      .expect("Failed to build Tokio runtime for VM background tasks");

    Arc::new(rt)
  }))
}

/// Virtual machine for executing bytecode.
pub struct Vm {
  pub(super) realm_state: RealmState,
  pub(super) execution: ExecutionState,
  pub(crate) exceptions: ExceptionState,
  pub(super) construction: ConstructionState,
  pub(super) modules: ModuleState,
  pub(super) jobs: JobState,
  pub(super) caches: CacheState,
  pub(super) profiling: ProfilingState,
  pub(super) memory: VmMemory,
  pub(super) interrupts: crate::vm::interrupt::VmInterruptState,
  /// Keeps this VM represented in the thread-local prototype heap lifetime.
  ///
  /// This is deliberately last so it is dropped after every Value-holding
  /// field above it.
  pub(super) _gc_registration: gc::VmGcRegistration,
}

pub(crate) struct RealmState {
  pub(super) realm: Realm,
  pub(super) globals: Rc<RefCell<ObjectData>>,
  pub(super) global_env: Rc<RefCell<EnvironmentRecord>>,
}

#[derive(Clone)]
pub(crate) struct SlotMetadata {
  pub(crate) names: Vec<Option<String>>,
  pub(crate) immutable: Vec<bool>,
  pub(crate) sloppy_immutable_silent: Vec<bool>,
}

#[derive(Clone)]
pub(crate) struct ArgumentsState {
  pub(crate) object: Option<Value>,
  pub(crate) mapped_count: usize,
}

#[derive(Clone)]
pub(crate) struct ExecutionContext {
  pub(crate) chunk: Arc<Chunk>,
  pub(crate) pc: InstructionIndex,
  pub(crate) active_function: Option<Value>,
  pub(crate) locals: Vec<BindingCell>,
  pub(crate) this_value: Value,
  pub(crate) closure_env: Option<crate::closure_environment::ClosureEnvironment>,
  pub(crate) slots: Option<SlotMetadata>,
  pub(crate) lexical_env: Rc<RefCell<EnvironmentRecord>>,
  pub(crate) yield_star: Option<YieldStarState>,
  pub(crate) arguments: ArgumentsState,
  pub(crate) strict: bool,
}

#[derive(Clone)]
pub(super) struct DirectEvalState {
  pub(super) names: Vec<Option<String>>,
  pub(super) immutable: Vec<bool>,
  pub(super) sloppy_immutable_silent: Vec<bool>,
}

pub(super) struct ExecutionState {
  pub(super) active: ExecutionContext,
  /// Number of active interpreter loops. Native calls only drain jobs when
  /// this is zero, preserving JavaScript job run-to-completion.
  pub(super) interpreter_depth: usize,
  /// Changes whenever exception handling transfers execution to a handler.
  pub(super) control_flow_epoch: u64,
  pub(super) builtin_this_arg_prepended: bool,
  pub(super) pending_closure_env: Option<crate::closure_environment::ClosureEnvironment>,
  pub(super) call_stack: CallStack,
  pub(super) root_chunk: Option<Arc<Chunk>>,
  pub(super) direct_eval_outer: Option<DirectEvalState>,
  pub(super) generator_return_frame_count: Option<usize>,
}

pub(crate) struct ExceptionState {
  pub(super) handlers: HandlerStack,
  pub(super) pending: Option<Value>,
  pub(super) pending_frame_count: Option<usize>,
  pub(super) unwound_register_state: Option<RegisterState>,
  pub(crate) last_uncaught: Option<Value>,
}

pub(super) struct ConstructionState {
  pub(super) new_target: Option<Value>,
  pub(super) recent_return_value: Option<Value>,
  pub(super) derived_this: Vec<Option<Value>>,
}

pub(super) struct ModuleState {
  pub(super) loader: Rc<RefCell<ModuleLoader>>,
  pub(super) table: Vec<HashMap<String, BindingCell>>,
  pub(super) current_specifier: Option<String>,
}

pub(super) struct JobState {
  pub(super) microtasks: VecDeque<Microtask>,
  pub(super) dynamic_imports: VecDeque<DynamicImportJob>,
  pub(super) unhandled_rejection_hook: Option<UnhandledRejectionHook>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum QuickenedKind {
  #[default]
  Ordinary,
  NumericCompareBranch,
  SingleArgumentMethodCall,
  NumericPropertyAdd,
  NumericIncrement,
  NumericAddLocal,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum SiteAction {
  #[default]
  Interpret,
  Legacy(QuickenedKind),
  Region(crate::vm::region::RegionId),
}

pub(super) struct QuickenedPlan {
  pub(super) _chunk: Arc<Chunk>,
  pub(super) sites: Arc<[SiteAction]>,
  pub(super) regions: Arc<[crate::vm::region::NativeRegion]>,
  pub(super) direct_calls: Rc<RefCell<Vec<Option<DirectCallSite>>>>,
}

#[derive(Clone)]
pub(super) struct DirectCallSite {
  pub(super) expected_proto: Rc<Function>,
  pub(super) target: crate::vm::jit::DirectCallTarget,
}

pub(super) struct CacheState {
  pub(super) property_get: HashMap<(usize, usize), PropertyGetIcEntry>,
  /// O(1) invalidation boundary for mutation-opaque native callbacks.
  pub(super) property_get_epoch: u64,
  /// VM-local quickened plans keyed by portable code identity.
  pub(super) quickened: HashMap<usize, QuickenedPlan>,
  pub(super) active_quickened_chunk: usize,
  pub(super) active_quickened_sites: Arc<[SiteAction]>,
  pub(super) active_regions: Arc<[crate::vm::region::NativeRegion]>,
  pub(super) active_direct_calls: Rc<RefCell<Vec<Option<DirectCallSite>>>>,
  /// Test and conformance switch for authoritative portable execution.
  pub(super) optimized_execution_enabled: bool,
  /// Diagnostic switch that leaves legacy quickening active.
  pub(super) native_regions_enabled: bool,
}

pub(super) struct ProfilingState {
  pub(super) enabled: bool,
  pub(super) opcode_counts: [u64; OP_COUNT],
  pub(super) run_started_at: Option<Instant>,
  /// Feedback keyed by stable portable code identity, never native code.
  pub(super) functions: HashMap<crate::vm::jit::FunctionId, crate::vm::jit::FunctionProfile>,
  /// Temporary identity bridge until executable code owns a stable ID. The
  /// retained Arc intentionally prevents pointer reuse during this phase.
  pub(super) code_ids: HashMap<usize, crate::vm::jit::FunctionId>,
  pub(super) chunk_refs: HashMap<usize, Arc<Chunk>>,
  pub(super) next_function_id: u64,
  pub(super) active_function_identity: usize,
  pub(super) active_function_id: Option<crate::vm::jit::FunctionId>,
  /// Avoid feedback-map traffic after a code has requested compilation.
  pub(super) last_hot_function: usize,
  pub(super) hot_functions: std::collections::HashSet<usize>,
  pub(super) jit: crate::vm::jit::JitRuntime,
  pub(super) jit_stats: crate::vm::jit::JitStats,
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
