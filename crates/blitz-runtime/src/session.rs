// SPDX-License-Identifier: MPL-2.0

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use denko::Vm;
use denko::compiler;
use denko::parser::Parser as JsParser;
use denko::value::{ObjectData, PropertyDescriptor, PropertyKey, Value};
use oxc_ast::ast as oast;
use oxc_span::SourceType;

use crate::host::{
  Output, ProcessOutput, RuntimeHandle, RuntimeOptions, install_runtime_with_output,
};

/// A hosted VM and its asynchronous resources.
///
/// Keeping both values together prevents hosts from evaluating against one VM
/// and accidentally driving another runtime handle.
pub struct HostedSession {
  vm: Vm,
  runtime: RuntimeHandle,
}

/// Result of driving every referenced hosted resource to completion.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use]
pub struct SessionExit {
  /// Process status requested by the settled Effect root, or zero.
  pub code: i32,
}

impl HostedSession {
  /// Create a session using the process stdout and stderr streams.
  pub fn new(options: RuntimeOptions) -> Self {
    Self::with_output(options, Rc::new(ProcessOutput))
  }

  /// Create a session whose console output is handled by an embedder adapter.
  pub fn with_output(
    options: RuntimeOptions,
    output: Rc<dyn Output>,
  ) -> Self {
    Self::with_globals_and_output(standard_globals(), options, output)
  }

  /// Create a hosted session around caller-supplied globals.
  ///
  /// This exists for the Test262 host adapter. Application embedders should
  /// use [`Self::new`] so the session owns global initialization as well.
  pub fn with_globals(
    globals: Rc<RefCell<ObjectData>>,
    options: RuntimeOptions,
  ) -> Self {
    Self::with_globals_and_output(globals, options, Rc::new(ProcessOutput))
  }

  fn with_globals_and_output(
    globals: Rc<RefCell<ObjectData>>,
    options: RuntimeOptions,
    output: Rc<dyn Output>,
  ) -> Self {
    let mut vm = Vm::new(globals);
    let runtime = install_runtime_with_output(&mut vm, options, output);
    Self { vm, runtime }
  }

  /// Evaluate modules before the entry point or first interactive submission.
  pub fn preload(
    &mut self,
    paths: &[PathBuf],
  ) -> Result<(), denko::errors::Error> {
    crate::modules::evaluate_preloads(&mut self.vm, paths)
  }

  /// Evaluate a source string and return its completion value.
  pub fn evaluate_script(
    &mut self,
    source: &str,
    path: &Path,
  ) -> Result<Value, denko::errors::Error> {
    let source_type = JsParser::source_type_for_path(path, false);
    let session = JsParser::parse_source_with_type(source, source_type)?;
    let chunk = compiler::compile_session(&session)?;
    self.vm.run(chunk).map_err(Into::into)
  }

  /// Evaluate a module entry in this session's shared module registry.
  pub fn evaluate_module(
    &mut self,
    path: &Path,
  ) -> Result<(), denko::errors::Error> {
    crate::modules::evaluate_entry(&mut self.vm, path)
  }

  /// Evaluate one REPL submission while retaining successful declarations.
  pub fn evaluate_repl(
    &mut self,
    source: &str,
  ) -> Result<Value, denko::errors::Error> {
    let source_type = SourceType::ts().with_module(true);
    let session = JsParser::parse_source_with_type(source, source_type)?;
    let lowered = lower_static_imports(session.program(), source);
    let session = JsParser::parse_source_with_type(&lowered, source_type)?;
    let chunk = compiler::compile_repl_session(&session)?;
    self.vm.run_repl(chunk).map_err(Into::into)
  }

  /// Process at most one ready runtime event, waiting up to `timeout`.
  pub fn poll(
    &mut self,
    timeout: Duration,
  ) -> Result<bool, denko::errors::RuntimeError> {
    self
      .runtime
      .run_once_timeout(&mut self.vm, timeout)
  }

  /// Drive referenced resources to completion and return the hosted exit code.
  pub fn run(&mut self) -> Result<SessionExit, denko::errors::RuntimeError> {
    self.runtime.run(&mut self.vm)?;
    Ok(SessionExit {
      code: self.runtime.exit_status().unwrap_or(0),
    })
  }

  /// Return the status recorded by a settled Effect root without driving it.
  pub fn exit_status(&self) -> SessionExit {
    SessionExit {
      code: self.runtime.exit_status().unwrap_or(0),
    }
  }

  /// Access the VM for specialized host adapters such as Test262.
  pub fn vm_mut(&mut self) -> &mut Vm {
    &mut self.vm
  }
}

fn standard_globals() -> Rc<RefCell<ObjectData>> {
  let globals = Rc::new(RefCell::new(ObjectData::new()));
  globals.borrow_mut().properties.insert(
    PropertyKey::String("globalThis".to_string().into()),
    PropertyDescriptor::ordinary_data(Value::Object(Rc::clone(&globals))),
  );
  globals
}

fn lower_static_imports(
  program: &oast::Program<'_>,
  source: &str,
) -> String {
  let mut replacements = Vec::new();
  for statement in &program.body {
    let oast::Statement::ImportDeclaration(import) = statement else {
      continue;
    };
    replacements.push((
      import.span.start as usize,
      import.span.end as usize,
      lower_static_import(import),
    ));
  }
  let mut lowered = source.to_string();
  for (start, end, replacement) in replacements.into_iter().rev() {
    lowered.replace_range(start..end, &replacement);
  }
  lowered
}

fn lower_static_import(import: &oast::ImportDeclaration<'_>) -> String {
  if import.import_kind == oast::ImportOrExportKind::Type {
    return String::new();
  }
  let specifier = serde_json::to_string(import.source.value.as_str())
    .expect("module specifiers are serializable strings");
  let Some(imports) = import.specifiers.as_ref() else {
    return format!("await import({specifier});");
  };
  let mut names = Vec::new();
  let mut values = Vec::new();
  for import in imports {
    match import {
      oast::ImportDeclarationSpecifier::ImportNamespaceSpecifier(namespace) => {
        names.push(namespace.local.name.to_string());
        values.push("module".to_string());
      }
      oast::ImportDeclarationSpecifier::ImportDefaultSpecifier(default) => {
        names.push(default.local.name.to_string());
        values.push("module[\"default\"]".to_string());
      }
      oast::ImportDeclarationSpecifier::ImportSpecifier(named)
        if named.import_kind == oast::ImportOrExportKind::Value =>
      {
        let imported = match &named.imported {
          oast::ModuleExportName::IdentifierName(name) => name.name.as_str(),
          oast::ModuleExportName::IdentifierReference(name) => name.name.as_str(),
          oast::ModuleExportName::StringLiteral(name) => name.value.as_str(),
        };
        let imported = serde_json::to_string(imported).expect("import names are serializable");
        names.push(named.local.name.to_string());
        values.push(format!("module[{imported}]"));
      }
      oast::ImportDeclarationSpecifier::ImportSpecifier(_) => {}
    }
  }
  if names.is_empty() {
    return if imports.is_empty() {
      format!("await import({specifier});")
    } else {
      String::new()
    };
  }
  format!(
    "const [{}] = ((module) => [{}])(await import({specifier}));",
    names.join(", "),
    values.join(", ")
  )
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::host::OutputStream;

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

  fn options() -> RuntimeOptions {
    RuntimeOptions::from_host(vec!["blitz-session-test".to_string()])
  }

  #[test]
  fn routes_console_output_through_the_embedder_adapter() {
    let output = Rc::new(CapturedOutput::default());
    let mut session = HostedSession::with_output(options(), output.clone());
    session
      .evaluate_repl("console.log('answer', 42)")
      .expect("evaluate console call");
    assert_eq!(
      output.0.borrow().as_slice(),
      &[(OutputStream::Stdout, "answer 42".to_string())]
    );
  }

  #[test]
  fn console_string_substitution_propagates_coercion_failures() {
    let output = Rc::new(CapturedOutput::default());
    let mut session = HostedSession::with_output(options(), output.clone());
    let result = session
      .evaluate_repl("console.log('%s', { toString() { throw new Error('coercion failed') } })");
    assert!(result.is_err());
    assert!(output.0.borrow().is_empty());
  }

  #[test]
  fn repl_submissions_share_bindings() {
    let mut session = HostedSession::new(options());
    session
      .evaluate_repl("const answer = 41")
      .expect("declare binding");
    assert_eq!(
      session
        .evaluate_repl("answer + 1")
        .expect("read binding"),
      Value::Number(42.0)
    );
  }

  #[test]
  fn run_returns_the_effect_root_exit_status() {
    let mut session = HostedSession::new(options());
    session
      .evaluate_repl(denko::js!(
        r#"
          import * as Effect from "effect/Effect";
          import * as Runtime from "blitz:runtime";
          Runtime.runMain(Effect.fail("expected"), { reportFailure: () => {} });
        "#
      ))
      .expect("start failed Effect root");
    assert_eq!(session.run().expect("drive root"), SessionExit { code: 1 });
  }
}
