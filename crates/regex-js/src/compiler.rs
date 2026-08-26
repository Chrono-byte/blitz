// SPDX-License-Identifier: MPL-2.0
//! Lower typed semantic regex IR into verified control-flow instructions.

use crate::analysis::Analysis;
use crate::ast::CharacterClass;
use crate::ir::{IrAssertion, IrNode, IrRejection};
use crate::program::{
  ClassId, EntryStrategy, Instruction, Program, ProgramCounter, ProgramError, VerifiedProgram,
};

const MAX_LOWERED_INSTRUCTIONS: usize = 1_000_000;

pub(crate) fn compile(node: &IrNode) -> Result<VerifiedProgram, IrRejection> {
  let analysis = Analysis::for_ir(node);
  let mut compiler = Compiler::default();
  compiler.compile_node(node)?;
  compiler.emit(Instruction::Accept)?;
  let strategy = compiler.entry_strategy(node)?;
  let program = Program {
    instructions: compiler.instructions.into_boxed_slice(),
    classes: compiler.classes.into_boxed_slice(),
    entry: ProgramCounter::from_index(0).map_err(map_program_error)?,
    strategy,
    analysis,
  };
  VerifiedProgram::verify(program).map_err(map_program_error)
}

#[derive(Default)]
struct Compiler {
  instructions: Vec<Instruction>,
  classes: Vec<CharacterClass>,
}

impl Compiler {
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
      IrNode::Sequence(nodes) => {
        for node in nodes {
          self.compile_node(node)?;
        }
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

  fn compile_repeat(
    &mut self,
    body: &IrNode,
    min: usize,
    max: Option<usize>,
    greedy: bool,
  ) -> Result<(), IrRejection> {
    if Analysis::for_ir(body).nullable {
      return Err(IrRejection::NullableRepeat);
    }
    for _ in 0..min {
      self.compile_node(body)?;
    }
    match max {
      Some(max) => {
        let optional = max
          .checked_sub(min)
          .ok_or(IrRejection::ProgramTooLarge)?;
        for _ in 0..optional {
          self.compile_optional(body, greedy)?;
        }
      }
      None => self.compile_star(body, greedy)?,
    }
    Ok(())
  }

  fn compile_optional(
    &mut self,
    body: &IrNode,
    greedy: bool,
  ) -> Result<(), IrRejection> {
    let split = self.emit(Instruction::Accept)?;
    let body_start = self.next_pc()?;
    self.compile_node(body)?;
    let after = self.next_pc()?;
    self.instructions[split.index()] = ordered_split(body_start, after, greedy);
    Ok(())
  }

  fn compile_star(
    &mut self,
    body: &IrNode,
    greedy: bool,
  ) -> Result<(), IrRejection> {
    let split = self.emit(Instruction::Accept)?;
    let body_start = self.next_pc()?;
    self.compile_node(body)?;
    self.emit(Instruction::Jump { target: split })?;
    let after = self.next_pc()?;
    self.instructions[split.index()] = ordered_split(body_start, after, greedy);
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
    let program = compile(&ir).expect("compiled class");
    assert_eq!(
      program.strategy(),
      EntryStrategy::SingleClass {
        class: ClassId::from_index(0).expect("class")
      }
    );
    assert_eq!(program.instructions().len(), 2);
  }

  #[test]
  fn greedy_star_prefers_the_body_and_retains_an_exit() {
    let ir = IrNode::Repeat {
      body: Box::new(IrNode::Literal(u32::from(b'a'))),
      min: 0,
      max: None,
      greedy: true,
    };
    let program = compile(&ir).expect("compiled star");
    assert!(matches!(
      program.instructions()[0],
      Instruction::Split {
        preferred,
        deferred,
      } if preferred.index() == 1 && deferred.index() == 3
    ));
  }

  #[test]
  fn rejects_a_nullable_repetition() {
    let ir = IrNode::Repeat {
      body: Box::new(IrNode::Empty),
      min: 0,
      max: None,
      greedy: true,
    };
    assert!(matches!(compile(&ir), Err(IrRejection::NullableRepeat)));
  }
}
