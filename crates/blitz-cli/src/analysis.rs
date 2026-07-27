// SPDX-License-Identifier: MPL-2.0

use crate::args::AnalyzeArgs;
use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};

#[derive(serde::Deserialize)]
#[serde(tag = "method", rename_all = "snake_case")]
enum ServerRequest {
  Analyze {
    id: u64,
    path: PathBuf,
    source: String,
    #[serde(default)]
    strict: bool,
  },
  Update {
    path: PathBuf,
    source: String,
  },
  Close {
    path: PathBuf,
  },
}

pub(crate) fn run(args: AnalyzeArgs) -> Result<(), denko::errors::Error> {
  if args.server {
    run_server(args.strict)?;
    return Ok(());
  }

  let file = args.file.as_deref().ok_or_else(|| {
    denko::errors::RuntimeError::from("Analyze requires a file unless --server is used")
  })?;
  let source = if file == Path::new("-") {
    let mut source = String::new();
    std::io::stdin()
      .read_to_string(&mut source)
      .map_err(|error| {
        denko::errors::RuntimeError::from(format!("Failed to read standard input: {error}"))
      })?;
    source
  } else {
    std::fs::read_to_string(file).map_err(|error| {
      denko::errors::RuntimeError::from(format!("Failed to read {}: {error}", file.display()))
    })?
  };
  let path = args.path.as_deref().unwrap_or(file);
  let result = denko::analysis::analyze(&source, path, args.strict);
  println!(
    "{}",
    serde_json::to_string(&result).map_err(|error| {
      denko::errors::RuntimeError::from(format!("Failed to encode analysis: {error}"))
    })?
  );
  Ok(())
}

fn run_server(strict: bool) -> Result<(), denko::errors::RuntimeError> {
  let stdin = std::io::stdin();
  let mut stdout = std::io::stdout().lock();
  let mut project = denko::analysis::AnalysisProject::with_module_declarations(
    blitz_runtime::effect_assets::public_declaration_sources(),
  )
  .with_module_documentation(blitz_runtime::effect_assets::public_documentation());
  for line in stdin.lock().lines() {
    let line = line.map_err(|error| {
      denko::errors::RuntimeError::from(format!("Failed to read analysis request: {error}"))
    })?;
    if line.trim().is_empty() {
      continue;
    }
    let response = match serde_json::from_str::<ServerRequest>(&line) {
      Ok(ServerRequest::Analyze {
        id,
        path,
        source,
        strict: request_strict,
      }) => {
        let result = project.analyze_document(source, &path, strict || request_strict);
        Some(serde_json::json!({ "id": id, "result": result }))
      }
      Ok(ServerRequest::Update { path, source }) => {
        project.update_document(source, &path);
        None
      }
      Ok(ServerRequest::Close { path }) => {
        project.close_document(&path);
        None
      }
      Err(error) => Some(serde_json::json!({ "id": null, "error": error.to_string() })),
    };
    if let Some(response) = response {
      serde_json::to_writer(&mut stdout, &response).map_err(|error| {
        denko::errors::RuntimeError::from(format!("Failed to encode analysis response: {error}"))
      })?;
      stdout.write_all(b"\n").map_err(|error| {
        denko::errors::RuntimeError::from(format!("Failed to write analysis response: {error}"))
      })?;
      stdout.flush().map_err(|error| {
        denko::errors::RuntimeError::from(format!("Failed to flush analysis response: {error}"))
      })?;
    }
  }
  Ok(())
}
