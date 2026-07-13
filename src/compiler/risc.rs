//! Stack-v1 analysis and lowering support for the RISC v2 backend.

use crate::bytecode::{
  BytecodeFormat, Chunk, Op, encode_compare_jump, encode_rd, encode_risc_aux16,
  encode_risc_cond_jump, encode_risc_registers,
};
use crate::errors::CompilerError;
use std::collections::VecDeque;

/// Verified operand-stack heights used to assign RISC value locations.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StackAnalysis {
  /// Stack height immediately before each instruction. Unreachable code is `None`.
  pub(crate) entry_heights: Vec<Option<usize>>,
  /// Largest live stack height in the chunk.
  pub(crate) max_height: usize,
}

/// Physical home assigned to one stable stack position.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ValueLocation {
  Register(u8),
  Spill(u16),
}

/// Register/spill assignment for stack positions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LocationPlan {
  pub(crate) locations: Vec<ValueLocation>,
  pub(crate) value_spill_count: u16,
}

impl LocationPlan {
  pub(crate) fn for_analysis(analysis: &StackAnalysis) -> Result<Self, CompilerError> {
    let mut locations = Vec::with_capacity(analysis.max_height);
    for position in 0..analysis.max_height {
      if position < 8 {
        locations.push(ValueLocation::Register(8 + position as u8));
      } else {
        let spill = u16::try_from(position - 8)
          .map_err(|_| CompilerError::from("RISC value spill count exceeds u16"))?;
        locations.push(ValueLocation::Spill(spill));
      }
    }
    Ok(Self {
      value_spill_count: u16::try_from(analysis.max_height.saturating_sub(8))
        .map_err(|_| CompilerError::from("RISC value spill count exceeds u16"))?,
      locations,
    })
  }

  fn get(
    &self,
    position: usize,
  ) -> Result<ValueLocation, CompilerError> {
    self
      .locations
      .get(position)
      .copied()
      .ok_or_else(|| CompilerError::from(format!("missing RISC location for stack {position}")))
  }
}

#[derive(Clone, Copy)]
enum BranchKind {
  Jump,
  Conditional(Op, u8),
  Compare(u8, u8, u8),
  Handler,
}

struct BranchPatch {
  risc_ip: usize,
  old_target: usize,
  kind: BranchKind,
}

/// Lower a verified stack-v1 chunk into register-addressed RISC v2 bytecode.
pub(crate) fn lower_chunk(chunk: &Chunk) -> Result<Chunk, CompilerError> {
  if chunk.format != BytecodeFormat::StackV1 {
    return Err(CompilerError::from("RISC lowering requires StackV1 input"));
  }
  for word in &chunk.code {
    let op = Op::from_byte((word & 0xFF) as u8)
      .ok_or_else(|| CompilerError::from("invalid opcode in StackV1 input"))?;
    if !risc_executor_supports(op) {
      return Err(CompilerError::from(format!(
        "RISC v2 compatibility fallback required for {}",
        op.name()
      )));
    }
  }
  let analysis = analyze_stack(chunk)?;
  let plan = LocationPlan::for_analysis(&analysis)?;
  let window_size = chunk
    .code
    .iter()
    .filter_map(|word| Op::from_byte((word & 0xFF) as u8).map(|op| (op, word >> 8)))
    .map(|(op, payload)| risc_window_size(op, payload))
    .max()
    .unwrap_or(0);
  let mut lowered = chunk.clone();
  lowered.format = BytecodeFormat::RiscV2;
  lowered.code.clear();
  lowered.lines.clear();
  lowered.risc_spill_count = plan
    .value_spill_count
    .checked_add(
      u16::try_from(window_size)
        .map_err(|_| CompilerError::from("RISC operand window exceeds u16"))?,
    )
    .ok_or_else(|| CompilerError::from("RISC spill count exceeds u16"))?;
  lowered.risc_value_spill_count = plan.value_spill_count;

  let mut old_to_new = vec![None; chunk.code.len()];
  let mut patches = Vec::new();

  for old_ip in 0..chunk.code.len() {
    let Some(height) = analysis.entry_heights[old_ip] else {
      continue;
    };
    old_to_new[old_ip] = Some(lowered.code.len());
    let instruction = chunk
      .decode_at(old_ip)
      .map_err(|error| CompilerError::from(format!("invalid stack IR: {error}")))?;
    let line = chunk.lines[old_ip];
    let arg = instruction.operand_u8(0).unwrap_or_default();

    match instruction.op {
      Op::Const | Op::GetGlobal | Op::TypeofGlobal => {
        let destination = plan.get(height)?;
        let rd = destination_register(destination, 0);
        emit(
          &mut lowered,
          encode_risc_aux16(instruction.op, rd, instruction.operand_u16()),
          line,
        );
        store_location(&mut lowered, destination, rd, line);
      }
      Op::Null | Op::True | Op::False | Op::Undefined | Op::GetThis | Op::GetNewTarget => {
        let destination = plan.get(height)?;
        let rd = destination_register(destination, 0);
        emit(&mut lowered, instruction.op as u32 | encode_rd(rd), line);
        store_location(&mut lowered, destination, rd, line);
      }
      Op::LoadLocal | Op::GetUpvalue | Op::MakeClosure | Op::GetBuiltin => {
        let destination = plan.get(height)?;
        let rd = destination_register(destination, 0);
        emit(
          &mut lowered,
          encode_risc_aux16(instruction.op, rd, arg as u16),
          line,
        );
        store_location(&mut lowered, destination, rd, line);
      }
      Op::GetModuleExport => {
        let destination = plan.get(height)?;
        let rd = destination_register(destination, 0);
        let word = instruction.op as u32
          | encode_rd(rd)
          | ((instruction.operand_u8(0).unwrap_or_default() as u32) << 16)
          | ((instruction.operand_u8(1).unwrap_or_default() as u32) << 24);
        emit(&mut lowered, word, line);
        store_location(&mut lowered, destination, rd, line);
      }
      Op::StoreLocal | Op::SetGlobal | Op::SetUpvalue => {
        let source = load_location(&mut lowered, plan.get(height - 1)?, 0, line);
        emit(
          &mut lowered,
          encode_risc_aux16(
            instruction.op,
            source,
            if matches!(instruction.op, Op::SetGlobal) {
              instruction.operand_u16()
            } else {
              arg as u16
            },
          ),
          line,
        );
      }
      Op::SetModuleExport => {
        let source = load_location(&mut lowered, plan.get(height - 1)?, 0, line);
        let word = instruction.op as u32
          | encode_rd(source)
          | ((instruction.operand_u8(0).unwrap_or_default() as u32) << 16)
          | ((instruction.operand_u8(1).unwrap_or_default() as u32) << 24);
        emit(&mut lowered, word, line);
      }
      Op::Pop | Op::PopN => {}
      Op::Dup => {
        let source = load_location(&mut lowered, plan.get(height - 1)?, 0, line);
        store_location(&mut lowered, plan.get(height)?, source, line);
      }
      Op::Swap => {
        let lower = load_location(&mut lowered, plan.get(height - 2)?, 0, line);
        if lower != 0 {
          emit(
            &mut lowered,
            encode_risc_registers(Op::Move, 0, lower),
            line,
          );
        }
        let upper = load_location(&mut lowered, plan.get(height - 1)?, 1, line);
        store_location(&mut lowered, plan.get(height - 2)?, upper, line);
        store_location(&mut lowered, plan.get(height - 1)?, 0, line);
      }
      Op::Neg | Op::Not | Op::BitNot | Op::Typeof => {
        let destination = plan.get(height - 1)?;
        let source = load_location(&mut lowered, destination, 0, line);
        let rd = destination_register(destination, source);
        emit(
          &mut lowered,
          encode_risc_registers(instruction.op, rd, source),
          line,
        );
        store_location(&mut lowered, destination, rd, line);
      }
      Op::Add
      | Op::Sub
      | Op::Mul
      | Op::Div
      | Op::Mod
      | Op::Pow
      | Op::BitAnd
      | Op::BitOr
      | Op::BitXor
      | Op::Shl
      | Op::Shr
      | Op::Ushr
      | Op::And
      | Op::Or
      | Op::GetProperty
      | Op::DeleteProperty
      | Op::HasProperty
      | Op::InstanceOf
      | Op::GetSuperProperty => {
        let destination = plan.get(height - 2)?;
        let left = load_location(&mut lowered, destination, 0, line);
        let right = load_location(&mut lowered, plan.get(height - 1)?, 1, line);
        let rd = destination_register(destination, left);
        if rd != left {
          emit(
            &mut lowered,
            encode_risc_registers(Op::Move, rd, left),
            line,
          );
        }
        emit(
          &mut lowered,
          encode_risc_registers(instruction.op, rd, right),
          line,
        );
        store_location(&mut lowered, destination, rd, line);
      }
      Op::SetProperty | Op::SetSuperProperty => {
        for (position, register) in [(height - 3, 0), (height - 2, 1), (height - 1, 2)] {
          let source = load_location(&mut lowered, plan.get(position)?, register, line);
          if source != register {
            emit(
              &mut lowered,
              encode_risc_registers(Op::Move, register, source),
              line,
            );
          }
        }
        emit(
          &mut lowered,
          encode_risc_registers(instruction.op, 0, 1) | (2_u32 << 16),
          line,
        );
        store_location(&mut lowered, plan.get(height - 3)?, 0, line);
      }
      Op::Call | Op::TailCall | Op::Construct => {
        let count = arg as usize;
        let base = height - count - 1;
        stage_window(
          &mut lowered,
          &plan,
          base + 1 + count.min(6),
          count.saturating_sub(6),
          line,
        )?;
        move_location_to_register(&mut lowered, plan.get(base)?, 0, line);
        for index in 0..count.min(6) {
          move_location_to_register(
            &mut lowered,
            plan.get(base + 1 + index)?,
            2 + index as u8,
            line,
          );
        }
        emit(
          &mut lowered,
          encode_risc_aux16(instruction.op, 0, count as u16),
          line,
        );
        store_location(&mut lowered, plan.get(base)?, 0, line);
      }
      Op::CallBuiltin => {
        let count = arg as usize;
        let base = height - count - 2;
        stage_window(
          &mut lowered,
          &plan,
          base + 2 + count.min(6),
          count.saturating_sub(6),
          line,
        )?;
        move_location_to_register(&mut lowered, plan.get(base)?, 1, line);
        move_location_to_register(&mut lowered, plan.get(base + 1)?, 0, line);
        for index in 0..count.min(6) {
          move_location_to_register(
            &mut lowered,
            plan.get(base + 2 + index)?,
            2 + index as u8,
            line,
          );
        }
        emit(
          &mut lowered,
          encode_risc_aux16(instruction.op, 0, count as u16),
          line,
        );
        store_location(&mut lowered, plan.get(base)?, 0, line);
      }
      Op::BuildArray => {
        let count = arg as usize;
        let base = height - count;
        stage_window(&mut lowered, &plan, base, count, line)?;
        let destination = plan.get(base)?;
        let rd = destination_register(destination, 0);
        emit(
          &mut lowered,
          encode_risc_aux16(instruction.op, rd, count as u16),
          line,
        );
        store_location(&mut lowered, destination, rd, line);
      }
      Op::BuildObject => {
        let count = arg as usize;
        let operand_count = count.saturating_mul(2);
        let base = height - operand_count;
        stage_window(&mut lowered, &plan, base, operand_count, line)?;
        let destination = plan.get(base)?;
        let rd = destination_register(destination, 0);
        emit(
          &mut lowered,
          encode_risc_aux16(instruction.op, rd, count as u16),
          line,
        );
        store_location(&mut lowered, destination, rd, line);
      }
      Op::DeleteIdentifier | Op::IteratorNext | Op::IteratorClose => {
        let destination = plan.get(height - 1)?;
        let source = load_location(&mut lowered, destination, 0, line);
        let rd = destination_register(destination, source);
        emit(
          &mut lowered,
          encode_risc_registers(instruction.op, rd, source),
          line,
        );
        store_location(&mut lowered, destination, rd, line);
      }
      Op::CloneLocalCell | Op::ResetLocalCell => {
        emit(
          &mut lowered,
          encode_risc_aux16(instruction.op, 0, arg as u16),
          line,
        );
      }
      Op::Jump => add_branch_patch(
        chunk,
        old_ip,
        &mut lowered,
        line,
        BranchKind::Jump,
        &mut patches,
      )?,
      Op::JumpIfFalse | Op::JumpIfTrue | Op::JumpIfNullish => {
        let source = load_location(&mut lowered, plan.get(height - 1)?, 0, line);
        add_branch_patch(
          chunk,
          old_ip,
          &mut lowered,
          line,
          BranchKind::Conditional(instruction.op, source),
          &mut patches,
        )?;
      }
      Op::CompareJump => {
        let left = load_location(&mut lowered, plan.get(height - 2)?, 0, line);
        let right = load_location(&mut lowered, plan.get(height - 1)?, 1, line);
        add_branch_patch(
          chunk,
          old_ip,
          &mut lowered,
          line,
          BranchKind::Compare(arg, left, right),
          &mut patches,
        )?;
      }
      Op::PushHandler => add_branch_patch(
        chunk,
        old_ip,
        &mut lowered,
        line,
        BranchKind::Handler,
        &mut patches,
      )?,
      Op::Return => {
        let source = if height == 0 {
          emit(&mut lowered, Op::Undefined as u32 | encode_rd(0), line);
          0
        } else {
          load_location(&mut lowered, plan.get(height - 1)?, 0, line)
        };
        emit(
          &mut lowered,
          instruction.op as u32 | encode_rd(source),
          line,
        );
      }
      Op::Throw | Op::PushWithEnv => {
        let source = load_location(&mut lowered, plan.get(height - 1)?, 0, line);
        emit(
          &mut lowered,
          instruction.op as u32 | encode_rd(source),
          line,
        );
      }
      Op::PopEnv | Op::PopHandler | Op::GeneratorInitDone => {
        emit(&mut lowered, instruction.op as u32, line);
      }
      other => {
        return Err(CompilerError::from(format!(
          "RISC lowering for {} is not implemented",
          other.name()
        )));
      }
    }
  }

  for patch in patches {
    let target = old_to_new
      .get(patch.old_target)
      .and_then(|target| *target)
      .ok_or_else(|| CompilerError::from("branch targets unreachable stack IR"))?;
    let offset = target as i64 - (patch.risc_ip as i64 + 1);
    let word =
      match patch.kind {
        BranchKind::Jump | BranchKind::Handler => {
          let op = if matches!(patch.kind, BranchKind::Handler) {
            Op::PushHandler
          } else {
            Op::Jump
          };
          if !(-(1 << 23)..(1 << 23)).contains(&offset) {
            return Err(CompilerError::from("RISC long branch exceeds signed i24"));
          }
          op as u32 | (((offset as u32) & 0x00FF_FFFF) << 8)
        }
        BranchKind::Conditional(op, register) => encode_risc_cond_jump(op, register, offset as i32)
          .ok_or_else(|| CompilerError::from("RISC conditional branch exceeds signed i20"))?,
        BranchKind::Compare(condition, left, right) => {
          encode_compare_jump(condition, left, right, offset as i16)
            .ok_or_else(|| CompilerError::from("RISC compare branch exceeds signed i13"))?
        }
      };
    lowered.code[patch.risc_ip] = word;
  }

  Ok(lowered)
}

fn risc_executor_supports(op: Op) -> bool {
  matches!(
    op,
    Op::Const
      | Op::Null
      | Op::True
      | Op::False
      | Op::Undefined
      | Op::LoadLocal
      | Op::StoreLocal
      | Op::Add
      | Op::Sub
      | Op::Mul
      | Op::Div
      | Op::Mod
      | Op::Pow
      | Op::Neg
      | Op::BitNot
      | Op::BitAnd
      | Op::BitOr
      | Op::BitXor
      | Op::Shl
      | Op::Shr
      | Op::Ushr
      | Op::CompareJump
      | Op::Jump
      | Op::JumpIfTrue
      | Op::JumpIfFalse
      | Op::JumpIfNullish
      | Op::And
      | Op::Or
      | Op::Not
      | Op::Typeof
      | Op::GetProperty
      | Op::SetProperty
      | Op::GetGlobal
      | Op::TypeofGlobal
      | Op::SetGlobal
      | Op::Call
      | Op::TailCall
      | Op::Construct
      | Op::CallBuiltin
      | Op::MakeClosure
      | Op::GetThis
      | Op::GetNewTarget
      | Op::BuildArray
      | Op::BuildObject
      | Op::Pop
      | Op::PopN
      | Op::Dup
      | Op::Swap
      | Op::Return
      | Op::CloneLocalCell
      | Op::ResetLocalCell
  )
}

fn emit(
  chunk: &mut Chunk,
  word: u32,
  line: u32,
) {
  chunk.write_instr(word, line);
}

fn destination_register(
  location: ValueLocation,
  spill_scratch: u8,
) -> u8 {
  match location {
    ValueLocation::Register(register) => register,
    ValueLocation::Spill(_) => spill_scratch,
  }
}

fn load_location(
  chunk: &mut Chunk,
  location: ValueLocation,
  scratch: u8,
  line: u32,
) -> u8 {
  match location {
    ValueLocation::Register(register) => register,
    ValueLocation::Spill(spill) => {
      emit(
        chunk,
        encode_risc_aux16(Op::LoadSpill, scratch, spill),
        line,
      );
      scratch
    }
  }
}

fn store_location(
  chunk: &mut Chunk,
  location: ValueLocation,
  source: u8,
  line: u32,
) {
  match location {
    ValueLocation::Register(register) if register != source => {
      emit(
        chunk,
        encode_risc_registers(Op::Move, register, source),
        line,
      );
    }
    ValueLocation::Register(_) => {}
    ValueLocation::Spill(spill) => {
      emit(
        chunk,
        encode_risc_aux16(Op::StoreSpill, source, spill),
        line,
      );
    }
  }
}

fn move_location_to_register(
  chunk: &mut Chunk,
  location: ValueLocation,
  register: u8,
  line: u32,
) {
  let source = load_location(chunk, location, register, line);
  if source != register {
    emit(
      chunk,
      encode_risc_registers(Op::Move, register, source),
      line,
    );
  }
}

fn stage_window(
  chunk: &mut Chunk,
  plan: &LocationPlan,
  first: usize,
  count: usize,
  line: u32,
) -> Result<(), CompilerError> {
  for index in 0..count {
    let source = load_location(chunk, plan.get(first + index)?, 0, line);
    let spill = plan
      .value_spill_count
      .checked_add(
        u16::try_from(index).map_err(|_| CompilerError::from("RISC operand window exceeds u16"))?,
      )
      .ok_or_else(|| CompilerError::from("RISC operand spill index exceeds u16"))?;
    emit(
      chunk,
      encode_risc_aux16(Op::StoreSpill, source, spill),
      line,
    );
  }
  Ok(())
}

fn risc_window_size(
  op: Op,
  payload: u32,
) -> usize {
  let count = (payload & 0xFF) as usize;
  match op {
    Op::Call | Op::TailCall | Op::Construct | Op::CallBuiltin => count.saturating_sub(6),
    Op::BuildArray => count,
    Op::BuildObject => count.saturating_mul(2),
    _ => 0,
  }
}

fn add_branch_patch(
  chunk: &Chunk,
  old_ip: usize,
  lowered: &mut Chunk,
  line: u32,
  kind: BranchKind,
  patches: &mut Vec<BranchPatch>,
) -> Result<(), CompilerError> {
  let instruction = chunk
    .decode_at(old_ip)
    .map_err(|error| CompilerError::from(format!("invalid branch: {error}")))?;
  let offset = instruction
    .branch_offset()
    .ok_or_else(|| CompilerError::from(format!("invalid branch at stack instruction {old_ip}")))?;
  let target = usize::try_from(old_ip as i64 + 1 + offset as i64)
    .map_err(|_| CompilerError::from("negative stack branch target"))?;
  let risc_ip = lowered.code.len();
  emit(lowered, 0, line);
  patches.push(BranchPatch {
    risc_ip,
    old_target: target,
    kind,
  });
  Ok(())
}

#[derive(Clone, Copy, Debug)]
struct StackEffect {
  pops: usize,
  pushes: usize,
}

impl StackEffect {
  const fn new(
    pops: usize,
    pushes: usize,
  ) -> Self {
    Self { pops, pushes }
  }
}

/// Verify exact stack heights across the control-flow graph.
pub(crate) fn analyze_stack(chunk: &Chunk) -> Result<StackAnalysis, CompilerError> {
  if chunk.code.is_empty() {
    return Ok(StackAnalysis {
      entry_heights: Vec::new(),
      max_height: 0,
    });
  }

  chunk
    .verify()
    .map_err(|error| CompilerError::from(format!("invalid stack IR: {error}")))?;
  let mut entry_heights = vec![None; chunk.code.len()];
  let mut work = VecDeque::from([(0_usize, 0_usize)]);
  let mut max_height = 0;

  while let Some((ip, height)) = work.pop_front() {
    if let Some(existing) = entry_heights[ip] {
      if existing != height {
        return Err(CompilerError::from(format!(
          "conflicting stack heights at instruction {ip}: {existing} and {height}"
        )));
      }
      continue;
    }
    entry_heights[ip] = Some(height);

    let instruction = chunk
      .decode_at(ip)
      .map_err(|error| CompilerError::from(format!("invalid stack IR: {error}")))?;
    let effect = stack_effect(instruction.op, instruction.payload)?;
    if height < effect.pops {
      return Err(CompilerError::from(format!(
        "stack underflow at instruction {ip} ({}) requires {}, has {}",
        instruction.op.name(),
        effect.pops,
        height
      )));
    }
    let output_height = height - effect.pops + effect.pushes;
    max_height = max_height.max(height).max(output_height);

    let fallthrough = ip + 1;
    let branch_target = instruction
      .branch_offset()
      .and_then(|offset| usize::try_from(ip as i64 + 1 + offset as i64).ok());

    match instruction.op {
      Op::Return | Op::Throw => {}
      Op::Jump => enqueue(&mut work, branch_target, output_height),
      Op::JumpIfFalse | Op::JumpIfTrue | Op::JumpIfNullish | Op::CompareJump => {
        enqueue(&mut work, branch_target, output_height);
        enqueue(
          &mut work,
          (fallthrough < chunk.code.len()).then_some(fallthrough),
          output_height,
        );
      }
      Op::PushHandler => {
        // An exception unwinder restores the pre-try height and pushes the thrown value.
        enqueue(&mut work, branch_target, height + 1);
        enqueue(
          &mut work,
          (fallthrough < chunk.code.len()).then_some(fallthrough),
          output_height,
        );
      }
      _ => enqueue(
        &mut work,
        (fallthrough < chunk.code.len()).then_some(fallthrough),
        output_height,
      ),
    }
  }

  Ok(StackAnalysis {
    entry_heights,
    max_height,
  })
}

fn enqueue(
  work: &mut VecDeque<(usize, usize)>,
  target: Option<usize>,
  height: usize,
) {
  if let Some(target) = target {
    work.push_back((target, height));
  }
}

fn stack_effect(
  op: Op,
  payload: u32,
) -> Result<StackEffect, CompilerError> {
  let arg = (payload & 0xFF) as usize;
  let effect = match op {
    Op::Const
    | Op::Null
    | Op::True
    | Op::False
    | Op::Undefined
    | Op::LoadLocal
    | Op::GetGlobal
    | Op::TypeofGlobal
    | Op::GetUpvalue
    | Op::GetThis
    | Op::GetNewTarget
    | Op::GetModuleExport
    | Op::MakeClosure
    | Op::GetBuiltin => StackEffect::new(0, 1),

    Op::StoreLocal
    | Op::SetGlobal
    | Op::SetUpvalue
    | Op::Pop
    | Op::PushWithEnv
    | Op::SetModuleExport
    | Op::Throw => StackEffect::new(1, 0),

    Op::Neg | Op::Not | Op::BitNot | Op::Typeof => StackEffect::new(1, 1),
    Op::Add
    | Op::Sub
    | Op::Mul
    | Op::Div
    | Op::Mod
    | Op::Pow
    | Op::BitAnd
    | Op::BitOr
    | Op::BitXor
    | Op::Shl
    | Op::Shr
    | Op::Ushr
    | Op::And
    | Op::Or
    | Op::GetProperty
    | Op::DeleteProperty
    | Op::HasProperty
    | Op::InstanceOf
    | Op::GetSuperProperty => StackEffect::new(2, 1),

    Op::SetProperty | Op::SetSuperProperty => StackEffect::new(3, 1),
    Op::DeleteIdentifier | Op::IteratorNext | Op::IteratorClose => StackEffect::new(1, 1),
    Op::Dup => StackEffect::new(1, 2),
    Op::Swap => StackEffect::new(2, 2),
    Op::PopN => StackEffect::new(arg, 0),
    Op::BuildArray => StackEffect::new(arg, 1),
    Op::BuildObject => StackEffect::new(arg.saturating_mul(2), 1),
    Op::Call | Op::TailCall | Op::Construct => StackEffect::new(arg + 1, 1),
    Op::CallBuiltin => StackEffect::new(arg + 2, 1),
    Op::SuperConstruct => StackEffect::new(arg, 1),
    Op::Spread => StackEffect::new(3, 1),
    Op::ArrayFromSpread => StackEffect::new(2, 1),
    Op::JumpIfFalse | Op::JumpIfTrue | Op::JumpIfNullish => StackEffect::new(1, 0),
    Op::CompareJump => StackEffect::new(2, 0),
    // Root/function epilogues may return with an empty stack, meaning undefined.
    Op::Return => StackEffect::new(0, 0),
    // Generator resume supplies the expression result before the successor executes.
    Op::Yield | Op::YieldStar | Op::AwaitPromise => StackEffect::new(1, 1),
    Op::CloneLocalCell
    | Op::ResetLocalCell
    | Op::Move
    | Op::Jump
    | Op::PushHandler
    | Op::PopHandler
    | Op::PopEnv
    | Op::GeneratorInitDone => StackEffect::new(0, 0),
    Op::LoadSpill | Op::StoreSpill => {
      return Err(CompilerError::from(
        "RISC spill opcode encountered in stack-v1 IR",
      ));
    }
  };
  Ok(effect)
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::compiler;
  use crate::parser::Parser;

  fn analyze(source: &str) -> StackAnalysis {
    let session = Parser::parse_source(source).expect("parse");
    let chunk = compiler::compile(session.program(), session.source()).expect("compile");
    analyze_stack(&chunk).expect("valid stack graph")
  }

  #[test]
  fn analyzes_branches_and_loops_with_exact_merge_heights() {
    let analysis = analyze("let x = 0; while (x < 3) { x = x + 1; } x;");
    assert!(analysis.max_height >= 2);
    assert!(analysis.entry_heights.iter().any(Option::is_some));
  }

  #[test]
  fn analyzes_calls_properties_and_spread_as_fixed_value_groups() {
    let analysis = analyze("let a = [1, 2]; Math.max(...a) + ({x: 1}).x;");
    assert!(analysis.max_height >= 3);
  }

  #[test]
  fn lowers_core_control_flow_to_register_operands() {
    let source = "let x = 1; while (x < 3) x = x + 1; x;";
    let session = Parser::parse_source(source).expect("parse");
    let chunk = compiler::compile_risc(session.program(), session.source()).expect("lower");
    assert_eq!(chunk.format, BytecodeFormat::RiscV2);
    assert!(
      chunk
        .code
        .iter()
        .any(|word| { matches!(Op::from_byte((*word & 0xFF) as u8), Some(Op::CompareJump)) })
    );
    chunk.verify().expect("verified RISC chunk");
  }

  #[test]
  fn default_frontend_selects_risc_and_preserves_stack_compatibility() {
    let core = Parser::parse_source("var x = 1 + 2;").expect("parse core");
    let core_chunk = compiler::compile_oxc(core.program(), core.source()).expect("compile core");
    assert_eq!(core_chunk.format, BytecodeFormat::RiscV2);

    let exceptional =
      Parser::parse_source("try { throw 1; } catch (error) { error; }").expect("parse exceptional");
    let exceptional_chunk = compiler::compile_oxc(exceptional.program(), exceptional.source())
      .expect("compile exceptional");
    assert_eq!(exceptional_chunk.format, BytecodeFormat::StackV1);
  }
}
