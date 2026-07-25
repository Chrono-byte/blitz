# Effect-native standard-library implementation plan

## Purpose

This plan implements the API direction in
[`EFFECT_STDLIB_DESIGN.md`](EFFECT_STDLIB_DESIGN.md) incrementally on the
current Blitz runtime. It is ordered to prove the difficult lifecycle and type
contracts before adding a broad module surface.

The corrected contracts and full module plans for all eleven remaining
API/runtime workstreams are specified in
[`EFFECT_RUNTIME_API_EXPANSION_PLAN.md`](EFFECT_RUNTIME_API_EXPANSION_PLAN.md).
That plan takes precedence where older signatures below are narrower or stale.

The first shippable vertical slice is:

1. the required real upstream Effect operations;
2. a cancellable host-operation bridge;
3. `blitz:runtime` as the root execution boundary;
4. `blitz:path` pure path operations; and
5. `blitz:fs` read operations with live and deterministic test Layers.

Do not implement all modules in one change. Each phase below must leave the
runtime usable and independently testable.

## Current baseline

Already present:

- `effect@3.16.7` is pinned in `tools/effect-assets`.
- The generated upstream source graph is canonicalized under
  `blitz-internal:effect/*` and committed.
- The engine now supports validated, parser-free prelinked module records,
  canonical live-binding IDs, reserved module indices, and mixed
  source/prelinked imports.
- `effect`, selected `effect/*` modules, `blitz:match`, `blitz:app`,
  `blitz:fs`, `blitz:http`, `blitz:runtime`, `blitz:terminal`,
  `blitz:websocket`, and private adapters are installed from one committed
  parser-free runtime graph by `blitz-runtime`.
- Effect and stdlib inputs plus the final runtime graph have deterministic
  generation and verification tools.
- Curated declarations exist for Effect, Context, Layer, Stream, Scope,
  Schedule, and Match.
- The runtime has a bounded event queue, generational resource registry,
  Tokio-backed host tasks, microtask checkpoints, HTTP/WebSocket resources,
  terminal events, and cancellation-safe timer primitives.
- The raw engine remains free of Blitz and Effect installation.

Important gaps:

- The curated Effect API is too small for service access, asynchronous
  registration, robust interruption, full scoped composition, and Stream
  adapters.
- There is no generic host-operation registry that can settle an Effect on the
  VM thread and discard cancellation races safely.
- Existing public host APIs are globals and callback/Promise oriented.
- `blitz:app` uses the versioned `__blitz_host_v1` bridge, but its host thunks
  still block through `Vm::block_on` inside `Effect.sync`; it is not a model for
  the cancellable asynchronous stdlib bridge.
- `blitz:path` is not yet part of the committed public runtime graph; the other
  runtime modules listed above are installed and remain under API expansion.
- The TypeScript checker still supports only part of upstream Effect's type
  language, so every declaration expansion needs focused checker fixtures.

The historical application-serving vertical slice is retained in
[`legacy/EFFECT_APP_SERVE_IMPLEMENTATION_PLAN.md`](legacy/EFFECT_APP_SERVE_IMPLEMENTATION_PLAN.md).
Its single-graph and scoped-resource decisions remain useful, but its starting
state is not current.

## Non-negotiable architecture

### One prelinked runtime graph and one Effect identity

Generate one runtime graph containing canonical upstream Effect plus stable
public/private Blitz stdlib JavaScript. Public aliases may increase, but no
facade may bundle a second copy of Effect internals. Context tags, fibers,
scopes, and scheduler state retain identity across all imports.

### JavaScript constructs Effects

Rust exposes host primitives and owned completion events. Blitz JavaScript
facades use upstream `Effect.async`, `Effect.acquireRelease`, `Effect.scoped`,
Context, Layer, and Stream operations. Rust must not emulate an Effect value.

### VM-thread ownership

Background tasks contain no JavaScript `Value`, `Rc`, `RefCell`, callback, or
VM reference. They send owned completion data to the bounded runtime event
queue. The VM thread resolves a generational operation ID and invokes the
stored rooted callback.

### Lazy and interruptible operations

Calling a stdlib function constructs a value only. Host work starts when the
Effect runs. Interruption requests cancellation, closes its liveness resource,
and prevents a racing completion from invoking a stale callback.

### Compatibility separation

Keep existing `Blitz.*`, Web, and future `node:*` behavior intact during the
migration. New `blitz:*` modules become the recommended API but initially
adapt the same host infrastructure.

### Precompiled asset discipline

The rules in
[`EFFECT_FULL_PREBUNDLING_PLAN.md`](EFFECT_FULL_PREBUNDLING_PLAN.md) are a
prerequisite for shipping new Effect-backed stdlib modules:

- The committed runtime artifact is one complete prelinked graph, not a
  bytecode-only cache. It contains the canonical Effect graph plus every stable
  public `blitz:*` facade and private `blitz-internal:*` JavaScript adapter.
- The graph contains verified chunks, ordered dependency edges, stable module
  indices, canonical live-binding IDs, imports, exports, roots, visibility,
  source integrity, Effect provenance, and runtime/host ABI metadata.
- Runtime startup may decode, validate, install host primitives, allocate slots,
  install records, and execute initializers. It must not parse, import-scan,
  typecheck, link through ESTree, or compile bundled source.
- Source may remain committed for reproducibility, licenses, hashes, and
  diagnostics, but it is never a runtime fallback. Bundle failure is atomic and
  fatal before records become visible or any initializer executes.
- Adding an Effect export, alias, canonical module, Blitz facade, or private
  adapter changes the graph input and requires deterministic regeneration and
  byte-for-byte verification of the final bundle.
- All public aliases and Blitz modules resolve into the same canonical Effect
  graph. The production installer accepts only the complete runtime bundle;
  manifest-only install plans are test-only or removed. It hashes supplied
  public declarations and validates each `declaration_sha256` before graph
  visibility. `blitz-runtime` installs exactly one runtime graph per VM and
  rejects a second installation before mutation; it never bundles or registers
  a second Effect copy.
- All post-registration initialization is infallible, or one transaction rolls
  back graph records, declarations, host descriptors, roots, channels, tasks,
  globals, and the singleton marker. Failure injection covers the point after
  graph registration.
- Rust host primitives are installed before graph evaluation and must match the
  artifact's host ABI version. Private bridge visibility is enforced by the
  manifest and user resolution.
- No asset generator runs from `build.rs`, Cargo build, or runtime, and no
  generation step may require the network.

Phase 0 declaration work may proceed independently. Before Phase 1 runtime
surface expansion is merged, complete Phases 3–5 of the full-prebundling plan,
establish the stdlib manifest/visibility pipeline, and cut `blitz-runtime` over
to the final runtime graph artifact.

### Runtime graph input manifest

Establish this before adding the first stdlib module. Each Blitz entry records:

- canonical specifier and stable source path;
- public or private visibility;
- source and declaration hashes, with no declaration for private entries;
- runtime ABI, host bridge ABI, and targeted Effect version;
- ordered static dependencies and whether a host primitive is required; and
- license/provenance metadata where applicable.

“Bundled stdlib” means exactly the entries in this manifest. Every manifest
entry must be present, prelinked, reachable from a declared root or bundled
edge, and covered by integrity checks; no unmanifested module may appear.

One generator owns this manifest and embedding metadata. One checked Rust
workspace tool consumes it together with the verified Effect manifest and owns
the versioned artifact consumed by `include_bytes!`. Its `--check` mode must
regenerate in memory and compare the complete output. Registration tests reject
duplicate specifiers, missing host primitives, unresolved graph edges, invalid
visibility, stale hashes, and Effect/runtime/host ABI mismatches before records
become visible or any initializer runs.

Every phase that adds or changes a bundled module must update its manifest
entry, regenerate the complete artifact, run `--check`, and extend the explicit
zero-parser/import-extraction/typechecker/compiler regression. No module phase
may defer these steps to a later cleanup.

## Phase 0: freeze contracts with fixtures

Before runtime implementation, add compile-only TypeScript fixtures defining
the first supported API and its negative cases.

### Public contracts

#### `blitz:runtime`

Start with a deliberately small execution API:

```ts
export interface RunMainOptions {
  readonly disableSignalHandlers?: boolean
}

export declare const runMain: <Success, Error>(
  program: Effect.Effect<Success, Error, never>,
  options?: RunMainOptions,
) => void

export declare const layerLive: Layer.Layer<RuntimeServices, RuntimeInitError>

export declare const provideLive: <Success, Error, Requirements>(
  program: Effect.Effect<Success, Error, Requirements>,
) => Effect.Effect<
  Success,
  Error | RuntimeInitError,
  Exclude<Requirements, RuntimeServices>
>
```

Validate exact Effect variance with both checkers. `runMain` accepts only a
closed Effect. `provideLive` explicitly supplies the actual shipped runtime
service union; neither API may widen unresolved requirements to `unknown`.

`RuntimeServices` is `never` in the initial runtime-only milestone and expands
only in the same change that ships a real service tag, facade, live Layer, host
ABI, and tests.

`runMain` is the only operation in this slice allowed to execute an Effect.
Libraries and stdlib modules never call it.

#### `blitz:path`

Use a small pure surface first:

```ts
join(...segments: ReadonlyArray<string>): string
resolve(base: string, ...segments: ReadonlyArray<string>): string
normalize(path: string): string
dirname(path: string): string
basename(path: string, suffix?: string): string
extname(path: string): string
isAbsolute(path: string): boolean
fromFileUrl(url: URL): Either.Either<string, PathError>
toFileUrl(path: string): Either.Either<URL, PathError>
```

Expose explicit `posix` and `windows` namespaces only after the common contract
is implemented and tested.

#### `blitz:fs`

The first filesystem surface is intentionally narrow:

```ts
export type Path = string | URL

export interface ReadFileOptions {
  readonly maxBytes?: number
}

export interface FileSystem {
  readonly readFile: (
    path: Path,
    options?: ReadFileOptions,
  ) => Effect.Effect<Uint8Array, FileSystemError>

  readonly readTextFile: (
    path: Path,
    options?: ReadTextFileOptions,
  ) => Effect.Effect<string, FileSystemError>
}

export declare const FileSystem: Context.Tag<FileSystem, FileSystem>

export declare const readFile: (
  path: Path,
  options?: ReadFileOptions,
) => Effect.Effect<Uint8Array, FileSystemError, FileSystem>

export declare const readTextFile: (
  path: Path,
  options?: ReadTextFileOptions,
) => Effect.Effect<string, FileSystemError, FileSystem>

export declare const layerLive: Layer.Layer<FileSystem>

export declare const layerMemory: (
  files?: Readonly<Record<string, string | Uint8Array>>,
) => Layer.Layer<FileSystem>
```

`layerMemory` is deterministic and public because it encourages service-based
tests. It must snapshot or defensively copy its input.

### Error contract

Define `FileSystemError` as a readonly tagged object with stable `reason`,
`operation`, optional `path`, and `message`. Do not expose `std::io::ErrorKind`
directly and do not include an untyped public `cause` in the first slice.

Map at least:

- `NotFound`
- `PermissionDenied`
- `InvalidPath`
- `IsDirectory`
- `TooManyOpenFiles`
- `FileTooLarge`
- `InvalidEncoding`
- `Io`

### Fixture matrix

Add positive and negative fixtures for:

- inferred success and tagged error channels;
- unsatisfied `FileSystem` requirements;
- requirement removal by `FileSystem.layerLive` and `layerMemory`;
- no `any` in public declarations;
- immutable options;
- `string | URL` path inputs;
- invalid option types;
- `Runtime.runMain` rejecting unresolved custom requirements.

### Phase 0 acceptance

- Both `tsc --strict` and `blitz check --strict` agree on the positive fixtures.
- Negative fixtures fail for the intended reason and location.
- No contract is weakened to accommodate a missing checker feature without a
  documented checker issue and a targeted follow-up phase.

## Phase 1: expand the curated upstream Effect surface

Add only operations required by the first slice. Avoid a broad declaration
expansion before runtime compatibility is proven.

### Required additions

First correct the already-curated models: upstream covariance, `Context.Tag`
identifier/service generic order, real Fiber types and `Fiber.await` returning
`Exit`, `acquireRelease` receiving the closing `Exit` with an infallible
finalizer, precise non-empty `Layer.mergeAll` inference, Layer-to-Layer
`Layer.provide`, Scope finalizers, and `Stream.runCollect` returning `Chunk`.
Remove local substitutes and non-upstream aliases rather than expanding them.

Then extend `effect/Effect` with the upstream runtime names and accurate curated
declarations for:

- `async`
- `runCallback` or the selected observable root-runner primitive
- `provideService` and `provideServiceEffect`
- the pinned context/service-access primitives required by generated accessors
- `catchAll`
- `mapError`
- `tap`
- `asVoid`
- `ensuring`
- `onInterrupt`
- `exit`
- `matchEffect`
- `all` or the minimum equivalent required by Layers

Extend Context/Layer only as required for service accessor implementation and
live/test Layer composition. Prefer upstream helpers over custom service
lookup logic.

Add minimal public aliases and declarations for:

- `effect/Cause`
- `effect/Exit`
- `effect/Option`
- `effect/Either`
- `effect/Function`
- `effect/Duration` if used by execution or cancellation options

Do not add Queue, PubSub, Ref, Deferred, Config, Logger, or expanded Stream
operations until a stdlib module needs them.

### Generator changes

Update all three lists together:

- `SUPPORTED` in `tools/effect-assets/scripts/generate.ts`
- `SUPPORTED` in `tools/effect-assets/scripts/verify.ts`
- `supported` in `tools/effect-assets/scripts/test.ts`

Add one declaration file per public Effect subpath. When adding `effect/Fiber`,
add the runtime alias `"Fiber.await": "_await"`, delete the local Fiber
substitute, and test root/deep Fiber identity. Generation is a strict ordered
pipeline:

1. `tools/effect-assets` deterministically writes and verifies canonical Effect
   source, declarations, manifest, licenses, hashes, and embedding metadata.
2. The stdlib asset step verifies Blitz facade and private adapter source,
   declarations, public/private visibility, hashes, and host ABI metadata.
3. The Rust runtime-graph precompiler verifies both manifests, constructs and
   instantiates their complete closed static graph, assigns stable module and
   binding IDs, compiles verified chunks, and atomically emits the final
   prelinked artifact.
4. `--check` regenerates in memory and byte-compares the complete artifact.

Each source generator is the sole writer of its generated inputs; the Rust
precompiler is the sole writer of the graph bundle. Do not revive the
transitional bytecode-only format, leave bundled stdlib modules source-backed,
or derive graph metadata at runtime.

If adding an export pulls unsupported syntax or host assumptions into the
canonical graph, fix ECMAScript correctness in `blitz-engine` or host
prerequisites in `blitz-runtime`. Do not transform away language behavior to
make a smoke test pass.

### Compatibility programs

Execute real programs covering:

- `Effect.async` synchronous resume;
- asynchronous resume after a runtime wakeup;
- cancellation finalizer exactly once;
- Context tag lookup and `Layer.succeed`;
- `Layer.effect` failure;
- `Cause`/`Exit` observation by `runMain`;
- root and deep imports sharing the same tag and fiber identities.

### Phase 1 acceptance

```sh
pnpm --dir tools/effect-assets typecheck
pnpm --dir tools/effect-assets test
pnpm --dir tools/effect-assets verify
cargo test -p blitz-runtime effect
```

Generation is deterministic, the complete prelinked graph bundle is current,
its no-parser/no-compiler runtime regression passes, and every newly declared
operation has a runtime fixture.

## Phase 2: integrate scheduler wakeups and timers

Finish the main event-loop integration before adding filesystem Effects. An
asynchronous Effect must wake an otherwise idle runtime and referenced timers
must participate in liveness.

### Runtime changes

In `crates/blitz-runtime/src/host/mod.rs`:

- add a scheduler/wakeup variant to `RuntimeEvent`;
- construct `TimerScheduler<RuntimeEvent>` with the existing event registry;
- retain scheduler state in `RuntimeHandle`;
- dispatch timer callbacks only on the VM thread;
- treat Effect scheduler wakeups as a reason to flush runnable work;
- flush VM microtasks after each host event;
- re-check liveness after callback, microtask, and cancellation work.

Install standard `queueMicrotask`, `setTimeout`, `clearTimeout`, `setInterval`,
`clearInterval`, and monotonic `performance.now()` host APIs only in
`blitz-runtime`. These globals retain standard callback behavior and are host
prerequisites for upstream Effect; they are not the Effect-native stdlib API.

### Ordering tests

Cover:

1. one host event per event-loop turn;
2. callback before the following microtask checkpoint;
3. promise and Effect work becoming runnable from a timer;
4. cancellation before wakeup;
5. cancellation racing with queued wakeup;
6. an unreferenced timer not keeping the process alive;
7. intervals not accumulating an unbounded backlog;
8. Effect sleep/fork/interruption if supported by the curated surface.

### Phase 2 acceptance

A program using upstream `Effect.async` and timers finishes without manual
polling, an interrupted pending effect exits, and no timer callback executes in
a Tokio task.

## Phase 3: generic host-operation registry

Create the reusable bridge for filesystem and later DNS, HTTP, subprocess, and
crypto operations.

### Rust types

Add a focused module such as:

```text
crates/blitz-runtime/src/host/operations.rs
```

It should provide:

- `OperationId` backed by a slot and generation;
- `OperationRegistry` owned by `RuntimeHandle`/the VM thread;
- a rooted JavaScript completion callback per active operation;
- a liveness `ResourceHandle` per active operation;
- cancellation signaling owned by the background task;
- exactly-once `complete` and idempotent `cancel` transitions;
- stale completion rejection after cancellation or slot reuse;
- diagnostic operation names;
- cleanup of all operations during runtime shutdown.

Add `RuntimeEvent::OperationCompleted` carrying only owned data. Do not put a
JavaScript callback or `Value` in the event.

Use a typed Rust completion enum rather than a JSON string for internal data.
The first variant may be filesystem-specific; generalize payloads only when a
second capability proves the abstraction.

### Private JavaScript bridge

Add private adapter source under `blitz-internal:*` as a build-time manifest
input. The graph precompiler embeds it, and runtime registers only the decoded
prelinked graph. User resolution exposes only graph `public_roots`; bundled
private dependencies are traversed by index, so direct user imports are
rejected without trusting importer text. A filesystem bridge should expose
narrow methods such as
`readFile(path, maxBytes, callback)` and `cancel(operationId)` rather than one
stringly typed universal dispatcher.

The public JavaScript facade wraps it conceptually as:

```ts
Effect.async((resume) => {
  const id = Host.readFile(path, maxBytes, (result) => {
    resume(result.ok ? Effect.succeed(result.value) : Effect.fail(result.error))
  })
  return Effect.sync(() => Host.cancel(id))
})
```

The exact upstream `Effect.async` callback contract must be copied from the
pinned release and covered by tests. Do not infer it from another Effect
version.

### Safety tests

- completion invokes the callback once;
- cancel invokes no callback;
- cancel after completion is a no-op;
- queued completion after cancel is stale;
- old IDs cannot resolve after slot reuse;
- queue saturation applies backpressure without losing completion;
- dropping `RuntimeHandle` cancels operations and releases roots;
- panic-free behavior for malformed private bridge arguments.

### Phase 3 acceptance

A synthetic delayed operation succeeds and can be interrupted through a real
Effect. Open and referenced resource counts return to zero on every terminal
path.

## Phase 4: `blitz:runtime` and root lifecycle

Implement the executable boundary before shipping host services.

### Virtual module

Add generated or checked-in Blitz-owned source and declarations as build-time
manifest inputs for:

- `blitz:runtime`
- a private runtime-control bridge

Keep Blitz facade sources in a dedicated runtime asset source directory and
record them in the stdlib manifest. Avoid adding JavaScript string literals to
`effect_assets.rs`. Effect inputs and Blitz facade inputs may have separate
manifests, but the graph precompiler combines them into one artifact and
`install_runtime` installs only that artifact atomically. The current
source-backed registration path is removed at cutover.

### Root execution

`Runtime.runMain` must:

1. reject a second root invocation clearly;
2. start the supplied Effect exactly once;
3. register a referenced root-fiber resource;
4. retain the root until completion;
5. observe the complete upstream `Exit`/`Cause`;
6. close the root resource on completion;
7. interrupt the root on shutdown signals;
8. keep driving finalizers before runtime exit;
9. report an unhandled Cause once;
10. set process exit status without calling an immediate host exit.

Select an upstream runner (`runCallback`, or a tested fiber observer) that
supports cancellation. Do not poll private fiber fields as the current IRC
example does. Advance the current immutable versioned host ABI for root and
operation control; do not reintroduce `globalThis.__blitz_app_transport`.

### Live Layer

The initial `RuntimeServices` contains only services actually implemented in
the vertical slice. Add FileSystem later in Phase 6 without inventing empty
placeholder services.

`Runtime.layerLive` is assembled in JavaScript from real Layers. Rust does not
inspect or build it.

### CLI integration

The CLI evaluates the entry module, then drives `RuntimeHandle` while a root,
referenced operation, server, terminal, timer, or queued event remains. Define
behavior for scripts that never call `runMain`: existing callback/global scripts
continue to run under legacy resource liveness.

### Phase 4 acceptance

- success exits zero after finalizers;
- expected failure and defect produce distinct useful reports;
- interruption waits for finalizers;
- a root with no host resources remains live while upstream Effect work is
  pending;
- a completed root does not keep the runtime alive;
- two `runMain` calls do not create competing roots.

## Phase 5: pure `blitz:path`

Implement the pure module independently of the asynchronous bridge.

### Placement

The public implementation should be JavaScript/TypeScript unless a measured
reason requires Rust. If Rust helpers are used, they remain pure host
primitives and must not perform filesystem access.

Use the process platform only for the common namespace. Explicit POSIX and
Windows variants must produce deterministic results on every host.

### URL behavior

`fromFileUrl` and `toFileUrl` require a correct runtime `URL` implementation.
If that prerequisite is not ready, ship the string path operations first and
withhold these two declarations rather than accepting loosely parsed URLs.

### Tests

Use table-driven cases for roots, separators, dot segments, trailing
separators, extensions, empty strings, drive letters, UNC paths, and non-ASCII
segments. Compare compatibility behavior against the documented contract, not
host `std::path` accidents.

### Phase 5 acceptance

The module imports in raw runtime programs, has no host I/O or Effect
requirement, and produces identical explicit POSIX/Windows results on all CI
platforms.

## Phase 6: `blitz:fs` read vertical slice

### Host implementation

Add a filesystem capability adapter using Tokio filesystem operations. Validate
and normalize public inputs on the VM thread, then move only owned paths and
limits into the task.

For `readFile`:

- enforce `maxBytes` before unbounded allocation;
- reject directories with `IsDirectory`;
- map stable I/O reasons;
- return owned bytes;
- convert to `Uint8Array` on the VM thread;
- support cancellation without invoking a stale callback.

For `readTextFile`:

- use `readFile` composition in JavaScript;
- decode UTF-8 strictly by default;
- return `InvalidEncoding` in the expected error channel;
- do not add a second Rust read path.

Do not block the VM thread with `std::fs::read` hidden inside `Effect.sync`.

### Public service

Implement:

- `FileSystem` Context tag;
- service accessor functions;
- immutable `layerLive`;
- deterministic `layerMemory`;
- tagged error constructors internal to the module unless public creation is
  needed.

The live Layer adapts the private host bridge. The memory Layer uses immutable
snapshots and real Effects but performs no host calls. Paths in the memory
Layer use one documented canonical syntax.

### Permissions seam

Even if full CLI permissions are not implemented, route live reads through one
policy method before starting Tokio work. The initial policy may allow all
reads, but it must be injectable/configurable from `RuntimeOptions` so later
permissions do not require rewriting the bridge.

### Tests

- successful binary and UTF-8 reads;
- missing, denied, directory, oversized, and invalid UTF-8 errors;
- URL path where URL support exists;
- interruption and completion race;
- memory Layer replacement proving no host access;
- defensive copies from `layerMemory` and returned byte arrays;
- no read begins before the Effect runs;
- no file operation is installed in the raw engine.

### Phase 6 acceptance

This application typechecks and executes using the real Effect runtime:

```ts
import * as Effect from "effect/Effect"
import * as FileSystem from "blitz:fs"
import * as Runtime from "blitz:runtime"

const program = FileSystem.readTextFile("./package.json").pipe(
  Effect.map((text) => console.log(text.length)),
)

Runtime.runMain(Runtime.provideLive(program))
```

An equivalent test using `FileSystem.layerMemory` performs no OS read. Both
programs preserve the `FileSystemError` type and leave no live operation or
root resource after completion.

## Phase 7: complete foundational services

Implement in this order, using the same operation registry:

1. remaining filesystem mutations and scoped file handles;
2. directory and watcher Streams;
3. process metadata/environment service;
4. signals as scoped Streams;
5. terminal output Effects and input Streams;
6. subprocess resources;
7. OS metadata and temporary resources;
8. cryptographic random bytes and digest operations.

Before terminal, watchers, or signals, expand upstream Stream operations needed
for async/scoped construction and cancellation. Before shared fan-out, add real
upstream Queue/PubSub rather than custom equivalents.

Each capability gets a live Layer, at least one deterministic test Layer where
useful, focused tagged errors, permission-policy hooks, and interruption tests.

## Phase 8: Effect-native networking

Build `blitz:http` and `blitz:websocket` over the existing transports and the
common operation/resource bridge.

### HTTP

- standard `Request`/`Response` interchange values;
- Effect-based client requests;
- scoped server acquisition;
- one child Scope per request;
- disconnect/timeout interruption;
- bounded streaming bodies;
- typed listen/client errors;
- explicit graceful shutdown.

### WebSocket

- scoped connect and accepted connections;
- messages as Stream;
- Effectful backpressured sends;
- idempotent close;
- bounded aggregate messages;
- no callback properties in the primary API.

Keep `Blitz.serve`, `Blitz.upgradeWebSocket`, `fetch`, and global `WebSocket` as
compatibility adapters over the same transport resources.

### Phase 8 acceptance

The old and new APIs can use the transport in separate tests without changing
engine semantics. The Effect API has complete interruption/finalization tests
and passes existing networking regression gates.

## Phase 9: rebuild app and RPC on stdlib services

Only after HTTP/WebSocket lifecycle is stable:

- make `blitz:app` a pure immutable description plus Effect composition;
- make `App.serve` delegate to `blitz:http`;
- implement RPC handlers as Effects and subscriptions as Streams;
- execute middleware in request child Scopes;
- provide application Layers once in the root Scope;
- preserve domain failures and redact defects at transport boundaries;
- migrate the versioned v1 host bridge to the generic cancellable operation ABI
  and ensure the removed `globalThis.__blitz_app_transport` name never returns;
- stop presenting the Rust `App`, `Layer`, and RPC builder model as a second
  public application API.

The Rust side retains validated wire envelopes, transport policy, and resource
ownership only where those responsibilities belong at the host boundary.

## Phase 10: compatibility adapters and deprecation

After the Effect-native API is stable:

- implement Web APIs over shared host resources without changing standards
  semantics;
- implement `node:*` modules and Node `process` behavior separately;
- document migration from `Blitz.*` callbacks;
- deprecate legacy globals in diagnostics and docs;
- remove them only in a versioned breaking release.

Do not make compatibility modules Context services or change Promise APIs to
Effects. Conversion happens at an explicit adapter boundary.

## Cross-cutting validation

Run the smallest relevant commands after each phase and the complete gate
before merging a vertical slice:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets
cargo test -p blitz-engine -p blitz-runtime -p blitz-cli
pnpm --dir tools/effect-assets typecheck
pnpm --dir tools/effect-assets test
pnpm --dir tools/effect-assets verify
cargo run -p effect-bytecode -- generate --check
```

Do not run a development server or a release build as a routine check.

Add focused benchmarks only after semantics are stable:

- operation registration/completion;
- small and bounded-large file reads;
- event queue throughput;
- cancellation churn and stale completion handling;
- Effect facade startup and module instantiation.

Performance work must not bypass the event queue, Scope, structured errors, or
permission checks.

## Review and commit boundaries

Keep changes reviewable in this order:

1. stdlib manifest, visibility, host ABI, generator, and atomic registration
   tests as part of the full-prebundling cutover;
2. declaration fixtures and minimal Effect export expansion;
3. regenerated deterministic source inputs, complete runtime graph bundle, and
   compatibility/no-parser tests;
4. scheduler/event-loop integration;
5. generic operation registry with synthetic tests;
6. `blitz:runtime` root lifecycle;
7. pure `blitz:path`;
8. filesystem private bridge;
9. `blitz:fs` live and memory Layers;
10. later capability modules one at a time.

Never combine generated Effect expansion, generic event-loop changes, and the
first real filesystem API in one patch. Separating them makes failures
attributable and rollback safe.

## First milestone definition of done

The first milestone is complete when all of the following are true:

- `Runtime.runMain` supervises one real Effect root and finalizes it cleanly.
- `blitz:path` provides the agreed pure string-path surface.
- `blitz:fs` exposes lazy, cancellable `readFile` and `readTextFile` Effects.
- FileSystem is a real Context service with live and memory Layers.
- Expected filesystem failures are typed tagged values.
- Interruption and completion races leak no callback roots or resources.
- Runtime event queues remain bounded.
- Raw-engine environments expose none of these modules or host operations.
- Public declarations contain no `any` and pass both strict checkers.
- Generated source assets and the complete prelinked graph bundle reproduce
  with no diff.
- Bundled Effect and Blitz stdlib startup performs zero source parses,
  import-extraction parses, typechecks, and compiler invocations.
- Duplicate specifiers, missing host primitives, invalid visibility, stale
  hashes, and Effect/runtime/host ABI mismatches fail before records become
  visible or initializers execute.
- Existing `Blitz.serve`, WebSocket, terminal, RPC, and Test262-focused tests
  continue to pass.
