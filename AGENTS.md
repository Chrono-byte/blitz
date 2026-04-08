# Repository Guidelines

## Project Structure & Module Organization
- `src/lib.rs` is the library entrypoint; `src/main.rs` provides the CLI.
- Core VM and bytecode live in `src/vm/`, `src/bytecode.rs`, `src/value.rs`, and `src/compiler/`.
- ECMAScript builtins and semantics are under `src/ecmascript/`.
- Local Test262 integration lives in `src/bin/test262.rs`; AST dumping is in `src/bin/ast_dump.rs`.
- Design notes and work plans live in `docs/`; Test262 is vendored in `test262/`.

## Build, Test, and Development Commands
- `cargo build` compiles the crate.
- `cargo test` runs the Rust unit test suite.
- `cargo test <name> --lib` runs a focused unit test, e.g. `cargo test comparison_branches_use_compare_jump --lib`.
- `cargo run --bin test262 -- --filter <path> --limit 1 --jobs 1` runs a targeted Test262 case.
- `cargo run --bin ast_dump -- <args>` is useful for parser or AST inspection.

## Coding Style & Naming Conventions
- Use standard Rust formatting: `cargo fmt` before committing.
- Keep imports grouped and prefer small helper functions over deep inline logic.
- Use `snake_case` for functions, modules, and file names; `UpperCamelCase` for types and enums.
- Match the surrounding code’s error handling style: return `Result` with `RuntimeError` or `CompilerError` instead of panicking in hot paths.

## Testing Guidelines
- Add unit tests next to the code they exercise, usually in the same module behind `#[cfg(test)]`.
- Prefer focused tests that cover opcode layout, stack behavior, and bytecode patching.
- For Test262 regressions, add or rerun a narrow filter first before broader suite runs.

## Commit & Pull Request Guidelines
- Commit messages in this repo are short, imperative, and specific, e.g. `Add inline cache for property gets`.
- Keep commits focused on one behavior change.
- PRs should include a short summary, the commands you ran, and any failing or intentionally deferred cases.
- If a change affects bytecode, call out compatibility risk and any tests that prove the new layout is safe.

## Agent Notes
- Avoid rewriting unrelated user changes.
- Prefer `apply_patch` for edits.
- When touching bytecode or VM paths, verify with a targeted Rust test and at least one relevant Test262 case.
- If you add, remove, or append an opcode, update `OP_COUNT` and every place that depends on opcode width or layout: bytecode scanners, disassemblers, hotpatch helpers, and runtime dispatch.

## VM Architecture Notes
- The VM is stack-based. Many bugs show up as stack imbalance, so check the exact push/pop count before changing control flow or call lowering.
- Property access has a per-site get-property IC in `Vm` state. When debugging lookup regressions, check both the cache key and the generation invalidation path in `set_property_tracked()`.
- Dense array access now has numeric-index fast paths. Prefer `array_index_from_value()` and direct dense-element helpers instead of stringifying indices in hot loops.
- `CompareJumpIfFalse` uses a 4-byte layout: opcode, compare-op byte, then a signed `i16` offset. Keep compiler patching, runtime decoding, and hotpatch helpers aligned.
- `TSBLITZ_TRACE=1` enables VM tracing. Use it to inspect instruction windows, stack size, and the current frame when a runtime error is hard to reproduce.
- When a Test262 case fails after a bytecode change, rerun one narrow case first, then expand. Many regressions are layout or patching bugs rather than semantic JS failures.
