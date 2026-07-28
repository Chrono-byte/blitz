// SPDX-License-Identifier: MPL-2.0
//! Interactive shell for the hosted Blitz runtime.

use std::cell::RefCell;
use std::env;
use std::path::PathBuf;
use std::rc::Rc;

use denko::value::{ObjectData, PropertyDescriptor, PropertyKey};
use denko::{compiler, parser::Parser as JsParser, value::Value, vm::Vm};
use oxc_ast::ast as oast;
use oxc_span::SourceType;

use crate::host::{RuntimeOptions, install_runtime};

/// Start an interactive Blitz runtime session.
///
/// The session installs the full hosted runtime before accepting input, so
/// globals such as `console` and `process` behave like they do during normal
/// Blitz program execution.
///
/// # Errors
///
/// Returns an error if the line editor cannot be initialized.
pub fn run(
  options: RuntimeOptions,
  preloads: &[PathBuf],
) -> Result<(), denko::errors::Error> {
  let globals = Rc::new(RefCell::new(ObjectData::new()));
  globals.borrow_mut().properties.insert(
    PropertyKey::String("globalThis".to_string().into()),
    PropertyDescriptor::ordinary_data(Value::Object(Rc::clone(&globals))),
  );
  let mut vm = Vm::new(globals);
  let _runtime = install_runtime(&mut vm, options);
  crate::modules::evaluate_preloads(&mut vm, preloads)?;

  run_loop(&mut vm)
}

fn run_loop(vm: &mut Vm) -> Result<(), denko::errors::Error> {
  println!("Blitz REPL. Type exit to quit.");
  let mut editor = rustyline::Editor::<(), rustyline::history::DefaultHistory>::new()
    .map_err(|error| denko::errors::RuntimeError::from(format!("Readline init error: {error}")))?;
  let history_path = env::temp_dir().join("blitz_repl_history");
  drop(editor.load_history(&history_path));
  loop {
    match editor.readline("> ") {
      Ok(input) => {
        let line = input.trim();
        if line.is_empty() {
          continue;
        }
        drop(editor.add_history_entry(line));
        drop(editor.save_history(&history_path));
        if line.eq_ignore_ascii_case("exit") || line.eq_ignore_ascii_case("quit") {
          break;
        }
        match evaluate(vm, line) {
          Ok(value) if !matches!(value, Value::Null | Value::Undefined) => {
            println!("{}", denko::inspect::inspect(&value));
          }
          Ok(_) => {}
          Err(error) => eprintln!("Error: {error}"),
        }
      }
      Err(rustyline::error::ReadlineError::Interrupted | rustyline::error::ReadlineError::Eof) => {
        break;
      }
      Err(error) => {
        eprintln!("Readline error: {error}");
        break;
      }
    }
  }
  Ok(())
}

fn evaluate(
  vm: &mut Vm,
  source: &str,
) -> Result<Value, denko::errors::Error> {
  let source_type = SourceType::ts().with_module(true);
  let session = JsParser::parse_source_with_type(source, source_type)?;
  let lowered = lower_static_imports(session.program(), source);
  let session = JsParser::parse_source_with_type(&lowered, source_type)?;
  let chunk = compiler::compile_oxc_repl(session.program(), &lowered)?;
  vm.run_repl(chunk)
    .map_err(denko::errors::Error::from)
}

// REPL submissions execute as persistent root chunks rather than module
// records. Lower import declarations through the realm's normal dynamic-import
// loader so package resolution, profile filtering, and module caching remain
// shared with hosted execution.
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

  if replacements.is_empty() {
    return source.to_string();
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
  if imports.is_empty() {
    return format!("await import({specifier});");
  }

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
        let imported =
          serde_json::to_string(imported).expect("import names are serializable strings");
        names.push(named.local.name.to_string());
        values.push(format!("module[{imported}]"));
      }
      oast::ImportDeclarationSpecifier::ImportSpecifier(_) => {}
    }
  }

  if names.is_empty() {
    return String::new();
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

  fn runtime_vm() -> (Vm, crate::host::RuntimeHandle) {
    let globals = Rc::new(RefCell::new(ObjectData::new()));
    globals.borrow_mut().properties.insert(
      PropertyKey::String("globalThis".to_string().into()),
      PropertyDescriptor::ordinary_data(Value::Object(Rc::clone(&globals))),
    );
    let mut vm = Vm::new(globals);
    let runtime = install_runtime(
      &mut vm,
      RuntimeOptions::from_host(vec!["blitz".to_string()]),
    );
    (vm, runtime)
  }

  #[test]
  fn source_returns_expression_result() {
    let (mut vm, _runtime) = runtime_vm();
    let result = evaluate(&mut vm, denko::js!("1 + 2")).expect("run");
    assert!(
      matches!(result, Value::Number(number) if (number - 3.0).abs() < f64::EPSILON),
      "expected 3, got {result:?}"
    );
  }

  #[test]
  fn session_persists_bindings_between_lines() {
    let (mut vm, _runtime) = runtime_vm();
    evaluate(&mut vm, denko::js!("let answer = 41")).expect("declare binding");
    evaluate(&mut vm, denko::js!("answer += 1")).expect("mutate binding");
    evaluate(
      &mut vm,
      denko::js!("const state = { answer }; function readAnswer() { return state.answer; }"),
    )
    .expect("declare object and closure");
    let result =
      evaluate(&mut vm, denko::js!("readAnswer()")).expect("read bindings from a later submission");

    assert_eq!(result, Value::Number(42.0));
  }

  #[test]
  fn session_accepts_typescript_and_top_level_await() {
    let (mut vm, _runtime) = runtime_vm();
    evaluate(
      &mut vm,
      denko::js!(
        typescript,
        "const pending: Promise<number> = Promise.resolve(42)"
      ),
    )
    .expect("declare typed promise");
    let result = evaluate(&mut vm, denko::js!(module, "await pending"))
      .expect("await a value from an earlier submission");

    assert_eq!(result, Value::Number(42.0));
  }

  #[test]
  fn session_accepts_static_imports() {
    let (mut vm, _runtime) = runtime_vm();
    evaluate(
      &mut vm,
      denko::js!(module, "import * as Effect from 'effect/Effect'"),
    )
    .expect("import namespace");
    evaluate(
      &mut vm,
      denko::js!(
        module,
        "import { succeed as succeedEffect } from 'effect/Effect'"
      ),
    )
    .expect("import named binding");
    let result = evaluate(
      &mut vm,
      denko::js!("typeof Effect.runSync === 'function' && succeedEffect(42)._tag === 'Success'"),
    )
    .expect("use imported bindings later");

    assert_eq!(result, Value::Bool(true));
  }

  #[test]
  fn session_includes_blitz_runtime_globals() {
    let (mut vm, _runtime) = runtime_vm();
    let result = evaluate(
      &mut vm,
      denko::js!("typeof console.log === 'function' && typeof process === 'object'"),
    )
    .expect("evaluate runtime globals");

    assert_eq!(result, Value::Bool(true));
  }
}
