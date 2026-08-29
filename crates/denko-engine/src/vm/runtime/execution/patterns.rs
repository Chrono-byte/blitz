// SPDX-License-Identifier: MPL-2.0

use crate::bytecode::{Chunk, Opcode, decode_aux16, decode_compare_jump, decode_rd, decode_rs};
use crate::vm::state::{QuickenedKind, SiteAction};

pub(super) fn build_quickened_sites(
  code: &Chunk,
  native_regions_enabled: bool,
) -> (Vec<SiteAction>, Vec<crate::vm::region::NativeRegion>) {
  let words = &code.words;
  fn matches_ops(
    code: &[u32],
    pc: usize,
    ops: &[Opcode],
  ) -> bool {
    code
      .get(pc..pc.saturating_add(ops.len()))
      .is_some_and(|words| {
        words
          .iter()
          .zip(ops)
          .all(|(word, op)| (*word & 0xff) as u8 == *op as u8)
      })
  }

  fn is_single_argument_method_call(
    code: &[u32],
    pc: usize,
    ops: &[Opcode],
  ) -> bool {
    let Some(words) = code.get(pc..pc.saturating_add(ops.len())) else {
      return false;
    };
    if !words
      .iter()
      .zip(ops)
      .all(|(word, op)| (*word & 0xff) as u8 == *op as u8)
      || decode_aux16(words[10]) != 1
    {
      return false;
    }

    let receiver_register = decode_rd(words[0]);
    let callee_register = decode_rd(words[3]);
    let argument_register = decode_rd(words[6]);
    let key_register = decode_rd(words[4]);
    decode_rd(words[1]) == receiver_register
      && decode_rd(words[2]) == receiver_register
      && decode_rd(words[5]) == callee_register
      && decode_rs(words[5]) == key_register
      && decode_rd(words[7]) == 2
      && decode_rs(words[7]) == argument_register
      && decode_rd(words[8]) == 1
      && decode_rs(words[8]) == receiver_register
      && decode_rd(words[9]) == 0
      && decode_rs(words[9]) == callee_register
      && decode_rd(words[11]) == receiver_register
      && decode_rs(words[11]) == 0
      && decode_aux16(words[2]) == decode_aux16(words[1])
      && decode_aux16(words[3]) == decode_aux16(words[1])
  }

  fn is_numeric_property_add(
    code: &[u32],
    pc: usize,
    ops: &[Opcode],
  ) -> bool {
    let Some(words) = code.get(pc..pc.saturating_add(ops.len())) else {
      return false;
    };
    if !words
      .iter()
      .zip(ops)
      .all(|(word, op)| (*word & 0xff) as u8 == *op as u8)
    {
      return false;
    }

    let accumulator_register = decode_rd(words[0]);
    let property_register = decode_rd(words[1]);
    let key_register = decode_rd(words[2]);
    let accumulator_slot = decode_aux16(words[0]);
    let temporary_slot = decode_aux16(words[5]);
    decode_rd(words[3]) == property_register
      && decode_rs(words[3]) == key_register
      && decode_rd(words[4]) == accumulator_register
      && decode_rs(words[4]) == property_register
      && decode_rd(words[5]) == accumulator_register
      && decode_rd(words[6]) == property_register
      && decode_rd(words[7]) == property_register
      && decode_rd(words[8]) == property_register
      && decode_aux16(words[6]) == accumulator_slot
      && decode_aux16(words[7]) == temporary_slot
      && decode_aux16(words[8]) == accumulator_slot
  }

  fn is_numeric_compare(
    code: &[u32],
    pc: usize,
    ops: &[Opcode],
  ) -> bool {
    let Some(words) = code.get(pc..pc.saturating_add(ops.len())) else {
      return false;
    };
    if !words
      .iter()
      .zip(ops)
      .all(|(word, op)| (*word & 0xff) as u8 == *op as u8)
    {
      return false;
    }
    let (_, left, right, _) = decode_compare_jump(words[2]);
    left == decode_rd(words[0]) && right == decode_rd(words[1])
  }

  fn is_numeric_increment(
    code: &[u32],
    pc: usize,
    ops: &[Opcode],
  ) -> bool {
    let Some(words) = code.get(pc..pc.saturating_add(ops.len())) else {
      return false;
    };
    if !words
      .iter()
      .zip(ops)
      .all(|(word, op)| (*word & 0xff) as u8 == *op as u8)
    {
      return false;
    }
    let source = decode_rd(words[0]);
    let constant = decode_rd(words[3]);
    let result = decode_rd(words[4]);
    let target_slot = decode_aux16(words[0]);
    let temporary_slot = decode_aux16(words[6]);
    decode_rd(words[1]) == source
      && decode_rs(words[1]) == source
      && decode_rd(words[2]) == source
      && decode_rs(words[2]) == source
      && decode_rs(words[4]) == source
      && decode_rd(words[5]) == result
      && decode_rs(words[5]) == constant
      && decode_rd(words[6]) == result
      && decode_rd(words[7]) == result
      && decode_rd(words[8]) == result
      && decode_rd(words[9]) == result
      && decode_aux16(words[7]) == target_slot
      && decode_aux16(words[8]) == temporary_slot
      && decode_aux16(words[9]) == target_slot
  }

  fn is_numeric_add_local(
    code: &[u32],
    pc: usize,
    ops: &[Opcode],
  ) -> bool {
    let Some(words) = code.get(pc..pc.saturating_add(ops.len())) else {
      return false;
    };
    matches_ops(code, pc, ops)
      && decode_rd(words[2]) == decode_rd(words[0])
      && decode_rs(words[2]) == decode_rd(words[1])
      && decode_rd(words[3]) == decode_rd(words[0])
  }

  const COMPARE: &[Opcode] = &[Opcode::LoadLocal, Opcode::Const, Opcode::CompareJump];
  const METHOD_CALL: &[Opcode] = &[
    Opcode::LoadLocal,
    Opcode::StoreLocal,
    Opcode::LoadLocal,
    Opcode::LoadLocal,
    Opcode::Const,
    Opcode::GetProperty,
    Opcode::LoadLocal,
    Opcode::Move,
    Opcode::Move,
    Opcode::Move,
    Opcode::CallMethod,
    Opcode::Move,
  ];
  const PROPERTY_ADD: &[Opcode] = &[
    Opcode::LoadLocal,
    Opcode::LoadLocal,
    Opcode::Const,
    Opcode::GetProperty,
    Opcode::Add,
    Opcode::StoreLocal,
    Opcode::LoadLocal,
    Opcode::LoadLocal,
    Opcode::StoreLocal,
  ];
  const INCREMENT: &[Opcode] = &[
    Opcode::LoadLocal,
    Opcode::Neg,
    Opcode::Neg,
    Opcode::Const,
    Opcode::Move,
    Opcode::Add,
    Opcode::StoreLocal,
    Opcode::LoadLocal,
    Opcode::LoadLocal,
    Opcode::StoreLocal,
  ];
  const ADD_LOCAL: &[Opcode] = &[
    Opcode::LoadLocal,
    Opcode::LoadLocal,
    Opcode::Add,
    Opcode::StoreLocal,
  ];

  let mut regions = Vec::new();
  let sites = (0..words.len())
    .map(|pc| {
      if native_regions_enabled
        && let Some(region) = crate::vm::region::OwnMethodBinaryLoopRegion::decode(code, pc)
        && let Some(id) = crate::vm::region::RegionId::from_index(regions.len())
      {
        regions.push(crate::vm::region::NativeRegion::OwnMethodBinaryLoop(region));
        SiteAction::Region(id)
      } else if native_regions_enabled
        && let Some(region) = crate::vm::region::OwnMethodReadLoopRegion::decode(code, pc)
        && let Some(id) = crate::vm::region::RegionId::from_index(regions.len())
      {
        regions.push(crate::vm::region::NativeRegion::OwnMethodReadLoop(region));
        SiteAction::Region(id)
      } else if native_regions_enabled
        && let Some(region) = crate::vm::region::OwnMethodEffectLoopRegion::decode(code, pc)
        && let Some(id) = crate::vm::region::RegionId::from_index(regions.len())
      {
        regions.push(crate::vm::region::NativeRegion::OwnMethodEffectLoop(region));
        SiteAction::Region(id)
      } else if native_regions_enabled
        && let Some(region) = crate::vm::region::OwnMethodNumericLoopRegion::decode(code, pc)
        && let Some(id) = crate::vm::region::RegionId::from_index(regions.len())
      {
        regions.push(crate::vm::region::NativeRegion::OwnMethodNumericLoop(
          region,
        ));
        SiteAction::Region(id)
      } else if native_regions_enabled
        && let Some(region) = crate::vm::region::DenseMaskedReadRegion::decode(code, pc)
        && let Some(id) = crate::vm::region::RegionId::from_index(regions.len())
      {
        regions.push(crate::vm::region::NativeRegion::DenseMaskedRead(region));
        SiteAction::Region(id)
      } else if native_regions_enabled
        && let Some(region) =
          crate::vm::region::NumericLoopRegion::decode_numeric_add_call(code, pc)
        && let Some(id) = crate::vm::region::RegionId::from_index(regions.len())
      {
        regions.push(crate::vm::region::NativeRegion::NumericLoop(region));
        SiteAction::Region(id)
      } else if native_regions_enabled
        && let Some(region) = crate::vm::region::NumericLoopRegion::decode_call(code, pc)
        && let Some(id) = crate::vm::region::RegionId::from_index(regions.len())
      {
        regions.push(crate::vm::region::NativeRegion::NumericLoop(region));
        SiteAction::Region(id)
      } else if native_regions_enabled
        && let Some(region) = crate::vm::region::NumericLoopRegion::decode_property(code, pc)
        && let Some(id) = crate::vm::region::RegionId::from_index(regions.len())
      {
        regions.push(crate::vm::region::NativeRegion::NumericLoop(region));
        SiteAction::Region(id)
      } else if native_regions_enabled
        && let Some(region) = crate::vm::region::NumericLoopRegion::decode_accumulator(code, pc)
        && let Some(id) = crate::vm::region::RegionId::from_index(regions.len())
      {
        regions.push(crate::vm::region::NativeRegion::NumericLoop(region));
        SiteAction::Region(id)
      } else if native_regions_enabled
        && let Some(region) = crate::vm::region::DenseArrayPushRegion::decode(code, pc)
        && let Some(id) = crate::vm::region::RegionId::from_index(regions.len())
      {
        regions.push(crate::vm::region::NativeRegion::DenseArrayPush(region));
        SiteAction::Region(id)
      } else if is_numeric_compare(words, pc, COMPARE) {
        SiteAction::Legacy(QuickenedKind::NumericCompareBranch)
      } else if code.root_slot_names.is_empty()
        && is_single_argument_method_call(words, pc, METHOD_CALL)
      {
        SiteAction::Legacy(QuickenedKind::SingleArgumentMethodCall)
      } else if is_numeric_property_add(words, pc, PROPERTY_ADD) {
        SiteAction::Legacy(QuickenedKind::NumericPropertyAdd)
      } else if is_numeric_increment(words, pc, INCREMENT) {
        SiteAction::Legacy(QuickenedKind::NumericIncrement)
      } else if is_numeric_add_local(words, pc, ADD_LOCAL) {
        SiteAction::Legacy(QuickenedKind::NumericAddLocal)
      } else {
        SiteAction::Interpret
      }
    })
    .collect();
  (sites, regions)
}
