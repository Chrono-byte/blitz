Blitz
=====

Blitz is a compact ECMAScript engine/runtime centered on a minimal Rust kernel that exposes strictly documented native primitives to a self-hosted TypeScript surface (the “soft” layer). The Rust base handles parsing, bytecode compilation, the VM/GC, and defensive kernels, while the TypeScript soft layer (`blitzlib` in `soft/`) installs the visible globals, prototypes, stdlib, and higher-level semantics so behavior stays auditable and easy to iterate.

Key layers
----------

- `src/` — the Rust kernel that ships the lexer, parser, compiler, VM, GC, and the few native hooks (`__native_*`) that blitzlib can call. `src/main.rs` boots the soft prelude, exposes natives, and can run a REPL or a script.
- `soft/` — the TypeScript host prelude at `soft/prelude.ts`, runtime helpers in `soft/core/`, stdlib packages under `soft/std/`, and any TypeScript-driven tests under `soft/tests/`. The soft layer is compiled via `soft/tsconfig.json`, which emits JS/declaration artifacts into `soft/dist/std`.
- `tests/` and `test262/` — Rust integration tests plus the upstream ECMAScript Test262 harness for the conformance material we exercise manually or with auxiliary runners.

Quick start
-----------

### Prerequisites

- [Rust](https://www.rust-lang.org/tools/install) 1.70+ (cargo build/test/run).
- [Node.js](https://nodejs.org/) 20+ and npm for the TypeScript toolchain.

### Building & running

1. `npm install` to pull `typescript`/`@types/node`.
2. `npm run build` (runs `cargo build --release` and `tsc` using the soft layer tsconfig).
3. Use the npm shortcuts or cargo directly:
   - `npm run vm` ⇢ `cargo run --` (starts the REPL).
   - `npm run repl` ⇢ `cargo run -- --repl`.
   - `npm run run -- path/to/file.ts` ⇢ `cargo run --release -- path/to/file.ts` (loads the soft prelude first).

Of course you can also use `cargo run [-- path.ts]` directly if you prefer.

Soft layer (blitzlib)
---------------------

The TypeScript soft layer is the canonical runtime surface that user code runs inside:

- `soft/prelude.ts` bootstraps globals, prototypes, and installs the builtin objects that user code expects to see.
- `soft/core/` contains runtime scaffolding (type definitions, runtime helpers, interpreter-like utilities) and ambient declaration files that describe the native ABI.
- `soft/std/` is a handwritten standard library that sits on top of the minimal native primitives; its modules are compiled through `soft/tsconfig.json`, which keeps strictness low so we can use flexible TypeScript while generating `dist/std`.
- `soft/tests/` is where you can add regression/unit tests that run inside blitzlib.

Documentation
-------------

- `docs/VM_V2` — the threading, isolation, and scheduling roadmap for the next-generation single-VM runtime.
- `docs/VM_IPC_SPEC.md` — the kernel ↔ scheduler/NIF IPC contract, framing, and serialization guidance that any new native hook or scheduler needs to follow.
- `test262/` — the upstream ECMAScript conformance suite; read `test262/README.md` for how to interpret and run the harness against blitz.

Testing
-------

- `cargo test` — exercise the Rust units and integration harness (see `tests/prelude_bootstrap.rs` for an example bootstrap run).
- `cargo clippy` and `cargo fmt` keep the kernel code clean.
- Keep `soft/tests/` in sync with any new API exposed to blitzlib; the TypeScript code is compiled with `tsc` during `npm run build`.
- Use the `test262/` material when you want to validate parser/VM conformance. The tree includes its own runner instructions.

Contributing
------------

1. Read the docs above so you understand the kernel ↔ blitzlib contract before adding features.
2. Add small, well-tested TypeScript implementations under `soft/` (and `soft/tests/` when appropriate).
3. When expanding native APIs, update the IPC/architecture documents and keep the surface documented in the soft layer so nothing is hidden behind an undocumented native hook.
4. Keep the TypeScript layer authoritative (call blitzlib functions, not native ones, unless absolutely necessary) and document how the new behavior maps to the Rust kernel.

License & governance
--------------------

- The `test262/` tree is governed by the Test262 license embedded inside that directory.
