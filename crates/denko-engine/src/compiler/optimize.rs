// SPDX-License-Identifier: MPL-2.0
//! Optimization passes for the RiscBuilder.

use super::emit::FrontendOp as Op;
use super::risc::{InstructionInputs, PendingInstruction, RiscBuilder, ValueId};
use crate::bytecode::{Chunk, Constant};
use std::collections::{HashMap, HashSet};

pub(crate) fn optimize(
  builder: &mut RiscBuilder,
  chunk: &mut Chunk,
) -> usize {
  let mut total = 0;
  total += constant_fold(builder, chunk);
  total += dead_code_elimination(builder, chunk);
  total
}

fn constant_fold(
  builder: &mut RiscBuilder,
  chunk: &mut Chunk,
) -> usize {
  // ValueId homes are stack-shaped and can be reused at control-flow merges.
  // Until propagation is block-aware, restrict folding to straight-line code
  // so a value from one predecessor cannot become a fact for another.
  if builder
    .instructions
    .iter()
    .any(|instruction| instruction.op.is_jump())
  {
    return 0;
  }
  let mut value_to_const: HashMap<usize, Option<f64>> = HashMap::new();
  let mut folded = 0;

  for idx in 0..builder.instructions.len() {
    let op = builder.instructions[idx].op;
    let inputs = builder.instructions[idx]
      .inputs
      .as_slice()
      .to_vec();
    let output = builder.instructions[idx].output;

    let folded_value = match op {
      Op::Sub | Op::Mul | Op::Div => {
        if inputs.len() != 2 {
          None
        } else {
          let left = value_to_const.get(&inputs[0].0).and_then(|v| *v);
          let right = value_to_const.get(&inputs[1].0).and_then(|v| *v);
          match (left, right) {
            (Some(l), Some(r)) => match op {
              Op::Sub => Some(l - r),
              Op::Mul => Some(l * r),
              Op::Div => Some(l / r),
              _ => None,
            },
            _ => None,
          }
        }
      }
      Op::Neg => {
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
      let const_idx = chunk.add_constant(constant);
      let Some(payload) = u32::try_from(const_idx)
        .ok()
        .filter(|index| *index <= u32::from(u16::MAX))
      else {
        // RISC constants carry a 16-bit index. Do not wrap a large index into
        // a different constant; leave the original operation intact.
        chunk.constants.pop();
        if let Some(out) = output {
          value_to_const.insert(out.0, None);
        }
        continue;
      };
      let inst = &mut builder.instructions[idx];
      inst.op = Op::Const;
      inst.payload = payload;
      inst.inputs = InstructionInputs::None;
      if let Some(out) = output {
        value_to_const.insert(out.0, Some(result));
      }
      folded += 1;
    } else {
      if let Some(out) = output {
        let const_val = if op == Op::Const {
          let idx_const = (builder.instructions[idx].payload & 0xffff) as usize;
          if let Some(Constant::Number(n)) = chunk.constants.get(idx_const) {
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
fn common_subexpression_elimination(builder: &mut RiscBuilder) -> usize {
  fn is_pure(op: Op) -> bool {
    matches!(
      op,
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
        | Op::Neg
        | Op::BitNot
        | Op::Const
    )
  }

  let is_commutative =
    |op: Op| matches!(op, Op::Add | Op::Mul | Op::BitAnd | Op::BitOr | Op::BitXor);

  let mut map: HashMap<(u32, Vec<usize>), usize> = HashMap::new();
  let mut remap: HashMap<usize, usize> = HashMap::new();
  let mut to_remove = HashSet::new();
  let mut eliminated = 0;

  let op_key = |op: Op| -> u32 {
    match op {
      Op::Bytecode(b) => b as u32,
      Op::Discard => 0xFF00,
      Op::Copy => 0xFF01,
      Op::Exchange => 0xFF02,
    }
  };

  for idx in 0..builder.instructions.len() {
    let inst = &builder.instructions[idx];
    if !is_pure(inst.op) {
      continue;
    }
    let key: (u32, Vec<usize>) = if inst.op == Op::Const {
      (op_key(Op::Const), vec![inst.payload as usize])
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
      if is_commutative(inst.op) {
        inputs.sort_unstable();
      }
      (op_key(inst.op), inputs)
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
      PendingInstruction {
        op: Op::Const,
        payload: 0,
        branch_offset: None,
        inputs: InstructionInputs::None,
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
      new_inputs.push(ValueId(id));
    }
    inst.inputs = InstructionInputs::from_slice(&new_inputs);
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
  for (old_ip, values) in std::mem::take(&mut builder.branch_values) {
    if let Some(new_ip) = old_to_new[old_ip] {
      let remapped_values = values
        .into_iter()
        .map(|v| {
          let mut id = v.0;
          while let Some(&mapped) = remap.get(&id) {
            id = mapped;
          }
          ValueId(id)
        })
        .collect();
      new_branch_values.insert(new_ip, remapped_values);
    }
  }
  builder.branch_values = new_branch_values;
  builder.instructions = new_instructions;
  builder.lines = new_lines;

  eliminated
}

fn dead_code_elimination(
  builder: &mut RiscBuilder,
  chunk: &mut Chunk,
) -> usize {
  let len = builder.instructions.len();
  if len == 0 {
    return 0;
  }
  let mut reachable = vec![false; len];
  let mut worklist = vec![0];
  reachable[0] = true;
  // Also mark finally handler targets as reachable roots
  for handler in &chunk.finally_handlers {
    if handler.push_ip < len {
      reachable[handler.push_ip] = true;
      worklist.push(handler.push_ip);
    }
    if handler.finalizer_ip < len {
      reachable[handler.finalizer_ip] = true;
      worklist.push(handler.finalizer_ip);
    }
  }
  let mut visited = HashSet::new();
  while let Some(ip) = worklist.pop() {
    if !visited.insert(ip) {
      continue;
    }
    if ip >= len {
      continue;
    }
    let inst = &builder.instructions[ip];
    let branch = inst.branch_offset;
    match inst.op {
      Op::Jump => {
        if let Some(offset) = branch
          && let Ok(target) = usize::try_from(ip as i64 + 1 + offset as i64)
          && target < len
          && !reachable[target]
        {
          reachable[target] = true;
          worklist.push(target);
        }
      }
      Op::JumpIfFalse | Op::JumpIfTrue | Op::JumpIfNullish | Op::CompareJump | Op::PushHandler => {
        if let Some(offset) = branch
          && let Ok(target) = usize::try_from(ip as i64 + 1 + offset as i64)
          && target < len
          && !reachable[target]
        {
          reachable[target] = true;
          worklist.push(target);
        }
        if ip + 1 < len && !reachable[ip + 1] {
          reachable[ip + 1] = true;
          worklist.push(ip + 1);
        }
      }
      Op::Return | Op::Throw => {}
      _ => {
        if ip + 1 < len && !reachable[ip + 1] {
          reachable[ip + 1] = true;
          worklist.push(ip + 1);
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
      PendingInstruction {
        op: Op::Const,
        payload: 0,
        branch_offset: None,
        inputs: InstructionInputs::None,
        output: None,
      },
    );
    if let Some(old_offset) = inst.branch_offset {
      let old_target = (old_idx as i64 + 1 + old_offset as i64) as usize;
      if old_target < len
        && let Some(new_target) = old_to_new[old_target]
      {
        let new_ip = new_instructions.len();
        let new_offset = new_target as i64 - (new_ip as i64 + 1);
        inst.branch_offset = Some(new_offset as i32);
      }
    }
    new_instructions.push(inst);
    new_lines.push(builder.lines[old_idx]);
  }

  let mut new_branch_values = HashMap::new();
  for (old_ip, values) in std::mem::take(&mut builder.branch_values) {
    if let Some(new_ip) = old_to_new[old_ip] {
      new_branch_values.insert(new_ip, values);
    }
  }
  builder.branch_values = new_branch_values;

  // Remap finally handlers
  let mut new_handlers = Vec::with_capacity(chunk.finally_handlers.len());
  for handler in std::mem::take(&mut chunk.finally_handlers) {
    if let (Some(new_push), Some(new_finalizer)) = (
      old_to_new[handler.push_ip],
      old_to_new[handler.finalizer_ip],
    ) {
      new_handlers.push(crate::bytecode::FinallyHandler {
        push_ip: new_push,
        finalizer_ip: new_finalizer,
        return_slot: handler.return_slot,
        did_return_slot: handler.did_return_slot,
      });
    }
  }
  chunk.finally_handlers = new_handlers;

  builder.instructions = new_instructions;
  builder.lines = new_lines;

  len - reachable_count
}
