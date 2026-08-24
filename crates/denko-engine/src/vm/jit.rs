// SPDX-License-Identifier: MPL-2.0
//! JIT-owned metadata. This module deliberately contains no native code yet.
//!
//! The interpreter and future JIT share these stable, VM-PC keyed identities and
//! feedback records. Generated machine code must never be the owner of feedback.
#![allow(dead_code, reason = "staged JIT metadata is consumed incrementally")]

use crate::value::Value;
pub(crate) mod native;

use crate::bytecode::{Chunk, Constant, Op};
use native::NativeCode;
use std::collections::HashMap;

#[derive(Clone, Copy, Debug)]
enum BaselineOp {
  SetNumberResult(f64),
  Return,
}

struct BaselineCompiler<'a> {
  chunk: &'a Chunk,
}

impl<'a> BaselineCompiler<'a> {
  fn compile_const_return(self) -> Result<NativeCode, String> {
    let Some([const_word, return_word]) = self.chunk.code.get(..2) else {
      return Err("baseline expects Const; Return prefix".to_string());
    };
    if (*const_word & 0xff) as u8 != Op::Const as u8
      || (*return_word & 0xff) as u8 != Op::Return as u8
      || ((*const_word >> 8) & 0xf) != ((*return_word >> 8) & 0xf)
    {
      return Err("baseline expects Const rN; Return rN".to_string());
    }
    let constant_index = ((*const_word >> 16) & 0xffff) as usize;
    let Constant::Number(value) = self
      .chunk
      .constants
      .get(constant_index)
      .ok_or_else(|| "constant index out of bounds".to_string())?
    else {
      return Err("baseline constant return currently supports numbers only".to_string());
    };
    let ops = [BaselineOp::SetNumberResult(*value), BaselineOp::Return];
    match ops {
      [BaselineOp::SetNumberResult(value), BaselineOp::Return] => NativeCode::return_number(value),
      _ => unreachable!(),
    }
  }

  fn compile_const_prefix(self) -> Result<NativeCode, String> {
    let Some(const_word) = self.chunk.code.first() else {
      return Err("baseline requires at least one instruction".to_string());
    };
    if (*const_word & 0xff) as u8 != Op::Const as u8 {
      return Err("baseline prefix must begin with Const".to_string());
    }
    let register = ((*const_word >> 8) & 0xf) as u32;
    let constant_index = ((*const_word >> 16) & 0xffff) as usize;
    let Constant::Number(value) = self
      .chunk
      .constants
      .get(constant_index)
      .ok_or_else(|| "constant index out of bounds".to_string())?
    else {
      return Err("baseline constant prefix currently supports numbers only".to_string());
    };
    if self
      .chunk
      .code
      .get(1)
      .is_some_and(|word| (word & 0xff) as u8 == Op::Return as u8)
    {
      return self.compile_const_return();
    }
    NativeCode::resume_number(register, 1, *value)
  }
}

pub(crate) const BASELINE_CALL_THRESHOLD: u64 = 1_000;
pub(crate) const BASELINE_LOOP_THRESHOLD: u64 = 1_000;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct FunctionId(u64);

impl FunctionId {
  pub(crate) const fn new(value: u64) -> Self {
    Self(value)
  }
}

pub(crate) type VmPc = usize;

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct JitStats {
  pub(crate) baseline_entries: u64,
  pub(crate) baseline_returns: u64,
  pub(crate) interpreter_resumes: u64,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct CodeVersionId(u64);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum JitCodeState {
  Valid,
  Invalidated,
  Retired,
}

#[derive(Clone, Debug)]
pub(crate) struct CompiledBlock {
  pub(crate) start_pc: VmPc,
  pub(crate) end_pc: VmPc,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct PcMap {
  pub(crate) entries: Vec<(usize, VmPc)>,
}

#[derive(Debug)]
pub(crate) struct JitCode {
  pub(crate) function: FunctionId,
  pub(crate) version: CodeVersionId,
  pub(crate) state: JitCodeState,
  pub(crate) blocks: Vec<CompiledBlock>,
  pub(crate) pc_map: PcMap,
  native: Option<NativeCode>,
}

#[derive(Default)]
pub(crate) struct JitRuntime {
  pub(crate) code: HashMap<FunctionId, Vec<JitCode>>,
  next_version: u64,
}

impl JitRuntime {
  pub(crate) fn allocate_version(&mut self) -> CodeVersionId {
    self.next_version = self.next_version.saturating_add(1);
    CodeVersionId(self.next_version)
  }

  pub(crate) fn install_placeholder(
    &mut self,
    function: FunctionId,
  ) -> CodeVersionId {
    let version = self.allocate_version();
    self
      .code
      .entry(function)
      .or_default()
      .push(JitCode {
        function,
        version,
        state: JitCodeState::Valid,
        blocks: Vec::new(),
        pc_map: PcMap::default(),
        native: None,
      });
    version
  }

  pub(crate) fn install_const_prefix(
    &mut self,
    function: FunctionId,
    chunk: &Chunk,
  ) -> Result<CodeVersionId, String> {
    let native = BaselineCompiler { chunk }.compile_const_prefix()?;
    let version = self.allocate_version();
    self
      .code
      .entry(function)
      .or_default()
      .push(JitCode {
        function,
        version,
        state: JitCodeState::Valid,
        blocks: vec![CompiledBlock {
          start_pc: 0,
          end_pc: chunk.code.len().min(2),
        }],
        pc_map: PcMap {
          entries: vec![(0, 0)],
        },
        native: Some(native),
      });
    Ok(version)
  }

  pub(crate) fn execute(
    &self,
    function: FunctionId,
    frame: *mut native::JitFrame,
  ) -> Option<native::JitExitCode> {
    let code = self
      .code
      .get(&function)?
      .iter()
      .find(|code| code.state == JitCodeState::Valid && code.native.is_some())?;
    // SAFETY: the frame is owned by the caller for the duration of this call.
    Some(unsafe { code.native.as_ref()?.call_frame(frame) })
  }

  pub(crate) fn invalidate(
    &mut self,
    function: FunctionId,
    version: CodeVersionId,
  ) {
    if let Some(code) = self.code.get_mut(&function) {
      if let Some(version) = code
        .iter_mut()
        .find(|code| code.version == version)
      {
        version.state = JitCodeState::Invalidated;
      }
    }
  }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum BaselineState {
  #[default]
  Cold,
  Requested,
  Compiling,
  Ready(CodeVersionId),
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum ValueKind {
  #[default]
  Unknown,
  Undefined,
  Null,
  Boolean,
  Number,
  String,
  Object,
  Other,
}

impl ValueKind {
  pub(crate) fn of(value: &Value) -> Self {
    match value {
      Value::Undefined => Self::Undefined,
      Value::Null => Self::Null,
      Value::Bool(_) => Self::Boolean,
      Value::Number(_) => Self::Number,
      Value::String(_) => Self::String,
      value if value.is_object() => Self::Object,
      _ => Self::Other,
    }
  }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct BinaryOpFeedback {
  pub(crate) lhs: ValueKind,
  pub(crate) rhs: ValueKind,
  pub(crate) result: ValueKind,
  pub(crate) observations: u32,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct BranchFeedback {
  pub(crate) taken: u64,
  pub(crate) not_taken: u64,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct FeedbackVector {
  pub(crate) binary_ops: HashMap<VmPc, BinaryOpFeedback>,
  pub(crate) branches: HashMap<VmPc, BranchFeedback>,
}

#[cfg(test)]
mod tests {
  use super::{BaselineCompiler, native::JitFrame};
  use crate::bytecode::{Chunk, Constant, Op};
  use crate::value::Value;
  use std::sync::Arc;

  #[test]
  fn compiles_actual_risc_const_return() {
    let mut chunk = Chunk::new(Arc::new(String::new()));
    chunk.constants.push(Constant::Number(42.0));
    chunk.write_instr(Op::Const as u32 | (0 << 8) | (0 << 16), 0);
    chunk.write_instr(Op::Return as u32, 0);
    let code = BaselineCompiler { chunk: &chunk }
      .compile_const_return()
      .expect("baseline compile");
    let mut result = Value::Undefined;
    let mut frame = JitFrame {
      vm: std::ptr::null_mut(),
      function_id: 0,
      vm_pc: 0,
      registers: std::ptr::null_mut(),
      register_count: 0,
      result: &mut result,
    };
    assert_eq!(unsafe { code.call_frame(&mut frame) }, 0);
    assert!(matches!(result, Value::Number(value) if value == 42.0));
  }

  #[test]
  fn exits_to_interpreter_at_next_risc_pc() {
    let mut chunk = Chunk::new(Arc::new(String::new()));
    chunk.constants.push(Constant::Number(42.0));
    chunk.write_instr(Op::Const as u32, 0);
    chunk.write_instr(Op::Neg as u32, 0);
    let code = BaselineCompiler { chunk: &chunk }
      .compile_const_prefix()
      .expect("baseline compile");
    let mut registers: [Value; 16] = std::array::from_fn(|_| Value::Undefined);
    let mut result = Value::Undefined;
    let mut frame = JitFrame {
      vm: std::ptr::null_mut(),
      function_id: 0,
      vm_pc: 0,
      registers: registers.as_mut_ptr(),
      register_count: registers.len(),
      result: &mut result,
    };
    assert_eq!(unsafe { code.call_frame(&mut frame) }, 1);
    assert_eq!(frame.vm_pc, 1);
    assert!(matches!(registers[0], Value::Number(value) if value == 42.0));
  }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct FunctionProfile {
  pub(crate) calls: u64,
  pub(crate) loop_counts: HashMap<VmPc, u64>,
  pub(crate) feedback: FeedbackVector,
  pub(crate) baseline: BaselineState,
}

impl FunctionProfile {
  pub(crate) fn record_call(&mut self) {
    self.calls = self.calls.saturating_add(1);
    if self.baseline == BaselineState::Cold && self.calls >= BASELINE_CALL_THRESHOLD {
      self.baseline = BaselineState::Requested;
    }
  }

  pub(crate) fn record_backedge(
    &mut self,
    pc: VmPc,
  ) {
    let count = self.loop_counts.entry(pc).or_default();
    *count = count.saturating_add(1);
    if self.baseline == BaselineState::Cold && *count >= BASELINE_LOOP_THRESHOLD {
      self.baseline = BaselineState::Requested;
    }
  }
}
