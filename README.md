# Blitz

Blitz is an experimental JavaScript and TypeScript application runtime written
in Rust. It is powered by **Denko**, its host-neutral ECMAScript engine, and
provides Effect-native APIs for building applications.

Unlike runtimes built around V8 or JavaScriptCore, Blitz and Denko own the full stack:
parsing, bytecode compilation, the VM, garbage collection, and host APIs.
JavaScript remains a first-class language, while TypeScript can run directly
and use Blitz's built-in checker.

> [!WARNING]
> Blitz is early. Expect bugs, missing language features, and APIs that change
> as the runtime develops.

## What works today

Blitz currently includes:

- direct JavaScript and TypeScript execution
- a register-based bytecode VM and garbage collector
- an in-process TypeScript checker and editor analysis API
- a REPL and command-line runtime
- Effect services for files, HTTP, terminals, and WebSockets
- selected Web and Node.js compatibility APIs
- a language server and local Zed extension
- a dedicated Test262 conformance runner

This is not yet a drop-in replacement for Node.js, Deno, or Bun. Compatibility
is being added deliberately without changing the engine's ECMAScript behavior
or the runtime's Effect-native APIs.

## Try it

For now, the easiest way to try Blitz is from this repository. You will need a
recent Rust toolchain.

```sh
# Run JavaScript
cargo run -p blitz-cli -- crates/blitz-runtime/examples/hello.js

# Run TypeScript directly
cargo run -p blitz-cli -- run crates/blitz-runtime/examples/typed.ts

# Start the REPL
cargo run -p blitz-cli
```

A small TypeScript program looks like this:

```ts
const name: string = "Blitz";
console.log("Hello from", name);
```

Save it as `hello.ts`, then run:

```sh
cargo run -p blitz-cli -- run hello.ts
```

## Type checking

Blitz includes an OXC-based TypeScript checker. It can check individual files
or discover TypeScript files in a project directory.

```sh
# Check the current directory
cargo run -p blitz-cli -- check

# Check one file in strict mode
cargo run -p blitz-cli -- check --strict path/to/app.ts

# Check a file before running it
cargo run -p blitz-cli -- run --check path/to/app.ts
```

The checker supports a growing subset of TypeScript rather than the entire
language. The current type model and known gaps are documented in
[docs/TYPESCRIPT.md](docs/TYPESCRIPT.md).

## Effect-native runtime APIs

First-party Blitz APIs use [Effect](https://effect.website) for asynchronous
work, typed failures, dependency injection, resource cleanup, and streams.
The runtime bundles Effect, so these modules do not require an npm install.

```ts
import * as Effect from "effect/Effect";
import * as FileSystem from "blitz:fs";
import * as Runtime from "blitz:runtime";

const program = FileSystem.readTextFile("./config.json").pipe(
  Effect.map(JSON.parse),
  Effect.tap((config) => Effect.sync(() => console.log(config))),
);

Runtime.runMain(Runtime.provideLive(program));
```

Public services currently include `blitz:fs`, `blitz:http`,
`blitz:terminal`, and `blitz:websocket`. See the
[Effect standard library design](docs/EFFECT_STDLIB_DESIGN.md) for the API
contracts and runtime boundaries. New portable service work is moving to
Effect's first-party APIs with Blitz supplying the live platform Layer; see the
[first-party Effect API migration](docs/FIRST_PARTY_EFFECT_API_MIGRATION.md).

## Project layout

- `crates/denko-engine` — the Denko parser, compiler, VM, garbage collector, and builtins
- `crates/blitz-runtime` — host capabilities, application APIs, and persistent hosted sessions
- `crates/blitz-cli` — the `blitz` command, checker, and analysis API
- `crates/blitz-test262` — Test262 discovery, execution, and reporting
- `crates/js-string` — JavaScript string implementation
- `crates/regex-js` — JavaScript regular-expression support
- `language-server` — Language Server Protocol adapter
- `editors/zed` — local Zed extension
- `docs` — design documents, plans, and compatibility notes

The engine is host-neutral. It implements language behavior, while
`blitz-runtime` supplies operating-system and application capabilities. The raw
engine harness can run without those host APIs:

```sh
cargo run -p denko-engine --bin denko -- path/to/file.js
```

## Conformance

Blitz uses the official Test262 suite to track ECMAScript behavior. The
repository contains a dedicated runner rather than mixing conformance behavior
into the application runtime.

```sh
cargo run -p blitz-test262 --bin test262
```

Run it with `--help` to see filtering, edition, worker, manifest, and report
options. More details are available in the
[Test262 notes](docs/test262/README.md).

## License

Code under `crates/` is licensed under the
[Mozilla Public License 2.0](LICENSE). Vendored and third-party files keep their
original licenses; see
[THIRD_PARTY_LICENSES.md](THIRD_PARTY_LICENSES.md) for details.
