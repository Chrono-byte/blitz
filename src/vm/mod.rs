//! VM entry point re-exporting state and runtime helpers.

pub mod runtime;
pub mod state;

pub use runtime::same_value;
pub use state::Vm;
