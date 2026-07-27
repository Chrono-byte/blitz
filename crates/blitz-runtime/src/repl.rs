// SPDX-License-Identifier: MPL-2.0
//! Interactive shell for the hosted Blitz runtime.

use std::cell::RefCell;
use std::env;
use std::rc::Rc;

use denko::value::{ObjectData, PropertyDescriptor, PropertyKey};
use denko::{compiler, parser::Parser as JsParser, value::Value, vm::Vm};

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
pub fn run(options: RuntimeOptions) -> Result<(), denko::errors::Error> {
  let globals = Rc::new(RefCell::new(ObjectData::new()));
  globals.borrow_mut().properties.insert(
    PropertyKey::String("globalThis".to_string().into()),
    PropertyDescriptor::ordinary_data(Value::Object(Rc::clone(&globals))),
  );
  let mut vm = Vm::new(globals);
  let _runtime = install_runtime(&mut vm, options);

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
  let session = JsParser::parse_source(source)?;
  let chunk = compiler::compile_oxc_repl(session.program(), source)?;
  vm.run(chunk).map_err(denko::errors::Error::from)
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
