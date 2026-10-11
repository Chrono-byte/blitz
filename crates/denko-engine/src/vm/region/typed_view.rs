// SPDX-License-Identifier: MPL-2.0
use super::numeric_body::NumericOperation;
use super::*;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(in crate::vm) struct TypedViewRegion {
  pub entry_pc: usize,
  pub completed_pc: usize,
  pub view_slot: usize,
}
#[derive(Clone, Copy, Debug)]
pub(in crate::vm) enum TypedViewOperation {
  Numeric(NumericOperation, Option<usize>),
  View(usize),
  Key(usize),
  Length(usize),
  Read(usize, usize),
  Write(usize, usize, usize),
  Guard(u8, usize, usize),
}
pub(in crate::vm) struct TypedViewProgram {
  pub operations: Vec<TypedViewOperation>,
  pub view_slot: usize,
  pub locals: Vec<usize>,
  pub reads: u16,
  pub writes: u16,
  pub comparison: (usize, usize),
}
impl TypedViewRegion {
  pub(in crate::vm) fn decode(
    code: &Chunk,
    pc: usize,
  ) -> Option<Self> {
    let header = code.words.get(pc..pc + 5)?;
    if !header
      .iter()
      .zip([
        Opcode::LoadLocal,
        Opcode::LoadLocal,
        Opcode::Const,
        Opcode::GetProperty,
        Opcode::CompareJump,
      ])
      .all(|(word, op)| opcode(*word) == op)
    {
      return None;
    }
    let (condition, left, right, offset) = decode_compare_jump(header[4]);
    if condition != 2
      || left != decode_rd(header[0])
      || right != decode_rd(header[1])
      || left == right
      || decode_rd(header[3]) != right
      || decode_rs(header[3]) != decode_rd(header[2])
    {
      return None;
    }
    let completed_pc = usize::try_from(pc as i64 + 5 + i64::from(offset)).ok()?;
    if completed_pc <= pc + 6 || completed_pc - pc > 128 {
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
      view_slot: decode_aux16(header[1]) as usize,
    };
    region.program(code)?;
    Some(region)
  }

  #[allow(clippy::too_many_lines)]
  pub(in crate::vm) fn program(
    self,
    code: &Chunk,
  ) -> Option<TypedViewProgram> {
    let view_slot = decode_aux16(code.words[self.entry_pc + 1]) as usize;
    let mut plan = TypedViewProgram {
      operations: Vec::new(),
      view_slot,
      locals: Vec::new(),
      reads: 0,
      writes: 0,
      comparison: (
        decode_rd(code.words[self.entry_pc]) as usize,
        decode_rd(code.words[self.entry_pc + 1]) as usize,
      ),
    };
    let mut tags = [0u8; 16];
    let mut pc = self.entry_pc;
    let mut guarded = false;
    let mut writes_view = false;
    let mut reads_view = false;
    while pc < self.completed_pc - 1 {
      if pc == self.entry_pc + 4 {
        if tags[plan.comparison.0] != 1 || tags[plan.comparison.1] != 1 {
          return None;
        }
        pc += 1;
        continue;
      }
      let word = code.words[pc];
      let op = opcode(word);
      let rd = decode_rd(word) as usize;
      let rs = decode_rs(word) as usize;
      fn numeric(
        tags: &mut [u8; 16],
        operation: NumericOperation,
        destination: Option<usize>,
      ) -> TypedViewOperation {
        if let Some(destination) = destination {
          tags[destination] = 1;
        }
        TypedViewOperation::Numeric(operation, destination)
      }
      let operation = match op {
        Opcode::LoadLocal if decode_aux16(word) as usize == view_slot => {
          tags[rd] = 2;
          TypedViewOperation::View(rd)
        }
        Opcode::Const => match code.constants.get(decode_aux16(word) as usize)? {
          crate::bytecode::Constant::Number(value) => {
            numeric(&mut tags, NumericOperation::Constant(rd, *value), Some(rd))
          }
          crate::bytecode::Constant::String(value) if value == "length" => {
            tags[rd] = 3;
            TypedViewOperation::Key(rd)
          }
          _ => return None,
        },
        Opcode::LoadLocal | Opcode::StoreLocal => {
          let slot = decode_aux16(word) as usize;
          if slot == view_slot {
            return None;
          }
          let local = match plan
            .locals
            .iter()
            .position(|entry| *entry == slot)
          {
            Some(index) => index,
            None => {
              if plan.locals.len() == 16 {
                return None;
              }
              plan.locals.push(slot);
              plan.locals.len() - 1
            }
          };
          if op == Opcode::LoadLocal {
            if plan.writes & (1 << local) == 0 {
              plan.reads |= 1 << local;
            }
            numeric(&mut tags, NumericOperation::Load(rd, local), Some(rd))
          } else {
            if tags[rd] != 1 {
              return None;
            }
            plan.writes |= 1 << local;
            TypedViewOperation::Numeric(NumericOperation::Store(local, rd), None)
          }
        }
        Opcode::Move if tags[rs] == 2 => {
          tags[rd] = 2;
          TypedViewOperation::View(rd)
        }
        Opcode::Move | Opcode::Neg => {
          if tags[rs] != 1 {
            return None;
          }
          if op == Opcode::Move {
            numeric(&mut tags, NumericOperation::Copy(rd, rs), Some(rd))
          } else {
            numeric(&mut tags, NumericOperation::Negate(rd, rs), Some(rd))
          }
        }
        Opcode::Add | Opcode::Sub | Opcode::Mul | Opcode::Div | Opcode::Mod | Opcode::Increment => {
          if tags[rd] != 1 || tags[rs] != 1 {
            return None;
          }
          numeric(
            &mut tags,
            NumericOperation::Binary(
              if op == Opcode::Increment {
                Opcode::Add
              } else {
                op
              },
              rd,
              rs,
            ),
            Some(rd),
          )
        }
        Opcode::GetProperty if tags[rd] == 2 && tags[rs] == 3 => {
          tags[rd] = 1;
          TypedViewOperation::Length(rd)
        }
        Opcode::GetProperty if tags[rd] == 2 && tags[rs] == 1 => {
          if writes_view {
            return None;
          }
          reads_view = true;
          tags[rd] = 1;
          TypedViewOperation::Read(rd, rs)
        }
        Opcode::SetProperty => {
          let value = ((word >> 16) & 15) as usize;
          if tags[rd] != 2
            || tags[rs] != 1
            || tags[value] != 1
            || guarded
            || reads_view
            || writes_view
          {
            return None;
          }
          writes_view = true;
          tags[rd] = 1;
          TypedViewOperation::Write(rd, rs, value)
        }
        Opcode::CompareJump => {
          let (condition, left, right, offset) = decode_compare_jump(word);
          if writes_view
            || condition > 7
            || tags[left as usize] != 1
            || tags[right as usize] != 1
            || offset <= 0
          {
            return None;
          }
          let target = usize::try_from(pc as i64 + 1 + i64::from(offset)).ok()?;
          if target >= self.completed_pc - 1 {
            return None;
          }
          guarded = true;
          pc = target - 1;
          TypedViewOperation::Guard(condition, left as usize, right as usize)
        }
        _ => return None,
      };
      plan.operations.push(operation);
      pc += 1;
    }
    Some(plan)
  }
}
