Blitz
=====

Blitz is a compact ECMAScript engine/runtime centered on a minimal Rust kernel that exposes strictly documented native primitives and drives an increasingly capable runtime surface. The Rust base handles JavaScript parsing, direct RISC v2 bytecode compilation, the VM/GC, and defensive kernels so behavior stays auditable and easy to iterate.

Key layers
----------

- `crates/blitz-engine/` — the host-neutral parser, compiler, VM, ECMAScript
  builtins, raw engine executable, and benchmarks.
- `crates/blitz-test262/` — the dedicated Test262 discovery, execution,
  isolation, and reporting harness built on the engine.
- `crates/blitz-runtime/` — reusable host capabilities, application primitives,
  RPC routing, and application examples.
- `crates/blitz-cli/` — the `blitz` executable and its current `run`, `check`,
  and `analyze` commands.
- `docs/` and `test262/` — design material and the vendored conformance suite.

Executables
-----------

- `blitz` is the application runtime. It installs host APIs such as `console`
  and `process` and is the executable intended for JavaScript and TypeScript
  applications.
- `blitz-engine` is a host-neutral engine harness. It exposes ECMAScript
  intrinsics and `globalThis`, but deliberately omits OS and runtime APIs.

```powershell
# Run an application with the Blitz runtime.
cargo run -p blitz-cli -- crates/blitz-runtime/examples/hello.js

# Type annotations are stripped during execution.
cargo run -p blitz-cli -- run path/to/application.ts

# Type checking uses Blitz's in-process OXC-based checker.
cargo run -p blitz-cli -- check
cargo run -p blitz-cli -- check --strict
cargo run -p blitz-cli -- run --check path/to/application.ts

# Emit compiler-owned JSON for editor tooling (source is read from stdin).
Write-Output "const answer: number = 42;" | cargo run -p blitz-cli -- analyze --path editor.ts -

# Exercise the raw engine without host capabilities.
cargo run -p blitz-engine --bin blitz-engine -- path/to/engine-test.js

# Run the dedicated Test262 conformance harness.
cargo run -p blitz-test262 --bin test262
```

Effect-first runtime APIs
-------------------------

Effect is the primary composition model for Blitz-owned runtime capabilities.
The default runtime prelinks one pinned upstream Effect graph and exposes
service-based modules such as `blitz:fs`, `blitz:http`, `blitz:terminal`, and
`blitz:websocket`. Their public JavaScript facades construct real Effect values
over small private host primitives; Rust owns OS resources but does not
reimplement Effect, retain JavaScript values in background tasks, or mutate VM
state outside the VM thread.

The runtime APIs follow a few consistent rules:

- Fallible and asynchronous operations are lazy Effects. Constructing an
  operation performs no host I/O.
- Replaceable capabilities are Context services. Accessors retain their service
  requirements, live implementations are provided as Layers, and tests can
  provide deterministic or in-memory Layers.
- Expected failures use narrow tagged values in the Effect error channel.
  Defects remain defects instead of being converted into generic runtime errors.
- Resources are Scope-owned. Interruption cancels pending work and runs
  finalizers; callers do not need manual cleanup for correctness.
- Incremental input and output use Effect Streams with explicit backpressure.
- Pure and infallible operations remain ordinary functions. ECMAScript, Web,
  and Node compatibility APIs keep their standard semantics rather than
  returning Effects.

```ts
import * as Effect from "effect/Effect"
import * as FileSystem from "blitz:fs"
import * as Runtime from "blitz:runtime"

const program = FileSystem.readTextFile("./config.json").pipe(
  Effect.map(JSON.parse),
  Effect.tap((config) => Effect.sync(() => console.log(config))),
)

Runtime.runMain(program)
```

`Runtime.runMain` supplies the aggregate live Layer, starts one root fiber,
maps process termination signals to interruption, and waits for scoped
finalizers before exit. Applications that need explicit composition can use
the individual service Layers or `Runtime.provideLive`.

The complete API contracts and boundaries are documented in
[`EFFECT_STDLIB_DESIGN.md`](docs/EFFECT_STDLIB_DESIGN.md). Delivery phases are
tracked in
[`EFFECT_STDLIB_IMPLEMENTATION_PLAN.md`](docs/EFFECT_STDLIB_IMPLEMENTATION_PLAN.md),
and the remaining runtime workstreams are specified in
[`EFFECT_RUNTIME_API_EXPANSION_PLAN.md`](docs/EFFECT_RUNTIME_API_EXPANSION_PLAN.md).
The immutable Rust application/RPC model is documented in
[`BLITZ_RUNTIME_RPC_SLICE.md`](docs/BLITZ_RUNTIME_RPC_SLICE.md) as an explicit
legacy compatibility layer.

The [Effect-native IRC chat example](examples/irc-chat-v2/README.md) runs a
server and two Blitz terminal clients.

`blitz check` accepts explicit source files or recursively checks TypeScript
files in the current directory. Dependency, build, and declaration trees are
skipped. Use `--project <directory-or-tsconfig>` to select another project
root. The checker currently covers primitive and structural annotations,
arrays, functions, calls, assignments, returns, unions, interfaces, and type
aliases; unsupported advanced TypeScript types remain permissive while the
checker grows. See [the TypeScript model](docs/TYPESCRIPT.md) for the supported
type language and remaining compatibility layers.

Editor tooling
--------------

[`language-server/`](language-server/) contains a thin TypeScript LSP adapter
backed by the compiler's `blitz analyze` JSON API. It currently provides Blitz
diagnostics, document symbols, completions, hover, and go-to-definition for
JavaScript, TypeScript, and TSX documents. The local Zed extension lives in
[`editors/zed/`](editors/zed/) and reuses Zed's built-in JS/TS grammars.

Licensing
---------

Blitz code under `crates/` is licensed under the Mozilla Public License 2.0. The vendored `test262/` suite retains its upstream license, and other third-party material retains its applicable license and notices. Do not assume that every file in this repository is MPL-2.0.

See [`LICENSE`](LICENSE) for the MPL-2.0 terms and [`THIRD_PARTY_LICENSES.md`](THIRD_PARTY_LICENSES.md) for third-party material.
