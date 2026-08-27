# Effect-native runtime API expansion plan

> Historical expansion plan. The service ownership and migration sequence are
> superseded by
> [`FIRST_PARTY_EFFECT_API_MIGRATION.md`](FIRST_PARTY_EFFECT_API_MIGRATION.md).
> Keep implemented `blitz:*` APIs stable until their atomic migrations ship,
> but do not use this plan to add new copies of first-party Effect services.

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
`RuntimePrecompiledBundle`. Remove manifest-only `RuntimeGraphInstallPlan::prepare`
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

```ts
export interface ParsedPath {
  readonly root: string
  readonly directory: string
  readonly base: string
  readonly name: string
  readonly extension: string
}

export interface FormatPath {
  readonly root?: string
  readonly directory?: string
  readonly base?: string
  readonly name?: string
  readonly extension?: string
}

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

export interface PathApi {
  readonly separator: "/" | "\\"
  readonly delimiter: ":" | ";"
  readonly join: (...segments: ReadonlyArray<string>) => string
  readonly resolve: (
    base: string,
    ...segments: ReadonlyArray<string>
  ) => string
  readonly normalize: (path: string) => string
  readonly relative: (from: string, to: string) => string
  readonly dirname: (path: string) => string
  readonly basename: (path: string, suffix?: string) => string
  readonly extname: (path: string) => string
  readonly isAbsolute: (path: string) => boolean
  readonly parse: (path: string) => ParsedPath
  readonly format: (path: FormatPath) => string
  readonly fromFileUrl: (
    url: URL,
  ) => Either.Either<string, PathError>
  readonly toFileUrl: (
    path: string,
  ) => Either.Either<URL, PathError>
}

export declare const separator: "/" | "\\"
export declare const delimiter: ":" | ";"
export declare const join: PathApi["join"]
export declare const resolve: PathApi["resolve"]
export declare const normalize: PathApi["normalize"]
export declare const relative: PathApi["relative"]
export declare const dirname: PathApi["dirname"]
export declare const basename: PathApi["basename"]
export declare const extname: PathApi["extname"]
export declare const isAbsolute: PathApi["isAbsolute"]
export declare const parse: PathApi["parse"]
export declare const format: PathApi["format"]
export declare const fromFileUrl: PathApi["fromFileUrl"]
export declare const toFileUrl: PathApi["toFileUrl"]

export declare const posix: PathApi
export declare const windows: PathApi
```

The common exports select the VM target platform once when the module is
installed. They never consult ambient cwd or host filesystem state.
`resolve(base, ...segments)` always takes an explicit base. The `posix` and
`windows` namespaces behave identically on every host.

Specify POSIX authorities, Windows drive letters, UNC paths, separators,
trailing separators, empty inputs, dot segments, extensions, and non-ASCII
round trips. Withhold URL functions until runtime URL conformance passes.

Implement in bundled JavaScript unless measurement proves a Rust helper is
needed. Test table-driven behavior against the documented contract, not host
`std::path` accidents.

---

## 7. Implement full `blitz:fs`

### Public model

```ts
export type Path = string | URL
export type Durability = "none" | "data" | "full"

export interface ReadOptions {
  readonly maxBytes?: number
}

export interface WriteOptions {
  readonly create?: "openExisting" | "create" | "createNew"
  readonly content?: "replace" | "append"
  readonly durability?: Durability
}

export interface AtomicWriteOptions {
  readonly replace?: "allow" | "forbid"
  readonly durability?: Durability
}

export interface OpenOptions {
  readonly access: "read" | "write" | "readWrite"
  readonly create?: "openExisting" | "create" | "createNew"
  readonly content?: "preserve" | "truncate" | "append"
}

export interface FileInfo {
  readonly type:
    | "file"
    | "directory"
    | "symbolicLink"
    | "other"
  readonly size: bigint
  readonly modifiedAtMillis: Option.Option<number>
  readonly accessedAtMillis: Option.Option<number>
  readonly createdAtMillis: Option.Option<number>
}

export interface FileReadOptions {
  readonly offset?: bigint
  readonly maxBytes: number
}

export interface FileWriteOptions {
  readonly offset?: bigint
}

export interface FileHandle {
  readonly read: (
    options: FileReadOptions,
  ) => Effect.Effect<Uint8Array, FileSystemError>
  readonly write: (
    bytes: Uint8Array,
    options?: FileWriteOptions,
  ) => Effect.Effect<number, FileSystemError>
  readonly stat: Effect.Effect<FileInfo, FileSystemError>
  readonly sync: (
    durability?: Exclude<Durability, "none">,
  ) => Effect.Effect<void, FileSystemError>
  readonly truncate: (
    size: bigint,
  ) => Effect.Effect<void, FileSystemError>
  readonly close: Effect.Effect<void, FileSystemError>
}

export interface DirectoryEntry {
  readonly name: string
  readonly path: string
  readonly type:
    | "file"
    | "directory"
    | "symbolicLink"
    | "other"
}

export interface ReadDirectoryOptions {
  readonly depth?: "one" | "recursive"
}

export interface WatchOptions {
  readonly depth?: "one" | "recursive"
  readonly queueCapacity?: number
}

export interface FileCreated {
  readonly _tag: "Created"
  readonly path: string
}

export interface FileModified {
  readonly _tag: "Modified"
  readonly path: string
}

export interface FileRemoved {
  readonly _tag: "Removed"
  readonly path: string
}

export interface FileRenamed {
  readonly _tag: "Renamed"
  readonly from: string
  readonly to: string
}

export type FileEvent =
  | FileCreated
  | FileModified
  | FileRemoved
  | FileRenamed

export interface RemoveOptions {
  readonly depth?: "one" | "recursive"
  readonly missing?: "fail" | "ignore"
}

export interface MakeDirectoryOptions {
  readonly parents?: "create" | "require"
  readonly existing?: "fail" | "succeed"
}

export interface FileSystemError {
  readonly _tag: "FileSystemError"
  readonly operation:
    | "read"
    | "write"
    | "open"
    | "stat"
    | "realPath"
    | "copy"
    | "rename"
    | "remove"
    | "makeDirectory"
    | "readDirectory"
    | "watch"
    | "sync"
    | "truncate"
    | "close"
  readonly reason:
    | "NotFound"
    | "PermissionDenied"
    | "AlreadyExists"
    | "InvalidPath"
    | "NotDirectory"
    | "IsDirectory"
    | "TooManyOpenFiles"
    | "FileTooLarge"
    | "InvalidEncoding"
    | "Closed"
    | "Unsupported"
    | "WatchOverflow"
    | "Io"
  readonly path?: string
  readonly destination?: string
  readonly message: string
}

export interface FileSystem {
  readonly readFile: (
    path: Path,
    options?: ReadOptions,
  ) => Effect.Effect<Uint8Array, FileSystemError>
  readonly readTextFile: (
    path: Path,
    options?: ReadOptions,
  ) => Effect.Effect<string, FileSystemError>
  readonly writeFile: (
    path: Path,
    bytes: Uint8Array,
    options?: WriteOptions,
  ) => Effect.Effect<void, FileSystemError>
  readonly writeTextFile: (
    path: Path,
    text: string,
    options?: WriteOptions,
  ) => Effect.Effect<void, FileSystemError>
  readonly writeFileAtomic: (
    path: Path,
    bytes: Uint8Array,
    options?: AtomicWriteOptions,
  ) => Effect.Effect<void, FileSystemError>
  readonly writeTextFileAtomic: (
    path: Path,
    text: string,
    options?: AtomicWriteOptions,
  ) => Effect.Effect<void, FileSystemError>
  readonly open: (
    path: Path,
    options: OpenOptions,
  ) => Effect.Effect<
    FileHandle,
    FileSystemError,
    Scope.Scope
  >
  readonly stat: (
    path: Path,
  ) => Effect.Effect<FileInfo, FileSystemError>
  readonly lstat: (
    path: Path,
  ) => Effect.Effect<FileInfo, FileSystemError>
  readonly exists: (
    path: Path,
  ) => Effect.Effect<boolean, FileSystemError>
  readonly realPath: (
    path: Path,
  ) => Effect.Effect<string, FileSystemError>
  readonly copy: (
    source: Path,
    destination: Path,
  ) => Effect.Effect<void, FileSystemError>
  readonly rename: (
    source: Path,
    destination: Path,
  ) => Effect.Effect<void, FileSystemError>
  readonly remove: (
    path: Path,
    options?: RemoveOptions,
  ) => Effect.Effect<void, FileSystemError>
  readonly makeDirectory: (
    path: Path,
    options?: MakeDirectoryOptions,
  ) => Effect.Effect<void, FileSystemError>
  readonly readDirectory: (
    path: Path,
    options?: ReadDirectoryOptions,
  ) => Stream.Stream<DirectoryEntry, FileSystemError>
  readonly watch: (
    path: Path,
    options?: WatchOptions,
  ) => Stream.Stream<FileEvent, FileSystemError>
}

export declare const FileSystem:
  Context.Tag<FileSystem, FileSystem>

export declare const layerLive: Layer.Layer<FileSystem>

export interface MemoryFileSystemOptions {
  readonly files?: Readonly<Record<string, Uint8Array | string>>
  readonly directories?: ReadonlyArray<string>
  readonly currentDirectory?: string
}

export declare const layerMemory: (
  options?: MemoryFileSystemOptions,
) => Layer.Layer<FileSystem>
```

The module exports a pipe-friendly accessor for every `FileSystem` method. Each
accessor has the same success and error channels as the service method and adds
`FileSystem` to its requirements; `open` additionally retains `Scope.Scope`.
Keeping this mechanical rule avoids maintaining a second semantic surface in
the design document.

Binary and text operations are distinct. Text is strict UTF-8. Whole-file reads
default to a 64 MiB maximum; callers must opt into a larger finite limit.
`writeFileAtomic` and `writeTextFileAtomic` write a sibling temporary file,
apply the requested durability, atomically replace the destination according
to policy, and clean up the temporary file on every Exit.

`exists` returns `false` only for `NotFound`; permission and other failures stay
in the error channel.

All host calls pass a permission check when the Effect runs. `readTextFile`
composes `readFile` and strict decoding in JavaScript. Bounded allocation is
enforced before and during reads. Returned and input bytes are defensively
copied at trust boundaries.

`readDirectory` and `watch` own acquisition and release inside Stream execution;
neither exposes a separate `Scope` requirement. Watch overflow fails the Stream
with `WatchOverflow`; no event is silently dropped.

`layerMemory` snapshots inputs, uses Effect-owned state rather than a module
global, copies every byte array, uses POSIX paths on every host, and performs no
host calls. Each factory call creates a distinct Layer identity and therefore a
distinct filesystem; normal Layer memoization shares that instance within one
application graph.

Implement in slices: reads; handles; metadata; mutations/atomic writes;
directory Streams; watchers. Each slice gets cancellation, finalization,
permissions, race, queue, temporary-directory, and memory-Layer tests.

---

## 8. Implement `blitz:process` and separate `blitz:subprocess`

### Current process

`Process` describes the current Blitz process only. Child creation belongs to
`blitz:subprocess`; neither module exposes the Node-compatible `process` object.

```ts
export interface ProcessSnapshot {
  readonly processId: number
  readonly executable: string
  readonly arguments: ReadonlyArray<string>
  readonly initialDirectory: string
}

export type Signal =
  | "SIGINT"
  | "SIGKILL"
  | "SIGTERM"
  | "SIGHUP"
  | "SIGQUIT"
  | "SIGUSR1"
  | "SIGUSR2"

export interface SignalEvent {
  readonly signal: Signal
}

export interface ProcessError {
  readonly _tag: "ProcessError"
  readonly operation:
    | "currentDirectory"
    | "changeDirectory"
    | "environment"
    | "setExitCode"
    | "exit"
    | "signals"
  readonly reason:
    | "InvalidArgument"
    | "PermissionDenied"
    | "NotFound"
    | "NotDirectory"
    | "Unsupported"
    | "Io"
  readonly name?: string
  readonly message: string
}

export interface Process {
  readonly snapshot: ProcessSnapshot
  readonly currentDirectory: Effect.Effect<string, ProcessError>
  readonly changeDirectory: (
    path: string,
  ) => Effect.Effect<void, ProcessError>
  readonly environment: (
    name: string,
  ) => Effect.Effect<Option.Option<string>, ProcessError>
  readonly environmentAll: Effect.Effect<
    Readonly<Record<string, string>>,
    ProcessError
  >
  readonly setExitCode: (
    code: number,
  ) => Effect.Effect<void, ProcessError>
  readonly exit: (
    code?: number,
  ) => Effect.Effect<never, ProcessError>
  readonly signals: (
    signals?: ReadonlyArray<Signal>,
  ) => Stream.Stream<SignalEvent, ProcessError>
}

export declare const Process: Context.Tag<Process, Process>

export declare const snapshot:
  Effect.Effect<ProcessSnapshot, never, Process>

export declare const currentDirectory:
  Effect.Effect<string, ProcessError, Process>

export declare const changeDirectory: (
  path: string,
) => Effect.Effect<void, ProcessError, Process>

export declare const environment: (
  name: string,
) => Effect.Effect<Option.Option<string>, ProcessError, Process>

export declare const environmentAll:
  Effect.Effect<
    Readonly<Record<string, string>>,
    ProcessError,
    Process
  >

export declare const setExitCode: (
  code: number,
) => Effect.Effect<void, ProcessError, Process>

export declare const exit: (
  code?: number,
) => Effect.Effect<never, ProcessError, Process>

export declare const signals: (
  signals?: ReadonlyArray<Signal>,
) => Stream.Stream<SignalEvent, ProcessError, Process>

export declare const layerLive: Layer.Layer<Process>

export declare const layerTest: (
  options: {
    readonly snapshot: ProcessSnapshot
    readonly currentDirectory?: string
    readonly environment?: Readonly<Record<string, string>>
  },
) => Layer.Layer<Process>
```

The live Layer copies startup metadata and environment into VM-owned state.
Current directory and requested exit status also live in the VM; changing them
never mutates an ambient OS process global. Host operations receive the logical
working directory explicitly. Access remains Effectful because it is mutable,
permission-gated, or resource-owning runtime state. Environment absence is
`Option.none`; permission or I/O failures remain typed errors. Name and
exit-code validation occurs when the Effect runs.

Typed environment decoding composes with the pinned Effect `Config` and
`Schema` modules instead of introducing a Blitz-specific decoder protocol. The
live Config provider reads the same permission-filtered VM-owned environment
snapshot. `exit` requests root interruption and permits finalizers; it never
performs immediate host termination. A signal Stream acquires and releases its
host subscription as part of Stream execution, so callers do not provide a
separate `Scope`.

The mutable Node-compatible global remains a separate compatibility adapter and
is not read at module initialization by Blitz libraries.

### Subprocess

Keep child creation in `blitz:subprocess`. Follow Effect's useful separation
between a pure immutable command description and an execution service, without
copying its broad `PlatformError` or unbounded collection helpers.

```ts
export interface Command {
  readonly _tag: "Command"
  readonly executable: string
  readonly arguments: ReadonlyArray<string>
  readonly workingDirectory: Option.Option<string>
  readonly environment: Readonly<Record<string, string>>
  readonly environmentMode: "extend" | "replace"
}

export type StdinMode = "inherit" | "null" | "pipe"
export type OutputMode = "inherit" | "null" | "pipe"

export interface SpawnOptions {
  readonly stdin: StdinMode
  readonly stdout: OutputMode
  readonly stderr: OutputMode
  readonly queueCapacity?: number
}

export type Signal =
  | "SIGINT"
  | "SIGKILL"
  | "SIGTERM"
  | "SIGHUP"
  | "SIGQUIT"
  | "SIGUSR1"
  | "SIGUSR2"

export interface Exited {
  readonly _tag: "Exited"
  readonly code: number
}

export interface Signaled {
  readonly _tag: "Signaled"
  readonly signal: Signal
}

export type ExitStatus = Exited | Signaled

export interface ChildInput {
  readonly write: (
    bytes: Uint8Array,
  ) => Effect.Effect<void, SubprocessError>
  readonly writeText: (
    text: string,
  ) => Effect.Effect<void, SubprocessError>
  readonly end: Effect.Effect<void, SubprocessError>
}

export interface ChildProcess {
  readonly processId: number
  readonly standardInput: Option.Option<ChildInput>
  readonly standardOutput:
    Option.Option<Stream.Stream<Uint8Array, SubprocessError>>
  readonly standardError:
    Option.Option<Stream.Stream<Uint8Array, SubprocessError>>
  readonly isRunning: Effect.Effect<boolean, SubprocessError>
  readonly wait: Effect.Effect<ExitStatus, SubprocessError>
  readonly kill: (
    signal?: Signal,
  ) => Effect.Effect<void, SubprocessError>
  readonly close: Effect.Effect<void, SubprocessError>
}

export interface OutputOptions {
  readonly input?: Uint8Array | string
  readonly maxStandardOutputBytes?: number
  readonly maxStandardErrorBytes?: number
}

export interface Output {
  readonly status: ExitStatus
  readonly standardOutput: Uint8Array
  readonly standardError: Uint8Array
}

export interface TextOutputOptions extends OutputOptions {
  readonly encoding?: "utf-8"
}

export interface TextOutput {
  readonly status: ExitStatus
  readonly standardOutput: string
  readonly standardError: string
}

export interface SubprocessError {
  readonly _tag: "SubprocessError"
  readonly operation:
    | "spawn"
    | "write"
    | "end"
    | "wait"
    | "kill"
    | "close"
    | "output"
  readonly reason:
    | "InvalidCommand"
    | "InvalidOptions"
    | "PermissionDenied"
    | "NotFound"
    | "NotExecutable"
    | "WorkingDirectoryNotFound"
    | "BrokenPipe"
    | "OutputLimitExceeded"
    | "Unsupported"
    | "Closed"
    | "Io"
  readonly executable?: string
  readonly stream?: "stdin" | "stdout" | "stderr"
  readonly message: string
}

export interface Subprocess {
  readonly spawn: (
    command: Command,
    options: SpawnOptions,
  ) => Effect.Effect<
    ChildProcess,
    SubprocessError,
    Scope.Scope
  >
  readonly output: (
    command: Command,
    options?: OutputOptions,
  ) => Effect.Effect<Output, SubprocessError>
  readonly outputText: (
    command: Command,
    options?: TextOutputOptions,
  ) => Effect.Effect<TextOutput, SubprocessError>
}

export declare const Subprocess:
  Context.Tag<Subprocess, Subprocess>

export declare const command: (
  executable: string,
  ...arguments_: ReadonlyArray<string>
) => Command

export declare const workingDirectory: {
  (path: string): (command: Command) => Command
  (command: Command, path: string): Command
}

export declare const environment: {
  (
    variables: Readonly<Record<string, string>>,
    options?: { readonly mode?: "extend" | "replace" },
  ): (command: Command) => Command
  (
    command: Command,
    variables: Readonly<Record<string, string>>,
    options?: { readonly mode?: "extend" | "replace" },
  ): Command
}

export declare const spawn: (
  command: Command,
  options: SpawnOptions,
) => Effect.Effect<
  ChildProcess,
  SubprocessError,
  Subprocess | Scope.Scope
>

export declare const output: (
  command: Command,
  options?: OutputOptions,
) => Effect.Effect<Output, SubprocessError, Subprocess>

export declare const outputText: (
  command: Command,
  options?: TextOutputOptions,
) => Effect.Effect<TextOutput, SubprocessError, Subprocess>

export declare const layerLive: Layer.Layer<Subprocess>

export interface ScriptedCommand {
  readonly command: Command
  readonly standardOutput?: ReadonlyArray<Uint8Array>
  readonly standardError?: ReadonlyArray<Uint8Array>
  readonly status?: ExitStatus
  readonly failure?: SubprocessError
}

export interface SubprocessInvocation {
  readonly command: Command
  readonly standardInput: Uint8Array
}

export interface TestSubprocessError {
  readonly _tag: "TestSubprocessError"
  readonly reason: "UnconsumedCommands"
  readonly remaining: number
  readonly message: string
}

export interface TestSubprocess {
  readonly layer: Layer.Layer<Subprocess>
  readonly invocations:
    Effect.Effect<ReadonlyArray<SubprocessInvocation>>
  readonly assertConsumed:
    Effect.Effect<void, TestSubprocessError>
}

export declare const makeTest: (
  script: ReadonlyArray<ScriptedCommand>,
) => Effect.Effect<TestSubprocess>
```

`command`, `workingDirectory`, and `environment` are pure. They create frozen
values, perform no service lookup, and defer validation and permission checks
until execution. There is no shell mode in the initial API. If a later
permission-gated shell API is justified, it uses a visibly separate
constructor rather than a boolean option.

`output` and `outputText` are the safe convenience boundary. They internally
scope the child, concurrently drain stdout and stderr while waiting, enforce
independent byte limits before and during allocation, and close stdin after
the optional input is written. Each output limit defaults to 16 MiB. An output
limit failure terminates and waits for the child before returning
`OutputLimitExceeded`. `outputText` performs strict UTF-8 decoding after byte
limits have been enforced.

`spawn` is the advanced boundary. It requires all three stdio modes explicitly
and returns a scoped resource. Piped handles are present only for the
corresponding `"pipe"` mode. Each output Stream has one owner, is backed by a
bounded queue, copies byte chunks at the host boundary, and unregisters its
waiter when interrupted. Callers must consume both piped output Streams
concurrently before awaiting a potentially pipe-blocked child.

Do not add `effect/Sink` to the curated graph solely for subprocess stdin.
`ChildInput` represents domain operations rather than cloning a concurrency
primitive. Reconsider `Sink` once both subprocess and filesystem streaming
writes can share it.

- `spawn` requires `Scope`;
- nonzero child exit is a successful `ExitStatus`;
- `wait` interruption unregisters only that waiter;
- scope exit kills and waits by default;
- `kill`, stdin end, and close are idempotent;
- output Streams are single-owner and bounded;
- unsupported signals/platform behavior are typed;
- command, cwd, environment, and inherited-resource permissions are checked at
  execution.

`makeTest` creates a deterministic scripted Layer and an observation handle. A
script matches the complete immutable `Command`, supplies chunked stdout/stderr
and an `ExitStatus`, records stdin, and can inject failures at each operation.
It uses Effect-owned state and never invokes the host. Unexpected commands fail
the attempted subprocess operation as `InvalidCommand`; tests use
`assertConsumed` to detect unused script entries. Script byte arrays are copied
when the test service is constructed and again when emitted.

Land process snapshot/environment first, then signals, then subprocess spawn and
wait, pipes and bounded `output`, and cleanup. Curate `effect/Sink` only in a
later independently tested slice if shared streaming-write APIs justify it.

---

## 9. Implement `blitz:terminal` and logging integration

```ts
export interface TerminalCapabilities {
  readonly interactive: boolean
  readonly colors: "none" | "ansi16" | "ansi256" | "trueColor"
  readonly unicode: boolean
}

export interface TerminalDimensions {
  readonly columns: number
  readonly rows: number
}

export interface KeyEvent {
  readonly key: string
  readonly text: Option.Option<string>
  readonly control: boolean
  readonly alt: boolean
  readonly shift: boolean
  readonly repeat: boolean
}

export interface PromptOptions {
  readonly message: string
  readonly input?: "visible" | "hidden"
  readonly defaultValue?: string
}

export interface TerminalError {
  readonly _tag: "TerminalError"
  readonly operation:
    | "capabilities"
    | "dimensions"
    | "write"
    | "flush"
    | "input"
    | "rawMode"
    | "alternateScreen"
    | "prompt"
  readonly reason:
    | "InvalidInput"
    | "NotInteractive"
    | "InputBusy"
    | "PromptBusy"
    | "Unsupported"
    | "Closed"
    | "Overflow"
    | "Host"
  readonly message: string
}

export interface Terminal {
  readonly capabilities:
    Effect.Effect<TerminalCapabilities, TerminalError>
  readonly dimensions:
    Effect.Effect<
      Option.Option<TerminalDimensions>,
      TerminalError
    >
  readonly write: (
    text: string,
  ) => Effect.Effect<void, TerminalError>
  readonly writeLine: (
    text?: string,
  ) => Effect.Effect<void, TerminalError>
  readonly flush: Effect.Effect<void, TerminalError>
  readonly bytes:
    Stream.Stream<Uint8Array, TerminalError>
  readonly text:
    Stream.Stream<string, TerminalError>
  readonly keys:
    Stream.Stream<KeyEvent, TerminalError>
  readonly lines:
    Stream.Stream<string, TerminalError>
  readonly rawMode:
    Effect.Effect<void, TerminalError, Scope.Scope>
  readonly alternateScreen:
    Effect.Effect<void, TerminalError, Scope.Scope>
  readonly prompt: (
    options: PromptOptions,
  ) => Effect.Effect<string, TerminalError>
}

export declare const Terminal: Context.Tag<Terminal, Terminal>

export declare const capabilities:
  Effect.Effect<TerminalCapabilities, TerminalError, Terminal>

export declare const dimensions:
  Effect.Effect<
    Option.Option<TerminalDimensions>,
    TerminalError,
    Terminal
  >

export declare const write: (
  text: string,
) => Effect.Effect<void, TerminalError, Terminal>

export declare const writeLine: (
  text?: string,
) => Effect.Effect<void, TerminalError, Terminal>

export declare const flush:
  Effect.Effect<void, TerminalError, Terminal>

export declare const bytes:
  Stream.Stream<Uint8Array, TerminalError, Terminal>

export declare const text:
  Stream.Stream<string, TerminalError, Terminal>

export declare const keys:
  Stream.Stream<KeyEvent, TerminalError, Terminal>

export declare const lines:
  Stream.Stream<string, TerminalError, Terminal>

export declare const rawMode:
  Effect.Effect<
    void,
    TerminalError,
    Terminal | Scope.Scope
  >

export declare const alternateScreen:
  Effect.Effect<
    void,
    TerminalError,
    Terminal | Scope.Scope
  >

export declare const prompt: (
  options: PromptOptions,
) => Effect.Effect<string, TerminalError, Terminal>

export declare const layerLive: Layer.Layer<Terminal>

export interface TestTerminal {
  readonly layer: Layer.Layer<Terminal>
  readonly output: Effect.Effect<ReadonlyArray<string>>
}

export declare const makeTest: (
  input?: ReadonlyArray<string | KeyEvent | Uint8Array>,
) => Effect.Effect<TestTerminal>
```

Output uses a bounded writer queue and acknowledges acceptance/flush according
to documented policy. `write` completes when the queue accepts the complete
string; `flush` completes only after all earlier output reaches the host. The
queue suspends producers rather than dropping output.

Input is pull-based or bounded; it never silently drops. Only one of `bytes`,
`text`, `keys`, `lines`, or `prompt` may own terminal input at a time.
Competing consumers fail with `InputBusy` or `PromptBusy`. Shared fan-out is an
application concern implemented with upstream `PubSub`, not an implicit
Terminal behavior. Each input Stream acquires and releases ownership inside
Stream execution.

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

`makeTest` creates a fresh Layer with scripted input and Effect-owned captured
output. Exhausted scripted input ends Streams normally and makes prompts fail
with `Closed`; the test implementation never touches a host terminal.

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

export interface HttpListenError {
  readonly _tag: "HttpListenError"
  readonly operation: "listen" | "shutdown"
  readonly reason:
    | "InvalidOptions"
    | "PermissionDenied"
    | "AddressInUse"
    | "AddressUnavailable"
    | "Unsupported"
    | "Io"
  readonly address?: string
  readonly message: string
}

export interface HttpClientError {
  readonly _tag: "HttpClientError"
  readonly operation:
    | "request"
    | "sendBody"
    | "receiveHeaders"
    | "receiveBody"
  readonly reason:
    | "InvalidRequest"
    | "PermissionDenied"
    | "Dns"
    | "Connect"
    | "Tls"
    | "Timeout"
    | "Limit"
    | "Disconnect"
    | "Protocol"
    | "Io"
  readonly url?: string
  readonly message: string
}

export interface HttpBodyError {
  readonly _tag: "HttpBodyError"
  readonly operation: "read" | "write" | "decode"
  readonly reason:
    | "Limit"
    | "Disconnect"
    | "InvalidEncoding"
    | "Closed"
    | "Transport"
  readonly message: string
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

export interface HttpServer {
  readonly serve: <E, R, MapperR = never>(
    options: ServeOptions<E, R, MapperR>,
  ) => Effect.Effect<never, HttpListenError, R | MapperR>
  readonly listen: <E, R, MapperR = never>(
    options: ServeOptions<E, R, MapperR>,
  ) => Effect.Effect<
    HttpServerResource,
    HttpListenError,
    R | MapperR | Scope.Scope
  >
}

export interface HttpRequestOptions<BodyE = never, BodyR = never> {
  readonly init?: Omit<RequestInit, "body">
  readonly body?: Stream.Stream<Uint8Array, BodyE, BodyR>
  readonly timeout?: Duration.DurationInput
  readonly maxResponseBodyBytes?: number
}

export interface HttpClient {
  readonly request: <BodyE = never, BodyR = never>(
    input: RequestInfo | URL,
    options?: HttpRequestOptions<BodyE, BodyR>,
  ) => Effect.Effect<
    Response,
    HttpClientError | BodyE,
    BodyR | Scope.Scope
  >
}

export declare const HttpServer:
  Context.Tag<HttpServer, HttpServer>

export declare const HttpClient:
  Context.Tag<HttpClient, HttpClient>

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

export declare const request: <
  BodyE = never,
  BodyR = never,
>(
  input: RequestInfo | URL,
  options?: HttpRequestOptions<BodyE, BodyR>,
) => Effect.Effect<
  Response,
  HttpClientError | BodyE,
  HttpClient | BodyR | Scope.Scope
>

export declare const withResponse: <
  A,
  E,
  R,
  BodyE = never,
  BodyR = never,
>(
  input: RequestInfo | URL,
  use: (response: Response) => Effect.Effect<A, E, R>,
  options?: HttpRequestOptions<BodyE, BodyR>,
) => Effect.Effect<
  A,
  HttpClientError | BodyE | E,
  HttpClient | BodyR | R
>

export declare const responseBody: (
  response: Response,
) => Stream.Stream<Uint8Array, HttpBodyError>

export declare const readBody: (
  response: Response,
  options?: { readonly maxBytes?: number },
) => Effect.Effect<Uint8Array, HttpBodyError>

export declare const readBodyText: (
  response: Response,
  options?: { readonly maxBytes?: number },
) => Effect.Effect<string, HttpBodyError>

export declare const layerServerLive: Layer.Layer<HttpServer>
export declare const layerClientLive: Layer.Layer<HttpClient>
```

Expected errors use the mapper or a documented redacted 500 policy. Defects are
logged and redacted. Disconnect, timeout, and shutdown interrupt the request
Scope and do not become ordinary handler failures.

Provide both:

- `serve`, a never-ending internally scoped server Effect;
- `listen`, a scoped server resource for advanced composition.

Each request owns a child Scope. Request and response bodies use bounded
Streams/adapters. Client `request` requires `HttpClient | Scope.Scope` while a
streaming body is live; `withResponse` is the safe convenience that scopes and
closes it. Request body Stream failures remain in the request error channel and
are not collapsed into `HttpClientError`.

`responseBody` is the Effect-native view of the standard response body and has
one owner. `readBody` and `readBodyText` default to 16 MiB limits; text decoding
is strict UTF-8. The standard `Response` remains usable at compatibility
boundaries without changing its Web semantics.

### WebSocket contracts

```ts
export interface TextMessage {
  readonly _tag: "Text"
  readonly data: string
}

export interface BinaryMessage {
  readonly _tag: "Binary"
  readonly data: Uint8Array
}

export type Message = TextMessage | BinaryMessage

export interface CloseOptions {
  readonly code?: number
  readonly reason?: string
}

export interface CloseEvent {
  readonly code: number
  readonly reason: string
  readonly remote: boolean
}

export interface WebSocketError {
  readonly _tag: "WebSocketError"
  readonly operation:
    | "connect"
    | "upgrade"
    | "send"
    | "messages"
    | "close"
  readonly reason:
    | "InvalidOptions"
    | "PermissionDenied"
    | "Dns"
    | "Connect"
    | "UpgradeRejected"
    | "Protocol"
    | "MessageTooLarge"
    | "Overflow"
    | "ConsumerBusy"
    | "Closed"
    | "Io"
  readonly url?: string
  readonly message: string
}

export interface Connection {
  readonly protocol: Option.Option<string>
  readonly messages:
    Stream.Stream<Message, WebSocketError>
  readonly send: (
    message: Message,
  ) => Effect.Effect<void, WebSocketError>
  readonly closed:
    Effect.Effect<CloseEvent, WebSocketError>
  readonly close: (
    options?: CloseOptions,
  ) => Effect.Effect<void, WebSocketError>
}

export interface ConnectOptions {
  readonly url: string | URL
  readonly protocols?: ReadonlyArray<string>
  readonly queueCapacity?: number
  readonly maxMessageBytes?: number
}

export type ConnectionHandler<E = never, R = never> = (
  connection: Connection,
) => Effect.Effect<void, E, R>

export interface UpgradeOptions<
  E,
  R,
  MapperR = never,
> {
  readonly protocols?: ReadonlyArray<string>
  readonly queueCapacity?: number
  readonly maxMessageBytes?: number
  readonly handler: ConnectionHandler<E, R>
  readonly onHandlerError?: (
    error: E,
  ) => Effect.Effect<void, never, MapperR>
}

export interface WebSocketUpgrade {
  readonly _tag: "WebSocketUpgrade"
}

export interface WebSocketClient {
  readonly connect: (
    options: ConnectOptions,
  ) => Effect.Effect<
    Connection,
    WebSocketError,
    Scope.Scope
  >
}

export interface WebSocketServer {
  readonly upgrade: <E, R, MapperR = never>(
    request: Request,
    options: UpgradeOptions<E, R, MapperR>,
  ) => Effect.Effect<
    WebSocketUpgrade,
    WebSocketError,
    R | MapperR
  >
}

export declare const WebSocketClient:
  Context.Tag<WebSocketClient, WebSocketClient>

export declare const WebSocketServer:
  Context.Tag<WebSocketServer, WebSocketServer>

export declare const connect: (
  options: ConnectOptions,
) => Effect.Effect<
  Connection,
  WebSocketError,
  WebSocketClient | Scope.Scope
>

export declare const upgrade: <E, R, MapperR = never>(
  request: Request,
  options: UpgradeOptions<E, R, MapperR>,
) => Effect.Effect<
  WebSocketUpgrade,
  WebSocketError,
  WebSocketServer | R | MapperR
>

export declare const layerClientLive:
  Layer.Layer<WebSocketClient>

export declare const layerServerLive:
  Layer.Layer<WebSocketServer>
```

`WebSocketUpgrade` is an opaque decision consumed only by `blitz:http`; it is
not a partially initialized connection and exposes no manual `accept`. The
upgrade operation captures the connection handler and its provided Context.
After the HTTP transport commits the handshake, it runs the handler in the
request child Scope. Expected handler errors use `onHandlerError` or the
documented redacted close policy; defects are logged and close with code 1011.

Connections are scoped. The message Stream has one owner and a second consumer
fails with `ConsumerBusy`. Explicit broadcast uses upstream `PubSub`.

Inbound overflow follows a documented close/error policy; outbound send waits
for capacity instead of `try_send`. Copy binary payloads at trust boundaries.
Inbound overflow closes with code 1009 and fails the Stream with `Overflow`.
`send` completes only after the bounded outbound queue accepts the complete
message. `close` is idempotent and waits for the close frame to be accepted;
Scope finalization performs a bounded graceful close before aborting transport.
The standard global WebSocket remains event/callback based.

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

```ts
export type JsonValue =
  | null
  | boolean
  | number
  | string
  | ReadonlyArray<JsonValue>
  | { readonly [key: string]: JsonValue }

export interface Decoder<A, E = never, R = never> {
  readonly decode: (
    input: unknown,
  ) => Effect.Effect<A, E, R>
}

export interface Encoder<A, E = never, R = never> {
  readonly encode: (
    value: A,
  ) => Effect.Effect<JsonValue, E, R>
}

export interface ProcedureLike {
  readonly _tag: "Procedure"
  readonly kind: "query" | "mutation" | "subscription"
}

export declare const ProcedureTypeId: unique symbol

export interface Procedure<
  I,
  O,
  E,
  R,
  Kind extends ProcedureLike["kind"],
> extends ProcedureLike {
  readonly kind: Kind
  readonly [ProcedureTypeId]: {
    readonly _Input: (_: I) => void
    readonly _Output: () => O
    readonly _Error: () => E
    readonly _Requirements: (_: R) => void
  }
}

export type Query<I, O, E = never, R = never> =
  Procedure<I, O, E, R, "query">

export type Mutation<I, O, E = never, R = never> =
  Procedure<I, O, E, R, "mutation">

export type Subscription<I, O, E = never, R = never> =
  Procedure<I, O, E, R, "subscription">

export declare const query: <I, O, DecodeE, HandlerE, EncodeE, R>(
  input: Decoder<I, DecodeE, R>,
  handler: (input: I) => Effect.Effect<O, HandlerE, R>,
  output: Encoder<O, EncodeE, R>,
) => Query<I, O, DecodeE | HandlerE | EncodeE, R>

export declare const mutation: <I, O, DecodeE, HandlerE, EncodeE, R>(
  input: Decoder<I, DecodeE, R>,
  handler: (input: I) => Effect.Effect<O, HandlerE, R>,
  output: Encoder<O, EncodeE, R>,
) => Mutation<I, O, DecodeE | HandlerE | EncodeE, R>

export declare const subscription: <
  I,
  O,
  DecodeE,
  HandlerE,
  EncodeE,
  R,
>(
  input: Decoder<I, DecodeE, R>,
  handler: (input: I) => Stream.Stream<O, HandlerE, R>,
  output: Encoder<O, EncodeE, R>,
) => Subscription<I, O, DecodeE | HandlerE | EncodeE, R>

export interface Middleware<AddedE, AddedR> {
  readonly apply: <I, O, E, R>(
    handler: (
      input: I,
    ) => Effect.Effect<O, E, R>,
  ) => (
    input: I,
  ) => Effect.Effect<O, E | AddedE, R | AddedR>
}

export declare const withMiddleware: <
  AddedE,
  AddedR,
>(
  middleware: Middleware<AddedE, AddedR>,
) => <I, O, E, R, Kind extends ProcedureLike["kind"]>(
  procedure: Procedure<I, O, E, R, Kind>,
) => Procedure<I, O, E | AddedE, R | AddedR, Kind>

export type Routes = Readonly<Record<string, ProcedureLike>>

export interface Router<R extends Routes> {
  readonly _tag: "Router"
  readonly routes: R
}

export declare const router: <R extends Routes>(
  routes: R,
) => Router<R>

export type ProcedureInput<P> =
  P extends Procedure<infer I, unknown, unknown, unknown, ProcedureLike["kind"]>
    ? I
    : never

export type ProcedureOutput<P> =
  P extends Procedure<unknown, infer O, unknown, unknown, ProcedureLike["kind"]>
    ? O
    : never

export type ProcedureError<P> =
  P extends Procedure<unknown, unknown, infer E, unknown, ProcedureLike["kind"]>
    ? E
    : never

export type ProcedureRequirements<P> =
  P extends Procedure<unknown, unknown, unknown, infer R, ProcedureLike["kind"]>
    ? R
    : never

export type RouterRequirements<R extends Routes> = {
  readonly [K in keyof R]: ProcedureRequirements<R[K]>
}[keyof R]

export interface RpcClientError {
  readonly _tag: "RpcClientError"
  readonly reason:
    | "InvalidInput"
    | "Encode"
    | "Transport"
    | "Protocol"
    | "Timeout"
    | "Disconnect"
    | "Limit"
  readonly route?: string
  readonly message: string
}

export interface RpcClient {
  readonly unary: (
    route: string,
    input: JsonValue,
  ) => Effect.Effect<JsonValue, RpcClientError>
  readonly subscription: (
    route: string,
    input: JsonValue,
  ) => Stream.Stream<JsonValue, RpcClientError>
}

export type Caller<R extends Routes> = {
  readonly [K in keyof R]:
    R[K] extends Subscription<
      infer I,
      infer O,
      infer E,
      unknown
    >
      ? (input: I) => Stream.Stream<O, RpcClientError | E, RpcClient>
      : R[K] extends Procedure<
          infer I,
          infer O,
          infer E,
          unknown,
          "query" | "mutation"
        >
        ? (input: I) => Effect.Effect<O, RpcClientError | E, RpcClient>
        : never
}

export declare const caller: <R extends Routes>(
  router: Router<R>,
) => Caller<R>
```

Never collapse router requirements to `unknown`. Domain failures are readonly,
tagged, and JSON-safe. Defects are logged and redacted. Batches, messages, and
subscriptions are bounded. Disconnect interrupts the procedure child Scope.
The public procedure values are frozen descriptions. Implementations may hide
their handlers behind a private symbol; the structural fields above describe
their type contract, not permission for Rust to execute them.

Middleware for subscription handlers has the corresponding Stream-preserving
shape and accumulates the same error and requirement unions. It never converts
a Stream to an Effect or catches defects.

### Application model

```ts
export interface App<
  Rts extends Routes,
  Requirements,
  LayerError,
> {
  readonly _tag: "App"
  readonly [AppTypeId]: {
    readonly _Routes: () => Rts
    readonly _Requirements: (_: Requirements) => void
    readonly _LayerError: () => LayerError
  }
}

export declare const AppTypeId: unique symbol

export interface AppServeOptions {
  readonly hostname?: string
  readonly port?: number
  readonly rpcPath?: string
  readonly requestTimeout?: Duration.DurationInput
  readonly maxMessageBytes?: number
  readonly queueCapacity?: number
}

export interface AppServeError {
  readonly _tag: "AppServeError"
  readonly reason:
    | "InvalidOptions"
    | "Layer"
    | "Listen"
    | "Transport"
  readonly message: string
}

export declare const make: <Rts extends Routes>(
  input: { readonly rpc: Router<Rts> },
) => App<Rts, RouterRequirements<Rts>, never>

export declare const provide: <
  Provided,
  E2,
  RIn,
>(
  layer: Layer.Layer<Provided, E2, RIn>,
) => <Rts extends Routes, R, E>(
  app: App<Rts, R, E>,
) => App<Rts, Exclude<R, Provided> | RIn, E | E2>

export declare const serve: <
  Rts extends Routes,
  R,
  LayerE,
>(
  app: App<Rts, R, LayerE>,
  options?: AppServeOptions,
) => Effect.Effect<
  never,
  AppServeError | LayerE,
  R | HttpServer | WebSocketServer
>
```

`App` is an opaque frozen description. Its unique-symbol type witness preserves
generic channels but is not a runtime layer or handler array. `provide` returns
a new description, removes services supplied by the Layer, adds the Layer's
inputs, and unions Layer errors. `serve` builds Layers once in the root Scope
and delegates transport lifecycle to `blitz:http`. Every request/subscription
has a child Scope. Shutdown stops ingress, interrupts children, waits for
finalizers, and closes resources.

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

The following baseline fixes their common public shapes. Each module declares
its same-named `Context.Tag`, pipe-friendly accessors, `layerLive`, and a
deterministic test Layer or test factory. The detailed subplans below define the
remaining per-operation rules.

```ts
// blitz:crypto
export type DigestAlgorithm = "SHA-256" | "SHA-384" | "SHA-512"
export type AeadAlgorithm = "AES-256-GCM" | "CHACHA20-POLY1305"

export interface SecretKey {
  readonly _tag: "SecretKey"
  readonly algorithm: AeadAlgorithm | "HMAC-SHA-256"
  readonly exportPolicy: "allow" | "forbid"
}

export interface CryptoError {
  readonly _tag: "CryptoError"
  readonly operation:
    | "randomBytes"
    | "digest"
    | "generateKey"
    | "importKey"
    | "exportKey"
    | "mac"
    | "deriveKey"
    | "encrypt"
    | "decrypt"
  readonly reason:
    | "InvalidInput"
    | "PermissionDenied"
    | "UnsupportedAlgorithm"
    | "InvalidKey"
    | "NotExportable"
    | "AuthenticationFailed"
    | "EntropyUnavailable"
    | "Host"
  readonly message: string
}

export interface Crypto {
  readonly randomBytes: (
    length: number,
  ) => Effect.Effect<Uint8Array, CryptoError>
  readonly digest: (
    algorithm: DigestAlgorithm,
    data: Uint8Array,
  ) => Effect.Effect<Uint8Array, CryptoError>
  readonly generateKey: (
    algorithm: SecretKey["algorithm"],
    options?: { readonly export?: "allow" | "forbid" },
  ) => Effect.Effect<SecretKey, CryptoError, Scope.Scope>
  readonly importKey: (
    algorithm: SecretKey["algorithm"],
    bytes: Uint8Array,
    options?: { readonly export?: "allow" | "forbid" },
  ) => Effect.Effect<SecretKey, CryptoError, Scope.Scope>
  readonly exportKey: (
    key: SecretKey,
  ) => Effect.Effect<Uint8Array, CryptoError>
  readonly mac: (
    key: SecretKey,
    data: Uint8Array,
  ) => Effect.Effect<Uint8Array, CryptoError>
  readonly deriveKey: (
    input: SecretKey,
    options: {
      readonly salt: Uint8Array
      readonly info?: Uint8Array
      readonly outputAlgorithm: SecretKey["algorithm"]
      readonly export?: "allow" | "forbid"
    },
  ) => Effect.Effect<SecretKey, CryptoError, Scope.Scope>
  readonly encrypt: (
    key: SecretKey,
    plaintext: Uint8Array,
    options: {
      readonly nonce: Uint8Array
      readonly additionalData?: Uint8Array
    },
  ) => Effect.Effect<Uint8Array, CryptoError>
  readonly decrypt: (
    key: SecretKey,
    ciphertext: Uint8Array,
    options: {
      readonly nonce: Uint8Array
      readonly additionalData?: Uint8Array
    },
  ) => Effect.Effect<Uint8Array, CryptoError>
}

// blitz:os
export interface OsSnapshot {
  readonly platform: "linux" | "darwin" | "windows" | "other"
  readonly architecture: string
  readonly endianness: "little" | "big"
  readonly hostname: string
  readonly cpuCount: number
  readonly homeDirectory: Option.Option<string>
  readonly temporaryDirectory: string
}

export interface TemporaryEntry {
  readonly path: string
  readonly type: "file" | "directory"
  readonly close: Effect.Effect<void, OsError>
}

export interface OsError {
  readonly _tag: "OsError"
  readonly operation: "snapshot" | "temporaryFile" | "temporaryDirectory" | "close"
  readonly reason:
    | "InvalidInput"
    | "PermissionDenied"
    | "Unavailable"
    | "Cleanup"
    | "Io"
  readonly message: string
}

export interface Os {
  readonly snapshot: Effect.Effect<OsSnapshot, OsError>
  readonly temporaryFile: (
    options?: { readonly prefix?: string; readonly suffix?: string },
  ) => Effect.Effect<TemporaryEntry, OsError, Scope.Scope>
  readonly temporaryDirectory: (
    options?: { readonly prefix?: string },
  ) => Effect.Effect<TemporaryEntry, OsError, Scope.Scope>
}

// Shared by blitz:dns, blitz:tcp, blitz:udp, and blitz:tls.
export interface IpAddress {
  readonly hostname: string
  readonly family: "ipv4" | "ipv6"
}

export interface SocketAddress extends IpAddress {
  readonly port: number
}

export interface DnsRecord extends IpAddress {
  readonly canonicalName: Option.Option<string>
}

export interface DnsError {
  readonly _tag: "DnsError"
  readonly operation: "lookup" | "reverse"
  readonly reason:
    | "InvalidHostname"
    | "PermissionDenied"
    | "NotFound"
    | "Timeout"
    | "Unsupported"
    | "Io"
  readonly hostname?: string
  readonly message: string
}

export interface Dns {
  readonly lookup: (
    hostname: string,
    options?: {
      readonly family?: "any" | "ipv4" | "ipv6"
      readonly order?: "system" | "ipv4First" | "ipv6First"
      readonly timeout?: Duration.DurationInput
    },
  ) => Effect.Effect<ReadonlyArray<DnsRecord>, DnsError>
  readonly reverse: (
    address: IpAddress,
  ) => Effect.Effect<ReadonlyArray<string>, DnsError>
}

export interface TcpError {
  readonly _tag: "TcpError"
  readonly operation: "connect" | "listen" | "accept" | "read" | "write" | "shutdown" | "close"
  readonly reason:
    | "InvalidAddress"
    | "PermissionDenied"
    | "Refused"
    | "Timeout"
    | "Overflow"
    | "Disconnect"
    | "Closed"
    | "Unsupported"
    | "Io"
  readonly address?: SocketAddress
  readonly message: string
}

export interface TcpConnection {
  readonly localAddress: SocketAddress
  readonly remoteAddress: SocketAddress
  readonly bytes: Stream.Stream<Uint8Array, TcpError>
  readonly write: (
    bytes: Uint8Array,
  ) => Effect.Effect<void, TcpError>
  readonly shutdown: (
    direction: "read" | "write" | "both",
  ) => Effect.Effect<void, TcpError>
  readonly close: Effect.Effect<void, TcpError>
}

export interface Tcp {
  readonly connect: (
    address: SocketAddress,
    options?: { readonly timeout?: Duration.DurationInput },
  ) => Effect.Effect<TcpConnection, TcpError, Scope.Scope>
  readonly listen: (
    address: SocketAddress,
    options?: { readonly queueCapacity?: number },
  ) => Stream.Stream<TcpConnection, TcpError>
}

export interface Datagram {
  readonly peer: SocketAddress
  readonly bytes: Uint8Array
}

export interface UdpError {
  readonly _tag: "UdpError"
  readonly operation: "bind" | "receive" | "send" | "multicast" | "close"
  readonly reason:
    | "InvalidAddress"
    | "PermissionDenied"
    | "MessageTooLarge"
    | "Overflow"
    | "Closed"
    | "Unsupported"
    | "Io"
  readonly message: string
}

export interface UdpSocket {
  readonly localAddress: SocketAddress
  readonly datagrams: Stream.Stream<Datagram, UdpError>
  readonly send: (
    peer: SocketAddress,
    bytes: Uint8Array,
  ) => Effect.Effect<void, UdpError>
  readonly close: Effect.Effect<void, UdpError>
}

export interface Udp {
  readonly bind: (
    address: SocketAddress,
    options?: {
      readonly queueCapacity?: number
      readonly maxDatagramBytes?: number
    },
  ) => Effect.Effect<UdpSocket, UdpError, Scope.Scope>
}

export interface TlsError {
  readonly _tag: "TlsError"
  readonly operation: "connect" | "accept" | "read" | "write" | "close"
  readonly reason:
    | "InvalidOptions"
    | "PermissionDenied"
    | "Certificate"
    | "Verification"
    | "Handshake"
    | "Protocol"
    | "Timeout"
    | "Disconnect"
    | "Io"
  readonly message: string
}

export interface TlsConnection {
  readonly localAddress: SocketAddress
  readonly remoteAddress: SocketAddress
  readonly protocol: Option.Option<string>
  readonly bytes: Stream.Stream<Uint8Array, TcpError | TlsError>
  readonly write: (
    bytes: Uint8Array,
  ) => Effect.Effect<void, TcpError | TlsError>
  readonly close: Effect.Effect<void, TcpError | TlsError>
}

export interface TlsPrivateKey {
  readonly _tag: "TlsPrivateKey"
}

export interface Tls {
  readonly connect: (
    address: SocketAddress,
    options: {
      readonly serverName: string
      readonly protocols?: ReadonlyArray<string>
      readonly timeout?: Duration.DurationInput
    },
  ) => Effect.Effect<TlsConnection, TcpError | TlsError, Scope.Scope>
  readonly accept: (
    connection: TcpConnection,
    options: {
      readonly certificateChain: ReadonlyArray<Uint8Array>
      readonly privateKey: TlsPrivateKey
      readonly protocols?: ReadonlyArray<string>
      readonly timeout?: Duration.DurationInput
    },
  ) => Effect.Effect<TlsConnection, TcpError | TlsError, Scope.Scope>
}

// blitz:worker
export interface WorkerMessage {
  readonly data: unknown
}

export interface WorkerError {
  readonly _tag: "WorkerError"
  readonly operation: "spawn" | "postMessage" | "messages" | "terminate"
  readonly reason:
    | "InvalidSpecifier"
    | "PermissionDenied"
    | "Clone"
    | "Startup"
    | "Overflow"
    | "Closed"
    | "Host"
  readonly message: string
}

export interface Worker {
  readonly messages: Stream.Stream<WorkerMessage, WorkerError>
  readonly postMessage: (
    value: unknown,
  ) => Effect.Effect<void, WorkerError>
  readonly terminate: Effect.Effect<void, WorkerError>
}

export interface WorkerRuntime {
  readonly spawn: (
    specifier: string | URL,
    options?: {
      readonly queueCapacity?: number
      readonly permissions?: ReadonlyArray<string>
    },
  ) => Effect.Effect<Worker, WorkerError, Scope.Scope>
}

// blitz:test
export interface RuntimeLeaks {
  readonly operations: number
  readonly resources: number
  readonly roots: number
}

export interface TestRuntimeError {
  readonly _tag: "TestRuntimeError"
  readonly reason: "LeakedOperations" | "LeakedResources" | "LeakedRoots"
  readonly leaks: RuntimeLeaks
  readonly message: string
}

export interface TestRuntime {
  readonly leaks: Effect.Effect<RuntimeLeaks>
  readonly assertNoLeaks: Effect.Effect<void, TestRuntimeError>
}

export declare const withLeakCheck: <A, E, R>(
  effect: Effect.Effect<A, E, R>,
) => Effect.Effect<A, E | TestRuntimeError, R | TestRuntime>
```

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
cargo run -p blitz-cli -- precompile runtime --check
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
