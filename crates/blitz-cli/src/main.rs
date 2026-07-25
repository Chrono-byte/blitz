// SPDX-License-Identifier: MPL-2.0
//! CLI entry point for the full Blitz JavaScript runtime.
//!
//! This executable installs OS-facing host APIs such as `console` and
//! `process` before evaluating user code. Use `blitz-engine` when testing the
//! host-neutral ECMAScript engine directly.
//!
//! The runtime CLI provides four main interfaces:
//! - **File execution**: Run a JavaScript or TypeScript file directly
//! - **Type checking**: Check TypeScript in-process from the OXC syntax tree
//! - **Editor analysis**: Emit compiler-owned analysis as JSON
//! - **REPL**: Interactive read-eval-print loop with history
//!
//! # Bootstrapping
//!
//! For execution and REPL startup, the CLI creates a VM and runs user code
//! within the initialized execution environment.
//!
//! # Usage
//!
//! ```bash
//! # Run a JavaScript file
//! cargo run -p blitz-cli -- path/to/file.js
//!
//! # Start REPL
//! cargo run -p blitz-cli -- --repl
//!
//! # Enable tracing
//! TSBLITZ_TRACE=1 cargo run -p blitz-cli -- file.js
//! ```

use clap::{Args, Parser, Subcommand};
use std::cell::RefCell;
use std::env;
use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::thread;
use std::time::{Duration, Instant};

use blitz::compiler;
use blitz::test262_support;
use blitz::{parser::Parser as JsParser, value::Value, vm::Vm};

/// Runs source code for a test while preserving the expression result.
/// Set as `#[cfg(test)]` to avoid the warning for it being unused.
///
/// # Errors
///
/// Returns an error if parsing, compilation, or execution fails.
#[cfg(test)]
fn run_source(
  vm: &mut Vm,
  source: &str,
) -> Result<Value, blitz::errors::Error> {
  let session = JsParser::parse_source(source)?;
  let oast_prog = session.program();

  let chunk = compiler::compile_oxc(oast_prog, source).map_err(blitz::errors::Error::from)?;
  vm.run(chunk).map_err(blitz::errors::Error::from)
}

/// Runs one source line in REPL mode while preserving the expression result.
///
/// # Errors
///
/// Returns an error if parsing, compilation, or execution fails.
fn run_source_repl(
  vm: &mut Vm,
  source: &str,
) -> Result<Value, blitz::errors::Error> {
  let session = JsParser::parse_source(source)?;
  let oast_prog = session.program();

  let chunk = compiler::compile_oxc_repl(oast_prog, source).map_err(blitz::errors::Error::from)?;
  vm.run(chunk).map_err(blitz::errors::Error::from)
}

/// Command line arguments for the Blitz executable.
#[derive(Parser, Debug)]
#[command(name = "blitz", about = "Blitz", subcommand_precedence_over_arg = true)]
struct Cli {
  /// Explicit toolchain or execution command.
  #[command(subcommand)]
  command: Option<CliCommand>,

  /// JavaScript file to execute. If omitted the REPL will be started.
  #[arg(value_name = "FILE")]
  file: Option<PathBuf>,

  /// Start the REPL even when a file is not provided.
  #[arg(short, long, action)]
  repl: bool,

  /// Run the file with Test262 host globals (`print`, `$262`, etc.).
  #[arg(long, action)]
  test262_harness: bool,

  /// Treat the input file as an ECMAScript module.
  #[arg(long, action)]
  module: bool,

  /// Arguments passed to the JavaScript program.
  #[arg(
    value_name = "ARGS",
    trailing_var_arg = true,
    allow_hyphen_values = true
  )]
  args: Vec<String>,
}

#[derive(Subcommand, Debug)]
/// Explicit Blitz commands.
enum CliCommand {
  /// Run a JavaScript or TypeScript file.
  Run(RunArgs),
  /// Type-check a project or source files with Blitz's in-process checker.
  Check(CheckArgs),
  /// Print compiler-owned editor analysis as JSON.
  Analyze(AnalyzeArgs),
}

#[derive(Args, Debug)]
/// Arguments for explicit file execution.
struct RunArgs {
  /// JavaScript or TypeScript file to execute.
  #[arg(value_name = "FILE")]
  file: PathBuf,

  /// Type-check the file before executing it.
  #[arg(long, action)]
  check: bool,

  /// Enable strict checking, including implicit-any diagnostics.
  #[arg(long, action)]
  strict: bool,

  /// Run the file with Test262 host globals (`print`, `$262`, etc.).
  #[arg(long, action)]
  test262_harness: bool,

  /// Treat the input file as an ECMAScript module.
  #[arg(long, action)]
  module: bool,

  /// Arguments passed to the JavaScript program.
  #[arg(
    value_name = "ARGS",
    trailing_var_arg = true,
    allow_hyphen_values = true
  )]
  args: Vec<String>,
}

#[derive(Args, Debug)]
/// Arguments for TypeScript project checking.
struct CheckArgs {
  /// Source files to check. The current directory is scanned when omitted.
  #[arg(value_name = "FILE")]
  paths: Vec<PathBuf>,

  /// Check a project directory or the directory containing a tsconfig file.
  #[arg(short, long, value_name = "PROJECT")]
  project: Option<PathBuf>,

  /// Enable strict checking, including implicit-any diagnostics.
  #[arg(long, action)]
  strict: bool,
}

#[derive(Args, Debug)]
/// Arguments for machine-readable editor analysis.
struct AnalyzeArgs {
  /// Source file to analyze, or `-` to read source from standard input.
  #[arg(value_name = "FILE", required_unless_present = "server")]
  file: Option<PathBuf>,

  /// Serve newline-delimited analysis requests over stdin/stdout.
  #[arg(long, action)]
  server: bool,

  /// Filename used to select syntax and module semantics for standard input.
  #[arg(long, value_name = "PATH")]
  path: Option<PathBuf>,

  /// Enable strict checking, including implicit-any diagnostics.
  #[arg(long, action)]
  strict: bool,
}

#[derive(Debug)]
/// Normalized execution options shared by legacy and explicit invocations.
struct ExecutionArgs {
  /// File to execute, or none for the REPL.
  file: Option<PathBuf>,
  /// Whether Test262 host globals should be installed.
  test262_harness: bool,
  /// Whether the entry should be loaded as an ECMAScript module.
  module: bool,
  /// Arguments exposed to the executed program.
  args: Vec<String>,
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

/// Returns whether a path has an executable TypeScript extension.
fn is_typescript_path(path: &Path) -> bool {
  path
    .extension()
    .and_then(|extension| extension.to_str())
    .is_some_and(|extension| matches!(extension, "ts" | "tsx" | "mts" | "cts"))
}

/// Recursively collects TypeScript sources while skipping dependency and build trees.
fn collect_typescript_files(
  directory: &Path,
  files: &mut Vec<PathBuf>,
) -> Result<(), blitz::errors::Error> {
  let entries = std::fs::read_dir(directory).map_err(|error| {
    blitz::errors::RuntimeError::from(format!(
      "Failed to read project directory {}: {}",
      directory.display(),
      error
    ))
  })?;
  for entry in entries {
    let entry = entry.map_err(|error| blitz::errors::RuntimeError::from(error.to_string()))?;
    let path = entry.path();
    let file_type = entry
      .file_type()
      .map_err(|error| blitz::errors::RuntimeError::from(error.to_string()))?;
    if file_type.is_symlink() {
      continue;
    }
    if file_type.is_dir() {
      let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
      if !matches!(name, ".git" | "node_modules" | "target") {
        collect_typescript_files(&path, files)?;
      }
    } else if is_typescript_path(&path) {
      let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
      if ![".d.ts", ".d.mts", ".d.cts"]
        .iter()
        .any(|suffix| name.ends_with(suffix))
      {
        files.push(path);
      }
    }
  }
  Ok(())
}

/// Resolves explicit files or discovers sources beneath a project root.
fn typecheck_paths(
  paths: &[PathBuf],
  project: Option<&Path>,
) -> Result<Vec<PathBuf>, blitz::errors::Error> {
  if project.is_some() && !paths.is_empty() {
    return Err(
      blitz::errors::RuntimeError::from("--project cannot be combined with source files").into(),
    );
  }
  if !paths.is_empty() {
    return Ok(paths.to_vec());
  }

  let requested = project.unwrap_or_else(|| Path::new("."));
  let root = if requested.is_file() {
    requested
      .parent()
      .unwrap_or_else(|| Path::new("."))
  } else {
    requested
  };
  let mut files = Vec::new();
  collect_typescript_files(root, &mut files)?;
  files.sort();
  Ok(files)
}

/// Parses and checks source files using Blitz's in-process OXC checker.
fn run_typecheck(
  paths: &[PathBuf],
  strict: bool,
) -> Result<bool, blitz::errors::Error> {
  let mut success = true;
  for path in paths {
    let source = std::fs::read_to_string(path).map_err(|error| {
      blitz::errors::RuntimeError::from(format!("Failed to read {}: {}", path.display(), error))
    })?;
    let source_type = JsParser::source_type_for_path(path, false);
    match JsParser::parse_source_with_type(&source, source_type) {
      Ok(session) => {
        let result = blitz::typecheck::with_strict_override(strict.then_some(true), || {
          blitz::typecheck::check(session.program())
        });
        if let Err(error) = result {
          eprintln!("{}: {}", path.display(), error);
          success = false;
        }
      }
      Err(error) => {
        eprintln!("{}: {}", path.display(), error);
        success = false;
      }
    }
  }
  Ok(success)
}

#[derive(serde::Deserialize)]
#[serde(tag = "method", rename_all = "snake_case")]
enum AnalysisServerRequest {
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

fn run_analysis_server(strict: bool) -> Result<(), blitz::errors::RuntimeError> {
  let stdin = std::io::stdin();
  let mut stdout = std::io::stdout().lock();
  let mut project = blitz::analysis::AnalysisProject::with_module_declarations(
    blitz_runtime::effect_assets::public_declaration_sources(),
  )
  .with_module_documentation(blitz_runtime::effect_assets::public_documentation());
  for line in stdin.lock().lines() {
    let line = line.map_err(|error| {
      blitz::errors::RuntimeError::from(format!("Failed to read analysis request: {error}"))
    })?;
    if line.trim().is_empty() {
      continue;
    }
    let response = match serde_json::from_str::<AnalysisServerRequest>(&line) {
      Ok(AnalysisServerRequest::Analyze {
        id,
        path,
        source,
        strict: request_strict,
      }) => {
        let result = project.analyze_document(source, &path, strict || request_strict);
        Some(serde_json::json!({ "id": id, "result": result }))
      }
      Ok(AnalysisServerRequest::Update { path, source }) => {
        project.update_document(source, &path);
        None
      }
      Ok(AnalysisServerRequest::Close { path }) => {
        project.close_document(&path);
        None
      }
      Err(error) => Some(serde_json::json!({ "id": null, "error": error.to_string() })),
    };
    if let Some(response) = response {
      serde_json::to_writer(&mut stdout, &response).map_err(|error| {
        blitz::errors::RuntimeError::from(format!("Failed to encode analysis response: {error}"))
      })?;
      stdout.write_all(b"\n").map_err(|error| {
        blitz::errors::RuntimeError::from(format!("Failed to write analysis response: {error}"))
      })?;
      stdout.flush().map_err(|error| {
        blitz::errors::RuntimeError::from(format!("Failed to flush analysis response: {error}"))
      })?;
    }
  }
  Ok(())
}

fn main() -> Result<(), blitz::errors::Error> {
  // Parse CLI early so clap can handle --help/--version before doing work.
  let cli = Cli::parse();

  let execution = match cli.command {
    Some(CliCommand::Check(check)) => {
      let paths = typecheck_paths(&check.paths, check.project.as_deref())?;
      let success = run_typecheck(&paths, check.strict)?;
      std::process::exit(if success { 0 } else { 1 });
    }
    Some(CliCommand::Run(run)) => {
      if run.check {
        let success = run_typecheck(std::slice::from_ref(&run.file), run.strict)?;
        if !success {
          std::process::exit(1);
        }
      }
      ExecutionArgs {
        file: Some(run.file),
        test262_harness: run.test262_harness,
        module: run.module,
        args: run.args,
      }
    }
    Some(CliCommand::Analyze(analyze)) => {
      if analyze.server {
        run_analysis_server(analyze.strict)?;
        return Ok(());
      }
      let file = analyze.file.as_deref().ok_or_else(|| {
        blitz::errors::RuntimeError::from("Analyze requires a file unless --server is used")
      })?;
      let source = if file == Path::new("-") {
        let mut source = String::new();
        std::io::stdin()
          .read_to_string(&mut source)
          .map_err(|error| {
            blitz::errors::RuntimeError::from(format!("Failed to read standard input: {error}"))
          })?;
        source
      } else {
        std::fs::read_to_string(file).map_err(|error| {
          blitz::errors::RuntimeError::from(format!("Failed to read {}: {error}", file.display()))
        })?
      };
      let path = analyze.path.as_deref().unwrap_or(file);
      let result = blitz::analysis::analyze(&source, path, analyze.strict);
      println!(
        "{}",
        serde_json::to_string(&result).map_err(|error| {
          blitz::errors::RuntimeError::from(format!("Failed to encode analysis: {error}"))
        })?
      );
      return Ok(());
    }
    None => ExecutionArgs {
      file: cli.file,
      test262_harness: cli.test262_harness,
      module: cli.module,
      args: cli.args,
    },
  };

  let async_done_state: Rc<RefCell<Option<test262_support::AsyncCompletion>>> =
    Rc::new(RefCell::new(None));

  // Public globals visible to user code (populated by the prelude)
  let globals: Rc<RefCell<blitz::value::ObjectData>> = if execution.test262_harness {
    let done_state =
      Rc::<RefCell<Option<test262_support::AsyncCompletion>>>::clone(&async_done_state);
    let print_native: blitz::value::NativeFn = Rc::new(move |vm, _this, args| {
      let value = args.first().cloned().unwrap_or(Value::Undefined);
      let rendered = match vm.to_string(&value) {
        Ok(s) => s,
        Err(_) => format!("{}", value),
      };
      if let Some(marker) = test262_support::async_completion_marker(&rendered) {
        *done_state.borrow_mut() = Some(marker);
        return Ok(Value::Null);
      }
      println!("{}", rendered);
      Ok(Value::Null)
    });
    test262_support::make_test262_globals_with_print(print_native)
  } else {
    let globals: Rc<RefCell<blitz::value::ObjectData>> =
      Rc::new(RefCell::new(blitz::value::ObjectData::new()));
    let global_this = Value::Object(Rc::clone(&globals));
    {
      let mut g = globals.borrow_mut();
      g.properties.insert(
        blitz::value::PropertyKey::String("globalThis".to_string()),
        blitz::value::PropertyDescriptor::data(global_this),
      );
    }
    globals
  };

  // Create one VM for the selected execution mode. The REPL reuses it for
  // every input so it does not recreate the VM on each line.
  // Create VM and run prelude. blitzlib can call `__host_get(name)` during
  // bootstrap to capture host-provided values; raw host entries are never
  // placed into the globals table.
  let mut repl_vm = Vm::new(globals);
  let mut runtime_argv = Vec::new();
  runtime_argv.push("blitz".to_string());
  if let Some(path) = &execution.file {
    runtime_argv.push(path.to_string_lossy().into_owned());
  }
  runtime_argv.extend(execution.args.iter().cloned());
  let mut runtime = blitz_runtime::host::install_runtime(
    &mut repl_vm,
    blitz_runtime::host::RuntimeOptions::from_host(runtime_argv),
  );

  // If a file was provided, run it and exit. Otherwise start REPL.
  if let Some(path) = execution.file {
    let source = std::fs::read_to_string(&path).map_err(|e| {
      blitz::errors::RuntimeError::from(format!("Failed to read {}: {}", path.display(), e))
    })?;
    if should_load_as_module(&path, &source, execution.module) {
      blitz_runtime::modules::evaluate_entry(&mut repl_vm, &path)?;
    } else {
      let source_type = JsParser::source_type_for_path(&path, false);
      let session = JsParser::parse_source_with_type(&source, source_type)?;
      let program = session.program();
      let chunk = compiler::compile_oxc(program, &source).map_err(blitz::errors::Error::from)?;
      repl_vm
        .run(chunk)
        .map_err(blitz::errors::Error::from)?;

      if execution.test262_harness && repl_vm.get_global("$DONE").is_some() {
        let start = Instant::now();
        let timeout = Duration::from_secs(30);
        loop {
          repl_vm
            .flush_microtasks()
            .map_err(blitz::errors::Error::from)?;

          if let Some(done) = async_done_state.borrow().clone() {
            match done {
              test262_support::AsyncCompletion::Complete => {
                println!("Test262:AsyncTestComplete");
                break;
              }
              test262_support::AsyncCompletion::Failure(message) => {
                return Err(
                  blitz::errors::RuntimeError::from(format!("async test failed: {}", message))
                    .into(),
                );
              }
            }
          }

          if start.elapsed() >= timeout {
            return Err(
              blitz::errors::RuntimeError::from(format!(
                "async test timed out after {}s",
                timeout.as_secs()
              ))
              .into(),
            );
          }

          thread::sleep(Duration::from_millis(10));
        }
      }
    }
    runtime
      .run(&mut repl_vm)
      .map_err(blitz::errors::Error::from)?;
    return Ok(());
  }

  // Start REPL (explicit flag or no file provided).
  println!("Blitz REPL. Type exit to quit.");
  let mut rl = rustyline::Editor::<(), rustyline::history::DefaultHistory>::new()
    .map_err(|e| blitz::errors::RuntimeError::from(format!("Readline init error: {}", e)))?;
  let history_path = env::temp_dir().join("blitz_repl_history");
  drop(rl.load_history(&history_path));
  loop {
    match rl.readline("> ") {
      Ok(input) => {
        let line: &str = input.trim();
        if line.is_empty() {
          continue;
        }
        drop(rl.add_history_entry(line));
        drop(rl.save_history(&history_path));
        if line.eq_ignore_ascii_case("exit") || line.eq_ignore_ascii_case("quit") {
          break;
        }
        match run_source_repl(&mut repl_vm, line) {
          Ok(v) if !matches!(v, Value::Null | Value::Undefined) => {
            println!("{}", blitz::inspect::inspect(&v));
          }
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
  use std::sync::atomic::{AtomicUsize, Ordering};

  static TEMP_DIRECTORY_ID: AtomicUsize = AtomicUsize::new(0);

  fn temporary_directory() -> PathBuf {
    env::temp_dir().join(format!(
      "blitz-cli-test-{}-{}",
      std::process::id(),
      TEMP_DIRECTORY_ID.fetch_add(1, Ordering::Relaxed)
    ))
  }

  fn test_globals() -> Rc<RefCell<ObjectData>> {
    let globals = Rc::new(RefCell::new(ObjectData::new()));
    globals.borrow_mut().properties.insert(
      PropertyKey::String("globalThis".to_string()),
      PropertyDescriptor::data(Value::Object(Rc::clone(&globals))),
    );
    globals
  }

  #[test]
  fn run_source_executes_basic_program() {
    let globals = Rc::new(RefCell::new(ObjectData::new()));
    let mut vm = Vm::new(globals);

    run_source(&mut vm, blitz::js!("var answer = 1 + 2; answer;")).expect("run");

    assert!(
      matches!(vm.get_global("answer"), Some(Value::Number(n)) if (n - 3.0).abs() < f64::EPSILON)
    );
  }

  #[test]
  fn run_source_returns_expression_result() {
    let globals = test_globals();
    let mut vm = Vm::new(globals);

    let result = run_source_repl(&mut vm, "1 + 2").expect("run");
    assert!(
      matches!(result, Value::Number(n) if (n - 3.0).abs() < f64::EPSILON),
      "expected 3, got {:?}",
      result
    );
  }

  #[test]
  fn typescript_annotations_are_erased_for_execution() {
    let source = blitz::js!(
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
    let mut vm = Vm::new(test_globals());

    vm.run(chunk).expect("run TypeScript");

    assert!(
      matches!(vm.get_global("result"), Some(Value::Number(n)) if (n - 42.0).abs() < f64::EPSILON)
    );
  }

  #[test]
  fn parses_legacy_file_invocation() {
    let cli = Cli::try_parse_from(["blitz", "program.js", "one"]).expect("parse");

    assert!(cli.command.is_none());
    assert_eq!(cli.file, Some(PathBuf::from("program.js")));
    assert_eq!(cli.args, ["one"]);
  }

  #[test]
  fn parses_run_with_typecheck() {
    let cli =
      Cli::try_parse_from(["blitz", "run", "--check", "program.ts", "--", "one"]).expect("parse");

    assert!(matches!(cli.command, Some(CliCommand::Run(_))));
    if let Some(CliCommand::Run(run)) = cli.command {
      assert!(run.check);
      assert_eq!(run.file, PathBuf::from("program.ts"));
      assert_eq!(run.args, ["one"]);
    }
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

  #[test]
  fn keeps_explicit_typecheck_paths() {
    let paths = vec![PathBuf::from("one.ts"), PathBuf::from("two.ts")];

    assert_eq!(typecheck_paths(&paths, None).expect("paths"), paths);
  }

  #[test]
  fn rejects_project_with_source_files() {
    let result = typecheck_paths(
      &[PathBuf::from("program.ts")],
      Some(Path::new("tsconfig.json")),
    );

    assert!(result.is_err());
  }

  #[test]
  fn discovers_project_typescript_sources() {
    let root = temporary_directory();
    let source_directory = root.join("src").join("nested");
    std::fs::create_dir_all(&source_directory).expect("create source directory");
    std::fs::create_dir_all(root.join("node_modules")).expect("create dependency directory");
    let source = source_directory.join("program.ts");
    std::fs::write(&source, blitz::js!(typescript, "let value: number = 1;"))
      .expect("write source");
    std::fs::write(
      source_directory.join("types.d.ts"),
      blitz::js!(typescript, "declare const value: number;"),
    )
    .expect("write declaration");
    std::fs::write(root.join("node_modules").join("ignored.ts"), "").expect("write ignored file");

    assert_eq!(typecheck_paths(&[], Some(&root)).expect("paths"), [source]);

    std::fs::remove_dir_all(root).expect("remove test directory");
  }

  #[test]
  fn in_process_checker_reports_file_errors() {
    let root = temporary_directory();
    std::fs::create_dir_all(&root).expect("create test directory");
    let valid = root.join("valid.ts");
    let invalid = root.join("invalid.ts");
    std::fs::write(&valid, blitz::js!(typescript, "const answer: number = 42;"))
      .expect("write valid source");
    std::fs::write(
      &invalid,
      blitz::js!(typescript, "const answer: number = 'wrong';"),
    )
    .expect("write invalid source");

    assert!(run_typecheck(&[valid], false).expect("check valid source"));
    assert!(!run_typecheck(&[invalid], false).expect("check invalid source"));

    std::fs::remove_dir_all(root).expect("remove test directory");
  }
}
