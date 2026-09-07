// SPDX-License-Identifier: MPL-2.0
//! Denko: **a host-neutral ECMAScript engine in Rust**.
//!
//! # Architecture
//!
//! This crate provides the execution engine for ECMAScript code:
//!
//! - **Kernel (Rust)** - The bare-metal runtime layer containing:
//!   - [`lexer`] - Tokenizes source code into tokens
//!   - [`parser`] - Parses tokens into AST (using oxc_ast)
//!   - [`compiler`] - Compiles AST into bytecode instructions
//!   - [`bytecode`] - Defines VM opcodes and instruction encoding
//!   - [`vm`] - bytecode interpreter and runtime
//!   - [`value`] - Runtime value types (numbers, strings, objects, closures)
//!   - [`errors`] - Error types for all VM operations
//!   - [`trace`] - Execution tracing and debugging utilities
//!
//! # Module Overview
//!
//! | Module | Purpose |
//! |--------|---------|
//! | [`ast`] | Re-export of oxc_ast types for AST representation |
//! | [`bytecode`] | VM instruction set and bytecode encoding/decoding |
//! | [`compiler`] | AST-to-bytecode compilation |
//! | [`errors`] | Error types for parsing, compilation, and runtime |
//! | [`lexer`] | Source tokenization |
//! | [`parser`] | Token-to-AST parsing |
//! | [`trace`] | Debug tracing and logging |
//! | [`mod@typecheck`] | TypeScript type checking integration |
//! | [`value`] | Runtime value system |
//! | [`vm`] | Bytecode interpreter and execution engine |
//!
//! # Usage
//!
//! ```no_run
//! use denko::Vm;
//!
//! // Example: construct a VM with an empty globals map. Compilation APIs are
//! // available via the `compiler`/`parser` modules; this snippet shows VM usage.
//! use std::rc::Rc;
//! use std::cell::RefCell;
//! use denko::value::ObjectData;
//! let globals = Rc::new(RefCell::new(ObjectData::new()));
//! let mut vm = Vm::new(globals);
//! // vm.run(&bytecode)?; // run compiled bytecode when available
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! See individual module documentation for detailed API usage.

pub mod analysis;
pub mod binding;
pub mod bytecode;
pub mod bytecode_bundle;
pub mod closure_environment;
mod collection_storage;
pub mod compiler;
pub mod ecmascript;
pub mod environment;
pub mod errors;
pub mod gc;
pub mod inspect;
pub mod lexer;
pub mod module;
pub mod parser;
pub mod prelinked;
pub mod prelinked_bundle;
pub mod realm;
pub mod resolver;
pub mod runtime_utils;
/// Test262 host support helpers shared by CLI/runner integrations.
pub mod test262_support;
pub mod trace;
pub mod typecheck;
pub mod value;
pub mod vm;

#[cfg(test)]
mod ecmascript_correctness_tests;

pub use binding::*;
pub use bytecode::*;
pub use compiler::*;
pub use denko_macros::js;
pub use errors::*;
pub use lexer::*;
pub use module::*;
pub use oast as ast;
pub use oxc_ast::ast as oast;
pub use parser::*;
pub use prelinked::*;
pub use prelinked_bundle::{
  PRELINKED_GRAPH_WIRE_VERSION, PrelinkedGraphCodecError, decode as decode_prelinked_graph,
  encode as encode_prelinked_graph,
};
pub use realm::*;
pub use resolver::*;
pub use trace::*;
pub use typecheck::{check as typecheck, type_to_string as type_to_string_display};
pub use value::*;
pub use vm::*;
