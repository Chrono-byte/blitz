// SPDX-License-Identifier: MPL-2.0

//! Immutable native-region plans decoded from verified portable bytecode.

use crate::bytecode::{Chunk, Op, decode_compare_jump, decode_rd, decode_risc_aux16, decode_rs};

const INDEX_VALUE: NumericValue = NumericValue(0);
const LIMIT_VALUE: NumericValue = NumericValue(1);
const ACCUMULATOR_VALUE: NumericValue = NumericValue(2);
const STEP_VALUE: NumericValue = NumericValue(3);
const ADDEND_VALUE: NumericValue = NumericValue(4);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct RegionId(u32);

impl RegionId {
  pub(super) fn index(self) -> usize {
    self.0 as usize
  }

  pub(super) fn from_index(index: usize) -> Option<Self> {
    u32::try_from(index).ok().map(Self)
  }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum NativeRegion {
  NumericLoop(NumericLoopRegion),
  DenseArrayPush(DenseArrayPushRegion),
}

impl NativeRegion {
  pub(super) fn cost(self) -> RegionCost {
    match self {
      Self::NumericLoop(region) => region.program.cost(),
      Self::DenseArrayPush(_) => RegionCost { max_iterations: 64 },
    }
  }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct DenseArrayPushRegion {
  pub(super) entry_pc: usize,
  pub(super) completed_pc: usize,
  pub(super) index_slot: usize,
  pub(super) array_slot: usize,
  pub(super) call_temporary_slot: usize,
  pub(super) increment_temporary_slot: usize,
  pub(super) index_register: usize,
  pub(super) limit_register: usize,
  pub(super) step_register: usize,
  pub(super) limit: f64,
  pub(super) step: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
#[repr(C)]
pub(crate) struct PreparedArrayPushRegion {
  pub(crate) index: f64,
  pub(crate) limit: f64,
  pub(crate) step: f64,
  pub(crate) iterations: u32,
  pub(crate) values: [f64; 64],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct RegionCost {
  pub(super) max_iterations: u8,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct NumericLoopRegion {
  pub(super) entry_pc: usize,
  pub(super) completed_pc: usize,
  pub(super) index_slot: usize,
  pub(super) accumulator_slot: usize,
  pub(super) temporary_slot: usize,
  pub(super) limit: f64,
  pub(super) step: f64,
  pub(super) prerequisites: NumericLoopPrerequisites,
  pub(super) exits: NumericExitMap,
  pub(super) program: ValidatedNumericProgram,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum NumericLoopPrerequisites {
  DirectCall {
    callee_slot: usize,
    call_site: usize,
    arguments: NumericLeafArguments,
  },
  AddIndex,
  AddProperty {
    object_slot: usize,
    key_constant: usize,
  },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct NumericExitMap {
  pub(super) index_register: usize,
  pub(super) limit_register: usize,
  pub(super) body: NumericBodyWriteback,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum NumericBodyWriteback {
  DirectCall {
    arguments: NumericLeafArguments,
  },
  Numeric {
    accumulator_register: usize,
    step_register: usize,
  },
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum NumericRegionShape {
  AddIndex,
  AddPrepared,
}

impl NumericRegionShape {
  pub(crate) fn index(self) -> usize {
    match self {
      Self::AddIndex => 0,
      Self::AddPrepared => 1,
    }
  }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct NumericValue(u8);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum NumericAddend {
  Index,
  Prepared,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct NumericProgramDraft {
  compare_lhs: NumericValue,
  compare_rhs: NumericValue,
  add_lhs: NumericValue,
  add_rhs: NumericValue,
  add_output: NumericValue,
  increment_value: NumericValue,
  increment_step: NumericValue,
  increment_output: NumericValue,
  addend: NumericAddend,
  batch_limit: u8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ValidatedNumericProgram {
  shape: NumericRegionShape,
  batch_limit: u8,
}

impl NumericProgramDraft {
  fn counted(addend: NumericAddend) -> Self {
    Self {
      compare_lhs: INDEX_VALUE,
      compare_rhs: LIMIT_VALUE,
      add_lhs: ACCUMULATOR_VALUE,
      add_rhs: match addend {
        NumericAddend::Index => INDEX_VALUE,
        NumericAddend::Prepared => ADDEND_VALUE,
      },
      add_output: ACCUMULATOR_VALUE,
      increment_value: INDEX_VALUE,
      increment_step: STEP_VALUE,
      increment_output: INDEX_VALUE,
      addend,
      batch_limit: 64,
    }
  }

  fn validate(self) -> Option<ValidatedNumericProgram> {
    let expected_rhs = match self.addend {
      NumericAddend::Index => INDEX_VALUE,
      NumericAddend::Prepared => ADDEND_VALUE,
    };
    if self.compare_lhs != INDEX_VALUE
      || self.compare_rhs != LIMIT_VALUE
      || self.add_lhs != ACCUMULATOR_VALUE
      || self.add_rhs != expected_rhs
      || self.add_output != ACCUMULATOR_VALUE
      || self.increment_value != INDEX_VALUE
      || self.increment_step != STEP_VALUE
      || self.increment_output != INDEX_VALUE
      || self.batch_limit == 0
    {
      return None;
    }
    Some(ValidatedNumericProgram {
      shape: match self.addend {
        NumericAddend::Index => NumericRegionShape::AddIndex,
        NumericAddend::Prepared => NumericRegionShape::AddPrepared,
      },
      batch_limit: self.batch_limit,
    })
  }
}

impl ValidatedNumericProgram {
  pub(super) fn shape(self) -> NumericRegionShape {
    self.shape
  }

  pub(super) fn cost(self) -> RegionCost {
    RegionCost {
      max_iterations: self.batch_limit,
    }
  }
}

#[derive(Clone, Copy, Debug, PartialEq)]
#[repr(C)]
pub(crate) struct PreparedNumericRegion {
  pub(crate) index: f64,
  pub(crate) limit: f64,
  pub(crate) accumulator: f64,
  pub(crate) step: f64,
  pub(crate) addend: f64,
  pub(crate) iterations: u32,
  pub(crate) shape: NumericRegionShape,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum NumericLeafArguments {
  None,
  OneConstant { value: f64, register: usize },
}

impl NumericLoopRegion {
  pub(super) fn decode_property(
    chunk: &Chunk,
    ip: usize,
  ) -> Option<Self> {
    let words = chunk
      .code
      .get(ip..ip + NUMERIC_PROPERTY_LOOP.len())?;
    if !words
      .iter()
      .zip(NUMERIC_PROPERTY_LOOP)
      .all(|(word, expected)| opcode(*word) == *expected)
    {
      return None;
    }

    let index_slot = decode_risc_aux16(words[0]) as usize;
    let accumulator_slot = decode_risc_aux16(words[3]) as usize;
    let object_slot = decode_risc_aux16(words[4]) as usize;
    let temporary_slot = decode_risc_aux16(words[8]) as usize;
    let (condition, left, right, exit_offset) = decode_compare_jump(words[2]);
    let loop_len = words.len();
    let jump_payload = ((words[loop_len - 1] >> 8) & 0x00ff_ffff) as i32;
    let jump_offset = if jump_payload & 0x0080_0000 != 0 {
      jump_payload | !0x00ff_ffff
    } else {
      jump_payload
    };
    let completed_pc = ip + loop_len;
    let actual_exit = (ip as i64 + 3 + exit_offset as i64) as usize;
    let actual_backedge = (ip as i64 + loop_len as i64 + jump_offset as i64) as usize;
    let index_register = decode_rd(words[0]);
    let accumulator_register = decode_rd(words[3]);
    let step_register = decode_rd(words[16]);
    if condition != 2
      || left != index_register
      || right != decode_rd(words[1])
      || actual_exit != completed_pc
      || actual_backedge != ip
      || decode_rd(words[6]) != decode_rd(words[4])
      || decode_rs(words[6]) != decode_rd(words[5])
      || decode_rd(words[7]) != accumulator_register
      || decode_rs(words[7]) != decode_rd(words[6])
      || decode_rd(words[8]) != accumulator_register
      || decode_risc_aux16(words[8]) as usize != temporary_slot
      || decode_risc_aux16(words[9]) as usize != accumulator_slot
      || decode_risc_aux16(words[10]) as usize != temporary_slot
      || decode_risc_aux16(words[11]) as usize != accumulator_slot
      || decode_risc_aux16(words[13]) as usize != index_slot
      || decode_rs(words[17]) != index_register
      || decode_rd(words[18]) != decode_rd(words[17])
      || decode_rs(words[18]) != step_register
      || decode_rd(words[19]) != decode_rd(words[18])
      || decode_risc_aux16(words[19]) as usize != temporary_slot
      || decode_risc_aux16(words[20]) as usize != index_slot
      || decode_risc_aux16(words[21]) as usize != temporary_slot
      || decode_risc_aux16(words[22]) as usize != index_slot
      || index_slot == accumulator_slot
      || index_slot == temporary_slot
      || accumulator_slot == temporary_slot
    {
      return None;
    }
    let key_constant = decode_risc_aux16(words[5]) as usize;
    match chunk.constants.get(key_constant) {
      Some(crate::bytecode::Constant::String(_)) => {}
      _ => return None,
    }

    Some(Self {
      entry_pc: ip,
      completed_pc,
      index_slot,
      accumulator_slot,
      temporary_slot,
      limit: number_constant(chunk, words[1])?,
      step: number_constant(chunk, words[16])?,
      prerequisites: NumericLoopPrerequisites::AddProperty {
        object_slot,
        key_constant,
      },
      exits: NumericExitMap {
        index_register: index_register as usize,
        limit_register: decode_rd(words[1]) as usize,
        body: NumericBodyWriteback::Numeric {
          accumulator_register: accumulator_register as usize,
          step_register: step_register as usize,
        },
      },
      program: NumericProgramDraft::counted(NumericAddend::Prepared).validate()?,
    })
  }

  pub(super) fn decode_accumulator(
    chunk: &Chunk,
    ip: usize,
  ) -> Option<Self> {
    let words = chunk
      .code
      .get(ip..ip + NUMERIC_ACCUMULATOR_LOOP.len())?;
    if !words
      .iter()
      .zip(NUMERIC_ACCUMULATOR_LOOP)
      .all(|(word, expected)| opcode(*word) == *expected)
    {
      return None;
    }

    let index_slot = decode_risc_aux16(words[0]) as usize;
    let accumulator_slot = decode_risc_aux16(words[3]) as usize;
    let temporary_slot = decode_risc_aux16(words[6]) as usize;
    let (condition, left, right, exit_offset) = decode_compare_jump(words[2]);
    let loop_len = words.len();
    let jump_payload = ((words[loop_len - 1] >> 8) & 0x00ff_ffff) as i32;
    let jump_offset = if jump_payload & 0x0080_0000 != 0 {
      jump_payload | !0x00ff_ffff
    } else {
      jump_payload
    };
    let completed_pc = ip + loop_len;
    let actual_exit = (ip as i64 + 3 + exit_offset as i64) as usize;
    let actual_backedge = (ip as i64 + loop_len as i64 + jump_offset as i64) as usize;
    let index_register = decode_rd(words[0]);
    let accumulator_register = decode_rd(words[3]);
    let step_register = decode_rd(words[14]);
    if condition != 2
      || left != index_register
      || right != decode_rd(words[1])
      || actual_exit != completed_pc
      || actual_backedge != ip
      || decode_rd(words[5]) != accumulator_register
      || decode_rs(words[5]) != decode_rd(words[4])
      || decode_rd(words[6]) != accumulator_register
      || decode_risc_aux16(words[6]) as usize != temporary_slot
      || decode_risc_aux16(words[7]) as usize != accumulator_slot
      || decode_risc_aux16(words[8]) as usize != temporary_slot
      || decode_risc_aux16(words[9]) as usize != accumulator_slot
      || decode_risc_aux16(words[11]) as usize != index_slot
      || decode_rs(words[15]) != index_register
      || decode_rd(words[16]) != decode_rd(words[15])
      || decode_rs(words[16]) != step_register
      || decode_rd(words[17]) != decode_rd(words[16])
      || decode_risc_aux16(words[17]) as usize != temporary_slot
      || decode_risc_aux16(words[18]) as usize != index_slot
      || decode_risc_aux16(words[19]) as usize != temporary_slot
      || decode_risc_aux16(words[20]) as usize != index_slot
      || index_slot == accumulator_slot
      || index_slot == temporary_slot
      || accumulator_slot == temporary_slot
    {
      return None;
    }

    Some(Self {
      entry_pc: ip,
      completed_pc,
      index_slot,
      accumulator_slot,
      temporary_slot,
      limit: number_constant(chunk, words[1])?,
      step: number_constant(chunk, words[14])?,
      prerequisites: NumericLoopPrerequisites::AddIndex,
      exits: NumericExitMap {
        index_register: index_register as usize,
        limit_register: decode_rd(words[1]) as usize,
        body: NumericBodyWriteback::Numeric {
          accumulator_register: accumulator_register as usize,
          step_register: step_register as usize,
        },
      },
      program: NumericProgramDraft::counted(NumericAddend::Index).validate()?,
    })
  }
}

impl DenseArrayPushRegion {
  pub(super) fn decode(
    chunk: &Chunk,
    ip: usize,
  ) -> Option<Self> {
    let words = chunk
      .code
      .get(ip..ip + DENSE_ARRAY_PUSH_LOOP.len())?;
    if !words
      .iter()
      .zip(DENSE_ARRAY_PUSH_LOOP)
      .all(|(word, expected)| opcode(*word) == *expected)
    {
      return None;
    }

    let index_slot = decode_risc_aux16(words[0]) as usize;
    let array_slot = decode_risc_aux16(words[3]) as usize;
    let call_temporary_slot = decode_risc_aux16(words[4]) as usize;
    let increment_temporary_slot = decode_risc_aux16(words[22]) as usize;
    let (condition, left, right, exit_offset) = decode_compare_jump(words[2]);
    let jump_payload = ((words[26] >> 8) & 0x00ff_ffff) as i32;
    let jump_offset = if jump_payload & 0x0080_0000 != 0 {
      jump_payload | !0x00ff_ffff
    } else {
      jump_payload
    };
    let completed_pc = ip + words.len();
    let actual_exit = (ip as i64 + 3 + exit_offset as i64) as usize;
    let actual_backedge = (ip as i64 + words.len() as i64 + jump_offset as i64) as usize;
    if condition != 2
      || left != decode_rd(words[0])
      || right != decode_rd(words[1])
      || actual_exit != completed_pc
      || actual_backedge != ip
      || decode_risc_aux16(words[5]) as usize != call_temporary_slot
      || decode_risc_aux16(words[6]) as usize != call_temporary_slot
      || decode_risc_aux16(words[9]) as usize != index_slot
      || decode_risc_aux16(words[16]) as usize != index_slot
      || decode_risc_aux16(words[23]) as usize != index_slot
      || decode_risc_aux16(words[24]) as usize != increment_temporary_slot
      || decode_risc_aux16(words[25]) as usize != index_slot
      || index_slot == call_temporary_slot
      || index_slot == increment_temporary_slot
      || call_temporary_slot == increment_temporary_slot
    {
      return None;
    }
    let Some(crate::bytecode::Constant::String(method_key)) = chunk
      .constants
      .get(decode_risc_aux16(words[7]) as usize)
    else {
      return None;
    };
    if method_key.as_str_lossy() != "push" {
      return None;
    }

    Some(Self {
      entry_pc: ip,
      completed_pc,
      index_slot,
      array_slot,
      call_temporary_slot,
      increment_temporary_slot,
      index_register: decode_rd(words[0]) as usize,
      limit_register: decode_rd(words[1]) as usize,
      step_register: decode_rd(words[19]) as usize,
      limit: number_constant(chunk, words[1])?,
      step: number_constant(chunk, words[19])?,
    })
  }
}

impl NumericLoopRegion {
  pub(super) fn decode_call(
    chunk: &Chunk,
    ip: usize,
  ) -> Option<Self> {
    let (words, call_offset, argument_constant_offset, increment_offset) = if let Some(words) =
      chunk.code.get(ip..ip + 22)
      && opcode(words[6]) == Op::Call
    {
      (words, 6, None, 15)
    } else if let Some(words) = chunk.code.get(ip..ip + 24)
      && opcode(words[8]) == Op::Call
    {
      (words, 8, Some(5), 17)
    } else {
      return None;
    };
    let expected_ops = if argument_constant_offset.is_some() {
      IDENTITY_CALL_LOOP
    } else {
      CONSTANT_CALL_LOOP
    };
    if !words
      .iter()
      .zip(expected_ops)
      .all(|(word, expected)| opcode(*word) == *expected)
    {
      return None;
    }

    let index_slot = decode_risc_aux16(words[0]) as usize;
    let accumulator_slot = decode_risc_aux16(words[3]) as usize;
    let callee_slot = decode_risc_aux16(words[4]) as usize;
    let temporary_slot = decode_risc_aux16(words[call_offset + 3]) as usize;
    let (condition, left, right, exit_offset) = decode_compare_jump(words[2]);
    let loop_len = words.len();
    let jump_payload = ((words[loop_len - 1] >> 8) & 0x00ff_ffff) as i32;
    let jump_offset = if jump_payload & 0x0080_0000 != 0 {
      jump_payload | !0x00ff_ffff
    } else {
      jump_payload
    };
    let completed_pc = ip + loop_len;
    let actual_exit = (ip as i64 + 3 + exit_offset as i64) as usize;
    let actual_backedge = (ip as i64 + loop_len as i64 + jump_offset as i64) as usize;
    let accumulator_register = decode_rd(words[3]);
    let callee_register = decode_rd(words[4]);
    let result_register = decode_rd(words[call_offset + 1]);
    let accumulator_store = call_offset + 3;
    let increment_register = decode_rd(words[increment_offset - 1]);
    if condition != 2
      || left != decode_rd(words[0])
      || right != decode_rd(words[1])
      || actual_exit != completed_pc
      || actual_backedge != ip
      || decode_rd(words[call_offset - 1]) != 0
      || decode_rs(words[call_offset - 1]) != callee_register
      || decode_risc_aux16(words[call_offset]) as usize
        != argument_constant_offset.is_some() as usize
      || decode_rs(words[call_offset + 1]) != 0
      || decode_rd(words[call_offset + 2]) != accumulator_register
      || decode_rs(words[call_offset + 2]) != result_register
      || decode_rd(words[accumulator_store]) != accumulator_register
      || decode_risc_aux16(words[accumulator_store]) as usize != temporary_slot
      || decode_risc_aux16(words[accumulator_store + 1]) as usize != accumulator_slot
      || decode_risc_aux16(words[accumulator_store + 2]) as usize != temporary_slot
      || decode_rd(words[accumulator_store + 3]) != decode_rd(words[accumulator_store + 2])
      || decode_risc_aux16(words[accumulator_store + 3]) as usize != accumulator_slot
      || decode_risc_aux16(words[accumulator_store + 4]) as usize != index_slot
      || decode_risc_aux16(words[increment_offset - 1]) as usize != index_slot
      || decode_rd(words[increment_offset + 1]) != increment_register
      || decode_rs(words[increment_offset + 1]) != decode_rd(words[increment_offset])
      || decode_risc_aux16(words[increment_offset + 2]) as usize != temporary_slot
      || decode_risc_aux16(words[increment_offset + 3]) as usize != index_slot
      || decode_risc_aux16(words[increment_offset + 4]) as usize != temporary_slot
      || decode_rd(words[increment_offset + 5]) != decode_rd(words[increment_offset + 4])
      || decode_risc_aux16(words[loop_len - 2]) as usize != index_slot
      || index_slot == accumulator_slot
      || index_slot == temporary_slot
      || accumulator_slot == temporary_slot
    {
      return None;
    }

    let argument = if let Some(offset) = argument_constant_offset {
      if decode_rd(words[offset + 1]) != 2
        || decode_rs(words[offset + 1]) != decode_rd(words[offset])
      {
        return None;
      }
      NumericLeafArguments::OneConstant {
        value: number_constant(chunk, words[offset])?,
        register: 2,
      }
    } else {
      NumericLeafArguments::None
    };

    Some(Self {
      entry_pc: ip,
      completed_pc,
      index_slot,
      accumulator_slot,
      temporary_slot,
      limit: number_constant(chunk, words[1])?,
      step: number_constant(chunk, words[increment_offset])?,
      prerequisites: NumericLoopPrerequisites::DirectCall {
        callee_slot,
        call_site: ip + call_offset,
        arguments: argument,
      },
      exits: NumericExitMap {
        index_register: decode_rd(words[0]) as usize,
        limit_register: decode_rd(words[1]) as usize,
        body: NumericBodyWriteback::DirectCall {
          arguments: argument,
        },
      },
      program: NumericProgramDraft::counted(NumericAddend::Prepared).validate()?,
    })
  }
}

fn opcode(word: u32) -> Op {
  // The VM verifies chunks before building optimization plans.
  unsafe { Op::from_byte_unchecked((word & 0xff) as u8) }
}

fn number_constant(
  chunk: &Chunk,
  word: u32,
) -> Option<f64> {
  match chunk
    .constants
    .get(decode_risc_aux16(word) as usize)
  {
    Some(crate::bytecode::Constant::Number(value)) => Some(*value),
    _ => None,
  }
}

const CONSTANT_CALL_LOOP: &[Op] = &[
  Op::LoadLocal,
  Op::Const,
  Op::CompareJump,
  Op::LoadLocal,
  Op::LoadLocal,
  Op::Move,
  Op::Call,
  Op::Move,
  Op::Add,
  Op::StoreLocal,
  Op::LoadLocal,
  Op::LoadLocal,
  Op::StoreLocal,
  Op::CloneLocalCell,
  Op::LoadLocal,
  Op::Const,
  Op::Add,
  Op::StoreLocal,
  Op::LoadLocal,
  Op::LoadLocal,
  Op::StoreLocal,
  Op::Jump,
];

const IDENTITY_CALL_LOOP: &[Op] = &[
  Op::LoadLocal,
  Op::Const,
  Op::CompareJump,
  Op::LoadLocal,
  Op::LoadLocal,
  Op::Const,
  Op::Move,
  Op::Move,
  Op::Call,
  Op::Move,
  Op::Add,
  Op::StoreLocal,
  Op::LoadLocal,
  Op::LoadLocal,
  Op::StoreLocal,
  Op::CloneLocalCell,
  Op::LoadLocal,
  Op::Const,
  Op::Add,
  Op::StoreLocal,
  Op::LoadLocal,
  Op::LoadLocal,
  Op::StoreLocal,
  Op::Jump,
];

const DENSE_ARRAY_PUSH_LOOP: &[Op] = &[
  Op::LoadLocal,
  Op::Const,
  Op::CompareJump,
  Op::LoadLocal,
  Op::StoreLocal,
  Op::LoadLocal,
  Op::LoadLocal,
  Op::Const,
  Op::GetProperty,
  Op::LoadLocal,
  Op::Move,
  Op::Move,
  Op::Move,
  Op::CallBuiltin,
  Op::Move,
  Op::CloneLocalCell,
  Op::LoadLocal,
  Op::Neg,
  Op::Neg,
  Op::Const,
  Op::Move,
  Op::Add,
  Op::StoreLocal,
  Op::LoadLocal,
  Op::LoadLocal,
  Op::StoreLocal,
  Op::Jump,
];

const NUMERIC_ACCUMULATOR_LOOP: &[Op] = &[
  Op::LoadLocal,
  Op::Const,
  Op::CompareJump,
  Op::LoadLocal,
  Op::LoadLocal,
  Op::Add,
  Op::StoreLocal,
  Op::LoadLocal,
  Op::LoadLocal,
  Op::StoreLocal,
  Op::CloneLocalCell,
  Op::LoadLocal,
  Op::Neg,
  Op::Neg,
  Op::Const,
  Op::Move,
  Op::Add,
  Op::StoreLocal,
  Op::LoadLocal,
  Op::LoadLocal,
  Op::StoreLocal,
  Op::Jump,
];

const NUMERIC_PROPERTY_LOOP: &[Op] = &[
  Op::LoadLocal,
  Op::Const,
  Op::CompareJump,
  Op::LoadLocal,
  Op::LoadLocal,
  Op::Const,
  Op::GetProperty,
  Op::Add,
  Op::StoreLocal,
  Op::LoadLocal,
  Op::LoadLocal,
  Op::StoreLocal,
  Op::CloneLocalCell,
  Op::LoadLocal,
  Op::Neg,
  Op::Neg,
  Op::Const,
  Op::Move,
  Op::Add,
  Op::StoreLocal,
  Op::LoadLocal,
  Op::LoadLocal,
  Op::StoreLocal,
  Op::Jump,
];
