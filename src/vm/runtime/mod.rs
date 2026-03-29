/// Async/microtask helpers and background runtime utilities.
pub mod async_tasks;
/// Call-related helpers (call frames, invocation dispatch, etc.).
pub mod calls;
/// ECMAScript numeric/primitive coercion helpers.
pub mod coercions;
/// Diagnostic/debug helpers that inspect VM state.
pub mod diagnostics;
/// Main VM execution loop and opcode helpers.
pub mod execution;
/// Object/property helpers that back ECMAScript property semantics.
pub mod objects;

pub use execution::same_value;
