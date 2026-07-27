// SPDX-License-Identifier: MPL-2.0

use crate::args::ExecutionArgs;
use denko::compiler;
use denko::parser::Parser as JsParser;
use denko::test262_support;
use denko::value::Value;
use denko::vm::Vm;
use std::cell::RefCell;
use std::path::Path;
use std::process::Command;
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

  let runtime_argv = runtime_argv(execution.file.as_deref(), &execution.args);
  let mut runtime_options = blitz_runtime::host::RuntimeOptions::from_host(runtime_argv);
  runtime_options.node_compat = node_compat;
  runtime_options.conditions = execution.conditions.clone();
  let cwd = std::env::current_dir().map_err(|error| {
    denko::errors::RuntimeError::from(format!("Failed to read working directory: {error}"))
  })?;
  let project = blitz_runtime::project::ProjectContext::discover(&cwd)?;
  runtime_options.env = project.load_environment(
    &runtime_options.env,
    &execution.env_files,
    execution.automatic_env,
  )?;

  if execution.explicit_run
    && let Some(target) = execution.file.as_ref()
  {
    let script_name = target.to_string_lossy();
    if let Some(commands) = project.package_script(&script_name) {
      run_package_script(&project, &commands, &execution.args, &runtime_options.env)?;
      return Ok(());
    }
    if execution.if_present && !target.is_file() {
      return Ok(());
    }
  }

  if let Some(source) = execution.eval {
    let mut vm = Vm::new(globals);
    let mut runtime = blitz_runtime::host::install_runtime(&mut vm, runtime_options);
    blitz_runtime::modules::evaluate_preloads(&mut vm, &execution.preloads)?;
    let value = evaluate_inline(&mut vm, &source)?;
    if execution.print {
      print_completion(&value);
    }
    runtime.run(&mut vm)?;
    return Ok(());
  }

  let Some(path) = execution.file else {
    return blitz_runtime::repl::run(runtime_options, &execution.preloads);
  };

  let mut vm = Vm::new(globals);
  let mut runtime = blitz_runtime::host::install_runtime(&mut vm, runtime_options);
  blitz_runtime::modules::evaluate_preloads(&mut vm, &execution.preloads)?;

  let source = std::fs::read_to_string(&path).map_err(|error| {
    denko::errors::RuntimeError::from(format!("Failed to read {}: {error}", path.display()))
  })?;
  let entry_directory = path
    .parent()
    .filter(|parent| !parent.as_os_str().is_empty())
    .unwrap_or_else(|| Path::new("."));
  let entry_project = blitz_runtime::project::ProjectContext::discover(entry_directory)?;
  let package_type = entry_project
    .package
    .as_ref()
    .and_then(|package| package.package_type);
  if should_load_as_module(&path, &source, execution.module, package_type) {
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

fn run_package_script(
  project: &blitz_runtime::project::ProjectContext,
  commands: &[blitz_runtime::project::PackageScriptCommand],
  arguments: &[String],
  environment: &std::collections::BTreeMap<String, String>,
) -> Result<(), denko::errors::Error> {
  let bin_directory = project.root.join("node_modules").join(".bin");
  let mut paths = vec![bin_directory];
  if let Some(path) = environment.get("PATH") {
    paths.extend(std::env::split_paths(path));
  }
  let path = std::env::join_paths(paths).map_err(|error| {
    denko::errors::RuntimeError::from(format!("Failed to construct package script PATH: {error}"))
  })?;

  for script in commands {
    let displayed_arguments = if script.forwards_arguments {
      arguments
        .iter()
        .map(|argument| format_shell_argument(argument))
        .collect::<Vec<_>>()
        .join(" ")
    } else {
      String::new()
    };
    if displayed_arguments.is_empty() {
      eprintln!("$ {}", script.command);
    } else {
      eprintln!("$ {} {displayed_arguments}", script.command);
    }

    let mut command = package_shell_command(script, arguments);
    command
      .current_dir(&project.root)
      .envs(environment)
      .env("PATH", &path)
      .env("npm_lifecycle_event", &script.event)
      .env("npm_lifecycle_script", &script.command);
    if let Some(path) = &project.package_json_path {
      command.env("npm_package_json", path);
    }
    if let Some(package) = &project.package {
      if let Some(name) = &package.name {
        command.env("npm_package_name", name);
      }
      if let Some(version) = &package.version {
        command.env("npm_package_version", version);
      }
    }

    let status = command.status().map_err(|error| {
      denko::errors::RuntimeError::from(format!(
        "Failed to execute package script {}: {error}",
        script.event
      ))
    })?;
    if !status.success() {
      std::process::exit(status.code().unwrap_or(1));
    }
  }
  Ok(())
}

#[cfg(unix)]
fn package_shell_command(
  script: &blitz_runtime::project::PackageScriptCommand,
  arguments: &[String],
) -> Command {
  let mut command = Command::new("/bin/sh");
  if script.forwards_arguments {
    command
      .arg("-c")
      .arg(format!("{} \"$@\"", script.command))
      .arg(&script.event)
      .args(arguments);
  } else {
    command.arg("-c").arg(&script.command);
  }
  command
}

#[cfg(windows)]
fn package_shell_command(
  script: &blitz_runtime::project::PackageScriptCommand,
  arguments: &[String],
) -> Command {
  let mut command = Command::new("cmd.exe");
  let mut source = script.command.clone();
  if script.forwards_arguments {
    for argument in arguments {
      source.push(' ');
      source.push_str(&format_shell_argument(argument));
    }
  }
  command.args(["/d", "/s", "/c", &source]);
  command
}

fn format_shell_argument(argument: &str) -> String {
  if argument
    .chars()
    .all(|character| character.is_ascii_alphanumeric() || "-_./:@".contains(character))
  {
    argument.to_string()
  } else {
    format!("\"{}\"", argument.replace('"', "\\\""))
  }
}

fn runtime_argv(
  file: Option<&Path>,
  arguments: &[String],
) -> Vec<String> {
  let executable = std::env::current_exe().map_or_else(
    |_| "blitz".to_string(),
    |path| path.to_string_lossy().into_owned(),
  );
  let mut argv = vec![executable];
  if let Some(file) = file {
    let file = std::fs::canonicalize(file).unwrap_or_else(|_| file.to_path_buf());
    argv.push(file.to_string_lossy().into_owned());
  }
  argv.extend(arguments.iter().cloned());
  argv
}

fn print_completion(value: &Value) {
  if let Value::String(string) = value {
    println!("{}", string.to_string_lossy());
  } else {
    println!("{}", denko::inspect::inspect(value));
  }
}

fn evaluate_inline(
  vm: &mut Vm,
  source: &str,
) -> Result<Value, denko::errors::Error> {
  let source_type = JsParser::source_type_for_path(Path::new("[eval].ts"), false);
  let session = JsParser::parse_source_with_type(source, source_type)?;
  let chunk = compiler::compile_oxc_repl(session.program(), source)?;
  vm.run(chunk).map_err(denko::errors::Error::from)
}

fn should_load_as_module(
  path: &Path,
  source: &str,
  explicit_module: bool,
  package_type: Option<blitz_runtime::project::PackageType>,
) -> bool {
  if explicit_module || JsParser::source_type_for_path(path, false).is_module() {
    return true;
  }

  if package_type == Some(blitz_runtime::project::PackageType::Module)
    && path
      .extension()
      .and_then(|extension| extension.to_str())
      .is_some_and(|extension| matches!(extension, "js" | "jsx"))
  {
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

  #[cfg(unix)]
  #[test]
  fn package_scripts_run_lifecycle_hooks_with_arguments_and_metadata() {
    let directory = std::env::temp_dir().join(format!(
      "blitz-cli-script-{}-{}",
      std::process::id(),
      std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos()
    ));
    std::fs::create_dir_all(&directory).expect("create package directory");
    std::fs::write(
      directory.join("package.json"),
      r#"{
        "name": "script-fixture",
        "version": "1.0.0",
        "scripts": {
          "precheck": "printf 'pre:%s\\n' \"$npm_lifecycle_event\" >> result.txt",
          "check": "printf 'main:%s:%s\\n' \"$npm_package_name\" \"$1\" >> result.txt; :",
          "postcheck": "printf 'post:%s\\n' \"$npm_lifecycle_event\" >> result.txt"
        }
      }"#,
    )
    .expect("write package manifest");
    let project = blitz_runtime::project::ProjectContext::discover(&directory).expect("project");
    let commands = project
      .package_script("check")
      .expect("script plan");
    let environment = std::env::vars().collect();

    run_package_script(&project, &commands, &["argument".to_string()], &environment)
      .expect("run package script");

    assert_eq!(
      std::fs::read_to_string(directory.join("result.txt")).expect("script output"),
      "pre:precheck\nmain:script-fixture:argument\npost:postcheck\n"
    );
    std::fs::remove_dir_all(directory).expect("remove package directory");
  }

  #[test]
  fn runtime_arguments_match_hosted_execution_modes() {
    let arguments = vec!["one".to_string(), "two".to_string()];
    let inline = runtime_argv(None, &arguments);
    assert!(inline[0].ends_with("blitz") || inline[0].contains("blitz-"));
    assert_eq!(&inline[1..], ["one", "two"]);

    let file = runtime_argv(Some(Path::new("missing-app.ts")), &arguments);
    assert_eq!(Path::new(&file[1]), Path::new("missing-app.ts"));
    assert_eq!(&file[2..], ["one", "two"]);
  }

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
  fn inline_typescript_returns_its_completion_value() {
    let mut vm = Vm::new(standard_globals());
    let result = evaluate_inline(
      &mut vm,
      denko::js!(typescript, "const value: number = 21; value * 2"),
    )
    .expect("evaluate inline TypeScript");

    assert_eq!(result, Value::Number(42.0));

    let string = evaluate_inline(&mut vm, denko::js!("'hello'")).expect("evaluate string");
    assert_eq!(string, Value::String("hello".into()));
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
    assert!(should_load_as_module(
      Path::new("entry.mjs"),
      "",
      false,
      None
    ));
    assert!(should_load_as_module(
      Path::new("entry.mts"),
      "",
      false,
      None
    ));
    assert!(!should_load_as_module(
      Path::new("entry.js"),
      "",
      false,
      None
    ));
    assert!(!should_load_as_module(
      Path::new("entry.cjs"),
      "",
      false,
      None
    ));
    assert!(should_load_as_module(Path::new("entry.js"), "", true, None));
    assert!(should_load_as_module(
      Path::new("entry.js"),
      "",
      false,
      Some(blitz_runtime::project::PackageType::Module)
    ));
  }

  #[test]
  fn typescript_imports_use_the_module_loader() {
    assert!(should_load_as_module(
      Path::new("entry.ts"),
      "import * as Ref from \"effect/Ref\"; Ref.make(0);",
      false,
      None,
    ));
    assert!(!should_load_as_module(
      Path::new("entry.ts"),
      "const answer: number = 42;",
      false,
      None,
    ));
  }
}
