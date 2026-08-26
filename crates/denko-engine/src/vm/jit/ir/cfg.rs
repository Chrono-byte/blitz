// SPDX-License-Identifier: MPL-2.0

//! Verified control-flow semantic IR.
//!
//! This lives beside the proven numeric-leaf adapter. It deliberately accepts
//! only register-local numeric control flow. VM locals remain unsupported
//! until their cell identity, mapped-arguments behavior, and deoptimization
//! effects have an explicit representation.

use super::{CompilerChunk, IrError, opcode};
use crate::bytecode::{Op, decode_compare_jump, decode_rd, decode_rs};
use std::collections::{BTreeSet, HashMap, VecDeque};

const REGISTER_COUNT: usize = 16;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct BlockId(u32);

impl BlockId {
  fn from_index(index: usize) -> Result<Self, CfgError> {
    u32::try_from(index)
      .map(Self)
      .map_err(|_| CfgError::TooLarge)
  }

  fn index(self) -> usize {
    self.0 as usize
  }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct CfgValueId(u32);

impl CfgValueId {
  fn from_index(index: usize) -> Result<Self, CfgError> {
    u32::try_from(index)
      .map(Self)
      .map_err(|_| CfgError::TooLarge)
  }

  fn index(self) -> usize {
    self.0 as usize
  }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct InstructionId(u32);

impl InstructionId {
  fn from_index(index: usize) -> Result<Self, CfgError> {
    u32::try_from(index)
      .map(Self)
      .map_err(|_| CfgError::TooLarge)
  }

  fn index(self) -> usize {
    self.0 as usize
  }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct FrameStateId(u32);

impl FrameStateId {
  fn from_index(index: usize) -> Result<Self, CfgError> {
    u32::try_from(index)
      .map(Self)
      .map_err(|_| CfgError::TooLarge)
  }

  fn index(self) -> usize {
    self.0 as usize
  }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct BytecodeOrigin {
  pub(crate) pc: u32,
}

impl BytecodeOrigin {
  fn from_pc(pc: usize) -> Result<Self, CfgError> {
    Ok(Self {
      pc: u32::try_from(pc).map_err(|_| CfgError::TooLarge)?,
    })
  }

  fn index(self) -> usize {
    self.pc as usize
  }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct Register(u8);

impl Register {
  fn from_encoded(register: u8) -> Result<Self, CfgError> {
    (usize::from(register) < REGISTER_COUNT)
      .then_some(Self(register))
      .ok_or(CfgError::InvalidRegister)
  }

  fn index(self) -> usize {
    usize::from(self.0)
  }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ValueRepresentation {
  Tagged,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ValueDefinition {
  EntryRegister {
    register: Register,
    block: BlockId,
  },
  BlockParameter {
    register: Register,
    block: BlockId,
    parameter: u8,
  },
  Instruction {
    block: BlockId,
    instruction: InstructionId,
  },
}

impl ValueDefinition {
  fn block(self) -> BlockId {
    match self {
      Self::EntryRegister { block, .. }
      | Self::BlockParameter { block, .. }
      | Self::Instruction { block, .. } => block,
    }
  }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ValueData {
  pub(crate) definition: ValueDefinition,
  pub(crate) representation: ValueRepresentation,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct BlockParameter {
  pub(crate) register: Register,
  pub(crate) value: CfgValueId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SemanticInstruction {
  Copy {
    input: CfgValueId,
  },
  JsNumericBinary {
    operation: super::super::NumericBinaryOp,
    left: CfgValueId,
    right: CfgValueId,
  },
}

impl SemanticInstruction {
  fn operands(self) -> [Option<CfgValueId>; 2] {
    match self {
      Self::Copy { input } => [Some(input), None],
      Self::JsNumericBinary { left, right, .. } => [Some(left), Some(right)],
    }
  }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct InstructionData {
  pub(crate) result: CfgValueId,
  pub(crate) operation: SemanticInstruction,
  pub(crate) origin: BytecodeOrigin,
  pub(crate) state_before: FrameStateId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Edge {
  pub(crate) target: BlockId,
  pub(crate) arguments: Box<[CfgValueId]>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Terminator {
  Jump {
    edge: Edge,
    origin: BytecodeOrigin,
    state: FrameStateId,
  },
  CompareBranch {
    condition: u8,
    left: CfgValueId,
    right: CfgValueId,
    if_true: Edge,
    if_false: Edge,
    origin: BytecodeOrigin,
    state: FrameStateId,
  },
  Return {
    value: CfgValueId,
    origin: BytecodeOrigin,
    state: FrameStateId,
  },
}

impl Terminator {
  fn edges(&self) -> [Option<&Edge>; 2] {
    match self {
      Self::Jump { edge, .. } => [Some(edge), None],
      Self::CompareBranch {
        if_true, if_false, ..
      } => [Some(if_true), Some(if_false)],
      Self::Return { .. } => [None, None],
    }
  }

  fn operands(&self) -> [Option<CfgValueId>; 2] {
    match self {
      Self::Jump { .. } => [None, None],
      Self::CompareBranch { left, right, .. } => [Some(*left), Some(*right)],
      Self::Return { value, .. } => [Some(*value), None],
    }
  }

  fn origin(&self) -> BytecodeOrigin {
    match self {
      Self::Jump { origin, .. }
      | Self::CompareBranch { origin, .. }
      | Self::Return { origin, .. } => *origin,
    }
  }

  fn state(&self) -> FrameStateId {
    match self {
      Self::Jump { state, .. } | Self::CompareBranch { state, .. } | Self::Return { state, .. } => {
        *state
      }
    }
  }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct BasicBlock {
  pub(crate) origin: BytecodeOrigin,
  pub(crate) parameters: Box<[BlockParameter]>,
  pub(crate) instructions: Box<[InstructionId]>,
  pub(crate) terminator: Terminator,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct FrameState {
  pub(crate) resume_at: BytecodeOrigin,
  pub(crate) registers: [CfgValueId; REGISTER_COUNT],
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SemanticCfg {
  entry: BlockId,
  blocks: Box<[BasicBlock]>,
  instructions: Box<[InstructionData]>,
  values: Box<[ValueData]>,
  frame_states: Box<[FrameState]>,
  code_len: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct VerifiedSemanticCfg(SemanticCfg);

impl VerifiedSemanticCfg {
  pub(crate) fn decode_numeric_control_flow(snapshot: &CompilerChunk) -> Result<Self, CfgError> {
    let candidate = Decoder::new(snapshot)?.decode()?;
    Self::verify(candidate)
  }

  fn verify(candidate: SemanticCfg) -> Result<Self, CfgError> {
    if candidate.blocks.is_empty()
      || candidate.entry.index() >= candidate.blocks.len()
      || candidate.code_len == 0
    {
      return Err(CfgError::InvalidGraph);
    }
    let mut predecessors = vec![0_usize; candidate.blocks.len()];
    for (block_index, block) in candidate.blocks.iter().enumerate() {
      let block_id = BlockId::from_index(block_index)?;
      if block.origin.index() >= candidate.code_len as usize
        || block.parameters.len() != REGISTER_COUNT
      {
        return Err(CfgError::InvalidGraph);
      }
      let mut parameter_registers = [false; REGISTER_COUNT];
      for (parameter_index, parameter) in block.parameters.iter().enumerate() {
        if parameter.value.index() >= candidate.values.len()
          || parameter_registers[parameter.register.index()]
        {
          return Err(CfgError::InvalidGraph);
        }
        parameter_registers[parameter.register.index()] = true;
        let expected = if block_id == candidate.entry {
          ValueDefinition::EntryRegister {
            register: parameter.register,
            block: block_id,
          }
        } else {
          ValueDefinition::BlockParameter {
            register: parameter.register,
            block: block_id,
            parameter: u8::try_from(parameter_index).map_err(|_| CfgError::TooLarge)?,
          }
        };
        if candidate.values[parameter.value.index()].definition != expected {
          return Err(CfgError::InvalidGraph);
        }
      }
      for (order, instruction_id) in block.instructions.iter().copied().enumerate() {
        let instruction = candidate
          .instructions
          .get(instruction_id.index())
          .ok_or(CfgError::InvalidGraph)?;
        if instruction.origin.index() >= candidate.code_len as usize
          || instruction.state_before.index() >= candidate.frame_states.len()
          || instruction.result.index() >= candidate.values.len()
          || candidate.values[instruction.result.index()].definition
            != (ValueDefinition::Instruction {
              block: block_id,
              instruction: instruction_id,
            })
        {
          return Err(CfgError::InvalidGraph);
        }
        if candidate.frame_states[instruction.state_before.index()].resume_at != instruction.origin
        {
          return Err(CfgError::InvalidGraph);
        }
        for operand in instruction
          .operation
          .operands()
          .into_iter()
          .flatten()
        {
          verify_local_use(&candidate, block_id, operand, order)?;
        }
        verify_frame_state(&candidate, block_id, instruction.state_before, order)?;
      }
      let terminator_order = block.instructions.len();
      if block.terminator.origin().index() >= candidate.code_len as usize
        || block.terminator.state().index() >= candidate.frame_states.len()
      {
        return Err(CfgError::InvalidGraph);
      }
      if candidate.frame_states[block.terminator.state().index()].resume_at
        != block.terminator.origin()
      {
        return Err(CfgError::InvalidGraph);
      }
      for operand in block.terminator.operands().into_iter().flatten() {
        verify_local_use(&candidate, block_id, operand, terminator_order)?;
      }
      verify_frame_state(
        &candidate,
        block_id,
        block.terminator.state(),
        terminator_order,
      )?;
      for edge in block.terminator.edges().into_iter().flatten() {
        let target = candidate
          .blocks
          .get(edge.target.index())
          .ok_or(CfgError::InvalidGraph)?;
        if edge.target == candidate.entry || edge.arguments.len() != target.parameters.len() {
          return Err(CfgError::InvalidGraph);
        }
        predecessors[edge.target.index()] = predecessors[edge.target.index()].saturating_add(1);
        for (argument, parameter) in edge
          .arguments
          .iter()
          .copied()
          .zip(target.parameters.iter())
        {
          verify_local_use(&candidate, block_id, argument, terminator_order)?;
          if candidate.values[argument.index()].representation
            != candidate.values[parameter.value.index()].representation
          {
            return Err(CfgError::InvalidGraph);
          }
        }
      }
    }
    if predecessors[candidate.entry.index()] != 0
      || predecessors
        .iter()
        .enumerate()
        .any(|(index, count)| index != candidate.entry.index() && *count == 0)
    {
      return Err(CfgError::InvalidGraph);
    }
    Ok(Self(candidate))
  }

  #[cfg(test)]
  fn blocks(&self) -> &[BasicBlock] {
    &self.0.blocks
  }

  #[cfg(test)]
  fn instructions(&self) -> &[InstructionData] {
    &self.0.instructions
  }
}

fn verify_local_use(
  candidate: &SemanticCfg,
  block: BlockId,
  value: CfgValueId,
  before_order: usize,
) -> Result<(), CfgError> {
  let definition = candidate
    .values
    .get(value.index())
    .ok_or(CfgError::InvalidGraph)?
    .definition;
  if definition.block() != block {
    return Err(CfgError::InvalidGraph);
  }
  if let ValueDefinition::Instruction { instruction, .. } = definition {
    let order = candidate.blocks[block.index()]
      .instructions
      .iter()
      .position(|id| *id == instruction)
      .ok_or(CfgError::InvalidGraph)?;
    if order >= before_order {
      return Err(CfgError::InvalidGraph);
    }
  }
  Ok(())
}

fn verify_frame_state(
  candidate: &SemanticCfg,
  block: BlockId,
  state: FrameStateId,
  before_order: usize,
) -> Result<(), CfgError> {
  let state = candidate
    .frame_states
    .get(state.index())
    .ok_or(CfgError::InvalidGraph)?;
  if state.resume_at.index() >= candidate.code_len as usize {
    return Err(CfgError::InvalidGraph);
  }
  for value in state.registers {
    verify_local_use(candidate, block, value, before_order)?;
  }
  Ok(())
}

struct Decoder<'a> {
  snapshot: &'a CompilerChunk,
  ranges: Vec<(usize, usize)>,
  reachable: Vec<usize>,
  remap: HashMap<usize, BlockId>,
  blocks: Vec<BasicBlock>,
  instructions: Vec<InstructionData>,
  values: Vec<ValueData>,
  frame_states: Vec<FrameState>,
}

impl<'a> Decoder<'a> {
  fn new(snapshot: &'a CompilerChunk) -> Result<Self, CfgError> {
    if snapshot.code.is_empty() {
      return Err(CfgError::UnsupportedBody);
    }
    let ranges = discover_ranges(&snapshot.code)?;
    let reachable = reachable_ranges(&snapshot.code, &ranges)?;
    let remap = reachable
      .iter()
      .enumerate()
      .map(|(new, old)| Ok((*old, BlockId::from_index(new)?)))
      .collect::<Result<HashMap<_, _>, CfgError>>()?;
    Ok(Self {
      snapshot,
      ranges,
      reachable,
      remap,
      blocks: Vec::new(),
      instructions: Vec::new(),
      values: Vec::new(),
      frame_states: Vec::new(),
    })
  }

  fn decode(mut self) -> Result<SemanticCfg, CfgError> {
    for new_index in 0..self.reachable.len() {
      self.decode_block(new_index)?;
    }
    Ok(SemanticCfg {
      entry: BlockId(0),
      blocks: self.blocks.into_boxed_slice(),
      instructions: self.instructions.into_boxed_slice(),
      values: self.values.into_boxed_slice(),
      frame_states: self.frame_states.into_boxed_slice(),
      code_len: u32::try_from(self.snapshot.code.len()).map_err(|_| CfgError::TooLarge)?,
    })
  }

  fn decode_block(
    &mut self,
    new_index: usize,
  ) -> Result<(), CfgError> {
    let old_index = self.reachable[new_index];
    let block = BlockId::from_index(new_index)?;
    let (start, end) = self.ranges[old_index];
    let mut state = [CfgValueId(0); REGISTER_COUNT];
    let mut parameters = Vec::with_capacity(REGISTER_COUNT);
    for (register_index, state_slot) in state.iter_mut().enumerate() {
      let register = Register::from_encoded(register_index as u8)?;
      let value = CfgValueId::from_index(self.values.len())?;
      let definition = if block == BlockId(0) {
        ValueDefinition::EntryRegister { register, block }
      } else {
        ValueDefinition::BlockParameter {
          register,
          block,
          parameter: register_index as u8,
        }
      };
      self.values.push(ValueData {
        definition,
        representation: ValueRepresentation::Tagged,
      });
      *state_slot = value;
      parameters.push(BlockParameter { register, value });
    }
    let mut instruction_ids = Vec::new();
    for pc in start..end.saturating_sub(1) {
      let word = self.snapshot.code[pc];
      let operation = match opcode(word) {
        Op::Move => SemanticInstruction::Copy {
          input: state[usize::from(decode_rs(word))],
        },
        Op::Add | Op::Sub | Op::Mul => {
          let operation = match opcode(word) {
            Op::Add => super::super::NumericBinaryOp::Add,
            Op::Sub => super::super::NumericBinaryOp::Subtract,
            Op::Mul => super::super::NumericBinaryOp::Multiply,
            _ => unreachable!(),
          };
          SemanticInstruction::JsNumericBinary {
            operation,
            left: state[usize::from(decode_rd(word))],
            right: state[usize::from(decode_rs(word))],
          }
        }
        _ => {
          return Err(CfgError::UnsupportedOpcode {
            pc,
            op: opcode(word),
          });
        }
      };
      let destination = Register::from_encoded(decode_rd(word))?;
      let state_before = self.push_state(pc, state)?;
      let instruction = InstructionId::from_index(self.instructions.len())?;
      let result = CfgValueId::from_index(self.values.len())?;
      self.values.push(ValueData {
        definition: ValueDefinition::Instruction { block, instruction },
        representation: ValueRepresentation::Tagged,
      });
      self.instructions.push(InstructionData {
        result,
        operation,
        origin: BytecodeOrigin::from_pc(pc)?,
        state_before,
      });
      state[destination.index()] = result;
      instruction_ids.push(instruction);
    }
    let terminal_pc = end.checked_sub(1).ok_or(CfgError::InvalidGraph)?;
    let terminal_word = self.snapshot.code[terminal_pc];
    let terminal_state = self.push_state(terminal_pc, state)?;
    let origin = BytecodeOrigin::from_pc(terminal_pc)?;
    let terminator = match opcode(terminal_word) {
      Op::Jump => Terminator::Jump {
        edge: self.edge_for(
          old_index,
          branch_target(terminal_pc, jump_offset(terminal_word))?,
          state,
        )?,
        origin,
        state: terminal_state,
      },
      Op::CompareJump => {
        let (condition, left, right, offset) = decode_compare_jump(terminal_word);
        Terminator::CompareBranch {
          condition,
          left: state[usize::from(left)],
          right: state[usize::from(right)],
          if_true: self.edge_for(old_index, terminal_pc + 1, state)?,
          if_false: self.edge_for(
            old_index,
            branch_target(terminal_pc, i32::from(offset))?,
            state,
          )?,
          origin,
          state: terminal_state,
        }
      }
      Op::Return => Terminator::Return {
        value: state[usize::from(decode_rd(terminal_word))],
        origin,
        state: terminal_state,
      },
      _ => {
        return Err(CfgError::UnsupportedOpcode {
          pc: terminal_pc,
          op: opcode(terminal_word),
        });
      }
    };
    self.blocks.push(BasicBlock {
      origin: BytecodeOrigin::from_pc(start)?,
      parameters: parameters.into_boxed_slice(),
      instructions: instruction_ids.into_boxed_slice(),
      terminator,
    });
    Ok(())
  }

  fn push_state(
    &mut self,
    pc: usize,
    registers: [CfgValueId; REGISTER_COUNT],
  ) -> Result<FrameStateId, CfgError> {
    let id = FrameStateId::from_index(self.frame_states.len())?;
    self.frame_states.push(FrameState {
      resume_at: BytecodeOrigin::from_pc(pc)?,
      registers,
    });
    Ok(id)
  }

  fn edge_for(
    &self,
    source_old: usize,
    target_pc: usize,
    state: [CfgValueId; REGISTER_COUNT],
  ) -> Result<Edge, CfgError> {
    let target_old = self
      .ranges
      .iter()
      .position(|(start, _)| *start == target_pc)
      .ok_or(CfgError::InvalidBranch)?;
    if source_old == target_old {
      return Err(CfgError::UnsupportedControlFlow);
    }
    let target = *self
      .remap
      .get(&target_old)
      .ok_or(CfgError::InvalidBranch)?;
    Ok(Edge {
      target,
      arguments: state.into_iter().collect(),
    })
  }
}

fn discover_ranges(code: &[u32]) -> Result<Vec<(usize, usize)>, CfgError> {
  let mut leaders = BTreeSet::from([0_usize]);
  for (pc, word) in code.iter().copied().enumerate() {
    match opcode(word) {
      Op::Jump => {
        let target = branch_target(pc, jump_offset(word))?;
        if target >= code.len() {
          return Err(CfgError::InvalidBranch);
        }
        leaders.insert(target);
        if pc + 1 < code.len() {
          leaders.insert(pc + 1);
        }
      }
      Op::CompareJump => {
        let target = branch_target(pc, i32::from(decode_compare_jump(word).3))?;
        if target >= code.len() {
          return Err(CfgError::InvalidBranch);
        }
        leaders.insert(target);
        if pc + 1 < code.len() {
          leaders.insert(pc + 1);
        }
      }
      Op::Return | Op::Throw if pc + 1 < code.len() => {
        leaders.insert(pc + 1);
      }
      _ => {}
    }
  }
  let leaders: Vec<_> = leaders.into_iter().collect();
  Ok(
    leaders
      .iter()
      .copied()
      .enumerate()
      .map(|(index, start)| {
        (
          start,
          leaders
            .get(index + 1)
            .copied()
            .unwrap_or(code.len()),
        )
      })
      .collect(),
  )
}

fn reachable_ranges(
  code: &[u32],
  ranges: &[(usize, usize)],
) -> Result<Vec<usize>, CfgError> {
  let starts: HashMap<_, _> = ranges
    .iter()
    .enumerate()
    .map(|(index, (start, _))| (*start, index))
    .collect();
  let mut reachable = BTreeSet::new();
  let mut work = VecDeque::from([0_usize]);
  while let Some(index) = work.pop_front() {
    if !reachable.insert(index) {
      continue;
    }
    let (_, end) = ranges
      .get(index)
      .copied()
      .ok_or(CfgError::InvalidGraph)?;
    let pc = end.checked_sub(1).ok_or(CfgError::InvalidGraph)?;
    let word = code[pc];
    let mut enqueue = |target: usize| -> Result<(), CfgError> {
      work.push_back(
        *starts
          .get(&target)
          .ok_or(CfgError::InvalidBranch)?,
      );
      Ok(())
    };
    match opcode(word) {
      Op::Jump => enqueue(branch_target(pc, jump_offset(word))?)?,
      Op::CompareJump => {
        enqueue(branch_target(pc, i32::from(decode_compare_jump(word).3))?)?;
        enqueue(pc + 1)?;
      }
      Op::Return | Op::Throw => {}
      _ => {
        if pc + 1 >= code.len() {
          return Err(CfgError::MissingTerminator);
        }
        enqueue(pc + 1)?;
      }
    }
  }
  Ok(reachable.into_iter().collect())
}

fn jump_offset(word: u32) -> i32 {
  let payload = ((word >> 8) & 0x00ff_ffff) as i32;
  if payload & 0x0080_0000 != 0 {
    payload | !0x00ff_ffff
  } else {
    payload
  }
}

fn branch_target(
  pc: usize,
  offset: i32,
) -> Result<usize, CfgError> {
  usize::try_from(pc as i64 + 1 + i64::from(offset)).map_err(|_| CfgError::InvalidBranch)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum CfgError {
  UnsupportedBody,
  UnsupportedOpcode { pc: usize, op: Op },
  UnsupportedControlFlow,
  InvalidBranch,
  MissingTerminator,
  InvalidRegister,
  InvalidGraph,
  TooLarge,
}

impl From<IrError> for CfgError {
  fn from(error: IrError) -> Self {
    match error {
      IrError::UnsupportedBody => Self::UnsupportedBody,
      IrError::TooManyParameters | IrError::TooManyValues => Self::TooLarge,
      IrError::InvalidSsa => Self::InvalidGraph,
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::bytecode::{encode_compare_jump, encode_risc_registers};

  fn jump(offset: i32) -> u32 {
    Op::Jump as u32 | (((offset as u32) & 0x00ff_ffff) << 8)
  }

  fn register_loop_snapshot() -> CompilerChunk {
    CompilerChunk {
      code: vec![
        jump(0),
        encode_compare_jump(2, 0, 1, 2).expect("branch"),
        encode_risc_registers(Op::Add, 2, 3),
        jump(-3),
        encode_risc_registers(Op::Return, 2, 0),
        Op::Undefined as u32,
        encode_risc_registers(Op::Return, 0, 0),
      ]
      .into_boxed_slice(),
      parameter_count: 4,
    }
  }

  #[test]
  fn decodes_a_register_loop_with_block_parameters_and_ignores_unreachable_epilogue() {
    let cfg = VerifiedSemanticCfg::decode_numeric_control_flow(&register_loop_snapshot())
      .expect("verified semantic CFG");
    assert_eq!(cfg.blocks().len(), 4);
    assert_eq!(cfg.blocks()[1].parameters.len(), REGISTER_COUNT);
    assert!(matches!(
      cfg.blocks()[1].terminator,
      Terminator::CompareBranch { .. }
    ));
    assert_eq!(cfg.instructions().len(), 1);
    assert_eq!(cfg.instructions()[0].origin.pc, 2);
  }

  #[test]
  fn rejects_observable_local_operations() {
    let mut snapshot = register_loop_snapshot();
    snapshot.code[2] = crate::bytecode::encode_risc_aux16(Op::StoreLocal, 2, 0);
    assert_eq!(
      VerifiedSemanticCfg::decode_numeric_control_flow(&snapshot),
      Err(CfgError::UnsupportedOpcode {
        pc: 2,
        op: Op::StoreLocal,
      })
    );
  }

  #[test]
  fn rejects_a_cross_block_value_that_does_not_arrive_as_an_argument() {
    let verified = VerifiedSemanticCfg::decode_numeric_control_flow(&register_loop_snapshot())
      .expect("verified CFG");
    let mut candidate = verified.0;
    let foreign = candidate.blocks[0].parameters[0].value;
    let return_block = candidate.blocks.len() - 1;
    let Terminator::Return { value, .. } = &mut candidate.blocks[return_block].terminator else {
      panic!("return block");
    };
    *value = foreign;
    assert_eq!(
      VerifiedSemanticCfg::verify(candidate),
      Err(CfgError::InvalidGraph)
    );
  }

  #[test]
  fn rejects_an_edge_with_the_wrong_block_argument_count() {
    let verified = VerifiedSemanticCfg::decode_numeric_control_flow(&register_loop_snapshot())
      .expect("verified CFG");
    let mut candidate = verified.0;
    let Terminator::Jump { edge, .. } = &mut candidate.blocks[0].terminator else {
      panic!("entry jump");
    };
    edge.arguments = Box::new([]);
    assert_eq!(
      VerifiedSemanticCfg::verify(candidate),
      Err(CfgError::InvalidGraph)
    );
  }

  #[test]
  fn rejects_a_same_block_use_before_definition() {
    let verified = VerifiedSemanticCfg::decode_numeric_control_flow(&register_loop_snapshot())
      .expect("verified CFG");
    let mut candidate = verified.0;
    let instruction = candidate.blocks[2].instructions[0];
    let result = candidate.instructions[instruction.index()].result;
    let SemanticInstruction::JsNumericBinary { left, .. } =
      &mut candidate.instructions[instruction.index()].operation
    else {
      panic!("numeric instruction");
    };
    *left = result;
    assert_eq!(
      VerifiedSemanticCfg::verify(candidate),
      Err(CfgError::InvalidGraph)
    );
  }

  #[test]
  fn cfg_snapshot_and_verified_graph_are_send_and_static() {
    fn assert_send_static<T: Send + 'static>() {}
    assert_send_static::<CompilerChunk>();
    assert_send_static::<VerifiedSemanticCfg>();
  }
}
