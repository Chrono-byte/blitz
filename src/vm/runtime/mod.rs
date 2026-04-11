/// Async/microtask helpers and background runtime utilities.
pub mod async_tasks;
/// Call handling and tail-call helpers.
pub mod calls;
/// Shared coercion and numeric parsing helpers.
mod coercions;
/// Main VM execution loop and opcode helpers.
pub mod execution;
/// Same-value comparison helper.
pub use execution::same_value;
