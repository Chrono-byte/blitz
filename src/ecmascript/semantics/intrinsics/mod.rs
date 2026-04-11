use super::*;

mod bootstrap;
mod core;

pub(crate) use bootstrap::install_minimal_kernel_constructors;
pub(crate) use core::{builtin_object, set_builtin_length, set_builtin_name};
