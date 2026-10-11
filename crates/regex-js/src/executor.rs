// SPDX-License-Identifier: MPL-2.0

use std::ops::Range;

use crate::canonical::symbols_equal;
use crate::matcher::{Match, MatchError, class_matches, is_line_terminator, is_word_character};
use crate::program::{EntryStrategy, Instruction, ProgramCounter, VerifiedProgram};
use crate::{Flags, Input};

const MIN_EXECUTION_STEPS: usize = 1_000_000;
const MAX_EXECUTION_STEPS: usize = 10_000_000;
const EXECUTION_STEPS_PER_INPUT_UNIT: usize = 32;

#[derive(Clone, Copy, Default)]
enum CaptureSlot {
  #[default]
  Undefined,
  Open(usize),
  Closed {
    start: usize,
    end: usize,
  },
}

#[derive(Clone, Copy)]
struct CaptureMutation {
  capture: crate::ir::CaptureId,
  previous: CaptureSlot,
}

#[derive(Clone, Copy)]
struct BacktrackFrame {
  pc: ProgramCounter,
  offset: usize,
  capture_checkpoint: usize,
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
) -> Result<Option<Match>, MatchError> {
  if start > input.len() {
    return Ok(None);
  }
  let mut budget = ExecutionBudget::new(input.len());
  if program.capture_count() == 0 && !program.analysis().has_assertion {
    if flags.sticky() {
      return captureless_at(program, input, start, flags, &mut budget);
    }
    match program.strategy() {
      EntryStrategy::SingleLiteral { symbol } => {
        return scan_symbol(input, start, flags, &mut budget, |actual| {
          symbols_equal(symbol, actual, flags)
        });
      }
      EntryStrategy::SingleClass { class } => {
        let class = &program.classes()[class.index()];
        return scan_symbol(input, start, flags, &mut budget, |actual| {
          class_matches(class, actual, flags)
        });
      }
      EntryStrategy::SingleDot => {
        return scan_symbol(input, start, flags, &mut budget, |actual| {
          flags.dot_all() || !is_line_terminator(actual)
        });
      }
      EntryStrategy::Anchored | EntryStrategy::GeneralSearch => {}
    }
  }
  general_find(program, input, start, flags, &mut budget)
}

fn scan_symbol(
  input: &Input,
  start: usize,
  flags: Flags,
  budget: &mut ExecutionBudget,
  predicate: impl Fn(u32) -> bool,
) -> Result<Option<Match>, MatchError> {
  let mut candidate = start;
  loop {
    budget.consume()?;
    let Some(point) = input.code_point_at(candidate, flags.unicode()) else {
      return Ok(None);
    };
    if predicate(point.value()) {
      return Ok(Some(Match::captureless(
        candidate..candidate + point.width(),
      )));
    }
    candidate = input.advance_string_index(candidate, flags.unicode());
    if candidate > input.len() {
      return Ok(None);
    }
  }
}

fn captureless_at(
  program: &VerifiedProgram,
  input: &Input,
  start: usize,
  flags: Flags,
  budget: &mut ExecutionBudget,
) -> Result<Option<Match>, MatchError> {
  let mut captures = [];
  let mut capture_log = Vec::new();
  let matched = execute_at(
    program,
    input,
    start,
    flags,
    &mut captures,
    &mut capture_log,
    budget,
  )?;
  Ok(matched.map(|end| Match::captureless(start..end)))
}

fn general_find(
  program: &VerifiedProgram,
  input: &Input,
  start: usize,
  flags: Flags,
  budget: &mut ExecutionBudget,
) -> Result<Option<Match>, MatchError> {
  let mut captures = vec![CaptureSlot::Undefined; program.capture_count()];
  let mut capture_log = Vec::new();
  let mut candidate = start;
  loop {
    if let Some(end) = execute_at(
      program,
      input,
      candidate,
      flags,
      &mut captures,
      &mut capture_log,
      budget,
    )? {
      let capture_ranges = captures
        .iter()
        .map(|capture| match capture {
          CaptureSlot::Closed { start, end } => Some(*start..*end),
          CaptureSlot::Undefined | CaptureSlot::Open(_) => None,
        })
        .collect();
      return Ok(Some(Match::from_captures(candidate..end, capture_ranges)));
    }
    if flags.sticky() || candidate == input.len() {
      return Ok(None);
    }
    candidate = input.advance_string_index(candidate, flags.unicode());
    if candidate > input.len() {
      return Ok(None);
    }
  }
}

#[allow(clippy::too_many_arguments)]
fn execute_at(
  program: &VerifiedProgram,
  input: &Input,
  start: usize,
  flags: Flags,
  captures: &mut [CaptureSlot],
  capture_log: &mut Vec<CaptureMutation>,
  budget: &mut ExecutionBudget,
) -> Result<Option<usize>, MatchError> {
  let initial_checkpoint = capture_log.len();
  let mut pc = program.entry();
  let mut offset = start;
  let mut backtrack = Vec::new();
  let mut progress = vec![0_usize; program.progress_count()];
  loop {
    let instruction = program.instructions()[pc.index()];
    if matches!(
      instruction,
      Instruction::Literal { .. }
        | Instruction::Dot
        | Instruction::Class { .. }
        | Instruction::AssertStart
        | Instruction::AssertEnd
        | Instruction::AssertWordBoundary { .. }
        | Instruction::AssertLookahead { .. }
        | Instruction::CaptureStart { .. }
        | Instruction::CaptureEnd { .. }
        | Instruction::ClearCaptures { .. }
        | Instruction::Backreference { .. }
    ) {
      budget.consume()?;
    }
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
        if offset == 0
          || (flags.multiline()
            && previous_symbol(input, offset, flags.unicode()).is_some_and(is_line_terminator))
        {
          pc = next_pc(pc)?;
        } else {
          failed = true;
        }
      }
      Instruction::AssertEnd => {
        if offset == input.len()
          || (flags.multiline()
            && input
              .code_point_at(offset, flags.unicode())
              .is_some_and(|point| is_line_terminator(point.value())))
        {
          pc = next_pc(pc)?;
        } else {
          failed = true;
        }
      }
      Instruction::AssertWordBoundary { expected } => {
        let before = previous_symbol(input, offset, flags.unicode())
          .is_some_and(|symbol| is_word_character(symbol, flags));
        let after = input
          .code_point_at(offset, flags.unicode())
          .is_some_and(|point| is_word_character(point.value(), flags));
        if (before != after) == expected {
          pc = next_pc(pc)?;
        } else {
          failed = true;
        }
      }
      Instruction::AssertLookahead {
        assertion,
        positive,
      } => {
        let checkpoint = capture_log.len();
        let matched = execute_at(
          &program.assertions()[assertion.index()],
          input,
          offset,
          flags,
          captures,
          capture_log,
          budget,
        )?;
        if positive == matched.is_some() {
          if !positive {
            restore_captures(captures, capture_log, checkpoint);
          }
          pc = next_pc(pc)?;
        } else {
          restore_captures(captures, capture_log, checkpoint);
          failed = true;
        }
      }
      Instruction::CaptureStart { capture } => {
        mutate_capture(captures, capture_log, capture, CaptureSlot::Open(offset));
        pc = next_pc(pc)?;
      }
      Instruction::CaptureEnd { capture } => {
        let start = match captures[capture.index()] {
          CaptureSlot::Open(start) => start,
          CaptureSlot::Undefined | CaptureSlot::Closed { .. } => offset,
        };
        mutate_capture(
          captures,
          capture_log,
          capture,
          CaptureSlot::Closed { start, end: offset },
        );
        pc = next_pc(pc)?;
      }
      Instruction::ClearCaptures { captures: set } => {
        for capture in program.capture_sets()[set.index()]
          .iter()
          .copied()
        {
          mutate_capture(captures, capture_log, capture, CaptureSlot::Undefined);
        }
        pc = next_pc(pc)?;
      }
      Instruction::Backreference { capture } => match captures[capture.index()] {
        CaptureSlot::Undefined | CaptureSlot::Open(_) => pc = next_pc(pc)?,
        CaptureSlot::Closed { start, end } => {
          if let Some(next_offset) = match_backreference(input, start..end, offset, flags, budget)?
          {
            offset = next_offset;
            pc = next_pc(pc)?;
          } else {
            failed = true;
          }
        }
      },
      Instruction::MarkProgress { progress: id } => {
        progress[id.index()] = offset;
        pc = next_pc(pc)?;
      }
      Instruction::ContinueProgress {
        progress: id,
        changed,
        unchanged,
      } => {
        pc = if progress[id.index()] == offset {
          unchanged
        } else {
          changed
        };
      }
      Instruction::Split {
        preferred,
        deferred,
      } => {
        backtrack.push(BacktrackFrame {
          pc: deferred,
          offset,
          capture_checkpoint: capture_log.len(),
        });
        pc = preferred;
      }
      Instruction::Jump { target } => pc = target,
      Instruction::Accept => return Ok(Some(offset)),
    }
    if failed {
      let Some(frame) = backtrack.pop() else {
        restore_captures(captures, capture_log, initial_checkpoint);
        return Ok(None);
      };
      restore_captures(captures, capture_log, frame.capture_checkpoint);
      pc = frame.pc;
      offset = frame.offset;
    }
  }
}

fn mutate_capture(
  captures: &mut [CaptureSlot],
  log: &mut Vec<CaptureMutation>,
  capture: crate::ir::CaptureId,
  value: CaptureSlot,
) {
  log.push(CaptureMutation {
    capture,
    previous: captures[capture.index()],
  });
  captures[capture.index()] = value;
}

fn restore_captures(
  captures: &mut [CaptureSlot],
  log: &mut Vec<CaptureMutation>,
  checkpoint: usize,
) {
  while log.len() > checkpoint {
    let mutation = log.pop().expect("capture log length checked");
    captures[mutation.capture.index()] = mutation.previous;
  }
}

fn match_backreference(
  input: &Input,
  expected: Range<usize>,
  mut actual_offset: usize,
  flags: Flags,
  budget: &mut ExecutionBudget,
) -> Result<Option<usize>, MatchError> {
  let mut expected_offset = expected.start;
  while expected_offset < expected.end {
    budget.consume()?;
    let Some(expected) = input.code_point_at(expected_offset, flags.unicode()) else {
      return Ok(None);
    };
    let Some(actual) = input.code_point_at(actual_offset, flags.unicode()) else {
      return Ok(None);
    };
    if !symbols_equal(expected.value(), actual.value(), flags) {
      return Ok(None);
    }
    expected_offset += expected.width();
    actual_offset += actual.width();
  }
  Ok(Some(actual_offset))
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
