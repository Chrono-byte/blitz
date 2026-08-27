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

#[test]
fn short_string_layout_and_traits_remain_compact() {
  assert!(JsString::empty().is_empty());
  assert!(std::mem::size_of::<JsString>() <= 40);
  fn assert_send_sync<T: Send + Sync>() {}
  assert_send_sync::<JsString>();
}

#[test]
fn short_and_heap_strings_have_identical_value_semantics() {
  use std::collections::hash_map::DefaultHasher;

  fn hash(value: &JsString) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
  }

  let short = JsString::new("length");
  let sliced = JsString::new("xlengthy")
    .slice(1..7)
    .expect("valid slice");
  assert_eq!(short, sliced);
  assert_eq!(short.cmp(&sliced), Ordering::Equal);
  assert_eq!(hash(&short), hash(&sliced));
  assert_eq!(short.as_slice(), sliced.as_slice());
  assert_eq!(short.as_str_lossy(), sliced.as_str_lossy());
}

#[test]
fn short_string_boundaries_preserve_exact_utf16() {
  for units in [
    vec![],
    vec![0; 7],
    vec![u16::from(b'a'); 8],
    vec![0x007F; 7],
    vec![0x0080],
    vec![0xD800],
    vec![0xDC00],
    vec![0xD83D, 0xDE00],
  ] {
    let value = JsString::from_utf16(units.clone());
    assert_eq!(value.as_slice(), units);
    assert_eq!(value.to_string_lossy(), String::from_utf16_lossy(&units));
  }
}

#[test]
fn short_ascii_slices_preserve_code_unit_boundaries_and_stable_views() {
  let value = JsString::new("abcdefg");
  assert!(std::ptr::eq(value.as_str_lossy(), value.as_str_lossy()));
  for start in 0..=value.len() {
    for end in start..=value.len() {
      let slice = value
        .slice(start..end)
        .expect("valid inline slice");
      assert_eq!(slice.as_slice(), &value.as_slice()[start..end]);
      assert_eq!(slice.as_str_lossy(), &"abcdefg"[start..end]);
    }
  }
}
