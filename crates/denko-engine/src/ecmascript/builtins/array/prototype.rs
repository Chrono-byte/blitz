// SPDX-License-Identifier: MPL-2.0
//! Ordered Array prototype installation phases.

use super::*;

mod after_iterator;
mod before_iterator;

pub(super) use after_iterator::install_after_iterator;
pub(super) use before_iterator::install_before_iterator;
