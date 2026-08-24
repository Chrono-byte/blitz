use super::*;

  fn regex(
source: &str,
flags: &str,
  ) -> Regex {
match Regex::new(source, flags) {
  Ok(regex) => regex,
  Err(error) => panic!("failed to parse {source:?}: {error}"),
}
  }

  #[test]
  fn reports_utf16_offsets_and_captures() {
let input = Input::new("😀abbbc");
let matched = regex("(a)(b+)", "").find(&input, 0);
let Some(matched) = matched else {
  panic!("expected a match");
};
assert_eq!(matched.range(), 2..6);
assert_eq!(matched.capture(1).map(Capture::range), Some(2..3));
assert_eq!(matched.capture(2).map(Capture::range), Some(3..6));
assert_eq!(
  input
    .slice(matched.range())
    .map(|value| value.to_string_lossy()),
  Some("abbb".to_string())
);
  }

  #[test]
  fn supports_alternation_and_quantifier_order() {
let greedy = regex("a(b|bb)+", "").find_str("abbb", 0);
let lazy = regex("a.+?c", "").find_str("a12c34c", 0);
assert_eq!(greedy.map(|matched| matched.range()), Some(0..4));
assert_eq!(lazy.map(|matched| matched.range()), Some(0..4));
  }

  #[test]
  fn supports_classes_anchors_and_multiline() {
let matched = regex(r"^[A-Z]\w+\s+\d+$", "im").find_str("x\nName 42\ny", 0);
assert_eq!(matched.map(|value| value.range()), Some(2..9));
  }

  #[test]
  fn supports_lookahead_and_backreferences() {
let matched = regex(r"(a|b)\1(?=!)", "").find_str("zaa!", 0);
assert_eq!(matched.map(|value| value.range()), Some(1..3));
assert!(
  regex(r"(a|b)\1(?!\?)", "")
    .find_str("zbb!", 0)
    .is_some()
);
  }

  #[test]
  fn sticky_only_matches_at_requested_offset() {
let expression = regex("a+", "y");
let input = Input::new("baaa");
assert!(expression.find(&input, 0).is_none());
assert_eq!(
  expression
    .find(&input, 1)
    .map(|value| value.range()),
  Some(1..4)
);
  }

  #[test]
  fn unicode_mode_advances_over_surrogate_pairs() {
let input = Input::new("😀x");
assert_eq!(
  regex(".", "u")
    .find(&input, 0)
    .map(|value| value.range()),
  Some(0..2)
);
assert_eq!(
  regex(".", "")
    .find(&input, 0)
    .map(|value| value.range()),
  Some(0..1)
);
  }

  #[test]
  fn exact_utf16_patterns_match_isolated_surrogates() {
let pattern = Input::from_utf16(vec![0xD800]);
let subject = Input::from_utf16(vec![u16::from(b'x'), 0xD800]);
let expression = match Regex::from_js(&pattern, "") {
  Ok(expression) => expression,
  Err(error) => panic!("isolated surrogate pattern should parse: {error}"),
};
assert_eq!(
  expression
    .find(&subject, 0)
    .map(|matched| matched.range()),
  Some(1..2)
);
  }

  #[test]
  fn rejects_duplicate_and_invalid_flags() {
assert!(Regex::new("", "gg").is_err());
assert!(Regex::new("", "q").is_err());
assert!(Regex::new("", "d").is_ok());
assert!(Regex::new("", "v").is_ok());
assert!(Regex::new("", "uv").is_err());
assert_eq!(regex("", "ymigd").flags().to_string(), "dgimy");
  }

  #[test]
  fn rejects_invalid_patterns() {
assert!(Regex::new("(", "").is_err());
assert!(Regex::new("[z-a]", "").is_err());
assert!(Regex::new("a{2,1}", "").is_err());
  }

  #[test]
  fn rejects_excessive_group_nesting() {
let pattern = format!("{}a{}", "(".repeat(300), ")".repeat(300));
assert!(Regex::new(&pattern, "").is_err());
  }

  #[test]
  fn reports_excessive_match_work() {
let expression = regex("(a+)+$", "");
let subject = Input::new(&format!("{}!", "a".repeat(24)));
assert_eq!(expression.try_find(&subject, 0), Err(MatchError));
  }

  #[test]
  fn supports_empty_and_inverted_empty_classes() {
assert!(regex("[]", "").find_str("anything", 0).is_none());
assert_eq!(
  regex("[^]", "")
    .find_str("x", 0)
    .map(|matched| matched.range()),
  Some(0..1)
);
  }

  #[test]
  fn repeated_atoms_clear_nested_captures() {
let matched = regex(r"(a|(b))+", "").find_str("aba", 0);
let Some(matched) = matched else {
  panic!("expected a match");
};
assert_eq!(matched.range(), 0..3);
assert!(matched.capture(2).is_none());
  }

  #[test]
  fn empty_repetitions_terminate() {
assert_eq!(
  regex("(?:)*a", "")
    .find_str("a", 0)
    .map(|matched| matched.range()),
  Some(0..1)
);
assert_eq!(
  regex("(?:){2}a", "")
    .find_str("a", 0)
    .map(|matched| matched.range()),
  Some(0..1)
);
  }

  #[test]
  fn parses_control_null_and_legacy_octal_escapes() {
assert_eq!(
  regex(r"\cA\0\11", "")
    .find_str("\u{1}\0\t", 0)
    .map(|matched| matched.range()),
  Some(0..3)
);
assert_eq!(
  regex(r"\8\9", "")
    .find_str("89", 0)
    .map(|matched| matched.range()),
  Some(0..2)
);
assert_eq!(
  regex(r"(a)\2", "")
    .find_str("a\u{2}", 0)
    .map(|matched| matched.range()),
  Some(0..2)
);
  }

  #[test]
  fn classifies_forward_decimal_escapes_as_backreferences() {
let matched = regex(r"\1(a)", "").find_str("a", 0);
assert_eq!(matched.map(|value| value.range()), Some(0..1));
  }

  #[test]
  fn applies_unicode_mode_escape_early_errors() {
for pattern in [r"\8", r"\a", r"\xZ", r"\uZZZZ", "{", "}", "]"] {
  assert!(Regex::new(pattern, "u").is_err(), "{pattern:?} should fail");
}
assert!(Regex::new(r"\-", "u").is_err());
assert!(Regex::new(r"[\-]", "u").is_ok());
  }

  #[test]
  fn preserves_annex_b_identity_and_malformed_quantifier_behavior() {
assert_eq!(
  regex(r"\xZ", "")
    .find_str("xZ", 0)
    .map(|matched| matched.range()),
  Some(0..2)
);
assert_eq!(
  regex("a{1,x", "")
    .find_str("a{1,x", 0)
    .map(|matched| matched.range()),
  Some(0..5)
);
assert!(Regex::new("a{1,x", "u").is_err());
  }

  #[test]
  fn combines_escaped_surrogate_pairs_in_unicode_mode() {
assert_eq!(
  regex(r"\uD83D\uDE00", "u")
    .find_str("😀", 0)
    .map(|matched| matched.range()),
  Some(0..2)
);
assert_eq!(
  regex(r"[\uD83D\uDE00]", "u")
    .find_str("😀", 0)
    .map(|matched| matched.range()),
  Some(0..2)
);
  }

  #[test]
  fn follows_ecmascript_case_canonicalization() {
assert!(regex("k", "iu").find_str("\u{212A}", 0).is_some());
assert!(
  regex("[a-z]", "iu")
    .find_str("\u{017F}", 0)
    .is_some()
);
assert!(
  regex("[\u{212A}]", "iu")
    .find_str("k", 0)
    .is_some()
);
assert!(
  regex(r"\w", "iu")
    .find_str("\u{212A}", 0)
    .is_some()
);
assert!(regex("k", "i").find_str("\u{212A}", 0).is_none());
assert!(
  regex("[a-z]", "i")
    .find_str("\u{017F}", 0)
    .is_none()
);
assert!(regex("ß", "iu").find_str("ss", 0).is_none());
  }

  #[test]
  fn permits_annex_b_quantified_lookahead_only_without_unicode() {
assert!(Regex::new("(?=a)?a", "").is_ok());
assert!(Regex::new("(?=a)?a", "u").is_err());
  }

  #[test]
  fn annex_b_class_ranges_allow_class_escapes() {
let expression = regex(r"[a-\d]", "");
assert!(expression.find_str("a", 0).is_some());
assert!(expression.find_str("-", 0).is_some());
assert!(expression.find_str("5", 0).is_some());
assert!(Regex::new(r"[a-\d]", "u").is_err());
  }

  #[test]
  fn escapes_source_text_without_losing_utf16() {
let expression = regex("/\n\r\u{2028}\u{2029}", "");
assert_eq!(
  expression.escaped_source().to_string_lossy(),
  r"\/\n\r\u2028\u2029"
);
assert_eq!(regex("", "").escaped_source().to_string_lossy(), "(?:)");

let source = Input::from_utf16(vec![0xD800, u16::from(b'/')]);
let expression = match Regex::from_js(&source, "") {
  Ok(expression) => expression,
  Err(error) => panic!("source should parse: {error}"),
};
assert_eq!(
  expression.escaped_source().as_slice(),
  &[0xD800, 0x005C, 0x002F]
);
  }

  #[test]
  fn supports_named_captures_and_forward_named_backreferences() {
let expression = regex(r"\k<letter>(?<letter>a)", "");
assert_eq!(
  expression
    .find_str("a", 0)
    .map(|matched| matched.range()),
  Some(0..1)
);
assert_eq!(expression.capture_index(&Input::new("letter")), Some(1));

let escaped = regex(r"(?<\u{3B1}>a)\k<α>", "u");
assert_eq!(
  escaped
    .find_str("aa", 0)
    .map(|matched| matched.range()),
  Some(0..2)
);
  }

  #[test]
  fn rejects_invalid_duplicate_and_unknown_group_names() {
assert!(Regex::new(r"(?<1>a)", "").is_err());
assert!(Regex::new(r"(?<a>x)(?<a>y)", "").is_err());
assert!(Regex::new(r"(?<a>x)\k<b>", "").is_err());
  }
