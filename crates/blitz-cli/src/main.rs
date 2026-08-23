// SPDX-License-Identifier: MPL-2.0
//! CLI entry point for the full Blitz JavaScript runtime.
//!
//! This executable installs OS-facing host APIs such as `console` and
//! `process` before evaluating user code. Use `denko-engine` when testing the
//! host-neutral ECMAScript engine directly.

mod analysis;
mod args;
mod execution;
mod precompile;
mod repl;
mod typecheck;

use args::{Cli, CliCommand, ExecutionArgs};
use clap::Parser;

fn main() -> Result<(), denko::errors::Error> {
  // Parse before runtime setup so clap handles --help and --version immediately.
  let cli = Cli::parse();
  if let Some(cwd) = &cli.cwd {
    std::env::set_current_dir(cwd).map_err(|error| {
      denko::errors::RuntimeError::from(format!(
        "Failed to change working directory to {}: {error}",
        cwd.display()
      ))
    })?;
  }
  let node_compat = cli.node_compat.into();

  let execution = match cli.command {
    Some(CliCommand::Check(check)) => {
      let paths = typecheck::paths(&check.paths, check.project.as_deref())?;
      let success = typecheck::run(&paths, check.strict)?;
      std::process::exit(if success { 0 } else { 1 });
    }
    Some(CliCommand::Run(run)) => {
      if run.check
        && run.file.is_file()
        && !typecheck::run(std::slice::from_ref(&run.file), run.strict)?
      {
        std::process::exit(1);
      }
      ExecutionArgs {
        file: Some(run.file),
        preloads: cli.preload,
        env_files: cli.env_file,
        automatic_env: !cli.no_env_file,
        conditions: cli.conditions,
        explicit_run: true,
        if_present: run.if_present,
        eval: None,
        print: false,
        test262_harness: run.test262_harness,
        module: run.module,
        args: run.args,
      }
    }
    Some(CliCommand::Repl) => ExecutionArgs {
      file: None,
      preloads: cli.preload,
      env_files: cli.env_file,
      automatic_env: !cli.no_env_file,
      conditions: cli.conditions,
      explicit_run: false,
      if_present: false,
      eval: None,
      print: false,
      test262_harness: false,
      module: false,
      args: Vec::new(),
    },
    Some(CliCommand::Analyze(analyze)) => return analysis::run(analyze),
    Some(CliCommand::Precompile(precompile)) => {
      if let Err(error) = precompile::run(precompile, cli.conditions) {
        eprintln!("blitz precompile: {error}");
        std::process::exit(1);
      }
      return Ok(());
    }
    None => {
      let inline = cli.eval.is_some() || cli.print.is_some();
      let mut args = cli.args;
      let file = if cli.repl || inline {
        if inline && let Some(argument) = cli.file {
          args.insert(0, argument.to_string_lossy().into_owned());
        }
        None
      } else {
        cli.file
      };
      ExecutionArgs {
        file,
        preloads: cli.preload,
        env_files: cli.env_file,
        automatic_env: !cli.no_env_file,
        conditions: cli.conditions,
        explicit_run: false,
        if_present: false,
        eval: cli.eval.or(cli.print.clone()),
        print: cli.print.is_some(),
        test262_harness: cli.test262_harness,
        module: cli.module,
        args,
      }
    }
  };

  let status = execution::run(execution, node_compat)?;
  if status != 0 {
    std::process::exit(status);
  }
  Ok(())
}
