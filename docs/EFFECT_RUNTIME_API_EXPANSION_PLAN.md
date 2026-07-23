# Effect-native runtime API expansion plan

## Purpose and status

This plan closes the eleven API and runtime gaps found after the parser-free
Effect/Blitz graph cutover. It defines the full target contracts, implementation
order, lifecycle rules, and acceptance gates. It supplements
[`EFFECT_STDLIB_DESIGN.md`](EFFECT_STDLIB_DESIGN.md) and refines the phase
sequence in
[`EFFECT_STDLIB_IMPLEMENTATION_PLAN.md`](EFFECT_STDLIB_IMPLEMENTATION_PLAN.md).
Where an older example conflicts with this plan, this plan wins.

The prelinked graph, visibility model, deterministic generator, and atomic
runtime installation are complete. The APIs below are not complete merely
because their host transport or a compatibility global already exists. A public
Effect API is complete only when its declarations, JavaScript facade, private
host ABI, runtime behavior, cancellation/finalization tests, and regenerated
complete graph land together.

Do not implement all workstreams in one patch. Every numbered workstream below
is a separately reviewable program with smaller commit boundaries.

## Global decisions

1. The pinned `effect@3.16.7` declarations and runtime behavior are the source
   of truth. Curated declarations may omit APIs but may not alter the semantics
   or types of APIs they include. Existing compatibility transforms, including
   the temporary internal `WeakMap` to `Map` rewrite, must be explicitly listed
   with retention tests and removed when the engine prerequisite lands; they are
   not silently treated as upstream behavior.
2. Public requirements are unions of service identifiers. They are not
   intersections and never default to `unknown`.
3. `unknown` is reserved for genuinely untrusted input, defects, opaque
   diagnostics, and tested erased constraints. Publicly observable values and
   inferred channels contain no `any`. Where upstream uses `any` only as an
   erased generic constraint, the curated declaration uses a semantically
   equivalent tested `unknown` form; if that is not possible, withhold the API
   rather than weakening its public contract.
4. Rust exposes host primitives and owned events only. JavaScript facades build
   real Effects, Layers, Scopes, Fibers, Queues, and Streams.
5. Background tasks never own JavaScript `Value`, `Vm`, `Rc`, `RefCell`,
   callbacks, roots, or Effect values.
6. Every asynchronous host operation is lazy, cancellable, generation-safe,
   bounded, and completed on the VM thread exactly once.
7. Every resource is Scope-owned. Explicit `close` may be offered for early
   release, but callers never need it for correctness.
8. `Runtime.runMain` accepts a closed Effect. `Runtime.provideLive` is the
   explicit convenience that supplies the aggregate live Layer.
9. Standards globals keep standards semantics; Node compatibility keeps Node
   semantics; `blitz:*` is the Effect-native API.
10. Each public/private module change regenerates and verifies the one complete
    runtime graph. There is no source-backed runtime fallback.

## Dependency order

```text
1 Effect declarations and checker fixtures
  ├─ 2 corrected application type model
  └─ 3 scheduler, roots, and host operation kernel
       └─ 4 root lifecycle and shutdown
            └─ 5 blitz:runtime
                 ├─ 6 blitz:path
                 ├─ 7 blitz:fs
                 ├─ 8 blitz:process / blitz:subprocess
                 ├─ 9 blitz:terminal / logging
                 └─ 10 blitz:http / blitz:websocket
                       └─ 11 blitz:app / blitz:rpc / later capabilities
```

`blitz:path` has no runtime dependency, but its URL functions wait for a
conformant `URL`. The pure string subset may land while workstreams 3–5 proceed.

Mapping to the older implementation phases is explicit:

| Workstream | Existing phases |
| --- | --- |
| 1 | Phases 0–1 |
| 2 | Phase 9 contract correction, prepared during Phase 0 |
| 3 | Phases 2–3 |
| 4–5 | Phase 4 |
| 6 | Phase 5 |
| 7 | Phases 6–7 |
| 8–9 | Phase 7 |
| 10 | Phase 8 |
| 11 | Phases 9–10 and separately staged later capabilities |

---

## 1. Correct the Effect and TypeScript foundation

### Problems to correct

The current curated declarations are not safe foundations for services:

- `Effect`, `Layer`, and `Stream` model requirement variance incorrectly;
- `Context.Tag` reverses upstream identifier/service generic order;
- `Fiber.await` returns a success directly instead of `Exit`;
- `Effect.acquireRelease` omits the closing `Exit` and permits typed finalizer
  errors;
- `Layer.mergeAll` erases all channels to `unknown`;
- `Layer.provide` models Effect provisioning instead of Layer provisioning;
- `Stream.runCollect` returns `ReadonlyArray` instead of `Chunk`;
- service-access, root observation, interruption, and scoped Stream operations
  are missing.

### Exact declaration rules

Model pinned upstream variance and names. Important forms include:

```ts
export interface Effect<out A, out E = never, out R = never> extends Pipeable
export interface Tag<in out Id, in out Service> extends Pipeable

export declare const acquireRelease: {
  <A, X, R2>(
    release: (
      resource: A,
      exit: Exit.Exit<unknown, unknown>,
    ) => Effect.Effect<X, never, R2>,
  ): <E, R>(
    acquire: Effect.Effect<A, E, R>,
  ) => Effect.Effect<A, E, Scope.Scope | R | R2>

  <A, E, R, X, R2>(
    acquire: Effect.Effect<A, E, R>,
    release: (
      resource: A,
      exit: Exit.Exit<unknown, unknown>,
    ) => Effect.Effect<X, never, R2>,
  ): Effect.Effect<A, E, Scope.Scope | R | R2>
}
```

Use upstream `Fiber.Fiber` and `Fiber.RuntimeFiber`; delete the local Fiber
substitute and update `Effect.fork`, join, and interruption declarations to the
real Fiber module. `Fiber.await` returns `Effect<Exit<A, E>>`, `Fiber.join`
observes the success/error channels, and interruption uses the pinned `Fiber`
module APIs. Add the generator alias `"Fiber.await": "_await"` and a runtime
fixture proving root/deep imports share the same Fiber identity.

Correct the foundational generic order explicitly:

```ts
Context.Tag<Id, Service>
Context.GenericTag: <Id, Service = Id>(key: string) => Context.Tag<Id, Service>

Layer.succeed: <Id, Service>(
  tag: Context.Tag<Id, Service>,
  service: Service,
) => Layer.Layer<Id>

Layer.effect: <Id, Service, E, R>(
  tag: Context.Tag<Id, Service>,
  service: Effect.Effect<Service, E, R>,
) => Layer.Layer<Id, E, R>
```

`Effect.provideService` removes the tag identifier `Id`, never the service value
type, from requirements. `Layer.mergeAll` requires a non-empty tuple and infers
unions of output, error, and input channels. `Layer.provide` and
`Layer.provideMerge` use exact upstream Layer-to-Layer semantics. Do not
approximate observable inference with `unknown`.

Add curated roots only as required, eventually covering:

- `effect/Cause`, `Exit`, `Fiber`, `FiberId`, `Scope`, `Chunk`;
- `Option`, `Either`, `Duration`, `Function`;
- `Context`, `Layer`, `Runtime` where needed;
- `Ref`, `Deferred`, `Queue`, `PubSub`;
- `Stream`, `Sink`, and the scoped/async Stream operations used by host modules;
- `Config`, `Console`, `Logger`, `Random`, and test services when first consumed.

Minimum Effect operations before host APIs:

- `async`, `runCallback`, `exit`, `matchCauseEffect`;
- `provide`, `provideService`, `provideServiceEffect`;
- a pinned service-access primitive such as `contextWith` and the supported
  `serviceFunctionEffect` family;
- `catchAll`, `mapError`, `tap`, `asVoid`, `ensuring`, `onInterrupt`;
- correct `acquireRelease`, `scoped`, `forkScoped`, and Fiber observation;
- `all` and only the Layer operations required for live/test composition.

### Checker and fixtures

Before trusting public fixtures, prove that `blitz check --strict` supports:

- public virtual declaration resolution;
- namespace imports;
- overload sets and generic inference;
- optional and readonly properties;
- variance-sensitive assignability;
- `Exclude`, `ReadonlyArray`, records, conditional and mapped types used by the
  curated declarations;
- missing-import/member errors without fallback to `any`.

Add positive and negative fixtures run by both upstream `tsc --strict` and
`blitz check --strict`. Cover Effect variance, tag identity, service access,
Layer union inference, Scope removal, finalizer typing, Fiber/Exit typing,
Stream/Chunk typing, and absence of `any`.

Every newly declared runtime export needs an executable compatibility program,
including synchronous and delayed `Effect.async`, cancellation exactly once,
Layer success/failure, Cause/Exit observation, and root/deep import identity.

### Files and completion gate

Update curated declarations, all synchronized Effect support lists, declaration
tests, checker fixtures, runtime compatibility tests, generated assets, and the
complete graph. Workstream 1 is complete only when generation is deterministic,
all declared operations execute, and `generate --check` passes.

---

## 2. Correct the application type model before expanding it

The current `Router` index of `unknown`, `AppDescription<Services = unknown>`,
public `layers` array, intersection-based service accumulation, and mismatch
between method and curried implementations must not be extended.

### Transitional corrected contract

Until typed RPC lands, use an opaque router identity and preserve every Layer
channel:

```ts
export interface Router {
  readonly _tag: "BlitzRouter"
}

export interface AppDescription<
  R extends Router = Router,
  Requirements = never,
  LayerError = never,
> {
  readonly rpc: R
}

export declare const make: <R extends Router>(input: {
  readonly rpc: R
}) => AppDescription<R>

export declare const provide: <ROut, E, RIn>(
  layer: Layer.Layer<ROut, E, RIn>,
) => <R extends Router, Required, ExistingError>(
  app: AppDescription<R, Required, ExistingError>,
) => AppDescription<
  R,
  Exclude<Required, ROut> | RIn,
  ExistingError | E
>
```

Keep layer storage private. If a method form remains temporarily, it must have
exactly the same generics and return a new frozen description. Prefer pipeable
free functions in the final API.

Do not claim that `serve` dispatches typed RPC until workstreams 10 and 11 are
complete. Consolidate duplicate app declarations into one generator-owned
contract. Add negative fixtures proving that missing requirements and Layer
errors are not erased.

---

## 3. Build the asynchronous runtime kernel

### Replace synchronous pseudo-Effects

The current app bridge wraps `Vm::block_on` work in `Effect.sync`. Replace it
with private registration/cancellation primitives wrapped by `Effect.async`.
`Vm::block_on` may remain in the outer embedder driver and legacy compatibility
paths, never inside an Effect-native host operation.

### Generational roots, operations, and resources

Add releasable generational `HostRoot` slots in the engine. Active roots
participate in GC; released roots are cleared before slot reuse. Stale
`(slot,generation)` values resolve to nothing.

Add a VM-thread `OperationRegistry` with all valid race transitions:

```text
Registered -> Running
Registered -> CancelRequested
Registered -> Completed
Running -> Completed
Running -> CancelRequested
CancelRequested -> Cancelled
CancelRequested -> Completed
Completed -> Closed
Cancelled -> Closed
```

The VM-thread transition that first claims the terminal outcome determines
whether completion resumes the Effect. Every later event is stale. Cancellation
may win before task registration, while running, or after a completion event has
been queued.

Each entry owns an operation kind, rooted completion callback, liveness handle,
and cancellation token. Background tasks receive only owned inputs, operation
IDs, cancellation tokens, and an event sender. Completion events carry typed
owned Rust payloads.

JavaScript-visible private IDs are frozen records such as
`{ index: number, generation: number }`, never a potentially lossy packed JS
number.

Completion dispatch must:

1. validate slot and generation;
2. reject stale or cancelled completion;
3. build the JavaScript result on the VM thread;
4. invoke the rooted callback once;
5. release the root and liveness resource;
6. ignore repeated completion safely.

### Scheduler and standards globals

Retain `TimerScheduler<RuntimeEvent>` inside `RuntimeHandle`. Add runtime events
for timer wakeups, Effect scheduler wakeups, operation completion/cancellation,
and signals. Install standards-compatible `queueMicrotask`, timers, intervals,
and monotonic `performance.now()` in `blitz-runtime`, not the engine.

One event-loop turn is:

1. receive one host event;
2. dispatch it on the VM thread;
3. flush all VM jobs/microtasks;
4. process cancellation and root transitions;
5. perform GC maintenance;
6. recompute liveness;
7. block only while referenced work remains.

Intervals coalesce wakeups and never accumulate an unbounded backlog.
Operation/data completions wait for bounded queue capacity; only idempotent
scheduler wake notifications may be coalesced.

### Private host ABI

Advance the immutable host bridge ABI for the new runtime kernel. The bridge
may remain a non-enumerable, non-writable host global used only by private
prelinked adapters; user modules must never resolve its private specifiers.
It exposes narrow capabilities (`runtime`, `operations`, `fs`, `http`, and so
on), not one stringly typed dispatcher.

Adapters use the exact pinned callback contract:

```ts
Effect.async((resume, signal) => {
  const id = Host.start(input, signal, result => resume(decode(result)))
  return Effect.sync(() => Host.cancel(id))
})
```

The host primitive inspects or translates the signal on the VM thread; no
JavaScript `AbortSignal` is retained by a background task. Synchronous
completion, an already-aborted signal, cancellation before task registration,
and a completion racing cancellation all follow the same terminal-claim rule.

Install and validate the complete bridge before graph registration. The current
runtime installs some state after graph registration, so the implementation must
either make every post-registration action infallible or extend the transaction
to graph records, declarations, host descriptors, channels, forwarding tasks,
roots, globals, and the singleton marker. Add failure injection immediately
after graph registration.

The production installation path accepts only a complete
`RuntimePrelinkedBundle`. Remove manifest-only `RuntimeGraphInstallPlan::prepare`
from production use or mark it test-only. Before registration, hash every public
declaration source and compare it with the manifest's `declaration_sha256` in
addition to existing graph/source validation. Roll back all state on
pre-commit failure and start no background task before commit.

### Kernel acceptance

Test root reuse, GC release, synchronous/delayed completion, all cancellation
orders, stale generations, malformed IDs, queue saturation, runtime drop,
shutdown with pending work, timer ordering, unref liveness, microtask ordering,
post-registration rollback, declaration-hash mismatch before initializers, and
proof that callbacks execute only on the VM thread.

---

## 4. Implement root lifecycle, signals, and shutdown

`RuntimeHandle` owns one root state:

```text
NotStarted
  -> Running
  -> InterruptRequested
  -> RootCompleted
  -> DrainingFinalizers
  -> Exited
```

A second root is rejected before starting an Effect. A referenced root resource
keeps the runtime alive even without current host I/O. Before the public facade
can land, the kernel must expose VM-thread root control that lets the JavaScript
facade register and retain exactly one upstream fiber observer, request
interruption, report its `Exit`, and keep the outer driver alive until
finalizers settle. JavaScript still constructs and starts the real Effect fiber;
Rust does not interpret it. `runMain` cannot be implemented as a standalone
facade over the current `RuntimeHandle`.

The first SIGINT/SIGTERM requests root interruption, stops new ingress, and
continues driving finalizers and operation cancellation. A second signal may
enter documented forced shutdown. No library calls `std::process::exit`.

Exit mapping is owned by the CLI/embedder boundary:

- success: `0`;
- expected failure or defect: `1` unless configured otherwise;
- SIGINT interruption: `130`;
- SIGTERM interruption: `143`.

Report one complete Cause; never report interruption as an ordinary expected
error. Close the root liveness resource only after its Exit is observed,
finalizers settle, failure reporting runs once, and the final status is stored.

Expose Rust embedder operations such as `drive`, `run_once`, `shutdown`, and
`exit_status`, but do not expose Rust representations of Effect, Layer, or App.
`RuntimeOptions` gains nonzero queue capacities, shutdown policy, immutable
startup process data, permission policy, signal policy, and terminal policy.

Acceptance covers success, expected failure, defect, interruption during every
pending operation, nested finalizers, duplicate roots, scripts without
`runMain`, and deterministic embedder exit status.

---

## 5. Implement the full `blitz:runtime` boundary

### Public contract

```ts
// Initial runtime milestone. Expand only in the same workstream that ships
// each service tag, facade, Layer, host ABI, and tests.
export type RuntimeServices = never

export interface RuntimeInitError {
  readonly _tag: "RuntimeInitError"
  readonly reason:
    | "AlreadyRunning"
    | "MissingHostPrimitive"
    | "AbiMismatch"
    | "PermissionConfiguration"
    | "Initialization"
  readonly message: string
}

export interface RunMainOptions {
  readonly disableSignalHandlers?: boolean
  readonly reportFailure?: (cause: Cause.Cause<unknown>) => void
}

export declare const layerLive:
  Layer.Layer<RuntimeServices, RuntimeInitError>

export declare const provideLive: <A, E, R>(
  program: Effect.Effect<A, E, R>,
) => Effect.Effect<
  A,
  E | RuntimeInitError,
  Exclude<R, RuntimeServices>
>

export declare const runMain: <A, E>(
  program: Effect.Effect<A, E, never>,
  options?: RunMainOptions,
) => void
```

`RuntimeServices` is the actual union of shipped service identifiers, never an
empty marker or `unknown`. Before the first service exists, `layerLive` is an
empty Layer and the union is `never`.

`runMain` is the only standard-library operation that executes an Effect. It
starts exactly one root, observes the complete Exit, interrupts on configured
signals, waits for finalizers, reports once, and stores process status. Library
code never calls `runMain` or `runPromise`.

`provideLive` is explicit: it supplies only listed runtime services and cannot
grant unrelated requirements or bypass permission policy. Tests normally use
module-specific deterministic Layers instead.

---

## 6. Implement full `blitz:path`

The module is pure. It performs no I/O and has no Effect requirement.

### Surface

Provide `join`, explicit-base `resolve`, `normalize`, `relative`, `dirname`,
`basename`, `extname`, `isAbsolute`, `parse`, `format`, separator, and delimiter.
Export deterministic `posix` and `windows` namespaces. The common namespace may
select the runtime platform, but explicit namespaces behave identically on all
hosts.

Do not let pure `resolve` read ambient cwd:

```ts
resolve(base: string, ...segments: ReadonlyArray<string>): string
```

URL conversion is fallible:

```ts
export interface PathError {
  readonly _tag: "PathError"
  readonly operation: "fromFileUrl" | "toFileUrl"
  readonly reason:
    | "NotFileUrl"
    | "InvalidEncoding"
    | "UnsupportedAuthority"
    | "RelativePath"
    | "InvalidPath"
  readonly input: string
  readonly message: string
}

fromFileUrl(url: URL): Either.Either<string, PathError>
toFileUrl(path: string): Either.Either<URL, PathError>
```

Specify POSIX authorities, Windows drive letters, UNC paths, separators,
trailing separators, empty inputs, dot segments, extensions, and non-ASCII
round trips. Withhold URL functions until runtime URL conformance passes.

Implement in bundled JavaScript unless measurement proves a Rust helper is
needed. Test table-driven behavior against the documented contract, not host
`std::path` accidents.

---

## 7. Implement full `blitz:fs`

### Public model

Use `Path = string | URL`, `Uint8Array` for bytes, and distinct binary/text
methods. Export an immutable `FileSystem` service/tag, pipeable accessors,
`layerLive`, and `layerMemory`.

Full operations:

- read/write binary and UTF-8 text;
- bounded reads and explicit durability for writes;
- stat/lstat, real path, copy, rename, remove, and directory creation;
- scoped file handles with positional read/write, stat, sync, truncate, and
  idempotent early close;
- directory traversal as a finite Stream;
- file watching as a scoped bounded Stream;
- atomic replacement with explicit durability/replace policy.

`FileSystemError` is a readonly tagged value with stable `reason`, `operation`,
optional normalized path, and message. Reasons include not found, permission,
already exists, invalid path, not/is directory, too many files, too large,
invalid encoding, closed, unsupported, watch overflow, and I/O.

`exists` returns `false` only for `NotFound`; permission and other failures stay
in the error channel.

All host calls pass a permission check when the Effect runs. `readTextFile`
composes `readFile` and strict decoding in JavaScript. Bounded allocation is
enforced before and during reads. Returned and input bytes are defensively
copied at trust boundaries.

`layerMemory` snapshots inputs, uses Effect-owned state rather than a module
global, copies every byte array, uses one documented portable path syntax, and
performs no host calls.

Implement in slices: reads; handles; metadata; mutations/atomic writes;
directory Streams; watchers. Each slice gets cancellation, finalization,
permissions, race, queue, temporary-directory, and memory-Layer tests.

---

## 8. Implement `blitz:process` and separate `blitz:subprocess`

### Current process

`Process` exposes an immutable startup snapshot, Effectful cwd/environment
access, typed environment decoding, exit-code mutation, controlled root exit,
and scoped signal Streams. Environment absence is `Option.none`; permission or
I/O failures remain typed errors. `exit` requests root interruption and permits
finalizers; it never performs immediate host termination.

The mutable Node-compatible global remains a separate compatibility adapter and
is not read at module initialization by Blitz libraries.

### Subprocess

Keep child creation in `blitz:subprocess`. Use immutable command/options records,
no implicit shell, explicit stdio modes, optional bounded stdin/stdout/stderr,
and a scoped `ChildProcess` resource.

- `spawn` requires `Scope`;
- nonzero child exit is a successful `ExitStatus`;
- `wait` interruption unregisters only that waiter;
- default scope exit kills and waits;
- `kill`, stdin end, and close are idempotent;
- output Streams are single-owner and bounded;
- unsupported signals/platform behavior are typed;
- command, cwd, environment, and inherited-resource permissions are checked at
  execution.

Land process snapshot/environment first, then signals, then subprocess spawn and
wait, pipes, and cleanup.

---

## 9. Implement `blitz:terminal` and logging integration

`Terminal` is a replaceable service with Effectful capabilities, dimensions,
ordered writes, input byte/text/key/line Streams, scoped raw mode, scoped
alternate screen, and interruptible prompts.

Output uses a bounded writer queue and acknowledges acceptance/flush according
to documented policy. Input is pull-based or bounded; it never silently drops.
Each low-level input Stream has one owner. Shared fan-out requires upstream
`PubSub`. Concurrent prompts fail with a typed `PromptBusy` error.

Raw and alternate-screen acquisition restore the prior state on success,
failure, defect, and interruption, including nested acquisitions. Prompt
interruption restores cursor/mode/input ownership.

Refactor the current rustyline callback implementation into a legacy adapter.
The new backend emits owned events, acknowledges writes, stops ingress on Scope
closure, and retains no callback roots except for legacy compatibility.

Do not create a competing logger framework. Supply live implementations for
upstream `effect/Console` and `effect/Logger`; preserve annotations, spans, and
structured causes. Add deterministic capture Layers for tests. The global
`console` remains synchronous compatibility/REPL presentation.

---

## 10. Implement `blitz:http` and `blitz:websocket`

### HTTP contracts

Use standard `Request`, `Response`, `Headers`, `URL`, and `AbortSignal` only
after their runtime implementations satisfy the required semantics.

A handler's expected error cannot become an HTTP response implicitly. `serve`
therefore requires either an infallible handler or an explicit typed mapper:

```ts
import type { WebSocketUpgrade } from "blitz:websocket"

export interface RequestContext {
  readonly requestId: string
  readonly remoteAddress: {
    readonly hostname: string
    readonly port: number
  }
  readonly signal: AbortSignal
  readonly body: Stream.Stream<Uint8Array, HttpBodyError>
}

export interface HttpServerResource {
  readonly localAddress: string
  readonly shutdown: Effect.Effect<void, never>
}

export type Handler<E = never, R = never> = (
  request: Request,
  context: RequestContext,
) => Effect.Effect<Response | WebSocketUpgrade, E, R>

export interface ServeOptions<E, R, MapperR = never> {
  readonly hostname?: string
  readonly port?: number
  readonly handler: Handler<E, R>
  readonly onHandlerError?: (
    error: E,
    request: Request,
    context: RequestContext,
  ) => Effect.Effect<Response, never, MapperR>
  readonly requestTimeout?: Duration.DurationInput
  readonly maxRequestBodyBytes?: number
  readonly maxResponseBodyBytes?: number
  readonly queueCapacity?: number
}

export declare const serve: <E, R, MapperR = never>(
  options: ServeOptions<E, R, MapperR>,
) => Effect.Effect<never, HttpListenError, HttpServer | R | MapperR>

export declare const listen: <E, R, MapperR = never>(
  options: ServeOptions<E, R, MapperR>,
) => Effect.Effect<
  HttpServerResource,
  HttpListenError,
  HttpServer | R | MapperR | Scope.Scope
>

export declare const request: (
  input: RequestInfo | URL,
  init?: RequestInit,
) => Effect.Effect<Response, HttpClientError, HttpClient | Scope.Scope>

export declare const withResponse: <A, E, R>(
  input: RequestInfo | URL,
  use: (response: Response) => Effect.Effect<A, E, R>,
  init?: RequestInit,
) => Effect.Effect<A, HttpClientError | E, HttpClient | R>
```

`HttpListenError`, `HttpClientError`, and `HttpBodyError` are readonly tagged
errors with stable reasons for invalid input, permissions, DNS/connect/TLS,
timeout, limits/overflow, disconnect, protocol, bind, and I/O.

Expected errors use the mapper or a documented redacted 500 policy. Defects are
logged and redacted. Disconnect, timeout, and shutdown interrupt the request
Scope and do not become ordinary handler failures.

Provide both:

- `serve`, a never-ending internally scoped server Effect;
- `listen`, a scoped server resource for advanced composition.

Each request owns a child Scope. Request and response bodies use bounded
Streams/adapters. Client `request` requires `HttpClient | Scope.Scope` while a
streaming body is live; `withResponse` is the safe convenience that scopes and
closes it.

### WebSocket contracts

Use discriminated text/binary messages, typed close options, a scoped
`Connection`, single-owner message Stream, backpressured Effectful send, and
idempotent close. A second message consumer fails. Explicit broadcast uses
upstream `PubSub`.

Inbound overflow follows a documented close/error policy; outbound send waits
for capacity instead of `try_send`. Copy binary payloads at trust boundaries.
HTTP upgrade is an explicit typed decision owned by the request Scope. Standard
global WebSocket remains event/callback based.

The current fully buffered `HttpRequestEvent.body`, oneshot response, and direct
callback dispatch cannot satisfy this contract. First replace them with
per-request generational resources, bounded body ingress/response egress,
disconnect events, and timeout cancellation. Then implement client body
streaming, server child Scopes, error mapping, WebSocket client,
upgrade/accepted connection, and graceful shutdown. Preserve legacy APIs as
adapters over shared resources.

---

## 11. Rebuild App/RPC and complete later runtime capabilities

### Typed RPC

Build RPC in JavaScript/TypeScript over Effect and Stream. Rust owns bounded wire
validation and transport resources, not handlers or a second procedure model.

Define generic:

- `Decoder<Input, DecodeError, DecodeRequirements>` from `unknown`;
- `Encoder<Output, EncodeError, EncodeRequirements>` to JSON-safe values;
- query/mutation procedures returning Effect;
- subscription procedures returning Stream;
- middleware that accumulates error and requirement channels;
- `Router<Routes>` preserving route keys and procedure types;
- caller projections for per-route input, output, domain error, and requirements.

Never collapse router requirements to `unknown`. Domain failures are readonly,
tagged, and JSON-safe. Defects are logged and redacted. Batches, messages, and
subscriptions are bounded. Disconnect interrupts the procedure child Scope.

### Application model

`App<Routes, Requirements, LayerError>` is an opaque frozen description. It does
not publicly expose its Layer array. `App.provide(layer)` returns a new value,
removes provided services, adds Layer inputs, and unions Layer errors.
`App.serve(app, options)` is pipeable, builds Layers once in the root Scope, and
delegates transport lifecycle to `blitz:http`. Every request/subscription has a
child Scope. Shutdown stops ingress, interrupts children, waits for finalizers,
and closes resources.

Remove or move the competing public Rust `App`, `Layer`, `Router`, and procedure
execution exports behind an explicitly host/internal or legacy namespace. Rust
embedder APIs may configure transport and policy but must not pretend to be
Effect APIs. The migration explicitly replaces the current app facade and
declaration, `app_transport.rs`, Rust `app.rs` public exports, Rust RPC execution
exports, related tests, and README examples; it is incomplete until `App.serve`
actually dispatches typed RPC and subscriptions rather than merely binding a
listener.

### Remaining capability subplans

Each capability below is a separate post-Phase-10 workstream, not one combined
patch.

#### `blitz:crypto`

Export a `Crypto` service/tag and live/test Layers. Operations include bounded
`randomBytes`, digest, MAC, KDF, authenticated encrypt/decrypt, key generation,
key import, and explicitly authorized export. Algorithm options are readonly
discriminated objects with no weak defaults. `SecretKey` is an opaque
resource-backed value with redacted inspection, scoped destruction where the
backend supports it, and no implicit serialization. `CryptoError` distinguishes
permission, unsupported algorithm, invalid input/key, authentication failure,
entropy, and host failure. Test standard vectors, malformed inputs, nonce/key
sizes, redaction, non-exportable keys, cancellation, zeroization best effort,
and deterministic test entropy. Web Crypto remains Promise-based compatibility.

#### `blitz:os`

Export an `Os` service/tag. Immutable snapshot effects expose platform,
architecture, endianness, hostname, CPU count, home directory Option, and temp
base. Resource operations create scoped temporary files/directories with
idempotent recursive cleanup. `OsError` separates permission, unavailable,
invalid input, cleanup, and I/O. Capture no ambient metadata at module import.
Test deterministic Layers, permission denial, non-UTF-8 host data policy,
cleanup on every Exit, and temporary-name collision/cancellation.

#### `blitz:dns`

Export a `Dns` service/tag with `lookup`, reverse lookup when supported, and a
scoped resolver configuration Layer. Results are readonly address records with
family and optional canonical name. Options cover family, timeout, and result
ordering. `DnsError` separates invalid hostname, permission, not found,
timeout, cancellation, and I/O. Caching/deduplication is a replaceable Layer,
not mutable module state. Test localhost/loopback, timeout, cancellation race,
TTL/cache policy, permissions, IPv4/IPv6 ordering, and deterministic fake
resolvers.

#### `blitz:tcp`

Export a `Tcp` service/tag. `connect` returns a scoped `TcpConnection`; `listen`
returns a scoped bounded Stream of accepted connections. Connections expose
readonly local/remote addresses, a single-owner bounded byte Stream,
backpressured writes, half-shutdown, and idempotent close. Typed errors cover
address, permission, refused, timeout, overflow, disconnect, closed, and I/O.
Test partial reads/writes, backpressure, listener shutdown, accept/connect
cancellation, half-close, stale resource IDs, and loopback permissions.

#### `blitz:udp`

Export a `Udp` service/tag. `bind` acquires a scoped socket with a bounded
Datagram Stream, backpressured `send`, local address, multicast options behind
explicit permission, and idempotent close. Datagrams carry copied bytes and a
typed peer address. Enforce per-datagram and aggregate limits; never truncate or
silently drop. Test ordering guarantees, overflow policy, cancellation,
multicast portability, malformed addresses, and loopback behavior.

#### `blitz:tls`

Build a `Tls` service/tag over scoped TCP resources. Client/server options use
explicit server name, ALPN, certificates, keys, trust roots, and handshake
timeout. Verification is secure by default; any insecure mode is visibly named,
permission-gated, and excluded from defaults. Connections retain TCP
backpressure/Stream ownership. `TlsError` separates options, permission,
certificate, verification, handshake, protocol, timeout, disconnect, and I/O.
Test trusted/untrusted chains, hostname mismatch, ALPN, mutual TLS, cancellation,
secret redaction, and graceful close.

#### `blitz:worker`

Export a `WorkerRuntime` service/tag. `spawn(specifier, options)` returns a scoped
worker with bounded message/error Streams, backpressured `postMessage`, and
idempotent interruption-aware termination. Structured cloning occurs at VM
boundaries into owned serialized data; no VM value crosses threads. Every worker
has its own VM, one runtime graph installation, permissions attenuated from its
parent, and deterministic shutdown. Test clone rejection, queue saturation,
startup failure, module visibility, permission attenuation, termination races,
resource leaks, and independent Effect identity per VM.

#### `blitz:test`

Compose upstream Effect test services instead of cloning them. Export helpers
and Layers for deterministic Clock/timers, Random/crypto, Console/Logger,
filesystem, process, terminal, DNS/network, HTTP/WebSocket, workers, and
permissions. Live I/O requires an explicit live Layer. Provide resource/operation
leak assertions, event advancement, scripted completions, failure injection,
and bounded queue controls. Test that fixtures are isolated, deterministic,
parallel-safe at the Rust level while JS remains single-threaded, and incapable
of accidental OS access.

Every capability gets public declarations, a facade, private adapters, manifest
entries, permission variants, typed owned Rust payloads, live and deterministic
Layers, interruption/finalization/backpressure tests, raw-engine absence, and a
regenerated complete graph.

Adopt `@effect/platform` only if a pinned version interoperates cleanly and is
vendored into the same graph without duplicating Effect identity.

### Compatibility and deprecation

Build Web and Node compatibility adapters over the same host resources without
changing their Promise/event/callback semantics. Mark `Blitz.*`, legacy
terminal callbacks, and the Rust application model deprecated only after stable
replacement APIs and migration examples exist. Remove them only in a versioned
breaking release.

---

## Per-workstream asset and validation protocol

Every Effect export, alias, public facade, or private adapter change must:

1. update its sole generator-owned source/declaration input;
2. update synchronized supported-export lists;
3. update the stdlib manifest with visibility, dependencies, hashes, ABI, and
   host primitive requirements;
4. regenerate Effect and stdlib assets;
5. precompile the complete runtime graph atomically;
6. byte-compare it with `generate --check`;
7. run declaration success and negative fixtures under both checkers;
8. execute every newly declared runtime operation;
9. test private-import rejection and raw-engine absence;
10. prove zero bundled parses, import extraction, typechecks, ESTree links, and
    compilations at startup;
11. test interruption, finalization, bounded queues, stale completion, and
    failure-before-side-effects where applicable.
12. prove `provideLive` builds the aggregate Layer once and closes its Scope on
    success, failure, defect, and interruption.
13. test root liveness with no host resource and signal interruption while
    finalizers are running.
14. for networking/App changes, test disconnect cancellation and prove
    `App.serve` dispatches typed RPC/subscriptions rather than only binding.
15. retain a compatibility regression for every documented source transform,
    including the temporary WeakMap retention behavior.

Complete gate:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets
cargo test --workspace
pnpm --dir tools/effect-assets generate
pnpm --dir tools/effect-assets typecheck
pnpm --dir tools/effect-assets test
pnpm --dir tools/effect-assets verify
cargo run -p effect-bytecode -- generate --check
git diff --check
```

Clippy warnings that predate a workstream may be recorded, but new code must not
introduce unreviewed warnings.

## Reviewable commit sequence

1. Checker fixtures and corrected core declarations.
2. Effect alias/runtime compatibility expansion and regenerated graph.
3. Generational host roots and scheduler integration.
4. Generic operation registry and synthetic cancellation tests.
5. Root lifecycle, signals, CLI exit handling, and `blitz:runtime`.
6. Pure `blitz:path`.
7. Filesystem reads plus live/memory Layers.
8. Remaining filesystem slices.
9. Process, signals, and terminal output/input slices.
10. Subprocess.
11. HTTP client/server lifecycle.
12. WebSocket lifecycle.
13. Typed RPC and App rebuild.
14. Crypto/OS/DNS/socket/TLS/worker/test modules one capability at a time.
15. Compatibility migration and eventual versioned deprecation.
