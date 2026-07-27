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

/// Parser facade around Oxc's source-text parser.
///
/// Use `new_from_source` with `parse`, or use the convenience `parse_source`
/// and `parse_source_with_type` functions.
pub struct Parser {
  /// Source text retained until `parse` consumes it.
  source: Option<String>,
}

// Helper methods for ParserSession are defined after the macro-generated type.

// ParserSession: safe self-referencing wrapper that owns the allocator and
// source string while exposing an `oxc_ast::ast::Program` that borrows them.
// We use `ouroboros::self_referencing` to safely model the self-referencing
// lifetime of `oast::Program<'this>`.

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
    // Default to ordinary script parsing for callers that don't provide
    // filename/extension information. Callers that need module semantics
    // must opt in explicitly via `parse_source_with_type(..., SourceType::mjs())`
    // or the appropriate TypeScript variant.
    Self::parse_source_with_type(source, SourceType::script())
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
    // Build a ParserSession with the source type requested by the caller.
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

    // Build a self-referencing ParserSession using try_new so we can return
    // parsing errors as ParserError when the underlying Oxc parser fails.
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
    // Run Oxc parser using a temporary allocator and borrowed source string.
    // We only need the ESTree JSON here so keeping allocator scoped is fine.
    let alloc = Allocator::default();
    let leaked_source = source; // short-lived borrow for parser
    // Default to JavaScript parsing for import extraction.
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
      // Treat `import ...` and re-export forms (`export * from`,
      // `export { ... } from`) as module dependencies for the module loader.
      if let Some(typ) = item.get("type").and_then(|t| t.as_str())
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
      if let Some(typ) = item.get("type").and_then(|t| t.as_str())
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

// Implement a small accessor for the generated ParserSession type. The
// `self_referencing` macro generates a `borrow_program` method; expose a
// convenient `program()` method that returns the borrowed `Program`.
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
