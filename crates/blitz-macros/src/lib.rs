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
      return Ok(Self {
        mode: LanguageMode::Script,
        expect_parse_error: false,
        source: input.parse()?,
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

/// Validate embedded JavaScript or TypeScript and expand to the original string literal.
///
/// The one-argument form parses an ordinary script. An explicit source mode can
/// be supplied as the first argument, such as `js!(module, "...")`. Negative
/// parser fixtures use `js!(parse_error(script), "...")`.
#[proc_macro]
pub fn js(input: TokenStream) -> TokenStream {
  let input = syn::parse_macro_input!(input as JavaScriptInput);
  if input.is_format {
    let format_tail = input.format_tail;
    let source = input.source;
    return quote!(format!(#source #format_tail)).into();
  }

  let source_text = input.source.value();
  let allocator = Allocator::default();
  let parsed = Parser::new(&allocator, &source_text, input.mode.source_type())
    .with_options(ParseOptions {
      parse_regular_expression: true,
      ..ParseOptions::default()
    })
    .parse();
  let has_errors = !parsed.diagnostics.is_empty();

  if input.expect_parse_error == has_errors {
    let source = input.source;
    return quote!(#source).into();
  }

  let message = if input.expect_parse_error {
    "JavaScript fixture was expected to fail parsing, but it parsed successfully".to_string()
  } else {
    parsed
      .diagnostics
      .into_iter()
      .map(|diagnostic| format!("{diagnostic:?}"))
      .collect::<Vec<_>>()
      .join("\n")
  };

  syn::Error::new(input.source.span(), message)
    .into_compile_error()
    .into()
}
