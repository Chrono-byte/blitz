// SPDX-License-Identifier: MPL-2.0
use super::ExecutionContext;
use super::handler::RegisterState;
use crate::bytecode::UpvalueSpec;
use crate::value::Value;
use crate::vm::state::RealmState;

/// Call frame.
pub(crate) struct CallFrame {
  /// The suspended caller execution context.
  pub(crate) context: ExecutionContext,
  /// Suspended caller registers and spill_slots for an interpreter-managed call.
  pub(crate) register_state: Option<RegisterState>,
  /// The caller realm to restore when this invocation completes.
  pub(crate) previous_realm: Option<RealmState>,
  /// The `[[NewTarget]]` value in effect for this frame, if any.
  pub(crate) new_target: Option<Value>,
  /// Upvalue metadata for the current function.
  pub(crate) upvalues: Vec<UpvalueSpec>,
}
