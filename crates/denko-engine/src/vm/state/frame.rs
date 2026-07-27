// SPDX-License-Identifier: MPL-2.0
use super::ExecutionContext;
use super::handler::RiscExecutionState;
use crate::bytecode::UpvalueSpec;
use crate::value::Value;

/// Call frame.
pub(crate) struct Frame {
  /// The suspended caller execution context.
  pub(crate) context: ExecutionContext,
  /// Suspended caller registers and spills for an interpreter-managed call.
  pub(crate) risc_state: Option<RiscExecutionState>,
  /// The callee value used to enter this frame.
  pub(crate) callee: Value,
  /// The `[[NewTarget]]` value in effect for this frame, if any.
  pub(crate) new_target: Option<Value>,
  /// Upvalue metadata for the current function.
  pub(crate) upvalues: Vec<UpvalueSpec>,
}
