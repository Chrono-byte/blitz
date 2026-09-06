// SPDX-License-Identifier: MPL-2.0

use super::*;

/// A counted, assignment-update loop summing an invariant own-property chain.
/// Every register written by the body is also described by this plan.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(in crate::vm) struct PropertyChainRegion {
  pub(in crate::vm) entry_pc: usize,
  pub(in crate::vm) completed_pc: usize,
  pub(in crate::vm) index_slot: usize,
  pub(in crate::vm) accumulator_slot: usize,
  pub(in crate::vm) object_slot: usize,
  pub(in crate::vm) index_register: usize,
  pub(in crate::vm) limit_register: usize,
  pub(in crate::vm) key_register: usize,
  pub(in crate::vm) key_constants: [usize; 8],
  pub(in crate::vm) key_count: usize,
  pub(in crate::vm) limit: f64,
  pub(in crate::vm) step: f64,
}

impl PropertyChainRegion {
  pub(in crate::vm) fn decode(
    code: &Chunk,
    pc: usize,
  ) -> Option<Self> {
    let words = code.words.get(pc..)?;
    let header = words.get(..5)?;
    if !header
      .iter()
      .zip([
        Opcode::LoadLocal,
        Opcode::Const,
        Opcode::CompareJump,
        Opcode::LoadLocal,
        Opcode::LoadLocal,
      ])
      .all(|(word, expected)| opcode(*word) == expected)
    {
      return None;
    }
    let index_register = decode_rd(header[0]);
    let limit_register = decode_rd(header[1]);
    let (condition, left, right, exit_offset) = decode_compare_jump(header[2]);
    if condition != 2
      || left != index_register
      || right != limit_register
      || index_register == limit_register
      || decode_rd(header[3]) != index_register
      || decode_rd(header[4]) != limit_register
    {
      return None;
    }
    let index_slot = decode_aux16(header[0]) as usize;
    let accumulator_slot = decode_aux16(header[3]) as usize;
    let object_slot = decode_aux16(header[4]) as usize;
    if index_slot == accumulator_slot
      || index_slot == object_slot
      || accumulator_slot == object_slot
    {
      return None;
    }
    let mut cursor = 5;
    let mut key_constants = [0; 8];
    let mut key_count = 0;
    let key_register = decode_rd(*words.get(cursor)?);
    if key_register == index_register || key_register == limit_register {
      return None;
    }
    while opcode(*words.get(cursor)?) == Opcode::Const {
      let pair = words.get(cursor..cursor + 2)?;
      if key_count == key_constants.len()
        || decode_rd(pair[0]) != key_register
        || opcode(pair[1]) != Opcode::GetProperty
        || decode_rd(pair[1]) != limit_register
        || decode_rs(pair[1]) != key_register
      {
        return None;
      }
      let constant = decode_aux16(pair[0]) as usize;
      if !matches!(
        code.constants.get(constant),
        Some(crate::bytecode::Constant::String(_))
      ) {
        return None;
      }
      key_constants[key_count] = constant;
      key_count += 1;
      cursor += 2;
    }
    let tail = words.get(cursor..cursor + 7)?;
    if key_count == 0
      || !tail
        .iter()
        .zip([
          Opcode::Add,
          Opcode::StoreLocal,
          Opcode::LoadLocal,
          Opcode::Const,
          Opcode::Add,
          Opcode::StoreLocal,
          Opcode::Jump,
        ])
        .all(|(word, expected)| opcode(*word) == expected)
    {
      return None;
    }
    if decode_rd(tail[0]) != index_register
      || decode_rs(tail[0]) != limit_register
      || decode_rd(tail[1]) != index_register
      || decode_aux16(tail[1]) as usize != accumulator_slot
      || decode_rd(tail[2]) != index_register
      || decode_aux16(tail[2]) as usize != index_slot
      || decode_rd(tail[3]) != limit_register
      || decode_rd(tail[4]) != index_register
      || decode_rs(tail[4]) != limit_register
      || decode_rd(tail[5]) != index_register
      || decode_aux16(tail[5]) as usize != index_slot
    {
      return None;
    }
    let completed_pc = pc + cursor + tail.len();
    let backedge = (tail[6] as i32) >> 8;
    if pc as i64 + 3 + i64::from(exit_offset) != completed_pc as i64
      || completed_pc as i64 + i64::from(backedge) != pc as i64
    {
      return None;
    }
    let limit = number_constant(code, header[1])?;
    let step = number_constant(code, tail[3])?;
    if !limit.is_finite() || !step.is_finite() || step <= 0.0 {
      return None;
    }
    Some(Self {
      entry_pc: pc,
      completed_pc,
      index_slot,
      accumulator_slot,
      object_slot,
      index_register: index_register as usize,
      limit_register: limit_register as usize,
      key_register: key_register as usize,
      key_constants,
      key_count,
      limit,
      step,
    })
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn property_chain_decoder_bounds_chain_length() {
    let template = crate::js!(
      r#"
      function sum(object) {
        var total = 0;
        for (var i = 0; i < 100; i = i + 1) total = total + object.KEY;
        return total;
      }
    "#
    );
    for count in 1..=9 {
      let source = template.replace("KEY", &vec!["a"; count].join("."));
      let session = crate::parser::Parser::parse_source(&source).expect("parse");
      let root = crate::compiler::compile_session(&session).expect("compile");
      let code = &root.functions[0].chunk;
      let region = (0..code.words.len()).find_map(|pc| PropertyChainRegion::decode(code, pc));
      assert_eq!(
        region.map(|region| region.key_count),
        (count <= 8).then_some(count)
      );
    }
  }

  #[test]
  fn property_chain_decoder_rejects_changed_flow_and_exits() {
    let session = crate::parser::Parser::parse_source(crate::js!(
      r#"
      function sum(object) {
        var total = 0;
        for (var i = 0; i < 100; i = i + 1) total = total + object.a.b;
        return total;
      }
    "#
    ))
    .expect("parse");
    let root = crate::compiler::compile_session(&session).expect("compile");
    let code = &root.functions[0].chunk;
    let region = (0..code.words.len())
      .find_map(|pc| PropertyChainRegion::decode(code, pc))
      .expect("recognized chain");
    for offset in [0, 1, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14] {
      let mut changed = (**code).clone();
      // Change the destination register without changing the opcode or other operands.
      changed.words[region.entry_pc + offset] ^= 1 << 8;
      assert!(
        PropertyChainRegion::decode(&changed, region.entry_pc).is_none(),
        "offset {offset}"
      );
    }
    let mut changed = (**code).clone();
    changed.words[region.completed_pc - 1] ^= 1 << 8;
    assert!(PropertyChainRegion::decode(&changed, region.entry_pc).is_none());
    for step in [0.0, -1.0, f64::NAN, f64::INFINITY] {
      let mut changed = (**code).clone();
      let constant = decode_aux16(changed.words[region.completed_pc - 4]) as usize;
      changed.constants[constant] = crate::bytecode::Constant::Number(step);
      assert!(PropertyChainRegion::decode(&changed, region.entry_pc).is_none());
    }
  }
}
