# Blitz Type-First Application Runtime

## Summary

Build a Blitz-native application framework that combines:

- Effect’s lazy `Effect<Success, Error, Requirements>` execution model, services, layers, scopes, fibers, and streams. [Effect type model](https://effect.website/docs/getting-started/the-effect-type/)
- tRPC-style immutable procedures, nested routers, middleware, inferred clients, and generatedless end-to-end types. [tRPC procedures](https://trpc.io/docs/server/procedures)
- Exhaustive Rust-style matching through Effect’s `Match` implementation and Blitz-compatible type declarations.
- Explicit runtime decoders for untrusted RPC inputs while keeping TypeScript types—not schemas—as the primary contract.

The first deliverable is an executable vertical slice: bundled Effect, strict typechecking, `App`, unary and subscription RPC, browser and Blitz clients, exhaustive matching, and a unified runtime event loop.

The concrete download, virtual-module installation, scheduler prerequisite, and scoped `App.serve` execution plan is tracked in [`docs/EFFECT_APP_SERVE_IMPLEMENTATION_PLAN.md`](docs/EFFECT_APP_SERVE_IMPLEMENTATION_PLAN.md).

## Public API

### Built-in modules

Provide these virtual ESM modules:

- `"effect"`: bundled, pinned Effect `3.16.7`.
- `"effect/Effect"`, `"effect/Context"`, `"effect/Layer"`, `"effect/Stream"`, `"effect/Scope"`, `"effect/Schedule"`, and `"effect/Match"`.
- `"blitz:app"`: application construction, layers, serving, lifecycle.
- `"blitz:rpc"`: procedures, routers, middleware, decoders, callers, and the Blitz client transport.
- `"blitz:match"`: re-export the supported upstream Effect `Match` API.
- `@blitz/client`: portable browser client package with the same inferred router contract.

The browser package has an exact `effect@3.16.7` peer dependency for the first release.

### Application shape

```ts
import { Effect, Layer, Stream } from "effect"
import { App } from "blitz:app"
import { Rpc, type Decoder } from "blitz:rpc"

type GetUserInput = Readonly<{ id: string }>

type NotFound = Readonly<{
  _tag: "NotFound"
  id: string
}>

const decodeGetUser: Decoder<GetUserInput> = {
  decode: (input) => {
    // Explicit runtime validation.
    // Return Effect.succeed(value) or Effect.fail(Rpc.decodeError(...)).
  },
}

const procedure = Rpc.procedure

export const appRouter = Rpc.router({
  users: Rpc.router({
    get: procedure
      .input(decodeGetUser)
      .query(({ input }) =>
        Effect.fail<NotFound>({
          _tag: "NotFound",
          id: input.id,
        }),
      ),

    changes: procedure
      .input(Rpc.noInput)
      .subscription(() =>
        Effect.succeed(
          Stream.make({ id: "1", operation: "updated" }),
        ),
      ),
  }),
})

const app = App.make({ rpc: appRouter }).pipe(
  App.provide(Layer.empty),
)

Effect.runFork(
  app.serve({
    hostname: "127.0.0.1",
    port: 8000,
    rpcPath: "/rpc",
  }),
)

export type AppRouter = typeof appRouter
```

`app.serve()` returns a lazy `Effect<never, ServeError, Requirements>`. Starting it registers live resources; interruption closes listeners, subscriptions, fibers, and layer scopes.

### Core types

Add:

```ts
type JsonPrimitive = null | boolean | number | string
type JsonValue =
  | JsonPrimitive
  | ReadonlyArray<JsonValue>
  | { readonly [key: string]: JsonValue }

interface TaggedRpcError {
  readonly _tag: string
}

interface Decoder<A, E extends TaggedRpcError = RpcDecodeError> {
  readonly decode: (
    input: unknown,
  ) => Effect.Effect<A, E, never>
}

interface Procedure<
  Kind extends "query" | "mutation" | "subscription",
  Input,
  Output,
  Error,
  Requirements,
> {}

interface Router<Definitions> {}
```

Also expose:

- `inferProcedureInput<T>`
- `inferProcedureOutput<T>`
- `inferProcedureError<T>`
- `inferProcedureRequirements<T>`
- `inferRouterClient<T>`
- `Rpc.noInput`
- `Rpc.decodeError(...)`
- `Rpc.createCaller(router)`
- `Rpc.createClient<AppRouter>(options)`

No public type uses `any`.

### Procedure builders

`Rpc.procedure` is immutable. Each builder call produces a new definition.

Supported operations:

- `.input(decoder)`: required for remotely exposed procedures, including `Rpc.noInput`.
- `.use(middleware)`: ordered middleware composition.
- `.meta(jsonMetadata)`: immutable JSON-safe metadata.
- `.query(handler)`
- `.mutation(handler)`
- `.subscription(handler)`

Query and mutation handlers must return:

```ts
Effect.Effect<Output, Error, Requirements>
```

Subscription handlers must return:

```ts
Effect.Effect<
  Stream.Stream<Output, StreamError, StreamRequirements>,
  SetupError,
  SetupRequirements
>
```

The resulting procedure error and requirement channels are the unions of setup and stream channels.

Middleware may:

- Read immutable request metadata.
- Fail with tagged errors.
- Provide Effect services to downstream middleware and handlers.
- Add requirements that must be satisfied by application layers.
- Install scoped finalizers.

Handler context includes `input`, procedure `path`, immutable request headers, remote address, request ID, and an interruption signal.

### Matching

`"blitz:match"` re-exports the real Effect `Match` runtime. The curated declaration facade guarantees inference and exhaustiveness for:

- Tagged unions using `_tag`.
- Finite literal unions.
- Literal and primitive patterns.
- Partial object patterns.
- Guards and fallback branches.
- `Match.value`, `Match.type`, `Match.valueTags`, `Match.typeTags`, and `Match.exhaustive`.

Incomplete tagged or literal matches produce compile-time diagnostics.

## Architecture and Implementation

### 1. Generic virtual-module support

Refactor the engine module loader to accept an embedder-provided virtual-module registry before filesystem resolution.

The registry stores:

- Canonical specifier.
- JavaScript source or generated bytecode.
- Source type.
- Optional declaration source.
- Stable content hash.

Keep this mechanism host-neutral. The engine knows nothing about Effect, RPC, HTTP, or Blitz-specific specifiers. `blitz-runtime` registers the concrete built-ins during `install_runtime`.

Virtual modules participate in normal construction, instantiation, caching, cycles, live exports, and dynamic import.

### 2. Bundle upstream Effect

Pin Effect `3.16.7` and commit reproducible generated assets rather than downloading packages at runtime.

Add a pnpm-managed generation workspace that:

- Fetches the exact package version.
- Preserves its MIT license.
- Produces deterministic ESM assets for the root and supported subpaths.
- Records package and generated-output hashes.
- Rejects stale generated artifacts in CI.

Execute upstream Effect code rather than recreating Effect semantics in Rust.

Run compatibility fixtures for Effect, Context, Layer, Scope, Fiber, Stream, Schedule, Match, interruption, and finalization. Any ECMAScript correctness failure is repaired in `blitz-engine`; host facilities such as timers remain in `blitz-runtime`.

### 3. Strict typechecker expansion

Introduce project-level typechecking with:

- Cross-file symbol tables and module graphs.
- Type-only imports and exports.
- `.d.ts` and virtual-module declarations.
- Generic function inference.
- Contextual callback typing.
- Generic method-chain propagation.
- Function overload resolution.
- Optional and readonly property metadata.
- Index signatures and exact excess-property checks.
- Imported `typeof` values.
- Conditional, mapped, `infer`, `Extract`, and `Exclude` behavior required by the curated facades.
- Accumulated source-located diagnostics.

Ship curated strict declarations for the supported Effect surface instead of attempting to evaluate Effect’s complete upstream declaration graph during this milestone.

Importing `blitz:app` or `blitz:rpc` without strict project settings produces a clear diagnostic. `blitz check --strict` remains valid; project configuration gains reliable `strict`, `include`, `exclude`, and module-resolution handling.

Unsupported Effect APIs may execute through the real bundle, but they are not declared as type-safe until added to the facade.

### 4. Unified runtime lifecycle

Replace the fixed HTTP/WebSocket/terminal selection loop with a runtime event registry supporting:

- HTTP requests.
- WebSocket events.
- Terminal events.
- Effect scheduler wakeups.
- Timers.
- Interruptions and shutdown signals.
- Future host resources.

Track liveness explicitly. The runtime exits only when no referenced resource, active Effect fiber, server, timer, or terminal session remains.

Add the host globals required by Effect’s scheduler—such as timers, microtask scheduling, monotonic time, and cancellation—to `blitz-runtime`, never the raw engine.

Run microtask and Effect scheduler checkpoints after every host event. Shutdown interrupts the root application fiber and waits for scoped finalizers before releasing transports.

### 5. App model

Implement `App` as an immutable runtime description containing:

- RPC router.
- Provided layers.
- Server configuration.
- Middleware and observability hooks.
- Root scope.

`App.provide(layer)` returns a new application and reduces its unsatisfied requirements at the type level.

`app.serve(options)` acquires the HTTP/WebSocket transport in an Effect scope and returns a never-ending Effect. Binding errors and layer-construction errors remain typed. Interruption performs graceful shutdown.

### 6. RPC execution

Use the existing HTTP and WebSocket backends rather than duplicating networking.

For every request:

1. Parse the protocol envelope.
2. Resolve the nested router path.
3. Run the attached decoder against `unknown`.
4. Construct a request scope and cancellation signal.
5. Execute middleware in declaration order.
6. Run the handler Effect with application layers.
7. Serialize success or expected failure.
8. Close the scope and run finalizers.

Effect failures containing JSON-safe tagged errors cross the wire unchanged. Defects, thrown exceptions, invalid success values, and serialization failures become redacted framework errors with a request ID; the full cause is logged server-side.

### 7. HTTP protocol

Use `POST /rpc` for both queries and mutations.

Single request:

```json
{
  "id": "1",
  "type": "query",
  "path": "users.get",
  "input": { "id": "42" }
}
```

Success:

```json
{ "id": "1", "ok": true, "data": {} }
```

Failure:

```json
{
  "id": "1",
  "ok": false,
  "error": { "_tag": "NotFound", "id": "42" }
}
```

An array represents a batch. Defaults:

- Maximum batch size: 32.
- Maximum HTTP body: 1 MiB.
- Batching disabled by default in clients.
- Valid RPC envelopes return HTTP 200, including domain failures.
- Malformed protocol requests return HTTP 400.
- Unknown HTTP paths return 404.
- Oversized bodies return 413.

Runtime serialization rejects cycles, non-JSON values, and non-finite numbers rather than silently coercing them.

### 8. WebSocket subscriptions

Upgrade the same `/rpc` route.

Client messages:

- `subscribe`: ID, path, input.
- `cancel`: ID.

Server messages:

- `next`: ID and data.
- `error`: ID and tagged error.
- `complete`: ID.

Cancellation interrupts the subscription fiber and runs its scope finalizers. Disconnecting interrupts every subscription owned by that connection.

Retain the existing 1 MiB message limit and bounded command queues. Queue exhaustion closes the connection with WebSocket code `1013`; active client streams fail with `RpcTransportError`.

The client does not silently reconnect or resubscribe. Callers use Effect/Stream retry policies explicitly.

### 9. Generatedless clients

`Rpc.createClient<AppRouter>` inside Blitz and `createClient<AppRouter>` from `@blitz/client` expose identical inferred procedure trees.

- Queries and mutations return `Effect<Output, DomainError | RpcClientError, never>`.
- Subscriptions return `Stream<Output, DomainError | RpcTransportError, never>`.
- Client inputs are inferred from the server router type.
- An optional batching configuration enables microtask-window batching up to 32 calls.
- Fetch and WebSocket constructors are injectable for tests.
- The browser client uses standard `fetch` and `WebSocket`.
- The Blitz transport supports `http://` and `ws://` in this milestone; TLS remains a later networking milestone.
- `Rpc.createCaller(router)` bypasses serialization for tests and in-process calls while preserving Effect errors and requirements.

### 10. Existing server compatibility

Keep `Blitz.serve` and `Blitz.upgradeWebSocket` operational and covered by their current tests.

Mark them documentation-deprecated in favor of `App` and `Rpc`. Do not emit runtime warnings and do not remove them in this vertical slice.

Preserve the active uncommitted WebSocket ownership changes and build the framework above the runtime-owned state.

## Tests and Acceptance Criteria

### Typechecking fixtures

Verify:

- Router input/output/error inference across files.
- Browser client inference from `typeof appRouter`.
- Missing or incorrect inputs.
- Missing decoder.
- Non-JSON output and tagged errors.
- Middleware-provided services.
- Unsatisfied application requirements.
- Invalid Effect handler return types.
- Subscription setup and stream channel inference.
- Exhaustive and non-exhaustive matching.
- Framework imports rejected outside strict mode.
- No framework declarations degrade to `any`.

### Effect compatibility

Run smoke programs covering:

- Success, failure, defects, recovery, and generators.
- Context tags and layers.
- Fork, join, interruption, and cancellation.
- Scope acquisition and finalizers.
- Timers and schedules.
- Streams and stream failures.
- Match exhaustiveness runtime behavior.
- Module root and supported deep imports.

### RPC integration

Test:

- Direct callers.
- HTTP query and mutation.
- Explicit batches and batch limits.
- Decoder failures.
- Domain errors.
- Redacted defects.
- Middleware ordering.
- Layer provisioning.
- JSON rejection cases.
- Unknown paths and malformed envelopes.
- Concurrent requests and cancellation.
- Subscription ordering, completion, failure, cancellation, disconnect cleanup, and backpressure.
- Multiple multiplexed subscriptions on one socket.

### Browser package

Use pnpm-based type and runtime tests with injected transports. Verify that the package bundles in a browser-targeted fixture and does not import Node-only APIs.

### Regression checks

Run:

- `cargo test -p blitz-engine -p blitz-runtime -p blitz-cli`
- `cargo clippy --workspace --all-targets`
- pnpm typecheck and test scripts for generated assets and `@blitz/client`
- Existing HTTP, WebSocket, terminal, module, and Test262-focused fixtures

## Rollout Order

1. Add virtual modules and declaration registration.
2. Add reproducible Effect assets and compatibility tests.
3. Expand project typechecking and land curated strict declarations.
4. Generalize the runtime event loop and Effect scheduler integration.
5. Implement immutable `App`, `Rpc`, decoder, middleware, and direct-caller models.
6. Add HTTP envelopes and bounded batching.
7. Add multiplexed WebSocket subscriptions and interruption.
8. Add Blitz and browser clients.
9. Add Match declarations and exhaustive fixtures.
10. Update documentation and deprecate the raw serving API.

## Assumptions and Defaults

- Effect is pinned to `3.16.7`; upgrades require compatibility and declaration review.
- Static TypeScript types remain authoritative; no schema DSL or compiler-generated validators is introduced.
- Every remote procedure requires an explicit decoder, including `Rpc.noInput`.
- Handlers are Effect-only; subscriptions return an Effect of an Effect Stream.
- Domain errors are JSON-safe tagged values using `_tag`.
- Wire values are JSON-safe only.
- RPC uses POST envelopes with optional bounded batching.
- Subscriptions use multiplexed WebSockets and explicit retry.
- Framework projects require strict typechecking.
- `App.serve()` is lazy and scoped.
- The initial Blitz client supports plaintext HTTP/WebSocket; browser TLS is provided by the browser.
- Raw `Blitz.serve` remains available as a deprecated low-level adapter.
