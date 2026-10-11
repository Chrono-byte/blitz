// SPDX-License-Identifier: MPL-2.0
use super::ExecutionContext;
use super::handler::RegisterState;
use crate::bytecode::UpvalueSpec;
use crate::value::Value;
use crate::vm::state::RealmState;

pub(crate) struct CallFrame {
  pub(crate) context: ExecutionContext,
  pub(crate) register_state: Option<RegisterState>,
  pub(crate) previous_realm: Option<RealmState>,
  pub(crate) new_target: Option<Value>,
  pub(crate) upvalues: Vec<UpvalueSpec>,
}
