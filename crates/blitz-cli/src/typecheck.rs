// SPDX-License-Identifier: MPL-2.0

use denko::parser::Parser as JsParser;
use std::path::{Path, PathBuf};

pub(crate) fn paths(
  paths: &[PathBuf],
  project: Option<&Path>,
) -> Result<Vec<PathBuf>, denko::errors::Error> {
  if project.is_some() && !paths.is_empty() {
    return Err(
      denko::errors::RuntimeError::from("--project cannot be combined with source files").into(),
    );
  }
  if !paths.is_empty() {
    return Ok(paths.to_vec());
  }

  let requested = project.unwrap_or_else(|| Path::new("."));
  let root = if requested.is_file() {
    requested
      .parent()
      .unwrap_or_else(|| Path::new("."))
  } else {
    requested
  };
  let mut files = Vec::new();
  collect_files(root, &mut files)?;
  files.sort();
  Ok(files)
}

pub(crate) fn run(
  paths: &[PathBuf],
  strict: bool,
) -> Result<bool, denko::errors::Error> {
  let mut success = true;
  for path in paths {
    let source = std::fs::read_to_string(path).map_err(|error| {
      denko::errors::RuntimeError::from(format!("Failed to read {}: {error}", path.display()))
    })?;
    let source_type = JsParser::source_type_for_path(path, false);
    match JsParser::parse_source_with_type(&source, source_type) {
      Ok(session) => {
        let result = denko::typecheck::with_strict_override(strict.then_some(true), || {
          denko::typecheck::check(session.program())
        });
        if let Err(error) = result {
          eprintln!("{}: {error}", path.display());
          success = false;
        }
      }
      Err(error) => {
        eprintln!("{}: {error}", path.display());
        success = false;
      }
    }
  }
  Ok(success)
}

fn is_typescript_path(path: &Path) -> bool {
  path
    .extension()
    .and_then(|extension| extension.to_str())
    .is_some_and(|extension| matches!(extension, "ts" | "tsx" | "mts" | "cts"))
}

fn collect_files(
  directory: &Path,
  files: &mut Vec<PathBuf>,
) -> Result<(), denko::errors::Error> {
  let entries = std::fs::read_dir(directory).map_err(|error| {
    denko::errors::RuntimeError::from(format!(
      "Failed to read project directory {}: {error}",
      directory.display()
    ))
  })?;
  for entry in entries {
    let entry = entry.map_err(|error| denko::errors::RuntimeError::from(error.to_string()))?;
    let path = entry.path();
    let file_type = entry
      .file_type()
      .map_err(|error| denko::errors::RuntimeError::from(error.to_string()))?;
    if file_type.is_symlink() {
      continue;
    }
    if file_type.is_dir() {
      let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
      if !matches!(name, ".git" | "node_modules" | "target") {
        collect_files(&path, files)?;
      }
    } else if is_typescript_path(&path) {
      let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
      if ![".d.ts", ".d.mts", ".d.cts"]
        .iter()
        .any(|suffix| name.ends_with(suffix))
      {
        files.push(path);
      }
    }
  }
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::env;
  use std::sync::atomic::{AtomicUsize, Ordering};

  static TEMP_DIRECTORY_ID: AtomicUsize = AtomicUsize::new(0);

  fn temporary_directory() -> PathBuf {
    env::temp_dir().join(format!(
      "blitz-cli-test-{}-{}",
      std::process::id(),
      TEMP_DIRECTORY_ID.fetch_add(1, Ordering::Relaxed)
    ))
  }

  #[test]
  fn keeps_explicit_paths() {
    let requested = vec![PathBuf::from("one.ts"), PathBuf::from("two.ts")];
    assert_eq!(paths(&requested, None).expect("paths"), requested);
  }

  #[test]
  fn rejects_project_with_source_files() {
    let result = paths(
      &[PathBuf::from("program.ts")],
      Some(Path::new("tsconfig.json")),
    );
    assert!(result.is_err());
  }

  #[test]
  fn discovers_project_typescript_sources() {
    let root = temporary_directory();
    let source_directory = root.join("src").join("nested");
    std::fs::create_dir_all(&source_directory).expect("create source directory");
    std::fs::create_dir_all(root.join("node_modules")).expect("create dependency directory");
    let source = source_directory.join("program.ts");
    std::fs::write(&source, denko::js!(typescript, "let value: number = 1;"))
      .expect("write source");
    std::fs::write(
      source_directory.join("types.d.ts"),
      denko::js!(typescript, "declare const value: number;"),
    )
    .expect("write declaration");
    std::fs::write(root.join("node_modules").join("ignored.ts"), "").expect("write ignored file");

    assert_eq!(paths(&[], Some(&root)).expect("paths"), [source]);
    std::fs::remove_dir_all(root).expect("remove test directory");
  }

  #[test]
  fn checker_reports_file_errors() {
    let root = temporary_directory();
    std::fs::create_dir_all(&root).expect("create test directory");
    let valid = root.join("valid.ts");
    let invalid = root.join("invalid.ts");
    std::fs::write(&valid, denko::js!(typescript, "const answer: number = 42;"))
      .expect("write valid source");
    std::fs::write(
      &invalid,
      denko::js!(typescript, "const answer: number = 'wrong';"),
    )
    .expect("write invalid source");

    assert!(run(&[valid], false).expect("check valid source"));
    assert!(!run(&[invalid], false).expect("check invalid source"));
    std::fs::remove_dir_all(root).expect("remove test directory");
  }
}
