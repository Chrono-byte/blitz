// SPDX-License-Identifier: MPL-2.0
//! Typed semantic regular-expression IR.

use crate::ast::{Assertion, CharacterClass, Node};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct CaptureId(u32);

impl CaptureId {
  pub(crate) fn from_index(index: usize) -> Result<Self, IrRejection> {
    u32::try_from(index)
      .map(Self)
      .map_err(|_| IrRejection::ProgramTooLarge)
  }

  pub(crate) const fn index(self) -> usize {
    self.0 as usize
  }
}

#[derive(Clone, Debug)]
pub(crate) enum IrNode {
  Empty,
  Literal(u32),
  Dot,
  Class(CharacterClass),
  Assert(IrAssertion),
  Backreference(CaptureId),
  Sequence(Box<[Self]>),
  Alternative(Box<[Self]>),
  Capture {
    capture: CaptureId,
    body: Box<Self>,
  },
  Repeat {
    body: Box<Self>,
    min: usize,
    max: Option<usize>,
    greedy: bool,
  },
}

#[derive(Clone, Debug)]
pub(crate) enum IrAssertion {
  Start,
  End,
  WordBoundary(bool),
  Lookahead { positive: bool, body: Box<IrNode> },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum IrRejection {
  ProgramTooLarge,
}

impl IrNode {
  pub(crate) fn from_ast(node: &Node) -> Result<Self, IrRejection> {
    match node {
      Node::Empty => Ok(Self::Empty),
      Node::Literal(symbol) => Ok(Self::Literal(*symbol)),
      Node::Dot => Ok(Self::Dot),
      Node::Class(class) => Ok(Self::Class(class.clone())),
      Node::Assertion(Assertion::Start) => Ok(Self::Assert(IrAssertion::Start)),
      Node::Assertion(Assertion::End) => Ok(Self::Assert(IrAssertion::End)),
      Node::Assertion(Assertion::WordBoundary(expected)) => {
        Ok(Self::Assert(IrAssertion::WordBoundary(*expected)))
      }
      Node::Assertion(Assertion::Lookahead { positive, body }) => {
        Ok(Self::Assert(IrAssertion::Lookahead {
          positive: *positive,
          body: Box::new(Self::from_ast(body)?),
        }))
      }
      Node::Backreference(index) => Ok(Self::Backreference(CaptureId::from_index(*index - 1)?)),
      Node::Sequence(nodes) => lower_nodes(nodes).map(Self::Sequence),
      Node::Alternative(nodes) => lower_nodes(nodes).map(Self::Alternative),
      Node::Group { index, body } => index.map_or_else(
        || Self::from_ast(body),
        |index| {
          Ok(Self::Capture {
            capture: CaptureId::from_index(index - 1)?,
            body: Box::new(Self::from_ast(body)?),
          })
        },
      ),
      Node::Quantifier {
        body,
        min,
        max,
        greedy,
      } => Ok(Self::Repeat {
        body: Box::new(Self::from_ast(body)?),
        min: *min,
        max: *max,
        greedy: *greedy,
      }),
    }
  }
}

fn lower_nodes(nodes: &[Node]) -> Result<Box<[IrNode]>, IrRejection> {
  nodes
    .iter()
    .map(IrNode::from_ast)
    .collect::<Result<Vec<_>, _>>()
    .map(Vec::into_boxed_slice)
}
