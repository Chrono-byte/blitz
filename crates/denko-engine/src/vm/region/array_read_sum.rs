// SPDX-License-Identifier: MPL-2.0
use super::*;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(in crate::vm) struct ArrayReadSumRegion {
  pub entry_pc: usize,
  pub completed_pc: usize,
  pub index_slot: usize,
  pub accumulator_slot: usize,
  pub array_slot: usize,
  pub index_register: usize,
  pub limit_register: usize,
  pub key_register: usize,
  pub indices: [usize; 8],
  pub count: usize,
  pub limit: f64,
  pub step: f64,
}

impl ArrayReadSumRegion {
  #[allow(clippy::too_many_lines)]
  pub(in crate::vm) fn decode(
    code: &Chunk,
    pc: usize,
  ) -> Option<Self> {
    let words = code.words.get(pc..)?;
    let header = words.get(..4)?;
    if !header
      .iter()
      .zip([
        Opcode::LoadLocal,
        Opcode::Const,
        Opcode::CompareJump,
        Opcode::LoadLocal,
      ])
      .all(|(word, op)| opcode(*word) == op)
    {
      return None;
    }
    let ir = decode_rd(header[0]);
    let lr = decode_rd(header[1]);
    let (condition, left, right, exit) = decode_compare_jump(header[2]);
    if condition != 2 || left != ir || right != lr || ir == lr || decode_rd(header[3]) != ir {
      return None;
    }
    let index_slot = decode_aux16(header[0]) as usize;
    let accumulator_slot = decode_aux16(header[3]) as usize;
    let array_slot = decode_aux16(*words.get(4)?) as usize;
    if index_slot == accumulator_slot || index_slot == array_slot || accumulator_slot == array_slot
    {
      return None;
    }
    let kr = decode_rd(*words.get(5)?);
    if kr == ir || kr == lr {
      return None;
    }
    let mut cursor = 4;
    let mut indices = [0; 8];
    let mut count = 0;
    while opcode(*words.get(cursor)?) == Opcode::LoadLocal {
      let group = words.get(cursor..cursor + 4)?;
      if count == indices.len()
        || !group
          .iter()
          .zip([
            Opcode::LoadLocal,
            Opcode::Const,
            Opcode::GetProperty,
            Opcode::Add,
          ])
          .all(|(word, op)| opcode(*word) == op)
        || decode_rd(group[0]) != lr
        || decode_aux16(group[0]) as usize != array_slot
        || decode_rd(group[1]) != kr
        || decode_rd(group[2]) != lr
        || decode_rs(group[2]) != kr
        || decode_rd(group[3]) != ir
        || decode_rs(group[3]) != lr
      {
        return None;
      }
      let value = number_constant(code, group[1])?;
      if !value.is_finite()
        || value < 0.0
        || value >= f64::from(u32::MAX)
        || value.fract() != 0.0
        || value.is_sign_negative()
      {
        return None;
      }
      indices[count] = value as usize;
      count += 1;
      cursor += 4;
    }
    let tail = words.get(cursor..cursor + 6)?;
    if count == 0
      || !tail
        .iter()
        .zip([
          Opcode::StoreLocal,
          Opcode::LoadLocal,
          Opcode::Const,
          Opcode::Add,
          Opcode::StoreLocal,
          Opcode::Jump,
        ])
        .all(|(word, op)| opcode(*word) == op)
      || decode_rd(tail[0]) != ir
      || decode_aux16(tail[0]) as usize != accumulator_slot
      || decode_rd(tail[1]) != ir
      || decode_aux16(tail[1]) as usize != index_slot
      || decode_rd(tail[2]) != lr
      || decode_rd(tail[3]) != ir
      || decode_rs(tail[3]) != lr
      || decode_rd(tail[4]) != ir
      || decode_aux16(tail[4]) as usize != index_slot
    {
      return None;
    }
    let completed_pc = pc + cursor + 6;
    if pc as i64 + 3 + i64::from(exit) != completed_pc as i64
      || completed_pc as i64 + i64::from((tail[5] as i32) >> 8) != pc as i64
    {
      return None;
    }
    let limit = number_constant(code, header[1])?;
    let step = number_constant(code, tail[2])?;
    if !limit.is_finite() || !step.is_finite() || step <= 0.0 {
      return None;
    }
    Some(Self {
      entry_pc: pc,
      completed_pc,
      index_slot,
      accumulator_slot,
      array_slot,
      index_register: ir as usize,
      limit_register: lr as usize,
      key_register: kr as usize,
      indices,
      count,
      limit,
      step,
    })
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn array_sum_decoder_validates_all_registers_and_backedge() {
    let session = crate::parser::Parser::parse_source(crate::js!(
      r#"
      function sum(array) {
        var total = 0;
        for (var i = 0; i < 100; i = i + 1) total = total + array[0] + array[2];
        return total;
      }
    "#
    ))
    .expect("parse");
    let root = crate::compiler::compile_session(&session).expect("compile");
    let code = &root.functions[0].chunk;
    let region = (0..code.words.len())
      .find_map(|pc| ArrayReadSumRegion::decode(code, pc))
      .expect("array sum");
    assert_eq!(&region.indices[..region.count], &[0, 2]);
    for offset in 0..region.completed_pc - region.entry_pc {
      if offset == 2 {
        continue;
      }
      let mut changed = (**code).clone();
      changed.words[region.entry_pc + offset] ^= 1 << 8;
      assert!(
        ArrayReadSumRegion::decode(&changed, region.entry_pc).is_none(),
        "offset {offset}"
      );
    }
  }
}
