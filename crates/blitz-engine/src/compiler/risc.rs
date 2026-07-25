// SPDX-License-Identifier: MPL-2.0
//! Explicit virtual-value builder and RISC v2 encoder.

use super::emit::FrontendOp as Op;
use crate::bytecode::{
  BytecodeVersion, Chunk, FinallyHandler, Op as BytecodeOp, encode_compare_jump, encode_rd,
  encode_risc_aux16 as encode_bytecode_aux16, encode_risc_cond_jump as encode_bytecode_cond_jump,
  encode_risc_registers as encode_bytecode_registers,
};
use crate::errors::CompilerError;
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ValueId(usize);

struct PendingInstruction {
  op: Op,
  payload: u32,
  inputs: InstructionInputs,
  output: Option<ValueId>,
}

enum InstructionInputs {
  None,
  One(ValueId),
  Two([ValueId; 2]),
  Many(Box<[ValueId]>),
}

impl InstructionInputs {
  fn from_slice(values: &[ValueId]) -> Self {
    match values {
      [] => Self::None,
      [value] => Self::One(*value),
      [left, right] => Self::Two([*left, *right]),
      _ => Self::Many(values.into()),
    }
  }

  fn as_slice(&self) -> &[ValueId] {
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

impl PendingInstruction {
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
    match self.op {
      Op::CompareJump => Some(i16::from_le_bytes([
        self.operand_u8(1).unwrap_or_default(),
        self.operand_u8(2).unwrap_or_default(),
      ]) as i32),
      Op::Jump | Op::JumpIfFalse | Op::JumpIfTrue | Op::JumpIfNullish | Op::PushHandler => {
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
  ) -> Result<ValueId, CompilerError> {
    self
      .inputs
      .as_slice()
      .get(index)
      .copied()
      .ok_or_else(|| CompilerError::from(format!("missing input {index} for {}", self.op.name())))
  }

  fn output(&self) -> Result<ValueId, CompilerError> {
    self
      .output
      .ok_or_else(|| CompilerError::from(format!("missing output for {}", self.op.name())))
  }
}

fn pending_instruction(
  builder: &RiscBuilder,
  ip: usize,
) -> Result<&PendingInstruction, CompilerError> {
  builder
    .instructions
    .get(ip)
    .ok_or_else(|| CompilerError::from("virtual instruction out of bounds"))
}

fn bytecode_op(op: Op) -> BytecodeOp {
  op.bytecode()
    .expect("virtual value operation cannot be encoded")
}

fn op_word(op: Op) -> u32 {
  bytecode_op(op) as u32
}

fn encode_risc_aux16(
  op: Op,
  rd: u8,
  aux: u16,
) -> u32 {
  encode_bytecode_aux16(bytecode_op(op), rd, aux)
}

fn encode_risc_registers(
  op: Op,
  rd: u8,
  rs: u8,
) -> u32 {
  encode_bytecode_registers(bytecode_op(op), rd, rs)
}

fn encode_risc_cond_jump(
  op: Op,
  rd: u8,
  offset: i32,
) -> Option<u32> {
  encode_bytecode_cond_jump(bytecode_op(op), rd, offset)
}

/// Physical home assigned to one explicit virtual value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ValueLocation {
  Register(u8),
  Spill(u16),
}

/// Register/spill assignment for explicit virtual values.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LocationPlan {
  pub(crate) locations: Vec<ValueLocation>,
  pub(crate) value_spill_count: u16,
}

impl LocationPlan {
  pub(crate) fn for_value_count(value_count: usize) -> Result<Self, CompilerError> {
    let mut locations = Vec::with_capacity(value_count);
    for position in 0..value_count {
      if position < 8 {
        locations.push(ValueLocation::Register(8 + position as u8));
      } else {
        let spill = u16::try_from(position - 8)
          .map_err(|_| CompilerError::from("RISC value spill count exceeds u16"))?;
        locations.push(ValueLocation::Spill(spill));
      }
    }
    Ok(Self {
      value_spill_count: u16::try_from(value_count.saturating_sub(8))
        .map_err(|_| CompilerError::from("RISC value spill count exceeds u16"))?,
      locations,
    })
  }

  fn get(
    &self,
    value: ValueId,
  ) -> Result<ValueLocation, CompilerError> {
    self
      .locations
      .get(value.0)
      .copied()
      .ok_or_else(|| CompilerError::from(format!("missing RISC location for value {}", value.0)))
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

/// Compiler-owned virtual-value builder. Frontend operations use stable value
/// positions; finalization assigns those values to `r8..r15` and spill slots.
#[derive(Default)]
pub(crate) struct RiscBuilder {
  instructions: Vec<PendingInstruction>,
  lines: Vec<u32>,
  pending: Option<(Op, u32, u32)>,
  values: Vec<ValueId>,
  max_values: usize,
  branch_values: HashMap<usize, Vec<ValueId>>,
  #[cfg(debug_assertions)]
  entry_values: HashMap<usize, Vec<ValueId>>,
  reachable: bool,
}

impl RiscBuilder {
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
  pub(crate) fn op_at(
    &self,
    index: usize,
  ) -> Option<Op> {
    self
      .instructions
      .get(index)
      .map(|instruction| instruction.op)
      .or_else(|| {
        (index == self.instructions.len())
          .then(|| self.pending.map(|pending| pending.0))
          .flatten()
      })
  }
  pub(crate) fn emit(
    &mut self,
    op: Op,
    payload: u32,
    line: u32,
  ) {
    self.finalize_pending();
    match op {
      Op::Discard => {
        self
          .values
          .pop()
          .expect("discard requires a value");
      }
      Op::Copy => {
        let value = *self.values.last().expect("copy requires a value");
        self.values.push(value);
        self.max_values = self.max_values.max(self.values.len());
      }
      Op::Exchange => {
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
    if instruction.op == Op::CompareJump {
      let condition = instruction.payload & 0xff;
      let offset = i16::try_from(value)
        .expect("compare jump offset overflow")
        .cast_unsigned();
      instruction.payload = condition | (u32::from(offset) << 8);
    } else {
      instruction.payload = value.cast_unsigned() & 0x00ff_ffff;
    }
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
        if let Some(entry) = self.entry_values.get(&target) {
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
    if op == Op::Return && !self.values.is_empty() {
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
    let inputs =
      InstructionInputs::from_slice(&self.values[self.values.len() - shape.input_count..]);
    self
      .values
      .truncate(self.values.len() - shape.input_count);
    let output = if !shape.has_output {
      None
    } else {
      let value = ValueId(self.values.len());
      self.values.push(value);
      Some(value)
    };
    self.max_values = self
      .max_values
      .max(entry_value_count)
      .max(self.values.len());
    let index = self.instructions.len();
    #[cfg(debug_assertions)]
    self.entry_values.insert(index, entry_values);
    self.instructions.push(PendingInstruction {
      op,
      payload,
      inputs,
      output,
    });
    self.lines.push(line);
    match op {
      Op::Jump => {
        self
          .branch_values
          .insert(index, self.values.clone());
        self.values.clear();
        self.reachable = false;
      }
      Op::JumpIfFalse | Op::JumpIfTrue | Op::JumpIfNullish | Op::CompareJump => {
        self
          .branch_values
          .insert(index, self.values.clone());
      }
      Op::PushHandler => {
        let mut handler = self.values.clone();
        handler.push(ValueId(handler.len()));
        self.max_values = self.max_values.max(handler.len());
        self.branch_values.insert(index, handler);
      }
      Op::Return | Op::Throw => {
        self.values.clear();
        self.reachable = false;
      }
      _ => {}
    }
  }

  pub(crate) fn finish(
    mut chunk: Chunk,
    mut builder: RiscBuilder,
  ) -> Result<Chunk, CompilerError> {
    builder.finalize_pending();
    for instruction in &builder.instructions {
      if !risc_executor_supports(instruction.op) {
        return Err(CompilerError::from(format!(
          "RISC encoder does not support {}",
          instruction.op.name()
        )));
      }
    }
    let plan = LocationPlan::for_value_count(builder.max_values)?;
    debug_assert!(
      chunk.code.is_empty(),
      "frontend instructions leaked into Chunk::code"
    );
    let window_size = builder
      .instructions
      .iter()
      .map(|instruction| risc_window_size(instruction.op, instruction.payload))
      .max()
      .unwrap_or(0);
    let frontend_handlers = chunk.finally_handlers.clone();
    chunk.version = BytecodeVersion::RiscV2;
    let mut lowered = chunk;
    lowered.risc_spill_count = plan
      .value_spill_count
      .checked_add(
        u16::try_from(window_size)
          .map_err(|_| CompilerError::from("RISC operand window exceeds u16"))?,
      )
      .ok_or_else(|| CompilerError::from("RISC spill count exceeds u16"))?;
    lowered.risc_value_spill_count = plan.value_spill_count;

    let mut old_to_new = vec![None; builder.instructions.len() + 1];
    let mut patches = Vec::new();
    let handler_values: HashMap<usize, ValueId> = builder
      .instructions
      .iter()
      .enumerate()
      .filter_map(|(ip, instruction)| {
        (instruction.op == Op::PushHandler)
          .then(|| instruction.branch_offset())
          .flatten()
          .and_then(|offset| usize::try_from(ip as i64 + 1 + offset as i64).ok())
          .zip(
            builder
              .branch_values
              .get(&ip)
              .and_then(|values| values.last())
              .copied(),
          )
      })
      .collect();

    for old_ip in 0..builder.instructions.len() {
      let instruction = pending_instruction(&builder, old_ip)?;
      old_to_new[old_ip] = Some(lowered.code.len());
      if let Some(exception) = handler_values.get(&old_ip).copied() {
        let exception_location = plan.get(exception)?;
        store_location(&mut lowered, exception_location, 7, builder.lines[old_ip]);
      }
      let line = builder.lines[old_ip];
      let arg = instruction.operand_u8(0).unwrap_or_default();

      match instruction.op {
        Op::Const | Op::GetGlobal | Op::TypeofGlobal => {
          let destination = plan.get(instruction.output()?)?;
          let rd = destination_register(destination, 0);
          emit(
            &mut lowered,
            encode_risc_aux16(instruction.op, rd, instruction.operand_u16()),
            line,
          );
          store_location(&mut lowered, destination, rd, line);
        }
        Op::Null | Op::True | Op::False | Op::Undefined | Op::GetThis | Op::GetNewTarget => {
          let destination = plan.get(instruction.output()?)?;
          let rd = destination_register(destination, 0);
          emit(&mut lowered, op_word(instruction.op) | encode_rd(rd), line);
          store_location(&mut lowered, destination, rd, line);
        }
        Op::LoadLocal | Op::GetUpvalue | Op::MakeClosure | Op::GetBuiltin => {
          let destination = plan.get(instruction.output()?)?;
          let rd = destination_register(destination, 0);
          emit(
            &mut lowered,
            encode_risc_aux16(instruction.op, rd, arg as u16),
            line,
          );
          store_location(&mut lowered, destination, rd, line);
        }
        Op::GetModuleExport => {
          let destination = plan.get(instruction.output()?)?;
          let rd = destination_register(destination, 0);
          let word = op_word(instruction.op)
            | encode_rd(rd)
            | ((instruction.operand_u8(0).unwrap_or_default() as u32) << 16)
            | ((instruction.operand_u8(1).unwrap_or_default() as u32) << 24);
          emit(&mut lowered, word, line);
          store_location(&mut lowered, destination, rd, line);
        }
        Op::StoreLocal | Op::SetGlobal | Op::SetUpvalue => {
          let source = load_location(&mut lowered, plan.get(instruction.input(0)?)?, 0, line);
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
          let source = load_location(&mut lowered, plan.get(instruction.input(0)?)?, 0, line);
          let word = op_word(instruction.op)
            | encode_rd(source)
            | ((instruction.operand_u8(0).unwrap_or_default() as u32) << 16)
            | ((instruction.operand_u8(1).unwrap_or_default() as u32) << 24);
          emit(&mut lowered, word, line);
        }
        Op::Neg | Op::Not | Op::BitNot | Op::Typeof => {
          let destination = plan.get(instruction.output()?)?;
          let source = load_location(&mut lowered, plan.get(instruction.input(0)?)?, 0, line);
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
          let destination = plan.get(instruction.output()?)?;
          let left = load_location(&mut lowered, plan.get(instruction.input(0)?)?, 0, line);
          let right = load_location(&mut lowered, plan.get(instruction.input(1)?)?, 1, line);
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
          store_location(&mut lowered, plan.get(instruction.output()?)?, 0, line);
        }
        Op::Spread => {
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
            encode_risc_registers(Op::Spread, 0, 1) | (2_u32 << 16),
            line,
          );
          store_location(&mut lowered, plan.get(instruction.output()?)?, 0, line);
        }
        Op::ArrayFromSpread => {
          let destination = plan.get(instruction.output()?)?;
          let values = load_location(&mut lowered, plan.get(instruction.input(0)?)?, 0, line);
          let mask = load_location(&mut lowered, plan.get(instruction.input(1)?)?, 1, line);
          let rd = destination_register(destination, values);
          if rd != values {
            emit(
              &mut lowered,
              encode_risc_registers(Op::Move, rd, values),
              line,
            );
          }
          emit(
            &mut lowered,
            encode_risc_registers(Op::ArrayFromSpread, rd, mask),
            line,
          );
          store_location(&mut lowered, destination, rd, line);
        }
        Op::Call | Op::TailCall | Op::Construct => {
          let count = arg as usize;
          stage_window(
            &mut lowered,
            &plan,
            &instruction.inputs.as_slice()[1 + count.min(6)..],
            line,
          )?;
          move_location_to_register(&mut lowered, plan.get(instruction.input(0)?)?, 0, line);
          for index in 0..count.min(6) {
            move_location_to_register(
              &mut lowered,
              plan.get(instruction.input(1 + index)?)?,
              2 + index as u8,
              line,
            );
          }
          emit(
            &mut lowered,
            encode_risc_aux16(instruction.op, 0, count as u16),
            line,
          );
          store_location(&mut lowered, plan.get(instruction.output()?)?, 0, line);
        }
        Op::CallBuiltin => {
          let count = arg as usize;
          stage_window(
            &mut lowered,
            &plan,
            &instruction.inputs.as_slice()[2 + count.min(6)..],
            line,
          )?;
          move_location_to_register(&mut lowered, plan.get(instruction.input(0)?)?, 1, line);
          move_location_to_register(&mut lowered, plan.get(instruction.input(1)?)?, 0, line);
          for index in 0..count.min(6) {
            move_location_to_register(
              &mut lowered,
              plan.get(instruction.input(2 + index)?)?,
              2 + index as u8,
              line,
            );
          }
          emit(
            &mut lowered,
            encode_risc_aux16(instruction.op, 0, count as u16),
            line,
          );
          store_location(&mut lowered, plan.get(instruction.output()?)?, 0, line);
        }
        Op::BuildArray => {
          let count = arg as usize;
          stage_window(&mut lowered, &plan, instruction.inputs.as_slice(), line)?;
          let destination = plan.get(instruction.output()?)?;
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
          stage_window(&mut lowered, &plan, instruction.inputs.as_slice(), line)?;
          let destination = plan.get(instruction.output()?)?;
          let rd = destination_register(destination, 0);
          emit(
            &mut lowered,
            encode_risc_aux16(instruction.op, rd, count as u16),
            line,
          );
          store_location(&mut lowered, destination, rd, line);
        }
        Op::SuperConstruct => {
          let count = arg as usize;
          stage_window(&mut lowered, &plan, instruction.inputs.as_slice(), line)?;
          let destination = plan.get(instruction.output()?)?;
          let rd = destination_register(destination, 0);
          emit(
            &mut lowered,
            encode_risc_aux16(Op::SuperConstruct, rd, count as u16),
            line,
          );
          store_location(&mut lowered, destination, rd, line);
        }
        Op::DeleteIdentifier | Op::IteratorNext | Op::IteratorClose => {
          let destination = plan.get(instruction.output()?)?;
          let source = load_location(&mut lowered, plan.get(instruction.input(0)?)?, 0, line);
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
          &builder,
          old_ip,
          &mut lowered,
          line,
          BranchKind::Jump,
          &mut patches,
        )?,
        Op::JumpIfFalse | Op::JumpIfTrue | Op::JumpIfNullish => {
          let source = load_location(&mut lowered, plan.get(instruction.input(0)?)?, 0, line);
          add_branch_patch(
            &builder,
            old_ip,
            &mut lowered,
            line,
            BranchKind::Conditional(instruction.op, source),
            &mut patches,
          )?;
        }
        Op::CompareJump => {
          let left = load_location(&mut lowered, plan.get(instruction.input(0)?)?, 0, line);
          let right = load_location(&mut lowered, plan.get(instruction.input(1)?)?, 1, line);
          add_branch_patch(
            &builder,
            old_ip,
            &mut lowered,
            line,
            BranchKind::Compare(arg, left, right),
            &mut patches,
          )?;
        }
        Op::PushHandler => add_branch_patch(
          &builder,
          old_ip,
          &mut lowered,
          line,
          BranchKind::Handler,
          &mut patches,
        )?,
        Op::Return => {
          let source = if instruction.inputs.is_empty() {
            emit(&mut lowered, op_word(Op::Undefined) | encode_rd(0), line);
            0
          } else {
            load_location(&mut lowered, plan.get(instruction.input(0)?)?, 0, line)
          };
          emit(
            &mut lowered,
            op_word(instruction.op) | encode_rd(source),
            line,
          );
        }
        Op::Throw | Op::PushWithEnv | Op::Yield | Op::YieldStar | Op::AwaitPromise => {
          let source = load_location(&mut lowered, plan.get(instruction.input(0)?)?, 0, line);
          emit(
            &mut lowered,
            op_word(instruction.op) | encode_rd(source),
            line,
          );
        }
        Op::PopEnv | Op::PopHandler | Op::GeneratorInitDone => {
          emit(&mut lowered, op_word(instruction.op), line);
        }
        other => {
          return Err(CompilerError::from(format!(
            "RISC encoding for {} is not implemented",
            other.name()
          )));
        }
      }
    }
    old_to_new[builder.instructions.len()] = Some(lowered.code.len());

    for patch in patches {
      let target = old_to_new
        .get(patch.old_target)
        .and_then(|target| *target)
        .ok_or_else(|| CompilerError::from("branch target is not present in frontend value IR"))?;
      let offset = target as i64 - (patch.risc_ip as i64 + 1);
      let word = encode_branch_patch(patch.kind, offset)?;
      lowered.code[patch.risc_ip] = word;
    }

    lowered.finally_handlers = frontend_handlers
      .iter()
      .map(|handler| {
        let push_ip = old_to_new
          .get(handler.push_ip)
          .and_then(|ip| *ip)
          .ok_or_else(|| CompilerError::from("finally handler push is unreachable"))?;
        let finalizer_ip = old_to_new
          .get(handler.finalizer_ip)
          .and_then(|ip| *ip)
          .ok_or_else(|| CompilerError::from("finally body is unreachable"))?;
        Ok(FinallyHandler {
          push_ip,
          finalizer_ip,
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
        Op::PushHandler
      } else {
        Op::Jump
      };
      let offset = i32::try_from(offset)
        .map_err(|_| CompilerError::from("RISC long branch exceeds signed i24"))?;
      if !(-(1 << 23)..(1 << 23)).contains(&offset) {
        return Err(CompilerError::from("RISC long branch exceeds signed i24"));
      }
      Ok(op_word(op) | ((offset.cast_unsigned() & 0x00FF_FFFF) << 8))
    }
    BranchKind::Conditional(op, register) => {
      let offset = i32::try_from(offset)
        .map_err(|_| CompilerError::from("RISC conditional branch exceeds signed i20"))?;
      encode_risc_cond_jump(op, register, offset)
        .ok_or_else(|| CompilerError::from("RISC conditional branch exceeds signed i20"))
    }
    BranchKind::Compare(condition, left, right) => {
      let offset = i16::try_from(offset)
        .map_err(|_| CompilerError::from("RISC compare branch exceeds signed i13"))?;
      encode_compare_jump(condition, left, right, offset)
        .ok_or_else(|| CompilerError::from("RISC compare branch exceeds signed i13"))
    }
  }
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
      | Op::DeleteProperty
      | Op::HasProperty
      | Op::GetGlobal
      | Op::TypeofGlobal
      | Op::SetGlobal
      | Op::GetUpvalue
      | Op::SetUpvalue
      | Op::Call
      | Op::TailCall
      | Op::Construct
      | Op::CallBuiltin
      | Op::Spread
      | Op::ArrayFromSpread
      | Op::MakeClosure
      | Op::GetThis
      | Op::GetNewTarget
      | Op::GetSuperProperty
      | Op::SetSuperProperty
      | Op::BuildArray
      | Op::BuildObject
      | Op::SuperConstruct
      | Op::Discard
      | Op::Copy
      | Op::Exchange
      | Op::Throw
      | Op::PushHandler
      | Op::PopHandler
      | Op::IteratorNext
      | Op::IteratorClose
      | Op::Return
      | Op::PushWithEnv
      | Op::PopEnv
      | Op::GeneratorInitDone
      | Op::Yield
      | Op::YieldStar
      | Op::AwaitPromise
      | Op::GetModuleExport
      | Op::SetModuleExport
      | Op::CloneLocalCell
      | Op::ResetLocalCell
      | Op::DeleteIdentifier
      | Op::GetBuiltin
      | Op::InstanceOf
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
  inputs: &[ValueId],
  line: u32,
) -> Result<(), CompilerError> {
  for (index, value) in inputs.iter().copied().enumerate() {
    let source = load_location(chunk, plan.get(value)?, 0, line);
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
    Op::SuperConstruct => count,
    _ => 0,
  }
}

fn add_branch_patch(
  builder: &RiscBuilder,
  old_ip: usize,
  lowered: &mut Chunk,
  line: u32,
  kind: BranchKind,
  patches: &mut Vec<BranchPatch>,
) -> Result<(), CompilerError> {
  let instruction = pending_instruction(builder, old_ip)?;
  let offset = instruction.branch_offset().ok_or_else(|| {
    CompilerError::from(format!("invalid branch at frontend instruction {old_ip}"))
  })?;
  let target = usize::try_from(old_ip as i64 + 1 + offset as i64)
    .map_err(|_| CompilerError::from("negative frontend branch target"))?;
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
  op: Op,
  payload: u32,
) -> Result<ValueShape, CompilerError> {
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
    | Op::GetBuiltin => ValueShape::new(0, true),

    Op::StoreLocal
    | Op::SetGlobal
    | Op::SetUpvalue
    | Op::PushWithEnv
    | Op::SetModuleExport
    | Op::Throw => ValueShape::new(1, false),

    Op::Neg | Op::Not | Op::BitNot | Op::Typeof => ValueShape::new(1, true),
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
    | Op::GetSuperProperty => ValueShape::new(2, true),

    Op::SetProperty | Op::SetSuperProperty => ValueShape::new(3, true),
    Op::DeleteIdentifier | Op::IteratorNext | Op::IteratorClose => ValueShape::new(1, true),
    Op::BuildArray => ValueShape::new(arg, true),
    Op::BuildObject => ValueShape::new(arg.saturating_mul(2), true),
    Op::Call | Op::TailCall | Op::Construct => ValueShape::new(arg + 1, true),
    Op::CallBuiltin => ValueShape::new(arg + 2, true),
    Op::SuperConstruct => ValueShape::new(arg, true),
    Op::Spread => ValueShape::new(3, true),
    Op::ArrayFromSpread => ValueShape::new(2, true),
    Op::JumpIfFalse | Op::JumpIfTrue | Op::JumpIfNullish => ValueShape::new(1, false),
    Op::CompareJump => ValueShape::new(2, false),
    Op::Return => ValueShape::new(0, false),
    // Generator resume supplies the expression result before the successor executes.
    Op::Yield | Op::YieldStar | Op::AwaitPromise => ValueShape::new(1, true),
    Op::CloneLocalCell
    | Op::ResetLocalCell
    | Op::Move
    | Op::Jump
    | Op::PushHandler
    | Op::PopHandler
    | Op::PopEnv
    | Op::GeneratorInitDone => ValueShape::new(0, false),
    Op::Discard | Op::Copy | Op::Exchange => {
      return Err(CompilerError::from(
        "virtual value lifecycle operation reached instruction emission",
      ));
    }
    Op::LoadSpill | Op::StoreSpill => {
      return Err(CompilerError::from(
        "RISC spill opcode encountered in frontend value IR",
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
  fn compiler_produces_only_verified_risc_chunks_recursively() {
    let source = crate::js!("function value(x) { try { return x + 1; } finally {} } value(2);");
    let session = Parser::parse_source(source).expect("parse");
    let chunk = compiler::compile(session.program(), session.source()).expect("compile");
    fn assert_risc(chunk: &Chunk) {
      assert_eq!(chunk.version, BytecodeVersion::RiscV2);
      chunk.verify().expect("verified RISC chunk");
      for word in &chunk.code {
        BytecodeOp::from_byte((word & 0xff) as u8).expect("public RISC opcode");
        assert!(!matches!(word & 0xff, 50..=53));
      }
      for function in &chunk.functions {
        assert_risc(&function.chunk);
      }
    }
    assert_risc(&chunk);
  }

  #[test]
  fn catch_destructuring_consumes_the_thrown_value_once() {
    for source in [
      "try { throw [1]; } catch ([value]) { value; }",
      "try { throw { value: 1 }; } catch ({ value }) { value; }",
    ] {
      let session = Parser::parse_source(source).expect("parse");
      let chunk = compiler::compile(session.program(), session.source()).expect("compile");
      chunk.verify().expect("verified RISC chunk");
    }
  }

  #[test]
  fn rejects_branch_offsets_before_narrowing() {
    let conditional = encode_branch_patch(BranchKind::Conditional(Op::JumpIfFalse, 0), 1_i64 << 32)
      .expect_err("offset must not wrap to i32");
    assert!(conditional.message.contains("signed i20"));

    let comparison = encode_branch_patch(BranchKind::Compare(0, 0, 0), 1_i64 << 16)
      .expect_err("offset must not wrap to i16");
    assert!(comparison.message.contains("signed i13"));
  }
}
