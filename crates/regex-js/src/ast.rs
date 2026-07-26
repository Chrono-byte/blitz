// SPDX-License-Identifier: MPL-2.0

#[derive(Clone, Debug)]
pub(crate) enum Node {
  Empty,
  Literal(u32),
  Dot,
  Class(CharacterClass),
  Assertion(Assertion),
  Backreference(usize),
  Sequence(Vec<Self>),
  Alternative(Vec<Self>),
  Group {
    index: Option<usize>,
    body: Box<Self>,
  },
  Quantifier {
    body: Box<Self>,
    min: usize,
    max: Option<usize>,
    greedy: bool,
  },
}

#[derive(Clone, Debug)]
pub(crate) enum Assertion {
  Start,
  End,
  WordBoundary(bool),
  Lookahead { positive: bool, body: Box<Node> },
}

#[derive(Clone, Debug)]
pub(crate) struct CharacterClass {
  pub(crate) inverted: bool,
  pub(crate) terms: Vec<ClassTerm>,
}

#[derive(Clone, Debug)]
pub(crate) enum ClassTerm {
  Range(u32, u32),
  Characters(Vec<u32>),
  Digit(bool),
  Space(bool),
  Word(bool),
}
