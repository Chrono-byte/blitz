//! Compiler-owned source analysis for editor integrations.
//!
//! This module deliberately exposes a small, stable, serializable model rather
//! than leaking OXC AST types into language servers and editor extensions.

use crate::parser::Parser;
use serde::Serialize;
use std::path::Path;

/// Analysis returned to editor tooling.
#[derive(Debug, Serialize)]
pub struct Analysis {
  /// Syntax and type errors found in the source.
  pub diagnostics: Vec<AnalysisDiagnostic>,
  /// Top-level source declarations.
  pub declarations: Vec<AnalysisDeclaration>,
  /// ECMAScript and TypeScript keywords offered by completion.
  pub keywords: Vec<&'static str>,
  /// Blitz runtime globals offered by completion and hover.
  pub runtime_globals: Vec<AnalysisRuntimeGlobal>,
}

/// An editor diagnostic using UTF-8 byte offsets.
#[derive(Debug, Serialize)]
pub struct AnalysisDiagnostic {
  /// Human-readable diagnostic text.
  pub message: String,
  /// Inclusive start byte offset.
  pub start: usize,
  /// Exclusive end byte offset.
  pub end: usize,
}

/// A top-level declaration and its source selection.
#[derive(Debug, Serialize)]
pub struct AnalysisDeclaration {
  /// Declared identifier.
  pub name: String,
  /// LSP-friendly declaration category.
  pub kind: &'static str,
  /// Human-readable declaration summary.
  pub detail: String,
  /// Inclusive identifier start byte offset.
  pub start: usize,
  /// Exclusive identifier end byte offset.
  pub end: usize,
}

/// A global installed by the Blitz runtime.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct AnalysisRuntimeGlobal {
  /// Global identifier.
  pub name: &'static str,
  /// Short hover/completion description.
  pub detail: &'static str,
}

const KEYWORDS: &[&str] = &[
  "async",
  "await",
  "break",
  "case",
  "catch",
  "class",
  "const",
  "continue",
  "debugger",
  "default",
  "delete",
  "do",
  "else",
  "export",
  "extends",
  "false",
  "finally",
  "for",
  "from",
  "function",
  "if",
  "implements",
  "import",
  "in",
  "instanceof",
  "interface",
  "let",
  "new",
  "null",
  "of",
  "return",
  "satisfies",
  "static",
  "super",
  "switch",
  "this",
  "throw",
  "true",
  "try",
  "type",
  "typeof",
  "undefined",
  "var",
  "void",
  "while",
  "with",
  "yield",
];

const RUNTIME_GLOBALS: &[AnalysisRuntimeGlobal] = &[
  AnalysisRuntimeGlobal {
    name: "Blitz",
    detail: "Blitz runtime namespace",
  },
  AnalysisRuntimeGlobal {
    name: "console",
    detail: "Console output API",
  },
  AnalysisRuntimeGlobal {
    name: "process",
    detail: "Blitz process API",
  },
  AnalysisRuntimeGlobal {
    name: "fetch",
    detail: "Fetch a network resource",
  },
  AnalysisRuntimeGlobal {
    name: "Request",
    detail: "Web Request constructor",
  },
  AnalysisRuntimeGlobal {
    name: "Response",
    detail: "Web Response constructor",
  },
  AnalysisRuntimeGlobal {
    name: "Headers",
    detail: "Web Headers constructor",
  },
  AnalysisRuntimeGlobal {
    name: "WebSocket",
    detail: "WebSocket constructor",
  },
];

/// Analyze one JavaScript or TypeScript source document.
#[must_use]
pub fn analyze(
  source: &str,
  path: &Path,
  strict: bool,
) -> Analysis {
  let source_type = Parser::source_type_for_path(path, true);
  let parsed = Parser::parse_source_with_type(source, source_type);
  let (diagnostics, declarations) = match parsed {
    Ok(session) => {
      let declarations = scan_declarations(source);
      let diagnostics = crate::typecheck::with_strict_override(strict.then_some(true), || {
        crate::typecheck::check(session.program())
          .err()
          .map(|error| diagnostic(error.message, error.source_location, source))
          .into_iter()
          .collect()
      });
      (diagnostics, declarations)
    }
    Err(error) => (
      vec![diagnostic(error.message, error.source_location, source)],
      Vec::new(),
    ),
  };

  Analysis {
    diagnostics,
    declarations,
    keywords: KEYWORDS.to_vec(),
    runtime_globals: RUNTIME_GLOBALS.to_vec(),
  }
}

fn diagnostic(
  message: String,
  location: Option<crate::errors::SourceLocation>,
  source: &str,
) -> AnalysisDiagnostic {
  let (start, end) = location.map_or_else(
    || quoted_source_span(&message, source).unwrap_or((0, usize::from(!source.is_empty()))),
    |location| {
      (
        location.start.min(source.len()),
        location.end.min(source.len()),
      )
    },
  );
  AnalysisDiagnostic {
    message,
    start,
    end: end.max(start.saturating_add(usize::from(start < source.len()))),
  }
}

fn quoted_source_span(
  message: &str,
  source: &str,
) -> Option<(usize, usize)> {
  let mut candidates = message.split('\'').skip(1).step_by(2);
  let mut result = None;
  for candidate in &mut candidates {
    if candidate.is_empty()
      || !candidate
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$'))
    {
      continue;
    }
    if let Some(start) = source.find(candidate) {
      result = Some((start, start + candidate.len()));
    }
  }
  result
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TokenKind {
  Identifier,
  Keyword,
  Symbol,
}

#[derive(Clone, Copy)]
struct Token<'a> {
  text: &'a str,
  start: usize,
  end: usize,
  kind: TokenKind,
  depth: usize,
}

fn scan_declarations(source: &str) -> Vec<AnalysisDeclaration> {
  let tokens = scan(source);
  let mut declarations = Vec::new();
  for (index, token) in tokens.iter().enumerate() {
    if token.depth != 0 || token.kind != TokenKind::Keyword {
      continue;
    }
    let kind = match token.text {
      "function" => "function",
      "class" => "class",
      "interface" => "interface",
      "type" => "type",
      "const" | "let" | "var" => "variable",
      _ => continue,
    };
    let Some(identifier) = tokens[index + 1..]
      .iter()
      .take_while(|candidate| candidate.text != ";" && candidate.text != "{")
      .find(|candidate| candidate.kind == TokenKind::Identifier)
    else {
      continue;
    };
    let line_start = source[..token.start]
      .rfind('\n')
      .map_or(0, |offset| offset + 1);
    let line_end = source[token.start..]
      .find('\n')
      .map_or(source.len(), |offset| token.start + offset);
    declarations.push(AnalysisDeclaration {
      name: identifier.text.to_string(),
      kind,
      detail: source[line_start..line_end].trim().to_string(),
      start: identifier.start,
      end: identifier.end,
    });
  }
  declarations
}

fn scan(source: &str) -> Vec<Token<'_>> {
  let bytes = source.as_bytes();
  let mut tokens = Vec::new();
  let mut offset = 0;
  let mut depth = 0;
  while offset < bytes.len() {
    if bytes[offset].is_ascii_whitespace() {
      offset += 1;
      continue;
    }
    if source[offset..].starts_with("//") {
      offset += 2;
      while offset < bytes.len() && bytes[offset] != b'\n' {
        offset += 1;
      }
      continue;
    }
    if source[offset..].starts_with("/*") {
      offset += 2;
      while offset + 1 < bytes.len() && !source[offset..].starts_with("*/") {
        offset += 1;
      }
      offset = (offset + 2).min(bytes.len());
      continue;
    }
    if matches!(bytes[offset], b'\'' | b'"' | b'`') {
      let quote = bytes[offset];
      offset += 1;
      while offset < bytes.len() {
        if bytes[offset] == b'\\' {
          offset = (offset + 2).min(bytes.len());
        } else if bytes[offset] == quote {
          offset += 1;
          break;
        } else {
          offset += 1;
        }
      }
      continue;
    }
    let start = offset;
    if bytes[offset].is_ascii_alphabetic() || matches!(bytes[offset], b'_' | b'$') {
      offset += 1;
      while offset < bytes.len()
        && (bytes[offset].is_ascii_alphanumeric() || matches!(bytes[offset], b'_' | b'$'))
      {
        offset += 1;
      }
      let text = &source[start..offset];
      tokens.push(Token {
        text,
        start,
        end: offset,
        kind: if KEYWORDS.contains(&text) {
          TokenKind::Keyword
        } else {
          TokenKind::Identifier
        },
        depth,
      });
      continue;
    }
    offset += 1;
    let text = &source[start..offset];
    tokens.push(Token {
      text,
      start,
      end: offset,
      kind: TokenKind::Symbol,
      depth,
    });
    if text == "{" {
      depth += 1;
    } else if text == "}" {
      depth = depth.saturating_sub(1);
    }
  }
  tokens
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn reports_declarations_and_runtime_globals() {
    let result = analyze(
      "interface Answer { value: number }\nconst answer: Answer = { value: 42 };\nfunction read() { return answer; }",
      Path::new("example.ts"),
      false,
    );

    assert!(result.diagnostics.is_empty());
    assert_eq!(
      result
        .declarations
        .iter()
        .map(|declaration| declaration.name.as_str())
        .collect::<Vec<_>>(),
      ["Answer", "answer", "read"]
    );
    assert!(
      result
        .runtime_globals
        .iter()
        .any(|global| global.name == "Blitz")
    );
  }

  #[test]
  fn reports_type_errors() {
    let result = analyze(
      "const answer: number = 'wrong';",
      Path::new("example.ts"),
      false,
    );

    assert_eq!(result.diagnostics.len(), 1);
    assert!(
      result.diagnostics[0]
        .message
        .contains("not assignable")
    );
  }
}
