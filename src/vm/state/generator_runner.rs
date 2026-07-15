use super::{RiscExecutionState, Vm};

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
