//! Blitz VM: **TypeScript/JavaScript VM in Rust**. A Rust kernel bootstraps into
//! a TypeScript runtime that provides the standard library and high-level APIs.
//!
//! # Architecture
//!
//! This crate provides the execution engine for JavaScript/TypeScript code:
//!
//! - **Kernel (Rust)** - The bare-metal runtime layer containing:
//!   - [`lexer`] - Tokenizes source code into tokens
//!   - [`parser`] - Parses tokens into AST (using oxc_ast)
//!   - [`compiler`] - Compiles AST into bytecode instructions
//!   - [`bytecode`] - Defines VM opcodes and instruction encoding
//!   - [`vm`] - Stack-based bytecode interpreter with heap/stack management
//!   - [`value`] - Runtime value types (numbers, strings, objects, closures)
//!   - [`errors`] - Error types for all VM operations
//!   - [`trace`] - Execution tracing and debugging utilities
//!
//! - **Soft Layer (TypeScript)** - Lives in the `soft/` directory and contains:
//!   - `blitzlib` - Standard library (console, Array, Object, String, Math, etc.)
//!   - Runtime APIs and host integration
//!   - All written in TS, executed by the Rust VM
//!
//! # Module Overview
//!
//! | Module | Purpose |
//! |--------|---------|
//! | [`ast`] | Re-export of oxc_ast types for AST representation |
//! | (removed) | Entry point bundling was removed; use [`module::ModuleLoader`] |
//! | [`bytecode`] | VM instruction set and bytecode encoding/decoding |
//! | [`compiler`] | AST-to-bytecode compilation |
//! | [`errors`] | Error types for parsing, compilation, and runtime |
//! | [`lexer`] | Source tokenization |
//! | [`parser`] | Token-to-AST parsing |
//! | [`trace`] | Debug tracing and logging |
//! | [`mod@typecheck`] | TypeScript type checking integration |
//! | [`value`] | Runtime value system |
//! | [`vm`] - | Bytecode interpreter and execution engine |
//!
//! # Usage
//!
//! ```no_run
//! use tsblitz::Vm;
//!
//! // Example: construct a VM with an empty globals map. Compilation APIs are
//! // available via the `compiler`/`parser` modules; this snippet shows VM usage.
//! use std::rc::Rc;
//! use std::cell::RefCell;
//! use std::collections::HashMap;
//! let globals = Rc::new(RefCell::new(HashMap::new()));
//! let mut vm = Vm::new(globals);
//! // vm.run(&bytecode)?; // run compiled bytecode when available
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! See individual module documentation for detailed API usage.

// bundler removed; ModuleLoader in `module` replaces previous bundler flows
pub mod bytecode;
pub mod compiler;
pub mod errors;
pub mod lexer;
pub mod module;
pub mod parser;
pub mod resolver;
pub mod runtime_utils;
pub mod trace;
pub mod typecheck;
pub mod value;
pub mod vm;

// Re-export oxc_ast::ast under the `ast` namespace for compatibility.
pub use bytecode::*;
pub use compiler::*;
pub use errors::*;
pub use lexer::*;
pub use module::*;
pub use oast as ast;
pub use oxc_ast::ast as oast;
pub use resolver::*;
// `oxc_typecheck` removed — use `typecheck` module which now provides oxc integrations.
pub use parser::*;
pub use trace::*;
pub use typecheck::{check as typecheck, type_to_string as type_to_string_display};
pub use value::*;
pub use vm::*;
