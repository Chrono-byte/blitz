// SPDX-License-Identifier: MPL-2.0
//! CLI entry point for the full Blitz JavaScript runtime.
//!
//! This executable installs OS-facing host APIs such as `console` and
//! `process` before evaluating user code. Use `blitz-engine` when testing the
//! host-neutral ECMAScript engine directly.

mod analysis;
mod args;
mod execution;
mod repl;
mod typecheck;

use args::{Cli, CliCommand, ExecutionArgs};
use clap::Parser;

fn main() -> Result<(), blitz::errors::Error> {
  // Parse before runtime setup so clap handles --help and --version immediately.
  let cli = Cli::parse();
  let node_compat = cli.node_compat.into();

  let execution = match cli.command {
    Some(CliCommand::Check(check)) => {
      let paths = typecheck::paths(&check.paths, check.project.as_deref())?;
      let success = typecheck::run(&paths, check.strict)?;
      std::process::exit(if success { 0 } else { 1 });
    }
    Some(CliCommand::Run(run)) => {
      if run.check && !typecheck::run(std::slice::from_ref(&run.file), run.strict)? {
        std::process::exit(1);
      }
      ExecutionArgs {
        file: Some(run.file),
        test262_harness: run.test262_harness,
        module: run.module,
        args: run.args,
      }
    }
    Some(CliCommand::Analyze(analyze)) => return analysis::run(analyze),
    None => ExecutionArgs {
      file: cli.file,
      test262_harness: cli.test262_harness,
      module: cli.module,
      args: cli.args,
    },
  };

  execution::run(execution, node_compat)
}
