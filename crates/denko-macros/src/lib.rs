// SPDX-License-Identifier: MPL-2.0
//! Compile-time validation for JavaScript and TypeScript source embedded in Rust.

use oxc_allocator::Allocator;
use oxc_parser::{ParseOptions, Parser};
use oxc_span::SourceType;
use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::{Ident, LitStr, Result, Token, parenthesized};

#[derive(Clone, Copy)]
enum LanguageMode {
  Script,
  Module,
  TypeScript,
  Tsx,
}

impl LanguageMode {
  fn source_type(self) -> SourceType {
    match self {
      Self::Script => SourceType::script(),
      Self::Module => SourceType::mjs(),
      Self::TypeScript => SourceType::ts(),
      Self::Tsx => SourceType::tsx(),
    }
  }
}

struct JavaScriptInput {
  mode: LanguageMode,
  expect_parse_error: bool,
  source: LitStr,
  is_format: bool,
  format_tail: TokenStream2,
}

impl Parse for JavaScriptInput {
  fn parse(input: ParseStream<'_>) -> Result<Self> {
    if input.peek(LitStr) {
      let source = input.parse()?;
      if !input.is_empty() {
        return Err(input.error("unexpected tokens after JavaScript source literal"));
      }
      return Ok(Self {
        mode: LanguageMode::Script,
        expect_parse_error: false,
        source,
        is_format: false,
        format_tail: TokenStream2::new(),
      });
    }

    let first: Ident = input.parse()?;
    let first_name = first.to_string();
    if first_name == "format" {
      let content;
      parenthesized!(content in input);
      let mode: Ident = content.parse()?;
      if !content.is_empty() {
        return Err(content.error("expected one source mode"));
      }
      input.parse::<Token![,]>()?;
      let source = input.parse()?;
      let format_tail = input.parse()?;
      return Ok(Self {
        mode: parse_mode(&mode)?,
        expect_parse_error: false,
        source,
        is_format: true,
        format_tail,
      });
    }

    let (mode, expect_parse_error) = if first_name == "parse_error" {
      let content;
      parenthesized!(content in input);
      let mode: Ident = content.parse()?;
      if !content.is_empty() {
        return Err(content.error("expected one source mode"));
      }
      (parse_mode(&mode)?, true)
    } else {
      (parse_mode(&first)?, false)
    };

    input.parse::<Token![,]>()?;
    let source = input.parse()?;
    if !input.is_empty() {
      return Err(input.error("unexpected tokens after JavaScript source literal"));
    }

    Ok(Self {
      mode,
      expect_parse_error,
      source,
      is_format: false,
      format_tail: TokenStream2::new(),
    })
  }
}

fn parse_mode(mode: &Ident) -> Result<LanguageMode> {
  match mode.to_string().as_str() {
    "script" => Ok(LanguageMode::Script),
    "module" => Ok(LanguageMode::Module),
    "typescript" | "ts" => Ok(LanguageMode::TypeScript),
    "tsx" => Ok(LanguageMode::Tsx),
    _ => Err(syn::Error::new(
      mode.span(),
      "expected script, module, typescript, ts, or tsx",
    )),
  }
}

/// Produce a parseable approximation of a Rust format string.
///
/// Escaped braces become JavaScript braces and each replacement field becomes
/// a unique identifier. Identifiers work as both expressions and bindings, so
/// this also supports templates that generate function parameters. The Rust
/// compiler remains responsible for validating the format arguments.
fn format_scaffold(source: &LitStr) -> Result<String> {
  let value = source.value();
  let mut scaffold = String::with_capacity(value.len());
  let mut chars = value.char_indices().peekable();
  let mut field_index = 0usize;

  while let Some((offset, character)) = chars.next() {
    match character {
      '{' if chars.peek().is_some_and(|(_, next)| *next == '{') => {
        chars.next();
        scaffold.push('{');
      }
      '}' if chars.peek().is_some_and(|(_, next)| *next == '}') => {
        chars.next();
        scaffold.push('}');
      }
      '{' => {
        let Some((_, _)) = chars.find(|(_, next)| *next == '}') else {
          return Err(syn::Error::new(
            source.span(),
            format!("unclosed format replacement field at byte {offset}"),
          ));
        };
        scaffold.push_str(&format!("__denko_interpolation_{field_index}__"));
        field_index += 1;
      }
      '}' => {
        return Err(syn::Error::new(
          source.span(),
          format!("unmatched closing brace in JavaScript format template at byte {offset}"),
        ));
      }
      _ => scaffold.push(character),
    }
  }

  Ok(scaffold)
}

fn validate_source(
  input: &JavaScriptInput,
  source_text: &str,
) -> Result<()> {
  let allocator = Allocator::default();
  let parsed = Parser::new(&allocator, source_text, input.mode.source_type())
    .with_options(ParseOptions {
      parse_regular_expression: true,
      ..ParseOptions::default()
    })
    .parse();
  let has_errors = !parsed.diagnostics.is_empty();

  if input.expect_parse_error == has_errors {
    return Ok(());
  }

  let message = if input.expect_parse_error {
    "JavaScript fixture was expected to fail parsing, but it parsed successfully".to_string()
  } else {
    parsed
      .diagnostics
      .into_iter()
      .map(|diagnostic| diagnostic.to_string())
      .collect::<Vec<_>>()
      .join("\n")
  };
  Err(syn::Error::new(input.source.span(), message))
}

/// Validate embedded JavaScript or TypeScript at compile time.
///
/// The one-argument form validates a script and expands to the original string
/// literal. Use an explicit mode for modules, TypeScript, or TSX:
/// `js!(module, "export const answer = 42;")`.
///
/// `js!(format(script), "const answer = {};", answer)` expands like `format!`
/// and validates its static JavaScript scaffold after replacing each format
/// field with an expression placeholder. Negative parser fixtures use
/// `js!(parse_error(script), "const = ;")`.
///
/// Invalid source is a compile-time error:
///
/// ```compile_fail
/// use denko_macros::js;
///
/// let _ = js!("const = ;");
/// ```
#[proc_macro]
pub fn js(input: TokenStream) -> TokenStream {
  let input = syn::parse_macro_input!(input as JavaScriptInput);
  let source_text = if input.is_format {
    match format_scaffold(&input.source) {
      Ok(scaffold) => scaffold,
      Err(error) => return error.into_compile_error().into(),
    }
  } else {
    input.source.value()
  };

  if let Err(error) = validate_source(&input, &source_text) {
    return error.into_compile_error().into();
  }

  let source = input.source;
  if input.is_format {
    let format_tail = input.format_tail;
    quote!(format!(#source #format_tail)).into()
  } else {
    quote!(#source).into()
  }
}
