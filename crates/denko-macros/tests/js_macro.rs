// SPDX-License-Identifier: MPL-2.0

use denko_macros::js;

const SCRIPT: &str = js!("const answer = 42;");
const MODULE: &str = js!(module, "export const answer = 42;");
const TYPESCRIPT: &str = js!(typescript, "const answer: number = 42;");
const TSX: &str = js!(tsx, "const answer = <output>{42}</output>;");
const PARSE_ERROR: &str = js!(parse_error(script), "const = ;");

#[test]
fn validated_sources_preserve_the_original_bytes() {
  assert_eq!(SCRIPT, "const answer = 42;");
  assert_eq!(MODULE, "export const answer = 42;");
  assert_eq!(TYPESCRIPT, "const answer: number = 42;");
  assert_eq!(TSX, "const answer = <output>{42}</output>;");
  assert_eq!(PARSE_ERROR, "const = ;");
}

#[test]
fn format_mode_validates_the_static_scaffold_and_preserves_format_semantics() {
  let value = 42;
  let source = js!(
    format(script),
    "function answer() {{ return {value}; }} answer();"
  );
  assert_eq!(source, "function answer() { return 42; } answer();");
}

#[test]
fn format_mode_supports_expression_and_binding_fields() {
  let value = 42;
  let source = js!(
    format(script),
    "function answer({{ value = {} }}) {{ return value + {value}; }}",
    value + 1
  );
  assert_eq!(
    source,
    "function answer({ value = 43 }) { return value + 42; }"
  );
}
