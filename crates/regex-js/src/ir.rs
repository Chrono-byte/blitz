// SPDX-License-Identifier: MPL-2.0
//! Typed semantic regular-expression IR.

use crate::ast::{Assertion, CharacterClass, Node};

#[derive(Clone, Debug)]
pub(crate) enum IrNode {
  Empty,
  Literal(u32),
  Dot,
  Class(CharacterClass),
  Assert(IrAssertion),
  Sequence(Box<[Self]>),
  Repeat {
    body: Box<Self>,
    min: usize,
    max: Option<usize>,
    greedy: bool,
  },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum IrAssertion {
  Start,
  End,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum IrRejection {
  CapturingGroup,
  Alternative,
  Lookahead,
  WordBoundary,
  Backreference,
  NullableRepeat,
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
      Node::Assertion(Assertion::WordBoundary(_)) => Err(IrRejection::WordBoundary),
      Node::Assertion(Assertion::Lookahead { .. }) => Err(IrRejection::Lookahead),
      Node::Backreference(_) => Err(IrRejection::Backreference),
      Node::Sequence(nodes) => nodes
        .iter()
        .map(Self::from_ast)
        .collect::<Result<Vec<_>, _>>()
        .map(Vec::into_boxed_slice)
        .map(Self::Sequence),
      Node::Alternative(_) => Err(IrRejection::Alternative),
      Node::Group { index, body } => {
        if index.is_some() {
          Err(IrRejection::CapturingGroup)
        } else {
          Self::from_ast(body)
        }
      }
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
