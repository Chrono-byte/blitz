// SPDX-License-Identifier: MPL-2.0
//! Lower typed semantic regex IR into verified control-flow instructions.

use crate::analysis::{Analysis, captures_written};
use crate::ast::CharacterClass;
use crate::ir::{IrAssertion, IrNode, IrRejection};
use crate::program::{
  AssertionId, CaptureSetId, ClassId, EntryStrategy, Instruction, Program, ProgramCounter,
  ProgramError, ProgressId, VerifiedProgram,
};

const MAX_LOWERED_INSTRUCTIONS: usize = 1_000_000;

pub(crate) fn compile(
  node: &IrNode,
  capture_count: usize,
) -> Result<VerifiedProgram, IrRejection> {
  compile_program(node, capture_count, true)
}

fn compile_program(
  node: &IrNode,
  capture_count: usize,
  select_strategy: bool,
) -> Result<VerifiedProgram, IrRejection> {
  let analysis = Analysis::for_ir(node);
  let mut compiler = Compiler::new(capture_count);
  compiler.compile_node(node)?;
  compiler.emit(Instruction::Accept)?;
  let strategy = if select_strategy {
    compiler.entry_strategy(node)?
  } else {
    EntryStrategy::GeneralSearch
  };
  let program = Program {
    instructions: compiler.instructions.into_boxed_slice(),
    classes: compiler.classes.into_boxed_slice(),
    assertions: compiler.assertions.into_boxed_slice(),
    capture_sets: compiler.capture_sets.into_boxed_slice(),
    capture_count,
    progress_count: compiler.progress_count,
    entry: ProgramCounter::from_index(0).map_err(map_program_error)?,
    strategy,
    analysis,
  };
  VerifiedProgram::verify(program).map_err(map_program_error)
}

struct Compiler {
  instructions: Vec<Instruction>,
  classes: Vec<CharacterClass>,
  assertions: Vec<VerifiedProgram>,
  capture_sets: Vec<Box<[crate::ir::CaptureId]>>,
  capture_count: usize,
  progress_count: usize,
}

impl Compiler {
  fn new(capture_count: usize) -> Self {
    Self {
      instructions: Vec::new(),
      classes: Vec::new(),
      assertions: Vec::new(),
      capture_sets: Vec::new(),
      capture_count,
      progress_count: 0,
    }
  }

  fn compile_node(
    &mut self,
    node: &IrNode,
  ) -> Result<(), IrRejection> {
    match node {
      IrNode::Empty => Ok(()),
      IrNode::Literal(symbol) => self
        .emit(Instruction::Literal { symbol: *symbol })
        .map(drop),
      IrNode::Dot => self.emit(Instruction::Dot).map(drop),
      IrNode::Class(class) => {
        let class = self.add_class(class.clone())?;
        self.emit(Instruction::Class { class }).map(drop)
      }
      IrNode::Assert(IrAssertion::Start) => self.emit(Instruction::AssertStart).map(drop),
      IrNode::Assert(IrAssertion::End) => self.emit(Instruction::AssertEnd).map(drop),
      IrNode::Assert(IrAssertion::WordBoundary(expected)) => self
        .emit(Instruction::AssertWordBoundary {
          expected: *expected,
        })
        .map(drop),
      IrNode::Assert(IrAssertion::Lookahead { positive, body }) => {
        let assertion =
          AssertionId::from_index(self.assertions.len()).map_err(map_program_error)?;
        self
          .assertions
          .push(compile_program(body, self.capture_count, false)?);
        self
          .emit(Instruction::AssertLookahead {
            assertion,
            positive: *positive,
          })
          .map(drop)
      }
      IrNode::Backreference(capture) => self
        .emit(Instruction::Backreference { capture: *capture })
        .map(drop),
      IrNode::Sequence(nodes) => {
        for node in nodes {
          self.compile_node(node)?;
        }
        Ok(())
      }
      IrNode::Alternative(nodes) => self.compile_alternative(nodes),
      IrNode::Capture { capture, body } => {
        self.emit(Instruction::CaptureStart { capture: *capture })?;
        self.compile_node(body)?;
        self.emit(Instruction::CaptureEnd { capture: *capture })?;
        Ok(())
      }
      IrNode::Repeat {
        body,
        min,
        max,
        greedy,
      } => self.compile_repeat(body, *min, *max, *greedy),
    }
  }

  fn compile_alternative(
    &mut self,
    nodes: &[IrNode],
  ) -> Result<(), IrRejection> {
    let Some((last, leading)) = nodes.split_last() else {
      return Ok(());
    };
    let mut exit_jumps = Vec::with_capacity(leading.len());
    for node in leading {
      let split = self.emit(Instruction::Accept)?;
      let preferred = self.next_pc()?;
      self.compile_node(node)?;
      exit_jumps.push(self.emit(Instruction::Accept)?);
      let deferred = self.next_pc()?;
      self.instructions[split.index()] = Instruction::Split {
        preferred,
        deferred,
      };
    }
    self.compile_node(last)?;
    let after = self.next_pc()?;
    for jump in exit_jumps {
      self.instructions[jump.index()] = Instruction::Jump { target: after };
    }
    Ok(())
  }

  fn compile_repeat(
    &mut self,
    body: &IrNode,
    min: usize,
    max: Option<usize>,
    greedy: bool,
  ) -> Result<(), IrRejection> {
    let captures = self.add_capture_set(captures_written(body))?;
    for _ in 0..min {
      self.compile_iteration(body, captures)?;
    }
    match max {
      Some(max) => {
        let optional = max
          .checked_sub(min)
          .ok_or(IrRejection::ProgramTooLarge)?;
        for _ in 0..optional {
          self.compile_optional(body, captures, greedy)?;
        }
      }
      None => self.compile_star(body, captures, greedy)?,
    }
    Ok(())
  }

  fn compile_iteration(
    &mut self,
    body: &IrNode,
    captures: CaptureSetId,
  ) -> Result<(), IrRejection> {
    self.emit(Instruction::ClearCaptures { captures })?;
    self.compile_node(body)
  }

  fn compile_optional(
    &mut self,
    body: &IrNode,
    captures: CaptureSetId,
    greedy: bool,
  ) -> Result<(), IrRejection> {
    let split = self.emit(Instruction::Accept)?;
    let body_start = self.next_pc()?;
    self.compile_iteration(body, captures)?;
    let after = self.next_pc()?;
    self.instructions[split.index()] = ordered_split(body_start, after, greedy);
    Ok(())
  }

  fn compile_star(
    &mut self,
    body: &IrNode,
    captures: CaptureSetId,
    greedy: bool,
  ) -> Result<(), IrRejection> {
    let split = self.emit(Instruction::Accept)?;
    let body_start = self.next_pc()?;
    let progress = ProgressId::from_index(self.progress_count).map_err(map_program_error)?;
    self.progress_count = self.progress_count.saturating_add(1);
    self.emit(Instruction::MarkProgress { progress })?;
    self.compile_iteration(body, captures)?;
    let branch = self.emit(Instruction::Accept)?;
    let after = self.next_pc()?;
    self.instructions[split.index()] = ordered_split(body_start, after, greedy);
    self.instructions[branch.index()] = Instruction::ContinueProgress {
      progress,
      changed: split,
      unchanged: after,
    };
    Ok(())
  }

  fn entry_strategy(
    &self,
    node: &IrNode,
  ) -> Result<EntryStrategy, IrRejection> {
    match node {
      IrNode::Literal(symbol) => Ok(EntryStrategy::SingleLiteral { symbol: *symbol }),
      IrNode::Class(_) => Ok(EntryStrategy::SingleClass {
        class: ClassId::from_index(0).map_err(map_program_error)?,
      }),
      IrNode::Dot => Ok(EntryStrategy::SingleDot),
      IrNode::Assert(IrAssertion::Start) => Ok(EntryStrategy::Anchored),
      IrNode::Sequence(nodes)
        if matches!(nodes.first(), Some(IrNode::Assert(IrAssertion::Start))) =>
      {
        Ok(EntryStrategy::Anchored)
      }
      _ => Ok(EntryStrategy::GeneralSearch),
    }
  }

  fn add_class(
    &mut self,
    class: CharacterClass,
  ) -> Result<ClassId, IrRejection> {
    let id = ClassId::from_index(self.classes.len()).map_err(map_program_error)?;
    self.classes.push(class);
    Ok(id)
  }

  fn add_capture_set(
    &mut self,
    captures: Box<[crate::ir::CaptureId]>,
  ) -> Result<CaptureSetId, IrRejection> {
    let id = CaptureSetId::from_index(self.capture_sets.len()).map_err(map_program_error)?;
    self.capture_sets.push(captures);
    Ok(id)
  }

  fn emit(
    &mut self,
    instruction: Instruction,
  ) -> Result<ProgramCounter, IrRejection> {
    if self.instructions.len() >= MAX_LOWERED_INSTRUCTIONS {
      return Err(IrRejection::ProgramTooLarge);
    }
    let pc = self.next_pc()?;
    self.instructions.push(instruction);
    Ok(pc)
  }

  fn next_pc(&self) -> Result<ProgramCounter, IrRejection> {
    ProgramCounter::from_index(self.instructions.len()).map_err(map_program_error)
  }
}

fn ordered_split(
  body: ProgramCounter,
  after: ProgramCounter,
  greedy: bool,
) -> Instruction {
  let (preferred, deferred) = if greedy { (body, after) } else { (after, body) };
  Instruction::Split {
    preferred,
    deferred,
  }
}

const fn map_program_error(_: ProgramError) -> IrRejection {
  IrRejection::ProgramTooLarge
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::ast::{CharacterClass, ClassTerm};

  #[test]
  fn lowers_a_single_class_to_a_verified_search_plan() {
    let ir = IrNode::Class(CharacterClass {
      inverted: false,
      terms: vec![ClassTerm::Digit(true)],
    });
    let program = compile(&ir, 0).expect("compiled class");
    assert!(matches!(
      program.strategy(),
      EntryStrategy::SingleClass { .. }
    ));
  }

  #[test]
  fn lowers_ordered_alternatives_and_captures() {
    let ir = IrNode::Alternative(Box::new([
      IrNode::Capture {
        capture: crate::ir::CaptureId::from_index(0).expect("capture"),
        body: Box::new(IrNode::Literal(u32::from(b'a'))),
      },
      IrNode::Literal(u32::from(b'b')),
    ]));
    let program = compile(&ir, 1).expect("compiled alternative");
    assert!(
      program
        .instructions()
        .iter()
        .any(|instruction| matches!(instruction, Instruction::Split { .. }))
    );
  }

  #[test]
  fn nullable_unbounded_repeat_has_a_progress_guard() {
    let ir = IrNode::Repeat {
      body: Box::new(IrNode::Empty),
      min: 0,
      max: None,
      greedy: true,
    };
    let program = compile(&ir, 0).expect("compiled nullable repeat");
    assert!(
      program
        .instructions()
        .iter()
        .any(|instruction| matches!(instruction, Instruction::ContinueProgress { .. }))
    );
  }
}
