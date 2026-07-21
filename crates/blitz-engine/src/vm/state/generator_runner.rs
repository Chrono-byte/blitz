use super::{RiscExecutionState, Vm, YieldStarResume};
use crate::value::Value;

/// Manages the execution state of a generator function.
pub(crate) struct GeneratorRunner {
  /// The VM executing the generator.
  pub(crate) vm: Vm,
  /// Persistent register file and spills for the suspended function.
  pub(crate) risc_state: RiscExecutionState,
  /// Whether the generator has been started.
  pub(crate) started: bool,
  /// Whether the generator has completed execution.
  pub(crate) completed: bool,
}

impl GeneratorRunner {
  pub(crate) fn resume_next(
    &mut self,
    value: Value,
  ) {
    if let Some(state) = &mut self.vm.yield_star_state {
      state.resume = Some(YieldStarResume::Next(value));
    } else {
      self.risc_state.resume_with(value);
    }
  }

  pub(crate) fn resume_throw(
    &mut self,
    value: Value,
  ) -> bool {
    let Some(state) = &mut self.vm.yield_star_state else {
      return false;
    };
    state.resume = Some(YieldStarResume::Throw(value));
    true
  }

  pub(crate) fn resume_return(
    &mut self,
    value: Value,
  ) -> bool {
    if let Some(state) = &mut self.vm.yield_star_state {
      state.resume = Some(YieldStarResume::Return(value));
      true
    } else {
      self.vm.begin_generator_return(value)
    }
  }
}
