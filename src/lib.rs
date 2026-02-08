//! Blitz VM: **Rust JavaScript VM**. Kernel bootstraps into the TypeScript runtime.
//
// Provide crate-level documentation while keeping linting strict.
//! The tsblitz crate contains the Rust kernel (lexer, parser, VM, compiler)
//! and exposes types used by the TypeScript soft layer. Public items are
//! documented in their respective modules.
//!
//! - **Rust (this crate)** = the runtime: lexer, parser, bytecode compiler, bytecode
//!   interpreter, heap/stack, opcodes. All execution lives here. Only minimal
//!   primitives (e.g. a single native I/O hook) are implemented in Rust.
//!
//! - **TypeScript (soft layer)** = everything above that: full standard library
//!   (console, Array, Object, String, …) and any extra runtime APIs. All written
//!   in TS and executed by this VM. No high-level APIs are implemented in Rust.

// The `ast` module is provided by oxc_ast and re-exported below.
pub mod bundler;
pub mod bytecode;
pub mod compiler;
pub mod errors;
pub mod lexer;
pub mod oxc_typecheck;
pub mod parser;
pub mod trace;
pub mod typecheck;
pub mod value;
pub mod vm;

// Re-export oxc_ast::ast under the `ast` namespace for compatibility.
pub use bundler::*;
pub use bytecode::*;
pub use compiler::*;
pub use errors::*;
pub use lexer::*;
pub use oast as ast;
pub use oxc_ast::ast as oast;
pub use oxc_typecheck::{
    TypeIssue, TypecheckConfig, TypecheckResult, check_file as oxc_check_file, check_soft_layer,
};
pub use parser::*;
pub use trace::*;
pub use typecheck::{check as typecheck, type_to_string as type_to_string_display};
pub use value::*;
pub use vm::*;
