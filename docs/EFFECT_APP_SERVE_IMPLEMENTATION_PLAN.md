# Effect download and `App.serve` implementation plan

This plan turns the existing virtual-module, event-registry, Effect manifest, and Rust RPC description slices into an executable JavaScript application runtime. It covers two linked deliverables:

1. Reproducibly vendor and register `effect@3.16.7` without downloading packages at build or runtime.
2. Implement `blitz:app` so `App.serve()` is a lazy, scoped Effect backed by Blitz's existing HTTP/WebSocket transports.

The implementation must preserve raw-engine isolation and keep `Blitz.serve` operational.

## Current starting point

Already present:

- `blitz-engine::VirtualModuleRegistry` accepts source/bytecode, declarations, and content hashes.
- `blitz-runtime::effect_assets` defines the pinned public Effect specifiers.
- `blitz-runtime::host::event_loop` unifies HTTP, WebSocket, and terminal delivery and tracks resource liveness.
- `blitz-runtime::app` and `blitz-runtime::rpc` provide a Rust-side description/protocol slice.

Important gaps:

- Effect assets do not exist and are not registered by `install_runtime`.
- Relative imports between virtual modules are not resolved relative to the importing virtual module.
- Timer, cancellation, monotonic-clock, and microtask host APIs required by Effect are absent.
- The current Rust `App::serve()` returns only a `ServePlan`; it is not the JavaScript `blitz:app` API.
- The low-level server does not expose a scoped application listener handle suitable for Effect acquisition/release.

## Architectural decisions

### Preserve one Effect module graph

Do not independently bundle every public Effect entry point. That can duplicate Effect internals and break service, fiber, scheduler, and singleton identity across `effect` and `effect/*` imports.

Vendor the upstream ESM graph once and expose public entries as aliases into that graph. Register internal files under canonical private specifiers such as:

```text
blitz-internal:effect/Effect.js
blitz-internal:effect/internal/fiberRuntime.js
```

Rewrite relative imports in generated assets to canonical private specifiers. This avoids filesystem resolution and gives every internal module one cache identity.

### Keep Effect semantics in JavaScript

`Effect`, `Layer`, `Scope`, fibers, interruption, and finalization execute from the upstream package. Rust supplies only host facilities and transport ownership.

### Split public JavaScript API from Rust transport state

- `blitz:app` owns immutable JavaScript `App` values and returns real upstream Effects.
- A private host bridge owns listener/subscription resources and dispatches transport events.
- The existing Rust `app.rs`/`rpc` model may supply protocol validation helpers, but it must not become a second Effect implementation.

### Make serving genuinely lazy

Calling `app.serve(options)` must not bind a socket. Binding occurs only when the returned Effect runs. Interruption closes the listener, interrupts request/subscription fibers, and waits for finalizers.

## Phase 1: generation workspace and pinned download

Create a pnpm workspace under `tools/effect-assets/`:

```text
tools/effect-assets/
  package.json
  pnpm-lock.yaml
  scripts/generate.ts
  scripts/verify.ts
  src/blitz-app.ts
  src/blitz-rpc.ts
  declarations/
  README.md
```

Use an exact dependency, never a range:

```json
{
  "devDependencies": {
    "effect": "3.16.7"
  }
}
```

Add packages with pnpm rather than editing dependency entries manually. Commit the workspace lockfile.

Generation steps:

1. Run `pnpm install --frozen-lockfile` in CI and `pnpm install` only when intentionally updating the lockfile.
2. Resolve `effect/package.json` and fail unless its version is exactly `3.16.7`.
3. Read the package's ESM export targets for the root and supported subpaths.
4. Walk their complete static import/re-export graph.
5. Reject dynamic or unresolved package imports instead of silently leaving filesystem dependencies.
6. Rewrite each internal relative specifier to a canonical `blitz-internal:effect/...` specifier.
7. Emit deterministic UTF-8 files with normalized LF endings and stable lexical ordering.
8. Copy Effect's MIT license and package metadata.
9. Generate `manifest.json` containing package version, source package integrity, every output SHA-256, canonical specifier, source type, and declaration asset.
10. Write to a temporary directory and atomically replace `crates/blitz-runtime/assets/effect/3.16.7/` only after validation succeeds.

Generated output:

```text
crates/blitz-runtime/assets/effect/3.16.7/
  LICENSE
  package.json
  manifest.json
  modules/...
  declarations/...
```

`verify.ts` regenerates into a temporary directory and byte-compares it with the committed assets. CI fails for stale files, changed hashes, untracked outputs, wrong versions, missing licenses, or imports escaping the manifest.

### Phase 1 acceptance

- Generation succeeds from a clean checkout with pnpm's frozen lockfile.
- A second generation produces no diff.
- No Cargo build script or runtime path accesses the network.
- All generated import specifiers resolve to another manifest entry.
- Root and deep imports share internal module identities.

## Phase 2: finish virtual-module graph support

Update `crates/blitz-engine/src/module.rs` before loading the generated graph:

1. Add importer-aware virtual resolution.
2. Resolve canonical private imports directly and relative virtual imports against the importing module's canonical specifier.
3. Use canonical specifiers—not synthetic filesystem paths—as graph identity.
4. Propagate construction errors for all imports from virtual modules; do not drop unresolved dependency errors.
5. Apply the same resolution path during construction, instantiation, re-export resolution, evaluation ordering, and dynamic import.
6. Expose declaration source by canonical specifier for project typechecking.

Add engine tests for:

- Relative virtual imports.
- Cycles and live bindings.
- Root/deep alias identity.
- Re-exports and dynamic import.
- A missing virtual dependency producing a source-located error.

### Phase 2 acceptance

A generated multi-file fixture can import, instantiate, cache, cycle, and dynamically import without touching the filesystem.

## Phase 3: runtime asset installer

Replace the metadata-only behavior in `crates/blitz-runtime/src/effect_assets.rs` with an installer:

```rust
pub fn install_effect_modules(vm: &Vm) -> Result<(), EffectAssetError>
```

The installer should:

1. Embed committed assets with `include_str!` or generated static tables.
2. Verify embedded SHA-256 hashes against the committed manifest in tests (and optionally debug builds).
3. Register every private Effect module first.
4. Register public aliases for `effect` and supported `effect/*` entries.
5. Register declarations alongside runtime sources.
6. Register `blitz:match` as a real re-export of `effect/Match`.
7. Reject duplicate canonical specifiers.

Call it from `install_runtime` before user module construction. Do not call it from the raw `blitz-engine` executable or Test262 raw-engine paths.

### Phase 3 acceptance

Programs run successfully with all supported root/deep imports, while raw-engine tests confirm those modules are unavailable unless an embedder registers them.

## Phase 4: Effect scheduler host prerequisites

Add host facilities in `blitz-runtime`, not `blitz-engine`:

- `queueMicrotask` wired to the VM microtask queue.
- `setTimeout` / `clearTimeout`.
- `setInterval` / `clearInterval` if required by the pinned Effect graph or compatibility fixtures.
- A monotonic `performance.now()` implementation.
- Cancellation-safe timer IDs.
- Timer resources integrated with `ResourceRegistry`, including reference/unreference behavior.
- An explicit runtime wakeup event so an Effect scheduler callback wakes an otherwise idle runtime.

After each HTTP, WebSocket, terminal, or timer event:

1. Dispatch the host event.
2. Flush VM promise microtasks.
3. Run Effect scheduler work until its immediate queue is quiescent.
4. Re-check runtime liveness.

Add compatibility fixtures for `Effect.sleep`, fork/join, interruption, `Scope.addFinalizer`, schedules, and stream timing before implementing serving.

### Phase 4 acceptance

Effect finalizers run exactly once on success, failure, defect, and interruption. A pending referenced timer/fiber keeps the runtime alive; canceled/unreferenced work does not.

## Phase 5: private scoped transport bridge

Refactor the existing server host around an internal application transport handle rather than implementing a second server:

```text
openAppServer(options, dispatch) -> server resource
closeAppServer(resource) -> completion
```

The bridge must:

- Bind only during Effect acquisition.
- Validate hostname, port, and RPC path before binding.
- Return typed bind failures to JavaScript.
- Register the listener as a referenced runtime resource.
- Route only the configured RPC path; preserve normal 404 behavior elsewhere.
- Reuse `HttpServer` and the existing WebSocket upgrade implementation.
- Track request and subscription fibers by server/connection/request ID.
- Stop accepting work during release.
- Interrupt active fibers and subscriptions.
- Close sockets/listener and await transport tasks before completing release.

Keep bridge functions private or under a clearly internal namespace. Do not change ECMAScript coercion/property behavior to support them.

### Phase 5 acceptance

Acquisition failures are observable Effect failures, release is idempotent, and interrupting the root serve fiber leaves no listener, socket, timer, request fiber, or referenced resource alive.

## Phase 6: implement `blitz:app`

Implement the runtime source in the generation workspace and register it as a virtual module.

Required immutable shape:

```ts
interface AppDescription {
  readonly rpc: Router
  readonly layers: ReadonlyArray<Layer.Layer<unknown, unknown, unknown>>
}
```

`App.make({ rpc })` freezes a new description. `App.provide(layer)` returns a new description with a new readonly layer list and does not mutate prior Apps.

`app.serve(options)` returns an upstream Effect assembled conceptually as:

```ts
Effect.scoped(
  Effect.acquireRelease(
    acquireAndStart(app, options),
    (server) => server.close,
  ).pipe(Effect.flatMap((server) => server.awaitShutdown)),
)
```

Exact implementation requirements:

1. Constructing the Effect performs no host I/O.
2. Merge/provide application layers inside the root scope.
3. Acquire the transport only after layers build successfully.
4. Fork each request and subscription in a child scope supervised by the root application scope.
5. Decode RPC input before middleware/handler execution.
6. Run middleware in declaration order.
7. Preserve expected tagged failures and redact defects.
8. Close request scopes after serialization.
9. On interruption, stop ingress, interrupt children, await finalizers, then close transports.
10. Resolve `serve` only by interruption/failure; its success channel remains `never` while live.

Expose a curated `blitz:app.d.ts` with `App.make`, `App.provide`, `ServeOptions`, `ServeError`, and requirement reduction. No public declaration may use `any`.

The Rust `ServePlan` should either become an internal host configuration type or be renamed to avoid suggesting it is the public JavaScript Effect API.

### Phase 6 acceptance

- Calling `app.serve()` alone does not occupy a port.
- `Effect.runFork(app.serve(...))` binds and serves.
- Interrupting that fiber releases the port and runs layer/request/subscription finalizers.
- A layer construction error prevents binding.
- A bind error remains in the Effect error channel.
- Two Apps derived through `App.provide` do not mutate one another.

## Phase 7: unary RPC vertical slice

Before subscriptions, complete one end-to-end unary path:

1. `POST /rpc` envelope parsing and 1 MiB enforcement.
2. Nested router lookup and query/mutation kind validation.
3. Explicit decoder execution against `unknown`.
4. Request metadata and cancellation signal construction.
5. Effect handler execution with application layers.
6. JSON-safe success/domain-error serialization.
7. HTTP 200 for valid RPC envelopes, including domain failures.
8. HTTP 400/404/413 for malformed protocol, unknown HTTP path, and oversized body.
9. Batches bounded at 32.

Do not add WebSocket subscriptions until unary cancellation, finalization, and concurrent request tests pass.

## Phase 8: subscription serving

Use the same scoped server resource and `/rpc` upgrade route. One connection supervises a map of subscription fibers. `cancel`, disconnect, shutdown, and queue exhaustion all interrupt the relevant fibers and close scopes. Preserve the existing 1 MiB WebSocket limit and close exhausted command queues with code `1013`.

## Test matrix

### Generated assets

- Deterministic generation and stale-output rejection.
- Exact version/license/hash checks.
- No unresolved imports or duplicate canonical modules.

### Effect compatibility

- Root and every supported deep import.
- Context/Layer service identity across different import paths.
- Scope/finalizers, fork/join/interruption, timers/schedules, streams, and Match.

### `App.serve`

- Laziness and bind failure.
- Layer acquisition before listener acquisition.
- Graceful root interruption and finalizer ordering.
- Concurrent unary requests and client disconnect cancellation.
- Batch bounds and JSON rejection.
- Multiplexed subscription completion/failure/cancel/disconnect/backpressure.
- Existing `Blitz.serve` and WebSocket regression tests.

### Commands

```sh
cargo fmt --all -- --check
cargo test -p blitz-engine -p blitz-runtime -p blitz-cli
cargo clippy --workspace --all-targets
pnpm --dir tools/effect-assets typecheck
pnpm --dir tools/effect-assets test
pnpm --dir tools/effect-assets verify
```

The repository currently emits pre-existing Clippy warnings. New code should not add warnings in the files it changes; warning cleanup outside this plan should remain separate.

## Pull-request sequence

Keep changes reviewable in this order:

1. Generation workspace, lockfile, license, committed Effect assets, and hash verification.
2. Importer-aware virtual-module resolution and graph tests.
3. Runtime installer and Effect import smoke tests.
4. Timer/microtask/monotonic-time host APIs and Effect scheduler fixtures.
5. Scoped server bridge with acquire/release tests.
6. `blitz:app` runtime and curated declarations.
7. Unary RPC serving and cancellation.
8. WebSocket subscriptions and shutdown supervision.

Do not combine asset generation and server lifecycle changes into one pull request. `App.serve` depends on validated Effect scheduler/finalizer behavior; implementing it earlier would make lifecycle failures difficult to distinguish from engine compatibility failures.
