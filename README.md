Blitz
=====

Blitz is a compact ECMAScript engine/runtime centered on a minimal Rust kernel that exposes strictly documented native primitives and drives an increasingly capable runtime surface. The Rust base handles parsing, bytecode compilation, the VM/GC, and defensive kernels so behavior stays auditable and easy to iterate.

Key layers
----------

- `src/` — the Rust kernel that ships the lexer, parser, compiler, VM, GC, and the native hooks that expose every capability to the runtime. `src/main.rs` boots the globals, exposes natives, and can run a REPL or execute scripts.
- `examples/` — sample scripts and entry points that exercise how Blitz should be embedded or orchestrated.
- `tests/` and `test262/` — Rust integration tests plus the upstream ECMAScript Test262 harness for the conformance material we exercise manually or with auxiliary runners.

Quick start
-----------

### Prerequisites

- [Rust](https://www.rust-lang.org/tools/install) 1.70+ (cargo build/test/run).

### Building & running

1. `cargo build --release` to produce the optimized binary.
2. Execute Blitz:
   - `cargo run -- path/to/file.ts` (boots the runtime and runs the file).
   - `cargo run -- --repl` (starts the REPL).
   - `cargo run --release -- path/to/file.ts` (runs a release build with a script).

Of course you can also use `cargo run [-- path.ts]` directly if you prefer.

Documentation
-------------

- `docs/VM_V2` — the threading, isolation, and scheduling roadmap for the next-generation single-VM runtime.
- `docs/VM_IPC_SPEC.md` — the kernel ↔ scheduler/NIF IPC contract, framing, and serialization guidance that any new native hook or scheduler needs to follow.
- `test262/` — the upstream ECMAScript conformance suite; read `test262/README.md` for how to interpret and run the harness against blitz.

Testing
-------

- `cargo test` — exercise the Rust units and integration harness (see `tests/prelude_bootstrap.rs` for an example bootstrap run).
- `cargo clippy` and `cargo fmt` keep the kernel code clean.
- Extend `tests/` and `test262/` whenever you add parser/VM surface changes so the regression coverage keeps up with the kernel.
- Use the `test262/` material when you want to validate parser/VM conformance. The tree includes its own runner instructions.

Contributing
------------

1. Read the docs above so you understand the kernel ↔ runtime contract before adding features.
2. Add small, well-tested Rust implementations under `src/` (and `tests/` or `examples/` when appropriate).
3. When expanding native APIs, update the IPC/architecture documents and keep the surface documented so nothing is hidden behind an undocumented native hook.
4. Keep the runtime behavior well-documented and rely on the published kernel APIs rather than introducing omni-directional shims.

License & governance
--------------------

- The `test262/` tree is governed by the Test262 license embedded inside that directory.
