// SPDX-License-Identifier: MPL-2.0
use super::*;

const MAX_OPERATIONS: usize = 64;
const MAX_LOCALS: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(in crate::vm) struct NumericBodyRegion {
  pub(in crate::vm) entry_pc: usize,
  pub(in crate::vm) completed_pc: usize,
}

#[derive(Clone, Copy, Debug)]
pub(in crate::vm) enum NumericOperation {
  Constant(usize, f64),
  Load(usize, usize),
  Store(usize, usize),
  Copy(usize, usize),
  Binary(Opcode, usize, usize),
  Negate(usize, usize),
}

pub(in crate::vm) struct NumericBodyProgram {
  pub(in crate::vm) operations: [NumericOperation; MAX_OPERATIONS],
  pub(in crate::vm) operation_count: usize,
  pub(in crate::vm) locals: [usize; MAX_LOCALS],
  pub(in crate::vm) local_count: usize,
  pub(in crate::vm) reads: u16,
  pub(in crate::vm) writes: u16,
  pub(in crate::vm) registers: u16,
  pub(in crate::vm) comparison: (usize, usize),
}

impl NumericBodyRegion {
  pub(in crate::vm) fn decode(
    code: &Chunk,
    pc: usize,
  ) -> Option<Self> {
    let header = code.words.get(pc..pc + 3)?;
    if opcode(header[0]) != Opcode::LoadLocal
      || !matches!(opcode(header[1]), Opcode::Const | Opcode::LoadLocal)
      || opcode(header[2]) != Opcode::CompareJump
    {
      return None;
    }
    let (condition, left, right, offset) = decode_compare_jump(header[2]);
    if condition != 2
      || left != decode_rd(header[0])
      || right != decode_rd(header[1])
      || left == right
    {
      return None;
    }
    let completed_pc = usize::try_from(pc as i64 + 3 + i64::from(offset)).ok()?;
    if completed_pc <= pc + 4 || completed_pc - pc > MAX_OPERATIONS + 2 {
      return None;
    }
    let last = *code.words.get(completed_pc - 1)?;
    if opcode(last) != Opcode::Jump
      || completed_pc as i64 + i64::from((last as i32) >> 8) != pc as i64
    {
      return None;
    }
    let region = Self {
      entry_pc: pc,
      completed_pc,
    };
    region.program(code)?;
    Some(region)
  }

  pub(in crate::vm) fn program(
    self,
    code: &Chunk,
  ) -> Option<NumericBodyProgram> {
    let mut plan = NumericBodyProgram {
      operations: [NumericOperation::Constant(0, 0.0); MAX_OPERATIONS],
      operation_count: 0,
      locals: [0; MAX_LOCALS],
      local_count: 0,
      reads: 0,
      writes: 0,
      registers: 0,
      comparison: (
        decode_rd(code.words[self.entry_pc]) as usize,
        decode_rd(code.words[self.entry_pc + 1]) as usize,
      ),
    };
    let mut initialized = 0u16;
    for pc in self.entry_pc..self.completed_pc - 1 {
      if pc == self.entry_pc + 2 {
        continue;
      }
      let word = code.words[pc];
      let op = opcode(word);
      let rd = decode_rd(word) as usize;
      let rs = decode_rs(word) as usize;
      let operation = match op {
        Opcode::Const => NumericOperation::Constant(rd, number_constant(code, word)?),
        Opcode::LoadLocal | Opcode::StoreLocal => {
          let slot = decode_aux16(word) as usize;
          let local = match plan.locals[..plan.local_count]
            .iter()
            .position(|value| *value == slot)
          {
            Some(index) => index,
            None => {
              if plan.local_count == MAX_LOCALS {
                return None;
              }
              let index = plan.local_count;
              plan.locals[index] = slot;
              plan.local_count += 1;
              index
            }
          };
          if op == Opcode::LoadLocal {
            if plan.writes & (1 << local) == 0 {
              plan.reads |= 1 << local;
            }
            NumericOperation::Load(rd, local)
          } else {
            if initialized & (1 << rd) == 0 {
              return None;
            }
            plan.writes |= 1 << local;
            NumericOperation::Store(local, rd)
          }
        }
        Opcode::Move | Opcode::Neg => {
          if initialized & (1 << rs) == 0 {
            return None;
          }
          if op == Opcode::Move {
            NumericOperation::Copy(rd, rs)
          } else {
            NumericOperation::Negate(rd, rs)
          }
        }
        Opcode::Add | Opcode::Sub | Opcode::Mul | Opcode::Div | Opcode::Mod => {
          if initialized & (1 << rd) == 0 || initialized & (1 << rs) == 0 {
            return None;
          }
          NumericOperation::Binary(op, rd, rs)
        }
        _ => return None,
      };
      if op != Opcode::StoreLocal {
        initialized |= 1 << rd;
        plan.registers |= 1 << rd;
      }
      *plan.operations.get_mut(plan.operation_count)? = operation;
      plan.operation_count += 1;
    }
    Some(plan)
  }
}

impl NumericOperation {
  pub(in crate::vm) fn execute(
    self,
    registers: &mut [f64; 16],
    locals: &mut [f64; 16],
  ) {
    match self {
      Self::Constant(rd, value) => registers[rd] = value,
      Self::Load(rd, slot) => registers[rd] = locals[slot],
      Self::Store(slot, rd) => locals[slot] = registers[rd],
      Self::Copy(rd, rs) => registers[rd] = registers[rs],
      Self::Negate(rd, rs) => registers[rd] = -registers[rs],
      Self::Binary(op, rd, rs) => {
        registers[rd] = match op {
          Opcode::Add => registers[rd] + registers[rs],
          Opcode::Sub => registers[rd] - registers[rs],
          Opcode::Mul => registers[rd] * registers[rs],
          Opcode::Div => registers[rd] / registers[rs],
          Opcode::Mod => registers[rd] % registers[rs],
          _ => unreachable!("validated numeric operation"),
        }
      }
    }
  }
}
