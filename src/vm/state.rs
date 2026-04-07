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
use crate::value::{ObjectData, Value, ValueSlot};
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

pub(crate) use frame::Frame;
pub(crate) use generator_runner::GeneratorRunner;
pub(crate) use handler::{HandlerRecord, StepOutcome};
pub(crate) use partial_descriptor::PartialDescriptor;
#[allow(unused_imports)]
pub(crate) use stack::{CallStack, HandlerStack, ReferenceRecord, ReferenceStack};

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

/// Number of interpreter steps between opportunistic GC attempts.
pub(super) const GC_STEP_INTERVAL: usize = 16;

type Microtask = Box<dyn FnMut(&mut Vm) -> Result<(), crate::errors::RuntimeError>>;
type DynamicImportJob = Box<dyn FnMut(&mut Vm) -> Result<(), crate::errors::RuntimeError>>;

#[derive(Default)]
pub(crate) struct OperandStack {
    inner: Vec<Value>,
    register: Option<Value>,
}

impl OperandStack {
    pub(crate) fn with_capacity(cap: usize) -> Self {
        Self {
            inner: Vec::with_capacity(cap),
            register: None,
        }
    }

    pub(crate) fn clear(&mut self) {
        self.inner.clear();
        self.register = None;
    }

    pub(crate) fn len(&self) -> usize {
        self.inner.len() + self.register.is_some() as usize
    }

    pub(crate) fn push(&mut self, value: Value) {
        if let Some(top) = self.register.replace(value) {
            self.inner.push(top);
        }
    }

    pub(crate) fn pop(&mut self) -> Option<Value> {
        if let Some(top) = self.register.take() {
            Some(top)
        } else {
            self.inner.pop()
        }
    }

    pub(crate) fn last(&self) -> Option<&Value> {
        self.register.as_ref().or_else(|| self.inner.last())
    }

    pub(crate) fn iter(&self) -> OperandStackIter<'_> {
        OperandStackIter {
            inner: self.inner.iter(),
            register: self.register.as_ref(),
            register_emitted: false,
        }
    }

    pub(crate) fn iter_rev(&self) -> OperandStackRevIter<'_> {
        OperandStackRevIter {
            inner: self.inner.iter().rev(),
            register: self.register.as_ref(),
            register_emitted: false,
        }
    }

    pub(crate) fn snapshot(&self) -> Vec<Value> {
        let mut values = self.inner.clone();
        if let Some(top) = self.register.as_ref() {
            values.push(top.clone());
        }
        values
    }

    pub(crate) fn take_all(&mut self) -> Vec<Value> {
        let mut values = std::mem::take(&mut self.inner);
        if let Some(top) = self.register.take() {
            values.push(top);
        }
        values
    }

    pub(crate) fn restore_from(&mut self, mut values: Vec<Value>) {
        if let Some(last) = values.pop() {
            self.inner = values;
            self.register = Some(last);
        } else {
            self.inner = Vec::new();
            self.register = None;
        }
    }

    pub(crate) fn truncate(&mut self, new_len: usize) {
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

    pub(crate) fn swap(&mut self, a: usize, b: usize) {
        let total = self.len();
        if a >= total || b >= total {
            panic!("swap index out of bounds");
        }
        let mut values = self.take_all();
        values.swap(a, b);
        self.restore_from(values);
    }
}

pub(crate) struct OperandStackIter<'a> {
    inner: std::slice::Iter<'a, Value>,
    register: Option<&'a Value>,
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

pub(crate) struct OperandStackRevIter<'a> {
    inner: std::iter::Rev<std::slice::Iter<'a, Value>>,
    register: Option<&'a Value>,
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
    pub(super) stack: OperandStack,
    /// Local variables for the current frame.
    pub(super) locals: Vec<ValueSlot>,
    /// Current `this` binding.
    pub(super) this_value: Value,
    /// Upvalue environment for the currently executing closure, if any.
    pub(super) current_closure_env: Option<Rc<Mutex<Vec<ValueSlot>>>>,
    /// Temporary root for a callee closure environment while a call frame is
    /// being prepared.
    pub(super) pending_closure_env: Option<Rc<Mutex<Vec<ValueSlot>>>>,
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
    /// Per-object mutation generation counter, keyed by `Rc` pointer address.
    /// Incremented on every `SetProperty` so that stale IC entries are
    /// detected and evicted on the next lookup.
    pub(super) obj_generations: HashMap<usize, u64>,
    /// Background Tokio runtime handle.
    pub(super) bg_runtime: Arc<Runtime>,
    /// Exception-handler stack.
    pub(super) handlers: HandlerStack,
    /// Flag set when a JS exception was unwound to a handler.
    pub(super) pending_exception: bool,
    /// Shared module loader for this realm, reused across VMs with the same globals.
    pub(super) module_loader: Rc<RefCell<ModuleLoader>>,
    /// Per-VM module table: vector indexed by module index, each mapping export name -> shared slot
    pub(super) module_table: Vec<HashMap<String, ValueSlot>>,
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
    /// Optional reference stack for future reference record handling.
    #[allow(dead_code)]
    pub(super) reference_stack: ReferenceStack,
    /// Optional hook invoked when an unhandled rejection or microtask exception
    /// is observed. The hook receives two Values: the first is the promise
    /// object (or Undefined for non-promise microtask exceptions) and the
    /// second is the rejection reason / exception value.
    pub(super) unhandled_rejection_hook: Option<Rc<RefCell<dyn FnMut(&mut Vm, Value, Value)>>>,
    /// Store of lexical environment records for the current execution context.
    pub(super) lexical_env: Rc<RefCell<EnvironmentRecord>>,
    /// Store of global environment records for the current execution context.
    pub(super) global_env: Rc<RefCell<EnvironmentRecord>>,
    /// Queue of values for the current `yield*` expression, if any.
    pub(super) yield_star_queue: Option<VecDeque<Value>>,
    /// Counter used to trigger opportunistic GC during interpreter progress.
    pub(super) gc_step_counter: usize,
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
    globals: &Rc<RefCell<ObjectData>>,
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
