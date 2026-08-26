// SPDX-License-Identifier: MPL-2.0
//! Explicit-stack executor for verified capture-free regex programs.

use std::ops::Range;

use crate::canonical::symbols_equal;
use crate::matcher::{MatchError, class_matches, is_line_terminator};
use crate::program::{EntryStrategy, Instruction, ProgramCounter, VerifiedProgram};
use crate::{Flags, Input};

const MIN_EXECUTION_STEPS: usize = 1_000_000;
const MAX_EXECUTION_STEPS: usize = 10_000_000;
const EXECUTION_STEPS_PER_INPUT_UNIT: usize = 32;

#[derive(Clone, Copy)]
struct BacktrackFrame {
  pc: ProgramCounter,
  offset: usize,
}

struct ExecutionBudget {
  remaining: usize,
}

impl ExecutionBudget {
  fn new(input_length: usize) -> Self {
    Self {
      remaining: input_length
        .saturating_mul(EXECUTION_STEPS_PER_INPUT_UNIT)
        .clamp(MIN_EXECUTION_STEPS, MAX_EXECUTION_STEPS),
    }
  }

  fn consume(&mut self) -> Result<(), MatchError> {
    self.remaining = self.remaining.checked_sub(1).ok_or(MatchError)?;
    Ok(())
  }
}

pub(crate) fn find(
  program: &VerifiedProgram,
  input: &Input,
  start: usize,
  flags: Flags,
) -> Result<Option<Range<usize>>, MatchError> {
  if start > input.len() {
    return Ok(None);
  }
  let mut budget = ExecutionBudget::new(input.len());
  if flags.sticky() {
    return execute_at(program, input, start, flags, &mut budget);
  }
  match program.strategy() {
    EntryStrategy::SingleLiteral { symbol } => {
      scan_symbol(program, input, start, flags, &mut budget, |actual| {
        symbols_equal(symbol, actual, flags)
      })
    }
    EntryStrategy::SingleClass { class } => {
      let class = &program.classes()[class.index()];
      scan_symbol(program, input, start, flags, &mut budget, |actual| {
        class_matches(class, actual, flags)
      })
    }
    EntryStrategy::SingleDot => scan_symbol(program, input, start, flags, &mut budget, |actual| {
      flags.dot_all() || !is_line_terminator(actual)
    }),
    EntryStrategy::Anchored if !flags.multiline() => {
      if start == 0 {
        execute_at(program, input, 0, flags, &mut budget)
      } else {
        Ok(None)
      }
    }
    EntryStrategy::Anchored | EntryStrategy::GeneralSearch => {
      general_search(program, input, start, flags, &mut budget)
    }
  }
}

fn scan_symbol(
  _program: &VerifiedProgram,
  input: &Input,
  start: usize,
  flags: Flags,
  budget: &mut ExecutionBudget,
  predicate: impl Fn(u32) -> bool,
) -> Result<Option<Range<usize>>, MatchError> {
  let mut candidate = start;
  loop {
    budget.consume()?;
    let Some(point) = input.code_point_at(candidate, flags.unicode()) else {
      return Ok(None);
    };
    if predicate(point.value()) {
      return Ok(Some(candidate..candidate + point.width()));
    }
    candidate = input.advance_string_index(candidate, flags.unicode());
    if candidate > input.len() {
      return Ok(None);
    }
  }
}

fn general_search(
  program: &VerifiedProgram,
  input: &Input,
  start: usize,
  flags: Flags,
  budget: &mut ExecutionBudget,
) -> Result<Option<Range<usize>>, MatchError> {
  let mut candidate = start;
  loop {
    if let Some(matched) = execute_at(program, input, candidate, flags, budget)? {
      return Ok(Some(matched));
    }
    if candidate == input.len() {
      return Ok(None);
    }
    candidate = input.advance_string_index(candidate, flags.unicode());
    if candidate > input.len() {
      return Ok(None);
    }
  }
}

fn execute_at(
  program: &VerifiedProgram,
  input: &Input,
  start: usize,
  flags: Flags,
  budget: &mut ExecutionBudget,
) -> Result<Option<Range<usize>>, MatchError> {
  let mut pc = program.entry();
  let mut offset = start;
  let mut backtrack = Vec::new();
  loop {
    budget.consume()?;
    let instruction = program.instructions()[pc.index()];
    let mut failed = false;
    match instruction {
      Instruction::Literal { symbol } => {
        if let Some(point) = input.code_point_at(offset, flags.unicode())
          && symbols_equal(symbol, point.value(), flags)
        {
          offset += point.width();
          pc = next_pc(pc)?;
        } else {
          failed = true;
        }
      }
      Instruction::Dot => {
        if let Some(point) = input.code_point_at(offset, flags.unicode())
          && (flags.dot_all() || !is_line_terminator(point.value()))
        {
          offset += point.width();
          pc = next_pc(pc)?;
        } else {
          failed = true;
        }
      }
      Instruction::Class { class } => {
        if let Some(point) = input.code_point_at(offset, flags.unicode())
          && class_matches(&program.classes()[class.index()], point.value(), flags)
        {
          offset += point.width();
          pc = next_pc(pc)?;
        } else {
          failed = true;
        }
      }
      Instruction::AssertStart => {
        let satisfied = offset == 0
          || (flags.multiline()
            && previous_symbol(input, offset, flags.unicode()).is_some_and(is_line_terminator));
        if satisfied {
          pc = next_pc(pc)?;
        } else {
          failed = true;
        }
      }
      Instruction::AssertEnd => {
        let satisfied = offset == input.len()
          || (flags.multiline()
            && input
              .code_point_at(offset, flags.unicode())
              .is_some_and(|point| is_line_terminator(point.value())));
        if satisfied {
          pc = next_pc(pc)?;
        } else {
          failed = true;
        }
      }
      Instruction::Split {
        preferred,
        deferred,
      } => {
        backtrack.push(BacktrackFrame {
          pc: deferred,
          offset,
        });
        pc = preferred;
      }
      Instruction::Jump { target } => pc = target,
      Instruction::Accept => return Ok(Some(start..offset)),
    }
    if failed {
      let Some(frame) = backtrack.pop() else {
        return Ok(None);
      };
      pc = frame.pc;
      offset = frame.offset;
    }
  }
}

fn next_pc(pc: ProgramCounter) -> Result<ProgramCounter, MatchError> {
  ProgramCounter::from_index(pc.index().checked_add(1).ok_or(MatchError)?).map_err(|_| MatchError)
}

fn previous_symbol(
  input: &Input,
  offset: usize,
  unicode: bool,
) -> Option<u32> {
  let previous = offset.checked_sub(1)?;
  let last = *input.as_slice().get(previous)?;
  if unicode
    && (0xDC00..=0xDFFF).contains(&last)
    && let Some(first) = previous
      .checked_sub(1)
      .and_then(|index| input.as_slice().get(index))
      .copied()
    && (0xD800..=0xDBFF).contains(&first)
  {
    return Some(0x1_0000 + ((u32::from(first) - 0xD800) << 10) + (u32::from(last) - 0xDC00));
  }
  Some(u32::from(last))
}
