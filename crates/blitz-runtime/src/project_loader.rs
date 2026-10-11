// SPDX-License-Identifier: MPL-2.0

use std::path::{Path, PathBuf};

use denko::parser::Parser as JsParser;

use crate::project::{PackageType, ProjectContext};

/// Evaluation semantics selected for an application entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModuleFormat {
  /// ECMAScript module linking and evaluation.
  Esm,
  /// Node-compatible CommonJS semantics, not implemented yet.
  CommonJs,
  /// JSON module semantics, not implemented yet.
  Json,
  /// A classic script entry evaluated in the global environment.
  ClassicScript,
}

/// Parser media type, kept separate from module semantics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MediaType {
  JavaScript,
  Jsx,
  TypeScript,
  Tsx,
  Json,
}

/// Canonical entry identity and its single format decision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedEntry {
  /// Canonical filesystem identity used by execution and `process.argv`.
  pub path: PathBuf,
  /// Evaluation semantics selected from extension, package scope, and source.
  pub format: ModuleFormat,
  /// Syntax/media interpretation independent of evaluation semantics.
  pub media_type: MediaType,
}

/// Runtime-owned resolver for application entry facts.
pub struct ProjectLoader;

impl ProjectLoader {
  /// Resolve and classify one filesystem entry without evaluating it.
  ///
  /// # Errors
  ///
  /// Returns an actionable error for missing entries and formats whose real
  /// evaluator is not implemented. Unsupported formats never fall through to
  /// classic-script or ESM parsing.
  pub fn resolve_entry(
    path: &Path,
    source: &str,
    force_module: bool,
  ) -> Result<ResolvedEntry, denko::errors::RuntimeError> {
    let path = std::fs::canonicalize(path).map_err(|error| {
      denko::errors::RuntimeError::from(format!(
        "BLITZ_ENTRY_NOT_FOUND: failed to resolve {}: {error}",
        path.display()
      ))
    })?;
    let project = ProjectContext::discover(path.parent().unwrap_or_else(|| Path::new(".")))?;
    let package_type = project
      .package
      .as_ref()
      .and_then(|package| package.package_type);
    let media_type = media_type(&path);
    let format = classify(&path, source, force_module, package_type, media_type);

    match format {
      ModuleFormat::CommonJs => Err(unsupported_format(
        "CommonJS",
        &path,
        "use an ESM entry or set package.json \"type\" to \"module\"",
      )),
      ModuleFormat::Json => Err(unsupported_format(
        "JSON modules",
        &path,
        "load JSON through node:fs until JSON module semantics are implemented",
      )),
      ModuleFormat::Esm | ModuleFormat::ClassicScript => Ok(ResolvedEntry {
        path,
        format,
        media_type,
      }),
    }
  }
}

fn classify(
  path: &Path,
  source: &str,
  force_module: bool,
  package_type: Option<PackageType>,
  media_type: MediaType,
) -> ModuleFormat {
  if force_module {
    return ModuleFormat::Esm;
  }
  let extension = path
    .extension()
    .and_then(|extension| extension.to_str())
    .unwrap_or_default()
    .to_ascii_lowercase();
  match extension.as_str() {
    "mjs" | "mts" => return ModuleFormat::Esm,
    "cjs" | "cts" => return ModuleFormat::CommonJs,
    "json" => return ModuleFormat::Json,
    _ => {}
  }
  match package_type {
    Some(PackageType::Module) => return ModuleFormat::Esm,
    Some(PackageType::Commonjs) => return ModuleFormat::CommonJs,
    Some(PackageType::Unknown) | None => {}
  }
  if matches!(media_type, MediaType::TypeScript | MediaType::Tsx)
    && JsParser::extract_imports_from_source_with_type(
      source,
      JsParser::source_type_for_path(path, true),
    )
    .is_ok_and(|imports| !imports.is_empty())
  {
    ModuleFormat::Esm
  } else {
    ModuleFormat::ClassicScript
  }
}

fn media_type(path: &Path) -> MediaType {
  match path
    .extension()
    .and_then(|extension| extension.to_str())
    .unwrap_or_default()
    .to_ascii_lowercase()
    .as_str()
  {
    "jsx" => MediaType::Jsx,
    "ts" | "mts" | "cts" => MediaType::TypeScript,
    "tsx" => MediaType::Tsx,
    "json" => MediaType::Json,
    _ => MediaType::JavaScript,
  }
}

fn unsupported_format(
  format: &str,
  path: &Path,
  hint: &str,
) -> denko::errors::RuntimeError {
  denko::errors::RuntimeError::from(format!(
    "BLITZ_MODULE_FORMAT_UNSUPPORTED: {format} execution is not available yet\n  entry: {}\n  hint: {hint}",
    path.display()
  ))
}

#[cfg(test)]
mod tests {
  use super::*;

  fn temporary_project(label: &str) -> PathBuf {
    let directory = std::env::temp_dir().join(format!(
      "blitz-project-loader-{label}-{}-{}",
      std::process::id(),
      std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos()
    ));
    std::fs::create_dir_all(&directory).expect("create project");
    directory
  }

  #[test]
  fn package_type_and_extensions_select_one_entry_format() {
    let root = temporary_project("formats");
    std::fs::write(root.join("package.json"), r#"{"type":"module"}"#).expect("manifest");
    for (name, expected) in [
      ("app.js", ModuleFormat::Esm),
      ("app.ts", ModuleFormat::Esm),
      ("app.mjs", ModuleFormat::Esm),
    ] {
      let path = root.join(name);
      std::fs::write(&path, "export const answer = 42;").expect("entry");
      assert_eq!(
        ProjectLoader::resolve_entry(&path, "export const answer = 42;", false)
          .expect("supported entry")
          .format,
        expected
      );
    }
    std::fs::remove_dir_all(root).expect("remove project");
  }

  #[test]
  fn unsupported_formats_fail_before_evaluation() {
    let root = temporary_project("unsupported");
    for (name, expected) in [("app.cjs", "CommonJS"), ("data.json", "JSON modules")] {
      let path = root.join(name);
      std::fs::write(&path, "{}").expect("entry");
      let error = ProjectLoader::resolve_entry(&path, "{}", false).expect_err("unsupported");
      assert!(
        error
          .message
          .contains("BLITZ_MODULE_FORMAT_UNSUPPORTED")
      );
      assert!(error.message.contains(expected));
    }
    std::fs::remove_dir_all(root).expect("remove project");
  }
}
