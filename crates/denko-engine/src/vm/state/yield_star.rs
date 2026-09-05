// SPDX-License-Identifier: MPL-2.0
use crate::value::Value;

#[derive(Clone)]
pub(crate) enum YieldStarResume {
  Next(Value),
  Throw(Value),
  Return(Value),
}

#[derive(Clone, Copy)]
pub(crate) enum YieldStarPhase {
  Resume,
  ReturnValue,
  Result {
    is_return: bool,
    missing_throw: bool,
  },
  SyncValue {
    done: bool,
    is_return: bool,
  },
}

#[derive(Clone)]
pub(crate) struct YieldStarState {
  pub(crate) iterator: Value,
  pub(crate) next_method: Value,
  pub(crate) resume: Option<YieldStarResume>,
  pub(crate) is_async: bool,
  pub(crate) sync: bool,
  pub(crate) phase: YieldStarPhase,
  pub(crate) settled: Option<Value>,
}
