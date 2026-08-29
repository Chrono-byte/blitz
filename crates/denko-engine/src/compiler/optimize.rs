// SPDX-License-Identifier: MPL-2.0
//! Optimization passes for the Lowering.

use super::bytecode::{Lowering, VirtualInputs, VirtualInstruction, VirtualValue};
use super::emit::LoweringOpcode;
use crate::bytecode::{Chunk, Constant};
use std::collections::{HashMap, HashSet};

pub(crate) fn optimize(
  builder: &mut Lowering,
  code: &mut Chunk,
) -> usize {
  let mut total = 0;
  total += constant_fold(builder, code);
  total += dead_code_elimination(builder, code);
  total
}

fn constant_fold(
  builder: &mut Lowering,
  code: &mut Chunk,
) -> usize {
  if builder
    .instructions
    .iter()
    .any(|instruction| instruction.opcode.is_jump())
  {
    return 0;
  }
  let mut value_to_const: HashMap<usize, Option<f64>> = HashMap::new();
  let mut folded = 0;

  for idx in 0..builder.instructions.len() {
    let op = builder.instructions[idx].opcode;
    let inputs = builder.instructions[idx]
      .inputs
      .as_slice()
      .to_vec();
    let output = builder.instructions[idx].output;

    let folded_value = match op {
      LoweringOpcode::Sub | LoweringOpcode::Mul | LoweringOpcode::Div => {
        if inputs.len() != 2 {
          None
        } else {
          let left = value_to_const.get(&inputs[0].0).and_then(|v| *v);
          let right = value_to_const.get(&inputs[1].0).and_then(|v| *v);
          match (left, right) {
            (Some(l), Some(r)) => match op {
              LoweringOpcode::Sub => Some(l - r),
              LoweringOpcode::Mul => Some(l * r),
              LoweringOpcode::Div => Some(l / r),
              _ => None,
            },
            _ => None,
          }
        }
      }
      LoweringOpcode::Neg => {
        if inputs.len() != 1 {
          None
        } else {
          value_to_const
            .get(&inputs[0].0)
            .and_then(|v| *v)
            .map(|x| -x)
        }
      }
      _ => None,
    };

    if let Some(result) = folded_value {
      let constant = Constant::Number(result);
      let const_idx = code.add_constant(constant);
      let Some(payload) = u32::try_from(const_idx)
        .ok()
        .filter(|index| *index <= u32::from(u16::MAX))
      else {
        code.constants.pop();
        if let Some(out) = output {
          value_to_const.insert(out.0, None);
        }
        continue;
      };
      let inst = &mut builder.instructions[idx];
      inst.opcode = LoweringOpcode::Const;
      inst.payload = payload;
      inst.inputs = VirtualInputs::None;
      if let Some(out) = output {
        value_to_const.insert(out.0, Some(result));
      }
      folded += 1;
    } else {
      if let Some(out) = output {
        let const_val = if op == LoweringOpcode::Const {
          let idx_const = (builder.instructions[idx].payload & 0xffff) as usize;
          if let Some(Constant::Number(n)) = code.constants.get(idx_const) {
            Some(*n)
          } else {
            None
          }
        } else {
          None
        };
        value_to_const.insert(out.0, const_val);
      }
    }
  }
  folded
}

#[allow(dead_code)]
fn common_subexpression_elimination(builder: &mut Lowering) -> usize {
  fn is_pure(opcode: LoweringOpcode) -> bool {
    matches!(
      opcode,
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
        | LoweringOpcode::Neg
        | LoweringOpcode::BitNot
        | LoweringOpcode::Const
    )
  }

  let is_commutative = |opcode: LoweringOpcode| {
    matches!(
      opcode,
      LoweringOpcode::Add
        | LoweringOpcode::Mul
        | LoweringOpcode::BitAnd
        | LoweringOpcode::BitOr
        | LoweringOpcode::BitXor
    )
  };

  let mut map: HashMap<(u32, Vec<usize>), usize> = HashMap::new();
  let mut remap: HashMap<usize, usize> = HashMap::new();
  let mut to_remove = HashSet::new();
  let mut eliminated = 0;

  let op_key = |opcode: LoweringOpcode| -> u32 {
    match opcode {
      LoweringOpcode::Bytecode(b) => b as u32,
      LoweringOpcode::Discard => 0xFF00,
      LoweringOpcode::Copy => 0xFF01,
      LoweringOpcode::Exchange => 0xFF02,
    }
  };

  for idx in 0..builder.instructions.len() {
    let inst = &builder.instructions[idx];
    if !is_pure(inst.opcode) {
      continue;
    }
    let key: (u32, Vec<usize>) = if inst.opcode == LoweringOpcode::Const {
      (op_key(LoweringOpcode::Const), vec![inst.payload as usize])
    } else {
      let mut inputs: Vec<usize> = inst
        .inputs
        .as_slice()
        .iter()
        .map(|v| {
          let mut id = v.0;
          while let Some(&mapped) = remap.get(&id) {
            id = mapped;
          }
          id
        })
        .collect();
      if is_commutative(inst.opcode) {
        inputs.sort_unstable();
      }
      (op_key(inst.opcode), inputs)
    };

    if let Some(&existing_output) = map.get(&key) {
      if let Some(output) = inst.output {
        remap.insert(output.0, existing_output);
        to_remove.insert(idx);
        eliminated += 1;
      }
    } else if let Some(output) = inst.output {
      map.insert(key, output.0);
    } else {
      map.insert(key, usize::MAX);
    }
  }

  if eliminated == 0 {
    return 0;
  }

  let len = builder.instructions.len();
  let mut old_to_new = vec![None; len];
  let mut new_idx = 0;
  for (old_idx, is_removed) in (0..len).map(|i| (i, to_remove.contains(&i))) {
    if !is_removed {
      old_to_new[old_idx] = Some(new_idx);
      new_idx += 1;
    }
  }

  let mut new_instructions = Vec::with_capacity(len - eliminated);
  let mut new_lines = Vec::with_capacity(len - eliminated);

  for old_idx in 0..len {
    if to_remove.contains(&old_idx) {
      continue;
    }
    let mut inst = std::mem::replace(
      &mut builder.instructions[old_idx],
      VirtualInstruction {
        opcode: LoweringOpcode::Const,
        payload: 0,
        branch_offset: None,
        inputs: VirtualInputs::None,
        output: None,
      },
    );
    let inputs = inst.inputs.as_slice().to_vec();
    let mut new_inputs = Vec::with_capacity(inputs.len());
    for v in inputs {
      let mut id = v.0;
      while let Some(&mapped) = remap.get(&id) {
        id = mapped;
      }
      new_inputs.push(VirtualValue(id));
    }
    inst.inputs = VirtualInputs::from_slice(&new_inputs);
    if let Some(offset) = inst.branch_offset {
      let old_target = (old_idx as i64 + 1 + offset as i64) as usize;
      if old_target < len
        && let Some(new_target) = old_to_new[old_target]
      {
        let new_offset = new_target as i64 - (new_instructions.len() as i64 + 1);
        inst.branch_offset = Some(new_offset as i32);
      }
    }
    new_instructions.push(inst);
    new_lines.push(builder.lines[old_idx]);
  }

  let mut new_branch_values = HashMap::new();
  for (old_pc, values) in std::mem::take(&mut builder.branch_values) {
    if let Some(new_pc) = old_to_new[old_pc] {
      let remapped_values = values
        .into_iter()
        .map(|v| {
          let mut id = v.0;
          while let Some(&mapped) = remap.get(&id) {
            id = mapped;
          }
          VirtualValue(id)
        })
        .collect();
      new_branch_values.insert(new_pc, remapped_values);
    }
  }
  builder.branch_values = new_branch_values;
  builder.instructions = new_instructions;
  builder.lines = new_lines;

  eliminated
}

fn dead_code_elimination(
  builder: &mut Lowering,
  code: &mut Chunk,
) -> usize {
  let len = builder.instructions.len();
  if len == 0 {
    return 0;
  }
  let mut reachable = vec![false; len];
  let mut worklist = vec![0];
  reachable[0] = true;
  for handler in &code.finally_handlers {
    if handler.push_pc < len {
      reachable[handler.push_pc] = true;
      worklist.push(handler.push_pc);
    }
    if handler.finalizer_pc < len {
      reachable[handler.finalizer_pc] = true;
      worklist.push(handler.finalizer_pc);
    }
  }
  let mut visited = HashSet::new();
  while let Some(pc) = worklist.pop() {
    if !visited.insert(pc) {
      continue;
    }
    if pc >= len {
      continue;
    }
    let inst = &builder.instructions[pc];
    let branch = inst.branch_offset;
    match inst.opcode {
      LoweringOpcode::Jump => {
        if let Some(offset) = branch
          && let Ok(target) = usize::try_from(pc as i64 + 1 + offset as i64)
          && target < len
          && !reachable[target]
        {
          reachable[target] = true;
          worklist.push(target);
        }
      }
      LoweringOpcode::JumpIfFalse
      | LoweringOpcode::JumpIfTrue
      | LoweringOpcode::JumpIfNullish
      | LoweringOpcode::CompareJump
      | LoweringOpcode::PushHandler => {
        if let Some(offset) = branch
          && let Ok(target) = usize::try_from(pc as i64 + 1 + offset as i64)
          && target < len
          && !reachable[target]
        {
          reachable[target] = true;
          worklist.push(target);
        }
        if pc + 1 < len && !reachable[pc + 1] {
          reachable[pc + 1] = true;
          worklist.push(pc + 1);
        }
      }
      LoweringOpcode::Return | LoweringOpcode::Throw => {}
      _ => {
        if pc + 1 < len && !reachable[pc + 1] {
          reachable[pc + 1] = true;
          worklist.push(pc + 1);
        }
      }
    }
  }

  let reachable_count = reachable.iter().filter(|&&r| r).count();
  if reachable_count == len {
    return 0;
  }

  let mut old_to_new = vec![None; len];
  let mut new_idx = 0;
  for (old_idx, &is_reach) in reachable.iter().enumerate() {
    if is_reach {
      old_to_new[old_idx] = Some(new_idx);
      new_idx += 1;
    }
  }

  let mut new_instructions = Vec::with_capacity(reachable_count);
  let mut new_lines = Vec::with_capacity(reachable_count);
  let mut new_to_old = Vec::with_capacity(reachable_count);
  for (old_idx, &is_reach) in reachable.iter().enumerate() {
    if is_reach {
      new_to_old.push(old_idx);
    }
  }

  for &old_idx in &new_to_old {
    let mut inst = std::mem::replace(
      &mut builder.instructions[old_idx],
      VirtualInstruction {
        opcode: LoweringOpcode::Const,
        payload: 0,
        branch_offset: None,
        inputs: VirtualInputs::None,
        output: None,
      },
    );
    if let Some(old_offset) = inst.branch_offset {
      let old_target = (old_idx as i64 + 1 + old_offset as i64) as usize;
      if old_target < len
        && let Some(new_target) = old_to_new[old_target]
      {
        let new_pc = new_instructions.len();
        let new_offset = new_target as i64 - (new_pc as i64 + 1);
        inst.branch_offset = Some(new_offset as i32);
      }
    }
    new_instructions.push(inst);
    new_lines.push(builder.lines[old_idx]);
  }

  let mut new_branch_values = HashMap::new();
  for (old_pc, values) in std::mem::take(&mut builder.branch_values) {
    if let Some(new_pc) = old_to_new[old_pc] {
      new_branch_values.insert(new_pc, values);
    }
  }
  builder.branch_values = new_branch_values;

  let mut new_handlers = Vec::with_capacity(code.finally_handlers.len());
  for handler in std::mem::take(&mut code.finally_handlers) {
    if let (Some(new_push), Some(new_finalizer)) = (
      old_to_new[handler.push_pc],
      old_to_new[handler.finalizer_pc],
    ) {
      new_handlers.push(crate::bytecode::FinallyHandler {
        push_pc: new_push,
        finalizer_pc: new_finalizer,
        return_slot: handler.return_slot,
        did_return_slot: handler.did_return_slot,
      });
    }
  }
  code.finally_handlers = new_handlers;

  builder.instructions = new_instructions;
  builder.lines = new_lines;

  len - reachable_count
}
