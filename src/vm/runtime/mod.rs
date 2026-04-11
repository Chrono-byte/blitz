/// Async/microtask helpers and background runtime utilities.
pub mod async_tasks;
/// Call handling and tail-call helpers.
pub mod calls;
/// Shared coercion and numeric parsing helpers.
mod coercions;
/// Exception construction and unwinding helpers.
mod exceptions;
/// Main VM execution loop and opcode helpers.
pub mod execution;
/// Lexical environment and identifier resolution helpers.
mod lexical;
/// Literal and closure construction helpers.
mod literals;
/// Property access, descriptors, and inline cache helpers.
mod property;
/// Same-value comparison helper.
pub use execution::same_value;
