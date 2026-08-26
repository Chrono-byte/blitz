// SPDX-License-Identifier: MPL-2.0
//! Immutable control-flow program produced from semantic regex IR.

use crate::analysis::Analysis;
use crate::ast::CharacterClass;
use crate::ir::CaptureId;

const MAX_PROGRAM_INSTRUCTIONS: usize = 1_000_000;
const MAX_PROGRAM_CLASSES: usize = 1_000_000;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct ProgramCounter(u32);

impl ProgramCounter {
  pub(crate) fn from_index(index: usize) -> Result<Self, ProgramError> {
    u32::try_from(index)
      .map(Self)
      .map_err(|_| ProgramError::TooLarge)
  }

  pub(crate) const fn index(self) -> usize {
    self.0 as usize
  }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct ClassId(u32);

impl ClassId {
  pub(crate) fn from_index(index: usize) -> Result<Self, ProgramError> {
    u32::try_from(index)
      .map(Self)
      .map_err(|_| ProgramError::TooLarge)
  }

  pub(crate) const fn index(self) -> usize {
    self.0 as usize
  }
}

macro_rules! index_id {
  ($name:ident) => {
    #[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
    pub(crate) struct $name(u32);

    impl $name {
      pub(crate) fn from_index(index: usize) -> Result<Self, ProgramError> {
        u32::try_from(index)
          .map(Self)
          .map_err(|_| ProgramError::TooLarge)
      }

      pub(crate) const fn index(self) -> usize {
        self.0 as usize
      }
    }
  };
}

index_id!(AssertionId);
index_id!(CaptureSetId);
index_id!(ProgressId);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Instruction {
  Literal {
    symbol: u32,
  },
  Dot,
  Class {
    class: ClassId,
  },
  AssertStart,
  AssertEnd,
  AssertWordBoundary {
    expected: bool,
  },
  AssertLookahead {
    assertion: AssertionId,
    positive: bool,
  },
  CaptureStart {
    capture: CaptureId,
  },
  CaptureEnd {
    capture: CaptureId,
  },
  ClearCaptures {
    captures: CaptureSetId,
  },
  Backreference {
    capture: CaptureId,
  },
  MarkProgress {
    progress: ProgressId,
  },
  ContinueProgress {
    progress: ProgressId,
    changed: ProgramCounter,
    unchanged: ProgramCounter,
  },
  Split {
    preferred: ProgramCounter,
    deferred: ProgramCounter,
  },
  Jump {
    target: ProgramCounter,
  },
  Accept,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum EntryStrategy {
  Anchored,
  SingleLiteral { symbol: u32 },
  SingleClass { class: ClassId },
  SingleDot,
  GeneralSearch,
}

#[derive(Clone, Debug)]
pub(crate) struct Program {
  pub(crate) instructions: Box<[Instruction]>,
  pub(crate) classes: Box<[CharacterClass]>,
  pub(crate) assertions: Box<[VerifiedProgram]>,
  pub(crate) capture_sets: Box<[Box<[CaptureId]>]>,
  pub(crate) capture_count: usize,
  pub(crate) progress_count: usize,
  pub(crate) entry: ProgramCounter,
  pub(crate) strategy: EntryStrategy,
  pub(crate) analysis: Analysis,
}

#[derive(Clone, Debug)]
pub(crate) struct VerifiedProgram(pub(super) Program);

impl VerifiedProgram {
  pub(crate) fn instructions(&self) -> &[Instruction] {
    &self.0.instructions
  }

  pub(crate) fn classes(&self) -> &[CharacterClass] {
    &self.0.classes
  }

  pub(crate) fn assertions(&self) -> &[VerifiedProgram] {
    &self.0.assertions
  }

  pub(crate) fn capture_sets(&self) -> &[Box<[CaptureId]>] {
    &self.0.capture_sets
  }

  pub(crate) const fn capture_count(&self) -> usize {
    self.0.capture_count
  }

  pub(crate) const fn progress_count(&self) -> usize {
    self.0.progress_count
  }

  pub(crate) const fn entry(&self) -> ProgramCounter {
    self.0.entry
  }

  pub(crate) const fn strategy(&self) -> EntryStrategy {
    self.0.strategy
  }

  pub(crate) const fn analysis(&self) -> Analysis {
    self.0.analysis
  }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ProgramError {
  TooLarge,
  InvalidProgram,
}

pub(crate) fn within_program_limits(program: &Program) -> bool {
  program.instructions.len() <= MAX_PROGRAM_INSTRUCTIONS
    && program.classes.len() <= MAX_PROGRAM_CLASSES
}
