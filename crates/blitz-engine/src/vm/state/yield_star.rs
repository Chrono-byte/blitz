// SPDX-License-Identifier: MPL-2.0
use crate::value::Value;

#[derive(Clone)]
pub(crate) enum YieldStarResume {
  Next(Value),
  Throw(Value),
  Return(Value),
}

#[derive(Clone)]
pub(crate) struct YieldStarState {
  pub(crate) iterator: Value,
  pub(crate) next_method: Value,
  pub(crate) resume: Option<YieldStarResume>,
}
