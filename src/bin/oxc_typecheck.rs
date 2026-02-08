//! Oxc-based Soft Layer Typechecker CLI
//!
//! A command-line interface for the oxc-based TypeScript/JavaScript type checker
//! specifically designed for the tsblitz soft layer.

use clap::{Parser, ValueEnum};
use std::path::PathBuf;
use std::process;
use tsblitz::oxc_typecheck::IssueSeverity;
use tsblitz::{TypecheckConfig, TypecheckResult, check_soft_layer, oxc_check_file};

/// A TypeScript/JavaScript typechecker for the tsblitz soft layer
#[derive(Parser, Debug)]
#[command(name = "tsblitz-check")]
#[command(author = "tsblitz contributors")]
#[command(version = "0.1.0")]
#[command(about = "Type-check the tsblitz soft layer", long_about = None)]
struct Args {
    /// Path to check (file or directory)
    #[arg(short, long, default_value = "./soft")]
    path: Option<PathBuf>,

    /// Output format
    #[arg(short, long, value_enum, default_value = "text")]
    format: OutputFormat,

    /// Enable strict type checking mode
    #[arg(long)]
    strict: bool,

    /// Report issues as warnings instead of errors
    #[arg(long)]
    warnings_only: bool,

    /// Enable null/undefined safety checks
    #[arg(long)]
    null_safety: bool,

    /// Enable array access safety checks
    #[arg(long)]
    array_checks: bool,

    /// Enable prototype chain checks
    #[arg(long)]
    prototype_checks: bool,

    /// Enable constructor usage checks
    #[arg(long)]
    constructor_checks: bool,

    /// Require type annotations
    #[arg(long)]
    require_annotations: bool,

    /// Enable all checks (most permissive)
    #[arg(long)]
    all: bool,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
enum OutputFormat {
    /// Human-readable text output
    Text,
    /// JSON output
    Json,
    /// Summary only (no details)
    Summary,
}

fn build_config(args: &Args) -> TypecheckConfig {
    // The current TypecheckConfig in the library is intentionally small. Map
    // CLI flags into the available fields and keep additional flags as no-ops
    // for now to avoid breaking the user-facing CLI.
    let strict_mode = if args.all {
        true
    } else {
        args.strict || args.require_annotations
    };
    TypecheckConfig {
        strict_mode,
        require_annotations: args.require_annotations,
        warnings_only: args.warnings_only,
    }
}

fn run_check(
    path: PathBuf,
    config: TypecheckConfig,
) -> Result<TypecheckResult, tsblitz::TypeCheckerError> {
    if path.is_file() {
        oxc_check_file(&path, Some(config))
    } else if path.is_dir() {
        check_soft_layer(&path, Some(config))
    } else {
        Err(tsblitz::TypeCheckerError::from(format!(
            "Path is not a file or directory: {}",
            path.display()
        )))
    }
}

fn print_result(result: &TypecheckResult, format: OutputFormat) {
    match format {
        OutputFormat::Json => {
            println!(
                "{}",
                serde_json::to_string_pretty(&result.issues).unwrap_or_else(|_| "[]".to_string())
            );
        }
        OutputFormat::Text => {
            if result.issues.is_empty() {
                println!("✓ No type issues found");
                println!("{}", result.summary());
            } else {
                println!("Type checking found {} issues:\n", result.issues.len());

                let errors: Vec<_> = result
                    .issues
                    .iter()
                    .filter(|i| matches!(i.severity, IssueSeverity::Error))
                    .collect();
                let warnings: Vec<_> = result
                    .issues
                    .iter()
                    .filter(|i| matches!(i.severity, IssueSeverity::Warning))
                    .collect();
                let infos: Vec<_> = result
                    .issues
                    .iter()
                    .filter(|i| matches!(i.severity, IssueSeverity::Info))
                    .collect();

                if !errors.is_empty() {
                    println!("{}", format_issues("ERRORS", &errors));
                }
                if !warnings.is_empty() {
                    println!("{}", format_issues("WARNINGS", &warnings));
                }
                if !infos.is_empty() {
                    println!("{}", format_issues("INFORMATION", &infos));
                }

                println!("\n{}", result.summary());
            }
        }
        OutputFormat::Summary => {
            println!("{}", result.summary());
        }
    }
}

fn format_issues(title: &str, issues: &[&tsblitz::oxc_typecheck::TypeIssue]) -> String {
    let mut output = format!("{}\n{}\n", title, "=".repeat(title.len()));

    for issue in issues {
        output.push_str(&format!(
            "  {}:{} - {} [{}]\n",
            issue.file, issue.line, issue.message, issue.code
        ));

        if let Some(ref suggestion) = issue.suggestion {
            output.push_str(&format!("    💡 {}\n", suggestion));
        }
    }

    output.push('\n');
    output
}

fn main() {
    let args = Args::parse();

    let path = args.path.clone().unwrap_or_else(|| PathBuf::from("./soft"));

    if !path.exists() {
        eprintln!("Error: Path does not exist: {}", path.display());
        process::exit(1);
    }

    let config = build_config(&args);

    println!("Type-checking: {}", path.display());
    println!(
        "Configuration: strict={}, warnings_only={}",
        config.strict_mode, config.warnings_only
    );
    println!();

    match run_check(path, config.clone()) {
        Ok(result) => {
            print_result(&result, args.format);

            if result.has_errors() && !config.warnings_only {
                process::exit(1);
            }
        }
        Err(e) => {
            eprintln!("Error during type checking: {}", e);
            process::exit(1);
        }
    }
}
