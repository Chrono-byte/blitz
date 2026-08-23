// SPDX-License-Identifier: MPL-2.0

use crate::args::AnalyzeArgs;
use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};

const ANALYSIS_PROTOCOL_VERSION: u32 = 1;

#[derive(serde::Deserialize)]
#[serde(tag = "method", rename_all = "snake_case")]
enum ServerRequest {
  Analyze {
    protocol_version: u32,
    id: u64,
    path: PathBuf,
    source: String,
    #[serde(default)]
    strict: bool,
  },
  Update {
    protocol_version: u32,
    path: PathBuf,
    source: String,
  },
  Close {
    protocol_version: u32,
    path: PathBuf,
  },
}

impl ServerRequest {
  fn protocol_version(&self) -> u32 {
    match self {
      Self::Analyze {
        protocol_version, ..
      }
      | Self::Update {
        protocol_version, ..
      }
      | Self::Close {
        protocol_version, ..
      } => *protocol_version,
    }
  }
}

#[derive(serde::Serialize)]
struct AnalysisResponse<'a> {
  protocol_version: u32,
  id: u64,
  result: &'a denko::analysis::Analysis,
}

fn analysis_response(
  id: u64,
  result: &denko::analysis::Analysis,
) -> Result<serde_json::Value, serde_json::Error> {
  serde_json::to_value(AnalysisResponse {
    protocol_version: ANALYSIS_PROTOCOL_VERSION,
    id,
    result,
  })
}

fn error_response(
  id: Option<u64>,
  error: impl Into<String>,
) -> serde_json::Value {
  serde_json::json!({
    "protocol_version": ANALYSIS_PROTOCOL_VERSION,
    "id": id,
    "error": error.into(),
  })
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
      Ok(request) if request.protocol_version() != ANALYSIS_PROTOCOL_VERSION => {
        Some(error_response(
          None,
          format!(
            "Unsupported analysis protocol version {}; expected {}",
            request.protocol_version(),
            ANALYSIS_PROTOCOL_VERSION
          ),
        ))
      }
      Ok(ServerRequest::Analyze {
        protocol_version: _,
        id,
        path,
        source,
        strict: request_strict,
      }) => {
        let result = project.analyze_document(source, &path, strict || request_strict);
        Some(analysis_response(id, &result).map_err(|error| {
          denko::errors::RuntimeError::from(format!("Failed to encode analysis: {error}"))
        })?)
      }
      Ok(ServerRequest::Update {
        protocol_version: _,
        path,
        source,
      }) => {
        project.update_document(source, &path);
        None
      }
      Ok(ServerRequest::Close {
        protocol_version: _,
        path,
      }) => {
        project.close_document(&path);
        None
      }
      Err(error) => Some(error_response(None, error.to_string())),
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

#[cfg(test)]
mod tests {
  use super::*;
  use denko::analysis::{
    Analysis, AnalysisDeclaration, AnalysisDefinition, AnalysisDiagnostic, AnalysisInlayHint,
    AnalysisRuntimeGlobal, AnalysisVirtualDocument,
  };

  #[test]
  fn response_shape_matches_cross_language_fixture() {
    let result = Analysis {
      diagnostics: vec![AnalysisDiagnostic {
        message: "example diagnostic".to_string(),
        start: 1,
        end: 2,
      }],
      declarations: vec![AnalysisDeclaration {
        name: "answer".to_string(),
        kind: "variable",
        detail: "const answer: number".to_string(),
        documentation: Some("The answer.".to_string()),
        start: 0,
        end: 18,
        selection_start: 6,
        selection_end: 12,
        children: Vec::new(),
      }],
      definitions: vec![AnalysisDefinition {
        start: 20,
        end: 26,
        target_path: Some("/project/source.ts".to_string()),
        target_virtual: Some("lib.es2023.d.ts".to_string()),
        detail: Some("const answer: number".to_string()),
        documentation: Some("The answer.".to_string()),
        target_start: 0,
        target_end: 18,
        target_selection_start: 6,
        target_selection_end: 12,
      }],
      keywords: vec!["const"],
      runtime_globals: vec![AnalysisRuntimeGlobal {
        name: "console",
        detail: "Console output",
      }],
      virtual_documents: vec![AnalysisVirtualDocument {
        id: "lib.es2023.d.ts".to_string(),
        text: "declare const answer: number;".to_string(),
      }],
      inlay_hints: vec![AnalysisInlayHint {
        position: 12,
        label: ": number".to_string(),
        kind: "type",
      }],
    };
    let actual = analysis_response(7, &result).expect("serialize fixture response");
    let expected: serde_json::Value = serde_json::from_str(include_str!(
      "../../../language-server/protocol/analysis-v1.fixture.json"
    ))
    .expect("parse checked protocol fixture");
    assert_eq!(actual, expected);
  }
}
