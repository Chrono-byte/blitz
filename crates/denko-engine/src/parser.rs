// SPDX-License-Identifier: MPL-2.0
//! Parser adapter using OXC for TypeScript/JavaScript parsing.
//!
//! This module provides a thin wrapper around the OXC parser, delegating parsing
//! to OXC and returning the produced AST (`oxc_ast::ast::Program`). It handles:
//! - Source text parsing into AST
//! - Import extraction for module graph building
//! - Error conversion to Blitz error types
//!
//! # Example
//!
//! ```no_run
//! use denko::{js, parser::Parser};
//!
//! let source = js!(typescript, "let x: number = 42;");
//! let program = Parser::parse_source(source)?;
//! // Use program AST for compilation or analysis
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

use crate::errors::ParserError;
use oxc_ast::ast as oast;

use ouroboros::self_referencing;
use oxc_allocator::Allocator;
use oxc_parser::{ParseOptions, Parser as OxcParser};
use oxc_span::SourceType;

use serde_json::Value;
use std::path::Path;

pub(crate) const EVAL_NEW_TARGET_SENTINEL: &str = "__denko_internal_eval_new_target_7f3a__";

fn rewrite_eval_new_target(source: &str) -> String {
  #[derive(Clone, Copy)]
  enum State {
    Code,
    SingleQuoted,
    DoubleQuoted,
    Template,
    LineComment,
    BlockComment,
  }

  let bytes = source.as_bytes();
  let mut output = Vec::with_capacity(source.len());
  let mut state = State::Code;
  let mut index = 0;
  while index < bytes.len() {
    match state {
      State::Code if bytes[index..].starts_with(b"new.target") => {
        let before_is_ident = index > 0
          && (bytes[index - 1].is_ascii_alphanumeric() || matches!(bytes[index - 1], b'_' | b'$'));
        let end = index + b"new.target".len();
        let after_is_ident = end < bytes.len()
          && (bytes[end].is_ascii_alphanumeric() || matches!(bytes[end], b'_' | b'$'));
        if !before_is_ident && !after_is_ident {
          output.extend_from_slice(EVAL_NEW_TARGET_SENTINEL.as_bytes());
          index = end;
          continue;
        }
        output.push(bytes[index]);
      }
      State::Code => {
        output.push(bytes[index]);
        state = match bytes[index] {
          b'\'' => State::SingleQuoted,
          b'"' => State::DoubleQuoted,
          b'`' => State::Template,
          b'/' if bytes.get(index + 1) == Some(&b'/') => State::LineComment,
          b'/' if bytes.get(index + 1) == Some(&b'*') => State::BlockComment,
          _ => State::Code,
        };
      }
      State::SingleQuoted | State::DoubleQuoted | State::Template => {
        output.push(bytes[index]);
        let delimiter = match state {
          State::SingleQuoted => b'\'',
          State::DoubleQuoted => b'"',
          State::Template => b'`',
          _ => unreachable!(),
        };
        if bytes[index] == b'\\' && index + 1 < bytes.len() {
          index += 1;
          output.push(bytes[index]);
        } else if bytes[index] == delimiter {
          state = State::Code;
        }
      }
      State::LineComment => {
        output.push(bytes[index]);
        if matches!(bytes[index], b'\n' | b'\r') {
          state = State::Code;
        }
      }
      State::BlockComment => {
        output.push(bytes[index]);
        if bytes[index] == b'*' && bytes.get(index + 1) == Some(&b'/') {
          index += 1;
          output.push(b'/');
          state = State::Code;
        }
      }
    }
    index += 1;
  }
  String::from_utf8(output).expect("eval rewrite preserves UTF-8 source bytes")
}

/// Parser facade around Oxc's source-text parser.
///
/// Use `new_from_source` with `parse`, or use the convenience `parse_source`
/// and `parse_source_with_type` functions.
pub struct Parser {
  /// Source text retained until `parse` consumes it.
  source: Option<String>,
}

#[self_referencing]
pub struct ParserSession {
  alloc: Allocator,
  source: String,
  #[borrows(alloc, source)]
  #[covariant]
  program: oast::Program<'this>,
}

impl Parser {
  /// Select the parser mode for a source file without changing the historical
  /// script semantics of `.js` files.
  #[must_use]
  pub fn source_type_for_path(
    path: &Path,
    module: bool,
  ) -> SourceType {
    let extension = path
      .extension()
      .and_then(|extension| extension.to_str())
      .unwrap_or_default();
    let inferred = SourceType::from_path(path).unwrap_or_else(|_| SourceType::script());

    if module {
      return inferred.with_module(true);
    }

    match extension {
      "mjs" | "mts" | "cjs" | "cts" => inferred,
      _ => inferred.with_module(false),
    }
  }

  /// Construct a parser from a source text string. This is the preferred
  /// constructor: it uses the Oxc parser under the hood.
  pub fn new_from_source(source: &str) -> Self {
    Self {
      source: Some(source.to_string()),
    }
  }

  /// Convenience top-level API: parse source text and return a
  /// `ParserSession` which owns backing data (allocator + source) and
  /// exposes the parsed `Program` that borrows that backing data.
  ///
  /// # Errors
  ///
  pub fn parse_source(source: &str) -> Result<ParserSession, ParserError> {
    Self::parse_source_with_type(source, SourceType::script())
  }

  /// Parse direct-eval Script code in the syntactic context inherited from
  /// its caller. Oxc's function-context option is required to admit
  /// `new.target`; the compiler still applies eval-specific early errors.
  pub(crate) fn parse_eval_source(
    source: &str,
    allow_new_target: bool,
  ) -> Result<ParserSession, ParserError> {
    let source = if allow_new_target {
      rewrite_eval_new_target(source)
    } else {
      source.to_string()
    };
    ParserSession::try_new(Allocator::default(), source, |alloc, source| {
      let ret = OxcParser::new(alloc, source, SourceType::script())
        .with_options(ParseOptions {
          parse_regular_expression: true,
          allow_return_outside_function: allow_new_target,
          ..ParseOptions::default()
        })
        .parse();
      if !ret.diagnostics.is_empty() {
        return Err(ParserError::from(
          ret
            .diagnostics
            .into_iter()
            .map(|error| format!("{:?}", error.with_source_code(source.to_string())))
            .collect::<Vec<_>>()
            .join("\n"),
        ));
      }
      Ok(ret.program)
    })
  }

  /// Parse source text with an explicit Oxc SourceType (js or ts).
  ///
  /// # Errors
  /// Throws an error if parsing the given source fails.
  pub fn parse_source_with_type(
    source: &str,
    source_type: SourceType,
  ) -> Result<ParserSession, ParserError> {
    let _p = Parser::new_from_source(source);
    ParserSession::try_new(Allocator::default(), source.to_string(), |alloc, source| {
      let ret = OxcParser::new(alloc, source, source_type)
        .with_options(ParseOptions {
          parse_regular_expression: true,
          ..ParseOptions::default()
        })
        .parse();

      if !ret.diagnostics.is_empty() {
        let mut msgs = Vec::new();
        for e in ret.diagnostics {
          let err = e.with_source_code(source.to_string());
          msgs.push(format!("{:?}", err));
        }
        return Err(ParserError::from(msgs.join("\n")));
      }

      Ok(ret.program)
    })
  }

  /// Parse and return a `ParserSession` owning allocator+source and
  /// exposing an `oxc_ast::ast::Program` that borrows them.
  ///
  /// The source is consumed, so a parser instance can be parsed only once.
  ///
  /// # Errors
  /// Returns an error if the parse fails.
  pub fn parse(&mut self) -> Result<ParserSession, ParserError> {
    let source_owned = match self.source.take() {
      Some(s) => s,
      None => {
        return Err(ParserError::from(
          "Parser::new(tokens) is no longer supported. Please use Parser::new_from_source(source) or Parser::parse_source(source).",
        ));
      }
    };

    ParserSession::try_new(Allocator::default(), source_owned, |alloc, source| {
      let ret = OxcParser::new(alloc, source, SourceType::unambiguous())
        .with_options(ParseOptions {
          parse_regular_expression: true,
          ..ParseOptions::default()
        })
        .parse();

      if !ret.diagnostics.is_empty() {
        let mut msgs = Vec::new();
        for e in ret.diagnostics {
          let err = e.with_source_code(source.to_string());
          msgs.push(format!("{:?}", err));
        }
        return Err(ParserError::from(msgs.join("\n")));
      }

      Ok(ret.program)
    })
  }

  /// Extract top-level static import specifiers (e.g. "./mod") from a source
  /// text without converting the entire ESTree into the internal AST. This
  /// helper is used by the module loader to build the module graph.
  ///
  /// # Errors
  /// Returns a ParserError if:
  /// - The Oxc parser reports syntax or other parse errors. All reported errors are
  ///   converted to strings and combined into the returned ParserError.
  /// - The ESTree JSON produced by Oxc cannot be parsed (serde_json failure).
  /// - The parsed program does not contain an expected "body" array.
  /// - In each case the returned ParserError contains a human-readable description
  ///   of the failure.
  pub fn extract_imports_from_source(source: &str) -> Result<Vec<String>, ParserError> {
    let alloc = Allocator::default();
    let leaked_source = source;
    let ret = OxcParser::new(&alloc, leaked_source, SourceType::unambiguous())
      .with_options(ParseOptions {
        parse_regular_expression: true,
        ..ParseOptions::default()
      })
      .parse();

    if !ret.diagnostics.is_empty() {
      let mut msgs = Vec::new();
      for e in ret.diagnostics {
        let err = e.with_source_code(source.to_string());
        msgs.push(format!("{:?}", err));
      }
      return Err(ParserError::from(msgs.join("\n")));
    }

    let estree_json = ret.program.to_pretty_estree_json(true, false);
    let root: Value = serde_json::from_str(&estree_json)
      .map_err(|e| ParserError::from(format!("Failed to parse Oxc ESTree JSON: {}", e)))?;

    let body = root
      .get("body")
      .and_then(|b| b.as_array())
      .ok_or_else(|| ParserError::from("Expected program 'body' array".to_string()))?;

    let mut imports = Vec::new();
    for item in body {
      let type_only = item
        .get("importKind")
        .or_else(|| item.get("exportKind"))
        .and_then(Value::as_str)
        == Some("type");
      if !type_only
        && let Some(typ) = item.get("type").and_then(|t| t.as_str())
        && (typ == "ImportDeclaration"
          || typ == "ExportAllDeclaration"
          || typ == "ExportNamedDeclaration")
        && let Some(source_node) = item.get("source")
        && let Some(val) = source_node.get("value").and_then(|v| v.as_str())
      {
        imports.push(val.to_string());
      }
    }
    Ok(imports)
  }

  /// Extract top-level static import specifiers from source text with explicit SourceType.
  ///
  /// # Errors
  /// Returns a ParserError if:
  /// - The Oxc parser reports syntax or other parse errors. All reported errors are
  ///   converted to strings and combined into the returned ParserError.
  /// - The ESTree JSON produced by Oxc cannot be parsed (serde_json failure).
  /// - The parsed program does not contain an expected "body" array.
  /// - In each case the returned ParserError contains a human-readable description
  ///   of the failure.
  pub fn extract_imports_from_source_with_type(
    source: &str,
    source_type: SourceType,
  ) -> Result<Vec<String>, ParserError> {
    let alloc = Allocator::default();
    let leaked_source = source;
    let ret = OxcParser::new(&alloc, leaked_source, source_type)
      .with_options(ParseOptions {
        parse_regular_expression: true,
        ..ParseOptions::default()
      })
      .parse();

    if !ret.diagnostics.is_empty() {
      let mut msgs = Vec::new();
      for e in ret.diagnostics {
        let err = e.with_source_code(source.to_string());
        msgs.push(format!("{:?}", err));
      }
      return Err(ParserError::from(msgs.join("\n")));
    }

    let estree_json = ret.program.to_pretty_estree_json(true, false);
    let root: Value = serde_json::from_str(&estree_json)
      .map_err(|e| ParserError::from(format!("Failed to parse Oxc ESTree JSON: {}", e)))?;

    let body = root
      .get("body")
      .and_then(|b| b.as_array())
      .ok_or_else(|| ParserError::from("Expected program 'body' array".to_string()))?;

    let mut imports = Vec::new();
    for item in body {
      let type_only = item
        .get("importKind")
        .or_else(|| item.get("exportKind"))
        .and_then(Value::as_str)
        == Some("type");
      if !type_only
        && let Some(typ) = item.get("type").and_then(|t| t.as_str())
        && (typ == "ImportDeclaration"
          || typ == "ExportAllDeclaration"
          || typ == "ExportNamedDeclaration")
        && let Some(source_node) = item.get("source")
        && let Some(val) = source_node.get("value").and_then(|v| v.as_str())
      {
        imports.push(val.to_string());
      }
    }
    Ok(imports)
  }
}

impl ParserSession {
  /// Simple get borrow program
  pub fn program(&self) -> &oast::Program<'_> {
    self.borrow_program()
  }

  /// Access the underlying source text
  pub fn source(&self) -> &str {
    self.borrow_source()
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn type_only_imports_are_not_runtime_dependencies() {
    let source = crate::js!(
      typescript,
      r#"
        import type { Missing } from "./types.ts";
        export type { AlsoMissing } from "./other-types.ts";
        import { value, type Shape } from "./runtime.ts";
      "#
    );

    let imports =
      Parser::extract_imports_from_source_with_type(source, SourceType::ts().with_module(true))
        .expect("extract TypeScript imports");

    assert_eq!(imports, vec!["./runtime.ts"]);
  }
}
