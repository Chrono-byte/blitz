Blitz
=====

Blitz is a compact ECMAScript engine/runtime centered on a minimal Rust kernel that exposes strictly documented native primitives and drives an increasingly capable runtime surface. The Rust base handles JavaScript parsing, direct RISC v2 bytecode compilation, the VM/GC, and defensive kernels so behavior stays auditable and easy to iterate.

Key layers
----------

- `crates/blitz-engine/` — the host-neutral parser, compiler, VM, ECMAScript
  builtins, raw engine executable, Test262 runner, and benchmarks.
- `crates/blitz-runtime/` — reusable host capabilities, application primitives,
  RPC routing, and application examples.
- `crates/blitz-cli/` — the `blitz` executable and home for the toolchain,
  project commands, and package management.
- `docs/` and `test262/` — design material and the vendored conformance suite.

Executables
-----------

- `blitz` is the application runtime. It installs host APIs such as `console`,
  `process`, and `Blitz.serve` and is the executable intended for JavaScript
  and TypeScript
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

# Exercise the raw engine without host capabilities.
cargo run -p blitz-engine --bin blitz-engine -- path/to/engine-test.js
```

The Deno-style HTTP server listens on `127.0.0.1:8000` by default. Pass an
options object or set `BLITZ_HOST`/`BLITZ_PORT` to override it. The new
immutable Rust application/RPC model is documented in
[`BLITZ_RUNTIME_RPC_SLICE.md`](docs/BLITZ_RUNTIME_RPC_SLICE.md); it preserves
`Blitz.serve` as the low-level compatibility API. The target Effect-native
host standard library and its migration rules are specified in
[`EFFECT_STDLIB_DESIGN.md`](docs/EFFECT_STDLIB_DESIGN.md).

```js
function callbackFunc(request, info) {
  return new Response(
    `Hello from ${request.url} via ${info.remoteAddr.transport}`,
  );
}

Blitz.serve({ hostname: "127.0.0.1", port: 8000 }, callbackFunc);
```

Upgrade a request on the same listener with the Deno-style WebSocket flow:

```js
Blitz.serve((request) => {
  if (request.headers.get("upgrade") !== "websocket") {
    return new Response("WebSocket upgrade required", { status: 426 });
  }

  const { socket, response } = Blitz.upgradeWebSocket(request);
  socket.addEventListener("open", () => socket.send("connected"));
  socket.onmessage = (event) => socket.send(`echo:${event.data}`);
  return response;
});
```

Server requests expose `text()` and `json()`. `Headers` supports `get`, `has`,
`set`, `append`, `delete`, and `forEach`; `Response` accepts status, status text,
and headers and exposes `ok`, `text()`, and `json()`.

The [IRC chat example](crates/blitz-runtime/examples/irc-chat/README.md) runs a
server and two Blitz terminal clients. Interactive applications can use
`Blitz.stdin.onLine`, `setPrompt`, and `onClose` with
`Blitz.stdout.writeLine`; incoming output is rendered above the active
Rustyline prompt without discarding buffered input.

`blitz check` accepts explicit source files or recursively checks TypeScript
files in the current directory. Dependency, build, and declaration trees are
skipped. Use `--project <directory-or-tsconfig>` to select another project
root. The checker currently covers primitive and structural annotations,
arrays, functions, calls, assignments, returns, unions, interfaces, and type
aliases; unsupported advanced TypeScript types remain permissive while the
checker grows. See [the TypeScript model](docs/TYPESCRIPT.md) for the supported
type language and remaining compatibility layers.

License
--------------------

- The `test262/` tree is governed by the Test262 license embedded inside that directory.
