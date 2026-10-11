// SPDX-License-Identifier: MPL-2.0
mod async_generators;
pub mod async_tasks;
/// Call handling and tail-call helpers.
pub mod calls;
mod coercions;
mod dynamic_code;
mod exceptions;
/// Main VM execution loop and opcode helpers.
pub mod execution;
mod iterators;
mod lexical;
mod lifecycle;
mod literals;
pub(crate) mod modules;
mod operators;
mod property;
mod realm;
/// Same-value comparison helper.
pub use property::same_value;
