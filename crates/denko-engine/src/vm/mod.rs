// SPDX-License-Identifier: MPL-2.0
//! VM entry point and public surface for the virtual machine.
//!
//! This crate exposes the VM core (state::Vm) and a set of small runtime
//! helper modules under `runtime` that implement auxiliary behaviour such as
//! microtasks, coercions, and call-site helpers. The intent is to keep the
//! public surface small: use `Vm` to create and drive execution, and prefer
//! helpers in `runtime` for shared, well-contained utilities.
//!
//! Example
//!
//! ```
//! use denko::vm::Vm;
//! // construct a VM and run bytecode
//! ```

mod interrupt;
/// VM-owned profiling and future native compilation metadata.
mod jit;
mod region;
/// Runtime helpers split into structured submodules.
pub mod runtime;
pub mod state;

pub use interrupt::{InterruptRequestError, VmInterruptHandle};
pub use jit::NativeExecutionStats;
pub use runtime::same_value;
pub use state::{HostRoot, Vm};
