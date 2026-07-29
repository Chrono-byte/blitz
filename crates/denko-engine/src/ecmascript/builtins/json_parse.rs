// SPDX-License-Identifier: MPL-2.0
//! ECMAScript JSON text parsing over exact UTF-16 code units.

use chumsky::prelude::*;
use js_string::JsString;
use std::ops::Range;

#[derive(Clone, Debug, PartialEq)]
pub(super) struct ParsedJson {
  pub(super) value: ParsedValue,
  pub(super) span: Range<usize>,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum ParsedValue {
  Null,
  Bool(bool),
  Number(f64),
  String(JsString),
  Array(Vec<ParsedJson>),
  Object(Vec<(JsString, ParsedJson)>),
}

fn is_digit(unit: u16) -> bool {
  matches!(unit, 0x30..=0x39)
}

fn hex_value(unit: u16) -> u16 {
  match unit {
    0x30..=0x39 => unit - 0x30,
    0x41..=0x46 => unit - 0x41 + 10,
    0x61..=0x66 => unit - 0x61 + 10,
    _ => unreachable!("hex parser only emits hexadecimal digits"),
  }
}

fn parser<'src>() -> impl Parser<'src, &'src [u16], ParsedJson, extra::Err<Rich<'src, u16>>> {
  let whitespace = one_of([0x20, 0x0A, 0x0D, 0x09])
    .repeated()
    .ignored();
  let padded = |unit| just(unit).padded_by(whitespace);

  recursive(|value| {
    let hex = any()
      .filter(|unit: &u16| matches!(*unit, 0x30..=0x39 | 0x41..=0x46 | 0x61..=0x66))
      .repeated()
      .exactly(4)
      .collect::<Vec<_>>()
      .map(|digits| {
        digits
          .into_iter()
          .fold(0, |value, unit| value * 16 + hex_value(unit))
      });

    let escape = just(0x5C).ignore_then(choice((
      just(0x22).to(0x22),
      just(0x5C).to(0x5C),
      just(0x2F).to(0x2F),
      just(0x62).to(0x08),
      just(0x66).to(0x0C),
      just(0x6E).to(0x0A),
      just(0x72).to(0x0D),
      just(0x74).to(0x09),
      just(0x75).ignore_then(hex),
    )));
    let unescaped = any().filter(|unit: &u16| *unit >= 0x20 && *unit != 0x22 && *unit != 0x5C);
    let string = unescaped
      .or(escape)
      .repeated()
      .collect::<Vec<_>>()
      .map(JsString::from_utf16)
      .delimited_by(just(0x22), just(0x22));

    let digits = any()
      .filter(|unit: &u16| is_digit(*unit))
      .repeated()
      .at_least(1)
      .ignored();
    let integer = just(0x30).ignored().or(
      any()
        .filter(|unit: &u16| matches!(*unit, 0x31..=0x39))
        .then(digits.or_not())
        .ignored(),
    );
    let fraction = just(0x2E).then(digits).ignored();
    let exponent = one_of([0x65, 0x45])
      .then(one_of([0x2B, 0x2D]).or_not())
      .then(digits)
      .ignored();
    let number = just(0x2D)
      .or_not()
      .then(integer)
      .then(fraction.or_not())
      .then(exponent.or_not())
      .to_slice()
      .map(|units: &[u16]| {
        let text = String::from_utf16(units).expect("JSON numbers contain only ASCII");
        text.parse().expect("validated JSON number")
      });

    let comma = padded(0x2C);
    let colon = padded(0x3A);
    let array = value
      .clone()
      .separated_by(comma)
      .collect::<Vec<_>>()
      .delimited_by(padded(0x5B), padded(0x5D));
    let member = string
      .padded_by(whitespace)
      .then_ignore(colon)
      .then(value.clone());
    let object = member
      .separated_by(comma)
      .collect::<Vec<_>>()
      .delimited_by(padded(0x7B), padded(0x7D));

    choice((
      just([0x6E, 0x75, 0x6C, 0x6C]).to(ParsedValue::Null),
      just([0x74, 0x72, 0x75, 0x65]).to(ParsedValue::Bool(true)),
      just([0x66, 0x61, 0x6C, 0x73, 0x65]).to(ParsedValue::Bool(false)),
      string.map(ParsedValue::String),
      number.map(ParsedValue::Number),
      array.map(ParsedValue::Array),
      object.map(ParsedValue::Object),
    ))
    .map_with(
      |value,
       extra: &mut chumsky::input::MapExtra<
        'src,
        '_,
        &'src [u16],
        extra::Err<Rich<'src, u16>>,
      >| ParsedJson {
        value,
        span: extra.span().into_range(),
      },
    )
    .padded_by(whitespace)
  })
  .then_ignore(end())
}

pub(super) fn parse(source: &JsString) -> Result<ParsedJson, String> {
  parser()
    .parse(source.as_slice())
    .into_result()
    .map_err(|errors| {
      errors
        .first()
        .map_or_else(|| "invalid JSON text".to_owned(), ToString::to_string)
    })
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn parses_exact_utf16_strings_and_ordered_objects() {
    let input = JsString::from_utf16(
      r#"{"first":"\ud800","second":[true,null,-1.25e2]}"#
        .encode_utf16()
        .collect::<Vec<_>>(),
    );
    let parsed = parse(&input).expect("valid JSON");
    let ParsedValue::Object(properties) = parsed.value else {
      panic!("expected object");
    };
    assert_eq!(properties[0].0, "first");
    assert_eq!(properties[1].0, "second");
    let ParsedValue::String(value) = &properties[0].1.value else {
      panic!("expected string");
    };
    assert_eq!(value.as_slice(), &[0xD800]);
    assert_eq!(properties[0].1.span, 9..17);
  }

  #[test]
  fn rejects_non_json_forms() {
    for source in ["", "+1", "01", "[1,]", "{\"a\":1,}", "'text'", "NaN"] {
      assert!(
        parse(&JsString::from(source)).is_err(),
        "accepted {source:?}"
      );
    }
    for control in 0..0x20 {
      let source = JsString::from_utf16(vec![0x22, control, 0x22]);
      assert!(
        parse(&source).is_err(),
        "accepted control code {control:#x}"
      );
    }
  }
}
