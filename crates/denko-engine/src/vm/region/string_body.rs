// SPDX-License-Identifier: MPL-2.0
use super::*;

/// Straight-line counted loops containing primitive string operations.
/// Runtime preparation proves invariance and callable identity before folding.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(in crate::vm) struct StringBodyRegion {
  pub(in crate::vm) entry_pc: usize,
  pub(in crate::vm) completed_pc: usize,
}

impl StringBodyRegion {
  pub(in crate::vm) fn decode(
    code: &Chunk,
    pc: usize,
  ) -> Option<Self> {
    let header = code.words.get(pc..pc + 3)?;
    if opcode(header[0]) != Opcode::LoadLocal
      || !matches!(opcode(header[1]), Opcode::Const | Opcode::LoadLocal)
      || opcode(header[2]) != Opcode::CompareJump
    {
      return None;
    }
    let (condition, left, right, offset) = decode_compare_jump(header[2]);
    if condition != 2
      || left != decode_rd(header[0])
      || right != decode_rd(header[1])
      || left == right
    {
      return None;
    }
    let completed_pc = usize::try_from(pc as i64 + 3 + i64::from(offset)).ok()?;
    if completed_pc <= pc + 4 || completed_pc - pc > 128 {
      return None;
    }
    let last = *code.words.get(completed_pc - 1)?;
    if opcode(last) != Opcode::Jump
      || completed_pc as i64 + i64::from((last as i32) >> 8) != pc as i64
    {
      return None;
    }
    let mut calls = 0;
    for word in code.words.get(pc + 3..completed_pc - 1)? {
      match opcode(*word) {
        Opcode::LoadLocal
        | Opcode::GetGlobal
        | Opcode::StoreLocal
        | Opcode::Const
        | Opcode::Move
        | Opcode::Add
        | Opcode::Sub
        | Opcode::Mul
        | Opcode::Div
        | Opcode::Mod
        | Opcode::Neg
        | Opcode::GetProperty => {}
        Opcode::CallMethod if decode_aux16(*word) <= 3 && decode_rd(*word) == 0 => calls += 1,
        _ => return None,
      }
    }
    (calls > 0).then_some(Self {
      entry_pc: pc,
      completed_pc,
    })
  }
}
