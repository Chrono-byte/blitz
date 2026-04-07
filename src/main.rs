//! CLI entry point for Blitz VM.
//!
//! The CLI provides two main interfaces:
//! - **File execution**: Run a JavaScript file directly
//! - **REPL**: Interactive read-eval-print loop with history
//!
//! # Bootstrapping
//!
//! On startup, the CLI creates a VM and executes user code within the
//! initialized execution environment.
//!
//! # Usage
//!
//! ```bash
//! # Run a JavaScript file
//! cargo run -- path/to/file.js
//!
//! # Start REPL
//! cargo run -- --repl
//!
//! # Enable tracing
//! TSBLITZ_TRACE=1 cargo run -- file.js
//! ```

use clap::Parser;
use std::cell::RefCell;
use std::env;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use blitz::compiler;
use blitz::{parser::Parser as JsParser, runtime_utils, value::Value, vm::Vm};

/// Runs source code in REPL mode (preserves expression results)
/// Set as `#[cfg(test)]` to avoid the warning for it being unused.
///
/// # Errors
///
/// Returns an error string if any stage of compilation fails
#[cfg(test)]
fn run_source(vm: &mut Vm, source: &str) -> Result<Value, blitz::errors::Error> {
    let session = JsParser::parse_source(source)?;
    let oast_prog = session.program();

    let chunk = compiler::compile_oxc(oast_prog, source).map_err(blitz::errors::Error::from)?;
    vm.run(chunk).map_err(blitz::errors::Error::from)
}

/// Runs source code in REPL mode (preserves expression results)
///
/// # Errors
///
/// Returns an error string if any stage of compilation fails
fn run_source_repl(vm: &mut Vm, source: &str) -> Result<Value, blitz::errors::Error> {
    let session = JsParser::parse_source(source)?;
    let oast_prog = session.program();

    let chunk =
        compiler::compile_oxc_repl(oast_prog, source).map_err(blitz::errors::Error::from)?;
    vm.run(chunk).map_err(blitz::errors::Error::from)
}

/// Command line arguments for the Blitz executable.
#[derive(Parser, Debug)]
#[command(name = "blitz", about = "Blitz")]
struct Cli {
    /// JavaScript file to execute. If omitted the REPL will be started.
    #[arg(value_name = "FILE")]
    file: Option<PathBuf>,

    /// Start the REPL even when a file is not provided.
    #[arg(short, long, action)]
    repl: bool,
}

fn main() -> Result<(), blitz::errors::Error> {
    // Public globals visible to user code (populated by the prelude)
    let globals: Rc<RefCell<blitz::value::ObjectData>> =
        Rc::new(RefCell::new(blitz::value::ObjectData::new()));

    // Provide a `globalThis` object that reflects the VM globals map so soft
    // layer code that writes `globalThis.x = ...` updates the VM globals.
    // We reuse the same Rc<RefCell<HashMap>> so property ops on the object and
    // Get/SetGlobal access the same backing map.
    let global_this = Value::Object(Rc::clone(&globals));
    {
        let mut g = globals.borrow_mut();
        g.properties.insert(
            blitz::value::PropertyKey::String("globalThis".to_string()),
            blitz::value::PropertyDescriptor::data(global_this),
        );
    }

    runtime_utils::install_minimal_kernel_constructors(&globals);

    // Parse CLI early so clap can handle --help/--version before doing work.
    let cli = Cli::parse();

    // Create a persistent VM for the REPL and reuse it for every input so
    // we don't recompile and recreate the VM on each line.
    // Create VM and run prelude. blitzlib can call `__host_get(name)` during
    // bootstrap to capture host-provided values; raw host entries are never
    // placed into the globals table.
    let mut repl_vm = Vm::new(globals);

    // If a file was provided, run it and exit. Otherwise start REPL.
    if let Some(path) = cli.file {
        // Use ModuleLoader to load and run the entry file (and its deps)
        let loader_handle = repl_vm.module_loader_handle();
        {
            let mut loader = loader_handle.borrow_mut();
            loader.construct(
                path.parent().unwrap_or(Path::new(".")),
                &format!("./{}", path.file_name().unwrap().to_string_lossy()),
            )?;
            loader
                .instantiate_all()
                .map_err(blitz::errors::Error::from)?;
        }
        blitz::module::evaluate_entry_shared(&loader_handle, &path, &mut repl_vm, false)
            .map_err(blitz::errors::Error::from)?;
        return Ok(());
    }

    // Start REPL (explicit flag or no file provided)
    println!("Blitz REPL. Type exit to quit.");
    let mut rl = rustyline::Editor::<(), rustyline::history::DefaultHistory>::new()
        .map_err(|e| blitz::errors::RuntimeError::from(format!("Readline init error: {}", e)))?;
    let history_path = env::temp_dir().join("blitz_repl_history");
    let _ = rl.load_history(&history_path);
    loop {
        match rl.readline("> ") {
            Ok(input) => {
                let line: &str = input.trim();
                if line.is_empty() {
                    continue;
                }
                let _ = rl.add_history_entry(line);
                let _ = rl.save_history(&history_path);
                if line.eq_ignore_ascii_case("exit") || line.eq_ignore_ascii_case("quit") {
                    break;
                }
                match run_source_repl(&mut repl_vm, line) {
                    Ok(v) if !matches!(v, Value::Null | Value::Undefined) => println!("{}", v),
                    Ok(_) => {}
                    Err(e) => eprintln!("Error: {}", e),
                }
            }
            Err(rustyline::error::ReadlineError::Interrupted)
            | Err(rustyline::error::ReadlineError::Eof) => break,
            Err(e) => {
                eprintln!("Readline error: {}", e);
                break;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use blitz::value::{ObjectData, PropertyDescriptor, PropertyKey};
    use std::cell::RefCell;
    use std::rc::Rc;

    fn test_globals() -> Rc<RefCell<ObjectData>> {
        let globals = Rc::new(RefCell::new(ObjectData::new()));
        globals.borrow_mut().properties.insert(
            PropertyKey::String("globalThis".to_string()),
            PropertyDescriptor::data(Value::Object(Rc::clone(&globals))),
        );
        runtime_utils::install_minimal_kernel_constructors(&globals);
        globals
    }

    #[test]
    fn run_source_executes_basic_program() {
        let globals = Rc::new(RefCell::new(ObjectData::new()));
        let mut vm = Vm::new(Rc::clone(&globals));

        run_source(&mut vm, "var answer = 1 + 2; answer;").expect("run");

        assert!(
            matches!(vm.get_global("answer"), Some(Value::Number(n)) if (n - 3.0).abs() < f64::EPSILON)
        );
    }

    #[test]
    fn run_source_returns_expression_result() {
        let globals = test_globals();
        let mut vm = Vm::new(Rc::clone(&globals));

        let result = run_source_repl(&mut vm, "1 + 2").expect("run");
        assert!(
            matches!(result, Value::Number(n) if (n - 3.0).abs() < f64::EPSILON),
            "expected 3, got {:?}",
            result
        );
    }
}
