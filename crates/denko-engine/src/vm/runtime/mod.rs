// SPDX-License-Identifier: MPL-2.0
/// Async/microtask helpers and background runtime utilities.
pub mod async_tasks;
/// Call handling and tail-call helpers.
pub mod calls;
/// Shared coercion and numeric parsing helpers.
mod coercions;
/// Dynamic source parsing, compilation, and evaluation.
mod dynamic_code;
/// Exception construction and unwinding helpers.
mod exceptions;
/// Main VM execution loop and opcode helpers.
pub mod execution;
/// Iterator protocol and delegated-yield operations.
mod iterators;
/// Lexical environment and identifier resolution helpers.
mod lexical;
/// VM construction, roots, garbage collection, profiling, and reset.
mod lifecycle;
/// Literal and closure construction helpers.
mod literals;
/// Module-loader registration and namespace access.
pub(crate) mod modules;
/// ECMAScript relational and object operators.
mod operators;
/// Property access, descriptors, and inline cache helpers.
mod property;
/// Realm, global-object, and execution-mode access.
mod realm;
/// Same-value comparison helper.
pub use property::same_value;
