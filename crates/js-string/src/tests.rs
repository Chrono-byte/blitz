use super::*;
  use std::collections::HashSet;

  #[test]
  fn preserves_isolated_surrogates() {
let value = JsString::from_utf16(vec![0xD800, u16::from(b'a'), 0xDC00]);
assert_eq!(value.as_slice(), &[0xD800, 0x0061, 0xDC00]);
assert_eq!(value.len(), 3);
assert!(!value.is_well_formed());
assert_eq!(value.to_string_lossy(), "\u{FFFD}a\u{FFFD}");
  }

  #[test]
  fn slices_share_semantic_content() {
let value = JsString::new("a😀b");
let slice = value.slice(1..3);
let Some(slice) = slice else {
  panic!("valid slice should exist");
};
assert_eq!(slice.as_slice(), &[0xD83D, 0xDE00]);
assert_eq!(slice.to_string_lossy(), "😀");
  }

  #[test]
  fn code_point_access_retains_code_unit_offsets() {
let value = JsString::new("😀x");
assert_eq!(
  value.code_point_at(0, true),
  Some(CodePoint {
    value: 0x1_F600,
    width: 2,
  })
);
assert_eq!(
  value.code_point_at(0, false),
  Some(CodePoint {
    value: 0xD83D,
    width: 1,
  })
);
assert_eq!(value.advance_string_index(0, true), 2);
  }

  #[test]
  fn compares_and_hashes_shared_slices_by_code_units() {
let source = JsString::new("xabcy");
let slice = source.slice(1..4);
let Some(slice) = slice else {
  panic!("valid slice should exist");
};
let owned = JsString::new("abc");
assert_eq!(slice, owned);
#[expect(
  clippy::mutable_key_type,
  reason = "JsString's interior mutability is limited to a derived UTF-8 cache"
)]
let mut values = HashSet::new();
values.insert(slice);
assert!(values.contains(&owned));
  }

  #[test]
  fn searches_by_code_unit_offset() {
let value = JsString::new("😀a😀");
let needle = JsString::new("😀");
assert_eq!(value.find(&needle, 0), Some(0));
assert_eq!(value.find(&needle, 1), Some(3));
assert_eq!(value.find(&JsString::empty(), 5), Some(5));
  }

  #[test]
  fn well_formed_replacement_only_changes_isolated_surrogates() {
let value = JsString::from_utf16(vec![0xD800, 0xD83D, 0xDE00, 0xDC00]);
assert_eq!(
  value.to_well_formed().as_slice(),
  &[0xFFFD, 0xD83D, 0xDE00, 0xFFFD]
);
  }

  #[test]
  fn builder_appends_code_units_and_code_points() {
let mut builder = JsStringBuilder::new();
builder.push_str("a");
builder.push_code_unit(0xD800);
assert!(builder.push_code_point(0x1_F600));
assert!(!builder.push_code_point(0xDFFF));
assert_eq!(
  builder.finish().as_slice(),
  &[0x0061, 0xD800, 0xD83D, 0xDE00]
);
  }
