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
  /// Finalizer control data when this protected region has a `finally` clause.
  pub(crate) finally: Option<FinallyHandlerRecord>,
}

pub(crate) struct FinallyHandlerRecord {
  pub(crate) finalizer_ip: usize,
  pub(crate) return_slot: usize,
  pub(crate) did_return_slot: usize,
}

/// Register and spill state that survives a suspended RISC execution.
pub(crate) struct RiscExecutionState {
  pub(crate) registers: [Value; 16],
  pub(crate) spills: Vec<Value>,
  pub(crate) resume_register: Option<usize>,
}

impl RiscExecutionState {
  pub(crate) fn new(
    this_value: Value,
    spill_count: usize,
  ) -> Self {
    let mut registers = std::array::from_fn(|_| Value::Undefined);
    registers[1] = this_value;
    Self {
      registers,
      spills: vec![Value::Undefined; spill_count],
      resume_register: None,
    }
  }

  pub(crate) fn resume_with(
    &mut self,
    value: Value,
  ) {
    if let Some(register) = self.resume_register.take() {
      self.registers[register] = value;
    }
  }
}

/// Boundary reached while executing a persistent RISC state.
pub(crate) enum RiscRunOutcome {
  Initialized,
  Return(Value),
  Yield(Value),
  YieldStar(Value),
}
