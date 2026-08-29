// SPDX-License-Identifier: MPL-2.0
use super::{RegisterState, Vm, YieldStarResume};
use crate::value::Value;

#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) enum GeneratorStatus {
  SuspendedStart,
  SuspendedYield,
  Completed,
}

/// Manages the execution state of a generator function.
pub(crate) struct GeneratorRunner {
  /// The VM executing the generator.
  pub(crate) vm: Vm,
  /// Persistent register file and spill_slots for the suspended function.
  pub(crate) register_state: RegisterState,
  pub(crate) status: GeneratorStatus,
}

impl GeneratorRunner {
  pub(crate) fn resume_next(
    &mut self,
    value: Value,
  ) {
    if let Some(state) = &mut self.vm.execution.active.yield_star {
      state.resume = Some(YieldStarResume::Next(value));
    } else {
      self.register_state.resume_with(value);
    }
  }

  pub(crate) fn resume_throw(
    &mut self,
    value: Value,
  ) -> bool {
    let Some(state) = &mut self.vm.execution.active.yield_star else {
      return false;
    };
    state.resume = Some(YieldStarResume::Throw(value));
    true
  }

  pub(crate) fn resume_return(
    &mut self,
    value: Value,
  ) -> bool {
    if let Some(state) = &mut self.vm.execution.active.yield_star {
      state.resume = Some(YieldStarResume::Return(value));
      true
    } else {
      self.vm.begin_generator_return(value)
    }
  }

  pub(crate) fn complete(&mut self) {
    self
      .vm
      .discard_suspended_activation(&mut self.register_state);
    self.status = GeneratorStatus::Completed;
  }
}
