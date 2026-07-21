use crate::bytecode::{Chunk, UpvalueSpec};
use crate::environment::EnvironmentRecord;
use crate::typecheck::Type;
use crate::value::{SharedValue, Value};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Mutex;

/// Call frame.
pub(crate) struct Frame {
  /// The chunk containing the bytecode for this frame.
  pub(crate) chunk: Rc<Chunk>,
  /// The current instruction pointer within the chunk.
  pub(crate) ip: usize,
  /// The stack of values for this frame.
  pub(crate) stack: Vec<Value>,
  /// The local variable slots for this frame.
  pub(crate) locals: Vec<SharedValue>,
  /// The `this` value for this frame.
  pub(crate) this_value: Value,
  /// The callee value used to enter this frame.
  pub(crate) callee: Value,
  /// The `[[NewTarget]]` value in effect for this frame, if any.
  pub(crate) new_target: Option<Value>,
  /// The closure environment for this frame.
  pub(crate) closure_env: Option<Rc<Mutex<Vec<SharedValue>>>>,
  /// Upvalue metadata for the current function.
  pub(crate) upvalues: Vec<UpvalueSpec>,
  /// The types of local variables for this frame.
  pub(crate) slot_types: Option<Vec<Option<Type>>>,
  /// The names of local variables for this frame.
  pub(crate) slot_names: Option<Vec<Option<String>>>,
  /// Per-slot immutability flags for this frame.
  pub(crate) slot_immutable: Option<Vec<bool>>,
  /// Per-slot sloppy-write-ignore flags for immutable bindings.
  pub(crate) slot_sloppy_immutable_silent: Option<Vec<bool>>,
  /// Length of caller stack at call entry (diagnostic only).
  pub(crate) stack_base: usize,
  /// Whether the suspended caller executes in strict mode.
  /// Restored when this frame is popped on return or exception unwind.
  pub(crate) is_strict: bool,
  /// The lexical environment for this frame.
  pub(crate) lexical_env: Rc<RefCell<EnvironmentRecord>>,
  /// The active yield-star delegation for this frame.
  pub(crate) yield_star_state: Option<super::YieldStarState>,
  /// The live arguments object for this frame, if the function uses mapped
  /// arguments semantics.
  pub(crate) arguments_object: Option<Value>,
  /// Number of leading arguments that remain mapped to local slots.
  pub(crate) arguments_mapped_count: usize,
}
