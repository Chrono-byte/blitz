# Effect-native runtime standard library

## Status

This document defines the target public TypeScript API for `blitz-runtime`.
It covers Blitz-owned host capabilities, not ECMAScript semantics. It replaces
callback-first Blitz APIs as the recommended application API while preserving
standards compatibility at the appropriate boundaries. The phased delivery
sequence is tracked in
[`EFFECT_STDLIB_IMPLEMENTATION_PLAN.md`](EFFECT_STDLIB_IMPLEMENTATION_PLAN.md).
The corrected contracts and full plans for the eleven remaining API/runtime
workstreams are in
[`EFFECT_RUNTIME_API_EXPANSION_PLAN.md`](EFFECT_RUNTIME_API_EXPANSION_PLAN.md).
The bundled Effect runtime must also satisfy the parser-free artifact contract
in [`EFFECT_FULL_PREBUNDLING_PLAN.md`](EFFECT_FULL_PREBUNDLING_PLAN.md).

Today the canonical Effect graph and manifest-listed Blitz stdlib JavaScript
are committed in one versioned prelinked runtime artifact. Runtime installation
validates pinned source manifests and host ABI, exposes only public roots, and
performs no bundled parsing, import extraction, AST/ESTree linking, typechecking,
or compilation. The artifact is not a heap snapshot; initializers still run.

## Goals

- Every fallible, asynchronous, interruptible, or resource-owning host operation
  is represented by the real bundled `effect` runtime.
- Requirements, expected failures, interruption, and resource lifetime remain
  visible in TypeScript types.
- APIs feel natural to TypeScript developers. Effect conventions are used where
  they improve composition; ceremony is not added to pure or infallible code.
- Programs are easy to test by replacing services with Layers.
- The VM owns JavaScript state. Rust owns OS resources and operates on opaque
  resource identifiers; background tasks never mutate JavaScript values.
- Web and Node compatibility do not distort the primary Blitz API.
- The canonical Effect graph starts without runtime source parsing,
  import-scanning, typechecking, ESTree linking, or compilation.

## Non-goals

- Reimplementing Effect primitives in Rust or in Blitz facades.
- Changing ECMAScript, Web, or Node semantics to return `Effect` values.
- Wrapping every pure function in `Effect`.
- A single untyped `RuntimeError` for unrelated failures.
- Ambient fallback services that make tests accidentally perform real I/O.

## API priority

When concerns conflict, use this order:

1. Correct ECMAScript and Web behavior.
2. Idiomatic, inferable TypeScript.
3. Explicit Effect errors, requirements, scopes, and interruption.
4. API symmetry or implementation convenience.

For example, `Path.join()` remains a pure function. `FileSystem.readFile()` is
an Effect because it performs fallible I/O. The standard `Response` constructor
must remain a constructor rather than becoming an Effect.

## Four public layers

### 1. ECMAScript built-ins

The engine implements these faithfully. They never depend on Effect.

### 2. Standards compatibility globals

`URL`, `Request`, `Response`, `Headers`, `fetch`, `WebSocket`, timers, streams,
and abort signals retain their standard Promise, event, and callback behavior.
They are useful compatibility boundaries, but are not the recommended API for
new Blitz applications.

### 3. Effect-native Blitz modules

`blitz:*` modules are the primary standard library. Host effects use services,
tagged errors, scopes, Layers, and Streams. They are implemented in JavaScript
on top of small private host bridges and the real bundled Effect package.

### 4. Node compatibility

`node:*`, CommonJS, and `process` preserve Node behavior. They adapt the same
host resources but do not leak Node conventions into `blitz:*` modules.

## Module conventions

Use namespace imports and ordinary module functions:

```ts
import * as Effect from "effect/Effect"
import * as FileSystem from "blitz:fs"

const program = FileSystem.readTextFile("./config.json").pipe(
  Effect.map(JSON.parse),
)
```

Each capability module exports:

- a service interface;
- a `Context.Tag` with the same descriptive name where TypeScript permits it;
- pipe-friendly accessor functions;
- `layer` or `layerLive` for the production implementation;
- focused tagged error types and constructors only when users need to create
  those errors themselves;
- immutable option and result objects.

Prefer one options object once an operation has multiple optional settings.
Do not use boolean flags, long positional argument lists, `any`, enum-valued
numbers, or classes whose primary purpose is namespacing.

Functions support data-first and data-last forms only when both are useful and
inference remains unambiguous. Do not duplicate every API merely for symmetry.

## Effect contract

### Operations

Use these return shapes consistently:

```ts
// Fallible operation using a service
Effect.Effect<Success, OperationError, Service>

// Acquired resource; release is guaranteed by Scope
Effect.Effect<Resource, AcquireError, Service | Scope.Scope>

// Incremental or unbounded data
Stream.Stream<Element, StreamError, Service>

// Pure validation
Either.Either<Valid, ValidationError>

// Optional pure lookup
Option.Option<Value>
```

A function returning an Effect must be lazy. Argument validation, service
lookup, acquisition, and host calls happen when the Effect runs, not while it
is constructed.

Expected failures use the error channel. Defects are reserved for broken
invariants and unrecoverable bugs. Interruption is not translated into a
normal error.

### Errors

Errors are readonly discriminated values with narrow operation-specific tags:

```ts
export interface FileSystemError {
  readonly _tag: "FileSystemError"
  readonly reason:
    | "NotFound"
    | "PermissionDenied"
    | "AlreadyExists"
    | "InvalidPath"
    | "NotDirectory"
    | "IsDirectory"
    | "TooManyOpenFiles"
    | "Io"
  readonly operation: "open" | "read" | "write" | "stat" | "remove"
  readonly path?: string
  readonly message: string
}
```

Keep stable semantic reasons separate from host-specific diagnostic details.
Do not expose Rust error strings as the only machine-readable contract. Errors
crossing RPC boundaries remain JSON-safe tagged values.

### Services and Layers

All replaceable capabilities are Context services. Accessor functions retain
the service requirement; they do not silently choose a live implementation.
Production entry points provide the live runtime Layer. Tests provide small
in-memory or deterministic Layers.

A service method is already an Effect and never throws expected host failures:

```ts
export type Path = string | URL

export interface FileSystem {
  readonly readFile: (
    path: Path,
    options?: ReadFileOptions,
  ) => Effect.Effect<Uint8Array, FileSystemError>
}

export declare const FileSystem: Context.Tag<FileSystem, FileSystem>

export declare const readFile: (
  path: Path,
  options?: ReadFileOptions,
) => Effect.Effect<Uint8Array, FileSystemError, FileSystem>

export declare const layerLive: Layer.Layer<FileSystem>
```

Service values are immutable JavaScript objects. Mutable OS state lives in the
runtime resource registry and is addressed with opaque, generational handles.

### Resource safety

Anything with `close`, `shutdown`, `kill`, or `release` behavior is acquired in
a Scope. Public code should normally not call release manually.

```ts
Effect.scoped(
  FileSystem.open("./events.log", { flag: "append" }).pipe(
    Effect.flatMap((file) => file.write(encoder.encode("started\n"))),
  ),
)
```

Release is idempotent in the host adapter. Interruption cancels pending I/O,
stops ingress, waits for owned children when required, and runs finalizers once.

### Concurrency and state

Use real Effect `Fiber`, `Queue`, `PubSub`, `Deferred`, `Ref`, `Scope`, and
`Stream` APIs. Do not add Blitz-specific clones. Application state should be
owned by these values or by an explicit service, not by mutable module globals.

Backpressure must be represented by bounded queues or pull-based Streams.
Queue overflow must never silently discard data unless the selected strategy
explicitly says so.

## Standard-library surface

### `blitz:runtime`

Application execution and the aggregate live Layer.

```ts
export interface RunMainOptions {
  readonly reportFailure?: (cause: Cause.Cause<unknown>) => void
  readonly disableSignalHandlers?: boolean
}

export declare const layerLive: Layer.Layer<RuntimeServices, RuntimeInitError>

export declare const provideLive: <Success, Error, Requirements>(
  program: Effect.Effect<Success, Error, Requirements>,
) => Effect.Effect<
  Success,
  Error | RuntimeInitError,
  Exclude<Requirements, RuntimeServices>
>

export declare const runMain: <Success, Error>(
  program: Effect.Effect<Success, Error, never>,
  options?: RunMainOptions,
) => void
```

`RuntimeServices` is `never` for the initial runtime milestone and expands only
when a service tag, facade, live Layer, host ABI, and tests ship together.

`runMain` is the normal executable boundary. It accepts only a closed Effect,
starts one root fiber, maps SIGINT/SIGTERM to interruption, waits for finalizers,
sets the exit status, and reports an unhandled Cause once. `provideLive` is the
explicit convenience that supplies only the shipped runtime service union.
Libraries never call `runMain` or `Effect.runPromise` internally.

Applications needing custom services compose Layers before this boundary.

### `blitz:path`

Pure, platform-aware path manipulation. Functions return values directly and
perform no filesystem access.

```ts
Path.join("var", "log", "app.log")
Path.resolve("/workspace", "src", "index.ts")
Path.extname("archive.tar.gz")
Path.fromFileUrl(url) // Either<string, PathError>
```

`resolve` takes an explicit base and never reads ambient cwd. File-URL
conversion returns `Either` for invalid schemes, authorities, encoding, relative
paths, and platform-incompatible inputs. Ordinary string operations follow
documented deterministic rules. Provide explicit `posix` and `windows`
namespaces rather than ambient surprises in portable libraries.

### `blitz:fs`

All filesystem APIs use one public alias:

```ts
export type Path = string | URL
```

- `readFile`, `readTextFile`, `writeFile`, `writeTextFile`
- `open` as a scoped resource
- `stat`, `exists`, `realPath`, `copy`, `rename`, `remove`, `makeDirectory`
- `readDirectory` as a `Stream<DirectoryEntry, FileSystemError, FileSystem>`
- file watching as a scoped Stream
- atomic write helper with explicit durability options

Paths accept `string | URL`; bytes use `Uint8Array`. Text methods require or
default an explicit documented encoding. No API returns mixed string-or-bytes
values based on an option.

### `blitz:process`

- immutable process metadata and arguments through a replaceable `Process`
  service;
- environment lookup returning `Option` or typed decoding effects;
- exit status mutation as an Effect;
- signal subscription as a scoped Stream;
- child processes acquired in Scope, with stdout/stderr Streams and Effectful
  stdin writes, wait, and kill operations.

The Node-compatible global `process` remains available separately. New Blitz
libraries do not read it at module initialization.

### `blitz:terminal`

- terminal capabilities and dimensions as Effects;
- `write` and `writeLine` as Effects;
- input bytes, text chunks, keys, and lines as Streams;
- raw mode and alternate-screen mode as scoped resources;
- prompts as interruptible Effects.

Terminal output must honor backpressure. Tests use a deterministic terminal
Layer rather than replacing globals.

### `blitz:http`

Use standard `Request`, `Response`, `Headers`, and `URL` values because they are
idiomatic TypeScript interchange types. Effect owns execution and lifecycle.

```ts
export type Handler<Error = never, Requirements = never> = (
  request: Request,
  context: RequestContext,
) => Effect.Effect<Response | WebSocketUpgrade, Error, Requirements>

export interface ServeOptions<Error, Requirements, MapperRequirements = never> {
  readonly hostname?: string
  readonly port?: number
  readonly handler: Handler<Error, Requirements>
  readonly onHandlerError?: (
    error: Error,
    request: Request,
    context: RequestContext,
  ) => Effect.Effect<Response, never, MapperRequirements>
  readonly requestTimeout?: Duration.DurationInput
  readonly maxRequestBodyBytes?: number
}

export declare const serve: <Error, Requirements, MapperRequirements = never>(
  options: ServeOptions<Error, Requirements, MapperRequirements>,
) => Effect.Effect<
  never,
  ListenError,
  HttpServer | Requirements | MapperRequirements
>

export declare const listen: <Error, Requirements, MapperRequirements = never>(
  options: ServeOptions<Error, Requirements, MapperRequirements>,
) => Effect.Effect<
  HttpServerResource,
  ListenError,
  HttpServer | Requirements | MapperRequirements | Scope.Scope
>

export declare const request: (
  input: RequestInfo | URL,
  init?: RequestInit,
) => Effect.Effect<
  Response,
  HttpClientError,
  HttpClient | Scope.Scope
>

export declare const withResponse: <Success, Error, Requirements>(
  input: RequestInfo | URL,
  use: (response: Response) => Effect.Effect<Success, Error, Requirements>,
  init?: RequestInit,
) => Effect.Effect<
  Success,
  HttpClientError | Error,
  HttpClient | Requirements
>
```

Each request runs in a child Scope. Disconnect and timeout interrupt the handler.
Servers are scoped by construction even when `serve` returns `never`. Streaming
request and response bodies use bounded transport adapters; helpers expose
Effect Streams without changing standard Web values.

### `blitz:websocket`

The primary API is connection/Stream based rather than callback-property based:

```ts
export interface WebSocketUpgrade {
  readonly _tag: "WebSocketUpgrade"
}

export interface Connection {
  readonly protocol: Option.Option<string>
  readonly messages: Stream.Stream<Message, WebSocketError>
  readonly send: (message: Message) => Effect.Effect<void, WebSocketError>
  readonly close: (options?: CloseOptions) => Effect.Effect<void, WebSocketError>
}

export declare const connect: (
  url: string | URL,
  options?: ConnectOptions,
) => Effect.Effect<Connection, WebSocketError, WebSocketClient | Scope.Scope>
```

A server upgrade is represented by an explicit typed decision in the HTTP
handler. Connections are scoped, sends observe bounded backpressure, and the
message Stream has one owner unless explicitly broadcast through `PubSub`.
The standard global `WebSocket` keeps browser-compatible event semantics.

### `blitz:crypto`

- secure random bytes through a `Random`/`Crypto` capability;
- digest, MAC, KDF, encryption, decryption, key import, and key generation as
  Effects where host work or secret material is involved;
- immutable algorithm option objects and `Uint8Array` results;
- secret values with redacted inspection and explicit export policy.

Do not invent weak convenience defaults for cryptographic algorithms.

### `blitz:os`

Replaceable host metadata and temporary-directory operations. Static platform
metadata may be exposed as pure values from an immutable service snapshot;
resource and entropy-dependent operations remain Effects.

### `blitz:console`

Prefer upstream `effect/Console`, `Effect.log*`, annotations, and Logger Layers.
Blitz supplies the live console implementation and terminal-aware logger. The
global `console` remains a compatibility and REPL API.

### `blitz:app` and `blitz:rpc`

These remain framework modules built entirely from the same services.

- descriptions and builders are immutable;
- handlers return Effects only;
- subscriptions return Streams;
- middleware adds typed errors and requirements;
- decoders consume `unknown` and return Effects or a documented pure decoding
  result;
- `App.serve` delegates to `blitz:http` and acquires resources in Scope;
- caller and client effects preserve domain errors;
- no Rust-side `App`, RPC builder, or `Layer` becomes a competing public model.

Use pipeable free functions for application transformations:

```ts
const app = pipe(
  App.make({ rpc: router }),
  App.provide(Database.layer),
)

Runtime.runMain(
  Runtime.provideLive(App.serve(app, { port: 8000 })),
)
```

Builder methods are acceptable where they materially improve contextual generic
inference, as with RPC procedure construction. They must return new values.

### Later modules

`blitz:dns`, `blitz:tcp`, `blitz:udp`, `blitz:tls`, `blitz:worker`, and
`blitz:test` follow the same service/resource/error rules. Low-level network
connections are scoped and expose Stream-based reads plus backpressured writes.

## Bundled Effect surface

Blitz pins `effect@3.16.7` and generates one canonical ESM source graph. The
final runtime form is one complete, deterministic prelinked module-graph
artifact containing that Effect graph plus every manifest-listed public and
private Blitz stdlib JavaScript module—not a bytecode-only cache. The artifact contains verified
chunks, ordered static dependencies, stable module indices, imports, exports,
public roots, visibility, source integrity, Effect provenance, runtime/host ABI
metadata, and canonical live-binding IDs.

Keep these rules:

- pin exact package, graph-format, runtime ABI, and host bridge ABI versions;
- let `tools/effect-assets` generate canonical source, licenses, integrity,
  manifest, declarations, and embedding metadata;
- let a data-driven stdlib asset step own Blitz facade/adapter source,
  declarations, visibility, hashes, and host ABI metadata;
- let the Rust graph precompiler alone generate the final graph artifact from
  the verified Effect and stdlib inputs;
- commit all reproducibility inputs and the final prelinked artifact;
- make generation deterministic, atomic, network-free, and absent from
  `build.rs` and runtime startup;
- register one canonical private module identity graph in `blitz-runtime` while
  keeping raw `blitz-engine` environments free of Effect and Blitz modules;
- allocate canonical live slots before evaluation so cycles, re-exports,
  namespaces, and root/deep import identity remain correct;
- validate format, bytecode, graph edges, descriptors, indices, binding IDs,
  visibility, package provenance, source hashes, and ABI versions before any
  initializer executes;
- retain source only for regeneration and diagnostics, never as a runtime
  fallback;
- install versioned Rust host primitives before atomically registering the
  graph, and reject missing primitives or ABI mismatches before side effects;
- permit runtime decode, validation, host installation, record installation,
  slot allocation, and initializer execution, but perform zero bundled Effect
  or Blitz stdlib parsing, import extraction, typechecking, ESTree linking, or
  compilation; and
- keep Effect and stdlib execution in upstream JavaScript on the single VM
  thread.

Changing supported Effect aliases/exports or any Blitz facade/private adapter
changes the prelinked graph. Such a change is incomplete unless source and
stdlib-manifest generation, graph precompilation, byte-for-byte `--check`,
descriptor/visibility/ABI validation, identity tests, atomic-failure tests, and
explicit no-parser/no-compiler regressions all pass together. Runtime
registration must never fall back to `VirtualModule::javascript` or
source-backed `VirtualModule::precompiled` for bundled Effect or stdlib modules.

The current curated aliases are too small for the stdlib. Expand them only
after the runtime cutover to full prebundling, and regenerate the complete
runtime graph with every Effect or stdlib expansion. At minimum support the
upstream modules and operations needed for:

- `Cause`, `Exit`, `Option`, `Either`, `Duration`, and `Function`;
- `Fiber`, `Scope`, `Ref`, `Deferred`, `Queue`, `PubSub`, and interruption;
- `Config`, `Console`, `Logger`, `Random`, and test services;
- `Effect.async`, promise interop, `gen`, `all`, `ensuring`, service access,
  scoped acquisition, and interruption handlers;
- Stream async/scoped constructors, effectful transforms, merging, buffering,
  broadcast, sinks, and consumers.

Do not hand-roll substitutes because the curated declaration checker supports
only a subset. Expand the checker and curated declarations instead. Runtime
compatibility tests must execute the real operation represented by every added
public declaration.

If `@effect/platform` is adopted, pin a version compatible with the embedded
Effect release and vendor it into the same generated canonical graph. Reuse its
public contracts where they produce a better interoperable API, but do not
adopt an API solely for nominal compatibility. There must still be one Effect
singleton graph.

## Private host bridge

Rust must not construct pseudo-Effects. A private bridge exposes only host
primitives. JavaScript facades turn those primitives into upstream Effects.

The preferred asynchronous bridge shape is registration plus cancellation:

```ts
interface HostBridge {
  start(
    operation: string,
    input: unknown,
    complete: (result: HostResult) => void,
  ): HostOperationId
  cancel(id: HostOperationId): void
}
```

A facade wraps this with `Effect.async`; its cancellation finalizer calls
`cancel`. Completion is delivered as an owned runtime event on the VM thread.
The exact private ABI may be specialized per capability, but it must provide:

- lazy start;
- exactly-once completion;
- cancellation race handling;
- bounded input, output, and queues;
- opaque generational resource IDs;
- structured errors;
- idempotent close;
- no JavaScript `Value`, `Rc`, or `RefCell` in Tokio tasks.

Private specifiers use `blitz-internal:*`, have no public declarations, and are
unavailable to user module resolution. The old `__blitz_app_transport` global
is forbidden. A versioned, immutable, non-enumerable host ABI object may be used
only as plumbing behind private adapters until the engine supports private
native-module bindings directly.

## Permissions

Capabilities are checked when an Effect performs an operation, not when a
module is imported. Permission denial is a typed expected error. Layers can
attenuate access—for example, exposing a filesystem rooted at one directory or
an HTTP client restricted to selected origins.

Permission prompts, if supported, are effects supplied by CLI policy. Library
code never prompts directly.

## Testing contract

Every module ships with:

1. declaration tests for success, error, and requirement inference;
2. negative TypeScript fixtures proving missing Layers and invalid options fail;
3. deterministic service/Layer tests with no OS access;
4. live loopback or temporary-directory integration tests;
5. interruption tests for every pending operation;
6. finalizer tests for success, failure, defect, and interruption;
7. bounded queue and backpressure tests;
8. raw-engine tests proving Blitz modules are absent without runtime install;
9. tests proving no public declaration degrades to `any`;
10. bundle tests proving corrupt or stale metadata fails validation before side
    effects;
11. manifest coverage tests proving every entry is prelinked and reachable and
    no unmanifested module is bundled;
12. direct-import tests proving private entries are inaccessible to user source;
13. explicit counters or hooks proving bundled Effect and stdlib startup
    performs zero parser, import-extraction, typechecker, and compiler
    invocations.

## Practices encouraged by the API

Recommended:

- one root `Runtime.runMain`;
- small programs composed before execution;
- explicit services and Layers;
- scoped acquisition;
- tagged expected errors and exhaustive matching;
- Stream for incremental data;
- immutable descriptions and configuration;
- dependency injection in tests.

Discouraged or rejected by API design:

- `Effect.run*` inside libraries, request handlers, or event callbacks;
- mutable module-global connection lists;
- unscoped handles requiring callers to remember cleanup;
- thrown strings or catch-all `unknown` host errors;
- Promise conversion in the middle of Effect code;
- callback properties in the primary Blitz API;
- unbounded queues or implicit data loss;
- import-time I/O;
- service locators hidden outside the Effect requirement type.

## Migration from the current runtime

1. **Complete:** deterministic runtime-graph generation and wire format for
   Effect plus the stdlib manifest, visibility, and host ABI; parser-free
   runtime cutover; and removal of transitional source-backed/bytecode-only
   installation.
2. Expand and test the bundled upstream Effect and Blitz stdlib surfaces,
   regenerating and byte-comparing the complete graph for every change.
3. Add a generic cancellable host-operation/resource bridge and resource IDs.
4. Implement `blitz:runtime`, including `layerLive` and `runMain`.
5. Implement foundational `blitz:path`, `blitz:fs`, `blitz:process`, and
   `blitz:terminal` services.
6. Replace the current Effect-wrapped `Blitz.serve` example with scoped
   `blitz:http` and `blitz:websocket` APIs.
7. Rebuild `blitz:app` and `blitz:rpc` on those public services.
8. Add crypto, subprocess, signals, outbound networking, and lower-level socket
   modules.
9. Build standards and Node compatibility adapters over the same host resource
   layer.
10. Deprecate `Blitz.serve`, `Blitz.upgradeWebSocket`, and terminal globals only
    after equivalent stable APIs and migration documentation exist.

The first implementation slice is `blitz:runtime`, pure `blitz:path`, and
`blitz:fs` read operations. It proves service requirements, live/test Layers,
host completion,
cancellation, structured errors, generated declarations, and the single Effect
module identity without depending on server complexity.
