# AGENTS.md - tsblitz Development Guide

## Project Overview

tsblitz is a TypeScript VM with a Rust kernel (parser, bytecode compiler, VM, GC) and a TypeScript soft layer (blitzlib) that provides the runtime surface. The Rust kernel lives in `src/`, the TypeScript layer in `soft/`.

## Build Commands

### Rust (Kernel)
```bash
# Build release binary
cargo build --release

# Build and run VM with args
cargo run -- <args>

# Build and run REPL
cargo run -- --repl

# Build release and compile TypeScript
npm run build

# Run clippy for linting
cargo clippy

# Run all tests
cargo test

# Run a single test (e.g., named "parser")
cargo test parser

# Run tests with output
cargo test -- --nocapture
```

### TypeScript (Soft Layer)
```bash
# Format code (uses oxfmt)
cd soft && pnpm format

# Lint code (uses oxlint)
cd soft && pnpm lint

# Check types
cd soft && pnpm check-types
```

## Code Style Guidelines

### Rust (Kernel)

**Imports**: Use `pub mod` for module declarations, group by functionality. Re-export public API items at crate root:
```rust
pub mod ast;
pub use ast::*;
```

**Naming Conventions**:
- Modules: `snake_case` (e.g., `lexer`, `bytecode`)
- Types: `PascalCase` (e.g., `Instruction`, `Value`)
- Functions/Variables: `snake_case` (e.g., `parse_expression`, `stack_offset`)
- Constants: `SCREAMING_SNAKE_CASE` for config values
- Avoid repetitive module names in types (e.g., `Vm::VmError` not `VmVmError`)

**Error Handling**:
- Prefer `Result<T, E>` over `unwrap()` and `expect()` in production code
- `unwrap()`/`expect()` are `warn` lints in clippy - minimize use
- Use `?` operator for propagation
- Document error conditions with `#[error]` or comments when using `panic`

**Documentation**:
- Crate-level docs with `//!` at top of `lib.rs`
- Public items should have doc comments
- `missing_docs` lint is `warn` - add docs for public APIs
- `missing_docs_in_private_items` is enabled in clippy.toml

**Memory Safety**:
- Kernel is memory-conscious: `trivial_copy_size_limit = 256`
- `vec_box_size_threshold = 4096` - avoid heap allocations
- `enum_variant-size-threshold = 200` - keep enums compact
- `verbose-bit-mask-threshold = 1` - prefer bit operations

**Complexity Limits** (from clippy.toml):
- `cognitive-complexity-threshold = 10` - keep functions simple
- `too-many-arguments-threshold = 7`
- `too-many-lines-threshold = 50` - prefer short functions

**Tests**:
- `allow-expect-in-tests = false` in clippy.toml
- `allow-unwrap-in-tests = false`
- `allow-panic-in-tests = false`
- Use proper assertions in tests

### TypeScript (Soft Layer)

**Formatting**: Uses `oxfmt` (configured in `.oxfmtrc.json`)

**Linting**: Uses `oxlint` (configured in `.oxlintrc.json`)

**Type Checking**: Uses `tsgo` with `tsconfig.check.json`

**Naming Conventions**:
- Variables/functions: `camelCase`
- Classes/Types: `PascalCase`
- Constants: `UPPER_SNAKE_CASE` or `camelCase` for object constants
- Files: `kebab-case.ts`

**Type Strictness**: `soft/tsconfig.json` has relaxed strictness (`strict: false`) to accommodate the soft layer's unusual patterns (host-backed globals, runtime-defined constructors). New code should aim for reasonable type safety where possible.

**Patterns**:
- Runtime-defined constructors are expected
- Prototype overwrites are permitted
- `noUncheckedIndexedAccess = false`
- `verbatimModuleSyntax = false`

### General

**File Organization**:
- `src/` - Rust kernel modules
- `soft/` - TypeScript prelude, core, and stdlib
- `tests/` - Integration/harness tests (Rust)
- `test262/` - Test262 conformance

**Codebase Structure**:
- Each Rust module in `src/` has a corresponding `.rs` file
- TypeScript modules in `soft/` follow: `prelude.ts` (entry), `core/` (runtime scaffolding), `std/` (stdlib)

**Documentation Files**:
- `ARCHITECTURE.md` - VM ↔ blitzlib contract
- `soft/CONTRACT.md` - Kernel ↔ soft layer interface
- `README.md` - High-level overview

**When Modifying**:
1. Update relevant docs (`ARCHITECTURE.md`, `CONTRACT.md`) when changing the kernel/soft layer contract
2. Add TypeScript tests under `soft/tests/` when adding new stdlib features
3. Document new native hooks with safety/semantics notes
