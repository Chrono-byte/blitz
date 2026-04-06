/// Async/microtask helpers and background runtime utilities.
pub mod async_tasks;
/// Main VM execution loop and opcode helpers.
pub mod execution;

pub use execution::same_value;
