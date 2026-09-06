// SPDX-License-Identifier: MPL-2.0

//! Immutable native-region plans decoded from verified portable bytecode.

pub(super) mod numeric_body;
pub(in crate::vm) mod typed_view;
pub(super) use typed_view::TypedViewRegion;
mod string_body;
pub(super) use numeric_body::NumericOperation;
pub(super) use string_body::StringBodyRegion;
mod patterns;
pub(super) use numeric_body::NumericBodyRegion;
mod array_read_sum;
mod property_chain;
pub(super) use array_read_sum::ArrayReadSumRegion;
pub(super) use property_chain::PropertyChainRegion;

use crate::bytecode::{
  Chunk, Function, Instruction, Opcode, decode_aux16, decode_compare_jump, decode_rd, decode_rs,
};
use patterns::*;

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
  NumericBody(NumericBodyRegion),
  StringBody(StringBodyRegion),
  NumericLoop(NumericLoopRegion),
  PropertyChain(PropertyChainRegion),
  ArrayReadSum(ArrayReadSumRegion),
  TypedView(TypedViewRegion),
  DenseArrayPush(DenseArrayPushRegion),
  DenseMaskedRead(DenseMaskedReadRegion),
  OwnMethodNumericLoop(OwnMethodNumericLoopRegion),
  OwnMethodEffectLoop(OwnMethodEffectLoopRegion),
  OwnMethodReadLoop(OwnMethodReadLoopRegion),
  OwnMethodBinaryLoop(OwnMethodBinaryLoopRegion),
}

impl NativeRegion {
  pub(super) fn cost(self) -> RegionCost {
    match self {
      Self::StringBody(_) => RegionCost { max_iterations: 64 },
      Self::NumericBody(_) => RegionCost { max_iterations: 64 },
      Self::NumericLoop(region) => region.program.cost(),
      Self::TypedView(_) => RegionCost { max_iterations: 64 },
      Self::ArrayReadSum(_) => RegionCost { max_iterations: 64 },
      Self::PropertyChain(_) => RegionCost { max_iterations: 64 },
      Self::DenseArrayPush(_) => RegionCost { max_iterations: 64 },
      Self::DenseMaskedRead(_) => RegionCost { max_iterations: 64 },
      Self::OwnMethodNumericLoop(region) => region.program.cost(),
      Self::OwnMethodEffectLoop(region) => region.program.cost(),
      Self::OwnMethodReadLoop(region) => region.program.cost(),
      Self::OwnMethodBinaryLoop(region) => region.program.cost(),
    }
  }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct NumericBinaryParameters {
  pub(super) left_slot: usize,
  pub(super) right_slot: usize,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct OwnMethodBinaryLoopRegion {
  pub(super) entry_pc: usize,
  pub(super) completed_pc: usize,
  pub(super) index_slot: usize,
  pub(super) accumulator_slot: usize,
  pub(super) receiver_slot: usize,
  pub(super) call_temporary_slot: usize,
  pub(super) arithmetic_temporary_slot: usize,
  pub(super) method_key_constant: usize,
  pub(super) left_argument: f64,
  pub(super) right_argument: f64,
  pub(super) limit: f64,
  pub(super) step: f64,
  pub(super) index_register: usize,
  pub(super) limit_register: usize,
  pub(super) previous_index_register: usize,
  pub(super) next_index_register: usize,
  pub(super) step_register: usize,
  pub(super) left_argument_register: usize,
  pub(super) right_argument_register: usize,
  pub(super) program: ValidatedNumericProgram,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct ReceiverNumericRead {
  pub(super) property_key_constant: usize,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct OwnMethodReadLoopRegion {
  pub(super) entry_pc: usize,
  pub(super) completed_pc: usize,
  pub(super) index_slot: usize,
  pub(super) accumulator_slot: usize,
  pub(super) receiver_slot: usize,
  pub(super) call_temporary_slot: usize,
  pub(super) arithmetic_temporary_slot: usize,
  pub(super) method_key_constant: usize,
  pub(super) limit: f64,
  pub(super) step: f64,
  pub(super) index_register: usize,
  pub(super) limit_register: usize,
  pub(super) previous_index_register: usize,
  pub(super) next_index_register: usize,
  pub(super) step_register: usize,
  pub(super) program: ValidatedNumericProgram,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum MethodResultSemantics {
  Undefined,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct ReceiverNumericEffect {
  pub(super) property_key_constant: usize,
  pub(super) argument_slot: usize,
  pub(super) result: MethodResultSemantics,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct OwnMethodEffectLoopRegion {
  pub(super) entry_pc: usize,
  pub(super) completed_pc: usize,
  pub(super) index_slot: usize,
  pub(super) receiver_slot: usize,
  pub(super) call_temporary_slot: usize,
  pub(super) arithmetic_temporary_slot: usize,
  pub(super) method_key_constant: usize,
  pub(super) argument: f64,
  pub(super) limit: f64,
  pub(super) step: f64,
  pub(super) index_register: usize,
  pub(super) limit_register: usize,
  pub(super) previous_index_register: usize,
  pub(super) next_index_register: usize,
  pub(super) step_register: usize,
  pub(super) argument_register: usize,
  pub(super) program: ValidatedNumericProgram,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct OwnMethodNumericLoopRegion {
  pub(super) entry_pc: usize,
  pub(super) completed_pc: usize,
  pub(super) index_slot: usize,
  pub(super) accumulator_slot: usize,
  pub(super) receiver_slot: usize,
  pub(super) call_temporary_slot: usize,
  pub(super) arithmetic_temporary_slot: usize,
  pub(super) key_constant: usize,
  pub(super) call_site: usize,
  pub(super) argument: f64,
  pub(super) limit: f64,
  pub(super) step: f64,
  pub(super) index_register: usize,
  pub(super) limit_register: usize,
  pub(super) previous_index_register: usize,
  pub(super) next_index_register: usize,
  pub(super) step_register: usize,
  pub(super) argument_register: usize,
  pub(super) program: ValidatedNumericProgram,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct DenseMaskedReadRegion {
  pub(super) entry_pc: usize,
  pub(super) completed_pc: usize,
  pub(super) index_slot: usize,
  pub(super) accumulator_slot: usize,
  pub(super) array_slot: usize,
  pub(super) temporary_slot: usize,
  pub(super) index_register: usize,
  pub(super) limit_register: usize,
  pub(super) previous_index_register: usize,
  pub(super) next_index_register: usize,
  pub(super) step_register: usize,
  pub(super) mask_register: usize,
  pub(super) limit: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
#[repr(C)]
pub(crate) struct PreparedDenseMaskedReadRegion {
  pub(crate) index: f64,
  pub(crate) limit: f64,
  pub(crate) accumulator: f64,
  pub(crate) elements: [f64; 4],
  pub(crate) iterations: u32,
}

/// Values materialized at a native array-loop boundary.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum ArrayPushValue {
  Index,
  NegatedIndex,
  NextIndex,
  Limit,
  Step,
  Receiver,
  MethodKey,
  Method,
  Length,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct DenseArrayPushRegion {
  pub(super) entry_pc: usize,
  pub(super) completed_pc: usize,
  pub(super) index_slot: usize,
  pub(super) array_slot: usize,
  pub(super) call_temporary_slot: usize,
  pub(super) increment_temporary_slot: Option<usize>,
  pub(super) clones_index_cell: bool,
  pub(super) register_writeback: [Option<ArrayPushValue>; 16],
  pub(super) index_register: usize,
  pub(super) limit_register: usize,
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
  SubtractPrepared,
  MultiplyPrepared,
  DividePrepared,
}

impl NumericRegionShape {
  pub(crate) fn index(self) -> usize {
    match self {
      Self::AddIndex => 0,
      Self::AddPrepared => 1,
      Self::SubtractPrepared => 2,
      Self::MultiplyPrepared => 3,
      Self::DividePrepared => 4,
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
  OneConstant {
    value: f64,
    register: usize,
  },
  AccumulatorAndConstant {
    accumulator_register: usize,
    value: f64,
    value_register: usize,
    result_register: usize,
  },
}

impl NumericLoopRegion {
  pub(super) fn decode_property(
    code: &Chunk,
    pc: usize,
  ) -> Option<Self> {
    let words = code
      .words
      .get(pc..pc + NUMERIC_PROPERTY_LOOP.len())?;
    if !words
      .iter()
      .zip(NUMERIC_PROPERTY_LOOP)
      .all(|(word, expected)| opcode(*word) == *expected)
    {
      return None;
    }

    let index_slot = decode_aux16(words[0]) as usize;
    let accumulator_slot = decode_aux16(words[3]) as usize;
    let object_slot = decode_aux16(words[4]) as usize;
    let temporary_slot = decode_aux16(words[8]) as usize;
    let (condition, left, right, exit_offset) = decode_compare_jump(words[2]);
    let loop_len = words.len();
    let jump_payload = ((words[loop_len - 1] >> 8) & 0x00ff_ffff) as i32;
    let jump_offset = if jump_payload & 0x0080_0000 != 0 {
      jump_payload | !0x00ff_ffff
    } else {
      jump_payload
    };
    let completed_pc = pc + loop_len;
    let actual_exit = (pc as i64 + 3 + exit_offset as i64) as usize;
    let actual_backedge = (pc as i64 + loop_len as i64 + jump_offset as i64) as usize;
    let index_register = decode_rd(words[0]);
    let accumulator_register = decode_rd(words[3]);
    let step_register = decode_rd(words[16]);
    if condition != 2
      || left != index_register
      || right != decode_rd(words[1])
      || actual_exit != completed_pc
      || actual_backedge != pc
      || decode_rd(words[6]) != decode_rd(words[4])
      || decode_rs(words[6]) != decode_rd(words[5])
      || decode_rd(words[7]) != accumulator_register
      || decode_rs(words[7]) != decode_rd(words[6])
      || decode_rd(words[8]) != accumulator_register
      || decode_aux16(words[8]) as usize != temporary_slot
      || decode_aux16(words[9]) as usize != accumulator_slot
      || decode_aux16(words[10]) as usize != temporary_slot
      || decode_aux16(words[11]) as usize != accumulator_slot
      || decode_aux16(words[13]) as usize != index_slot
      || decode_rs(words[17]) != index_register
      || decode_rd(words[18]) != decode_rd(words[17])
      || decode_rs(words[18]) != step_register
      || decode_rd(words[19]) != decode_rd(words[18])
      || decode_aux16(words[19]) as usize != temporary_slot
      || decode_aux16(words[20]) as usize != index_slot
      || decode_aux16(words[21]) as usize != temporary_slot
      || decode_aux16(words[22]) as usize != index_slot
      || index_slot == accumulator_slot
      || index_slot == temporary_slot
      || accumulator_slot == temporary_slot
    {
      return None;
    }
    let key_constant = decode_aux16(words[5]) as usize;
    match code.constants.get(key_constant) {
      Some(crate::bytecode::Constant::String(_)) => {}
      _ => return None,
    }
    Some(Self {
      entry_pc: pc,
      completed_pc,
      index_slot,
      accumulator_slot,
      temporary_slot,
      limit: number_constant(code, words[1])?,
      step: number_constant(code, words[16])?,
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
    code: &Chunk,
    pc: usize,
  ) -> Option<Self> {
    let words = code
      .words
      .get(pc..pc + NUMERIC_ACCUMULATOR_LOOP.len())?;
    if !words
      .iter()
      .zip(NUMERIC_ACCUMULATOR_LOOP)
      .all(|(word, expected)| opcode(*word) == *expected)
    {
      return None;
    }

    let index_slot = decode_aux16(words[0]) as usize;
    let accumulator_slot = decode_aux16(words[3]) as usize;
    let temporary_slot = decode_aux16(words[6]) as usize;
    let (condition, left, right, exit_offset) = decode_compare_jump(words[2]);
    let loop_len = words.len();
    let jump_payload = ((words[loop_len - 1] >> 8) & 0x00ff_ffff) as i32;
    let jump_offset = if jump_payload & 0x0080_0000 != 0 {
      jump_payload | !0x00ff_ffff
    } else {
      jump_payload
    };
    let completed_pc = pc + loop_len;
    let actual_exit = (pc as i64 + 3 + exit_offset as i64) as usize;
    let actual_backedge = (pc as i64 + loop_len as i64 + jump_offset as i64) as usize;
    let index_register = decode_rd(words[0]);
    let accumulator_register = decode_rd(words[3]);
    let step_register = decode_rd(words[14]);
    if condition != 2
      || left != index_register
      || right != decode_rd(words[1])
      || actual_exit != completed_pc
      || actual_backedge != pc
      || decode_rd(words[5]) != accumulator_register
      || decode_rs(words[5]) != decode_rd(words[4])
      || decode_rd(words[6]) != accumulator_register
      || decode_aux16(words[6]) as usize != temporary_slot
      || decode_aux16(words[7]) as usize != accumulator_slot
      || decode_aux16(words[8]) as usize != temporary_slot
      || decode_aux16(words[9]) as usize != accumulator_slot
      || decode_aux16(words[11]) as usize != index_slot
      || decode_rs(words[15]) != index_register
      || decode_rd(words[16]) != decode_rd(words[15])
      || decode_rs(words[16]) != step_register
      || decode_rd(words[17]) != decode_rd(words[16])
      || decode_aux16(words[17]) as usize != temporary_slot
      || decode_aux16(words[18]) as usize != index_slot
      || decode_aux16(words[19]) as usize != temporary_slot
      || decode_aux16(words[20]) as usize != index_slot
      || index_slot == accumulator_slot
      || index_slot == temporary_slot
      || accumulator_slot == temporary_slot
    {
      return None;
    }

    Some(Self {
      entry_pc: pc,
      completed_pc,
      index_slot,
      accumulator_slot,
      temporary_slot,
      limit: number_constant(code, words[1])?,
      step: number_constant(code, words[14])?,
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
    code: &Chunk,
    pc: usize,
  ) -> Option<Self> {
    use ArrayPushValue as V;
    let words = code.words.get(pc..)?;
    let body_length = DENSE_ARRAY_PUSH_BODY.len();
    let body = words.get(..body_length)?;
    if !body
      .iter()
      .zip(DENSE_ARRAY_PUSH_BODY)
      .all(|(word, expected)| opcode(*word) == *expected)
    {
      return None;
    }
    let index_slot = decode_aux16(body[0]) as usize;
    let array_slot = decode_aux16(body[3]) as usize;
    let call_temporary_slot = decode_aux16(body[4]) as usize;
    if index_slot == array_slot
      || index_slot == call_temporary_slot
      || array_slot == call_temporary_slot
    {
      return None;
    }
    let index_register = decode_rd(body[0]) as usize;
    let limit_register = decode_rd(body[1]) as usize;
    let limit = number_constant(code, body[1])?;
    let (condition, left, right, exit_offset) = decode_compare_jump(body[2]);
    if condition != 2
      || left as usize != index_register
      || right as usize != limit_register
      || left == right
    {
      return None;
    }
    let mut registers = [None; 16];
    registers[index_register] = Some(V::Index);
    registers[limit_register] = Some(V::Limit);
    let mut receiver_stored = false;
    for &word in &body[3..] {
      let rd = decode_rd(word) as usize;
      let rs = decode_rs(word) as usize;
      match opcode(word) {
        Opcode::LoadLocal => {
          let slot = decode_aux16(word) as usize;
          registers[rd] = Some(
            if slot == array_slot || (slot == call_temporary_slot && receiver_stored) {
              V::Receiver
            } else if slot == index_slot {
              V::Index
            } else {
              return None;
            },
          );
        }
        Opcode::StoreLocal => {
          if decode_aux16(word) as usize != call_temporary_slot
            || registers[rd] != Some(V::Receiver)
          {
            return None;
          }
          receiver_stored = true;
        }
        Opcode::Const => {
          if !matches!(code.constants.get(decode_aux16(word) as usize), Some(crate::bytecode::Constant::String(key)) if key.as_str_lossy() == "push")
          {
            return None;
          }
          registers[rd] = Some(V::MethodKey);
        }
        Opcode::GetProperty => {
          if registers[rd] != Some(V::Receiver) || registers[rs] != Some(V::MethodKey) {
            return None;
          }
          registers[rd] = Some(V::Method);
        }
        Opcode::Move => registers[rd] = Some(registers[rs]?),
        Opcode::CallMethod => {
          if decode_aux16(word) != 1
            || registers[0] != Some(V::Method)
            || registers[1] != Some(V::Receiver)
            || registers[2] != Some(V::Index)
          {
            return None;
          }
          registers[0] = Some(V::Length);
        }
        _ => return None,
      }
    }

    // Decode the update by its data flow, covering assignment, compound assignment,
    // and increment without depending on one register allocation or loop binding form.
    let mut clones_index_cell = false;
    let mut increment_temporary_slot = None;
    let mut step = None;
    let mut stored_index = false;
    let mut computed_next = false;
    for offset in body_length..body_length + 14 {
      let word = *words.get(offset)?;
      let rd = decode_rd(word) as usize;
      let rs = decode_rs(word) as usize;
      let slot = decode_aux16(word) as usize;
      match opcode(word) {
        Opcode::CloneLocalCell if offset == body_length && slot == index_slot => {
          clones_index_cell = true
        }
        Opcode::LoadLocal => {
          registers[rd] = Some(if slot == index_slot {
            if stored_index { V::NextIndex } else { V::Index }
          } else if increment_temporary_slot == Some(slot) {
            V::NextIndex
          } else {
            return None;
          });
        }
        Opcode::Const if step.is_none() => {
          let value = number_constant(code, word)?;
          if !value.is_finite() || value <= 0.0 {
            return None;
          }
          step = Some(value);
          registers[rd] = Some(V::Step);
        }
        Opcode::Neg => {
          registers[rd] = Some(match registers[rs]? {
            V::Index => V::NegatedIndex,
            V::NegatedIndex => V::Index,
            _ => return None,
          })
        }
        Opcode::Move => registers[rd] = Some(registers[rs]?),
        Opcode::Add | Opcode::Increment => {
          if computed_next
            || registers[rd] != Some(V::Index)
            || registers[rs] != Some(V::Step)
            || (opcode(word) == Opcode::Increment && step != Some(1.0))
          {
            return None;
          }
          registers[rd] = Some(V::NextIndex);
          computed_next = true;
        }
        Opcode::StoreLocal => {
          if registers[rd] != Some(V::NextIndex) {
            return None;
          }
          if slot == index_slot {
            if stored_index {
              return None;
            }
            stored_index = true;
          } else if increment_temporary_slot.is_none()
            && slot != array_slot
            && slot != call_temporary_slot
          {
            increment_temporary_slot = Some(slot);
          } else {
            return None;
          }
        }
        Opcode::Jump => {
          let completed_pc = pc + offset + 1;
          let backedge = completed_pc as i64 + Instruction::decode(word).ok()?.operand_i24() as i64;
          if !stored_index
            || !computed_next
            || backedge != pc as i64
            || pc as i64 + 3 + exit_offset as i64 != completed_pc as i64
          {
            return None;
          }
          return Some(Self {
            entry_pc: pc,
            completed_pc,
            index_slot,
            array_slot,
            call_temporary_slot,
            increment_temporary_slot,
            clones_index_cell,
            index_register,
            limit_register,
            register_writeback: registers,
            limit,
            step: step?,
          });
        }
        _ => return None,
      }
    }
    None
  }
}

impl DenseMaskedReadRegion {
  pub(super) fn decode(
    code: &Chunk,
    pc: usize,
  ) -> Option<Self> {
    let words = code
      .words
      .get(pc..pc + DENSE_MASKED_READ_LOOP.len())?;
    if !words
      .iter()
      .zip(DENSE_MASKED_READ_LOOP)
      .all(|(word, expected)| opcode(*word) == *expected)
    {
      return None;
    }
    let index_slot = decode_aux16(words[0]) as usize;
    let accumulator_slot = decode_aux16(words[3]) as usize;
    let array_slot = decode_aux16(words[4]) as usize;
    let temporary_slot = decode_aux16(words[10]) as usize;
    let index_register = decode_rd(words[0]);
    let limit_register = decode_rd(words[1]);
    let accumulator_register = decode_rd(words[3]);
    let array_register = decode_rd(words[4]);
    let masked_index_register = decode_rd(words[5]);
    let mask_register = decode_rd(words[6]);
    let previous_index_register = decode_rd(words[15]);
    let step_register = decode_rd(words[18]);
    let next_index_register = decode_rd(words[19]);
    let (condition, left, right, exit_offset) = decode_compare_jump(words[2]);
    let jump_payload = ((words[25] >> 8) & 0x00ff_ffff) as i32;
    let jump_offset = if jump_payload & 0x0080_0000 != 0 {
      jump_payload | !0x00ff_ffff
    } else {
      jump_payload
    };
    let completed_pc = pc + words.len();
    let actual_exit = (pc as i64 + 3 + exit_offset as i64) as usize;
    let actual_backedge = (pc as i64 + words.len() as i64 + jump_offset as i64) as usize;
    if condition != 2
      || left != index_register
      || right != limit_register
      || actual_exit != completed_pc
      || actual_backedge != pc
      || decode_aux16(words[5]) as usize != index_slot
      || decode_rd(words[7]) != masked_index_register
      || decode_rs(words[7]) != mask_register
      || decode_rd(words[8]) != array_register
      || decode_rs(words[8]) != masked_index_register
      || decode_rd(words[9]) != accumulator_register
      || decode_rs(words[9]) != array_register
      || decode_rd(words[10]) != accumulator_register
      || decode_aux16(words[11]) as usize != accumulator_slot
      || decode_aux16(words[12]) as usize != temporary_slot
      || decode_aux16(words[13]) as usize != accumulator_slot
      || decode_aux16(words[14]) as usize != index_slot
      || decode_aux16(words[15]) as usize != index_slot
      || decode_rd(words[16]) != previous_index_register
      || decode_rs(words[16]) != previous_index_register
      || decode_rd(words[17]) != previous_index_register
      || decode_rs(words[17]) != previous_index_register
      || decode_rd(words[19]) != decode_rd(words[20])
      || decode_rs(words[19]) != previous_index_register
      || decode_rs(words[20]) != step_register
      || decode_rd(words[21]) != next_index_register
      || decode_rd(words[22]) != next_index_register
      || decode_rd(words[23]) != next_index_register
      || decode_rd(words[24]) != next_index_register
      || decode_aux16(words[21]) as usize != temporary_slot
      || decode_aux16(words[22]) as usize != index_slot
      || decode_aux16(words[23]) as usize != temporary_slot
      || decode_aux16(words[24]) as usize != index_slot
      || index_slot == accumulator_slot
      || index_slot == temporary_slot
      || accumulator_slot == temporary_slot
      || number_constant(code, words[6])? != 3.0
      || number_constant(code, words[18])? != 1.0
    {
      return None;
    }

    Some(Self {
      entry_pc: pc,
      completed_pc,
      index_slot,
      accumulator_slot,
      array_slot,
      temporary_slot,
      index_register: index_register as usize,
      limit_register: limit_register as usize,
      previous_index_register: previous_index_register as usize,
      next_index_register: next_index_register as usize,
      step_register: step_register as usize,
      mask_register: mask_register as usize,
      limit: number_constant(code, words[1])?,
    })
  }
}

impl NumericBinaryParameters {
  pub(super) fn decode(proto: &Function) -> Option<Self> {
    let [left, right, add, return_word] = proto.chunk.words.as_slice() else {
      return None;
    };
    let left_slot = decode_aux16(*left) as usize;
    let right_slot = decode_aux16(*right) as usize;
    if proto.arity < 2
      || proto.has_rest
      || proto.captures_this
      || !proto.upvalues.is_empty()
      || opcode(*left) != Opcode::LoadLocal
      || opcode(*right) != Opcode::LoadLocal
      || opcode(*add) != Opcode::Add
      || opcode(*return_word) != Opcode::Return
      || left_slot != 0
      || right_slot != 1
      || decode_rd(*add) != decode_rd(*left)
      || decode_rs(*add) != decode_rd(*right)
      || decode_rd(*return_word) != decode_rd(*add)
    {
      return None;
    }
    Some(Self {
      left_slot,
      right_slot,
    })
  }
}

impl OwnMethodBinaryLoopRegion {
  pub(super) fn decode(
    code: &Chunk,
    pc: usize,
  ) -> Option<Self> {
    let words = code
      .words
      .get(pc..pc + OWN_METHOD_BINARY_LOOP.len())?;
    if !words
      .iter()
      .zip(OWN_METHOD_BINARY_LOOP)
      .all(|(word, expected)| opcode(*word) == *expected)
    {
      return None;
    }
    let index_slot = decode_aux16(words[0]) as usize;
    let accumulator_slot = decode_aux16(words[3]) as usize;
    let receiver_slot = decode_aux16(words[4]) as usize;
    let call_temporary_slot = decode_aux16(words[5]) as usize;
    let arithmetic_temporary_slot = decode_aux16(words[19]) as usize;
    let index_register = decode_rd(words[0]);
    let limit_register = decode_rd(words[1]);
    let accumulator_register = decode_rd(words[3]);
    let receiver_register = decode_rd(words[4]);
    let callee_register = decode_rd(words[7]);
    let key_register = decode_rd(words[8]);
    let left_argument_register = decode_rd(words[10]);
    let right_argument_register = decode_rd(words[11]);
    let result_register = decode_rd(words[17]);
    let previous_index_register = decode_rd(words[24]);
    let step_register = decode_rd(words[27]);
    let next_index_register = decode_rd(words[28]);
    let (condition, left, right, exit_offset) = decode_compare_jump(words[2]);
    let jump_payload = ((words[34] >> 8) & 0x00ff_ffff) as i32;
    let jump_offset = if jump_payload & 0x0080_0000 != 0 {
      jump_payload | !0x00ff_ffff
    } else {
      jump_payload
    };
    let completed_pc = pc + words.len();
    if condition != 2
      || left != index_register
      || right != limit_register
      || (pc as i64 + 3 + exit_offset as i64) as usize != completed_pc
      || (pc as i64 + words.len() as i64 + jump_offset as i64) as usize != pc
      || decode_rd(words[5]) != receiver_register
      || decode_aux16(words[6]) as usize != call_temporary_slot
      || decode_aux16(words[7]) as usize != call_temporary_slot
      || decode_rd(words[9]) != callee_register
      || decode_rs(words[9]) != key_register
      || decode_rd(words[12]) != 2
      || decode_rs(words[12]) != left_argument_register
      || decode_rd(words[13]) != 3
      || decode_rs(words[13]) != right_argument_register
      || decode_rd(words[14]) != 1
      || decode_rs(words[14]) != receiver_register
      || decode_rd(words[15]) != 0
      || decode_rs(words[15]) != callee_register
      || decode_aux16(words[16]) != 2
      || decode_rd(words[17]) != result_register
      || decode_rs(words[17]) != 0
      || decode_rd(words[18]) != accumulator_register
      || decode_rs(words[18]) != result_register
      || decode_rd(words[19]) != accumulator_register
      || decode_aux16(words[20]) as usize != accumulator_slot
      || decode_aux16(words[21]) as usize != arithmetic_temporary_slot
      || decode_aux16(words[22]) as usize != accumulator_slot
      || decode_aux16(words[23]) as usize != index_slot
      || decode_aux16(words[24]) as usize != index_slot
      || decode_rd(words[25]) != previous_index_register
      || decode_rs(words[25]) != previous_index_register
      || decode_rd(words[26]) != previous_index_register
      || decode_rs(words[26]) != previous_index_register
      || decode_rd(words[28]) != decode_rd(words[29])
      || decode_rs(words[28]) != previous_index_register
      || decode_rs(words[29]) != step_register
      || decode_rd(words[30]) != next_index_register
      || decode_aux16(words[30]) as usize != arithmetic_temporary_slot
      || decode_rd(words[31]) != next_index_register
      || decode_aux16(words[31]) as usize != index_slot
      || decode_rd(words[32]) != next_index_register
      || decode_aux16(words[32]) as usize != arithmetic_temporary_slot
      || decode_rd(words[33]) != next_index_register
      || decode_aux16(words[33]) as usize != index_slot
      || [
        index_slot,
        accumulator_slot,
        receiver_slot,
        call_temporary_slot,
      ]
      .into_iter()
      .any(|slot| slot == arithmetic_temporary_slot)
      || index_slot == accumulator_slot
      || index_slot == receiver_slot
      || index_slot == call_temporary_slot
      || accumulator_slot == receiver_slot
      || accumulator_slot == call_temporary_slot
      || receiver_slot == call_temporary_slot
    {
      return None;
    }
    let method_key_constant = decode_aux16(words[8]) as usize;
    match code.constants.get(method_key_constant) {
      Some(crate::bytecode::Constant::String(_)) => {}
      _ => return None,
    }
    let step = number_constant(code, words[27])?;
    if step != 1.0 {
      return None;
    }
    Some(Self {
      entry_pc: pc,
      completed_pc,
      index_slot,
      accumulator_slot,
      receiver_slot,
      call_temporary_slot,
      arithmetic_temporary_slot,
      method_key_constant,
      left_argument: number_constant(code, words[10])?,
      right_argument: number_constant(code, words[11])?,
      limit: number_constant(code, words[1])?,
      step,
      index_register: index_register as usize,
      limit_register: limit_register as usize,
      previous_index_register: previous_index_register as usize,
      next_index_register: next_index_register as usize,
      step_register: step_register as usize,
      left_argument_register: left_argument_register as usize,
      right_argument_register: right_argument_register as usize,
      program: NumericProgramDraft::counted(NumericAddend::Prepared).validate()?,
    })
  }
}

impl ReceiverNumericRead {
  pub(super) fn decode(proto: &Function) -> Option<Self> {
    let words = proto.chunk.words.as_slice();
    let [get_this, key, get_property, return_word, ..] = words else {
      return None;
    };
    let valid_legacy_trailer = match words {
      [_, _, _, _, implicit_undefined, implicit_return] => {
        opcode(*implicit_undefined) == Opcode::Undefined
          && opcode(*implicit_return) == Opcode::Return
          && decode_rd(*implicit_return) == decode_rd(*implicit_undefined)
      }
      [_, _, _, _] => true,
      _ => false,
    };
    if proto.arity != 0
      || proto.has_rest
      || proto.captures_this
      || !proto.upvalues.is_empty()
      || !valid_legacy_trailer
      || opcode(*get_this) != Opcode::GetThis
      || opcode(*key) != Opcode::Const
      || opcode(*get_property) != Opcode::GetProperty
      || opcode(*return_word) != Opcode::Return
      || decode_rd(*get_property) != decode_rd(*get_this)
      || decode_rs(*get_property) != decode_rd(*key)
      || decode_rd(*return_word) != decode_rd(*get_property)
    {
      return None;
    }
    let property_key_constant = decode_aux16(*key) as usize;
    match proto.chunk.constants.get(property_key_constant) {
      Some(crate::bytecode::Constant::String(_)) => Some(Self {
        property_key_constant,
      }),
      _ => None,
    }
  }
}

impl OwnMethodReadLoopRegion {
  pub(super) fn decode(
    code: &Chunk,
    pc: usize,
  ) -> Option<Self> {
    let words = code
      .words
      .get(pc..pc + OWN_METHOD_READ_LOOP.len())?;
    if !words
      .iter()
      .zip(OWN_METHOD_READ_LOOP)
      .all(|(word, expected)| opcode(*word) == *expected)
    {
      return None;
    }
    let index_slot = decode_aux16(words[0]) as usize;
    let accumulator_slot = decode_aux16(words[3]) as usize;
    let receiver_slot = decode_aux16(words[4]) as usize;
    let call_temporary_slot = decode_aux16(words[5]) as usize;
    let arithmetic_temporary_slot = decode_aux16(words[15]) as usize;
    let index_register = decode_rd(words[0]);
    let limit_register = decode_rd(words[1]);
    let accumulator_register = decode_rd(words[3]);
    let receiver_register = decode_rd(words[4]);
    let callee_register = decode_rd(words[7]);
    let key_register = decode_rd(words[8]);
    let result_register = decode_rd(words[13]);
    let previous_index_register = decode_rd(words[20]);
    let step_register = decode_rd(words[23]);
    let next_index_register = decode_rd(words[24]);
    let (condition, left, right, exit_offset) = decode_compare_jump(words[2]);
    let jump_payload = ((words[30] >> 8) & 0x00ff_ffff) as i32;
    let jump_offset = if jump_payload & 0x0080_0000 != 0 {
      jump_payload | !0x00ff_ffff
    } else {
      jump_payload
    };
    let completed_pc = pc + words.len();
    if condition != 2
      || left != index_register
      || right != limit_register
      || (pc as i64 + 3 + exit_offset as i64) as usize != completed_pc
      || (pc as i64 + words.len() as i64 + jump_offset as i64) as usize != pc
      || decode_rd(words[5]) != receiver_register
      || decode_aux16(words[6]) as usize != call_temporary_slot
      || decode_aux16(words[7]) as usize != call_temporary_slot
      || decode_rd(words[9]) != callee_register
      || decode_rs(words[9]) != key_register
      || decode_rd(words[10]) != 1
      || decode_rs(words[10]) != receiver_register
      || decode_rd(words[11]) != 0
      || decode_rs(words[11]) != callee_register
      || decode_aux16(words[12]) != 0
      || decode_rd(words[13]) != result_register
      || decode_rs(words[13]) != 0
      || decode_rd(words[14]) != accumulator_register
      || decode_rs(words[14]) != result_register
      || decode_rd(words[15]) != accumulator_register
      || decode_aux16(words[16]) as usize != accumulator_slot
      || decode_aux16(words[17]) as usize != arithmetic_temporary_slot
      || decode_aux16(words[18]) as usize != accumulator_slot
      || decode_aux16(words[19]) as usize != index_slot
      || decode_aux16(words[20]) as usize != index_slot
      || decode_rd(words[21]) != previous_index_register
      || decode_rs(words[21]) != previous_index_register
      || decode_rd(words[22]) != previous_index_register
      || decode_rs(words[22]) != previous_index_register
      || decode_rd(words[24]) != decode_rd(words[25])
      || decode_rs(words[24]) != previous_index_register
      || decode_rs(words[25]) != step_register
      || decode_rd(words[26]) != next_index_register
      || decode_aux16(words[26]) as usize != arithmetic_temporary_slot
      || decode_rd(words[27]) != next_index_register
      || decode_aux16(words[27]) as usize != index_slot
      || decode_rd(words[28]) != next_index_register
      || decode_aux16(words[28]) as usize != arithmetic_temporary_slot
      || decode_rd(words[29]) != next_index_register
      || decode_aux16(words[29]) as usize != index_slot
      || [
        index_slot,
        accumulator_slot,
        receiver_slot,
        call_temporary_slot,
      ]
      .into_iter()
      .any(|slot| slot == arithmetic_temporary_slot)
      || index_slot == accumulator_slot
      || index_slot == receiver_slot
      || index_slot == call_temporary_slot
      || accumulator_slot == receiver_slot
      || accumulator_slot == call_temporary_slot
      || receiver_slot == call_temporary_slot
    {
      return None;
    }
    let method_key_constant = decode_aux16(words[8]) as usize;
    match code.constants.get(method_key_constant) {
      Some(crate::bytecode::Constant::String(_)) => {}
      _ => return None,
    }
    let step = number_constant(code, words[23])?;
    if step != 1.0 {
      return None;
    }
    Some(Self {
      entry_pc: pc,
      completed_pc,
      index_slot,
      accumulator_slot,
      receiver_slot,
      call_temporary_slot,
      arithmetic_temporary_slot,
      method_key_constant,
      limit: number_constant(code, words[1])?,
      step,
      index_register: index_register as usize,
      limit_register: limit_register as usize,
      previous_index_register: previous_index_register as usize,
      next_index_register: next_index_register as usize,
      step_register: step_register as usize,
      program: NumericProgramDraft::counted(NumericAddend::Prepared).validate()?,
    })
  }
}

impl ReceiverNumericEffect {
  pub(super) fn decode(proto: &Function) -> Option<Self> {
    const OPS: &[Opcode] = &[
      Opcode::GetThis,
      Opcode::StoreLocal,
      Opcode::Const,
      Opcode::StoreLocal,
      Opcode::LoadLocal,
      Opcode::LoadLocal,
      Opcode::GetProperty,
      Opcode::LoadLocal,
      Opcode::Add,
      Opcode::StoreLocal,
      Opcode::LoadLocal,
      Opcode::LoadLocal,
      Opcode::LoadLocal,
      Opcode::Move,
      Opcode::Move,
      Opcode::Move,
      Opcode::SetProperty,
      Opcode::Move,
      Opcode::Undefined,
      Opcode::Return,
    ];
    let words = proto.chunk.words.get(..OPS.len())?;
    if proto.chunk.words.len() != OPS.len()
      || proto.arity < 1
      || proto.has_rest
      || proto.captures_this
      || !proto.upvalues.is_empty()
      || !words
        .iter()
        .zip(OPS)
        .all(|(word, expected)| opcode(*word) == *expected)
    {
      return None;
    }
    let this_register = decode_rd(words[0]);
    let this_slot = decode_aux16(words[1]) as usize;
    let key_register = decode_rd(words[2]);
    let key_slot = decode_aux16(words[3]) as usize;
    let value_register = decode_rd(words[4]);
    let loaded_key_register = decode_rd(words[5]);
    let argument_register = decode_rd(words[7]);
    let value_slot = decode_aux16(words[9]) as usize;
    if decode_rd(words[1]) != this_register
      || decode_rd(words[3]) != key_register
      || decode_aux16(words[4]) as usize != this_slot
      || decode_aux16(words[5]) as usize != key_slot
      || decode_rd(words[6]) != value_register
      || decode_rs(words[6]) != loaded_key_register
      || decode_aux16(words[7]) != 0
      || decode_rd(words[8]) != value_register
      || decode_rs(words[8]) != argument_register
      || decode_rd(words[9]) != value_register
      || decode_aux16(words[10]) as usize != this_slot
      || decode_aux16(words[11]) as usize != key_slot
      || decode_aux16(words[12]) as usize != value_slot
      || decode_rd(words[13]) != 0
      || decode_rs(words[13]) != decode_rd(words[10])
      || decode_rd(words[14]) != 1
      || decode_rs(words[14]) != decode_rd(words[11])
      || decode_rd(words[15]) != 2
      || decode_rs(words[15]) != decode_rd(words[12])
      || decode_rd(words[16]) != 0
      || decode_rs(words[16]) != 1
      || decode_aux16(words[16]) != 2
      || decode_rs(words[17]) != 0
      || decode_rd(words[18]) != decode_rd(words[17])
      || decode_rd(words[19]) != decode_rd(words[18])
    {
      return None;
    }
    let property_key_constant = decode_aux16(words[2]) as usize;
    match proto.chunk.constants.get(property_key_constant) {
      Some(crate::bytecode::Constant::String(_)) => Some(Self {
        property_key_constant,
        argument_slot: 0,
        result: MethodResultSemantics::Undefined,
      }),
      _ => None,
    }
  }
}

impl OwnMethodEffectLoopRegion {
  pub(super) fn decode(
    code: &Chunk,
    pc: usize,
  ) -> Option<Self> {
    let words = code
      .words
      .get(pc..pc + OWN_METHOD_EFFECT_LOOP.len())?;
    if !words
      .iter()
      .zip(OWN_METHOD_EFFECT_LOOP)
      .all(|(word, expected)| opcode(*word) == *expected)
    {
      return None;
    }
    let index_slot = decode_aux16(words[0]) as usize;
    let receiver_slot = decode_aux16(words[3]) as usize;
    let call_temporary_slot = decode_aux16(words[4]) as usize;
    let arithmetic_temporary_slot = decode_aux16(words[22]) as usize;
    let index_register = decode_rd(words[0]);
    let limit_register = decode_rd(words[1]);
    let receiver_register = decode_rd(words[3]);
    let callee_register = decode_rd(words[6]);
    let key_register = decode_rd(words[7]);
    let argument_register = decode_rd(words[9]);
    let previous_index_register = decode_rd(words[16]);
    let step_register = decode_rd(words[19]);
    let next_index_register = decode_rd(words[20]);
    let (condition, left, right, exit_offset) = decode_compare_jump(words[2]);
    let jump_payload = ((words[26] >> 8) & 0x00ff_ffff) as i32;
    let jump_offset = if jump_payload & 0x0080_0000 != 0 {
      jump_payload | !0x00ff_ffff
    } else {
      jump_payload
    };
    let completed_pc = pc + words.len();
    if condition != 2
      || left != index_register
      || right != limit_register
      || (pc as i64 + 3 + exit_offset as i64) as usize != completed_pc
      || (pc as i64 + words.len() as i64 + jump_offset as i64) as usize != pc
      || decode_rd(words[4]) != receiver_register
      || decode_aux16(words[5]) as usize != call_temporary_slot
      || decode_aux16(words[6]) as usize != call_temporary_slot
      || decode_rd(words[8]) != callee_register
      || decode_rs(words[8]) != key_register
      || decode_rd(words[10]) != 2
      || decode_rs(words[10]) != argument_register
      || decode_rd(words[11]) != 1
      || decode_rs(words[11]) != receiver_register
      || decode_rd(words[12]) != 0
      || decode_rs(words[12]) != callee_register
      || decode_aux16(words[13]) != 1
      || decode_rs(words[14]) != 0
      || decode_aux16(words[15]) as usize != index_slot
      || decode_aux16(words[16]) as usize != index_slot
      || decode_rd(words[17]) != previous_index_register
      || decode_rs(words[17]) != previous_index_register
      || decode_rd(words[18]) != previous_index_register
      || decode_rs(words[18]) != previous_index_register
      || decode_rd(words[20]) != decode_rd(words[21])
      || decode_rs(words[20]) != previous_index_register
      || decode_rs(words[21]) != step_register
      || decode_rd(words[22]) != next_index_register
      || decode_rd(words[23]) != next_index_register
      || decode_aux16(words[23]) as usize != index_slot
      || decode_rd(words[24]) != next_index_register
      || decode_aux16(words[24]) as usize != arithmetic_temporary_slot
      || decode_rd(words[25]) != next_index_register
      || decode_aux16(words[25]) as usize != index_slot
      || index_slot == receiver_slot
      || index_slot == call_temporary_slot
      || index_slot == arithmetic_temporary_slot
      || receiver_slot == call_temporary_slot
      || receiver_slot == arithmetic_temporary_slot
      || call_temporary_slot == arithmetic_temporary_slot
    {
      return None;
    }
    let method_key_constant = decode_aux16(words[7]) as usize;
    match code.constants.get(method_key_constant) {
      Some(crate::bytecode::Constant::String(_)) => {}
      _ => return None,
    }
    let step = number_constant(code, words[19])?;
    if step != 1.0 {
      return None;
    }
    Some(Self {
      entry_pc: pc,
      completed_pc,
      index_slot,
      receiver_slot,
      call_temporary_slot,
      arithmetic_temporary_slot,
      method_key_constant,
      argument: number_constant(code, words[9])?,
      limit: number_constant(code, words[1])?,
      step,
      index_register: index_register as usize,
      limit_register: limit_register as usize,
      previous_index_register: previous_index_register as usize,
      next_index_register: next_index_register as usize,
      step_register: step_register as usize,
      argument_register: argument_register as usize,
      program: NumericProgramDraft::counted(NumericAddend::Prepared).validate()?,
    })
  }
}

impl OwnMethodNumericLoopRegion {
  pub(super) fn decode(
    code: &Chunk,
    pc: usize,
  ) -> Option<Self> {
    let words = code
      .words
      .get(pc..pc + OWN_METHOD_NUMERIC_LOOP.len())?;
    if !words
      .iter()
      .zip(OWN_METHOD_NUMERIC_LOOP)
      .all(|(word, expected)| opcode(*word) == *expected)
    {
      return None;
    }
    let index_slot = decode_aux16(words[0]) as usize;
    let accumulator_slot = decode_aux16(words[3]) as usize;
    let receiver_slot = decode_aux16(words[4]) as usize;
    let call_temporary_slot = decode_aux16(words[5]) as usize;
    let arithmetic_temporary_slot = decode_aux16(words[17]) as usize;
    let index_register = decode_rd(words[0]);
    let limit_register = decode_rd(words[1]);
    let accumulator_register = decode_rd(words[3]);
    let receiver_register = decode_rd(words[4]);
    let callee_register = decode_rd(words[7]);
    let key_register = decode_rd(words[8]);
    let argument_register = decode_rd(words[10]);
    let result_register = decode_rd(words[15]);
    let previous_index_register = decode_rd(words[22]);
    let step_register = decode_rd(words[25]);
    let next_index_register = decode_rd(words[26]);
    let (condition, left, right, exit_offset) = decode_compare_jump(words[2]);
    let jump_payload = ((words[32] >> 8) & 0x00ff_ffff) as i32;
    let jump_offset = if jump_payload & 0x0080_0000 != 0 {
      jump_payload | !0x00ff_ffff
    } else {
      jump_payload
    };
    let completed_pc = pc + words.len();
    let actual_exit = (pc as i64 + 3 + exit_offset as i64) as usize;
    let actual_backedge = (pc as i64 + words.len() as i64 + jump_offset as i64) as usize;
    if condition != 2
      || left != index_register
      || right != limit_register
      || actual_exit != completed_pc
      || actual_backedge != pc
      || decode_rd(words[5]) != receiver_register
      || decode_aux16(words[6]) as usize != call_temporary_slot
      || decode_aux16(words[7]) as usize != call_temporary_slot
      || decode_rd(words[9]) != callee_register
      || decode_rs(words[9]) != key_register
      || decode_rd(words[11]) != 2
      || decode_rs(words[11]) != argument_register
      || decode_rd(words[12]) != 1
      || decode_rs(words[12]) != receiver_register
      || decode_rd(words[13]) != 0
      || decode_rs(words[13]) != callee_register
      || decode_aux16(words[14]) != 1
      || decode_rd(words[15]) != result_register
      || decode_rs(words[15]) != 0
      || decode_rd(words[16]) != accumulator_register
      || decode_rs(words[16]) != result_register
      || decode_rd(words[17]) != accumulator_register
      || decode_aux16(words[18]) as usize != accumulator_slot
      || decode_aux16(words[19]) as usize != arithmetic_temporary_slot
      || decode_aux16(words[20]) as usize != accumulator_slot
      || decode_aux16(words[21]) as usize != index_slot
      || decode_aux16(words[22]) as usize != index_slot
      || decode_rd(words[23]) != previous_index_register
      || decode_rs(words[23]) != previous_index_register
      || decode_rd(words[24]) != previous_index_register
      || decode_rs(words[24]) != previous_index_register
      || decode_rd(words[26]) != decode_rd(words[27])
      || decode_rs(words[26]) != previous_index_register
      || decode_rs(words[27]) != step_register
      || decode_rd(words[28]) != next_index_register
      || decode_aux16(words[28]) as usize != arithmetic_temporary_slot
      || decode_rd(words[29]) != next_index_register
      || decode_aux16(words[29]) as usize != index_slot
      || decode_rd(words[30]) != next_index_register
      || decode_aux16(words[30]) as usize != arithmetic_temporary_slot
      || decode_rd(words[31]) != next_index_register
      || decode_aux16(words[31]) as usize != index_slot
      || [
        index_slot,
        accumulator_slot,
        receiver_slot,
        call_temporary_slot,
      ]
      .into_iter()
      .any(|slot| slot == arithmetic_temporary_slot)
      || index_slot == accumulator_slot
      || index_slot == receiver_slot
      || index_slot == call_temporary_slot
      || accumulator_slot == receiver_slot
      || accumulator_slot == call_temporary_slot
      || receiver_slot == call_temporary_slot
    {
      return None;
    }
    let key_constant = decode_aux16(words[8]) as usize;
    match code.constants.get(key_constant) {
      Some(crate::bytecode::Constant::String(_)) => {}
      _ => return None,
    }
    let step = number_constant(code, words[25])?;
    if step != 1.0 {
      return None;
    }

    Some(Self {
      entry_pc: pc,
      completed_pc,
      index_slot,
      accumulator_slot,
      receiver_slot,
      call_temporary_slot,
      arithmetic_temporary_slot,
      key_constant,
      call_site: pc + 14,
      argument: number_constant(code, words[10])?,
      limit: number_constant(code, words[1])?,
      step,
      index_register: index_register as usize,
      limit_register: limit_register as usize,
      previous_index_register: previous_index_register as usize,
      next_index_register: next_index_register as usize,
      step_register: step_register as usize,
      argument_register: argument_register as usize,
      program: NumericProgramDraft::counted(NumericAddend::Prepared).validate()?,
    })
  }
}

impl NumericLoopRegion {
  pub(super) fn decode_numeric_add_call(
    code: &Chunk,
    pc: usize,
  ) -> Option<Self> {
    let words = code
      .words
      .get(pc..pc + NUMERIC_ADD_CALL_LOOP.len())?;
    if !words
      .iter()
      .zip(NUMERIC_ADD_CALL_LOOP)
      .all(|(word, expected)| opcode(*word) == *expected)
    {
      return None;
    }
    let index_slot = decode_aux16(words[0]) as usize;
    let accumulator_slot = decode_aux16(words[4]) as usize;
    let callee_slot = decode_aux16(words[3]) as usize;
    let temporary_slot = decode_aux16(words[16]) as usize;
    let (condition, left, right, exit_offset) = decode_compare_jump(words[2]);
    let completed_pc = pc + words.len();
    let jump_payload = ((words[20] >> 8) & 0x00ff_ffff) as i32;
    let jump_offset = if jump_payload & 0x0080_0000 != 0 {
      jump_payload | !0x00ff_ffff
    } else {
      jump_payload
    };
    let index_register = decode_rd(words[0]);
    let accumulator_register = decode_rd(words[4]);
    let constant_register = decode_rd(words[5]);
    let result_register = decode_rd(words[10]);
    let increment_register = decode_rd(words[13]);
    if condition != 2
      || left != index_register
      || right != decode_rd(words[1])
      || (pc as i64 + 3 + exit_offset as i64) as usize != completed_pc
      || (pc as i64 + words.len() as i64 + jump_offset as i64) as usize != pc
      || decode_rd(words[6]) != 2
      || decode_rs(words[6]) != accumulator_register
      || decode_rd(words[7]) != 3
      || decode_rs(words[7]) != constant_register
      || decode_rd(words[8]) != 0
      || decode_rs(words[8]) != decode_rd(words[3])
      || decode_aux16(words[9]) != 2
      || decode_rs(words[10]) != 0
      || decode_rd(words[11]) != result_register
      || decode_aux16(words[11]) as usize != accumulator_slot
      || decode_aux16(words[12]) as usize != index_slot
      || decode_aux16(words[13]) as usize != index_slot
      || decode_rd(words[15]) != increment_register
      || decode_rs(words[15]) != decode_rd(words[14])
      || decode_rd(words[16]) != increment_register
      || decode_aux16(words[16]) as usize != temporary_slot
      || decode_aux16(words[17]) as usize != index_slot
      || decode_aux16(words[18]) as usize != temporary_slot
      || decode_rd(words[19]) != decode_rd(words[18])
      || decode_aux16(words[19]) as usize != index_slot
      || index_slot == accumulator_slot
      || index_slot == callee_slot
      || index_slot == temporary_slot
      || accumulator_slot == callee_slot
      || accumulator_slot == temporary_slot
      || callee_slot == temporary_slot
    {
      return None;
    }
    let argument = NumericLeafArguments::AccumulatorAndConstant {
      accumulator_register: 2,
      value: number_constant(code, words[5])?,
      value_register: 3,
      result_register: result_register as usize,
    };
    Some(Self {
      entry_pc: pc,
      completed_pc,
      index_slot,
      accumulator_slot,
      temporary_slot,
      limit: number_constant(code, words[1])?,
      step: number_constant(code, words[14])?,
      prerequisites: NumericLoopPrerequisites::DirectCall {
        callee_slot,
        call_site: pc + 9,
        arguments: argument,
      },
      exits: NumericExitMap {
        index_register: index_register as usize,
        limit_register: decode_rd(words[1]) as usize,
        body: NumericBodyWriteback::DirectCall {
          arguments: argument,
        },
      },
      program: NumericProgramDraft::counted(NumericAddend::Prepared).validate()?,
    })
  }

  pub(super) fn decode_call(
    code: &Chunk,
    pc: usize,
  ) -> Option<Self> {
    let (words, call_offset, argument_constant_offset, increment_offset) = if let Some(words) =
      code.words.get(pc..pc + 22)
      && opcode(words[6]) == Opcode::Call
    {
      (words, 6, None, 15)
    } else if let Some(words) = code.words.get(pc..pc + 24)
      && opcode(words[8]) == Opcode::Call
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

    let index_slot = decode_aux16(words[0]) as usize;
    let accumulator_slot = decode_aux16(words[3]) as usize;
    let callee_slot = decode_aux16(words[4]) as usize;
    let temporary_slot = decode_aux16(words[call_offset + 3]) as usize;
    let (condition, left, right, exit_offset) = decode_compare_jump(words[2]);
    let loop_len = words.len();
    let jump_payload = ((words[loop_len - 1] >> 8) & 0x00ff_ffff) as i32;
    let jump_offset = if jump_payload & 0x0080_0000 != 0 {
      jump_payload | !0x00ff_ffff
    } else {
      jump_payload
    };
    let completed_pc = pc + loop_len;
    let actual_exit = (pc as i64 + 3 + exit_offset as i64) as usize;
    let actual_backedge = (pc as i64 + loop_len as i64 + jump_offset as i64) as usize;
    let accumulator_register = decode_rd(words[3]);
    let callee_register = decode_rd(words[4]);
    let result_register = decode_rd(words[call_offset + 1]);
    let accumulator_store = call_offset + 3;
    let increment_register = decode_rd(words[increment_offset - 1]);
    if condition != 2
      || left != decode_rd(words[0])
      || right != decode_rd(words[1])
      || actual_exit != completed_pc
      || actual_backedge != pc
      || decode_rd(words[call_offset - 1]) != 0
      || decode_rs(words[call_offset - 1]) != callee_register
      || decode_aux16(words[call_offset]) as usize != argument_constant_offset.is_some() as usize
      || decode_rs(words[call_offset + 1]) != 0
      || decode_rd(words[call_offset + 2]) != accumulator_register
      || decode_rs(words[call_offset + 2]) != result_register
      || decode_rd(words[accumulator_store]) != accumulator_register
      || decode_aux16(words[accumulator_store]) as usize != temporary_slot
      || decode_aux16(words[accumulator_store + 1]) as usize != accumulator_slot
      || decode_aux16(words[accumulator_store + 2]) as usize != temporary_slot
      || decode_rd(words[accumulator_store + 3]) != decode_rd(words[accumulator_store + 2])
      || decode_aux16(words[accumulator_store + 3]) as usize != accumulator_slot
      || decode_aux16(words[accumulator_store + 4]) as usize != index_slot
      || decode_aux16(words[increment_offset - 1]) as usize != index_slot
      || decode_rd(words[increment_offset + 1]) != increment_register
      || decode_rs(words[increment_offset + 1]) != decode_rd(words[increment_offset])
      || decode_aux16(words[increment_offset + 2]) as usize != temporary_slot
      || decode_aux16(words[increment_offset + 3]) as usize != index_slot
      || decode_aux16(words[increment_offset + 4]) as usize != temporary_slot
      || decode_rd(words[increment_offset + 5]) != decode_rd(words[increment_offset + 4])
      || decode_aux16(words[loop_len - 2]) as usize != index_slot
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
        value: number_constant(code, words[offset])?,
        register: 2,
      }
    } else {
      NumericLeafArguments::None
    };

    Some(Self {
      entry_pc: pc,
      completed_pc,
      index_slot,
      accumulator_slot,
      temporary_slot,
      limit: number_constant(code, words[1])?,
      step: number_constant(code, words[increment_offset])?,
      prerequisites: NumericLoopPrerequisites::DirectCall {
        callee_slot,
        call_site: pc + call_offset,
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

#[cfg(test)]
mod array_push_decode_tests {
  use super::*;

  #[test]
  fn counted_push_rejects_wrong_register_flow_and_call_arity() {
    let session = crate::parser::Parser::parse_source(crate::js!(
      r#"
      function fill() { var values = []; for (var i = 0; i < 100; i++) values.push(i); }
    "#
    ))
    .expect("parse");
    let root = crate::compiler::compile_session(&session).expect("compile");
    let code = &root.functions[0].chunk;
    let region = (0..code.words.len())
      .find_map(|pc| DenseArrayPushRegion::decode(code, pc))
      .expect("recognized loop");
    for (offset, replacement) in [
      (
        10,
        crate::bytecode::encode_register_instruction(Opcode::Move, 2, 8),
      ),
      (
        13,
        crate::bytecode::encode_aux16_instruction(Opcode::CallMethod, 0, 2),
      ),
    ] {
      let mut changed = (**code).clone();
      changed.words[region.entry_pc + offset] = replacement;
      assert!(DenseArrayPushRegion::decode(&changed, region.entry_pc).is_none());
    }
    let mut changed = (**code).clone();
    let step = changed
      .words
      .iter()
      .enumerate()
      .skip(region.entry_pc + 15)
      .find(|(_, word)| opcode(**word) == Opcode::Const)
      .map(|(_, word)| decode_aux16(*word) as usize)
      .expect("step constant");
    changed.constants[step] = crate::bytecode::Constant::Number(2.0);
    // Increment adds one regardless of its unused source register.
    assert!(DenseArrayPushRegion::decode(&changed, region.entry_pc).is_none());
  }
}
