// SPDX-License-Identifier: MPL-2.0

use super::emit::LoweringOpcode;
use crate::bytecode::{
  BytecodeVersion, Chunk, FinallyHandler, Opcode, Register, abi,
  encode_aux16_instruction as encode_bytecode_aux16, encode_compare_jump,
  encode_conditional_branch as encode_bytecode_cond_jump, encode_rd,
  encode_register_instruction as encode_bytecode_registers,
};
use crate::errors::CompilerError;
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct VirtualValue(pub(crate) usize);

impl VirtualValue {
  #[allow(dead_code)]
  pub(crate) fn from_index(index: usize) -> Self {
    Self(index)
  }
}

pub(crate) struct VirtualInstruction {
  pub(crate) opcode: LoweringOpcode,
  pub(crate) payload: u32,
  pub(crate) branch_offset: Option<i32>,
  pub(crate) inputs: VirtualInputs,
  pub(crate) output: Option<VirtualValue>,
}

#[derive(Clone)]
pub(crate) enum VirtualInputs {
  None,
  One(VirtualValue),
  Two([VirtualValue; 2]),
  Many(Box<[VirtualValue]>),
}

impl VirtualInputs {
  pub(crate) fn from_slice(values: &[VirtualValue]) -> Self {
    match values {
      [] => Self::None,
      [value] => Self::One(*value),
      [left, right] => Self::Two([*left, *right]),
      _ => Self::Many(values.into()),
    }
  }

  pub(crate) fn as_slice(&self) -> &[VirtualValue] {
    match self {
      Self::None => &[],
      Self::One(value) => std::slice::from_ref(value),
      Self::Two(values) => values,
      Self::Many(values) => values,
    }
  }

  fn is_empty(&self) -> bool {
    matches!(self, Self::None)
  }
}

impl VirtualInstruction {
  fn operand_u8(
    &self,
    index: usize,
  ) -> Option<u8> {
    (index < 3).then(|| ((self.payload >> (index * 8)) & 0xff) as u8)
  }

  fn operand_u16(&self) -> u16 {
    (self.payload & 0xffff) as u16
  }

  fn branch_offset(&self) -> Option<i32> {
    if let Some(offset) = self.branch_offset {
      return Some(offset);
    }
    match self.opcode {
      LoweringOpcode::CompareJump => Some(i16::from_le_bytes([
        self.operand_u8(1).unwrap_or_default(),
        self.operand_u8(2).unwrap_or_default(),
      ]) as i32),
      LoweringOpcode::Jump
      | LoweringOpcode::JumpIfFalse
      | LoweringOpcode::JumpIfTrue
      | LoweringOpcode::JumpIfNullish
      | LoweringOpcode::PushHandler => {
        let payload = self.payload & 0x00ff_ffff;
        Some(if payload & 0x0080_0000 != 0 {
          (payload | 0xff00_0000) as i32
        } else {
          payload as i32
        })
      }
      _ => None,
    }
  }

  fn input(
    &self,
    index: usize,
  ) -> Result<VirtualValue, CompilerError> {
    self
      .inputs
      .as_slice()
      .get(index)
      .copied()
      .ok_or_else(|| {
        CompilerError::from(format!("missing input {index} for {}", self.opcode.name()))
      })
  }

  fn output(&self) -> Result<VirtualValue, CompilerError> {
    self
      .output
      .ok_or_else(|| CompilerError::from(format!("missing output for {}", self.opcode.name())))
  }
}

fn pending_instruction(
  builder: &Lowering,
  pc: usize,
) -> Result<&VirtualInstruction, CompilerError> {
  builder
    .instructions
    .get(pc)
    .ok_or_else(|| CompilerError::from("virtual instruction out of bounds"))
}

fn bytecode_op(op: LoweringOpcode) -> Opcode {
  op.bytecode()
    .expect("virtual value operation cannot be encoded")
}

fn op_word(op: LoweringOpcode) -> u32 {
  bytecode_op(op) as u32
}

fn encode_aux16_instruction(
  op: LoweringOpcode,
  rd: u8,
  aux: u16,
) -> u32 {
  encode_bytecode_aux16(bytecode_op(op), rd, aux)
}

fn encode_register_instruction(
  op: LoweringOpcode,
  rd: u8,
  rs: u8,
) -> u32 {
  encode_bytecode_registers(bytecode_op(op), rd, rs)
}

fn encode_conditional_branch(
  op: LoweringOpcode,
  rd: u8,
  offset: i32,
) -> Option<u32> {
  encode_bytecode_cond_jump(bytecode_op(op), rd, offset)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AllocatedValue {
  Register(u8),
  SpillSlot(u16),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AllocationPlan {
  pub(crate) locations: Vec<AllocatedValue>,
  pub(crate) value_spill_count: u16,
}

impl AllocationPlan {
  pub(crate) fn for_value_count(value_count: usize) -> Result<Self, CompilerError> {
    let mut locations = Vec::with_capacity(value_count);
    for position in 0..value_count {
      if position < Register::COUNT - abi::FIRST_TEMPORARY.index() {
        locations.push(AllocatedValue::Register(
          abi::FIRST_TEMPORARY.encoding() + position as u8,
        ));
      } else {
        let spill = u16::try_from(position - abi::FIRST_TEMPORARY.index())
          .map_err(|_| CompilerError::from("value spill count exceeds u16"))?;
        locations.push(AllocatedValue::SpillSlot(spill));
      }
    }
    Ok(Self {
      value_spill_count: u16::try_from(value_count.saturating_sub(abi::FIRST_TEMPORARY.index()))
        .map_err(|_| CompilerError::from("value spill count exceeds u16"))?,
      locations,
    })
  }

  fn get(
    &self,
    value: VirtualValue,
  ) -> Result<AllocatedValue, CompilerError> {
    self
      .locations
      .get(value.0)
      .copied()
      .ok_or_else(|| CompilerError::from(format!("missing location for value {}", value.0)))
  }
}

#[derive(Clone, Copy)]
enum BranchKind {
  Jump,
  Conditional(LoweringOpcode, u8),
  Compare(u8, u8, u8),
  Handler,
}

struct BranchPatch {
  pc: usize,
  old_target: usize,
  kind: BranchKind,
  expanded_compare: bool,
}

#[derive(Default)]
pub(crate) struct Lowering {
  pub(crate) instructions: Vec<VirtualInstruction>,
  pub(crate) lines: Vec<u32>,
  pending: Option<(LoweringOpcode, u32, u32)>,
  pub(crate) values: Vec<VirtualValue>,
  pub(crate) max_values: usize,
  pub(crate) branch_values: HashMap<usize, Vec<VirtualValue>>,
  #[cfg(debug_assertions)]
  entry_values: Vec<Vec<VirtualValue>>,
  pub(crate) reachable: bool,
}

impl Lowering {
  pub(crate) fn new() -> Self {
    Self {
      reachable: true,
      ..Self::default()
    }
  }
  pub(crate) fn len(&mut self) -> usize {
    self.instructions.len() + usize::from(self.pending.is_some())
  }
  pub(crate) fn is_empty(&self) -> bool {
    self.instructions.is_empty() && self.pending.is_none()
  }
  pub(crate) fn is_reachable(&mut self) -> bool {
    self.finalize_pending();
    self.reachable
  }
  pub(crate) fn op_at(
    &self,
    index: usize,
  ) -> Option<LoweringOpcode> {
    self
      .instructions
      .get(index)
      .map(|instruction| instruction.opcode)
      .or_else(|| {
        (index == self.instructions.len())
          .then(|| self.pending.map(|pending| pending.0))
          .flatten()
      })
  }
  pub(crate) fn emit(
    &mut self,
    op: LoweringOpcode,
    payload: u32,
    line: u32,
  ) {
    self.finalize_pending();
    match op {
      LoweringOpcode::Discard => {
        self
          .values
          .pop()
          .expect("discard requires a value");
      }
      LoweringOpcode::Copy => {
        let value = *self.values.last().expect("copy requires a value");
        self.values.push(value);
        self.max_values = self.max_values.max(self.values.len());
      }
      LoweringOpcode::Exchange => {
        let len = self.values.len();
        assert!(len >= 2, "exchange requires two values");
        self.values.swap(len - 2, len - 1);
      }
      _ => self.pending = Some((op, payload & 0x00ff_ffff, line)),
    }
  }
  pub(crate) fn append_operand_byte(
    &mut self,
    index: usize,
    byte: u8,
    line: u32,
  ) {
    let (_, payload, pending_line) = self
      .pending
      .as_mut()
      .expect("operand follows instruction");
    *payload |= u32::from(byte) << (index * 8);
    *pending_line = line;
  }
  pub(crate) fn set_last_payload(
    &mut self,
    payload: u32,
    line: u32,
  ) {
    let (_, pending_payload, pending_line) = self
      .pending
      .as_mut()
      .expect("payload follows instruction");
    *pending_payload = payload & 0x00ff_ffff;
    *pending_line = line;
  }
  pub(crate) fn patch_branch(
    &mut self,
    index: usize,
    value: i32,
  ) {
    self.finalize_pending();
    let instruction = self
      .instructions
      .get_mut(index)
      .expect("jump patch in bounds");
    instruction.branch_offset = Some(value);
    let target = usize::try_from(index as i64 + 1 + i64::from(value)).expect("branch target");
    if let Some(edge) = self.branch_values.get(&index).cloned() {
      if target == self.instructions.len() {
        if self.reachable {
          assert!(
            self.values.starts_with(&edge) || edge.starts_with(&self.values),
            "conflicting explicit values at branch merge"
          );
          self.values = edge;
        } else {
          self.values = edge;
          self.reachable = true;
        }
      } else {
        #[cfg(debug_assertions)]
        if let Some(entry) = self.entry_values.get(target) {
          assert!(
            entry.starts_with(&edge) || edge.starts_with(entry),
            "conflicting explicit value homes at branch merge"
          );
        }
      }
    }
  }
  pub(crate) fn patch_branch_line(
    &mut self,
    index: usize,
    value: i32,
    line: u32,
  ) {
    self.patch_branch(index, value);
    if let Some(target_line) = self.lines.get_mut(index) {
      *target_line = line;
    }
  }

  fn finalize_pending(&mut self) {
    let Some((op, payload, line)) = self.pending.take() else {
      return;
    };
    let mut shape =
      operation_shape(op, payload).expect("frontend operation has a defined value shape");
    if op == LoweringOpcode::Return && !self.values.is_empty() {
      shape.input_count = 1;
    }
    assert!(
      self.values.len() >= shape.input_count,
      "frontend value underflow for {}",
      op.name()
    );
    let entry_value_count = self.values.len();
    #[cfg(debug_assertions)]
    let entry_values = self.values.clone();
    let inputs = VirtualInputs::from_slice(&self.values[self.values.len() - shape.input_count..]);
    self
      .values
      .truncate(self.values.len() - shape.input_count);
    let output = if !shape.has_output {
      None
    } else {
      let value = VirtualValue(self.values.len());
      self.values.push(value);
      Some(value)
    };
    self.max_values = self
      .max_values
      .max(entry_value_count)
      .max(self.values.len());
    let index = self.instructions.len();
    #[cfg(debug_assertions)]
    self.entry_values.push(entry_values);
    self.instructions.push(VirtualInstruction {
      opcode: op,
      payload,
      branch_offset: None,
      inputs,
      output,
    });
    self.lines.push(line);
    match op {
      LoweringOpcode::Jump => {
        self
          .branch_values
          .insert(index, self.values.clone());
        self.values.clear();
        self.reachable = false;
      }
      LoweringOpcode::JumpIfFalse
      | LoweringOpcode::JumpIfTrue
      | LoweringOpcode::JumpIfNullish
      | LoweringOpcode::CompareJump => {
        self
          .branch_values
          .insert(index, self.values.clone());
      }
      LoweringOpcode::PushHandler => {
        let mut handler = self.values.clone();
        handler.push(VirtualValue(handler.len()));
        self.max_values = self.max_values.max(handler.len());
        self.branch_values.insert(index, handler);
      }
      LoweringOpcode::Return | LoweringOpcode::Throw => {
        self.values.clear();
        self.reachable = false;
      }
      _ => {}
    }
  }

  pub(crate) fn finish(
    mut chunk: Chunk,
    mut builder: Lowering,
  ) -> Result<Chunk, CompilerError> {
    builder.finalize_pending();
    crate::compiler::optimize::optimize(&mut builder, &mut chunk);
    for instruction in &builder.instructions {
      if !executor_supports(instruction.opcode) {
        return Err(CompilerError::from(format!(
          "encoder does not support {}",
          instruction.opcode.name()
        )));
      }
    }
    let plan = AllocationPlan::for_value_count(builder.max_values)?;
    debug_assert!(
      chunk.words.is_empty(),
      "frontend instructions leaked into Chunk::chunk"
    );
    let window_size = builder
      .instructions
      .iter()
      .map(|instruction| window_size(instruction.opcode, instruction.payload))
      .max()
      .unwrap_or(0);
    let frontend_handlers = chunk.finally_handlers.clone();
    chunk.version = BytecodeVersion::RiscV2;
    let mut lowered = chunk;
    lowered.spill_slot_count = plan
      .value_spill_count
      .checked_add(
        u16::try_from(window_size)
          .map_err(|_| CompilerError::from("operand window exceeds u16"))?,
      )
      .ok_or_else(|| CompilerError::from("spill count exceeds u16"))?;
    lowered.value_spill_slot_count = plan.value_spill_count;

    let mut old_to_new = vec![None; builder.instructions.len() + 1];
    let mut patches = Vec::new();
    let handler_values: HashMap<usize, VirtualValue> = builder
      .instructions
      .iter()
      .enumerate()
      .filter_map(|(pc, instruction)| {
        (instruction.opcode == LoweringOpcode::PushHandler)
          .then(|| instruction.branch_offset())
          .flatten()
          .and_then(|offset| usize::try_from(pc as i64 + 1 + offset as i64).ok())
          .zip(
            builder
              .branch_values
              .get(&pc)
              .and_then(|values| values.last())
              .copied(),
          )
      })
      .collect();

    for (old_pc, new_pc) in old_to_new
      .iter_mut()
      .enumerate()
      .take(builder.instructions.len())
    {
      let instruction = pending_instruction(&builder, old_pc)?;
      *new_pc = Some(lowered.words.len());
      if let Some(exception) = handler_values.get(&old_pc).copied() {
        let exception_location = plan.get(exception)?;
        store_location(
          &mut lowered,
          exception_location,
          abi::EXCEPTION.encoding(),
          builder.lines[old_pc],
        );
      }
      let line = builder.lines[old_pc];
      let arg = instruction.operand_u8(0).unwrap_or_default();

      match instruction.opcode {
        LoweringOpcode::Const
        | LoweringOpcode::GetGlobal
        | LoweringOpcode::TypeofGlobal
        | LoweringOpcode::GetBuiltin => {
          let destination = plan.get(instruction.output()?)?;
          let rd = destination_register(destination, 0);
          emit(
            &mut lowered,
            encode_aux16_instruction(instruction.opcode, rd, instruction.operand_u16()),
            line,
          );
          store_location(&mut lowered, destination, rd, line);
        }
        LoweringOpcode::Null
        | LoweringOpcode::True
        | LoweringOpcode::False
        | LoweringOpcode::Undefined
        | LoweringOpcode::GetThis
        | LoweringOpcode::GetNewTarget => {
          let destination = plan.get(instruction.output()?)?;
          let rd = destination_register(destination, 0);
          emit(
            &mut lowered,
            op_word(instruction.opcode) | encode_rd(rd),
            line,
          );
          store_location(&mut lowered, destination, rd, line);
        }
        LoweringOpcode::LoadLocal | LoweringOpcode::GetUpvalue | LoweringOpcode::MakeClosure => {
          let destination = plan.get(instruction.output()?)?;
          let rd = destination_register(destination, 0);
          emit(
            &mut lowered,
            encode_aux16_instruction(
              instruction.opcode,
              rd,
              if matches!(
                instruction.opcode,
                LoweringOpcode::LoadLocal
                  | LoweringOpcode::GetUpvalue
                  | LoweringOpcode::MakeClosure
              ) {
                instruction.operand_u16()
              } else {
                arg as u16
              },
            ),
            line,
          );
          store_location(&mut lowered, destination, rd, line);
        }
        LoweringOpcode::GetModuleExport => {
          let destination = plan.get(instruction.output()?)?;
          let rd = destination_register(destination, 0);
          let word = op_word(instruction.opcode)
            | encode_rd(rd)
            | ((instruction.operand_u8(0).unwrap_or_default() as u32) << 16)
            | ((instruction.operand_u8(1).unwrap_or_default() as u32) << 24);
          emit(&mut lowered, word, line);
          store_location(&mut lowered, destination, rd, line);
        }
        LoweringOpcode::StoreLocal | LoweringOpcode::SetGlobal | LoweringOpcode::SetUpvalue => {
          let source = load_location(&mut lowered, plan.get(instruction.input(0)?)?, 0, line);
          emit(
            &mut lowered,
            encode_aux16_instruction(instruction.opcode, source, instruction.operand_u16()),
            line,
          );
        }
        LoweringOpcode::SetModuleExport => {
          let source = load_location(&mut lowered, plan.get(instruction.input(0)?)?, 0, line);
          let word = op_word(instruction.opcode)
            | encode_rd(source)
            | ((instruction.operand_u8(0).unwrap_or_default() as u32) << 16)
            | ((instruction.operand_u8(1).unwrap_or_default() as u32) << 24);
          emit(&mut lowered, word, line);
        }
        LoweringOpcode::Neg
        | LoweringOpcode::Not
        | LoweringOpcode::BitNot
        | LoweringOpcode::Typeof => {
          let destination = plan.get(instruction.output()?)?;
          let source = load_location(&mut lowered, plan.get(instruction.input(0)?)?, 0, line);
          let rd = destination_register(destination, source);
          emit(
            &mut lowered,
            encode_register_instruction(instruction.opcode, rd, source),
            line,
          );
          store_location(&mut lowered, destination, rd, line);
        }
        LoweringOpcode::Add
        | LoweringOpcode::Sub
        | LoweringOpcode::Mul
        | LoweringOpcode::Div
        | LoweringOpcode::Mod
        | LoweringOpcode::Pow
        | LoweringOpcode::BitAnd
        | LoweringOpcode::BitOr
        | LoweringOpcode::BitXor
        | LoweringOpcode::Shl
        | LoweringOpcode::Shr
        | LoweringOpcode::Ushr
        | LoweringOpcode::And
        | LoweringOpcode::Or
        | LoweringOpcode::Increment
        | LoweringOpcode::Decrement
        | LoweringOpcode::GetProperty
        | LoweringOpcode::DeleteProperty
        | LoweringOpcode::HasProperty
        | LoweringOpcode::InstanceOf
        | LoweringOpcode::GetSuperProperty => {
          let destination = plan.get(instruction.output()?)?;
          let left = load_location(&mut lowered, plan.get(instruction.input(0)?)?, 0, line);
          let right = load_location(&mut lowered, plan.get(instruction.input(1)?)?, 1, line);
          let rd = destination_register(destination, left);
          if rd != left {
            emit(
              &mut lowered,
              encode_register_instruction(LoweringOpcode::Move, rd, left),
              line,
            );
          }
          emit(
            &mut lowered,
            encode_register_instruction(instruction.opcode, rd, right),
            line,
          );
          store_location(&mut lowered, destination, rd, line);
        }
        LoweringOpcode::SetProperty | LoweringOpcode::SetSuperProperty => {
          for (index, register) in [(0, 0), (1, 1), (2, 2)] {
            let source = load_location(
              &mut lowered,
              plan.get(instruction.input(index)?)?,
              register,
              line,
            );
            if source != register {
              emit(
                &mut lowered,
                encode_register_instruction(LoweringOpcode::Move, register, source),
                line,
              );
            }
          }
          emit(
            &mut lowered,
            encode_register_instruction(instruction.opcode, 0, 1) | (2_u32 << 16),
            line,
          );
          store_location(&mut lowered, plan.get(instruction.output()?)?, 0, line);
        }
        LoweringOpcode::Spread => {
          for (index, register) in [(0, 0), (1, 1), (2, 2)] {
            move_location_to_register(
              &mut lowered,
              plan.get(instruction.input(index)?)?,
              register,
              line,
            );
          }
          emit(
            &mut lowered,
            encode_register_instruction(LoweringOpcode::Spread, 0, 1) | (2_u32 << 16),
            line,
          );
          store_location(&mut lowered, plan.get(instruction.output()?)?, 0, line);
        }
        LoweringOpcode::ArrayFromSpread => {
          let destination = plan.get(instruction.output()?)?;
          let values = load_location(&mut lowered, plan.get(instruction.input(0)?)?, 0, line);
          let mask = load_location(&mut lowered, plan.get(instruction.input(1)?)?, 1, line);
          let rd = destination_register(destination, values);
          if rd != values {
            emit(
              &mut lowered,
              encode_register_instruction(LoweringOpcode::Move, rd, values),
              line,
            );
          }
          emit(
            &mut lowered,
            encode_register_instruction(LoweringOpcode::ArrayFromSpread, rd, mask),
            line,
          );
          store_location(&mut lowered, destination, rd, line);
        }
        LoweringOpcode::Call | LoweringOpcode::TailCall | LoweringOpcode::Construct => {
          let count = arg as usize;
          let inputs = instruction.inputs.as_slice();
          let direct = count <= abi::REGISTER_ARGUMENT_COUNT
            && all_register_homed(&plan, &inputs[..1 + count])?;
          if direct {
            for index in 0..count {
              move_location_to_register(
                &mut lowered,
                plan.get(inputs[1 + index])?,
                abi::argument_register(index)
                  .expect("bounded call argument must have an ABI register")
                  .encoding(),
                line,
              );
            }
            move_location_to_register(
              &mut lowered,
              plan.get(inputs[0])?,
              abi::V2_CALL_OPERAND.encoding(),
              line,
            );
          } else {
            let overflow = count.saturating_sub(abi::REGISTER_ARGUMENT_COUNT);
            stage_window_at(&mut lowered, &plan, inputs, overflow, line)?;
            stage_window(
              &mut lowered,
              &plan,
              &inputs[1 + count.min(abi::REGISTER_ARGUMENT_COUNT)..],
              line,
            )?;
            let window_base = plan
              .value_spill_count
              .checked_add(
                u16::try_from(overflow)
                  .map_err(|_| CompilerError::from("call window exceeds u16"))?,
              )
              .ok_or_else(|| CompilerError::from("call window exceeds u16"))?;
            move_location_to_register(
              &mut lowered,
              AllocatedValue::SpillSlot(window_base),
              abi::V2_CALL_OPERAND.encoding(),
              line,
            );
            for index in 0..count.min(abi::REGISTER_ARGUMENT_COUNT) {
              move_location_to_register(
                &mut lowered,
                AllocatedValue::SpillSlot(
                  window_base
                    .checked_add(
                      u16::try_from(1 + index)
                        .map_err(|_| CompilerError::from("call input exceeds u16"))?,
                    )
                    .ok_or_else(|| CompilerError::from("call input exceeds u16"))?,
                ),
                abi::argument_register(index)
                  .expect("bounded call argument must have an ABI register")
                  .encoding(),
                line,
              );
            }
          }
          emit(
            &mut lowered,
            encode_aux16_instruction(
              instruction.opcode,
              abi::V2_CALL_OPERAND.encoding(),
              count as u16,
            ),
            line,
          );
          store_location(
            &mut lowered,
            plan.get(instruction.output()?)?,
            abi::RETURN.encoding(),
            line,
          );
        }
        LoweringOpcode::CallMethod => {
          let count = arg as usize;
          let inputs = instruction.inputs.as_slice();
          let direct = count <= abi::REGISTER_ARGUMENT_COUNT
            && all_register_homed(&plan, &inputs[..2 + count])?;
          if direct {
            for index in 0..count {
              move_location_to_register(
                &mut lowered,
                plan.get(inputs[2 + index])?,
                abi::argument_register(index)
                  .expect("bounded call argument must have an ABI register")
                  .encoding(),
                line,
              );
            }
            move_location_to_register(
              &mut lowered,
              plan.get(inputs[0])?,
              abi::RECEIVER.encoding(),
              line,
            );
            move_location_to_register(
              &mut lowered,
              plan.get(inputs[1])?,
              abi::V2_CALL_OPERAND.encoding(),
              line,
            );
          } else {
            let overflow = count.saturating_sub(abi::REGISTER_ARGUMENT_COUNT);
            stage_window_at(&mut lowered, &plan, inputs, overflow, line)?;
            stage_window(
              &mut lowered,
              &plan,
              &inputs[2 + count.min(abi::REGISTER_ARGUMENT_COUNT)..],
              line,
            )?;
            let window_base = plan
              .value_spill_count
              .checked_add(
                u16::try_from(overflow)
                  .map_err(|_| CompilerError::from("method call window exceeds u16"))?,
              )
              .ok_or_else(|| CompilerError::from("method call window exceeds u16"))?;
            move_location_to_register(
              &mut lowered,
              AllocatedValue::SpillSlot(window_base),
              abi::RECEIVER.encoding(),
              line,
            );
            move_location_to_register(
              &mut lowered,
              AllocatedValue::SpillSlot(window_base + 1),
              abi::V2_CALL_OPERAND.encoding(),
              line,
            );
            for index in 0..count.min(abi::REGISTER_ARGUMENT_COUNT) {
              move_location_to_register(
                &mut lowered,
                AllocatedValue::SpillSlot(
                  window_base
                    .checked_add(
                      u16::try_from(2 + index)
                        .map_err(|_| CompilerError::from("method call input exceeds u16"))?,
                    )
                    .ok_or_else(|| CompilerError::from("method call input exceeds u16"))?,
                ),
                abi::argument_register(index)
                  .expect("bounded call argument must have an ABI register")
                  .encoding(),
                line,
              );
            }
          }
          emit(
            &mut lowered,
            encode_aux16_instruction(
              instruction.opcode,
              abi::V2_CALL_OPERAND.encoding(),
              count as u16,
            ),
            line,
          );
          store_location(
            &mut lowered,
            plan.get(instruction.output()?)?,
            abi::RETURN.encoding(),
            line,
          );
        }
        LoweringOpcode::BuildArray => {
          let count = instruction.operand_u16();
          stage_window(&mut lowered, &plan, instruction.inputs.as_slice(), line)?;
          let destination = plan.get(instruction.output()?)?;
          let rd = destination_register(destination, 0);
          emit(
            &mut lowered,
            encode_aux16_instruction(instruction.opcode, rd, count),
            line,
          );
          store_location(&mut lowered, destination, rd, line);
        }
        LoweringOpcode::BuildObject => {
          let count = arg as usize;
          stage_window(&mut lowered, &plan, instruction.inputs.as_slice(), line)?;
          let destination = plan.get(instruction.output()?)?;
          let rd = destination_register(destination, 0);
          emit(
            &mut lowered,
            encode_aux16_instruction(instruction.opcode, rd, count as u16),
            line,
          );
          store_location(&mut lowered, destination, rd, line);
        }
        LoweringOpcode::SuperConstruct => {
          let count = arg as usize;
          stage_window(&mut lowered, &plan, instruction.inputs.as_slice(), line)?;
          let destination = plan.get(instruction.output()?)?;
          let rd = destination_register(destination, 0);
          emit(
            &mut lowered,
            encode_aux16_instruction(LoweringOpcode::SuperConstruct, rd, count as u16),
            line,
          );
          store_location(&mut lowered, destination, rd, line);
        }
        LoweringOpcode::DeleteIdentifier
        | LoweringOpcode::IteratorNext
        | LoweringOpcode::IteratorClose
        | LoweringOpcode::GetAsyncIterator => {
          let destination = plan.get(instruction.output()?)?;
          let source = load_location(&mut lowered, plan.get(instruction.input(0)?)?, 0, line);
          let rd = destination_register(destination, source);
          emit(
            &mut lowered,
            encode_register_instruction(instruction.opcode, rd, source),
            line,
          );
          store_location(&mut lowered, destination, rd, line);
        }
        LoweringOpcode::CloneLocalCell | LoweringOpcode::ResetLocalCell => {
          emit(
            &mut lowered,
            encode_aux16_instruction(instruction.opcode, 0, instruction.operand_u16()),
            line,
          );
        }
        LoweringOpcode::Jump => add_branch_patch(
          &builder,
          old_pc,
          &mut lowered,
          line,
          BranchKind::Jump,
          &mut patches,
        )?,
        LoweringOpcode::JumpIfFalse
        | LoweringOpcode::JumpIfTrue
        | LoweringOpcode::JumpIfNullish => {
          let source = load_location(&mut lowered, plan.get(instruction.input(0)?)?, 0, line);
          add_branch_patch(
            &builder,
            old_pc,
            &mut lowered,
            line,
            BranchKind::Conditional(instruction.opcode, source),
            &mut patches,
          )?;
        }
        LoweringOpcode::CompareJump => {
          let left = load_location(&mut lowered, plan.get(instruction.input(0)?)?, 0, line);
          let right = load_location(&mut lowered, plan.get(instruction.input(1)?)?, 1, line);
          add_branch_patch(
            &builder,
            old_pc,
            &mut lowered,
            line,
            BranchKind::Compare(arg, left, right),
            &mut patches,
          )?;
        }
        LoweringOpcode::PushHandler => add_branch_patch(
          &builder,
          old_pc,
          &mut lowered,
          line,
          BranchKind::Handler,
          &mut patches,
        )?,
        LoweringOpcode::Return => {
          let source = if instruction.inputs.is_empty() {
            emit(
              &mut lowered,
              op_word(LoweringOpcode::Undefined) | encode_rd(0),
              line,
            );
            0
          } else {
            load_location(&mut lowered, plan.get(instruction.input(0)?)?, 0, line)
          };
          emit(
            &mut lowered,
            op_word(instruction.opcode) | encode_rd(source),
            line,
          );
        }
        LoweringOpcode::Throw | LoweringOpcode::PushWithEnv => {
          let source = load_location(&mut lowered, plan.get(instruction.input(0)?)?, 0, line);
          emit(
            &mut lowered,
            op_word(instruction.opcode) | encode_rd(source),
            line,
          );
        }
        LoweringOpcode::PushLexicalEnv => {
          let source = load_location(&mut lowered, plan.get(instruction.input(0)?)?, 0, line);
          emit(
            &mut lowered,
            encode_aux16_instruction(instruction.opcode, source, instruction.operand_u16()),
            line,
          );
        }
        LoweringOpcode::Yield | LoweringOpcode::YieldStar | LoweringOpcode::AwaitPromise => {
          let destination = plan.get(instruction.output()?)?;
          let source = load_location(&mut lowered, plan.get(instruction.input(0)?)?, 0, line);
          let rd = destination_register(destination, source);
          emit(
            &mut lowered,
            op_word(instruction.opcode)
              | encode_rd(rd)
              | ((instruction.operand_u8(0).unwrap_or_default() as u32) << 16),
            line,
          );
          store_location(&mut lowered, destination, rd, line);
        }
        LoweringOpcode::PopEnv | LoweringOpcode::PopHandler | LoweringOpcode::GeneratorInitDone => {
          emit(&mut lowered, op_word(instruction.opcode), line);
        }
        other => {
          return Err(CompilerError::from(format!(
            "encoding for {} is not implemented",
            other.name()
          )));
        }
      }
    }
    old_to_new[builder.instructions.len()] = Some(lowered.words.len());

    relax_compare_branches(&mut lowered, &mut old_to_new, &mut patches)?;

    for patch in patches {
      let target = old_to_new
        .get(patch.old_target)
        .and_then(|target| *target)
        .ok_or_else(|| CompilerError::from("branch target is not present in frontend value IR"))?;
      let offset = target as i64 - (patch.pc as i64 + 1);
      if patch.expanded_compare {
        let BranchKind::Compare(condition, left, right) = patch.kind else {
          return Err(CompilerError::from(
            "only compare branches may use the expanded branch sequence",
          ));
        };
        lowered.words[patch.pc] = encode_compare_jump(condition, left, right, 1)
          .ok_or_else(|| CompilerError::from("invalid expanded compare branch"))?;
        lowered.words[patch.pc + 1] = encode_branch_patch(BranchKind::Jump, 1)?;
        let long_offset = target as i64 - (patch.pc as i64 + 3);
        lowered.words[patch.pc + 2] = encode_branch_patch(BranchKind::Jump, long_offset)?;
      } else {
        let word = encode_branch_patch(patch.kind, offset)?;
        lowered.words[patch.pc] = word;
      }
    }

    lowered.finally_handlers = frontend_handlers
      .iter()
      .map(|handler| {
        let push_pc = old_to_new
          .get(handler.push_pc)
          .and_then(|pc| *pc)
          .ok_or_else(|| CompilerError::from("finally handler push is unreachable"))?;
        let finalizer_pc = old_to_new
          .get(handler.finalizer_pc)
          .and_then(|pc| *pc)
          .ok_or_else(|| CompilerError::from("finally body is unreachable"))?;
        Ok(FinallyHandler {
          push_pc,
          finalizer_pc,
          return_slot: handler.return_slot,
          did_return_slot: handler.did_return_slot,
        })
      })
      .collect::<Result<Vec<_>, CompilerError>>()?;

    Ok(lowered)
  }
}

fn encode_branch_patch(
  kind: BranchKind,
  offset: i64,
) -> Result<u32, CompilerError> {
  match kind {
    BranchKind::Jump | BranchKind::Handler => {
      let op = if matches!(kind, BranchKind::Handler) {
        LoweringOpcode::PushHandler
      } else {
        LoweringOpcode::Jump
      };
      let offset =
        i32::try_from(offset).map_err(|_| CompilerError::from("long branch exceeds signed i24"))?;
      if !(-(1 << 23)..(1 << 23)).contains(&offset) {
        return Err(CompilerError::from("long branch exceeds signed i24"));
      }
      Ok(op_word(op) | ((offset.cast_unsigned() & 0x00FF_FFFF) << 8))
    }
    BranchKind::Conditional(op, register) => {
      let offset = i32::try_from(offset)
        .map_err(|_| CompilerError::from("conditional branch exceeds signed i20"))?;
      encode_conditional_branch(op, register, offset)
        .ok_or_else(|| CompilerError::from("conditional branch exceeds signed i20"))
    }
    BranchKind::Compare(condition, left, right) => {
      let offset = i16::try_from(offset)
        .map_err(|_| CompilerError::from("compare branch exceeds signed i13"))?;
      encode_compare_jump(condition, left, right, offset)
        .ok_or_else(|| CompilerError::from("compare branch exceeds signed i13"))
    }
  }
}

fn relax_compare_branches(
  lowered: &mut Chunk,
  old_to_new: &mut [Option<usize>],
  patches: &mut [BranchPatch],
) -> Result<(), CompilerError> {
  loop {
    let mut expansion = None;
    for (index, patch) in patches.iter().enumerate() {
      if patch.expanded_compare || !matches!(patch.kind, BranchKind::Compare(..)) {
        continue;
      }
      let target = old_to_new
        .get(patch.old_target)
        .and_then(|target| *target)
        .ok_or_else(|| CompilerError::from("branch target is not present in frontend value IR"))?;
      let offset = target as i64 - (patch.pc as i64 + 1);
      if !(-4096..=4095).contains(&offset) {
        expansion = Some((index, patch.pc));
        break;
      }
    }

    let Some((patch_index, compare_pc)) = expansion else {
      return Ok(());
    };
    let line = lowered
      .lines
      .get(compare_pc)
      .copied()
      .ok_or_else(|| CompilerError::from("missing line for compare branch"))?;
    lowered
      .words
      .splice(compare_pc + 1..compare_pc + 1, [0, 0]);
    lowered
      .lines
      .splice(compare_pc + 1..compare_pc + 1, [line, line]);

    for target in old_to_new.iter_mut().flatten() {
      if *target > compare_pc {
        *target = target
          .checked_add(2)
          .ok_or_else(|| CompilerError::from("branch relaxation overflow"))?;
      }
    }
    for patch in patches.iter_mut() {
      if patch.pc > compare_pc {
        patch.pc = patch
          .pc
          .checked_add(2)
          .ok_or_else(|| CompilerError::from("branch relaxation overflow"))?;
      }
    }
    patches[patch_index].expanded_compare = true;
  }
}

fn executor_supports(op: LoweringOpcode) -> bool {
  matches!(
    op,
    LoweringOpcode::Const
      | LoweringOpcode::Null
      | LoweringOpcode::True
      | LoweringOpcode::False
      | LoweringOpcode::Undefined
      | LoweringOpcode::LoadLocal
      | LoweringOpcode::StoreLocal
      | LoweringOpcode::Add
      | LoweringOpcode::Sub
      | LoweringOpcode::Mul
      | LoweringOpcode::Div
      | LoweringOpcode::Mod
      | LoweringOpcode::Pow
      | LoweringOpcode::Neg
      | LoweringOpcode::Increment
      | LoweringOpcode::Decrement
      | LoweringOpcode::BitNot
      | LoweringOpcode::BitAnd
      | LoweringOpcode::BitOr
      | LoweringOpcode::BitXor
      | LoweringOpcode::Shl
      | LoweringOpcode::Shr
      | LoweringOpcode::Ushr
      | LoweringOpcode::CompareJump
      | LoweringOpcode::Jump
      | LoweringOpcode::JumpIfTrue
      | LoweringOpcode::JumpIfFalse
      | LoweringOpcode::JumpIfNullish
      | LoweringOpcode::And
      | LoweringOpcode::Or
      | LoweringOpcode::Not
      | LoweringOpcode::Typeof
      | LoweringOpcode::GetProperty
      | LoweringOpcode::SetProperty
      | LoweringOpcode::DeleteProperty
      | LoweringOpcode::HasProperty
      | LoweringOpcode::GetGlobal
      | LoweringOpcode::TypeofGlobal
      | LoweringOpcode::SetGlobal
      | LoweringOpcode::GetUpvalue
      | LoweringOpcode::SetUpvalue
      | LoweringOpcode::Call
      | LoweringOpcode::TailCall
      | LoweringOpcode::Construct
      | LoweringOpcode::CallMethod
      | LoweringOpcode::Spread
      | LoweringOpcode::ArrayFromSpread
      | LoweringOpcode::MakeClosure
      | LoweringOpcode::GetThis
      | LoweringOpcode::GetNewTarget
      | LoweringOpcode::GetSuperProperty
      | LoweringOpcode::SetSuperProperty
      | LoweringOpcode::BuildArray
      | LoweringOpcode::BuildObject
      | LoweringOpcode::SuperConstruct
      | LoweringOpcode::Discard
      | LoweringOpcode::Copy
      | LoweringOpcode::Exchange
      | LoweringOpcode::Throw
      | LoweringOpcode::PushHandler
      | LoweringOpcode::PopHandler
      | LoweringOpcode::IteratorNext
      | LoweringOpcode::IteratorClose
      | LoweringOpcode::GetAsyncIterator
      | LoweringOpcode::Return
      | LoweringOpcode::PushWithEnv
      | LoweringOpcode::PushLexicalEnv
      | LoweringOpcode::PopEnv
      | LoweringOpcode::GeneratorInitDone
      | LoweringOpcode::Yield
      | LoweringOpcode::YieldStar
      | LoweringOpcode::AwaitPromise
      | LoweringOpcode::GetModuleExport
      | LoweringOpcode::SetModuleExport
      | LoweringOpcode::CloneLocalCell
      | LoweringOpcode::ResetLocalCell
      | LoweringOpcode::DeleteIdentifier
      | LoweringOpcode::GetBuiltin
      | LoweringOpcode::InstanceOf
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
  location: AllocatedValue,
  spill_scratch: u8,
) -> u8 {
  match location {
    AllocatedValue::Register(register) => register,
    AllocatedValue::SpillSlot(_) => spill_scratch,
  }
}

fn load_location(
  chunk: &mut Chunk,
  location: AllocatedValue,
  scratch: u8,
  line: u32,
) -> u8 {
  match location {
    AllocatedValue::Register(register) => register,
    AllocatedValue::SpillSlot(spill) => {
      emit(
        chunk,
        encode_aux16_instruction(LoweringOpcode::LoadSpill, scratch, spill),
        line,
      );
      scratch
    }
  }
}

fn store_location(
  chunk: &mut Chunk,
  location: AllocatedValue,
  source: u8,
  line: u32,
) {
  match location {
    AllocatedValue::Register(register) if register != source => {
      emit(
        chunk,
        encode_register_instruction(LoweringOpcode::Move, register, source),
        line,
      );
    }
    AllocatedValue::Register(_) => {}
    AllocatedValue::SpillSlot(spill) => {
      emit(
        chunk,
        encode_aux16_instruction(LoweringOpcode::StoreSpill, source, spill),
        line,
      );
    }
  }
}

fn move_location_to_register(
  chunk: &mut Chunk,
  location: AllocatedValue,
  register: u8,
  line: u32,
) {
  let source = load_location(chunk, location, register, line);
  if source != register {
    emit(
      chunk,
      encode_register_instruction(LoweringOpcode::Move, register, source),
      line,
    );
  }
}

fn all_register_homed(
  plan: &AllocationPlan,
  inputs: &[VirtualValue],
) -> Result<bool, CompilerError> {
  inputs
    .iter()
    .try_fold(true, |all_registers, value| {
      Ok(all_registers && matches!(plan.get(*value)?, AllocatedValue::Register(_)))
    })
}

fn stage_window(
  chunk: &mut Chunk,
  plan: &AllocationPlan,
  inputs: &[VirtualValue],
  line: u32,
) -> Result<(), CompilerError> {
  stage_window_at(chunk, plan, inputs, 0, line)
}

fn stage_window_at(
  chunk: &mut Chunk,
  plan: &AllocationPlan,
  inputs: &[VirtualValue],
  window_offset: usize,
  line: u32,
) -> Result<(), CompilerError> {
  for (index, value) in inputs.iter().copied().enumerate() {
    let AllocatedValue::Register(source) = plan.get(value)? else {
      continue;
    };
    let spill = plan
      .value_spill_count
      .checked_add(
        u16::try_from(window_offset.saturating_add(index))
          .map_err(|_| CompilerError::from("operand window exceeds u16"))?,
      )
      .ok_or_else(|| CompilerError::from("operand spill index exceeds u16"))?;
    emit(
      chunk,
      encode_aux16_instruction(LoweringOpcode::StoreSpill, source, spill),
      line,
    );
  }
  for (index, value) in inputs.iter().copied().enumerate() {
    let location = plan.get(value)?;
    if matches!(location, AllocatedValue::Register(_)) {
      continue;
    }
    let source = load_location(chunk, location, 0, line);
    let spill = plan
      .value_spill_count
      .checked_add(
        u16::try_from(window_offset.saturating_add(index))
          .map_err(|_| CompilerError::from("operand window exceeds u16"))?,
      )
      .ok_or_else(|| CompilerError::from("operand spill index exceeds u16"))?;
    emit(
      chunk,
      encode_aux16_instruction(LoweringOpcode::StoreSpill, source, spill),
      line,
    );
  }
  Ok(())
}

fn window_size(
  op: LoweringOpcode,
  payload: u32,
) -> usize {
  let count = (payload & 0xFF) as usize;
  match op {
    LoweringOpcode::Call | LoweringOpcode::TailCall | LoweringOpcode::Construct => count
      .saturating_add(1)
      .saturating_add(count.saturating_sub(6)),
    LoweringOpcode::CallMethod => count
      .saturating_add(2)
      .saturating_add(count.saturating_sub(6)),
    LoweringOpcode::BuildArray => (payload & 0xFFFF) as usize,
    LoweringOpcode::BuildObject => count.saturating_mul(2),
    LoweringOpcode::SuperConstruct => count,
    _ => 0,
  }
}

fn add_branch_patch(
  builder: &Lowering,
  old_pc: usize,
  lowered: &mut Chunk,
  line: u32,
  kind: BranchKind,
  patches: &mut Vec<BranchPatch>,
) -> Result<(), CompilerError> {
  let instruction = pending_instruction(builder, old_pc)?;
  let offset = instruction.branch_offset().ok_or_else(|| {
    CompilerError::from(format!("invalid branch at frontend instruction {old_pc}"))
  })?;
  let target = usize::try_from(old_pc as i64 + 1 + offset as i64)
    .map_err(|_| CompilerError::from("negative frontend branch target"))?;
  let pc = lowered.words.len();
  emit(lowered, 0, line);
  patches.push(BranchPatch {
    pc,
    old_target: target,
    kind,
    expanded_compare: false,
  });
  Ok(())
}

#[derive(Clone, Copy, Debug)]
struct ValueShape {
  input_count: usize,
  has_output: bool,
}

impl ValueShape {
  const fn new(
    input_count: usize,
    has_output: bool,
  ) -> Self {
    Self {
      input_count,
      has_output,
    }
  }
}

fn operation_shape(
  op: LoweringOpcode,
  payload: u32,
) -> Result<ValueShape, CompilerError> {
  let arg = (payload & 0xFF) as usize;
  let effect = match op {
    LoweringOpcode::Const
    | LoweringOpcode::Null
    | LoweringOpcode::True
    | LoweringOpcode::False
    | LoweringOpcode::Undefined
    | LoweringOpcode::LoadLocal
    | LoweringOpcode::GetGlobal
    | LoweringOpcode::TypeofGlobal
    | LoweringOpcode::GetUpvalue
    | LoweringOpcode::GetThis
    | LoweringOpcode::GetNewTarget
    | LoweringOpcode::GetModuleExport
    | LoweringOpcode::MakeClosure
    | LoweringOpcode::GetBuiltin => ValueShape::new(0, true),

    LoweringOpcode::StoreLocal
    | LoweringOpcode::SetGlobal
    | LoweringOpcode::SetUpvalue
    | LoweringOpcode::PushWithEnv
    | LoweringOpcode::PushLexicalEnv
    | LoweringOpcode::SetModuleExport
    | LoweringOpcode::Throw => ValueShape::new(1, false),

    LoweringOpcode::Neg | LoweringOpcode::Not | LoweringOpcode::BitNot | LoweringOpcode::Typeof => {
      ValueShape::new(1, true)
    }
    LoweringOpcode::Add
    | LoweringOpcode::Sub
    | LoweringOpcode::Mul
    | LoweringOpcode::Div
    | LoweringOpcode::Mod
    | LoweringOpcode::Pow
    | LoweringOpcode::BitAnd
    | LoweringOpcode::BitOr
    | LoweringOpcode::BitXor
    | LoweringOpcode::Shl
    | LoweringOpcode::Shr
    | LoweringOpcode::Ushr
    | LoweringOpcode::And
    | LoweringOpcode::Or
    | LoweringOpcode::Increment
    | LoweringOpcode::Decrement
    | LoweringOpcode::GetProperty
    | LoweringOpcode::DeleteProperty
    | LoweringOpcode::HasProperty
    | LoweringOpcode::InstanceOf
    | LoweringOpcode::GetSuperProperty => ValueShape::new(2, true),

    LoweringOpcode::SetProperty | LoweringOpcode::SetSuperProperty => ValueShape::new(3, true),
    LoweringOpcode::DeleteIdentifier
    | LoweringOpcode::IteratorNext
    | LoweringOpcode::IteratorClose
    | LoweringOpcode::GetAsyncIterator => ValueShape::new(1, true),
    LoweringOpcode::BuildArray => ValueShape::new((payload & 0xFFFF) as usize, true),
    LoweringOpcode::BuildObject => ValueShape::new(arg.saturating_mul(2), true),
    LoweringOpcode::Call | LoweringOpcode::TailCall | LoweringOpcode::Construct => {
      ValueShape::new(arg + 1, true)
    }
    LoweringOpcode::CallMethod => ValueShape::new(arg + 2, true),
    LoweringOpcode::SuperConstruct => ValueShape::new(arg, true),
    LoweringOpcode::Spread => ValueShape::new(3, true),
    LoweringOpcode::ArrayFromSpread => ValueShape::new(2, true),
    LoweringOpcode::JumpIfFalse | LoweringOpcode::JumpIfTrue | LoweringOpcode::JumpIfNullish => {
      ValueShape::new(1, false)
    }
    LoweringOpcode::CompareJump => ValueShape::new(2, false),
    LoweringOpcode::Return => ValueShape::new(0, false),
    LoweringOpcode::Yield | LoweringOpcode::YieldStar | LoweringOpcode::AwaitPromise => {
      ValueShape::new(1, true)
    }
    LoweringOpcode::CloneLocalCell
    | LoweringOpcode::ResetLocalCell
    | LoweringOpcode::Move
    | LoweringOpcode::Jump
    | LoweringOpcode::PushHandler
    | LoweringOpcode::PopHandler
    | LoweringOpcode::PopEnv
    | LoweringOpcode::GeneratorInitDone => ValueShape::new(0, false),
    LoweringOpcode::Discard | LoweringOpcode::Copy | LoweringOpcode::Exchange => {
      return Err(CompilerError::from(
        "virtual value lifecycle operation reached instruction emission",
      ));
    }
    LoweringOpcode::LoadSpill | LoweringOpcode::StoreSpill => {
      return Err(CompilerError::from(
        "spill opcode encountered in frontend value IR",
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

  #[test]
  fn compiler_produces_only_verified_code_recursively() {
    let source = crate::js!("function value(x) { try { return x + 1; } finally {} } value(2);");
    let session = Parser::parse_source(source).expect("parse");
    let chunk = compiler::compile(session.program(), session.source()).expect("compile");
    chunk.verify().expect("verified chunk tree");
    fn assert_bytecode(chunk: &Chunk) {
      assert_eq!(chunk.version, BytecodeVersion::RiscV2);
      for word in &chunk.words {
        Opcode::from_byte((word & 0xff) as u8).expect("public opcode");
        assert!(!matches!(word & 0xff, 50..=53));
      }
      for function in &chunk.functions {
        assert_bytecode(&function.chunk);
      }
    }
    assert_bytecode(&chunk);
  }

  #[test]
  fn catch_destructuring_emits_verified_bytecode() {
    for source in [
      "try { throw [1]; } catch ([value]) { value; }",
      "try { throw { value: 1 }; } catch ({ value }) { value; }",
    ] {
      let session = Parser::parse_source(source).expect("parse");
      let chunk = compiler::compile(session.program(), session.source()).expect("compile");
      chunk.verify().expect("verified chunk");
    }
  }

  #[test]
  fn builtin_operands_preserve_u16_constant_indexes() {
    let mut source = (0..260)
      .map(|value| format!("{value};"))
      .collect::<String>();
    source.push_str(crate::js!("let { ...rest } = {};"));
    let session = Parser::parse_source(&source).expect("parse");
    let chunk = compiler::compile(session.program(), session.source()).expect("compile");
    chunk.verify().expect("verified chunk");
  }

  #[test]
  fn rejects_branch_offsets_before_narrowing() {
    let conditional = encode_branch_patch(
      BranchKind::Conditional(LoweringOpcode::JumpIfFalse, 0),
      1_i64 << 32,
    )
    .expect_err("offset must not wrap to i32");
    assert!(conditional.message.contains("signed i20"));

    let comparison = encode_branch_patch(BranchKind::Compare(0, 0, 0), 1_i64 << 16)
      .expect_err("offset must not wrap to i16");
    assert!(comparison.message.contains("signed i13"));
  }

  #[test]
  fn frontend_compare_branch_offsets_remain_full_width_until_lowering() {
    let mut builder = Lowering::new();
    builder.emit(LoweringOpcode::True, 0, 1);
    builder.emit(LoweringOpcode::True, 0, 1);
    builder.emit(LoweringOpcode::CompareJump, 6, 1);
    let branch_pc = builder.len() - 1;
    builder.patch_branch(branch_pc, i32::from(i16::MAX) + 1);
    assert_eq!(
      builder.instructions[branch_pc].branch_offset(),
      Some(i32::from(i16::MAX) + 1)
    );
  }

  fn assert_suspend_stores_spilled_output(op: LoweringOpcode) {
    let mut builder = Lowering::new();
    for _ in 0..9 {
      builder.emit(LoweringOpcode::Undefined, 0, 1);
    }
    builder.emit(op, 0, 1);
    builder.emit(LoweringOpcode::Return, 0, 1);

    let chunk = Lowering::finish(Chunk::default(), builder).expect("lower chunk");
    let suspend_pc = chunk
      .words
      .iter()
      .position(|word| Opcode::from_byte((word & 0xff) as u8) == Some(op.bytecode().unwrap()))
      .expect("suspending operation");
    assert_eq!(chunk.value_spill_slot_count, 1);
    assert_eq!(
      Opcode::from_byte((chunk.words[suspend_pc + 1] & 0xff) as u8),
      Some(LoweringOpcode::StoreSpill.bytecode().unwrap()),
      "{op:?} must store its spilled output"
    );
  }

  #[test]
  fn relaxes_compare_branches_beyond_signed_i13() {
    let body = crate::js!("value += 1;\n").repeat(1_100);
    let source =
      crate::js!("let value = 0; if (value === 0) { /* body */ }").replace("/* body */", &body);

    let session = Parser::parse_source(&source).expect("parse");
    let chunk = compiler::compile(session.program(), session.source()).expect("compile");
    chunk.verify().expect("verified chunk");

    let expanded = chunk.words.windows(3).any(|words| {
      Opcode::from_byte((words[0] & 0xff) as u8) == Some(Opcode::CompareJump)
        && crate::bytecode::decode_compare_jump(words[0]).3 == 1
        && Opcode::from_byte((words[1] & 0xff) as u8) == Some(Opcode::Jump)
        && ((words[1] as i32) >> 8) == 1
        && Opcode::from_byte((words[2] & 0xff) as u8) == Some(Opcode::Jump)
    });
    assert!(expanded, "expected a relaxed long compare branch");
  }

  #[test]
  fn yield_stores_spilled_output_location() {
    assert_suspend_stores_spilled_output(LoweringOpcode::Yield);
  }

  #[test]
  fn yield_star_stores_spilled_output_location() {
    assert_suspend_stores_spilled_output(LoweringOpcode::YieldStar);
  }

  #[test]
  fn await_stores_spilled_output_location() {
    assert_suspend_stores_spilled_output(LoweringOpcode::AwaitPromise);
  }
}
