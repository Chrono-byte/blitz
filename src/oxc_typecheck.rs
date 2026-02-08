//! Lightweight placeholder typechecker using oxc components where possible.
//!
//! This module provides a small, well-typed surface the rest of the crate can
//! depend on while a more complete integration with `oxc_ast`/`oxc_parser` is
//! developed. The focus here is correctness of types and minimal behaviour.

// ParserError from crate::errors is not required in this module; do not import it here.
use crate::ast::*;
use crate::parser::Parser as BridgeParser;
use oxc_allocator::Allocator; // allocator kept for future parser work
use oxc_parser::{ParseOptions, Parser as OxcParser};
use oxc_span::{SourceType, Span};
// Note: avoid glob-importing `oxc_ast::ast` to keep namespace explicit. We
// only use specific oxc_span helpers (LineIndex) directly here for now.
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::Path;

/// Severity levels for type checking issues.
///
/// These mirror common diagnostic severities (error, warning, info, hint)
/// and are used to classify reported TypeIssue entries.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum IssueSeverity {
    /// An error that should be fixed.
    Error,
    /// A warning indicating a potential problem.
    Warning,
    /// Informational message.
    Info,
    /// A hint or suggestion.
    Hint,
}

/// A single diagnostic produced by the typechecker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TypeIssue {
    /// Path to the file containing the issue.
    pub file: String,

    /// 0-based start line of the issue.
    pub line: usize,

    /// 0-based start column of the issue.
    pub column: usize,

    /// 0-based end line of the issue.
    pub end_line: usize,

    /// 0-based end column of the issue.
    pub end_column: usize,

    /// Diagnostic severity.
    pub severity: IssueSeverity,

    /// Short machine-readable code for the issue (e.g. "PARSE_ERROR").
    pub code: String,

    /// Human-readable message describing the problem.
    pub message: String,

    /// Optional suggested fix or guidance.
    pub suggestion: Option<String>,

    /// Optional rule or category for this issue.
    pub rule: Option<String>,
}

/// Configuration options controlling the behaviour of the typechecker.
#[derive(Debug, Clone)]
pub struct TypecheckConfig {
    /// Enable strict mode (more rigorous checks).
    pub strict_mode: bool,

    /// Require explicit type annotations where appropriate.
    pub require_annotations: bool,

    /// Treat errors as warnings to avoid failing CI/flows.
    pub warnings_only: bool,
}

impl Default for TypecheckConfig {
    fn default() -> Self {
        Self {
            strict_mode: true,
            require_annotations: true,
            warnings_only: false,
        }
    }
}

/// Built-in JavaScript types modelled by the lightweight typechecker.
#[derive(Debug, Clone, PartialEq, Hash, Eq, Serialize, Deserialize)]
pub enum BuiltinType {
    /// `undefined`
    Undefined,
    /// `null`
    Null,
    /// `boolean`
    Boolean,
    /// `number`
    Number,
    /// `string`
    String,
    /// `symbol`
    Symbol,
    /// `bigint`
    BigInt,
    /// Generic object
    Object,
    /// Function type
    Function,
    /// Array type
    Array,
    /// Promise
    Promise,
    /// Map
    Map,
    /// Set
    Set,
    /// RegExp
    RegExp,
    /// Date
    Date,
    /// Error
    Error,
    /// TypeError
    TypeError,
    /// RangeError
    RangeError,
    /// SyntaxError
    SyntaxError,
    /// JSON global
    JSON,
    /// Math global
    Math,
    /// `void`
    Void,
    /// `never`
    Never,
    /// `any`
    Any,
    /// Unknown/unspecified
    Unknown,
}

impl fmt::Display for BuiltinType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BuiltinType::Undefined => write!(f, "undefined"),
            BuiltinType::Null => write!(f, "null"),
            BuiltinType::Boolean => write!(f, "boolean"),
            BuiltinType::Number => write!(f, "number"),
            BuiltinType::String => write!(f, "string"),
            BuiltinType::Symbol => write!(f, "symbol"),
            BuiltinType::BigInt => write!(f, "bigint"),
            BuiltinType::Object => write!(f, "object"),
            BuiltinType::Function => write!(f, "function"),
            BuiltinType::Array => write!(f, "Array"),
            BuiltinType::Promise => write!(f, "Promise"),
            BuiltinType::Map => write!(f, "Map"),
            BuiltinType::Set => write!(f, "Set"),
            BuiltinType::RegExp => write!(f, "RegExp"),
            BuiltinType::Date => write!(f, "Date"),
            BuiltinType::Error => write!(f, "Error"),
            BuiltinType::TypeError => write!(f, "TypeError"),
            BuiltinType::RangeError => write!(f, "RangeError"),
            BuiltinType::SyntaxError => write!(f, "SyntaxError"),
            BuiltinType::JSON => write!(f, "JSON"),
            BuiltinType::Math => write!(f, "Math"),
            BuiltinType::Void => write!(f, "void"),
            BuiltinType::Never => write!(f, "never"),
            BuiltinType::Any => write!(f, "any"),
            BuiltinType::Unknown => write!(f, "unknown"),
        }
    }
}

/// Simplified, serializable type representation used by this module.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Type {
    /// A builtin type like `number` or `string`.
    Builtin(BuiltinType),

    /// Array of another type.
    Array(Box<Type>),

    /// Function type (placeholder).
    Function, // placeholder

    /// Reference to a named type (e.g. interface or typedef).
    /// Name of the referenced type.
    TypeReference { name: String },

    /// Union of multiple types.
    Union(Vec<Type>),

    /// Generic object type.
    Object,

    /// Unknown type (could not be inferred).
    Unknown,
}

/// Scope containing mappings for variables and named types.
#[derive(Debug, Clone, Default)]
pub struct Scope {
    /// Mapping from variable name to its type.
    pub variables: HashMap<String, Type>,

    /// Mapping from type name to its Type representation.
    pub types: HashMap<String, Type>,
}

/// Aggregated result returned after running the typechecker.
#[derive(Debug, Default)]
pub struct TypecheckResult {
    /// Collected diagnostic issues.
    pub issues: Vec<TypeIssue>,

    /// Number of files processed.
    pub files_checked: usize,

    /// Count of error-severity issues.
    pub errors_count: usize,

    /// Count of warning-severity issues.
    pub warnings_count: usize,

    /// Count of informational issues.
    pub info_count: usize,
}

impl TypecheckResult {
    /// Return true if the result contains any errors.
    pub fn has_errors(&self) -> bool {
        self.errors_count > 0
    }

    /// Return a short summary string of the result counts.
    pub fn summary(&self) -> String {
        format!(
            "Checked {} files: {} errors, {} warnings, {} info",
            self.files_checked, self.errors_count, self.warnings_count, self.info_count
        )
    }
}

/// Main lightweight typechecker used to validate the TypeScript soft layer.
///
/// This is intentionally small and conservative: it primarily parses files,
/// converts parser diagnostics into TypeIssue entries, and provides a place
/// to add semantic checks later.
pub struct SoftLayerTypechecker {
    /// Configuration for the checker.
    config: TypecheckConfig,

    /// Allocator used by oxc when parsing. Kept for potential future use.
    _allocator: Allocator,

    /// Registered builtin types.
    builtin_types: HashSet<BuiltinType>,

    /// Global named types (e.g. `Array`, `String`).
    global_types: HashMap<String, Type>,

    /// Accumulated diagnostics.
    issues: Vec<TypeIssue>,

    /// Number of files processed so far.
    files_processed: usize,
}

impl SoftLayerTypechecker {
    /// Create a new typechecker with default configuration.
    pub fn new() -> Self {
        let mut this = Self {
            config: TypecheckConfig::default(),
            _allocator: Allocator::default(),
            builtin_types: HashSet::new(),
            global_types: HashMap::new(),
            issues: Vec::new(),
            files_processed: 0,
        };
        this.init_builtin_types();
        this.init_global_types();
        this
    }

    /// Create a new typechecker with a custom `TypecheckConfig`.
    pub fn with_config(config: TypecheckConfig) -> Self {
        let mut this = Self {
            config,
            _allocator: Allocator::default(),
            builtin_types: HashSet::new(),
            global_types: HashMap::new(),
            issues: Vec::new(),
            files_processed: 0,
        };
        this.init_builtin_types();
        this.init_global_types();
        this
    }

    fn init_builtin_types(&mut self) {
        self.builtin_types.insert(BuiltinType::Undefined);
        self.builtin_types.insert(BuiltinType::Null);
        self.builtin_types.insert(BuiltinType::Boolean);
        self.builtin_types.insert(BuiltinType::Number);
        self.builtin_types.insert(BuiltinType::String);
        self.builtin_types.insert(BuiltinType::Symbol);
        self.builtin_types.insert(BuiltinType::BigInt);
        self.builtin_types.insert(BuiltinType::Object);
        self.builtin_types.insert(BuiltinType::Function);
        self.builtin_types.insert(BuiltinType::Array);
        self.builtin_types.insert(BuiltinType::Promise);
        self.builtin_types.insert(BuiltinType::Map);
        self.builtin_types.insert(BuiltinType::Set);
        self.builtin_types.insert(BuiltinType::RegExp);
        self.builtin_types.insert(BuiltinType::Date);
        self.builtin_types.insert(BuiltinType::Error);
        self.builtin_types.insert(BuiltinType::TypeError);
        self.builtin_types.insert(BuiltinType::RangeError);
        self.builtin_types.insert(BuiltinType::SyntaxError);
        self.builtin_types.insert(BuiltinType::JSON);
        self.builtin_types.insert(BuiltinType::Math);
    }

    fn init_global_types(&mut self) {
        self.global_types
            .insert("Object".to_string(), Type::Builtin(BuiltinType::Object));
        self.global_types
            .insert("Array".to_string(), Type::Builtin(BuiltinType::Array));
        self.global_types
            .insert("String".to_string(), Type::Builtin(BuiltinType::String));
        self.global_types
            .insert("Number".to_string(), Type::Builtin(BuiltinType::Number));
        self.global_types
            .insert("Boolean".to_string(), Type::Builtin(BuiltinType::Boolean));
        self.global_types
            .insert("Symbol".to_string(), Type::Builtin(BuiltinType::Symbol));
        self.global_types
            .insert("BigInt".to_string(), Type::Builtin(BuiltinType::BigInt));
        self.global_types
            .insert("Function".to_string(), Type::Function);
        self.global_types
            .insert("Promise".to_string(), Type::Builtin(BuiltinType::Promise));
        self.global_types
            .insert("Map".to_string(), Type::Builtin(BuiltinType::Map));
        self.global_types
            .insert("Set".to_string(), Type::Builtin(BuiltinType::Set));
        self.global_types
            .insert("RegExp".to_string(), Type::Builtin(BuiltinType::RegExp));
        self.global_types
            .insert("Date".to_string(), Type::Builtin(BuiltinType::Date));
        self.global_types
            .insert("Error".to_string(), Type::Builtin(BuiltinType::Error));

        let console_fn = Type::Function;
        self.global_types
            .insert("console".to_string(), Type::Object);
        self.global_types
            .insert("console.log".to_string(), console_fn);
    }

    /// Typecheck a single file.
    ///
    /// Currently this runs the Oxc parser and converts parser diagnostics
    /// into `TypeIssue` entries. AST analysis and inference are TODOs for
    /// future phases.
    pub fn check_file(&mut self, file_path: &Path) -> Result<(), crate::errors::TypeCheckerError> {
        if !file_path.exists() {
            return Err(crate::errors::TypeCheckerError::from(format!(
                "File not found: {}",
                file_path.display()
            )));
        }

        let source = std::fs::read_to_string(file_path).map_err(|e| {
            crate::errors::TypeCheckerError::from(format!(
                "Failed to read file {}: {}",
                file_path.display(),
                e
            ))
        })?;

        // Run Oxc parser in TypeScript mode and collect parse diagnostics.
        let allocator = Allocator::default();
        let ret = OxcParser::new(&allocator, &source, SourceType::ts())
            .with_options(ParseOptions {
                parse_regular_expression: true,
                ..ParseOptions::default()
            })
            .parse();

        if !ret.errors.is_empty() {
            for e in ret.errors {
                // Include source context in the formatted message when possible.
                let msg = e.with_source_code(source.clone());
                self.add_issue(
                    IssueSeverity::Error,
                    "PARSE_ERROR",
                    &format!("{:?}", msg),
                    Some(Span::default()),
                    None,
                    file_path,
                    Some(&source),
                );
            }
        }

        // Prefer using the parser return's `program` when available. If the
        // parser returned a Module, iterate its top-level items and emit a
        // `DEBUGGER_STATEMENT` warning when the slice of source covered by an
        // item's span contains the `debugger` keyword. Otherwise fall back to
        // a source scan to preserve behaviour on unknown program variants.
        // The parser return `ret` contains `program` which is the oxc AST
        // Program. We do not analyze oxc AST directly here; instead we use the
        // bridge conversion to our local AST. Remove heuristic checks and rely
        // on the bridge's `Program` for further analysis.
        match BridgeParser::parse_source(&source) {
            Ok(program) => {
                // Basic semantic checks over the local bridge AST.
                // - Warn when functions or params lack return/type annotations (when configured)
                // - Warn when variable declarations lack a type annotation (when configured)
                self.check_program(&program, file_path, &source);
            }
            Err(e) => {
                self.add_issue(
                    IssueSeverity::Error,
                    "ESTREE_CONVERT",
                    &format!("{}", e.message),
                    None,
                    None,
                    file_path,
                    Some(&source),
                );
            }
        }

        // Count the file as processed regardless of parse/convert errors so callers see progress.
        self.files_processed += 1;
        Ok(())
    }

    // Walk a parsed Program and run simple, conservative checks.
    fn check_program(&mut self, program: &Program, file_path: &Path, source: &str) {
        for stmt in &program.body {
            self.check_statement(stmt, file_path, source);
        }
    }

    fn check_statement(&mut self, stmt: &Statement, file_path: &Path, source: &str) {
        match stmt {
            Statement::FunctionDecl(fd) => {
                if self.config.require_annotations && fd.return_type.is_none() {
                    self.add_issue(
                        IssueSeverity::Warning,
                        "MISSING_RETURN_ANNOTATION",
                        &format!("Function '{}' is missing a return type annotation", fd.name),
                        None,
                        None,
                        file_path,
                        Some(source),
                    );
                }
                for p in &fd.params {
                    if self.config.require_annotations && p.type_ann.is_none() {
                        self.add_issue(
                            IssueSeverity::Warning,
                            "MISSING_PARAM_TYPE",
                            &format!(
                                "Parameter '{}' in function '{}' is missing a type annotation",
                                p.name, fd.name
                            ),
                            None,
                            None,
                            file_path,
                            Some(source),
                        );
                    }
                }
                // Recurse into function body
                for s in &fd.body.body {
                    self.check_statement(s, file_path, source);
                }
            }
            Statement::VarDecl(v) | Statement::Declare(v) => {
                if self.config.require_annotations && v.type_ann.is_none() {
                    self.add_issue(
                        IssueSeverity::Warning,
                        "MISSING_VAR_TYPE",
                        &format!("Variable '{}' is missing a type annotation", v.name),
                        None,
                        None,
                        file_path,
                        Some(source),
                    );
                }
                if let Some(init) = &v.init {
                    self.check_expr(init, file_path, source);
                }
            }
            Statement::Block(b) => {
                for s in &b.body {
                    self.check_statement(s, file_path, source);
                }
            }
            Statement::ExpressionStatement(es) => {
                self.check_expr(&es.expression, file_path, source);
            }
            Statement::If(i) => {
                self.check_expr(&i.test, file_path, source);
                self.check_statement(&i.consequent, file_path, source);
                if let Some(alt) = &i.alternate {
                    self.check_statement(alt, file_path, source);
                }
            }
            Statement::While(w) => {
                self.check_expr(&w.test, file_path, source);
                self.check_statement(&w.body, file_path, source);
            }
            Statement::Return(r) => {
                if let Some(arg) = &r.argument {
                    self.check_expr(arg, file_path, source);
                }
            }
            Statement::Try(t) => {
                for s in &t.block.body {
                    self.check_statement(s, file_path, source);
                }
                if let Some((_, b)) = &t.handler {
                    for s in &b.body {
                        self.check_statement(s, file_path, source);
                    }
                }
                if let Some(f) = &t.finalizer {
                    for s in &f.body {
                        self.check_statement(s, file_path, source);
                    }
                }
            }
            _ => {}
        }
    }

    fn check_expr(&mut self, expr: &Expr, file_path: &Path, source: &str) {
        match expr {
            Expr::CallExpr(c) => {
                self.check_expr(&c.callee, file_path, source);
                for a in &c.args {
                    self.check_expr(a, file_path, source);
                }
            }
            Expr::BinaryExpr(b) => {
                self.check_expr(&b.left, file_path, source);
                self.check_expr(&b.right, file_path, source);
            }
            Expr::UnaryExpr(u) => {
                self.check_expr(&u.argument, file_path, source);
            }
            Expr::MemberExpr(m) => {
                self.check_expr(&m.object, file_path, source);
                self.check_expr(&m.property, file_path, source);
            }
            Expr::ArrayExpr(a) => {
                for e in &a.elements {
                    self.check_expr(e, file_path, source);
                }
            }
            Expr::ObjectExpr(o) => {
                for p in &o.properties {
                    self.check_expr(&p.value, file_path, source);
                }
            }
            Expr::FunctionExpr(f) => {
                if self.config.require_annotations && f.return_type.is_none() {
                    self.add_issue(
                        IssueSeverity::Warning,
                        "MISSING_RETURN_ANNOTATION",
                        "Function expression missing return type annotation",
                        None,
                        None,
                        file_path,
                        Some(source),
                    );
                }
                for p in &f.params {
                    if self.config.require_annotations && p.type_ann.is_none() {
                        self.add_issue(
                            IssueSeverity::Warning,
                            "MISSING_PARAM_TYPE",
                            &format!("Parameter '{}' in function expression is missing a type annotation", p.name),
                            None,
                            None,
                            file_path,
                            Some(source),
                        );
                    }
                }
                for s in &f.body.body {
                    self.check_statement(s, file_path, source);
                }
            }
            _ => {}
        }
    }

    /// Typecheck all `.ts` files in a directory recursively and collect results.
    pub fn check_directory(
        &mut self,
        dir: &Path,
    ) -> Result<TypecheckResult, crate::errors::TypeCheckerError> {
        if !dir.exists() || !dir.is_dir() {
            return Err(crate::errors::TypeCheckerError::from(format!(
                "Directory not found: {}",
                dir.display()
            )));
        }

        let mut paths: Vec<std::path::PathBuf> = Vec::new();
        self.collect_ts_files(dir, &mut paths);

        for path in paths {
            if let Err(e) = self.check_file(&path) {
                self.add_issue(
                    IssueSeverity::Error,
                    "FILE_ERROR",
                    &format!("Failed to typecheck file: {}", e.message),
                    None,
                    None,
                    &path,
                    None,
                );
            }
        }

        Ok(self.build_result())
    }

    // Recursively collect .ts (non-d.ts) files under `dir`.
    fn collect_ts_files(&self, dir: &Path, paths: &mut Vec<std::path::PathBuf>) {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
                    if name != Some("node_modules".to_string()) && name != Some(".git".to_string())
                    {
                        self.collect_ts_files(&path, paths);
                    }
                } else if let Some(ext) = path.extension().and_then(|e| e.to_str())
                    && ext == "ts"
                    && !path.to_string_lossy().ends_with(".d.ts")
                {
                    paths.push(path);
                }
            }
        }
    }

    // Build a TypecheckResult from the accumulated internal state.
    fn build_result(&mut self) -> TypecheckResult {
        let mut result = TypecheckResult::default();
        result.files_checked = self.files_processed;
        result.issues = self.issues.clone();

        for issue in &self.issues {
            match issue.severity {
                IssueSeverity::Error => result.errors_count += 1,
                IssueSeverity::Warning => result.warnings_count += 1,
                IssueSeverity::Info => result.info_count += 1,
                IssueSeverity::Hint => {}
            }
        }

        result
    }

    // Helper to push a new TypeIssue into internal storage.
    fn add_issue(
        &mut self,
        severity: IssueSeverity,
        code: &str,
        message: &str,
        span_opt: Option<Span>,
        suggestion: Option<&str>,
        file_path: &Path,
        source_text: Option<&str>,
    ) {
        let suggestion = suggestion.map(ToString::to_string);

        let severity = if self.config.warnings_only {
            match severity {
                IssueSeverity::Error => IssueSeverity::Warning,
                _ => severity,
            }
        } else {
            severity
        };

        // Fallback: derive line/column by scanning source text. This avoids
        // depending on oxc_span helpers that may not be exported in all
        // versions. Lines and columns are 0-based here to match TypeIssue.
        let (line, column, end_line, end_column) =
            if let (Some(span), Some(src)) = (span_opt, source_text) {
                let start = span.start as usize;
                let end = (span.end as usize).min(src.len());
                let mut sline = 0usize;
                let mut scol = 0usize;
                let mut byte_idx = 0usize;
                for (i, ch) in src.char_indices() {
                    if i >= start {
                        break;
                    }
                    if ch == '\n' {
                        sline += 1;
                        scol = 0;
                        byte_idx = i + ch.len_utf8();
                    } else {
                        scol += 1;
                        byte_idx = i + ch.len_utf8();
                    }
                }
                let mut eline = sline;
                let mut ecol = scol;
                for (i, ch) in src[byte_idx..].char_indices() {
                    let idx = byte_idx + i;
                    if idx >= end {
                        break;
                    }
                    if ch == '\n' {
                        eline += 1;
                        ecol = 0;
                    } else {
                        ecol += 1;
                    }
                }
                (sline, scol, eline, ecol)
            } else {
                (0usize, 0usize, 0usize, 0usize)
            };

        self.issues.push(TypeIssue {
            file: file_path.display().to_string(),
            line,
            column,
            end_line,
            end_column,
            severity,
            code: code.to_string(),
            message: message.to_string(),
            suggestion,
            rule: Some(code.to_string()),
        });
    }

    /// Return a slice of collected issues.
    pub fn get_issues(&self) -> &[TypeIssue] {
        &self.issues
    }

    /// Serialize collected issues as pretty JSON.
    pub fn issues_as_json(&self) -> String {
        serde_json::to_string_pretty(&self.issues).unwrap_or_default()
    }

    /// Generate a human-readable Markdown report summarizing issues.
    pub fn generate_report(&self) -> String {
        let mut report = String::new();

        report.push_str("# Soft Layer Type Analysis Report\n\n");

        let errors: Vec<&TypeIssue> = self
            .issues
            .iter()
            .filter(|i| i.severity == IssueSeverity::Error)
            .collect();
        let warnings: Vec<&TypeIssue> = self
            .issues
            .iter()
            .filter(|i| i.severity == IssueSeverity::Warning)
            .collect();
        let infos: Vec<&TypeIssue> = self
            .issues
            .iter()
            .filter(|i| i.severity == IssueSeverity::Info)
            .collect();

        report.push_str(&format!("**Files Processed:** {}\n", self.files_processed));
        report.push_str(&format!("**Total Issues:** {}\n", self.issues.len()));
        report.push_str(&format!("- **Errors:** {}\n", errors.len()));
        report.push_str(&format!("- **Warnings:** {}\n", warnings.len()));
        report.push_str(&format!("- **Info:** {}\n", infos.len()));
        report.push('\n');

        if !errors.is_empty() {
            report.push_str("## Errors\n\n");
            for issue in errors {
                report.push_str(&format!("### {}: {}\n", issue.code, issue.message));
                report.push_str(&format!("**File:** {}\n", issue.file));
                report.push('\n');
            }
        }

        report
    }
}

impl Default for SoftLayerTypechecker {
    fn default() -> Self {
        Self::new()
    }
}

/// Utility function to run the typechecker on a directory.
///
/// Returns a `TypecheckResult` summarizing found issues.
pub fn check_soft_layer(
    dir: &Path,
    config: Option<TypecheckConfig>,
) -> Result<TypecheckResult, crate::errors::TypeCheckerError> {
    let config = config.unwrap_or_default();
    let mut typechecker = SoftLayerTypechecker::with_config(config);
    typechecker.check_directory(dir)
}

/// Utility function to run the typechecker on a single file.
///
/// Returns a `TypecheckResult` for the file.
pub fn check_file(
    file: &Path,
    config: Option<TypecheckConfig>,
) -> Result<TypecheckResult, crate::errors::TypeCheckerError> {
    let config = config.unwrap_or_default();
    let mut typechecker = SoftLayerTypechecker::with_config(config);
    typechecker.check_file(file)?;
    Ok(typechecker.build_result())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_builtin_types_registration() {
        let typechecker = SoftLayerTypechecker::new();
        assert!(typechecker.builtin_types.contains(&BuiltinType::Number));
        assert!(typechecker.builtin_types.contains(&BuiltinType::String));
    }

    #[test]
    fn test_global_types_registration() {
        let typechecker = SoftLayerTypechecker::new();
        assert!(typechecker.global_types.contains_key("Object"));
        assert!(typechecker.global_types.contains_key("Array"));
        assert!(typechecker.global_types.contains_key("console"));
    }

    #[test]
    fn test_check_file_counts() {
        let mut typechecker = SoftLayerTypechecker::new();
        let tmp = std::env::temp_dir().join("tsblitz_dummy.ts");
        std::fs::write(&tmp, "const x = 1;").unwrap();
        typechecker.check_file(&tmp).unwrap();
        assert_eq!(typechecker.files_processed, 1);
        let _ = std::fs::remove_file(tmp);
    }
}
