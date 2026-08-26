// SPDX-License-Identifier: MPL-2.0

use serde::Serialize;

/// Analysis returned to editor tooling.
#[derive(Clone, Debug, Serialize)]
pub struct Analysis {
  /// Syntax and type errors found in the source.
  pub diagnostics: Vec<AnalysisDiagnostic>,
  /// Source declarations arranged as a navigation tree.
  pub declarations: Vec<AnalysisDeclaration>,
  /// Semantically resolved references and their declaration targets.
  pub definitions: Vec<AnalysisDefinition>,
  /// ECMAScript and TypeScript keywords offered by completion.
  pub keywords: Vec<&'static str>,
  /// Blitz runtime globals offered by completion and hover.
  pub runtime_globals: Vec<AnalysisRuntimeGlobal>,
  /// Compiler-owned declaration documents referenced by definitions.
  pub virtual_documents: Vec<AnalysisVirtualDocument>,
  /// Contextual types inferred for editor inlay hints.
  pub inlay_hints: Vec<AnalysisInlayHint>,
}

/// An editor diagnostic using UTF-8 byte offsets.
#[derive(Clone, Debug, Serialize)]
pub struct AnalysisDiagnostic {
  /// Human-readable diagnostic text.
  pub message: String,
  /// Inclusive start byte offset.
  pub start: usize,
  /// Exclusive end byte offset.
  pub end: usize,
}

/// A declaration in the document navigation tree.
#[derive(Clone, Debug, Serialize)]
pub struct AnalysisDeclaration {
  /// Declared identifier.
  pub name: String,
  /// LSP-friendly declaration category.
  pub kind: &'static str,
  /// Human-readable declaration summary.
  pub detail: String,
  /// Markdown documentation attached to the declaration.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub documentation: Option<String>,
  /// Inclusive declaration start byte offset.
  pub start: usize,
  /// Exclusive declaration end byte offset.
  pub end: usize,
  /// Inclusive declared-name start byte offset.
  pub selection_start: usize,
  /// Exclusive declared-name end byte offset.
  pub selection_end: usize,
  /// Declarations contained by this declaration.
  pub children: Vec<AnalysisDeclaration>,
}

/// A reference resolved to a declaration in the analyzed document.
#[derive(Clone, Debug, Serialize)]
pub struct AnalysisDefinition {
  /// Inclusive reference start byte offset.
  pub start: usize,
  /// Exclusive reference end byte offset.
  pub end: usize,
  /// Canonical filesystem path for an external target.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub target_path: Option<String>,
  /// Compiler-owned virtual declaration document for a built-in target.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub target_virtual: Option<String>,
  /// Hover text supplied by an external or built-in declaration.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub detail: Option<String>,
  /// Markdown documentation supplied by the target declaration.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub documentation: Option<String>,
  /// Inclusive target declaration start byte offset.
  pub target_start: usize,
  /// Exclusive target declaration end byte offset.
  pub target_end: usize,
  /// Inclusive target identifier start byte offset.
  pub target_selection_start: usize,
  /// Exclusive target identifier end byte offset.
  pub target_selection_end: usize,
}

/// A global installed by the Blitz runtime.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct AnalysisRuntimeGlobal {
  /// Global identifier.
  pub name: &'static str,
  /// Short hover/completion description.
  pub detail: &'static str,
}

/// Compiler-owned source used as a definition target.
#[derive(Clone, Debug, Serialize)]
pub struct AnalysisVirtualDocument {
  /// Stable virtual document identifier.
  pub id: String,
  /// Declaration source text.
  pub text: String,
}

/// A compiler-inferred annotation at a source position.
#[derive(Clone, Debug, Serialize)]
pub struct AnalysisInlayHint {
  /// UTF-8 byte position where the hint is inserted.
  pub position: usize,
  /// Rendered TypeScript annotation.
  pub label: String,
  /// Hint category used by protocol adapters.
  pub kind: &'static str,
}
