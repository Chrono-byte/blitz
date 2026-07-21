Blitz
=====

Blitz is a compact ECMAScript engine/runtime centered on a minimal Rust kernel that exposes strictly documented native primitives and drives an increasingly capable runtime surface. The Rust base handles JavaScript parsing, direct RISC v2 bytecode compilation, the VM/GC, and defensive kernels so behavior stays auditable and easy to iterate.

Key layers
----------

- `crates/blitz-engine/` — the host-neutral parser, compiler, VM, ECMAScript
  builtins, raw engine executable, Test262 runner, and benchmarks.
- `crates/blitz-runtime/` — reusable host capabilities and application examples.
- `crates/blitz-cli/` — the `blitz` executable and home for the toolchain,
  project commands, and package management.
- `docs/` and `test262/` — design material and the vendored conformance suite.

Executables
-----------

- `blitz` is the application runtime. It installs host APIs such as `console`
  and `process` and is the executable intended for JavaScript applications.
- `blitz-engine` is a host-neutral engine harness. It exposes ECMAScript
  intrinsics and `globalThis`, but deliberately omits OS and runtime APIs.

```powershell
# Run an application with the Blitz runtime.
cargo run -p blitz-cli -- crates/blitz-runtime/examples/simple_demo.js

# Exercise the raw engine without host capabilities.
cargo run -p blitz-engine --bin blitz-engine -- path/to/engine-test.js
```

License
--------------------

- The `test262/` tree is governed by the Test262 license embedded inside that directory.
