// SPDX-License-Identifier: MPL-2.0

use clap::{Args, Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

/// Node.js compatibility profiles exposed by the hosted runtime.
#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
#[value(rename_all = "lower")]
pub(crate) enum NodeCompatArg {
  /// Disable all `node:*` compatibility modules.
  Off,
  /// Enable the core process-local compatibility modules.
  Core,
  /// Enable compatibility modules used by development tooling.
  Tooling,
  /// Enable all completed Node compatibility modules.
  Full,
}

impl From<NodeCompatArg> for blitz_runtime::host::NodeCompatProfile {
  fn from(profile: NodeCompatArg) -> Self {
    match profile {
      NodeCompatArg::Off => Self::Off,
      NodeCompatArg::Core => Self::Core,
      NodeCompatArg::Tooling => Self::Tooling,
      NodeCompatArg::Full => Self::Full,
    }
  }
}

/// Command line arguments for the Blitz executable.
#[derive(Parser, Debug)]
#[command(name = "blitz", about = "Blitz", subcommand_precedence_over_arg = true)]
pub(crate) struct Cli {
  /// Explicit toolchain or execution command.
  #[command(subcommand)]
  pub(crate) command: Option<CliCommand>,

  /// JavaScript file to execute. If omitted the REPL will be started.
  #[arg(value_name = "FILE")]
  pub(crate) file: Option<PathBuf>,

  /// Select the Node.js compatibility profile: off, core, tooling, or full.
  #[arg(long, value_enum, default_value_t = NodeCompatArg::Core, global = true)]
  pub(crate) node_compat: NodeCompatArg,

  /// Start the REPL even when a file is not provided.
  #[arg(short, long, action)]
  pub(crate) repl: bool,

  /// Run the file with Test262 host globals (`print`, `$262`, etc.).
  #[arg(long, action)]
  pub(crate) test262_harness: bool,

  /// Treat the input file as an ECMAScript module.
  #[arg(long, action)]
  pub(crate) module: bool,

  /// Arguments passed to the JavaScript program.
  #[arg(
    value_name = "ARGS",
    trailing_var_arg = true,
    allow_hyphen_values = true
  )]
  pub(crate) args: Vec<String>,
}

/// Explicit Blitz commands.
#[derive(Subcommand, Debug)]
pub(crate) enum CliCommand {
  /// Run a JavaScript or TypeScript file.
  Run(RunArgs),
  /// Type-check a project or source files with Blitz's in-process checker.
  Check(CheckArgs),
  /// Print compiler-owned editor analysis as JSON.
  Analyze(AnalyzeArgs),
}

/// Arguments for explicit file execution.
#[derive(Args, Debug)]
pub(crate) struct RunArgs {
  /// JavaScript or TypeScript file to execute.
  #[arg(value_name = "FILE")]
  pub(crate) file: PathBuf,

  /// Type-check the file before executing it.
  #[arg(long, action)]
  pub(crate) check: bool,

  /// Enable strict checking, including implicit-any diagnostics.
  #[arg(long, action)]
  pub(crate) strict: bool,

  /// Run the file with Test262 host globals (`print`, `$262`, etc.).
  #[arg(long, action)]
  pub(crate) test262_harness: bool,

  /// Treat the input file as an ECMAScript module.
  #[arg(long, action)]
  pub(crate) module: bool,

  /// Arguments passed to the JavaScript program.
  #[arg(
    value_name = "ARGS",
    trailing_var_arg = true,
    allow_hyphen_values = true
  )]
  pub(crate) args: Vec<String>,
}

/// Arguments for TypeScript project checking.
#[derive(Args, Debug)]
pub(crate) struct CheckArgs {
  /// Source files to check. The current directory is scanned when omitted.
  #[arg(value_name = "FILE")]
  pub(crate) paths: Vec<PathBuf>,

  /// Check a project directory or the directory containing a tsconfig file.
  #[arg(short, long, value_name = "PROJECT")]
  pub(crate) project: Option<PathBuf>,

  /// Enable strict checking, including implicit-any diagnostics.
  #[arg(long, action)]
  pub(crate) strict: bool,
}

/// Arguments for machine-readable editor analysis.
#[derive(Args, Debug)]
pub(crate) struct AnalyzeArgs {
  /// Source file to analyze, or `-` to read source from standard input.
  #[arg(value_name = "FILE", required_unless_present = "server")]
  pub(crate) file: Option<PathBuf>,

  /// Serve newline-delimited analysis requests over stdin/stdout.
  #[arg(long, action)]
  pub(crate) server: bool,

  /// Filename used to select syntax and module semantics for standard input.
  #[arg(long, value_name = "PATH")]
  pub(crate) path: Option<PathBuf>,

  /// Enable strict checking, including implicit-any diagnostics.
  #[arg(long, action)]
  pub(crate) strict: bool,
}

/// Normalized execution options shared by legacy and explicit invocations.
#[derive(Debug)]
pub(crate) struct ExecutionArgs {
  pub(crate) file: Option<PathBuf>,
  pub(crate) test262_harness: bool,
  pub(crate) module: bool,
  pub(crate) args: Vec<String>,
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn parses_legacy_file_invocation() {
    let cli = Cli::try_parse_from(["blitz", "program.js", "one"]).expect("parse");

    assert!(cli.command.is_none());
    assert_eq!(cli.file, Some(PathBuf::from("program.js")));
    assert_eq!(cli.args, ["one"]);
    assert_eq!(cli.node_compat, NodeCompatArg::Core);
  }

  #[test]
  fn parses_node_compatibility_profile() {
    for (value, expected) in [
      ("off", NodeCompatArg::Off),
      ("core", NodeCompatArg::Core),
      ("tooling", NodeCompatArg::Tooling),
      ("full", NodeCompatArg::Full),
    ] {
      let cli = Cli::try_parse_from(["blitz", "--node-compat", value]).expect("parse");
      assert_eq!(cli.node_compat, expected);
    }

    let cli = Cli::try_parse_from(["blitz", "run", "--node-compat", "tooling", "program.js"])
      .expect("parse explicit run");
    assert_eq!(cli.node_compat, NodeCompatArg::Tooling);
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
}
