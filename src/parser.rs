//! Parser adapter that delegates parsing to the Oxc parser and returns the
//! produced Oxc AST (`oxc_ast::ast::Program`).
//!
//! Historically this module converted Oxc's ESTree JSON into an internal
//! `tsblitz::ast` representation. During the migration to use `oxc_ast` natively
//! the legacy conversion helpers were removed. Callers should use
//! `Parser::parse_source(...)` which returns an `oast::Program` (an alias for
//! `oxc_ast::ast::Program`).

use crate::errors::ParserError;
use oxc_ast::ast as oast;

use ouroboros::self_referencing;
use oxc_allocator::Allocator;
use oxc_parser::{ParseOptions, Parser as OxcParser};
use oxc_span::SourceType;

use serde_json::Value;

/// Parser facade. Historically this crate had a token-based parser that took
/// tokens produced by `Lexer`. We now prefer parsing source text directly
/// with Oxc. To ease migration, we keep a `new_from_source` constructor and a
/// `parse` method that operates on source text.
///
/// NOTE: Calling `Parser::new(tokens)` (the legacy token-based constructor)
/// then `parse()` is no longer supported. Use `Parser::new_from_source(...)`
/// or the convenience `parse_source(...)` function instead.
pub struct Parser {
    /// If present, source text to parse. (We do not currently support the old
    /// token-based entry path.)
    source: Option<String>,
}

/// A ParserSession owns the source string and allocator and exposes the
/// parsed `oast::Program` which borrows both. The self-referential struct is
/// implemented using `ouroboros` so the owned data stays alive for the
/// lifetime of the returned session without unsafe leaks.
#[self_referencing]
pub struct ParserSession {
    // owned source string
    source: String,
    // owned allocator
    allocator: Allocator,
    // borrows from source and allocator
    #[borrows(source, allocator)]
    #[covariant]
    program: oast::Program<'this>,
}

impl ParserSession {
    /// Create a new ParserSession by parsing `source` and returning the
    /// session which owns required backing data.
    pub fn try_new(source: &str) -> Result<Self, ParserError> {
        let source_owned = source.to_string();
        let alloc = Allocator::default();
        let ret = OxcParser::new(&alloc, &source_owned, SourceType::ts())
            .with_options(ParseOptions {
                parse_regular_expression: true,
                ..ParseOptions::default()
            })
            .parse();

        if !ret.errors.is_empty() {
            let mut msgs = Vec::new();
            for e in ret.errors {
                let err = e.with_source_code(source_owned.clone());
                msgs.push(format!("{:?}", err));
            }
            return Err(ParserError::from(msgs.join("\n")));
        }

        // Build the self-referential struct with owned source/allocator and
        // borrowed program. `ouroboros` ensures lifetimes are sound.
        // Use `move` so the parser return (`ret`) is moved into the closure.
        Ok(ParserSessionTryBuilder {
            source: source_owned,
            allocator: alloc,
            program_builder: move |s, a| -> Result<oast::Program<'_>, ParserError> { Ok(ret.program) },
        }
        .try_build()?)
    }

    /// Access the parsed program borrowed from the session.
    pub fn program(&self) -> &oast::Program<'_> {
        self.borrow_program()
    }
}

impl Parser {
    /// Legacy constructor kept for API compatibility. This will return an error
    /// at parse time instructing users to switch to `new_from_source`.
    #[deprecated(
        since = "0.1.0",
        note = "Use `Parser::new_from_source(...)` or `parse_source(...)` instead"
    )]
    pub fn new(_tokens: Vec<crate::lexer::Token>) -> Self {
        Self { source: None }
    }

    /// Construct a parser from a source text string. This is the preferred
    /// constructor: it uses the Oxc parser under the hood.
    pub fn new_from_source(source: &str) -> Self {
        Self {
            source: Some(source.to_string()),
        }
    }

    /// Convenience top-level API: parse source text and return a session that
    /// safely owns the source and allocator while exposing a borrowed
    /// `oast::Program` tied to them. This replaces the previous leaking
    /// behaviour and avoids process-wide leaks.
    pub fn parse_source(source: &str) -> Result<ParserSession, ParserError> {
        ParserSession::try_new(source)
    }

    /// Parse and return an `oxc_ast::ast::Program`.
    ///
    /// If `self.source` is None, returns an error explaining that the token
    /// based constructor is no longer supported.
    pub fn parse(&mut self) -> Result<oast::Program, ParserError> {
        let source_owned = match self.source.take() {
            Some(s) => s,
            None => {
                return Err(ParserError::from(
                    "Parser::new(tokens) is no longer supported. Please use Parser::new_from_source(source) or Parser::parse_source(source).",
                ));
            }
        };

        // Run Oxc parser (TypeScript mode by default)
        let alloc = Allocator::default();
        let ret = OxcParser::new(&alloc, &source_owned, SourceType::ts())
            .with_options(ParseOptions {
                parse_regular_expression: true,
                ..ParseOptions::default()
            })
            .parse();

        if !ret.errors.is_empty() {
            let mut msgs = Vec::new();
            for e in ret.errors {
                let err = e.with_source_code(source_owned.clone());
                msgs.push(format!("{:?}", err));
            }
            return Err(ParserError::from(msgs.join("\n")));
        }

        // Return the parsed oxc AST program directly. The program borrows the
        // allocator and source owned by this Parser instance.
        Ok(ret.program)
    }

    // Backwards-compatible helper: parse_source_into_local_ast for consumers that
    // still expect the old `crate::ast::Program`. This converts oxc_ast -> crate::ast
    // using a minimal adapter so we can migrate the callsites incrementally.
    #[allow(dead_code)]
    pub fn parse_source_into_local_ast(_source: &str) -> Result<crate::ast::Program, ParserError> {
        Err(ParserError::from("parse_source_into_local_ast is removed; use parse_source which returns oxc_ast::ast::Program".to_string()))
    }

    /// Extract top-level static import specifiers (e.g. "./mod") from a source
    /// text without converting the entire ESTree into the internal AST. This
    /// helper is used by the bundler/loader to build the module graph.
    pub fn extract_imports_from_source(source: &str) -> Result<Vec<String>, ParserError> {
        // Run Oxc parser using a temporary allocator and borrowed source string.
        // We only need the ESTree JSON here so keeping allocator scoped is fine.
        let alloc = Allocator::default();
        let leaked_source = source; // short-lived borrow for parser
        let ret = OxcParser::new(&alloc, leaked_source, SourceType::ts())
            .with_options(ParseOptions {
                parse_regular_expression: true,
                ..ParseOptions::default()
            })
            .parse();

        if !ret.errors.is_empty() {
            let mut msgs = Vec::new();
            for e in ret.errors {
                let err = e.with_source_code(source.to_string());
                msgs.push(format!("{:?}", err));
            }
            return Err(ParserError::from(msgs.join("\n")));
        }

        let estree_json = ret.program.to_pretty_estree_ts_json(false);
        let root: Value = serde_json::from_str(&estree_json)
            .map_err(|e| ParserError::from(format!("Failed to parse Oxc ESTree JSON: {}", e)))?;

        let body = root
            .get("body")
            .and_then(|b| b.as_array())
            .ok_or_else(|| ParserError::from("Expected program 'body' array".to_string()))?;

        let mut imports = Vec::new();
        for item in body {
            // Treat `import ...` and re-export forms (`export * from`,
            // `export { ... } from`) as module dependencies for bundling.
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

// Note:
// The detailed ESTree -> local AST conversion helpers that used to live in this
// module were removed as part of the migration to use `oxc_ast` directly. If a
// compatibility layer is required (to produce the old owned AST), implement it
// in a separate module so consumers can migrate incrementally.
