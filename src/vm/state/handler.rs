use crate::bytecode::Chunk;
use crate::environment::EnvironmentRecord;
use crate::value::Value;
use std::cell::RefCell;
use std::rc::Rc;

/// Exception handler record.
pub(crate) struct HandlerRecord {
  /// The chunk containing the handler code.
  pub(crate) chunk: Rc<Chunk>,
  /// The instruction pointer where the handler jumps to.
  pub(crate) handler_ip: usize,
  /// The saved operand stack length for unwinding.
  pub(crate) saved_stack_len: usize,
  /// The saved locals length for unwinding.
  pub(crate) saved_locals_len: usize,
  /// The saved frame count for unwinding.
  pub(crate) frame_count: usize,
  /// The saved lexical environment for unwinding.
  pub(crate) saved_lexical_env: Rc<RefCell<EnvironmentRecord>>,
}

/// Outcome of a single VM execution step.
pub(crate) enum StepOutcome {
  /// Continue execution with the next instruction.
  Continue,
  /// Return from the current function with a value.
  Return(Value),
  /// Yield a value from a generator function.
  Yield(Value),
}
