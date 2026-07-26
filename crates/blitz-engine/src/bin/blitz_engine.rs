// SPDX-License-Identifier: MPL-2.0
//! Host-neutral command-line harness for the Blitz ECMAScript engine.
//!
//! This binary intentionally does not install runtime APIs such as `console`,
//! `process`, filesystem access, or networking. It is useful for engine
//! development, conformance work, and embedding diagnostics. Applications
//! should use the `blitz` runtime binary.

use std::cell::RefCell;
use std::env;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use blitz::compiler;
use blitz::parser::Parser as JsParser;
use blitz::value::{ObjectData, PropertyDescriptor, PropertyKey, Value};
use blitz::vm::Vm;
use clap::Parser;

/// Raw engine command-line arguments.
#[derive(Parser, Debug)]
#[command(
  name = "blitz-engine",
  about = "Host-neutral command-line harness for the Blitz ECMAScript engine"
)]
struct Cli {
  /// JavaScript file to execute. If omitted, the engine REPL is started.
  #[arg(value_name = "FILE")]
  file: Option<PathBuf>,

  /// Start the engine REPL even when a file is not provided.
  #[arg(short, long, action)]
  repl: bool,

  /// Treat the input file as an ECMAScript module.
  #[arg(long, action)]
  module: bool,
}

fn main() -> Result<(), blitz::errors::Error> {
  let cli = Cli::parse();
  let mut vm = Vm::new(engine_globals());

  if let Some(path) = cli.file {
    if cli.module {
      run_module(&mut vm, &path)?;
    } else {
      run_script(&mut vm, &path)?;
    }
    return Ok(());
  }

  run_repl(&mut vm)
}

fn engine_globals() -> Rc<RefCell<ObjectData>> {
  let globals = Rc::new(RefCell::new(ObjectData::new()));
  globals.borrow_mut().properties.insert(
    PropertyKey::String("globalThis".to_string().into()),
    PropertyDescriptor::ordinary_data(Value::Object(Rc::clone(&globals))),
  );
  globals
}

fn run_script(
  vm: &mut Vm,
  path: &Path,
) -> Result<(), blitz::errors::Error> {
  let source = std::fs::read_to_string(path).map_err(|error| {
    blitz::errors::RuntimeError::from(format!("Failed to read {}: {error}", path.display()))
  })?;
  let session = JsParser::parse_source(&source)?;
  let chunk = compiler::compile_oxc(session.program(), &source)?;
  vm.run(chunk)?;
  Ok(())
}

fn run_module(
  vm: &mut Vm,
  path: &Path,
) -> Result<(), blitz::errors::Error> {
  let loader_handle = vm.module_loader_handle();
  {
    let mut loader = loader_handle.borrow_mut();
    loader.construct(
      path.parent().unwrap_or_else(|| Path::new(".")),
      &format!(
        "./{}",
        path
          .file_name()
          .ok_or_else(|| blitz::errors::RuntimeError::from("module path has no file name"))?
          .to_string_lossy()
      ),
    )?;
    loader.instantiate_all()?;
  }
  blitz::module::evaluate_entry_shared(&loader_handle, path, vm, false)?;
  Ok(())
}

fn run_repl(vm: &mut Vm) -> Result<(), blitz::errors::Error> {
  println!("Blitz engine REPL. Host APIs are disabled. Type exit to quit.");
  let mut editor = rustyline::Editor::<(), rustyline::history::DefaultHistory>::new()
    .map_err(|error| blitz::errors::RuntimeError::from(format!("Readline init error: {error}")))?;
  let history_path = env::temp_dir().join("blitz_engine_repl_history");
  drop(editor.load_history(&history_path));

  loop {
    match editor.readline(">> ") {
      Ok(line) => {
        let trimmed = line.trim();
        if trimmed.eq_ignore_ascii_case("exit") || trimmed.eq_ignore_ascii_case("quit") {
          break;
        }
        drop(editor.add_history_entry(line.as_str()));
        match run_repl_source(vm, &line) {
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

  drop(editor.save_history(&history_path));
  Ok(())
}

fn run_repl_source(
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

  #[test]
  fn raw_engine_globals_exclude_runtime_hosts() {
    let vm = Vm::new(engine_globals());
    assert!(vm.get_global("globalThis").is_some());
    assert!(vm.get_global("console").is_none());
    assert!(vm.get_global("process").is_none());
  }
}
