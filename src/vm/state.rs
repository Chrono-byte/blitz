//! Stack-based bytecode interpreter with runtime type enforcement.
//!
//! The VM executes compiled bytecode in a stack-based environment. It provides:
//! - Stack and local variable management
//! - Function call frames and closures
//! - Property access with generation-guarded inline caching
//! - Exception handling (try/catch)
//! - Runtime type checking
//!
//! # Architecture
//!
//! The VM maintains:
//! - A value stack for operands and intermediate results
//! - A locals array for function-local variables
//! - A globals map for global variables
//! - A call stack of frames for function calls
//! - Exception handler records for try/catch
//! - A per-object generation table for IC invalidation
//!
//! # Execution
//!
//! The VM uses a fetch-decode-execute loop over bytecode instructions.
//! Each instruction manipulates the stack or control flow.
//!
//! # Type Safety
//!
//! When type annotations are available, the VM enforces them at runtime:
//! - Local variable assignments check slot types
//! - Global variable assignments check global types
//! - Function arguments are validated on call
//!
//! # Example
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
