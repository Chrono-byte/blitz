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

use crate::bytecode::Chunk;
use crate::environment::EnvironmentRecord;
use crate::module::ModuleLoader;
use crate::realm::Realm;
use crate::typecheck::Type;
use crate::value::{ObjectData, Value};
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::env;
use std::rc::Rc;
use std::sync::{Arc, OnceLock};
use tokio::runtime::Runtime;

mod frame;
mod generator_runner;
mod handler;
mod partial_descriptor;

pub(crate) use frame::Frame;
pub(crate) use generator_runner::GeneratorRunner;
pub(crate) use handler::{HandlerRecord, StepOutcome};
pub(crate) use partial_descriptor::PartialDescriptor;

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

type Microtask = Box<dyn FnMut(&mut Vm) -> Result<(), crate::errors::RuntimeError>>;

// ---------------------------------------------------------------------------
// Global background Tokio runtime (lazy, shared across all Vm instances)
// ---------------------------------------------------------------------------

static GLOBAL_BG_RUNTIME: OnceLock<Arc<Runtime>> = OnceLock::new();

pub(super) fn get_global_bg_runtime() -> Arc<Runtime> {
    GLOBAL_BG_RUNTIME
        .get_or_init(|| {
            let threads_from_env = env::var("TSBLITZ_BG_THREADS")
                .ok()
                .and_then(|s| s.parse::<usize>().ok())
                .filter(|&n| n > 0 && n <= 1024); // upper bound for sanity

            let cpus = std::thread::available_parallelism()
                .map(std::num::NonZero::get)
                .unwrap_or(4);

            // Reserve 1 core for main thread, but at least 1 worker
            let threads = threads_from_env.unwrap_or_else(|| cpus.saturating_sub(1).max(1));

            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(threads)
                .thread_name("blitz-bg")
                .build()
                .expect("Failed to build Tokio runtime for VM background tasks");

            Arc::new(rt)
        })
        .clone()
}

// ---------------------------------------------------------------------------
// VM
// ---------------------------------------------------------------------------

/// Virtual machine for executing bytecode.
//
// Conformance notes
//
// The VM implements a stack-based bytecode interpreter suitable for
// executing compiled chunks produced by the project's compiler. A few
// runtime behaviors are intentionally non-standard or are pragmatic
// approximations of ECMAScript semantics:
//
// - Runtime type checking: the VM can enforce TypeScript-like slot type
//   annotations at runtime. This is a non-spec extension and should be
//   disabled for strict ECMA-262 conformance or made opt-in.
// - Global background runtime: a shared Tokio runtime backs blocking and
//   background operations; this is a host implementation detail and not
//   mandated by ECMA-262.
// - Inline caching and generation counters: the VM implements a simple
//   generation counter approach for invalidating property caches. The
//   mechanism is an implementation detail and should be validated against
//   correctness tests rather than spec text.
//
// See `src/runtime_utils.rs`, `src/module.rs`, and the `runtime` submodules
// for additional documentation about known deviations and recommended fixes
// when stricter conformance is required.
pub struct Vm {
    /// The ECMAScript Realm this VM operates in.
    pub(super) realm: Realm,
    /// Chunk being executed.
    pub(super) chunk: Rc<Chunk>,
    /// Instruction pointer within the chunk.
    pub(super) ip: usize,
    /// Operand stack. Shared across all frames; each frame records its base
    /// offset so the stack is truncated correctly on return.
    pub(super) stack: Vec<Value>,
    /// Local variables for the current frame.
    pub(super) locals: Vec<Rc<RefCell<Value>>>,
    /// Current `this` binding.
    pub(super) this_value: Value,
    /// Upvalue environment for the currently executing closure, if any.
    pub(super) current_closure_env: Option<Rc<RefCell<Vec<Rc<RefCell<Value>>>>>>,
    /// Global variables (derived from realm.global_object for convenience).
    pub(super) globals: Rc<RefCell<ObjectData>>,
    /// Call-frame stack.
    pub(super) frames: Vec<Frame>,
    /// Root chunk (kept alive for global type metadata).
    pub(super) root_chunk: Option<Rc<Chunk>>,
    /// Slot-type annotations for the current function (formerly used by TS checks).
    pub(super) current_slot_types: Option<Vec<Option<Type>>>,
    /// Per-object mutation generation counter, keyed by `Rc` pointer address.
    /// Incremented on every `SetProperty` so that stale IC entries are
    /// detected and evicted on the next lookup.
    pub(super) obj_generations: HashMap<usize, u64>,
    /// Background Tokio runtime handle.
    pub(super) bg_runtime: Arc<Runtime>,
    /// Exception-handler stack.
    pub(super) handlers: Vec<HandlerRecord>,
    /// Flag set when a JS exception was unwound to a handler.
    pub(super) pending_exception: bool,
    /// Per-VM module loader responsible for module construction/instantiation/eval
    pub(super) module_loader: Option<ModuleLoader>,
    /// Per-VM module table: vector indexed by module index, each mapping export name -> shared slot
    pub(super) module_table: Vec<HashMap<String, Rc<RefCell<Value>>>>,
    /// Current [[NewTarget]] while evaluating a constructor call.
    pub(super) construct_new_target: Option<Value>,
    /// Last uncaught thrown JS value observed during unwinding.
    pub(crate) last_uncaught_exception: Option<Value>,
    pub(super) microtasks: VecDeque<Microtask>,
    pub(super) lexical_env: Rc<RefCell<EnvironmentRecord>>,
    pub(super) global_env: Rc<RefCell<EnvironmentRecord>>,
    pub(super) yield_star_queue: Option<VecDeque<Value>>,
}
