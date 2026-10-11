// SPDX-License-Identifier: MPL-2.0

mod interrupt;
mod jit;
mod region;
/// Runtime helpers split into structured submodules.
pub mod runtime;
pub mod state;

pub use interrupt::{InterruptRequestError, VmInterruptHandle};
pub use jit::NativeExecutionStats;
pub use runtime::same_value;
pub use state::{HostRoot, Vm};
