Blitz
=====

Blitz is a compact ECMAScript engine/runtime centered on a minimal Rust kernel that exposes strictly documented native primitives and drives an increasingly capable runtime surface. The Rust base handles JavaScript parsing, bytecode compilation, the VM/GC, and defensive kernels so behavior stays auditable and easy to iterate.

Key layers
----------

- `src/` — the Rust kernel that ships the lexer, parser, compiler, VM, GC, and the native hooks that expose every capability to the runtime. `src/main.rs` boots the globals, exposes natives, and can run a REPL or execute scripts.
- `examples/` — sample JavaScript entry points and probes that exercise how Blitz should be embedded or orchestrated.
- `tests/`, `benches/`, and `test262/` — Rust integration tests, Criterion benchmarks for the VM pipeline, plus the upstream ECMAScript Test262 harness for the conformance material we exercise manually or with auxiliary runners.

Executables
-----------

- `blitz` is the application runtime. It installs host APIs such as `console`
  and `process` and is the executable intended for JavaScript applications.
- `blitz-engine` is a host-neutral engine harness. It exposes ECMAScript
  intrinsics and `globalThis`, but deliberately omits OS and runtime APIs.

```powershell
# Run an application with the Blitz runtime.
cargo run --bin blitz -- examples/simple_demo.js

# Exercise the raw engine without host capabilities.
cargo run --bin blitz-engine -- path/to/engine-test.js
```

License
--------------------

- The `test262/` tree is governed by the Test262 license embedded inside that directory.
