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
#[command(
  name = "blitz",
  version,
  about = "Blitz JavaScript and TypeScript runtime",
  subcommand_precedence_over_arg = true
)]
pub(crate) struct Cli {
  /// Explicit toolchain or execution command.
  #[command(subcommand)]
  pub(crate) command: Option<CliCommand>,

  /// JavaScript file to execute. If omitted the REPL will be started.
  #[arg(value_name = "FILE")]
  pub(crate) file: Option<PathBuf>,

  /// Select the Node.js compatibility profile: off, core, tooling, or full.
  #[arg(long, value_enum, default_value_t = NodeCompatArg::Full, global = true)]
  pub(crate) node_compat: NodeCompatArg,

  /// Change the working directory before project discovery and execution.
  #[arg(long, value_name = "DIRECTORY", global = true)]
  pub(crate) cwd: Option<PathBuf>,

  /// Load environment variables from a file. May be repeated.
  #[arg(
    long,
    value_name = "FILE",
    global = true,
    conflicts_with = "no_env_file"
  )]
  pub(crate) env_file: Vec<PathBuf>,

  /// Disable automatic `.env` file loading.
  #[arg(long, global = true, conflicts_with = "env_file")]
  pub(crate) no_env_file: bool,

  /// Add package export conditions. Comma-separated values are accepted.
  #[arg(long, value_name = "CONDITION", value_delimiter = ',', global = true)]
  pub(crate) conditions: Vec<String>,

  /// Preload a module before evaluating the entry point.
  #[arg(
    short = 'r',
    long,
    visible_aliases = ["require", "import"],
    value_name = "MODULE",
    global = true
  )]
  pub(crate) preload: Vec<PathBuf>,

  /// Start the hosted Blitz REPL instead of executing a positional file.
  #[arg(long, action)]
  pub(crate) repl: bool,

  /// Evaluate JavaScript or TypeScript source.
  #[arg(short = 'e', long, value_name = "SOURCE", conflicts_with = "print")]
  pub(crate) eval: Option<String>,

  /// Evaluate source and print its completion value.
  #[arg(short = 'p', long, value_name = "SOURCE", conflicts_with = "eval")]
  pub(crate) print: Option<String>,

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
  /// Run a package script or JavaScript/TypeScript file.
  Run(RunArgs),
  /// Start the hosted Blitz REPL.
  Repl,
  /// Type-check a project or source files with Blitz's in-process checker.
  Check(CheckArgs),
  /// Print compiler-owned editor analysis as JSON.
  Analyze(AnalyzeArgs),
  /// Precompile deterministic Blitz artifacts.
  Precompile(PrecompileArgs),
}

/// Arguments for explicit file execution.
#[derive(Args, Debug)]
pub(crate) struct RunArgs {
  /// Package script name or JavaScript/TypeScript file to execute.
  #[arg(value_name = "TARGET")]
  pub(crate) file: PathBuf,

  /// Exit successfully when neither a package script nor a source file exists.
  #[arg(long, action)]
  pub(crate) if_present: bool,

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

/// Arguments for artifact precompilation.
#[derive(Args, Debug)]
pub(crate) struct PrecompileArgs {
  /// Artifact to precompile.
  #[command(subcommand)]
  pub(crate) command: PrecompileCommand,
}

/// Precompiled artifact targets.
#[derive(Subcommand, Debug)]
pub(crate) enum PrecompileCommand {
  /// Build the embedded Blitz runtime bundle.
  Runtime(PrecompileRuntimeArgs),
  /// Build a portable prelinked bundle from a module-project entry point.
  Project(PrecompileProjectArgs),
}

/// Arguments for runtime-bundle precompilation.
#[derive(Args, Debug)]
pub(crate) struct PrecompileRuntimeArgs {
  /// Regenerate in memory and compare with the committed artifact.
  #[arg(long)]
  pub(crate) check: bool,

  /// Override the versioned artifact path.
  #[arg(long)]
  pub(crate) output: Option<PathBuf>,
}

/// Arguments for module-project precompilation.
#[derive(Args, Debug)]
pub(crate) struct PrecompileProjectArgs {
  /// Module entry point whose complete static dependency graph is bundled.
  #[arg(value_name = "ENTRY")]
  pub(crate) entry: PathBuf,

  /// Output bundle path. Defaults to ENTRY with a `.blitzbundle` extension.
  #[arg(short, long, value_name = "FILE")]
  pub(crate) output: Option<PathBuf>,

  /// Override the bundle name discovered from package.json.
  #[arg(long, value_name = "NAME")]
  pub(crate) name: Option<String>,

  /// Override the bundle version discovered from package.json.
  #[arg(long, value_name = "VERSION")]
  pub(crate) version: Option<String>,

  /// Regenerate in memory and compare with the output bundle.
  #[arg(long)]
  pub(crate) check: bool,

  /// Exclude Blitz's embedded Effect, stdlib, and Node-compatible modules.
  #[arg(long)]
  pub(crate) no_runtime: bool,
}

/// Normalized execution options shared by legacy and explicit invocations.
#[derive(Debug)]
pub(crate) struct ExecutionArgs {
  pub(crate) file: Option<PathBuf>,
  pub(crate) preloads: Vec<PathBuf>,
  pub(crate) env_files: Vec<PathBuf>,
  pub(crate) automatic_env: bool,
  pub(crate) conditions: Vec<String>,
  pub(crate) explicit_run: bool,
  pub(crate) if_present: bool,
  pub(crate) eval: Option<String>,
  pub(crate) print: bool,
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
    assert_eq!(cli.node_compat, NodeCompatArg::Full);
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
    let cli = Cli::try_parse_from([
      "blitz",
      "run",
      "--check",
      "--if-present",
      "program.ts",
      "--",
      "one",
    ])
    .expect("parse");

    assert!(matches!(cli.command, Some(CliCommand::Run(_))));
    if let Some(CliCommand::Run(run)) = cli.command {
      assert!(run.check);
      assert!(run.if_present);
      assert_eq!(run.file, PathBuf::from("program.ts"));
      assert_eq!(run.args, ["one"]);
    }
  }

  #[test]
  fn parses_explicit_repl_command() {
    let cli = Cli::try_parse_from(["blitz", "repl"]).expect("parse");
    assert!(matches!(cli.command, Some(CliCommand::Repl)));
  }

  #[test]
  fn parses_runtime_precompilation() {
    let cli = Cli::try_parse_from([
      "blitz",
      "precompile",
      "runtime",
      "--check",
      "--output",
      "runtime.blitzbundle",
    ])
    .expect("parse");

    let Some(CliCommand::Precompile(precompile)) = cli.command else {
      panic!("expected precompile command");
    };
    let PrecompileCommand::Runtime(runtime) = precompile.command else {
      panic!("expected runtime target");
    };
    assert!(runtime.check);
    assert_eq!(runtime.output, Some(PathBuf::from("runtime.blitzbundle")));
  }

  #[test]
  fn parses_project_precompilation() {
    let cli = Cli::try_parse_from([
      "blitz",
      "precompile",
      "project",
      "src/main.ts",
      "--output",
      "dist/app.blitzbundle",
      "--name",
      "example",
      "--version",
      "1.2.3",
    ])
    .expect("parse");

    let Some(CliCommand::Precompile(precompile)) = cli.command else {
      panic!("expected precompile command");
    };
    let PrecompileCommand::Project(project) = precompile.command else {
      panic!("expected project target");
    };
    assert_eq!(project.entry, PathBuf::from("src/main.ts"));
    assert_eq!(project.output, Some(PathBuf::from("dist/app.blitzbundle")));
    assert_eq!(project.name.as_deref(), Some("example"));
    assert_eq!(project.version.as_deref(), Some("1.2.3"));
    assert!(!project.check);
    assert!(!project.no_runtime);
  }

  #[test]
  fn parses_eval_and_print_modes() {
    let eval = Cli::try_parse_from(["blitz", "-e", "console.log('hello')"]).expect("parse eval");
    assert_eq!(eval.eval.as_deref(), Some("console.log('hello')"));
    assert!(eval.print.is_none());

    let print = Cli::try_parse_from(["blitz", "--print", "1 + 2"]).expect("parse print");
    assert_eq!(print.print.as_deref(), Some("1 + 2"));
    assert!(print.eval.is_none());

    assert!(Cli::try_parse_from(["blitz", "-e", "1", "-p", "2"]).is_err());
  }

  #[test]
  fn parses_project_context_options() {
    let cli = Cli::try_parse_from([
      "blitz",
      "--cwd",
      "project",
      "--env-file",
      "base.env",
      "--env-file",
      "local.env",
      "--conditions",
      "development,custom",
      "app.ts",
    ])
    .expect("parse project options");

    assert_eq!(cli.cwd, Some(PathBuf::from("project")));
    assert_eq!(
      cli.env_file,
      [PathBuf::from("base.env"), PathBuf::from("local.env")]
    );
    assert_eq!(cli.conditions, ["development", "custom"]);
    assert!(Cli::try_parse_from(["blitz", "--env-file", ".env", "--no-env-file"]).is_err());
  }

  #[test]
  fn parses_ordered_preloads_for_runtime_commands() {
    let cli = Cli::try_parse_from([
      "blitz",
      "-r",
      "setup.ts",
      "--import",
      "tracing.ts",
      "run",
      "app.ts",
    ])
    .expect("parse preloads");

    assert_eq!(
      cli.preload,
      [PathBuf::from("setup.ts"), PathBuf::from("tracing.ts")]
    );
  }
}
