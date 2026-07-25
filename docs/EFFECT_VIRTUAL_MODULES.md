# Effect and Blitz runtime-graph manifest

`blitz-runtime` installs one generated, prelinked runtime graph for the first
application-runtime Effect surface. It pins `effect@3.16.7`, includes the
manifest-listed Blitz facades/private adapters, and keeps the raw engine unaware
of Effect- and Blitz-specific specifiers.

The manifest currently supports:

| Specifier | Origin |
| --- | --- |
| `effect` | Effect 3.16.7 root asset |
| `effect/Effect` | Effect 3.16.7 generated subpath |
| `effect/Context` | Effect 3.16.7 generated subpath |
| `effect/Layer` | Effect 3.16.7 generated subpath |
| `effect/Ref` | Effect 3.16.7 generated subpath |
| `effect/Stream` | Effect 3.16.7 generated subpath |
| `effect/Scope` | Effect 3.16.7 generated subpath |
| `effect/Schedule` | Effect 3.16.7 generated subpath |
| `effect/Match` | Effect 3.16.7 generated subpath |
| `blitz:match` | Blitz facade over `effect/Match` |
| `blitz:fs` | Typed filesystem service and live Layer |
| `blitz:http` | Scoped Effect HTTP service |
| `blitz:websocket` | Scoped connections and message Streams |
| `blitz:terminal` | Effect output and scoped line Stream |
| `blitz:runtime` | Aggregate live Layer and supervised root runner |
| `blitz:app` | Transitional application facade |
| `node:process` | Scoped Node-compatible view of the runtime process singleton |
| `blitz-internal:*` | Private host-ABI adapters |

Public-root resolution is exact and case-sensitive. It does not normalize or
fall back to arbitrary `effect/*` or `blitz:*` imports. Non-root modules remain
reachable only through bundled dependency indices; direct user imports of
`blitz-internal:*` fail.

## Asset contract

The pnpm workspace at `tools/effect-assets/` downloads the exact package only
during explicit generation. It commits a canonical ESM graph, Effect's MIT
license, package integrity, deterministic SHA-256 manifest, and curated
public declarations under `crates/blitz-runtime/assets/effect/3.16.7/`.
Private runtime modules intentionally have no declaration assets; Blitz does
not vendor Effect's complete upstream declaration graph.

The separate stdlib manifest under
`crates/blitz-runtime/assets/stdlib/` owns facade source, declarations,
visibility, dependencies, hashes, and host primitive requirements.
`BLITZ_MATCH_SUPPORTED_EXPORTS` identifies the initial curated Match declaration
surface (`value`, `type`, `valueTags`, `typeTags`, and `exhaustive`).

The checked Rust `effect-bytecode` tool combines both verified manifests into
`assets/runtime/v1/runtime-graph.blitzpg`. `install_runtime` decodes and caches
that artifact, validates exact manifest hashes and runtime/host ABI, installs
host primitives, and atomically registers graph plus public declarations. It
does not register bundled JavaScript through source-backed virtual modules.

Asset generation, parser-free registration, import, and synchronous Effect
execution are landed. Pipeline counters assert zero bundled parses, import
extraction, AST/ESTree linking, typechecks, and compilations. Runtime regressions cover asynchronous execution through `Effect.runFork` and
synchronous host boundaries through `Effect.runSync` / `Effect.runSyncExit`,
including typed `Effect.try` recovery with `catchTag` and state transitions
through `Ref`.

The default runtime does not install the legacy `Blitz.*` compatibility
namespace. Applications use the Effect-native `blitz:*` modules; compatibility
tests may explicitly opt into the dormant adapter.

`blitz:fs` currently provides lazy typed whole-file reads and writes for UTF-8
text and `Uint8Array` data. The initial adapter is synchronous beneath the
Effect boundary; cancellable Tokio-backed file operations and scoped open-file
resources remain follow-up work. `blitz:http`, `blitz:websocket`, and `blitz:terminal` share
runtime-owned resources, while `blitz:runtime` supplies their live Layers and
starts one root Effect. HTTP request contexts supervise accepted WebSocket
connection fibers in the listener Scope. HTTP handlers currently must settle
without an asynchronous suspension; timer- or I/O-suspended request Effects
remain a runtime-kernel follow-up.

`blitz:app` serving is lazy and scoped: listener validation and binding occur
only when the serve Effect runs, invalid options and bind failures use tagged
`ServeError` values, and release idempotently closes ingress, active
connections, and the path forwarder. The listener is registered with runtime
liveness while acquired. Interruption is supported by VM-thread timer globals
(`setTimeout`, `clearTimeout`, `setInterval`, and `clearInterval`) backed by the
runtime event queue; callbacks remain rooted until firing or cancellation, and
`performance.now()` uses a monotonic clock. RPC router/layer request dispatch
remains a separate slice; requests currently enter the compatibility event
channel.

The engine now initializes public, private, computed, and static class fields,
including Effect's scheduler and metric-registry state such as `buckets = []`
and `map = empty()`. Computed names are evaluated once at class definition,
and public fields use own-property definition semantics rather than inherited
setter assignment.

The execution path also depends on object spread preserving enumerable string
and symbol properties, derived-constructor `this` surviving nested calls after
`super()`, and the standard `Set.prototype.delete` operation. These are now
implemented. Spread `super(...arguments)` remains a separate unsupported
constructor form outside the generated path exercised here.

The generated graph currently substitutes `Map` for Effect's internal
`WeakMap` identity caches because Blitz exposes only a placeholder `WeakMap`.
This preserves results but retains cache keys strongly; replace the transform
when the engine implements faithful weak collections. Effect's optional `URL`
hash branch is also guarded because `URL` is a runtime host API rather than an
ECMAScript engine intrinsic.
