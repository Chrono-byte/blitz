// SPDX-License-Identifier: MPL-2.0
use std::ops::Range;
use std::{error, fmt};

use crate::ast::{Assertion, CharacterClass, ClassTerm, Node};
use crate::canonical::{range_contains, symbols_equal};
use crate::{Flags, Input, Regex};

const MIN_MATCH_STEPS: usize = 1_000_000;
const MAX_MATCH_STEPS: usize = 10_000_000;
const MATCH_STEPS_PER_INPUT_UNIT: usize = 32;
const MAX_MATCH_DEPTH: usize = 2_048;

/// A regular-expression match exceeded a host-safety resource limit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MatchError;

impl fmt::Display for MatchError {
  fn fmt(
    &self,
    formatter: &mut fmt::Formatter<'_>,
  ) -> fmt::Result {
    formatter.write_str("regular-expression match resource limit exceeded")
  }
}

impl error::Error for MatchError {}

struct MatchContext {
  remaining_steps: usize,
  depth: usize,
}

impl MatchContext {
  fn new(input_length: usize) -> Self {
    Self {
      remaining_steps: input_length
        .saturating_mul(MATCH_STEPS_PER_INPUT_UNIT)
        .clamp(MIN_MATCH_STEPS, MAX_MATCH_STEPS),
      depth: 0,
    }
  }

  fn enter(&mut self) -> Result<(), MatchError> {
    if self.remaining_steps == 0 || self.depth >= MAX_MATCH_DEPTH {
      return Err(MatchError);
    }
    self.remaining_steps -= 1;
    self.depth += 1;
    Ok(())
  }

  fn exit(&mut self) {
    self.depth -= 1;
  }
}

/// The UTF-16 span of one capture.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Capture {
  range: Range<usize>,
}

impl Capture {
  /// Return this capture's UTF-16 code-unit range.
  #[must_use]
  pub fn range(&self) -> Range<usize> {
    self.range.clone()
  }
}

/// One regular-expression match and its captures.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Match {
  captures: Vec<Option<Capture>>,
}

impl Match {
  pub(crate) fn captureless(range: Range<usize>) -> Self {
    Self {
      captures: vec![Some(Capture { range })],
    }
  }

  pub(crate) fn from_captures(
    range: Range<usize>,
    captures: Vec<Option<Range<usize>>>,
  ) -> Self {
    let mut values = Vec::with_capacity(captures.len() + 1);
    values.push(Some(Capture { range }));
    values.extend(
      captures
        .into_iter()
        .map(|range| range.map(|range| Capture { range })),
    );
    Self { captures: values }
  }

  /// Return the complete match's UTF-16 range.
  #[must_use]
  pub fn range(&self) -> Range<usize> {
    self
      .captures
      .first()
      .and_then(Option::as_ref)
      .map_or(0..0, Capture::range)
  }

  /// Return a capture by index. Capture zero is the complete match.
  #[must_use]
  pub fn capture(
    &self,
    index: usize,
  ) -> Option<&Capture> {
    self.captures.get(index).and_then(Option::as_ref)
  }

  /// Return all captures, including capture zero.
  #[must_use]
  pub fn captures(&self) -> &[Option<Capture>] {
    &self.captures
  }
}

#[derive(Clone)]
struct State {
  offset: usize,
  captures: Vec<Option<Range<usize>>>,
}

pub(crate) fn find(
  regex: &Regex,
  input: &Input,
  start: usize,
) -> Result<Option<Match>, MatchError> {
  if start > input.len() {
    return Ok(None);
  }
  let mut context = MatchContext::new(input.len());
  let initial = start;
  let mut candidate = start;
  loop {
    let state = State {
      offset: candidate,
      captures: vec![None; regex.capture_count + 1],
    };
    if let Some(mut matched) =
      match_node_with_context(&regex.body, state, input, regex.flags, &mut context)?
        .into_iter()
        .next()
    {
      matched.captures[0] = Some(candidate..matched.offset);
      return Ok(Some(Match {
        captures: matched
          .captures
          .into_iter()
          .map(|range| range.map(|range| Capture { range }))
          .collect(),
      }));
    }
    if regex.flags.sticky() || candidate == input.len() {
      return Ok(None);
    }
    candidate = input.advance_string_index(candidate, regex.flags.unicode());
    if candidate <= initial || candidate > input.len() {
      return Ok(None);
    }
  }
}

fn match_node_with_context(
  node: &Node,
  state: State,
  input: &Input,
  flags: Flags,
  context: &mut MatchContext,
) -> Result<Vec<State>, MatchError> {
  context.enter()?;
  let result = match node {
    Node::Empty => Ok(vec![state]),
    Node::Literal(expected) => Ok(match_symbol(state, input, flags, |actual| {
      symbols_equal(*expected, actual, flags)
    })),
    Node::Dot => Ok(match_symbol(state, input, flags, |actual| {
      flags.dot_all() || !is_line_terminator(actual)
    })),
    Node::Class(class) => Ok(match_symbol(state, input, flags, |actual| {
      class_matches(class, actual, flags)
    })),
    Node::Assertion(assertion) => match_assertion(assertion, state, input, flags, context),
    Node::Backreference(index) => Ok(match_backreference(*index, state, input, flags)),
    Node::Sequence(nodes) => match_sequence(nodes, state, input, flags, context),
    Node::Alternative(alternatives) => {
      let mut matches = Vec::new();
      for alternative in alternatives {
        matches.extend(match_node_with_context(
          alternative,
          state.clone(),
          input,
          flags,
          context,
        )?);
      }
      Ok(matches)
    }
    Node::Group { index, body } => {
      let start = state.offset;
      Ok(
        match_node_with_context(body, state, input, flags, context)?
          .into_iter()
          .map(|mut matched| {
            if let Some(index) = index {
              matched.captures[*index] = Some(start..matched.offset);
            }
            matched
          })
          .collect(),
      )
    }
    Node::Quantifier {
      body,
      min,
      max,
      greedy,
    } => {
      let mut matches = Vec::new();
      let repetition_limit = max.unwrap_or_else(|| input.len().saturating_add(1));
      let mut capture_indices = Vec::new();
      collect_capture_indices(body, &mut capture_indices);
      repeat(
        body,
        state,
        0,
        *min,
        repetition_limit,
        *greedy,
        input,
        flags,
        &capture_indices,
        &mut matches,
        context,
      )?;
      Ok(matches)
    }
  };
  context.exit();
  result
}

fn match_sequence(
  nodes: &[Node],
  state: State,
  input: &Input,
  flags: Flags,
  context: &mut MatchContext,
) -> Result<Vec<State>, MatchError> {
  let mut states = vec![state];
  for node in nodes {
    let mut next_states = Vec::new();
    for state in states {
      next_states.extend(match_node_with_context(node, state, input, flags, context)?);
    }
    states = next_states;
    if states.is_empty() {
      break;
    }
  }
  Ok(states)
}

#[allow(clippy::too_many_arguments)]
fn repeat(
  body: &Node,
  state: State,
  count: usize,
  min: usize,
  max: usize,
  greedy: bool,
  input: &Input,
  flags: Flags,
  capture_indices: &[usize],
  output: &mut Vec<State>,
  context: &mut MatchContext,
) -> Result<(), MatchError> {
  context.enter()?;
  if !greedy && count >= min {
    output.push(state.clone());
  }
  if count < max {
    let mut iteration_state = state.clone();
    for index in capture_indices {
      iteration_state.captures[*index] = None;
    }
    for next in match_node_with_context(body, iteration_state, input, flags, context)? {
      let progressed = next.offset != state.offset;
      let next_max = if progressed || count + 1 < min {
        max
      } else {
        count + 1
      };
      repeat(
        body,
        next,
        count + 1,
        min,
        next_max,
        greedy,
        input,
        flags,
        capture_indices,
        output,
        context,
      )?;
    }
  }
  if greedy && count >= min {
    output.push(state);
  }
  context.exit();
  Ok(())
}

fn match_symbol(
  mut state: State,
  input: &Input,
  flags: Flags,
  predicate: impl FnOnce(u32) -> bool,
) -> Vec<State> {
  let Some(point) = input.code_point_at(state.offset, flags.unicode()) else {
    return Vec::new();
  };
  let actual = point.value();
  if !predicate(actual) {
    return Vec::new();
  }
  state.offset += point.width();
  vec![state]
}

fn match_assertion(
  assertion: &Assertion,
  state: State,
  input: &Input,
  flags: Flags,
  context: &mut MatchContext,
) -> Result<Vec<State>, MatchError> {
  let satisfied = match assertion {
    Assertion::Start => {
      state.offset == 0
        || (flags.multiline()
          && previous_symbol(input, state.offset, flags.unicode()).is_some_and(is_line_terminator))
    }
    Assertion::End => {
      state.offset == input.len()
        || (flags.multiline()
          && input
            .code_point_at(state.offset, flags.unicode())
            .is_some_and(|point| is_line_terminator(point.value())))
    }
    Assertion::WordBoundary(expected) => {
      let before = previous_symbol(input, state.offset, flags.unicode())
        .is_some_and(|symbol| is_word_character(symbol, flags));
      let after = input
        .code_point_at(state.offset, flags.unicode())
        .is_some_and(|point| is_word_character(point.value(), flags));
      (before != after) == *expected
    }
    Assertion::Lookahead { positive, body } => {
      let matched = match_node_with_context(body, state.clone(), input, flags, context)?
        .into_iter()
        .next();
      if *positive {
        return Ok(
          matched
            .map(|mut matched| {
              matched.offset = state.offset;
              vec![matched]
            })
            .unwrap_or_default(),
        );
      }
      return Ok(if matched.is_none() {
        vec![state]
      } else {
        Vec::new()
      });
    }
  };
  Ok(if satisfied { vec![state] } else { Vec::new() })
}

fn match_backreference(
  index: usize,
  mut state: State,
  input: &Input,
  flags: Flags,
) -> Vec<State> {
  let Some(Some(range)) = state.captures.get(index) else {
    return vec![state];
  };
  let mut expected_offset = range.start;
  let mut actual_offset = state.offset;
  while expected_offset < range.end {
    let Some(expected) = input.code_point_at(expected_offset, flags.unicode()) else {
      return Vec::new();
    };
    let Some(actual) = input.code_point_at(actual_offset, flags.unicode()) else {
      return Vec::new();
    };
    if !symbols_equal(expected.value(), actual.value(), flags) {
      return Vec::new();
    }
    expected_offset += expected.width();
    actual_offset += actual.width();
  }
  state.offset = actual_offset;
  vec![state]
}

pub(crate) fn class_matches(
  class: &CharacterClass,
  symbol: u32,
  flags: Flags,
) -> bool {
  let matched = class.terms.iter().any(|term| match term {
    ClassTerm::Range(start, end) => range_contains(*start, *end, symbol, flags),
    ClassTerm::Characters(characters) => characters
      .iter()
      .any(|character| symbols_equal(*character, symbol, flags)),
    ClassTerm::Digit(positive) => is_digit(symbol) == *positive,
    ClassTerm::Space(positive) => is_space(symbol) == *positive,
    ClassTerm::Word(positive) => is_word_character(symbol, flags) == *positive,
  });
  matched != class.inverted
}

fn collect_capture_indices(
  node: &Node,
  output: &mut Vec<usize>,
) {
  match node {
    Node::Group { index, body } => {
      if let Some(index) = index {
        output.push(*index);
      }
      collect_capture_indices(body, output);
    }
    Node::Sequence(nodes) | Node::Alternative(nodes) => {
      for child in nodes {
        collect_capture_indices(child, output);
      }
    }
    Node::Quantifier { body, .. } | Node::Assertion(Assertion::Lookahead { body, .. }) => {
      collect_capture_indices(body, output);
    }
    _ => {}
  }
}

const fn is_digit(symbol: u32) -> bool {
  symbol >= b'0' as u32 && symbol <= b'9' as u32
}

const fn is_word(symbol: u32) -> bool {
  is_digit(symbol)
    || (symbol >= b'A' as u32 && symbol <= b'Z' as u32)
    || (symbol >= b'a' as u32 && symbol <= b'z' as u32)
    || symbol == b'_' as u32
}

pub(crate) fn is_word_character(
  symbol: u32,
  flags: Flags,
) -> bool {
  is_word(symbol)
    || (flags.unicode()
      && flags.ignore_case()
      && is_word(crate::canonical::canonicalize(symbol, flags)))
}

fn is_space(symbol: u32) -> bool {
  matches!(
    symbol,
    0x0009..=0x000D
      | 0x0020
      | 0x00A0
      | 0x1680
      | 0x2000..=0x200A
      | 0x2028
      | 0x2029
      | 0x202F
      | 0x205F
      | 0x3000
      | 0xFEFF
  )
}

pub(crate) const fn is_line_terminator(symbol: u32) -> bool {
  matches!(symbol, 0x000A | 0x000D | 0x2028 | 0x2029)
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
