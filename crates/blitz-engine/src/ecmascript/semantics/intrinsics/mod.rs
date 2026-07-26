// SPDX-License-Identifier: MPL-2.0
use super::*;

mod bootstrap;
mod core;
mod foundation;
mod globals;
mod hidden;

pub(crate) use bootstrap::install_minimal_kernel_constructors;
pub(crate) use core::{IntrinsicInstaller, builtin_object, set_builtin_length, set_builtin_name};
pub(crate) use foundation::create_foundation;
pub(crate) use globals::{GlobalIntrinsicValues, install_global_bindings};
pub(crate) use hidden::install_hidden_builtins;
