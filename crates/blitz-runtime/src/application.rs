// SPDX-License-Identifier: MPL-2.0
//! Complete hosted application execution behind one operation.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use denko::value::{ObjectData, Value};

use crate::host::{Output, RuntimeOptions};
use crate::project_loader::{ModuleFormat, ProjectLoader};
use crate::session::{HostedSession, SessionExit};

/// User-selected application input.
pub enum ApplicationEntry {
  /// A JavaScript or TypeScript filesystem entry.
  File {
    /// Entry path, resolved before evaluation.
    path: PathBuf,
    /// Force ECMAScript-module interpretation for migration compatibility.
    force_module: bool,
  },
  /// Inline JavaScript or TypeScript source.
  Eval {
    /// Source text evaluated in the persistent hosted realm.
    source: String,
  },
}

/// Normalized complete-program request shared by the CLI and embedders.
pub struct ApplicationRequest {
  /// Program input.
  pub entry: ApplicationEntry,
  /// Modules evaluated in order before the application entry.
  pub preloads: Vec<PathBuf>,
}

/// Observable result of a completed hosted application.
#[derive(Clone, Debug, PartialEq)]
pub struct ApplicationOutcome {
  /// Process status after all referenced runtime resources settle.
  pub exit: SessionExit,
  /// Completion value for inline/classic-script evaluation.
  pub completion: Option<Value>,
}

/// Owns VM construction, entry classification, evaluation, and runtime drain.
pub struct HostedApplication {
  session: HostedSession,
}

impl HostedApplication {
  /// Create an application using process stdout and stderr.
  #[must_use]
  pub fn new(options: RuntimeOptions) -> Self {
    Self {
      session: HostedSession::new(options),
    }
  }

  /// Create an application with an embedder-provided output adapter.
  #[must_use]
  pub fn with_output(
    options: RuntimeOptions,
    output: Rc<dyn Output>,
  ) -> Self {
    Self {
      session: HostedSession::with_output(options, output),
    }
  }

  /// Create an application with specialized globals such as the Test262 host.
  #[must_use]
  pub fn with_globals(
    globals: Rc<RefCell<ObjectData>>,
    options: RuntimeOptions,
  ) -> Self {
    Self {
      session: HostedSession::with_globals(globals, options),
    }
  }

  /// Execute one application and drive its referenced resources to completion.
  ///
  /// # Errors
  ///
  /// Returns project, file, parser, compiler, module, or runtime failures. Entry
  /// formats without a correct evaluator fail during preparation, before user
  /// source is parsed under the wrong semantics.
  pub fn run(
    mut self,
    request: ApplicationRequest,
  ) -> Result<ApplicationOutcome, denko::errors::Error> {
    self.session.preload(&request.preloads)?;
    let completion = match request.entry {
      ApplicationEntry::File { path, force_module } => {
        let source = std::fs::read_to_string(&path).map_err(|error| {
          denko::errors::RuntimeError::from(format!("Failed to read {}: {error}", path.display()))
        })?;
        let entry = ProjectLoader::resolve_entry(&path, &source, force_module)?;
        match entry.format {
          ModuleFormat::Esm => {
            self.session.evaluate_module(&entry.path)?;
            None
          }
          ModuleFormat::ClassicScript => Some(
            self
              .session
              .evaluate_script(&source, &entry.path)?,
          ),
          ModuleFormat::CommonJs | ModuleFormat::Json => {
            return Err(
              denko::errors::RuntimeError::from(
                "BLITZ_INTERNAL_ERROR: project loader returned an unsupported entry format",
              )
              .into(),
            );
          }
        }
      }
      ApplicationEntry::Eval { source } => Some(self.session.evaluate_repl(&source)?),
    };
    Ok(ApplicationOutcome {
      exit: self.session.run()?,
      completion,
    })
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::host::{NodeCompatProfile, OutputStream, RuntimeOptions};

  #[derive(Default)]
  struct CapturedOutput(RefCell<Vec<(OutputStream, String)>>);

  impl Output for CapturedOutput {
    fn write_line(
      &self,
      stream: OutputStream,
      line: &str,
    ) -> Result<(), denko::errors::RuntimeError> {
      self
        .0
        .borrow_mut()
        .push((stream, line.to_string()));
      Ok(())
    }
  }

  #[test]
  fn runs_a_package_scoped_esm_entry_through_one_application_operation() {
    let root = std::env::temp_dir().join(format!(
      "blitz-hosted-application-{}-{}",
      std::process::id(),
      std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos()
    ));
    std::fs::create_dir_all(&root).expect("project");
    std::fs::write(root.join("package.json"), r#"{"type":"module"}"#).expect("manifest");
    let entry = root.join("main.js");
    std::fs::write(
      &entry,
      denko::js!(module, "export const answer = 42; console.log(answer);"),
    )
    .expect("entry");
    let mut options = RuntimeOptions::from_host(vec!["blitz".to_string()]);
    options.node_compat = NodeCompatProfile::Off;

    let output = Rc::new(CapturedOutput::default());
    let outcome = HostedApplication::with_output(options, output.clone())
      .run(ApplicationRequest {
        entry: ApplicationEntry::File {
          path: entry,
          force_module: false,
        },
        preloads: Vec::new(),
      })
      .expect("run application");

    assert_eq!(outcome.exit.code, 0);
    assert_eq!(outcome.completion, None);
    assert_eq!(
      *output.0.borrow(),
      [(OutputStream::Stdout, "42".to_string())]
    );
    std::fs::remove_dir_all(root).expect("remove project");
  }
}
