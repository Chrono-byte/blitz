// SPDX-License-Identifier: MPL-2.0
//! Compiler-owned source analysis for editor integrations.
//!
//! This module deliberately exposes a small, stable, serializable model rather
//! than leaking OXC AST types into language servers and editor extensions.

mod model;
mod project;

use crate::parser::Parser;
use oxc_semantic::SemanticBuilder;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

pub use model::{
  Analysis, AnalysisDeclaration, AnalysisDefinition, AnalysisDiagnostic, AnalysisInlayHint,
  AnalysisRuntimeGlobal, AnalysisVirtualDocument,
};

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

pub use project::AnalysisProject;
use project::AnalysisSources;

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
    AnalysisSources {
      overlays: &HashMap::new(),
      module_declarations: &HashMap::new(),
      module_documentation: &HashMap::new(),
      declaration_paths: &HashMap::new(),
    },
    &mut HashSet::new(),
  )
}

fn analyze_with_sources(
  source: &str,
  path: &Path,
  strict: bool,
  sources: AnalysisSources<'_>,
  dependencies: &mut HashSet<PathBuf>,
) -> Analysis {
  let module_declarations = sources.module_declarations;
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
        sources,
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
      let (check_result, type_hints) = if declaration_dependencies_available(&declaration_sources) {
        crate::typecheck::with_strict_override(strict.then_some(true), || {
          crate::typecheck::check_and_inlay_hints_with_declarations(
            session.program(),
            &declaration_sources,
            &source_imports,
          )
        })
      } else {
        (Ok(()), Vec::new())
      };
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

fn declaration_dependencies_available(
  declarations: &[crate::typecheck::VirtualDeclaration]
) -> bool {
  let available = declarations
    .iter()
    .map(|declaration| declaration.specifier.as_str())
    .collect::<HashSet<_>>();
  declarations.iter().all(|declaration| {
    source_imports(&declaration.source, Path::new("virtual.d.ts"))
      .iter()
      .all(|specifier| available.contains(specifier.as_str()))
  })
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
    documentation: preceding_jsdoc(source, start),
    start,
    end,
    selection_start,
    selection_end,
    children: std::mem::take(children),
  })
}

fn preceding_jsdoc(
  source: &str,
  declaration_start: usize,
) -> Option<String> {
  let before = source.get(..declaration_start)?;
  let comment_start = before.rfind("/**")?;
  let comment = before.get(comment_start..)?;
  let comment_end = comment.find("*/")? + 2;
  let prefix = comment[comment_end..].trim();
  if !prefix.is_empty()
    && !prefix.split_whitespace().all(|token| {
      matches!(
        token,
        "export" | "default" | "declare" | "const" | "let" | "var"
      )
    })
  {
    return None;
  }
  let body = comment.get(3..comment_end.saturating_sub(2))?;
  let documentation = body
    .lines()
    .map(|line| {
      let line = line.trim_start();
      line
        .strip_prefix('*')
        .and_then(|line| line.strip_prefix(' ').or(Some(line)))
        .unwrap_or(line)
    })
    .collect::<Vec<_>>()
    .join("\n")
    .trim()
    .to_string();
  (!documentation.is_empty()).then_some(documentation)
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
        documentation: declaration.documentation.clone(),
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
      documentation: None,
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
            documentation: None,
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
          documentation: target.documentation.clone(),
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
  documentation: Option<String>,
  start: usize,
  end: usize,
  selection_start: usize,
  selection_end: usize,
}

type ExportMap = HashMap<String, Vec<DefinitionTarget>>;

fn resolve_external_definitions(
  program: &oxc_ast::ast::Program<'_>,
  path: &Path,
  sources: AnalysisSources<'_>,
  dependencies: &mut HashSet<PathBuf>,
  definitions: &mut Vec<AnalysisDefinition>,
  virtual_documents: &mut Vec<AnalysisVirtualDocument>,
) {
  let AnalysisSources {
    overlays,
    module_declarations,
    module_documentation,
    declaration_paths,
  } = sources;
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
  resolve_declaration_module_members(
    &root,
    module_declarations,
    module_documentation,
    declaration_paths,
    definitions,
    virtual_documents,
  );
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
  module_documentation: &HashMap<(String, String), String>,
  declaration_paths: &HashMap<String, PathBuf>,
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
    let Some(mut types) = declaration_types(declaration_source, &virtual_id) else {
      continue;
    };
    apply_module_path(specifier, &mut types, declaration_paths);
    apply_module_documentation(specifier, &mut types, module_documentation);
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

fn apply_module_path(
  specifier: &str,
  declarations: &mut DeclarationTypes,
  paths: &HashMap<String, PathBuf>,
) {
  let Some(path) = paths.get(specifier) else {
    return;
  };
  for declaration in declarations.values_mut() {
    declaration.target.path = Some(path.clone());
    declaration.target.virtual_id = None;
  }
}

fn apply_module_documentation(
  specifier: &str,
  declarations: &mut DeclarationTypes,
  documentation: &HashMap<(String, String), String>,
) {
  for (name, declaration) in declarations {
    declaration.target.documentation = documentation
      .get(&(specifier.to_string(), name.clone()))
      .cloned()
      .or_else(|| declaration.target.documentation.clone());
  }
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
    documentation: declaration.documentation.clone(),
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
    documentation: target.documentation.clone(),
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
  if let Ok(path) = crate::resolver::resolve_declaration_specifier(importer, specifier) {
    return Some(normalize_path(&path));
  }

  let base = importer
    .parent()
    .unwrap_or_else(|| Path::new("."));
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
          documentation: declaration.documentation.clone(),
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
              documentation: item.documentation.clone(),
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
mod tests;
