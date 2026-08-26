// SPDX-License-Identifier: MPL-2.0
//! Independent verifier for compiled regular-expression programs.

use std::collections::VecDeque;

use crate::program::{
  EntryStrategy, Instruction, Program, ProgramError, VerifiedProgram, within_program_limits,
};

impl VerifiedProgram {
  pub(crate) fn verify(program: Program) -> Result<Self, ProgramError> {
    if !within_program_limits(&program)
      || program.instructions.is_empty()
      || program.entry.index() >= program.instructions.len()
    {
      return Err(ProgramError::InvalidProgram);
    }
    verify_strategy(&program)?;

    let mut reachable = vec![false; program.instructions.len()];
    let mut queue = VecDeque::from([program.entry]);
    let mut accepts = 0_usize;
    while let Some(pc) = queue.pop_front() {
      let Some(instruction) = program.instructions.get(pc.index()) else {
        return Err(ProgramError::InvalidProgram);
      };
      if std::mem::replace(&mut reachable[pc.index()], true) {
        continue;
      }
      let fallthrough = || {
        pc.index()
          .checked_add(1)
          .and_then(|index| crate::program::ProgramCounter::from_index(index).ok())
          .ok_or(ProgramError::InvalidProgram)
      };
      match instruction {
        Instruction::Literal { .. }
        | Instruction::Dot
        | Instruction::AssertStart
        | Instruction::AssertEnd => queue.push_back(fallthrough()?),
        Instruction::Class { class } => {
          if class.index() >= program.classes.len() {
            return Err(ProgramError::InvalidProgram);
          }
          queue.push_back(fallthrough()?);
        }
        Instruction::Split {
          preferred,
          deferred,
        } => {
          queue.push_back(*preferred);
          queue.push_back(*deferred);
        }
        Instruction::Jump { target } => queue.push_back(*target),
        Instruction::Accept => accepts += 1,
      }
    }
    if accepts == 0 {
      return Err(ProgramError::InvalidProgram);
    }
    Ok(Self(program))
  }
}

fn verify_strategy(program: &Program) -> Result<(), ProgramError> {
  let valid = match program.strategy {
    EntryStrategy::Anchored => matches!(
      program.instructions.get(program.entry.index()),
      Some(Instruction::AssertStart)
    ),
    EntryStrategy::SingleLiteral { symbol } => {
      program.instructions.as_ref() == [Instruction::Literal { symbol }, Instruction::Accept]
    }
    EntryStrategy::SingleClass { class } => {
      class.index() < program.classes.len()
        && program.instructions.as_ref() == [Instruction::Class { class }, Instruction::Accept]
    }
    EntryStrategy::SingleDot => {
      program.instructions.as_ref() == [Instruction::Dot, Instruction::Accept]
    }
    EntryStrategy::GeneralSearch => true,
  };
  if valid {
    Ok(())
  } else {
    Err(ProgramError::InvalidProgram)
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::analysis::Analysis;
  use crate::program::ProgramCounter;

  fn literal_program() -> Program {
    Program {
      instructions: Box::new([
        Instruction::Literal {
          symbol: u32::from(b'a'),
        },
        Instruction::Accept,
      ]),
      classes: Box::new([]),
      entry: ProgramCounter::from_index(0).expect("entry"),
      strategy: EntryStrategy::SingleLiteral {
        symbol: u32::from(b'a'),
      },
      analysis: Analysis {
        nullable: false,
        minimum_width: 1,
        capture_free: true,
        nfa_eligible: true,
      },
    }
  }

  #[test]
  fn accepts_a_well_formed_literal_program() {
    assert!(VerifiedProgram::verify(literal_program()).is_ok());
  }

  #[test]
  fn rejects_an_out_of_range_control_flow_target() {
    let mut program = literal_program();
    program.instructions = Box::new([Instruction::Jump {
      target: ProgramCounter::from_index(7).expect("target"),
    }]);
    program.strategy = EntryStrategy::GeneralSearch;
    assert!(matches!(
      VerifiedProgram::verify(program),
      Err(ProgramError::InvalidProgram)
    ));
  }

  #[test]
  fn rejects_strategy_metadata_that_disagrees_with_the_program() {
    let mut program = literal_program();
    program.strategy = EntryStrategy::SingleDot;
    assert!(matches!(
      VerifiedProgram::verify(program),
      Err(ProgramError::InvalidProgram)
    ));
  }
}
