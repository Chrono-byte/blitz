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
        | Instruction::AssertEnd
        | Instruction::AssertWordBoundary { .. } => queue.push_back(fallthrough()?),
        Instruction::Class { class } => {
          if class.index() >= program.classes.len() {
            return Err(ProgramError::InvalidProgram);
          }
          queue.push_back(fallthrough()?);
        }
        Instruction::AssertLookahead { assertion, .. } => {
          let Some(assertion) = program.assertions.get(assertion.index()) else {
            return Err(ProgramError::InvalidProgram);
          };
          if assertion.capture_count() != program.capture_count {
            return Err(ProgramError::InvalidProgram);
          }
          queue.push_back(fallthrough()?);
        }
        Instruction::CaptureStart { capture }
        | Instruction::CaptureEnd { capture }
        | Instruction::Backreference { capture } => {
          if capture.index() >= program.capture_count {
            return Err(ProgramError::InvalidProgram);
          }
          queue.push_back(fallthrough()?);
        }
        Instruction::ClearCaptures { captures } => {
          if captures.index() >= program.capture_sets.len() {
            return Err(ProgramError::InvalidProgram);
          }
          queue.push_back(fallthrough()?);
        }
        Instruction::MarkProgress { progress } => {
          if progress.index() >= program.progress_count {
            return Err(ProgramError::InvalidProgram);
          }
          queue.push_back(fallthrough()?);
        }
        Instruction::ContinueProgress {
          progress,
          changed,
          unchanged,
        } => {
          if progress.index() >= program.progress_count {
            return Err(ProgramError::InvalidProgram);
          }
          queue.push_back(*changed);
          queue.push_back(*unchanged);
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
    for captures in &program.capture_sets {
      let mut previous = None;
      for capture in captures {
        if capture.index() >= program.capture_count
          || previous.is_some_and(|previous| previous >= capture.index())
        {
          return Err(ProgramError::InvalidProgram);
        }
        previous = Some(capture.index());
      }
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
      assertions: Box::new([]),
      capture_sets: Box::new([]),
      capture_count: 0,
      progress_count: 0,
      entry: ProgramCounter::from_index(0).expect("entry"),
      strategy: EntryStrategy::SingleLiteral {
        symbol: u32::from(b'a'),
      },
      analysis: Analysis {
        nullable: false,
        minimum_width: 1,
        capture_free: true,
        nfa_eligible: true,
        has_assertion: false,
        has_backreference: false,
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
