//! VM entry point re-exporting state and runtime helpers.

/// Runtime helpers split into structured submodules.
pub mod runtime;
pub mod state;

pub use runtime::same_value;
pub use state::Vm;
