//! Error types for the tsblitz kernel.
//!
//! This module defines a structured error hierarchy using `thiserror` for
//! ergonomic error handling throughout the VM subsystems.
//!
//! # Error Types
//!
//! | Error | Source | Description |
//! |-------|--------|-------------|
//! | [`ParserError`] | Parser | Lexical and syntactic errors |
//! | [`TypeCheckerError`] | Type checker | Static type checking failures |
//! | [`CompilerError`] | Compiler | Bytecode generation errors |
//! | [`RuntimeError`] | VM | Runtime execution errors |
//! | [`BundlerError`] | Module loader | Module resolution failures (module loader)
//!
//! # Usage
//!
//! ```
//! use tsblitz::errors::{Error, ParserError};
//!
//! fn may_fail() -> Result<(), Error> {
//!     Err(ParserError::from("Unexpected token"))?
//! }
//! ```

use thiserror::Error;

#[derive(Debug, Error)]
/// Top-level error enum for tsblitz.
///
/// This aggregates specific error kinds produced by different subsystems:
/// - `Parser` for lexical/parse errors
/// - `TypeChecker` for static type checking failures
/// - `Compiler` for bytecode/compiler errors
/// - `Runtime` for errors occurring during VM execution
/// - `Bundler` for errors produced by the module loader (ModuleLoader)
pub enum Error {
    /// Error originating from the parser.
    #[error(transparent)]
    Parser(#[from] ParserError),

    /// Error originating from the type checker.
    #[error(transparent)]
    TypeChecker(#[from] TypeCheckerError),

    /// Error originating from the compiler.
    #[error(transparent)]
    Compiler(#[from] CompilerError),

    /// Error originating from the VM/runtime.
    #[error(transparent)]
    Runtime(#[from] RuntimeError),

    /// Error originating from the module loader.
    #[error(transparent)]
    Bundler(#[from] BundlerError),
}

#[derive(Debug, Error)]
#[error("Parser error: {message}")]
/// Parser errors include an error message and an optional source location
/// indicating where the error occurred in the input text.
pub struct ParserError {
    /// Human-readable error message.
    pub message: String,

    /// Optional location of the error in the source (start/end indices, line/column).
    pub source_location: Option<SourceLocation>,
}

#[derive(Debug, Error)]
#[error("Type checker error: {message}")]
/// Errors produced by the static type checker.
pub struct TypeCheckerError {
    /// Human-readable error message produced during type checking.
    pub message: String,

    /// Optional location in source where the problem was found.
    pub source_location: Option<SourceLocation>,
}

#[derive(Debug, Error)]
#[error("Compiler error: {message}")]
/// Errors produced during compilation/bytecode generation.
pub struct CompilerError {
    /// Description of the compiler failure.
    pub message: String,

    /// Optional location in source related to the compiler error.
    pub source_location: Option<SourceLocation>,
}

#[derive(Debug, Error)]
#[error("Runtime error: {message}")]
/// Runtime errors that occur while executing bytecode on the VM.
pub struct RuntimeError {
    /// Error description.
    pub message: String,
}

#[derive(Debug, Error)]
#[error("Module loader error: {message}")]
/// Errors emitted by the module loader when resolving modules.
pub struct BundlerError {
    /// Human-readable module loader error message.
    pub message: String,
}

#[derive(Debug, Clone)]
/// SourceLocation describes a span in the input source text.
pub struct SourceLocation {
    /// Byte index of the start of the span.
    pub start: usize,

    /// Byte index of the end of the span.
    pub end: usize,

    /// 1-based line number where the span starts.
    pub line: u32,

    /// 1-based column number where the span starts.
    pub column: u32,
}

impl From<String> for ParserError {
    fn from(message: String) -> Self {
        Self {
            message,
            source_location: None,
        }
    }
}

impl From<&str> for ParserError {
    fn from(message: &str) -> Self {
        Self::from(message.to_string())
    }
}

impl From<String> for TypeCheckerError {
    fn from(message: String) -> Self {
        Self {
            message,
            source_location: None,
        }
    }
}

impl From<&str> for TypeCheckerError {
    fn from(message: &str) -> Self {
        Self::from(message.to_string())
    }
}

impl From<String> for CompilerError {
    fn from(message: String) -> Self {
        Self {
            message,
            source_location: None,
        }
    }
}

impl From<&str> for CompilerError {
    fn from(message: &str) -> Self {
        Self::from(message.to_string())
    }
}

impl From<String> for RuntimeError {
    fn from(message: String) -> Self {
        Self { message }
    }
}

impl From<&str> for RuntimeError {
    fn from(message: &str) -> Self {
        Self::from(message.to_string())
    }
}

impl From<String> for BundlerError {
    fn from(message: String) -> Self {
        Self { message }
    }
}

impl From<&str> for BundlerError {
    fn from(message: &str) -> Self {
        Self::from(message.to_string())
    }
}
