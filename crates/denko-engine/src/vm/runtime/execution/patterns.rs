// SPDX-License-Identifier: MPL-2.0

use crate::bytecode::{Chunk, Op, decode_compare_jump, decode_rd, decode_risc_aux16, decode_rs};
use crate::vm::state::{QuickenedKind, SiteAction};

pub(super) fn build_quickened_sites(
  chunk: &Chunk,
  native_regions_enabled: bool,
) -> (Vec<SiteAction>, Vec<crate::vm::region::NativeRegion>) {
  let code = &chunk.code;
  fn matches_ops(
    code: &[u32],
    ip: usize,
    ops: &[Op],
  ) -> bool {
    code
      .get(ip..ip.saturating_add(ops.len()))
      .is_some_and(|words| {
        words
          .iter()
          .zip(ops)
          .all(|(word, op)| (*word & 0xff) as u8 == *op as u8)
      })
  }

  fn is_single_argument_method_call(
    code: &[u32],
    ip: usize,
    ops: &[Op],
  ) -> bool {
    let Some(words) = code.get(ip..ip.saturating_add(ops.len())) else {
      return false;
    };
    if !words
      .iter()
      .zip(ops)
      .all(|(word, op)| (*word & 0xff) as u8 == *op as u8)
      || decode_risc_aux16(words[10]) != 1
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
      && decode_risc_aux16(words[2]) == decode_risc_aux16(words[1])
      && decode_risc_aux16(words[3]) == decode_risc_aux16(words[1])
  }

  fn is_numeric_property_add(
    code: &[u32],
    ip: usize,
    ops: &[Op],
  ) -> bool {
    let Some(words) = code.get(ip..ip.saturating_add(ops.len())) else {
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
    let accumulator_slot = decode_risc_aux16(words[0]);
    let temporary_slot = decode_risc_aux16(words[5]);
    decode_rd(words[3]) == property_register
      && decode_rs(words[3]) == key_register
      && decode_rd(words[4]) == accumulator_register
      && decode_rs(words[4]) == property_register
      && decode_rd(words[5]) == accumulator_register
      && decode_rd(words[6]) == property_register
      && decode_rd(words[7]) == property_register
      && decode_rd(words[8]) == property_register
      && decode_risc_aux16(words[6]) == accumulator_slot
      && decode_risc_aux16(words[7]) == temporary_slot
      && decode_risc_aux16(words[8]) == accumulator_slot
  }

  fn is_numeric_compare(
    code: &[u32],
    ip: usize,
    ops: &[Op],
  ) -> bool {
    let Some(words) = code.get(ip..ip.saturating_add(ops.len())) else {
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
    ip: usize,
    ops: &[Op],
  ) -> bool {
    let Some(words) = code.get(ip..ip.saturating_add(ops.len())) else {
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
    let target_slot = decode_risc_aux16(words[0]);
    let temporary_slot = decode_risc_aux16(words[6]);
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
      && decode_risc_aux16(words[7]) == target_slot
      && decode_risc_aux16(words[8]) == temporary_slot
      && decode_risc_aux16(words[9]) == target_slot
  }

  fn is_numeric_add_local(
    code: &[u32],
    ip: usize,
    ops: &[Op],
  ) -> bool {
    let Some(words) = code.get(ip..ip.saturating_add(ops.len())) else {
      return false;
    };
    matches_ops(code, ip, ops)
      && decode_rd(words[2]) == decode_rd(words[0])
      && decode_rs(words[2]) == decode_rd(words[1])
      && decode_rd(words[3]) == decode_rd(words[0])
  }

  const COMPARE: &[Op] = &[Op::LoadLocal, Op::Const, Op::CompareJump];
  const METHOD_CALL: &[Op] = &[
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
  ];
  const PROPERTY_ADD: &[Op] = &[
    Op::LoadLocal,
    Op::LoadLocal,
    Op::Const,
    Op::GetProperty,
    Op::Add,
    Op::StoreLocal,
    Op::LoadLocal,
    Op::LoadLocal,
    Op::StoreLocal,
  ];
  const INCREMENT: &[Op] = &[
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
  ];
  const ADD_LOCAL: &[Op] = &[Op::LoadLocal, Op::LoadLocal, Op::Add, Op::StoreLocal];

  let mut regions = Vec::new();
  let sites = (0..code.len())
    .map(|ip| {
      if native_regions_enabled
        && let Some(region) = crate::vm::region::OwnMethodBinaryLoopRegion::decode(chunk, ip)
        && let Some(id) = crate::vm::region::RegionId::from_index(regions.len())
      {
        regions.push(crate::vm::region::NativeRegion::OwnMethodBinaryLoop(region));
        SiteAction::Region(id)
      } else if native_regions_enabled
        && let Some(region) = crate::vm::region::OwnMethodReadLoopRegion::decode(chunk, ip)
        && let Some(id) = crate::vm::region::RegionId::from_index(regions.len())
      {
        regions.push(crate::vm::region::NativeRegion::OwnMethodReadLoop(region));
        SiteAction::Region(id)
      } else if native_regions_enabled
        && let Some(region) = crate::vm::region::OwnMethodEffectLoopRegion::decode(chunk, ip)
        && let Some(id) = crate::vm::region::RegionId::from_index(regions.len())
      {
        regions.push(crate::vm::region::NativeRegion::OwnMethodEffectLoop(region));
        SiteAction::Region(id)
      } else if native_regions_enabled
        && let Some(region) = crate::vm::region::OwnMethodNumericLoopRegion::decode(chunk, ip)
        && let Some(id) = crate::vm::region::RegionId::from_index(regions.len())
      {
        regions.push(crate::vm::region::NativeRegion::OwnMethodNumericLoop(
          region,
        ));
        SiteAction::Region(id)
      } else if native_regions_enabled
        && let Some(region) = crate::vm::region::DenseMaskedReadRegion::decode(chunk, ip)
        && let Some(id) = crate::vm::region::RegionId::from_index(regions.len())
      {
        regions.push(crate::vm::region::NativeRegion::DenseMaskedRead(region));
        SiteAction::Region(id)
      } else if native_regions_enabled
        && let Some(region) =
          crate::vm::region::NumericLoopRegion::decode_numeric_add_call(chunk, ip)
        && let Some(id) = crate::vm::region::RegionId::from_index(regions.len())
      {
        regions.push(crate::vm::region::NativeRegion::NumericLoop(region));
        SiteAction::Region(id)
      } else if native_regions_enabled
        && let Some(region) = crate::vm::region::NumericLoopRegion::decode_call(chunk, ip)
        && let Some(id) = crate::vm::region::RegionId::from_index(regions.len())
      {
        regions.push(crate::vm::region::NativeRegion::NumericLoop(region));
        SiteAction::Region(id)
      } else if native_regions_enabled
        && let Some(region) = crate::vm::region::NumericLoopRegion::decode_property(chunk, ip)
        && let Some(id) = crate::vm::region::RegionId::from_index(regions.len())
      {
        regions.push(crate::vm::region::NativeRegion::NumericLoop(region));
        SiteAction::Region(id)
      } else if native_regions_enabled
        && let Some(region) = crate::vm::region::NumericLoopRegion::decode_accumulator(chunk, ip)
        && let Some(id) = crate::vm::region::RegionId::from_index(regions.len())
      {
        regions.push(crate::vm::region::NativeRegion::NumericLoop(region));
        SiteAction::Region(id)
      } else if native_regions_enabled
        && let Some(region) = crate::vm::region::DenseArrayPushRegion::decode(chunk, ip)
        && let Some(id) = crate::vm::region::RegionId::from_index(regions.len())
      {
        regions.push(crate::vm::region::NativeRegion::DenseArrayPush(region));
        SiteAction::Region(id)
      } else if is_numeric_compare(code, ip, COMPARE) {
        SiteAction::Legacy(QuickenedKind::NumericCompareBranch)
      } else if is_single_argument_method_call(code, ip, METHOD_CALL) {
        SiteAction::Legacy(QuickenedKind::SingleArgumentMethodCall)
      } else if is_numeric_property_add(code, ip, PROPERTY_ADD) {
        SiteAction::Legacy(QuickenedKind::NumericPropertyAdd)
      } else if is_numeric_increment(code, ip, INCREMENT) {
        SiteAction::Legacy(QuickenedKind::NumericIncrement)
      } else if is_numeric_add_local(code, ip, ADD_LOCAL) {
        SiteAction::Legacy(QuickenedKind::NumericAddLocal)
      } else {
        SiteAction::Interpret
      }
    })
    .collect();
  (sites, regions)
}
