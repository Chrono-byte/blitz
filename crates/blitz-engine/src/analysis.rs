// SPDX-License-Identifier: MPL-2.0
//! Compiler-owned source analysis for editor integrations.
//!
//! This module deliberately exposes a small, stable, serializable model rather
//! than leaking OXC AST types into language servers and editor extensions.

use crate::parser::Parser;
use oxc_semantic::SemanticBuilder;
use serde::Serialize;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

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

const BUILTIN_LIB_ID: &str = "lib.es2023.d.ts";
const BUILTIN_LIB_SOURCE: &str = include_str!("lib.es2023.d.ts");

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

#[derive(Clone)]
struct CachedProjectAnalysis {
  source: String,
  strict: bool,
  analysis: Analysis,
}

/// Persistent compiler state for an editor workspace.
#[derive(Default)]
pub struct AnalysisProject {
  overlays: HashMap<PathBuf, String>,
  module_declarations: HashMap<String, String>,
  declaration_cache: HashMap<PathBuf, String>,
  declaration_paths: HashMap<String, PathBuf>,
  analyses: HashMap<PathBuf, CachedProjectAnalysis>,
  dependencies: HashMap<PathBuf, HashSet<PathBuf>>,
  reverse_dependencies: HashMap<PathBuf, HashSet<PathBuf>>,
}

impl AnalysisProject {
  /// Create project state with declarations for runtime or package modules.
  #[must_use]
  pub fn with_module_declarations(
    declarations: impl IntoIterator<Item = (String, String)>
  ) -> Self {
    Self {
      module_declarations: declarations.into_iter().collect(),
      ..Self::default()
    }
  }

  /// Add or update an open document overlay.
  pub fn update_document(
    &mut self,
    source: String,
    path: &Path,
  ) {
    let path = normalize_path(path);
    if self.overlays.get(&path) != Some(&source) {
      self.overlays.insert(path.clone(), source);
      let declaration_changed = self.refresh_declaration_overlay(&path);
      if declaration_changed {
        self.analyses.clear();
      }
      self.invalidate(&path);
    }
  }

  /// Analyze and retain an open document, invalidating affected dependents when it changes.
  pub fn analyze_document(
    &mut self,
    source: String,
    path: &Path,
    strict: bool,
  ) -> Analysis {
    let path = normalize_path(path);
    self.update_document(source.clone(), &path);
    if self.discover_declarations(&source, &path) {
      // A newly discovered declaration can affect any already analyzed
      // document importing the same module specifier.
      self.analyses.clear();
    }
    if let Some(cached) = self.analyses.get(&path)
      && cached.source == source
      && cached.strict == strict
    {
      return cached.analysis.clone();
    }

    let mut dependencies = HashSet::new();
    let analysis = analyze_with_sources(
      &source,
      &path,
      strict,
      &self.overlays,
      &self.module_declarations,
      &mut dependencies,
    );
    self.record_dependencies(&path, dependencies);
    self.analyses.insert(
      path,
      CachedProjectAnalysis {
        source,
        strict,
        analysis: analysis.clone(),
      },
    );
    analysis
  }

  /// Remove an editor overlay and invalidate analyses that depended on it.
  pub fn close_document(
    &mut self,
    path: &Path,
  ) {
    let path = normalize_path(path);
    if self.overlays.remove(&path).is_some() {
      let declaration_changed = self.refresh_declaration_overlay(&path);
      if declaration_changed {
        self.analyses.clear();
      }
      self.invalidate(&path);
    }
    self.remove_dependencies(&path);
  }

  fn discover_declarations(
    &mut self,
    source: &str,
    importer: &Path,
  ) -> bool {
    let mut changed = false;
    for specifier in source_imports(source, importer) {
      changed |= self.discover_declaration(importer, &specifier);
    }
    changed
  }

  fn discover_declaration(
    &mut self,
    importer: &Path,
    specifier: &str,
  ) -> bool {
    if self.module_declarations.contains_key(specifier) {
      return false;
    }
    let Some(path) = resolve_declaration_path(importer, specifier, &self.overlays) else {
      return false;
    };
    let Some(source) = self.read_declaration(&path) else {
      return false;
    };
    self
      .declaration_paths
      .insert(specifier.to_string(), path.clone());
    self
      .module_declarations
      .insert(specifier.to_string(), source.clone());

    let mut changed = true;
    for dependency in source_imports(&source, &path) {
      changed |= self.discover_declaration(&path, &dependency);
    }
    changed
  }

  fn read_declaration(
    &mut self,
    path: &Path,
  ) -> Option<String> {
    if let Some(source) = self.overlays.get(path) {
      return Some(source.clone());
    }
    if let Some(source) = self.declaration_cache.get(path) {
      return Some(source.clone());
    }
    let source = std::fs::read_to_string(path).ok()?;
    self
      .declaration_cache
      .insert(path.to_path_buf(), source.clone());
    Some(source)
  }

  fn refresh_declaration_overlay(
    &mut self,
    path: &Path,
  ) -> bool {
    let affected = self
      .declaration_paths
      .iter()
      .filter(|(_, declaration_path)| declaration_path.as_path() == path)
      .map(|(specifier, _)| specifier.clone())
      .collect::<Vec<_>>();
    let changed = !affected.is_empty();
    for specifier in affected {
      if let Some(source) = self.read_declaration(path) {
        self.module_declarations.insert(specifier, source);
      } else {
        self.module_declarations.remove(&specifier);
        self.declaration_paths.remove(&specifier);
      }
    }
    changed
  }

  fn invalidate(
    &mut self,
    changed: &Path,
  ) {
    let mut pending = vec![changed.to_path_buf()];
    let mut invalidated = HashSet::new();
    while let Some(path) = pending.pop() {
      if !invalidated.insert(path.clone()) {
        continue;
      }
      self.analyses.remove(&path);
      if let Some(dependents) = self.reverse_dependencies.get(&path) {
        pending.extend(dependents.iter().cloned());
      }
    }
  }

  fn record_dependencies(
    &mut self,
    importer: &Path,
    dependencies: HashSet<PathBuf>,
  ) {
    self.remove_dependencies(importer);
    self
      .dependencies
      .insert(importer.to_path_buf(), dependencies.clone());
    for dependency in dependencies {
      self
        .reverse_dependencies
        .entry(dependency)
        .or_default()
        .insert(importer.to_path_buf());
    }
  }

  fn remove_dependencies(
    &mut self,
    importer: &Path,
  ) {
    let Some(previous) = self.dependencies.remove(importer) else {
      return;
    };
    for dependency in previous {
      let remove_entry = self
        .reverse_dependencies
        .get_mut(&dependency)
        .is_some_and(|importers| {
          importers.remove(importer);
          importers.is_empty()
        });
      if remove_entry {
        self.reverse_dependencies.remove(&dependency);
      }
    }
  }
}

/// Analyze one JavaScript or TypeScript source document.
#[must_use]
pub fn analyze(
  source: &str,
  path: &Path,
  strict: bool,
) -> Analysis {
  analyze_with_sources(
    source,
    path,
    strict,
    &HashMap::new(),
    &HashMap::new(),
    &mut HashSet::new(),
  )
}

fn analyze_with_sources(
  source: &str,
  path: &Path,
  strict: bool,
  overlays: &HashMap<PathBuf, String>,
  module_declarations: &HashMap<String, String>,
  dependencies: &mut HashSet<PathBuf>,
) -> Analysis {
  let mut virtual_documents = vec![AnalysisVirtualDocument {
    id: BUILTIN_LIB_ID.to_string(),
    text: BUILTIN_LIB_SOURCE.to_string(),
  }];
  let source_type = Parser::source_type_for_path(path, true);
  let parsed = Parser::parse_source_with_type(source, source_type);
  let (diagnostics, declarations, definitions, inlay_hints) = match parsed {
    Ok(session) => {
      let declarations = declaration_tree(session.program(), source);
      let mut definitions = definition_map(session.program(), &declarations);
      add_declaration_definitions(&declarations, &mut definitions);
      resolve_external_definitions(
        session.program(),
        path,
        overlays,
        module_declarations,
        dependencies,
        &mut definitions,
        &mut virtual_documents,
      );
      let imports = analysis_imports(session.program());
      let declaration_sources = imports
        .iter()
        .filter_map(|specifier| {
          module_declarations
            .get(specifier)
            .map(|source| crate::typecheck::VirtualDeclaration {
              specifier: specifier.clone(),
              source: source.clone(),
            })
        })
        .collect::<Vec<_>>();
      let source_imports = imports
        .into_iter()
        .filter(|specifier| {
          !module_declarations.contains_key(specifier)
            && (specifier.starts_with('.') || specifier.starts_with('/'))
        })
        .collect::<Vec<_>>();
      let (check_result, type_hints) =
        crate::typecheck::with_strict_override(strict.then_some(true), || {
          crate::typecheck::check_and_inlay_hints_with_declarations(
            session.program(),
            &declaration_sources,
            &source_imports,
          )
        });
      let diagnostics = check_result
        .err()
        .map(|error| diagnostic(error.message, error.source_location, source))
        .into_iter()
        .collect();
      let inlay_hints = type_hints
        .into_iter()
        .map(|hint| AnalysisInlayHint {
          position: hint.position,
          label: hint.label,
          kind: match hint.kind {
            crate::typecheck::TypeInlayHintKind::Type => "type",
            crate::typecheck::TypeInlayHintKind::Parameter => "parameter",
          },
        })
        .collect();
      (diagnostics, declarations, definitions, inlay_hints)
    }
    Err(error) => (
      vec![diagnostic(error.message, error.source_location, source)],
      Vec::new(),
      Vec::new(),
      Vec::new(),
    ),
  };

  Analysis {
    diagnostics,
    declarations,
    definitions,
    keywords: KEYWORDS.to_vec(),
    runtime_globals: RUNTIME_GLOBALS.to_vec(),
    virtual_documents,
    inlay_hints,
  }
}

fn analysis_imports(program: &oxc_ast::ast::Program<'_>) -> Vec<String> {
  let Ok(root) = serde_json::from_str::<Value>(&program.to_pretty_estree_json(true, false)) else {
    return Vec::new();
  };
  root
    .get("body")
    .and_then(Value::as_array)
    .into_iter()
    .flatten()
    .filter_map(|statement| {
      (statement.get("type").and_then(Value::as_str) == Some("ImportDeclaration"))
        .then(|| {
          statement
            .get("source")?
            .get("value")?
            .as_str()
            .map(str::to_string)
        })
        .flatten()
    })
    .collect()
}

fn source_imports(
  source: &str,
  path: &Path,
) -> Vec<String> {
  let source_type = Parser::source_type_for_path(path, true);
  Parser::parse_source_with_type(source, source_type)
    .ok()
    .map_or_else(Vec::new, |session| analysis_imports(session.program()))
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

fn declaration_tree(
  program: &oxc_ast::ast::Program<'_>,
  source: &str,
) -> Vec<AnalysisDeclaration> {
  let json = program.to_pretty_estree_json(true, false);
  serde_json::from_str::<Value>(&json)
    .map(|root| collect_declarations(&root, source))
    .unwrap_or_default()
}

fn collect_declarations(
  value: &Value,
  source: &str,
) -> Vec<AnalysisDeclaration> {
  match value {
    Value::Array(values) => values
      .iter()
      .flat_map(|value| collect_declarations(value, source))
      .collect(),
    Value::Object(object) => {
      let mut children = object
        .values()
        .flat_map(|value| collect_declarations(value, source))
        .collect::<Vec<_>>();
      declaration_from_object(object, source, &mut children)
        .map_or(children, |declaration| vec![declaration])
    }
    _ => Vec::new(),
  }
}

fn declaration_from_object(
  object: &serde_json::Map<String, Value>,
  source: &str,
  children: &mut Vec<AnalysisDeclaration>,
) -> Option<AnalysisDeclaration> {
  let node_type = object.get("type")?.as_str()?;
  let (kind, name_field) = match node_type {
    "FunctionDeclaration" => ("function", "id"),
    "ClassDeclaration" => ("class", "id"),
    "VariableDeclarator" => ("variable", "id"),
    "TSInterfaceDeclaration" => ("interface", "id"),
    "TSTypeAliasDeclaration" => ("type", "id"),
    "TSEnumDeclaration" => ("enum", "id"),
    "TSEnumMember" => ("enumMember", "id"),
    "TSModuleDeclaration" => ("namespace", "id"),
    "MethodDefinition" | "TSMethodSignature" => {
      let kind = match object.get("kind").and_then(Value::as_str) {
        Some("constructor") => "constructor",
        Some("get" | "set") => "property",
        _ => "method",
      };
      (kind, "key")
    }
    "PropertyDefinition" | "AccessorProperty" | "TSPropertySignature" => ("property", "key"),
    "Property"
      if object
        .get("value")
        .and_then(|value| value.get("type"))
        .and_then(Value::as_str)
        .is_some_and(|kind| matches!(kind, "ArrowFunctionExpression" | "FunctionExpression")) =>
    {
      ("method", "key")
    }
    _ => return None,
  };
  let name_node = object.get(name_field)?;
  let name = node_name(name_node)?;
  let (selection_start, _) = node_span(name_node)?;
  let selection_end = selection_start + name.len();
  let (start, end) = object_span(object).unwrap_or((selection_start, selection_end));
  let declaration_source = &source[start.min(source.len())..end.min(source.len())];
  let detail = if matches!(node_type, "TSPropertySignature" | "TSMethodSignature") {
    declaration_source
      .split_whitespace()
      .collect::<Vec<_>>()
      .join(" ")
  } else {
    let detail_end = declaration_source
      .find(['\n', '{'])
      .unwrap_or(declaration_source.len());
    declaration_source[..detail_end]
      .trim()
      .to_string()
  };
  Some(AnalysisDeclaration {
    name,
    kind,
    detail,
    start,
    end,
    selection_start,
    selection_end,
    children: std::mem::take(children),
  })
}

fn node_name(value: &Value) -> Option<String> {
  value
    .get("name")
    .and_then(Value::as_str)
    .or_else(|| value.get("value").and_then(Value::as_str))
    .map(str::to_string)
}

fn node_span(value: &Value) -> Option<(usize, usize)> {
  object_span(value.as_object()?)
}

fn object_span(object: &serde_json::Map<String, Value>) -> Option<(usize, usize)> {
  Some((
    usize::try_from(object.get("start")?.as_u64()?).ok()?,
    usize::try_from(object.get("end")?.as_u64()?).ok()?,
  ))
}

fn add_declaration_definitions(
  declarations: &[AnalysisDeclaration],
  definitions: &mut Vec<AnalysisDefinition>,
) {
  for declaration in declarations {
    let already_present = definitions.iter().any(|definition| {
      definition.start == declaration.selection_start && definition.end == declaration.selection_end
    });
    if !already_present {
      definitions.push(AnalysisDefinition {
        start: declaration.selection_start,
        end: declaration.selection_end,
        target_path: None,
        target_virtual: None,
        detail: None,
        target_start: declaration.start,
        target_end: declaration.end,
        target_selection_start: declaration.selection_start,
        target_selection_end: declaration.selection_end,
      });
    }
    add_declaration_definitions(&declaration.children, definitions);
  }
}

fn definition_map(
  program: &oxc_ast::ast::Program<'_>,
  declarations: &[AnalysisDeclaration],
) -> Vec<AnalysisDefinition> {
  let semantic = SemanticBuilder::new()
    .with_build_nodes(true)
    .build(program)
    .semantic;
  let scoping = semantic.scoping();
  let mut definitions = Vec::new();
  for symbol_id in scoping.symbol_ids() {
    let target = scoping.symbol_span(symbol_id);
    let (target_start, target_end) =
      containing_declaration_range(declarations, target.start as usize, target.end as usize)
        .unwrap_or((target.start as usize, target.end as usize));
    definitions.push(AnalysisDefinition {
      start: target.start as usize,
      end: target.end as usize,
      target_path: None,
      target_virtual: None,
      detail: None,
      target_start,
      target_end,
      target_selection_start: target.start as usize,
      target_selection_end: target.end as usize,
    });
    definitions.extend(
      semantic
        .symbol_references(symbol_id)
        .map(|reference| {
          let span = semantic.reference_span(reference);
          AnalysisDefinition {
            start: span.start as usize,
            end: span.end as usize,
            target_path: None,
            target_virtual: None,
            detail: None,
            target_start,
            target_end,
            target_selection_start: target.start as usize,
            target_selection_end: target.end as usize,
          }
        }),
    );
  }

  if let Ok(session) = Parser::parse_source_with_type(
    BUILTIN_LIB_SOURCE,
    Parser::source_type_for_path(Path::new(BUILTIN_LIB_ID), true),
  ) {
    let builtin_declarations = declaration_tree(session.program(), BUILTIN_LIB_SOURCE);
    let mut globals = HashMap::new();
    collect_builtin_globals(&builtin_declarations, &mut globals);
    for (name, reference_ids) in scoping.root_unresolved_references() {
      let Some(target) = globals.get(name.as_str()) else {
        continue;
      };
      for reference_id in reference_ids {
        let span = semantic.reference_span(scoping.get_reference(*reference_id));
        definitions.push(AnalysisDefinition {
          start: span.start as usize,
          end: span.end as usize,
          target_path: None,
          target_virtual: Some(BUILTIN_LIB_ID.to_string()),
          detail: Some(if target.kind == "variable" {
            format!("var {}", target.detail)
          } else {
            target.detail.clone()
          }),
          target_start: target.start,
          target_end: target.end,
          target_selection_start: target.selection_start,
          target_selection_end: target.selection_end,
        });
      }
    }
  }
  definitions
}

fn collect_builtin_globals<'a>(
  declarations: &'a [AnalysisDeclaration],
  globals: &mut HashMap<&'a str, &'a AnalysisDeclaration>,
) {
  for declaration in declarations {
    if matches!(declaration.kind, "variable" | "function") {
      globals.insert(&declaration.name, declaration);
    }
    collect_builtin_globals(&declaration.children, globals);
  }
}

fn containing_declaration_range(
  declarations: &[AnalysisDeclaration],
  selection_start: usize,
  selection_end: usize,
) -> Option<(usize, usize)> {
  for declaration in declarations {
    if declaration.selection_start == selection_start && declaration.selection_end == selection_end
    {
      return Some((declaration.start, declaration.end));
    }
    if let Some(range) =
      containing_declaration_range(&declaration.children, selection_start, selection_end)
    {
      return Some(range);
    }
  }
  None
}

#[derive(Clone, Debug)]
struct DefinitionTarget {
  path: Option<PathBuf>,
  virtual_id: Option<String>,
  detail: Option<String>,
  start: usize,
  end: usize,
  selection_start: usize,
  selection_end: usize,
}

type ExportMap = HashMap<String, Vec<DefinitionTarget>>;

fn resolve_external_definitions(
  program: &oxc_ast::ast::Program<'_>,
  path: &Path,
  overlays: &HashMap<PathBuf, String>,
  module_declarations: &HashMap<String, String>,
  dependencies: &mut HashSet<PathBuf>,
  definitions: &mut Vec<AnalysisDefinition>,
  virtual_documents: &mut Vec<AnalysisVirtualDocument>,
) {
  let Ok(root) = serde_json::from_str::<Value>(&program.to_pretty_estree_json(true, false)) else {
    return;
  };
  let Some(body) = root.get("body").and_then(Value::as_array) else {
    return;
  };
  let mut cache = HashMap::new();
  let mut visiting = HashSet::new();
  let mut imported_bindings = HashMap::<(usize, usize), Vec<DefinitionTarget>>::new();
  let mut namespaces = HashMap::<String, ExportMap>::new();
  let mut direct_links = Vec::new();

  for statement in body {
    let Some(object) = statement.as_object() else {
      continue;
    };
    match object.get("type").and_then(Value::as_str) {
      Some("ImportDeclaration") => {
        let Some(source) = object
          .get("source")
          .and_then(|source| source.get("value"))
          .and_then(Value::as_str)
        else {
          continue;
        };
        let Some(target_path) = resolve_module_path(path, source, overlays) else {
          continue;
        };
        dependencies.insert(target_path.clone());
        let exports = module_exports(
          &target_path,
          overlays,
          dependencies,
          &mut cache,
          &mut visiting,
        );
        for specifier in object
          .get("specifiers")
          .and_then(Value::as_array)
          .into_iter()
          .flatten()
        {
          let Some(specifier_object) = specifier.as_object() else {
            continue;
          };
          let Some(local) = specifier_object.get("local") else {
            continue;
          };
          let Some(local_name) = node_name(local) else {
            continue;
          };
          let Some((local_start, _)) = node_span(local) else {
            continue;
          };
          let local_span = (local_start, local_start + local_name.len());
          match specifier_object
            .get("type")
            .and_then(Value::as_str)
          {
            Some("ImportSpecifier") => {
              if let Some(imported_name) = specifier_object
                .get("imported")
                .and_then(node_name)
                && let Some(targets) = exports.get(&imported_name)
              {
                imported_bindings.insert(local_span, targets.clone());
              }
            }
            Some("ImportDefaultSpecifier") => {
              if let Some(targets) = exports.get("default") {
                imported_bindings.insert(local_span, targets.clone());
              }
            }
            Some("ImportNamespaceSpecifier") => {
              namespaces.insert(local_name, exports.clone());
            }
            _ => {}
          }
        }
      }
      Some("ExportNamedDeclaration")
        if object
          .get("source")
          .is_some_and(|value| !value.is_null()) =>
      {
        let Some(source) = object
          .get("source")
          .and_then(|source| source.get("value"))
          .and_then(Value::as_str)
        else {
          continue;
        };
        let Some(target_path) = resolve_module_path(path, source, overlays) else {
          continue;
        };
        dependencies.insert(target_path.clone());
        let exports = module_exports(
          &target_path,
          overlays,
          dependencies,
          &mut cache,
          &mut visiting,
        );
        for specifier in object
          .get("specifiers")
          .and_then(Value::as_array)
          .into_iter()
          .flatten()
        {
          let Some(local) = specifier.get("local") else {
            continue;
          };
          let Some(imported_name) = node_name(local) else {
            continue;
          };
          let Some((start, _)) = node_span(local) else {
            continue;
          };
          if let Some(targets) = exports.get(&imported_name) {
            direct_links.extend(
              targets
                .iter()
                .map(|target| definition_link(start, start + imported_name.len(), target)),
            );
          }
        }
      }
      _ => {}
    }
  }

  if !imported_bindings.is_empty() {
    *definitions = definitions
      .iter()
      .flat_map(|definition| {
        imported_bindings
          .get(&(
            definition.target_selection_start,
            definition.target_selection_end,
          ))
          .map_or_else(
            || vec![definition.clone()],
            |targets| {
              targets
                .iter()
                .map(|target| definition_link(definition.start, definition.end, target))
                .collect()
            },
          )
      })
      .collect();
  }
  collect_namespace_member_links(&root, &namespaces, &mut direct_links);
  definitions.extend(direct_links);
  resolve_declaration_module_members(&root, module_declarations, definitions, virtual_documents);
}

#[derive(Clone, Debug)]
struct DeclarationType {
  target: DefinitionTarget,
  members: HashMap<String, DefinitionTarget>,
}

type DeclarationTypes = HashMap<String, DeclarationType>;

fn resolve_declaration_module_members(
  root: &Value,
  module_declarations: &HashMap<String, String>,
  definitions: &mut Vec<AnalysisDefinition>,
  virtual_documents: &mut Vec<AnalysisVirtualDocument>,
) {
  let Some(body) = root.get("body").and_then(Value::as_array) else {
    return;
  };
  let mut namespaces = HashMap::<String, DeclarationTypes>::new();
  let mut named_types = HashMap::<String, DeclarationType>::new();
  let mut imported_bindings = HashMap::<(usize, usize), DefinitionTarget>::new();
  let mut used_declaration_sources = Vec::new();

  for statement in body {
    let Some(object) = statement.as_object() else {
      continue;
    };
    if object.get("type").and_then(Value::as_str) != Some("ImportDeclaration") {
      continue;
    }
    let Some(specifier) = object
      .get("source")
      .and_then(|source| source.get("value"))
      .and_then(Value::as_str)
    else {
      continue;
    };
    let Some(declaration_source) = module_declarations.get(specifier) else {
      continue;
    };
    used_declaration_sources.push(declaration_source.as_str());
    let virtual_id = declaration_virtual_id(specifier);
    let Some(types) = declaration_types(declaration_source, &virtual_id) else {
      continue;
    };
    if !virtual_documents
      .iter()
      .any(|document| document.id == virtual_id)
    {
      virtual_documents.push(AnalysisVirtualDocument {
        id: virtual_id.clone(),
        text: declaration_source.clone(),
      });
    }

    for import in object
      .get("specifiers")
      .and_then(Value::as_array)
      .into_iter()
      .flatten()
    {
      let Some(import_object) = import.as_object() else {
        continue;
      };
      let Some(local) = import_object.get("local") else {
        continue;
      };
      let Some(local_name) = node_name(local) else {
        continue;
      };
      let Some((start, _)) = node_span(local) else {
        continue;
      };
      let local_span = (start, start + local_name.len());
      match import_object.get("type").and_then(Value::as_str) {
        Some("ImportNamespaceSpecifier") => {
          namespaces.insert(local_name, types.clone());
        }
        Some("ImportSpecifier") => {
          let Some(imported_name) = import_object.get("imported").and_then(node_name) else {
            continue;
          };
          if let Some(declaration_type) = types.get(&imported_name) {
            named_types.insert(local_name, declaration_type.clone());
            imported_bindings.insert(local_span, declaration_type.target.clone());
          }
        }
        Some("ImportDefaultSpecifier") => {
          if let Some(declaration_type) = types.get("default") {
            named_types.insert(local_name, declaration_type.clone());
            imported_bindings.insert(local_span, declaration_type.target.clone());
          }
        }
        _ => {}
      }
    }
  }

  if !imported_bindings.is_empty() {
    *definitions = definitions
      .iter()
      .map(|definition| {
        imported_bindings
          .get(&(
            definition.target_selection_start,
            definition.target_selection_end,
          ))
          .map_or_else(
            || definition.clone(),
            |target| definition_link(definition.start, definition.end, target),
          )
      })
      .collect();
  }

  let mut typed_bindings = HashMap::new();
  collect_typed_bindings(root, &namespaces, &named_types, &mut typed_bindings);
  collect_typed_member_links(root, &typed_bindings, definitions);
  collect_declaration_namespace_links(root, &namespaces, definitions);

  for declaration_source in used_declaration_sources {
    for (namespace, dependency_specifier) in declaration_namespace_imports(declaration_source) {
      let Some(dependency_source) = module_declarations.get(&dependency_specifier) else {
        continue;
      };
      let dependency_virtual_id = declaration_virtual_id(&dependency_specifier);
      let Some(dependency_types) = declaration_types(dependency_source, &dependency_virtual_id)
      else {
        continue;
      };
      for (type_name, declaration_type) in dependency_types {
        let result_marker = format!("{namespace}.{type_name}");
        if !definitions.iter().any(|definition| {
          definition
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains(&result_marker))
        }) {
          continue;
        }
        if !virtual_documents
          .iter()
          .any(|document| document.id == dependency_virtual_id)
        {
          virtual_documents.push(AnalysisVirtualDocument {
            id: dependency_virtual_id.clone(),
            text: dependency_source.clone(),
          });
        }
        for (member_name, member_target) in declaration_type.members {
          collect_call_result_member_links(
            root,
            &result_marker,
            &member_name,
            &member_target,
            definitions,
          );
        }
      }
    }
  }
}

fn declaration_virtual_id(specifier: &str) -> String {
  let safe = specifier
    .chars()
    .map(|character| {
      if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
        character
      } else {
        '_'
      }
    })
    .collect::<String>();
  format!("{safe}.d.ts")
}

fn declaration_namespace_imports(source: &str) -> Vec<(String, String)> {
  let source_type = Parser::source_type_for_path(Path::new("module.d.ts"), true);
  let Ok(session) = Parser::parse_source_with_type(source, source_type) else {
    return Vec::new();
  };
  let Ok(root) = serde_json::from_str::<Value>(
    &session
      .program()
      .to_pretty_estree_json(true, false),
  ) else {
    return Vec::new();
  };
  root
    .get("body")
    .and_then(Value::as_array)
    .into_iter()
    .flatten()
    .filter_map(|statement| {
      let object = statement.as_object()?;
      if object.get("type").and_then(Value::as_str) != Some("ImportDeclaration") {
        return None;
      }
      let specifier = object
        .get("source")?
        .get("value")?
        .as_str()?
        .to_string();
      object
        .get("specifiers")?
        .as_array()?
        .iter()
        .find(|import| {
          import.get("type").and_then(Value::as_str) == Some("ImportNamespaceSpecifier")
        })
        .and_then(|import| import.get("local"))
        .and_then(node_name)
        .map(|namespace| (namespace, specifier))
    })
    .collect()
}

fn declaration_types(
  source: &str,
  virtual_id: &str,
) -> Option<DeclarationTypes> {
  let source_type = Parser::source_type_for_path(Path::new(virtual_id), true);
  let session = Parser::parse_source_with_type(source, source_type).ok()?;
  let declarations = declaration_tree(session.program(), source);
  Some(
    declarations
      .iter()
      .map(|declaration| {
        let target = virtual_definition_target(virtual_id, source, declaration);
        let members = declaration
          .children
          .iter()
          .map(|member| {
            (
              member.name.clone(),
              virtual_definition_target(virtual_id, source, member),
            )
          })
          .collect();
        (
          declaration.name.clone(),
          DeclarationType { target, members },
        )
      })
      .collect(),
  )
}

fn virtual_definition_target(
  virtual_id: &str,
  source: &str,
  declaration: &AnalysisDeclaration,
) -> DefinitionTarget {
  let detail = if matches!(declaration.kind, "variable" | "property" | "method") {
    source[declaration.start.min(source.len())..declaration.end.min(source.len())]
      .split_whitespace()
      .collect::<Vec<_>>()
      .join(" ")
  } else {
    declaration.detail.clone()
  };
  DefinitionTarget {
    path: None,
    virtual_id: Some(virtual_id.to_string()),
    detail: Some(detail),
    start: declaration.start,
    end: declaration.end,
    selection_start: declaration.selection_start,
    selection_end: declaration.selection_end,
  }
}

fn type_reference_parts(value: &Value) -> Option<(Option<String>, String)> {
  let annotation = value
    .get("typeAnnotation")?
    .get("typeAnnotation")?;
  if annotation.get("type").and_then(Value::as_str) != Some("TSTypeReference") {
    return None;
  }
  let type_name = annotation.get("typeName")?;
  match type_name.get("type").and_then(Value::as_str) {
    Some("Identifier") => Some((None, node_name(type_name)?)),
    Some("TSQualifiedName") => Some((
      Some(type_name.get("left").and_then(node_name)?),
      type_name.get("right").and_then(node_name)?,
    )),
    _ => None,
  }
}

fn collect_typed_bindings(
  value: &Value,
  namespaces: &HashMap<String, DeclarationTypes>,
  named_types: &HashMap<String, DeclarationType>,
  typed_bindings: &mut HashMap<(usize, usize), DeclarationType>,
) {
  match value {
    Value::Array(values) => {
      for value in values {
        collect_typed_bindings(value, namespaces, named_types, typed_bindings);
      }
    }
    Value::Object(object) => {
      if object.get("type").and_then(Value::as_str) == Some("Identifier")
        && let Some(name) = object.get("name").and_then(Value::as_str)
        && let Some((start, _)) = node_span(value)
        && let Some((namespace, type_name)) = type_reference_parts(value)
      {
        let declaration_type = namespace
          .as_ref()
          .and_then(|namespace| namespaces.get(namespace))
          .and_then(|types| types.get(&type_name))
          .or_else(|| named_types.get(&type_name));
        if let Some(declaration_type) = declaration_type {
          typed_bindings.insert((start, start + name.len()), declaration_type.clone());
        }
      }
      for child in object.values() {
        collect_typed_bindings(child, namespaces, named_types, typed_bindings);
      }
    }
    _ => {}
  }
}

fn collect_typed_member_links(
  value: &Value,
  typed_bindings: &HashMap<(usize, usize), DeclarationType>,
  definitions: &mut Vec<AnalysisDefinition>,
) {
  match value {
    Value::Array(values) => {
      for value in values {
        collect_typed_member_links(value, typed_bindings, definitions);
      }
    }
    Value::Object(object) => {
      if object.get("type").and_then(Value::as_str) == Some("MemberExpression")
        && object.get("computed").and_then(Value::as_bool) == Some(false)
        && let Some(object_node) = object.get("object")
        && let Some((object_start, object_end)) = node_span(object_node)
        && let Some(property) = object.get("property")
        && let Some(property_name) = node_name(property)
        && let Some((property_start, _)) = node_span(property)
        && let Some(binding_target) = definitions.iter().find(|definition| {
          definition.start == object_start
            && definition.end == object_end
            && definition.target_path.is_none()
            && definition.target_virtual.is_none()
        })
        && let Some(declaration_type) = typed_bindings.get(&(
          binding_target.target_selection_start,
          binding_target.target_selection_end,
        ))
        && let Some(target) = declaration_type.members.get(&property_name)
      {
        definitions.push(definition_link(
          property_start,
          property_start + property_name.len(),
          target,
        ));
      }
      for child in object.values() {
        collect_typed_member_links(child, typed_bindings, definitions);
      }
    }
    _ => {}
  }
}

fn collect_call_result_member_links(
  value: &Value,
  result_marker: &str,
  member_name: &str,
  member_target: &DefinitionTarget,
  definitions: &mut Vec<AnalysisDefinition>,
) {
  match value {
    Value::Array(values) => {
      for value in values {
        collect_call_result_member_links(
          value,
          result_marker,
          member_name,
          member_target,
          definitions,
        );
      }
    }
    Value::Object(object) => {
      if object.get("type").and_then(Value::as_str) == Some("MemberExpression")
        && object.get("computed").and_then(Value::as_bool) == Some(false)
        && object
          .get("property")
          .and_then(node_name)
          .as_deref()
          == Some(member_name)
        && let Some(call) = object.get("object")
        && call.get("type").and_then(Value::as_str) == Some("CallExpression")
        && let Some(callee) = call.get("callee")
        && let Some((callee_start, callee_end)) = expression_reference_span(callee)
        && definitions.iter().any(|definition| {
          definition.start == callee_start
            && definition.end == callee_end
            && definition
              .detail
              .as_deref()
              .is_some_and(|detail| detail.contains(result_marker))
        })
        && let Some(property) = object.get("property")
        && let Some((start, _)) = node_span(property)
      {
        definitions.push(definition_link(
          start,
          start + member_name.len(),
          member_target,
        ));
      }
      for child in object.values() {
        collect_call_result_member_links(
          child,
          result_marker,
          member_name,
          member_target,
          definitions,
        );
      }
    }
    _ => {}
  }
}

fn expression_reference_span(value: &Value) -> Option<(usize, usize)> {
  if value.get("type").and_then(Value::as_str) == Some("MemberExpression")
    && value.get("computed").and_then(Value::as_bool) == Some(false)
  {
    let property = value.get("property")?;
    let name = node_name(property)?;
    let (start, _) = node_span(property)?;
    Some((start, start + name.len()))
  } else {
    node_span(value)
  }
}

fn collect_declaration_namespace_links(
  value: &Value,
  namespaces: &HashMap<String, DeclarationTypes>,
  definitions: &mut Vec<AnalysisDefinition>,
) {
  match value {
    Value::Array(values) => {
      for value in values {
        collect_declaration_namespace_links(value, namespaces, definitions);
      }
    }
    Value::Object(object) => {
      let node_type = object.get("type").and_then(Value::as_str);
      let parts = match node_type {
        Some("TSQualifiedName") => Some((object.get("left"), object.get("right"))),
        Some("MemberExpression")
          if object.get("computed").and_then(Value::as_bool) == Some(false) =>
        {
          Some((object.get("object"), object.get("property")))
        }
        _ => None,
      };
      if let Some((Some(namespace_node), Some(member_node))) = parts
        && let Some(namespace) = node_name(namespace_node)
        && let Some(member_name) = node_name(member_node)
        && let Some((start, _)) = node_span(member_node)
        && let Some(target) = namespaces
          .get(&namespace)
          .and_then(|types| types.get(&member_name))
          .map(|declaration_type| &declaration_type.target)
      {
        definitions.push(definition_link(start, start + member_name.len(), target));
      }
      for child in object.values() {
        collect_declaration_namespace_links(child, namespaces, definitions);
      }
    }
    _ => {}
  }
}

fn definition_link(
  start: usize,
  end: usize,
  target: &DefinitionTarget,
) -> AnalysisDefinition {
  AnalysisDefinition {
    start,
    end,
    target_path: target
      .path
      .as_ref()
      .map(|path| path.to_string_lossy().into_owned()),
    target_virtual: target.virtual_id.clone(),
    detail: target.detail.clone(),
    target_start: target.start,
    target_end: target.end,
    target_selection_start: target.selection_start,
    target_selection_end: target.selection_end,
  }
}

fn normalize_path(path: &Path) -> PathBuf {
  if let Ok(path) = std::fs::canonicalize(path) {
    return path;
  }
  let absolute = if path.is_absolute() {
    path.to_path_buf()
  } else {
    std::env::current_dir()
      .map(|directory| directory.join(path))
      .unwrap_or_else(|_| path.to_path_buf())
  };
  let mut normalized = PathBuf::new();
  for component in absolute.components() {
    match component {
      std::path::Component::CurDir => {}
      std::path::Component::ParentDir => {
        normalized.pop();
      }
      _ => normalized.push(component.as_os_str()),
    }
  }
  normalized
}

fn resolve_module_path(
  importer: &Path,
  specifier: &str,
  overlays: &HashMap<PathBuf, String>,
) -> Option<PathBuf> {
  let base = importer
    .parent()
    .unwrap_or_else(|| Path::new("."));
  if let Ok(path) = crate::resolver::resolve_specifier(base, specifier) {
    return Some(normalize_path(&path));
  }
  if !specifier.starts_with('.') && !specifier.starts_with('/') {
    return None;
  }

  let candidate = normalize_path(&base.join(specifier));
  let mut candidates = vec![candidate.clone()];
  if candidate.extension().is_none() {
    for extension in ["ts", "tsx", "js", "jsx", "mts", "cts", "mjs", "cjs"] {
      candidates.push(candidate.with_extension(extension));
      candidates.push(candidate.join(format!("index.{extension}")));
    }
  } else if matches!(
    candidate
      .extension()
      .and_then(|extension| extension.to_str()),
    Some("js" | "jsx" | "mjs" | "cjs")
  ) {
    for extension in ["ts", "tsx", "mts", "cts"] {
      candidates.push(candidate.with_extension(extension));
    }
  }
  candidates
    .into_iter()
    .map(|path| normalize_path(&path))
    .find(|path| overlays.contains_key(path))
}

fn resolve_declaration_path(
  importer: &Path,
  specifier: &str,
  overlays: &HashMap<PathBuf, String>,
) -> Option<PathBuf> {
  let base = importer
    .parent()
    .unwrap_or_else(|| Path::new("."));
  if let Ok(path) = crate::resolver::resolve_declaration_specifier(base, specifier) {
    return Some(normalize_path(&path));
  }

  let candidate = normalize_path(&base.join(specifier));
  let mut candidates = vec![candidate.clone()];
  if candidate.extension().is_none() {
    for extension in ["d.ts", "d.mts", "d.cts"] {
      candidates.push(candidate.with_extension(extension));
      candidates.push(candidate.join(format!("index.{extension}")));
    }
  } else {
    for extension in ["d.ts", "d.mts", "d.cts"] {
      candidates.push(candidate.with_extension(extension));
    }
  }
  candidates
    .into_iter()
    .map(|path| normalize_path(&path))
    .find(|path| overlays.contains_key(path))
}

fn module_exports(
  path: &Path,
  overlays: &HashMap<PathBuf, String>,
  dependencies: &mut HashSet<PathBuf>,
  cache: &mut HashMap<PathBuf, ExportMap>,
  visiting: &mut HashSet<PathBuf>,
) -> ExportMap {
  let path = normalize_path(path);
  if let Some(exports) = cache.get(&path) {
    return exports.clone();
  }
  if !visiting.insert(path.clone()) {
    return ExportMap::new();
  }
  let Some((source, root, declarations)) = parse_module_analysis(&path, overlays) else {
    visiting.remove(&path);
    return ExportMap::new();
  };
  let local = top_level_declarations(&path, &declarations);
  let mut exports = direct_exports(&root, &source, &path, &local);
  cache.insert(path.clone(), exports.clone());

  if let Some(body) = root.get("body").and_then(Value::as_array) {
    for statement in body {
      let Some(object) = statement.as_object() else {
        continue;
      };
      match object.get("type").and_then(Value::as_str) {
        Some("ExportNamedDeclaration")
          if object
            .get("source")
            .is_some_and(|value| !value.is_null()) =>
        {
          let Some(source_specifier) = object
            .get("source")
            .and_then(|source| source.get("value"))
            .and_then(Value::as_str)
          else {
            continue;
          };
          let Some(dependency_path) = resolve_module_path(&path, source_specifier, overlays) else {
            continue;
          };
          dependencies.insert(dependency_path.clone());
          let dependency =
            module_exports(&dependency_path, overlays, dependencies, cache, visiting);
          for specifier in object
            .get("specifiers")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
          {
            let Some(local_name) = specifier.get("local").and_then(node_name) else {
              continue;
            };
            let Some(exported_name) = specifier.get("exported").and_then(node_name) else {
              continue;
            };
            if let Some(targets) = dependency.get(&local_name) {
              exports.insert(exported_name, targets.clone());
            }
          }
        }
        Some("ExportAllDeclaration") => {
          let Some(source_specifier) = object
            .get("source")
            .and_then(|source| source.get("value"))
            .and_then(Value::as_str)
          else {
            continue;
          };
          let Some(dependency_path) = resolve_module_path(&path, source_specifier, overlays) else {
            continue;
          };
          dependencies.insert(dependency_path.clone());
          let dependency =
            module_exports(&dependency_path, overlays, dependencies, cache, visiting);
          if let Some(exported_name) = object.get("exported").and_then(node_name) {
            // Namespace exports resolve their members when imported as a namespace.
            for (name, targets) in dependency {
              exports.insert(format!("{exported_name}.{name}"), targets);
            }
          } else {
            for (name, targets) in dependency {
              if name != "default" {
                exports.entry(name).or_insert(targets);
              }
            }
          }
        }
        _ => {}
      }
    }
  }
  visiting.remove(&path);
  cache.insert(path, exports.clone());
  exports
}

fn parse_module_analysis(
  path: &Path,
  overlays: &HashMap<PathBuf, String>,
) -> Option<(String, Value, Vec<AnalysisDeclaration>)> {
  let source = overlays
    .get(path)
    .cloned()
    .or_else(|| std::fs::read_to_string(path).ok())?;
  let source_type = Parser::source_type_for_path(path, true);
  let session = Parser::parse_source_with_type(&source, source_type).ok()?;
  let root = serde_json::from_str::<Value>(
    &session
      .program()
      .to_pretty_estree_json(true, false),
  )
  .ok()?;
  let declarations = declaration_tree(session.program(), &source);
  Some((source, root, declarations))
}

fn top_level_declarations(
  path: &Path,
  declarations: &[AnalysisDeclaration],
) -> ExportMap {
  declarations
    .iter()
    .map(|declaration| {
      (
        declaration.name.clone(),
        vec![DefinitionTarget {
          path: Some(path.to_path_buf()),
          virtual_id: None,
          detail: Some(declaration.detail.clone()),
          start: declaration.start,
          end: declaration.end,
          selection_start: declaration.selection_start,
          selection_end: declaration.selection_end,
        }],
      )
    })
    .collect()
}

fn direct_exports(
  root: &Value,
  source: &str,
  path: &Path,
  local: &ExportMap,
) -> ExportMap {
  let mut exports = ExportMap::new();
  let Some(body) = root.get("body").and_then(Value::as_array) else {
    return exports;
  };
  for statement in body {
    let Some(object) = statement.as_object() else {
      continue;
    };
    match object.get("type").and_then(Value::as_str) {
      Some("ExportNamedDeclaration") if object.get("source").is_none_or(Value::is_null) => {
        if let Some(declaration) = object
          .get("declaration")
          .filter(|value| !value.is_null())
        {
          for item in collect_declarations(declaration, source) {
            if let Some(targets) = local.get(&item.name) {
              exports.insert(item.name, targets.clone());
            }
          }
        }
        for specifier in object
          .get("specifiers")
          .and_then(Value::as_array)
          .into_iter()
          .flatten()
        {
          let Some(local_name) = specifier.get("local").and_then(node_name) else {
            continue;
          };
          let Some(exported_name) = specifier.get("exported").and_then(node_name) else {
            continue;
          };
          if let Some(targets) = local.get(&local_name) {
            exports.insert(exported_name, targets.clone());
          }
        }
      }
      Some("ExportDefaultDeclaration") => {
        let Some(declaration) = object.get("declaration") else {
          continue;
        };
        if let Some(name) = node_name(declaration)
          && let Some(targets) = local.get(&name)
        {
          exports.insert("default".to_string(), targets.clone());
          continue;
        }
        let declarations = collect_declarations(declaration, source);
        if let Some(item) = declarations.first() {
          exports.insert(
            "default".to_string(),
            vec![DefinitionTarget {
              path: Some(path.to_path_buf()),
              virtual_id: None,
              detail: Some(item.detail.clone()),
              start: item.start,
              end: item.end,
              selection_start: item.selection_start,
              selection_end: item.selection_end,
            }],
          );
        }
      }
      _ => {}
    }
  }
  exports
}

fn collect_namespace_member_links(
  value: &Value,
  namespaces: &HashMap<String, ExportMap>,
  definitions: &mut Vec<AnalysisDefinition>,
) {
  match value {
    Value::Array(values) => {
      for value in values {
        collect_namespace_member_links(value, namespaces, definitions);
      }
    }
    Value::Object(object) => {
      if object.get("type").and_then(Value::as_str) == Some("MemberExpression")
        && object.get("computed").and_then(Value::as_bool) == Some(false)
        && let Some(namespace) = object.get("object").and_then(node_name)
        && let Some(property) = object.get("property")
        && let Some(property_name) = node_name(property)
        && let Some(exports) = namespaces.get(&namespace)
        && let Some(targets) = exports.get(&property_name)
        && let Some((start, _)) = node_span(property)
      {
        definitions.extend(
          targets
            .iter()
            .map(|target| definition_link(start, start + property_name.len(), target)),
        );
      }
      for child in object.values() {
        collect_namespace_member_links(child, namespaces, definitions);
      }
    }
    _ => {}
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::sync::atomic::{AtomicUsize, Ordering};

  fn temporary_directory(label: &str) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let directory = std::env::temp_dir().join(format!(
      "blitz-analysis-{label}-{}-{}",
      std::process::id(),
      NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&directory).expect("create analysis fixture");
    directory
  }

  #[test]
  fn reports_declarations_and_runtime_globals() {
    let result = analyze(
      crate::js!(
        typescript,
        "interface Answer { value: number }\nconst answer: Answer = { value: 42 };\nfunction read() { return answer; }"
      ),
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
  fn reports_nested_declarations() {
    let result = analyze(
      crate::js!(
        typescript,
        "class Service { value: number; read() { const local = 1; return local; } }"
      ),
      Path::new("symbols.ts"),
      false,
    );

    let service = &result.declarations[0];
    assert_eq!(service.name, "Service");
    assert_eq!(service.kind, "class");
    assert_eq!(
      service
        .children
        .iter()
        .map(|declaration| (declaration.name.as_str(), declaration.kind))
        .collect::<Vec<_>>(),
      [("value", "property"), ("read", "method")]
    );
    assert_eq!(service.children[1].children[0].name, "local");
    assert!(service.start < service.selection_start);
    assert!(service.end > service.selection_end);

    let object = analyze(
      "const query = define({ handler: (context) => { const user = context.user; return user; } });",
      Path::new("object-symbols.ts"),
      false,
    );
    let query = &object.declarations[0];
    assert_eq!(query.children[0].name, "handler");
    assert_eq!(query.children[0].kind, "method");
    assert_eq!(query.children[0].children[0].name, "user");
  }

  #[test]
  fn resolves_shadowed_references_by_symbol_identity() {
    let source = "const value = 1; function read() { const value = 2; return value; } value;";
    let result = analyze(source, Path::new("definitions.ts"), false);
    let outer_declaration = source.find("value").expect("outer declaration");
    let inner_declaration = source[outer_declaration + 1..]
      .find("value")
      .map(|offset| outer_declaration + 1 + offset)
      .expect("inner declaration");
    let inner_reference = source.rfind("value; }").expect("inner reference");
    let outer_reference = source.rfind("value;").expect("outer reference");

    assert!(result.definitions.iter().any(|definition| {
      definition.start == inner_reference && definition.target_start == inner_declaration
    }));
    assert!(result.definitions.iter().any(|definition| {
      definition.start == outer_reference && definition.target_start == outer_declaration
    }));
  }

  #[test]
  fn reports_contextual_callback_parameter_inlay_hints() {
    let declaration = r#"
      export interface HandlerContext { readonly userId: string }
      export declare const configure: (options: {
        handler: (context: HandlerContext) => void
      }) => void;
    "#;
    let source = r#"
      import { configure } from "example/context";
      configure({ handler: (context) => context.userId });
    "#;
    let mut project = AnalysisProject::with_module_declarations([(
      "example/context".to_string(),
      declaration.to_string(),
    )]);
    let result = project.analyze_document(source.to_string(), Path::new("editor.ts"), false);

    assert_eq!(
      result
        .inlay_hints
        .iter()
        .map(|hint| (hint.label.as_str(), hint.kind))
        .collect::<Vec<_>>(),
      [("options:", "parameter"), (": { userId: string }", "type")]
    );
  }

  #[test]
  fn serializes_selected_argument_parameter_name_hints() {
    let result = analyze(
      "declare function send(message: string): void; declare const input: string; send(input);",
      Path::new("parameter-hints.ts"),
      false,
    );

    assert_eq!(result.inlay_hints.len(), 1);
    assert_eq!(result.inlay_hints[0].label, "message:");
    assert_eq!(result.inlay_hints[0].kind, "parameter");
  }

  #[test]
  fn resolves_members_from_imported_module_declarations() {
    let declaration = r#"
      import type * as Effect from "effect/Effect";
      export interface Connection {
        readonly send: (message: string) => Effect.Effect<void>;
        readonly close: () => Effect.Effect<void>;
      }
      export declare const connect: () => Effect.Effect<Connection>;
    "#;
    let effect_declaration = r#"
      export interface Effect<Value> {
        readonly pipe: { <Result>(f: (value: Value) => Result): Result };
      }
    "#;
    let source = r#"
      import * as WebSocket from "blitz:websocket";
      const transmit = (connection: WebSocket.Connection) => connection.send("hello").pipe();
      WebSocket.connect().pipe();
    "#;
    let mut project = AnalysisProject::with_module_declarations([
      ("blitz:websocket".to_string(), declaration.to_string()),
      ("effect/Effect".to_string(), effect_declaration.to_string()),
    ]);
    let result = project.analyze_document(source.to_string(), Path::new("editor.ts"), false);
    let start = source.rfind("send").unwrap();
    let definition = result
      .definitions
      .iter()
      .find(|definition| definition.start == start)
      .unwrap();

    assert_eq!(
      definition.target_virtual.as_deref(),
      Some("blitz_websocket.d.ts")
    );
    assert!(
      definition
        .detail
        .as_deref()
        .unwrap()
        .contains("send")
    );
    assert!(
      result
        .virtual_documents
        .iter()
        .any(|document| document.id == "blitz_websocket.d.ts")
    );
    assert_eq!(
      result
        .definitions
        .iter()
        .filter(|definition| {
          definition.target_virtual.as_deref() == Some("effect_Effect.d.ts")
            && definition
              .detail
              .as_deref()
              .is_some_and(|detail| detail.contains("pipe"))
        })
        .count(),
      2
    );
  }

  #[test]
  fn discovers_package_json_declarations_for_contextual_typing() {
    let directory = temporary_directory("package");
    let source_directory = directory.join("src");
    let types_directory = directory
      .join("node_modules")
      .join("fixture-types");
    let legacy_directory = directory
      .join("node_modules")
      .join("fixture-typings");
    std::fs::create_dir_all(&source_directory).expect("create source directory");
    std::fs::create_dir_all(types_directory.join("types")).expect("create types directory");
    std::fs::create_dir_all(&legacy_directory).expect("create legacy package directory");
    std::fs::write(
      types_directory.join("package.json"),
      r#"{
        "name": "fixture-types",
        "types": "./types/index.d.ts",
        "exports": {
          ".": {
            "import": "./index.js",
            "types": "./types/index.d.ts"
          }
        }
      }"#,
    )
    .expect("write types package manifest");
    std::fs::write(
      types_directory.join("index.js"),
      "export function configure() {};",
    )
    .expect("write runtime entry");
    std::fs::write(
      types_directory.join("types/index.d.ts"),
      "export declare const configure: (options: { handler: (context: { userId: string }) => void }) => void;",
    )
    .expect("write package declaration");
    std::fs::write(
      legacy_directory.join("package.json"),
      r#"{ "name": "fixture-typings", "typings": "./index.d.ts" }"#,
    )
    .expect("write legacy package manifest");
    std::fs::write(
      legacy_directory.join("index.d.ts"),
      "export declare const create: (options: { handler: (context: { name: string }) => void }) => void;",
    )
    .expect("write legacy declaration");

    let path = source_directory.join("editor.ts");
    let source = r#"
      import { configure } from "fixture-types";
      import { create } from "fixture-typings";
      configure({ handler: (context) => context.userId });
      create({ handler: (context) => context.name });
    "#;
    let mut project = AnalysisProject::default();
    let result = project.analyze_document(source.to_string(), &path, false);

    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    assert_eq!(
      result
        .inlay_hints
        .iter()
        .filter(|hint| hint.kind == "type")
        .map(|hint| hint.label.as_str())
        .collect::<Vec<_>>(),
      [": { userId: string }", ": { name: string }"]
    );
    assert!(
      result.virtual_documents.iter().any(|document| {
        document.id == "fixture-types.d.ts" && document.text.contains("userId")
      })
    );
    assert!(
      result.virtual_documents.iter().any(|document| {
        document.id == "fixture-typings.d.ts" && document.text.contains("name")
      })
    );
    let configure_start = source
      .find("configure")
      .expect("configure import");
    assert!(result.definitions.iter().any(|definition| {
      definition.start == configure_start
        && definition.target_virtual.as_deref() == Some("fixture-types.d.ts")
    }));
    assert_eq!(project.declaration_cache.len(), 2);

    std::fs::remove_dir_all(directory).expect("remove analysis fixture");
  }

  #[test]
  fn discovers_relative_dts_entries_and_keeps_them_virtual() {
    let directory = temporary_directory("relative-dts");
    let source_directory = directory.join("src");
    std::fs::create_dir_all(&source_directory).expect("create source directory");
    let declaration = source_directory.join("nearby.d.ts");
    std::fs::write(
      &declaration,
      "export declare const configure: (options: { handler: (context: { value: number }) => void }) => void;",
    )
    .expect("write nearby declaration");

    let path = source_directory.join("editor.ts");
    let source = r#"
      import { configure } from "./nearby";
      configure({ handler: (context) => context.value });
    "#;
    let mut project = AnalysisProject::default();
    let result = project.analyze_document(source.to_string(), &path, false);

    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    assert!(
      result
        .inlay_hints
        .iter()
        .any(|hint| { hint.kind == "type" && hint.label == ": { value: number }" })
    );
    let configure_start = source
      .find("configure")
      .expect("configure import");
    assert!(result.definitions.iter().any(|definition| {
      definition.start == configure_start
        && definition.target_virtual.as_deref() == Some("__nearby.d.ts")
    }));
    assert!(
      result
        .virtual_documents
        .iter()
        .any(|document| document.id == "__nearby.d.ts")
    );

    project.update_document(
      "export declare const configure: (options: { handler: (context: { value: string }) => void }) => void;"
        .to_string(),
      &declaration,
    );
    let updated = project.analyze_document(source.to_string(), &path, false);
    assert!(
      updated
        .inlay_hints
        .iter()
        .any(|hint| { hint.kind == "type" && hint.label == ": { value: string }" })
    );

    std::fs::remove_dir_all(directory).expect("remove analysis fixture");
  }

  #[test]
  fn project_uses_open_document_overlays_and_invalidates_dependents() {
    let directory = temporary_directory("overlays");
    let dependency = directory.join("dependency.ts");
    let importer = directory.join("importer.ts");
    let importer_source =
      "import { value } from \"./dependency.ts\";\nconsole.log(value);".to_string();
    let mut project = AnalysisProject::default();

    project.analyze_document("export const value = 1;".to_string(), &dependency, false);
    let first = project.analyze_document(importer_source.clone(), &importer, false);
    let first_target = first
      .definitions
      .iter()
      .find(|definition| definition.target_path.as_deref() == dependency.to_str())
      .unwrap()
      .target_selection_start;

    project.analyze_document(
      "// unsaved change\nexport const value = 2;".to_string(),
      &dependency,
      false,
    );
    let second = project.analyze_document(importer_source, &importer, false);
    let second_target = second
      .definitions
      .iter()
      .find(|definition| definition.target_path.as_deref() == dependency.to_str())
      .unwrap()
      .target_selection_start;

    assert!(second_target > first_target);

    std::fs::remove_dir_all(directory).expect("remove analysis fixture");
  }

  #[test]
  fn resolves_ecmascript_builtin_globals_to_virtual_declarations() {
    let source = "const values = new Map<string, number>();";
    let result = analyze(source, Path::new("editor.ts"), false);
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    let start = source.find("Map").unwrap();
    let definition = result
      .definitions
      .iter()
      .find(|definition| definition.start == start)
      .unwrap();

    assert_eq!(definition.target_virtual.as_deref(), Some(BUILTIN_LIB_ID));
    assert!(
      definition
        .detail
        .as_deref()
        .unwrap()
        .contains("MapConstructor")
    );
    assert!(
      result
        .virtual_documents
        .iter()
        .any(|document| document.id == BUILTIN_LIB_ID)
    );
  }

  #[test]
  fn treats_unloaded_editor_imports_as_external_bindings() {
    let result = analyze(
      "import * as Ref from \"effect/Ref\"; type ValueRef = Ref.Ref<number>; Ref.make(1);",
      Path::new("editor.ts"),
      false,
    );

    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
  }

  #[test]
  fn reports_type_errors() {
    let result = analyze(
      crate::js!(typescript, "const answer: number = 'wrong';"),
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
