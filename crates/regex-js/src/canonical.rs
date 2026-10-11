// SPDX-License-Identifier: MPL-2.0
use crate::Flags;

pub(crate) fn canonicalize(
  symbol: u32,
  flags: Flags,
) -> u32 {
  if !flags.ignore_case() {
    return symbol;
  }
  if flags.unicode() {
    return char::from_u32(symbol)
      .and_then(unicode_case_mapping::case_folded)
      .map_or(symbol, |folded| folded.get());
  }

  let Some(character) = char::from_u32(symbol) else {
    return symbol;
  };
  let uppercase = unicode_case_mapping::to_uppercase(character);
  let Some(mapped) = single_mapping(&uppercase) else {
    return symbol;
  };
  if mapped > u32::from(u16::MAX) {
    return symbol;
  }
  if symbol >= 128 && mapped < 128 {
    return symbol;
  }
  mapped
}

pub(crate) fn symbols_equal(
  left: u32,
  right: u32,
  flags: Flags,
) -> bool {
  canonicalize(left, flags) == canonicalize(right, flags)
}

pub(crate) fn range_contains(
  start: u32,
  end: u32,
  symbol: u32,
  flags: Flags,
) -> bool {
  if (start..=end).contains(&symbol) {
    return true;
  }
  if !flags.ignore_case() {
    return false;
  }

  let canonical = canonicalize(symbol, flags);
  (start..=end).any(|candidate| canonicalize(candidate, flags) == canonical)
}

fn single_mapping<const N: usize>(mapping: &[u32; N]) -> Option<u32> {
  let first = mapping[0];
  if first == 0 || mapping[1..].iter().any(|value| *value != 0) {
    None
  } else {
    Some(first)
  }
}
