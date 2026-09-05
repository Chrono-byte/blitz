// SPDX-License-Identifier: MPL-2.0
use crate::bytecode::{Chunk, InstructionIndex, Register, abi};
use crate::environment::EnvironmentRecord;
use crate::value::Value;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

/// Exception handler record.
pub(crate) struct HandlerRecord {
  /// The code containing the handler code.
  pub(crate) chunk: Arc<Chunk>,
  /// The instruction pointer where the handler jumps to.
  pub(crate) handler_pc: InstructionIndex,
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
  pub(crate) finalizer_pc: InstructionIndex,
  pub(crate) return_slot: usize,
  pub(crate) did_return_slot: usize,
}

/// Register and spill state that survives a suspended execution.
pub(crate) struct RegisterState {
  pub(crate) registers: [Value; Register::COUNT],
  pub(crate) spill_slots: Vec<Value>,
  pub(crate) resume_destination: Option<usize>,
  pub(crate) pending_operation: Option<PendingOperation>,
  pub(crate) await_settlement: Option<AwaitSettlement>,
}

pub(crate) enum AwaitSettlement {
  Fulfilled(Value),
  Rejected(Value),
}

pub(crate) enum PendingOperation {
  YieldStar { destination: usize },
  NextNative { destination: usize, record: Value },
  CloseNative { destination: usize, record: Value },
}

pub(crate) enum PendingOperationStep {
  Complete {
    destination: usize,
    value: Value,
  },
  Await {
    value: Value,
    operation: PendingOperation,
  },
  Throw(Value),
}

impl RegisterState {
  pub(crate) fn new(
    this_value: Value,
    arguments: &[Value],
    spill_count: usize,
  ) -> Self {
    Self {
      registers: Self::call_registers(this_value, arguments),
      spill_slots: vec![Value::Undefined; spill_count],
      resume_destination: None,
      pending_operation: None,
      await_settlement: None,
    }
  }

  pub(crate) fn call_registers(
    this_value: Value,
    arguments: &[Value],
  ) -> [Value; Register::COUNT] {
    let mut registers = std::array::from_fn(|_| Value::Undefined);
    registers[abi::RECEIVER.index()] = this_value;
    for (index, value) in arguments
      .iter()
      .take(abi::REGISTER_ARGUMENT_COUNT)
      .enumerate()
    {
      let register = abi::argument_register(index)
        .expect("bounded register argument index must have an ABI register");
      registers[register.index()] = value.clone();
    }
    registers
  }

  pub(crate) fn settle_pending_operation(
    &mut self,
    settlement: AwaitSettlement,
  ) {
    self.await_settlement = Some(settlement);
  }

  pub(crate) fn resume_with(
    &mut self,
    value: Value,
  ) {
    if let Some(register) = self.resume_destination.take() {
      self.registers[register] = value;
    }
  }

  pub(crate) fn clear(&mut self) {
    self.registers.fill(Value::Undefined);
    self.spill_slots.clear();
    self.resume_destination = None;
    self.pending_operation = None;
    self.await_settlement = None;
  }
}

/// Boundary reached while executing a persistent state.
pub(crate) enum RunOutcome {
  Initialized,
  Return(Value),
  Yield(Value),
  YieldStar(Value),
  Await(Value),
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::bytecode::{Register, abi};

  #[test]
  fn function_entry_leaves_return_register_dead() {
    let receiver = Value::String("receiver".into());
    let arguments = (0..7)
      .map(|value| Value::Number(value as f64))
      .collect::<Vec<_>>();
    let state = RegisterState::new(receiver.clone(), &arguments, 0);

    assert!(matches!(
      state.registers[abi::RETURN.index()],
      Value::Undefined
    ));
    assert!(matches!(
      &state.registers[abi::RECEIVER.index()],
      value if crate::runtime_utils::strict_equal(value, &receiver)
    ));
    for (index, argument) in arguments
      .iter()
      .take(abi::REGISTER_ARGUMENT_COUNT)
      .enumerate()
    {
      let register = abi::argument_register(index).expect("register argument");
      assert!(crate::runtime_utils::strict_equal(
        &state.registers[register.index()],
        argument,
      ));
    }
    assert_eq!(Register::COUNT, state.registers.len());
  }
}
