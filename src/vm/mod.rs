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
//! use blitz::vm::Vm;
//! // construct a VM and run bytecode
//! ```

/// Runtime helpers split into structured submodules.
pub mod runtime;
pub mod state;

pub use runtime::same_value;
pub use state::Vm;
