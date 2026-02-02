//! Tsblitz VM: **actual runtime in Rust**, **std + more in TypeScript**.
//!
//! - **Rust (this crate)** = the runtime: lexer, parser, bytecode compiler, bytecode
//!   interpreter, heap/stack, opcodes. All execution lives here. Only minimal
//!   primitives (e.g. a single native I/O hook) are implemented in Rust.
//!
//! - **TypeScript (soft layer)** = everything above that: full standard library
//!   (console, Array, Object, String, …) and any extra runtime APIs. All written
//!   in TS and executed by this VM. No high-level APIs are implemented in Rust.

pub mod ast;
pub mod bundler;
pub mod bytecode;
pub mod compiler;
pub mod lexer;
pub mod parser;
pub mod trace;
pub mod typecheck;
pub mod value;
pub mod vm;

pub use ast::*;
pub use bundler::*;
pub use bytecode::*;
pub use compiler::*;
pub use lexer::*;
pub use parser::*;
pub use typecheck::{check as typecheck, type_to_string as type_to_string_display};
pub use value::*;
pub use vm::*;
