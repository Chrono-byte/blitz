// SPDX-License-Identifier: MPL-2.0
use std::collections::HashMap;

use crate::ast::{Assertion, CharacterClass, ClassTerm, Node};
use crate::{Error, ErrorKind, Flags};
use js_string::{JsString, JsStringBuilder};

pub(crate) struct ParsedPattern {
  pub(crate) body: Node,
  pub(crate) capture_count: usize,
  pub(crate) capture_names: Vec<Option<JsString>>,
}

pub(crate) fn parse(
  source: &JsString,
  flags: Flags,
) -> Result<ParsedPattern, Error> {
  Parser::new(source, flags)?.parse()
}

struct Parser {
  units: Vec<u16>,
  cursor: usize,
  flags: Flags,
  capture_count: usize,
  total_capture_count: usize,
  capture_names: Vec<Option<JsString>>,
  named_indices: HashMap<JsString, usize>,
}

impl Parser {
  fn new(
    source: &JsString,
    flags: Flags,
  ) -> Result<Self, Error> {
    let units = source.as_slice().to_vec();
    let scan = scan_capturing_groups(&units)?;
    Ok(Self {
      units,
      cursor: 0,
      flags,
      capture_count: 0,
      total_capture_count: scan.count,
      capture_names: scan.names,
      named_indices: scan.named_indices,
    })
  }

  fn parse(mut self) -> Result<ParsedPattern, Error> {
    let body = self.parse_alternative(None)?;
    if self.cursor != self.units.len() {
      return Err(self.error(ErrorKind::UnexpectedToken, "unexpected pattern token"));
    }
    Ok(ParsedPattern {
      body,
      capture_count: self.capture_count,
      capture_names: self.capture_names,
    })
  }

  fn parse_alternative(
    &mut self,
    terminator: Option<u16>,
  ) -> Result<Node, Error> {
    let mut alternatives = Vec::new();
    let mut sequence = Vec::new();
    while let Some(unit) = self.peek() {
      if Some(unit) == terminator {
        break;
      }
      if unit == ascii(b'|') {
        self.cursor += 1;
        alternatives.push(sequence_node(std::mem::take(&mut sequence)));
        continue;
      }
      let atom = self.parse_atom()?;
      sequence.push(self.parse_quantifier(atom)?);
    }
    alternatives.push(sequence_node(sequence));
    if alternatives.len() == 1 {
      Ok(alternatives.remove(0))
    } else {
      Ok(Node::Alternative(alternatives))
    }
  }

  fn parse_atom(&mut self) -> Result<Node, Error> {
    let Some(unit) = self.next() else {
      return Err(self.error(ErrorKind::UnexpectedEnd, "expected pattern atom"));
    };
    match unit {
      value if value == ascii(b'.') => Ok(Node::Dot),
      value if value == ascii(b'^') => Ok(Node::Assertion(Assertion::Start)),
      value if value == ascii(b'$') => Ok(Node::Assertion(Assertion::End)),
      value if value == ascii(b'[') => self.parse_class(),
      value if value == ascii(b'(') => self.parse_group(),
      value if value == ascii(b'\\') => self.parse_escape(false),
      value if value == ascii(b'*') || value == ascii(b'+') || value == ascii(b'?') => {
        Err(self.error(
          ErrorKind::InvalidQuantifier,
          "quantifier has no preceding atom",
        ))
      }
      value if value == ascii(b')') => {
        Err(self.error(ErrorKind::UnexpectedToken, "unmatched closing parenthesis"))
      }
      value
        if self.flags.unicode()
          && (value == ascii(b'{') || value == ascii(b'}') || value == ascii(b']')) =>
      {
        Err(self.error(
          ErrorKind::UnexpectedToken,
          "unescaped syntax character in Unicode pattern",
        ))
      }
      value => Ok(Node::Literal(self.literal_symbol(value))),
    }
  }

  fn parse_group(&mut self) -> Result<Node, Error> {
    let mut capture = true;
    let mut assertion = None;
    let mut group_name = None;
    if self.consume(ascii(b'?')) {
      match self.next() {
        Some(value) if value == ascii(b':') => capture = false,
        Some(value) if value == ascii(b'=') => {
          capture = false;
          assertion = Some(true);
        }
        Some(value) if value == ascii(b'!') => {
          capture = false;
          assertion = Some(false);
        }
        Some(value) if value == ascii(b'<') => {
          if matches!(self.peek(), Some(next) if next == ascii(b'=') || next == ascii(b'!')) {
            return Err(self.error(
              ErrorKind::UnexpectedToken,
              "lookbehind assertions are not implemented",
            ));
          }
          group_name = Some(parse_group_name(&self.units, &mut self.cursor)?);
        }
        _ => {
          return Err(self.error(
            ErrorKind::UnexpectedToken,
            "unsupported or invalid group prefix",
          ));
        }
      }
    }

    let index = if capture {
      self.capture_count += 1;
      if let Some(group_name) = &group_name
        && self
          .capture_names
          .get(self.capture_count)
          .and_then(Option::as_ref)
          != Some(group_name)
      {
        return Err(self.error(
          ErrorKind::InvalidGroupName,
          "named capture numbering is inconsistent",
        ));
      }
      Some(self.capture_count)
    } else {
      None
    };
    let body = self.parse_alternative(Some(ascii(b')')))?;
    if !self.consume(ascii(b')')) {
      return Err(self.error(ErrorKind::UnterminatedGroup, "unterminated group"));
    }
    if let Some(positive) = assertion {
      Ok(Node::Assertion(Assertion::Lookahead {
        positive,
        body: Box::new(body),
      }))
    } else {
      Ok(Node::Group {
        index,
        body: Box::new(body),
      })
    }
  }

  fn parse_class(&mut self) -> Result<Node, Error> {
    let inverted = self.consume(ascii(b'^'));
    let mut terms = Vec::new();
    while let Some(unit) = self.peek() {
      if unit == ascii(b']') {
        self.cursor += 1;
        return Ok(Node::Class(CharacterClass { inverted, terms }));
      }
      let left = self.parse_class_term()?;
      if self.peek() == Some(ascii(b'-'))
        && self.units.get(self.cursor + 1).copied() != Some(ascii(b']'))
      {
        self.cursor += 1;
        let right = self.parse_class_term()?;
        let (ClassTerm::Range(start, start_end), ClassTerm::Range(end, end_end)) = (&left, &right)
        else {
          if !self.flags.unicode() {
            terms.push(left);
            terms.push(ClassTerm::Range(
              u32::from(ascii(b'-')),
              u32::from(ascii(b'-')),
            ));
            terms.push(right);
            continue;
          }
          return Err(self.error(
            ErrorKind::InvalidRange,
            "character-class range endpoints must be characters",
          ));
        };
        if start != start_end || end != end_end || start > end {
          return Err(self.error(ErrorKind::InvalidRange, "invalid character-class range"));
        }
        terms.push(ClassTerm::Range(*start, *end));
      } else {
        terms.push(left);
      }
    }
    Err(self.error(ErrorKind::UnterminatedClass, "unterminated character class"))
  }

  fn parse_class_term(&mut self) -> Result<ClassTerm, Error> {
    let Some(unit) = self.next() else {
      return Err(self.error(ErrorKind::UnterminatedClass, "unterminated character class"));
    };
    if unit != ascii(b'\\') {
      let symbol = self.literal_symbol(unit);
      return Ok(ClassTerm::Range(symbol, symbol));
    }
    let Some(escaped) = self.next() else {
      return Err(self.error(ErrorKind::InvalidEscape, "trailing escape"));
    };
    match escaped {
      value if value == ascii(b'd') => Ok(ClassTerm::Digit(true)),
      value if value == ascii(b'D') => Ok(ClassTerm::Digit(false)),
      value if value == ascii(b's') => Ok(ClassTerm::Space(true)),
      value if value == ascii(b'S') => Ok(ClassTerm::Space(false)),
      value if value == ascii(b'w') => Ok(ClassTerm::Word(true)),
      value if value == ascii(b'W') => Ok(ClassTerm::Word(false)),
      value if value == ascii(b'b') => Ok(ClassTerm::Range(8, 8)),
      value if value == ascii(b'f') => Ok(ClassTerm::Range(12, 12)),
      value if value == ascii(b'n') => Ok(ClassTerm::Range(10, 10)),
      value if value == ascii(b'r') => Ok(ClassTerm::Range(13, 13)),
      value if value == ascii(b't') => Ok(ClassTerm::Range(9, 9)),
      value if value == ascii(b'v') => Ok(ClassTerm::Range(11, 11)),
      value if value == ascii(b'c') => self.parse_class_control_escape(),
      value if value == ascii(b'0') => {
        if self.peek().is_some_and(is_ascii_digit) {
          if self.flags.unicode() {
            return Err(self.error(
              ErrorKind::InvalidEscape,
              "legacy octal escape is invalid in Unicode mode",
            ));
          }
          let symbol = self.parse_legacy_octal(escaped);
          Ok(ClassTerm::Range(symbol, symbol))
        } else {
          Ok(ClassTerm::Range(0, 0))
        }
      }
      value if (ascii(b'1')..=ascii(b'9')).contains(&value) => {
        if self.flags.unicode() {
          return Err(self.error(
            ErrorKind::InvalidEscape,
            "decimal escape is invalid in a Unicode character class",
          ));
        }
        let symbol = if value <= ascii(b'7') {
          self.parse_legacy_octal(value)
        } else {
          u32::from(value)
        };
        Ok(ClassTerm::Range(symbol, symbol))
      }
      value if value == ascii(b'x') => {
        let Some(symbol) = self.try_parse_fixed_hex(2) else {
          if self.flags.unicode() {
            return Err(self.error(ErrorKind::InvalidEscape, "invalid hexadecimal escape"));
          }
          return Ok(ClassTerm::Range(u32::from(value), u32::from(value)));
        };
        Ok(ClassTerm::Range(symbol, symbol))
      }
      value if value == ascii(b'u') => {
        let Some(symbol) = self.try_parse_unicode_escape()? else {
          return Ok(ClassTerm::Range(u32::from(value), u32::from(value)));
        };
        Ok(ClassTerm::Range(symbol, symbol))
      }
      value if self.identity_escape_allowed(value, true) => {
        Ok(ClassTerm::Range(u32::from(value), u32::from(value)))
      }
      _ => Err(self.error(ErrorKind::InvalidEscape, "invalid character-class escape")),
    }
  }

  fn parse_escape(
    &mut self,
    _in_class: bool,
  ) -> Result<Node, Error> {
    let Some(unit) = self.next() else {
      return Err(self.error(ErrorKind::InvalidEscape, "trailing escape"));
    };
    match unit {
      value if value == ascii(b'b') => Ok(Node::Assertion(Assertion::WordBoundary(true))),
      value if value == ascii(b'B') => Ok(Node::Assertion(Assertion::WordBoundary(false))),
      value if value == ascii(b'd') => Ok(class_node(ClassTerm::Digit(true))),
      value if value == ascii(b'D') => Ok(class_node(ClassTerm::Digit(false))),
      value if value == ascii(b's') => Ok(class_node(ClassTerm::Space(true))),
      value if value == ascii(b'S') => Ok(class_node(ClassTerm::Space(false))),
      value if value == ascii(b'w') => Ok(class_node(ClassTerm::Word(true))),
      value if value == ascii(b'W') => Ok(class_node(ClassTerm::Word(false))),
      value if value == ascii(b'f') => Ok(Node::Literal(12)),
      value if value == ascii(b'n') => Ok(Node::Literal(10)),
      value if value == ascii(b'r') => Ok(Node::Literal(13)),
      value if value == ascii(b't') => Ok(Node::Literal(9)),
      value if value == ascii(b'v') => Ok(Node::Literal(11)),
      value if value == ascii(b'c') => self.parse_control_escape(),
      value if value == ascii(b'0') => {
        if self.peek().is_some_and(is_ascii_digit) {
          if self.flags.unicode() {
            return Err(self.error(
              ErrorKind::InvalidEscape,
              "legacy octal escape is invalid in Unicode mode",
            ));
          }
          Ok(Node::Literal(self.parse_legacy_octal(value)))
        } else {
          Ok(Node::Literal(0))
        }
      }
      value if value == ascii(b'x') => {
        let Some(symbol) = self.try_parse_fixed_hex(2) else {
          if self.flags.unicode() {
            return Err(self.error(ErrorKind::InvalidEscape, "invalid hexadecimal escape"));
          }
          return Ok(Node::Literal(u32::from(value)));
        };
        Ok(Node::Literal(symbol))
      }
      value if value == ascii(b'k') && !self.named_indices.is_empty() => {
        if !self.consume(ascii(b'<')) {
          return Err(self.error(
            ErrorKind::InvalidGroupName,
            "named backreference is missing '<'",
          ));
        }
        let name = parse_group_name(&self.units, &mut self.cursor)?;
        let Some(index) = self.named_indices.get(&name).copied() else {
          return Err(self.error(ErrorKind::InvalidGroupName, "unknown named backreference"));
        };
        Ok(Node::Backreference(index))
      }
      value if value == ascii(b'u') => {
        let Some(symbol) = self.try_parse_unicode_escape()? else {
          return Ok(Node::Literal(u32::from(value)));
        };
        Ok(Node::Literal(symbol))
      }
      value if (ascii(b'1')..=ascii(b'9')).contains(&value) => {
        let checkpoint = self.cursor;
        let mut index = usize::from(value - ascii(b'0'));
        while let Some(next) = self.peek()
          && is_ascii_digit(next)
        {
          self.cursor += 1;
          index = index
            .saturating_mul(10)
            .saturating_add(usize::from(next - ascii(b'0')));
        }
        if index <= self.total_capture_count {
          Ok(Node::Backreference(index))
        } else if self.flags.unicode() {
          Err(self.error(ErrorKind::InvalidEscape, "invalid numeric backreference"))
        } else {
          self.cursor = checkpoint;
          if value <= ascii(b'7') {
            Ok(Node::Literal(self.parse_legacy_octal(value)))
          } else {
            Ok(Node::Literal(u32::from(value)))
          }
        }
      }
      value if self.identity_escape_allowed(value, false) => Ok(Node::Literal(u32::from(value))),
      _ => Err(self.error(ErrorKind::InvalidEscape, "invalid identity escape")),
    }
  }

  fn try_parse_unicode_escape(&mut self) -> Result<Option<u32>, Error> {
    if self.flags.unicode() && self.consume(ascii(b'{')) {
      let start = self.cursor;
      let mut value = 0_u32;
      let mut digits = 0;
      while let Some(unit) = self.peek() {
        if unit == ascii(b'}') {
          break;
        }
        let Some(digit) = hex_value(unit) else {
          return Err(self.error(ErrorKind::InvalidEscape, "invalid Unicode escape"));
        };
        self.cursor += 1;
        digits += 1;
        value = value
          .checked_mul(16)
          .and_then(|current| current.checked_add(digit))
          .ok_or_else(|| self.error(ErrorKind::InvalidEscape, "Unicode escape is too large"))?;
      }
      if digits == 0 || !self.consume(ascii(b'}')) || value > 0x10_FFFF {
        self.cursor = self.cursor.max(start);
        return Err(self.error(
          ErrorKind::InvalidEscape,
          "invalid Unicode code-point escape",
        ));
      }
      Ok(Some(value))
    } else {
      let checkpoint = self.cursor;
      let Some(value) = self.try_parse_fixed_hex(4) else {
        if self.flags.unicode() {
          return Err(self.error(ErrorKind::InvalidEscape, "invalid Unicode escape"));
        }
        self.cursor = checkpoint;
        return Ok(None);
      };
      if self.flags.unicode() && (0xD800..=0xDBFF).contains(&value) {
        let pair_checkpoint = self.cursor;
        if self.consume(ascii(b'\\'))
          && self.consume(ascii(b'u'))
          && let Some(trail) = self.try_parse_fixed_hex(4)
          && (0xDC00..=0xDFFF).contains(&trail)
        {
          return Ok(Some(0x1_0000 + ((value - 0xD800) << 10) + (trail - 0xDC00)));
        }
        self.cursor = pair_checkpoint;
      }
      Ok(Some(value))
    }
  }

  fn try_parse_fixed_hex(
    &mut self,
    count: usize,
  ) -> Option<u32> {
    let checkpoint = self.cursor;
    let mut value = 0_u32;
    for _ in 0..count {
      let Some(unit) = self.next() else {
        self.cursor = checkpoint;
        return None;
      };
      let Some(digit) = hex_value(unit) else {
        self.cursor = checkpoint;
        return None;
      };
      value = value * 16 + digit;
    }
    Some(value)
  }

  fn parse_control_escape(&mut self) -> Result<Node, Error> {
    if let Some(unit) = self.peek()
      && is_ascii_letter(unit)
    {
      self.cursor += 1;
      return Ok(Node::Literal(u32::from(unit) % 32));
    }
    if self.flags.unicode() {
      return Err(self.error(ErrorKind::InvalidEscape, "invalid control escape"));
    }
    Ok(Node::Sequence(vec![
      Node::Literal(u32::from(ascii(b'\\'))),
      Node::Literal(u32::from(ascii(b'c'))),
    ]))
  }

  fn parse_class_control_escape(&mut self) -> Result<ClassTerm, Error> {
    if let Some(unit) = self.peek()
      && (is_ascii_letter(unit)
        || (!self.flags.unicode() && (is_ascii_digit(unit) || unit == ascii(b'_'))))
    {
      self.cursor += 1;
      return Ok(ClassTerm::Range(u32::from(unit) % 32, u32::from(unit) % 32));
    }
    if self.flags.unicode() {
      return Err(self.error(ErrorKind::InvalidEscape, "invalid control escape"));
    }
    Ok(ClassTerm::Characters(vec![
      u32::from(ascii(b'\\')),
      u32::from(ascii(b'c')),
    ]))
  }

  fn parse_legacy_octal(
    &mut self,
    first: u16,
  ) -> u32 {
    let mut value = u32::from(first - ascii(b'0'));
    let max_additional = if first <= ascii(b'3') { 2 } else { 1 };
    for _ in 0..max_additional {
      let Some(next) = self.peek() else {
        break;
      };
      if !(ascii(b'0')..=ascii(b'7')).contains(&next) {
        break;
      }
      self.cursor += 1;
      value = value * 8 + u32::from(next - ascii(b'0'));
    }
    value
  }

  fn identity_escape_allowed(
    &self,
    unit: u16,
    in_class: bool,
  ) -> bool {
    if !self.flags.unicode() {
      return true;
    }
    is_syntax_character(unit) || unit == ascii(b'/') || (in_class && unit == ascii(b'-'))
  }

  fn parse_quantifier(
    &mut self,
    atom: Node,
  ) -> Result<Node, Error> {
    let bounds = match self.peek() {
      Some(value) if value == ascii(b'*') => {
        self.cursor += 1;
        Some((0, None))
      }
      Some(value) if value == ascii(b'+') => {
        self.cursor += 1;
        Some((1, None))
      }
      Some(value) if value == ascii(b'?') => {
        self.cursor += 1;
        Some((0, Some(1)))
      }
      Some(value) if value == ascii(b'{') => self.parse_braced_quantifier()?,
      _ => None,
    };
    let Some((min, max)) = bounds else {
      return Ok(atom);
    };
    if matches!(
      atom,
      Node::Assertion(Assertion::Start | Assertion::End | Assertion::WordBoundary(_))
    ) || (self.flags.unicode() && matches!(atom, Node::Assertion(Assertion::Lookahead { .. })))
    {
      return Err(self.error(
        ErrorKind::InvalidQuantifier,
        "assertions cannot be quantified",
      ));
    }
    let greedy = !self.consume(ascii(b'?'));
    Ok(Node::Quantifier {
      body: Box::new(atom),
      min,
      max,
      greedy,
    })
  }

  fn parse_braced_quantifier(&mut self) -> Result<Option<(usize, Option<usize>)>, Error> {
    let start = self.cursor;
    self.cursor += 1;
    let Some(min) = self.parse_decimal() else {
      self.cursor = start;
      return Ok(None);
    };
    let max = if self.consume(ascii(b'}')) {
      Some(min)
    } else if self.consume(ascii(b',')) {
      if self.consume(ascii(b'}')) {
        None
      } else {
        let Some(max) = self.parse_decimal() else {
          return self
            .invalid_braced_quantifier_or_literal(start, "invalid quantifier upper bound");
        };
        if !self.consume(ascii(b'}')) {
          return self.invalid_braced_quantifier_or_literal(start, "unterminated quantifier");
        }
        Some(max)
      }
    } else {
      self.cursor = start;
      return Ok(None);
    };
    if max.is_some_and(|max| min > max) {
      return Err(self.error(
        ErrorKind::InvalidQuantifier,
        "quantifier minimum exceeds maximum",
      ));
    }
    Ok(Some((min, max)))
  }

  fn invalid_braced_quantifier_or_literal(
    &mut self,
    start: usize,
    message: &'static str,
  ) -> Result<Option<(usize, Option<usize>)>, Error> {
    if self.flags.unicode() {
      Err(self.error(ErrorKind::InvalidQuantifier, message))
    } else {
      self.cursor = start;
      Ok(None)
    }
  }

  fn parse_decimal(&mut self) -> Option<usize> {
    let mut value = 0_usize;
    let mut found = false;
    while let Some(unit) = self.peek()
      && is_ascii_digit(unit)
    {
      found = true;
      self.cursor += 1;
      value = value
        .saturating_mul(10)
        .saturating_add(usize::from(unit - ascii(b'0')));
    }
    found.then_some(value)
  }

  fn literal_symbol(
    &mut self,
    first: u16,
  ) -> u32 {
    if self.flags.unicode()
      && (0xD800..=0xDBFF).contains(&first)
      && let Some(second) = self.peek()
      && (0xDC00..=0xDFFF).contains(&second)
    {
      self.cursor += 1;
      return 0x1_0000 + ((u32::from(first) - 0xD800) << 10) + (u32::from(second) - 0xDC00);
    }
    u32::from(first)
  }

  fn peek(&self) -> Option<u16> {
    self.units.get(self.cursor).copied()
  }

  fn next(&mut self) -> Option<u16> {
    let unit = self.peek()?;
    self.cursor += 1;
    Some(unit)
  }

  fn consume(
    &mut self,
    expected: u16,
  ) -> bool {
    if self.peek() == Some(expected) {
      self.cursor += 1;
      true
    } else {
      false
    }
  }

  fn error(
    &self,
    kind: ErrorKind,
    message: &'static str,
  ) -> Error {
    Error::new(kind, self.cursor, message)
  }
}

fn sequence_node(mut nodes: Vec<Node>) -> Node {
  match nodes.len() {
    0 => Node::Empty,
    1 => nodes.remove(0),
    _ => Node::Sequence(nodes),
  }
}

fn class_node(term: ClassTerm) -> Node {
  Node::Class(CharacterClass {
    inverted: false,
    terms: vec![term],
  })
}

const fn ascii(byte: u8) -> u16 {
  byte as u16
}

const fn is_ascii_digit(unit: u16) -> bool {
  unit >= ascii(b'0') && unit <= ascii(b'9')
}

const fn is_ascii_letter(unit: u16) -> bool {
  (unit >= ascii(b'a') && unit <= ascii(b'z')) || (unit >= ascii(b'A') && unit <= ascii(b'Z'))
}

const fn is_syntax_character(unit: u16) -> bool {
  matches!(
    unit,
    value
      if value == ascii(b'^')
        || value == ascii(b'$')
        || value == ascii(b'\\')
        || value == ascii(b'.')
        || value == ascii(b'*')
        || value == ascii(b'+')
        || value == ascii(b'?')
        || value == ascii(b'(')
        || value == ascii(b')')
        || value == ascii(b'[')
        || value == ascii(b']')
        || value == ascii(b'{')
        || value == ascii(b'}')
        || value == ascii(b'|')
  )
}

fn hex_value(unit: u16) -> Option<u32> {
  match unit {
    value if (ascii(b'0')..=ascii(b'9')).contains(&value) => Some(u32::from(value - ascii(b'0'))),
    value if (ascii(b'a')..=ascii(b'f')).contains(&value) => {
      Some(u32::from(value - ascii(b'a')) + 10)
    }
    value if (ascii(b'A')..=ascii(b'F')).contains(&value) => {
      Some(u32::from(value - ascii(b'A')) + 10)
    }
    _ => None,
  }
}

struct CaptureScan {
  count: usize,
  names: Vec<Option<JsString>>,
  named_indices: HashMap<JsString, usize>,
}

fn scan_capturing_groups(units: &[u16]) -> Result<CaptureScan, Error> {
  let mut count = 0;
  let mut names = vec![None];
  let mut named_indices = HashMap::new();
  let mut index = 0;
  let mut in_class = false;
  while index < units.len() {
    match units[index] {
      value if value == ascii(b'\\') => index = index.saturating_add(2),
      value if value == ascii(b'[') && !in_class => {
        in_class = true;
        index += 1;
      }
      value if value == ascii(b']') && in_class => {
        in_class = false;
        index += 1;
      }
      value if value == ascii(b'(') && !in_class => {
        let prefix = units.get(index + 2).copied();
        let noncapturing = units.get(index + 1) == Some(&ascii(b'?'))
          && matches!(prefix, Some(value) if value == ascii(b':') || value == ascii(b'=') || value == ascii(b'!'));
        let lookbehind = units.get(index + 1) == Some(&ascii(b'?'))
          && prefix == Some(ascii(b'<'))
          && matches!(
            units.get(index + 3),
            Some(value) if *value == ascii(b'=') || *value == ascii(b'!')
          );
        if noncapturing || lookbehind {
          index += 1;
          continue;
        }

        count += 1;
        let named = units.get(index + 1) == Some(&ascii(b'?')) && prefix == Some(ascii(b'<'));
        let name = if named {
          let mut name_cursor = index + 3;
          let name = parse_group_name(units, &mut name_cursor)?;
          if named_indices
            .insert(name.clone(), count)
            .is_some()
          {
            return Err(Error::new(
              ErrorKind::InvalidGroupName,
              index,
              "duplicate capture group name",
            ));
          }
          index = name_cursor;
          Some(name)
        } else {
          index += 1;
          None
        };
        names.push(name);
      }
      _ => index += 1,
    }
  }
  Ok(CaptureScan {
    count,
    names,
    named_indices,
  })
}

fn parse_group_name(
  units: &[u16],
  cursor: &mut usize,
) -> Result<JsString, Error> {
  let start = *cursor;
  let mut output = JsStringBuilder::new();
  let mut position = 0;
  loop {
    let Some(unit) = units.get(*cursor).copied() else {
      return Err(Error::new(
        ErrorKind::InvalidGroupName,
        *cursor,
        "unterminated capture group name",
      ));
    };
    if unit == ascii(b'>') {
      if position == 0 {
        return Err(Error::new(
          ErrorKind::InvalidGroupName,
          *cursor,
          "capture group name is empty",
        ));
      }
      *cursor += 1;
      return Ok(output.finish());
    }

    let code_point = read_group_name_code_point(units, cursor)?;
    let Some(character) = char::from_u32(code_point) else {
      return Err(Error::new(
        ErrorKind::InvalidGroupName,
        *cursor,
        "capture group name contains an invalid code point",
      ));
    };
    let valid = if position == 0 {
      character == '$' || character == '_' || unicode_id_start::is_id_start(character)
    } else {
      character == '$'
        || character == '_'
        || character == '\u{200C}'
        || character == '\u{200D}'
        || unicode_id_start::is_id_continue(character)
    };
    if !valid {
      return Err(Error::new(
        ErrorKind::InvalidGroupName,
        start,
        "invalid capture group name",
      ));
    }
    if !output.push_code_point(code_point) {
      return Err(Error::new(
        ErrorKind::InvalidGroupName,
        *cursor,
        "invalid capture group name code point",
      ));
    }
    position += 1;
  }
}

fn read_group_name_code_point(
  units: &[u16],
  cursor: &mut usize,
) -> Result<u32, Error> {
  let Some(first) = units.get(*cursor).copied() else {
    return Err(Error::new(
      ErrorKind::InvalidGroupName,
      *cursor,
      "unterminated capture group name",
    ));
  };
  if first == ascii(b'\\') {
    if units.get(*cursor + 1) != Some(&ascii(b'u')) {
      return Err(Error::new(
        ErrorKind::InvalidGroupName,
        *cursor,
        "capture group name escape must be Unicode",
      ));
    }
    *cursor += 2;
    return read_group_name_unicode_escape(units, cursor);
  }
  *cursor += 1;
  if (0xD800..=0xDBFF).contains(&first)
    && let Some(second) = units.get(*cursor).copied()
    && (0xDC00..=0xDFFF).contains(&second)
  {
    *cursor += 1;
    return Ok(0x1_0000 + ((u32::from(first) - 0xD800) << 10) + (u32::from(second) - 0xDC00));
  }
  Ok(u32::from(first))
}

fn read_group_name_unicode_escape(
  units: &[u16],
  cursor: &mut usize,
) -> Result<u32, Error> {
  if units.get(*cursor) == Some(&ascii(b'{')) {
    *cursor += 1;
    let mut value = 0_u32;
    let mut digits = 0;
    while let Some(unit) = units.get(*cursor).copied()
      && unit != ascii(b'}')
    {
      let Some(digit) = hex_value(unit) else {
        return Err(Error::new(
          ErrorKind::InvalidGroupName,
          *cursor,
          "invalid Unicode escape in capture group name",
        ));
      };
      value = value
        .checked_mul(16)
        .and_then(|current| current.checked_add(digit))
        .ok_or_else(|| {
          Error::new(
            ErrorKind::InvalidGroupName,
            *cursor,
            "capture group name escape is too large",
          )
        })?;
      *cursor += 1;
      digits += 1;
    }
    if digits == 0 || units.get(*cursor) != Some(&ascii(b'}')) || value > 0x10_FFFF {
      return Err(Error::new(
        ErrorKind::InvalidGroupName,
        *cursor,
        "invalid Unicode escape in capture group name",
      ));
    }
    *cursor += 1;
    return Ok(value);
  }

  let Some(value) = read_exact_hex(units, cursor, 4) else {
    return Err(Error::new(
      ErrorKind::InvalidGroupName,
      *cursor,
      "invalid Unicode escape in capture group name",
    ));
  };
  Ok(value)
}

fn read_exact_hex(
  units: &[u16],
  cursor: &mut usize,
  count: usize,
) -> Option<u32> {
  let checkpoint = *cursor;
  let mut value = 0;
  for _ in 0..count {
    let digit = units.get(*cursor).copied().and_then(hex_value);
    let Some(digit) = digit else {
      *cursor = checkpoint;
      return None;
    };
    *cursor += 1;
    value = value * 16 + digit;
  }
  Some(value)
}
