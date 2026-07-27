// SPDX-License-Identifier: MPL-2.0

use crate::args::ExecutionArgs;
use denko::compiler;
use denko::parser::Parser as JsParser;
use denko::test262_support;
use denko::value::Value;
use denko::vm::Vm;
use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use std::thread;
use std::time::{Duration, Instant};

pub(crate) fn run(
  execution: ExecutionArgs,
  node_compat: blitz_runtime::host::NodeCompatProfile,
) -> Result<(), denko::errors::Error> {
  let async_done_state: Rc<RefCell<Option<test262_support::AsyncCompletion>>> =
    Rc::new(RefCell::new(None));
  let globals = if execution.test262_harness {
    test262_globals(&async_done_state)
  } else {
    standard_globals()
  };

  let mut runtime_argv = vec!["blitz".to_string()];
  if let Some(path) = &execution.file {
    runtime_argv.push(path.to_string_lossy().into_owned());
  }
  runtime_argv.extend(execution.args.iter().cloned());
  let mut runtime_options = blitz_runtime::host::RuntimeOptions::from_host(runtime_argv);
  runtime_options.node_compat = node_compat;

  let Some(path) = execution.file else {
    return blitz_runtime::repl::run(runtime_options);
  };

  let mut vm = Vm::new(globals);
  let mut runtime = blitz_runtime::host::install_runtime(&mut vm, runtime_options);

  let source = std::fs::read_to_string(&path).map_err(|error| {
    denko::errors::RuntimeError::from(format!("Failed to read {}: {error}", path.display()))
  })?;
  if should_load_as_module(&path, &source, execution.module) {
    blitz_runtime::modules::evaluate_entry(&mut vm, &path)?;
  } else {
    let source_type = JsParser::source_type_for_path(&path, false);
    let session = JsParser::parse_source_with_type(&source, source_type)?;
    let chunk = compiler::compile_oxc(session.program(), &source)?;
    vm.run(chunk)?;

    if execution.test262_harness && vm.get_global("$DONE").is_some() {
      wait_for_test262_completion(&mut vm, &async_done_state)?;
    }
  }
  runtime.run(&mut vm)?;
  Ok(())
}

fn should_load_as_module(
  path: &Path,
  source: &str,
  explicit_module: bool,
) -> bool {
  if explicit_module || JsParser::source_type_for_path(path, false).is_module() {
    return true;
  }

  is_typescript_path(path)
    && JsParser::extract_imports_from_source_with_type(
      source,
      JsParser::source_type_for_path(path, true),
    )
    .is_ok_and(|imports| !imports.is_empty())
}

fn is_typescript_path(path: &Path) -> bool {
  path
    .extension()
    .and_then(|extension| extension.to_str())
    .is_some_and(|extension| matches!(extension, "ts" | "tsx" | "mts" | "cts"))
}

fn standard_globals() -> Rc<RefCell<denko::value::ObjectData>> {
  let globals = Rc::new(RefCell::new(denko::value::ObjectData::new()));
  let global_this = Value::Object(Rc::clone(&globals));
  globals.borrow_mut().properties.insert(
    denko::value::PropertyKey::String("globalThis".to_string().into()),
    denko::value::PropertyDescriptor::ordinary_data(global_this),
  );
  globals
}

fn test262_globals(
  async_done_state: &Rc<RefCell<Option<test262_support::AsyncCompletion>>>
) -> Rc<RefCell<denko::value::ObjectData>> {
  let done_state = Rc::clone(async_done_state);
  let print_native: denko::value::NativeFn = Rc::new(move |vm, _this, args| {
    let value = args.first().cloned().unwrap_or(Value::Undefined);
    let rendered = match vm.to_string(&value) {
      Ok(string) => string.to_string_lossy(),
      Err(_) => format!("{value}"),
    };
    if let Some(marker) = test262_support::async_completion_marker(&rendered) {
      *done_state.borrow_mut() = Some(marker);
      return Ok(Value::Null);
    }
    println!("{rendered}");
    Ok(Value::Null)
  });
  test262_support::make_test262_globals_with_print(print_native)
}

fn wait_for_test262_completion(
  vm: &mut Vm,
  async_done_state: &Rc<RefCell<Option<test262_support::AsyncCompletion>>>,
) -> Result<(), denko::errors::Error> {
  let start = Instant::now();
  let timeout = Duration::from_secs(30);
  loop {
    vm.flush_microtasks()?;

    if let Some(done) = async_done_state.borrow().clone() {
      match done {
        test262_support::AsyncCompletion::Complete => {
          println!("Test262:AsyncTestComplete");
          return Ok(());
        }
        test262_support::AsyncCompletion::Failure(message) => {
          return Err(
            denko::errors::RuntimeError::from(format!("async test failed: {message}")).into(),
          );
        }
      }
    }

    if start.elapsed() >= timeout {
      return Err(
        denko::errors::RuntimeError::from(format!(
          "async test timed out after {}s",
          timeout.as_secs()
        ))
        .into(),
      );
    }
    thread::sleep(Duration::from_millis(10));
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn executes_basic_program() {
    let mut vm = Vm::new(standard_globals());
    let source = denko::js!("var answer = 1 + 2; answer;");
    let session = JsParser::parse_source(source).expect("parse");
    let chunk = compiler::compile_oxc(session.program(), source).expect("compile");
    vm.run(chunk).expect("run");

    assert!(
      matches!(vm.get_global("answer"), Some(Value::Number(number)) if (number - 3.0).abs() < f64::EPSILON)
    );
  }

  #[test]
  fn typescript_annotations_are_erased_for_execution() {
    let source = denko::js!(
      typescript,
      r#"
      interface Answer { value: number }
      const answer: Answer = { value: 42 };
      var result: number = answer.value as number;
    "#
    );
    let session = JsParser::parse_source_with_type(
      source,
      JsParser::source_type_for_path(Path::new("answer.ts"), false),
    )
    .expect("parse TypeScript");
    let chunk = compiler::compile_oxc(session.program(), source).expect("compile TypeScript");
    let mut vm = Vm::new(standard_globals());

    vm.run(chunk).expect("run TypeScript");
    assert!(
      matches!(vm.get_global("result"), Some(Value::Number(number)) if (number - 42.0).abs() < f64::EPSILON)
    );
  }

  #[test]
  fn explicit_module_extensions_use_the_module_loader() {
    assert!(should_load_as_module(Path::new("entry.mjs"), "", false));
    assert!(should_load_as_module(Path::new("entry.mts"), "", false));
    assert!(!should_load_as_module(Path::new("entry.js"), "", false));
    assert!(!should_load_as_module(Path::new("entry.cjs"), "", false));
    assert!(should_load_as_module(Path::new("entry.js"), "", true));
  }

  #[test]
  fn typescript_imports_use_the_module_loader() {
    assert!(should_load_as_module(
      Path::new("entry.ts"),
      "import * as Ref from \"effect/Ref\"; Ref.make(0);",
      false,
    ));
    assert!(!should_load_as_module(
      Path::new("entry.ts"),
      "const answer: number = 42;",
      false,
    ));
  }
}
