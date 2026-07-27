// SPDX-License-Identifier: MPL-2.0

use blitz::{compiler, parser::Parser as JsParser, value::Value, vm::Vm};
use std::env;

pub(crate) fn run(vm: &mut Vm) -> Result<(), blitz::errors::Error> {
  println!("Blitz REPL. Type exit to quit.");
  let mut editor = rustyline::Editor::<(), rustyline::history::DefaultHistory>::new()
    .map_err(|error| blitz::errors::RuntimeError::from(format!("Readline init error: {error}")))?;
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
        match run_source(vm, line) {
          Ok(value) if !matches!(value, Value::Null | Value::Undefined) => {
            println!("{}", blitz::inspect::inspect(&value));
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

fn run_source(
  vm: &mut Vm,
  source: &str,
) -> Result<Value, blitz::errors::Error> {
  let session = JsParser::parse_source(source)?;
  let chunk = compiler::compile_oxc_repl(session.program(), source)?;
  vm.run(chunk).map_err(blitz::errors::Error::from)
}

#[cfg(test)]
mod tests {
  use super::*;
  use blitz::value::{ObjectData, PropertyDescriptor, PropertyKey};
  use std::cell::RefCell;
  use std::rc::Rc;

  #[test]
  fn source_returns_expression_result() {
    let globals = Rc::new(RefCell::new(ObjectData::new()));
    globals.borrow_mut().properties.insert(
      PropertyKey::String("globalThis".to_string().into()),
      PropertyDescriptor::ordinary_data(Value::Object(Rc::clone(&globals))),
    );
    let mut vm = Vm::new(globals);

    let result = run_source(&mut vm, "1 + 2").expect("run");
    assert!(
      matches!(result, Value::Number(number) if (number - 3.0).abs() < f64::EPSILON),
      "expected 3, got {result:?}"
    );
  }
}
