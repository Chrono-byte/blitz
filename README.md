Blitz
===================================

What this project is
- Blitz is a small TypeScript runtime: a Rust kernel (parser, bytecode compiler, VM, GC) that runs a TypeScript "soft" layer which provides the JavaScript-visible runtime surface and standard library.
- The TypeScript layer (stored in `soft/` and conceptually referred to as "blitzlib") is the canonical host prelude + stdlib: it installs globals, prototypes, std APIs and implements higher-level semantics in TypeScript so behavior is auditable and easy to iterate.

Repo layout (high level)
- `src/` — Rust kernel: lexer, parser, compiler, VM, native hooks. Entry point is `src/main.rs`.
- `soft/` — TypeScript host prelude and standard library (prelude at `soft/prelude.ts`, runtime helpers at `soft/core/`, small stdlib under `soft/std/`).
- `test262/`, `tests/` — tests and conformance harnesses (where present).

Quick start
- Build and run the REPL: `cargo run` (starts the REPL; type `exit` to quit).
- Run a TypeScript file: `cargo run -- path/to/file.ts` (the bundler inlines the soft prelude and executes your entry file in the same VM instance).

How blitzlib and the kernel interact
- The Rust kernel provides a minimal, well-documented native ABI (examples: `__native_print`, `__native_new_object`, and scheduling/file primitives). The kernel enforces low-level invariants (object identity, descriptor semantics, numeric behavior, GC).
- blitzlib (the TypeScript soft layer in `soft/`) implements user-visible builtins, prototypes, stdlib functions and higher-level runtime behavior. blitzlib can only access native capabilities that the kernel deliberately exposes.

Goals and constraints
- Make the TypeScript layer authoritative for user-facing semantics while keeping the Rust kernel minimal, stable, and defensive about inputs crossing the boundary.
- Keep the native surface small and explicit; add natives only when necessary and well-documented.

Where to look first
- `soft/prelude.ts` — bootstraps globals and installs builtins at startup.
- `soft/core/` — runtime scaffolding and types used by the prelude.
- `src/main.rs` — shows how the prelude is bundled and executed and which natives are injected into the VM.
- `ARCHITECTURE.md` — design goals and the VM ↔ blitzlib contract.

Next steps for contributors
1. Read `ARCHITECTURE.md` to understand responsibilities between kernel and blitzlib.
2. Add small, well-tested TypeScript implementations in `soft/` with unit tests under `soft/tests/` when appropriate.
3. When adding native hooks, update the contract in `ARCHITECTURE.md` and document the safety/semantics.

License and governance
- See project root for license and contributor guidelines (if present). Follow the migration roadmap in `ARCHITECTURE.md` for large runtime changes.
