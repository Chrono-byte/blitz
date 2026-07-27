# Blitz Deno-compatible core plan

## Status

This compatibility track is paused. The default runtime no longer installs the
`Blitz.*` namespace; Effect-native `blitz:*` modules are the sole planned public
capability API. The compatibility implementation and fixtures remain available
behind explicit host configuration while migrations are completed.

## Original goal

Grow `Blitz.*` into a focused, behavior-tested counterpart to the most useful
Deno runtime APIs. Compatibility means matching Deno's signatures, lifecycle,
error timing, cancellation, and observable behavior where Blitz exposes the
same member. It does not mean exposing the entire Deno namespace immediately.

The target is current Deno 2.x behavior, pinned per compatibility fixture so
upstream changes are intentional upgrades rather than accidental drift.

## Boundaries

1. `denko-engine` remains host-neutral and implements only ECMAScript behavior.
2. `blitz-runtime` owns `Blitz.*`, operating-system capabilities, resources,
   permissions, and event delivery.
3. `blitz-cli` owns diagnostics and command-line presentation.
4. Standard Web APIs stay global (`Request`, `Response`, `WebSocket`, `fetch`,
   streams, URL, crypto); they should not be duplicated under `Blitz`.
5. Effect services and `Blitz.*` adapters share the same runtime resources.
   Neither API may wrap or supervise a detached instance of the other.
6. Tokio tasks exchange owned data through bounded channels. JavaScript values
   and callbacks remain rooted and invoked on the VM thread.
7. Existing Blitz-only conveniences remain available unless they prevent
   compatible behavior. Extensions must be documented as such.

## Compatibility contract

Every added member requires:

- a public TypeScript declaration;
- a Deno comparison fixture covering success and failure behavior;
- tests for cancellation, shutdown, and resource liveness when applicable;
- stable error classes/codes and matching synchronous-versus-asynchronous
  failure timing;
- explicit permission checks before side effects;
- documentation identifying supported options and intentional differences;
- no raw-engine global changes.

Maintain a machine-readable surface manifest with each member classified as
`compatible`, `partial`, `extension`, or `planned`. CI should reject an exported
member without declarations and compatibility metadata.

## Phase 0: surface and test infrastructure

**Status:** Initial declarations, the machine-readable surface manifest, and a
shared Deno/Blitz server lifecycle fixture are implemented.

Create the foundation before adding more methods:

- add global declarations for `Blitz`, server handles, addresses, standard I/O,
  resource handles, and errors;
- add a Deno 2.x compatibility fixture harness that can execute the same
  behavioral case against `Deno` and `Blitz`;
- add the surface manifest and documentation generator;
- define reusable runtime errors and abort/cancellation plumbing;
- define a common resource-handle contract: idempotent close, generation-safe
  events, `ref()`, `unref()`, and observable completion;
- inventory the existing members as partial or extensions.

Initial inventory:

| Member | Classification |
| --- | --- |
| `Blitz.serve` | partial Deno counterpart |
| `Blitz.upgradeWebSocket` | partial Deno counterpart |
| `Blitz.stdin.onLine/onClose/setPrompt/close` | Blitz extension |
| `Blitz.stdout.write/writeLine` | partial/extension |

## Phase 1: finish the server family

This is the first implementation milestone because the transport, event queue,
timers, and scoped Effect acquisition already exist.

### `Blitz.serve`

**Status:** The first lifecycle handle slice is implemented. `signal`,
`onListen`, `onError`, DNS hostnames, and fully asynchronous shutdown remain.

Return a real server handle instead of only `{ hostname, port }`:

- `addr` with Deno-shaped TCP address metadata;
- `finished: Promise<void>`;
- `shutdown(): Promise<void>`;
- `ref()` and `unref()`;
- preserve `hostname` and `port` temporarily as deprecated aliases.

Add compatible options incrementally:

- `hostname` and `port` with Deno-compatible validation and hostname resolution;
- `signal` for abort-driven shutdown;
- `onListen`;
- `onError` with response validation;
- graceful shutdown with active HTTP and WebSocket connections accounted for.

Keep raw `Blitz.serve` eager like `Deno.serve`. Keep `blitz:app` lazy and scoped.
Both must acquire the same underlying listener resource.

### Request and response behavior

Replace the current convenient object subset with standards-backed objects:

- request body bytes and one-shot body consumption;
- `arrayBuffer()`, `bytes()`, `text()`, and `json()` behavior;
- abort signal propagation on disconnect;
- streaming request and response bodies with bounded backpressure;
- correct header combination, status validation, and body/status restrictions;
- response errors routed through `onError` rather than hidden by generic 500s.

### `Blitz.upgradeWebSocket`

Match Deno's return shape and supported options:

- `{ response, socket }` remains stable;
- subprotocol negotiation;
- idle timeout behavior;
- validation of upgrade headers and repeated upgrades;
- correct socket state, close/error ordering, and binary data behavior;
- shutdown integration with the owning server handle.

### Phase 1 acceptance

- Existing HTTP/WebSocket demos continue to work.
- A server can be interrupted, shut down, and rebound to the same port.
- `finished` settles exactly once after listener and connection cleanup.
- `ref`/`unref` affect runtime liveness without closing the listener.
- Shared fixtures produce equivalent normalized observations under Deno and
  Blitz for every supported option.

## Phase 2: Deno-shaped standard I/O

Build byte and stream primitives first, then extend the existing terminal
objects without removing line-editor conveniences.

### `Blitz.stdin`

Add:

- `readable`;
- `read(buffer)` and `readSync(buffer)` where support is practical;
- `isTerminal()`;
- `setRaw(mode, options?)`;
- compatible EOF and cancellation behavior.

Retain `onLine`, `onClose`, `setPrompt`, and `close` as documented Blitz
extensions for interactive applications.

### `Blitz.stdout` and `Blitz.stderr`

Add:

- `writable`;
- `write(bytes)` and `writeSync(bytes)` returning byte counts;
- `isTerminal()`;
- ordered, backpressured writes.

Move string convenience toward explicit extension names or preserve current
methods with clear overload documentation. Add `Blitz.stderr` in the same
slice.

## Phase 3: runtime and process core

Expose Deno-shaped runtime metadata over existing host state:

- `Blitz.args`;
- `Blitz.cwd()` and `Blitz.chdir()`;
- `Blitz.execPath()`;
- `Blitz.pid`, `Blitz.ppid`, `Blitz.build`, and `Blitz.version`;
- `Blitz.exit()` and `Blitz.exitCode`;
- `Blitz.env.get/set/delete/has/toObject`;
- `Blitz.memoryUsage()` and `Blitz.systemMemoryInfo()` only when their values
  can be defined accurately across supported platforms.

Do not duplicate state independently from `process`; both adapters should read
and operate on the same runtime-owned process service.

## Phase 4: permissions and filesystem core

Permission policy must land before broad filesystem or subprocess access.
Start with explicit read, write, environment, network, run, and signal grants.
Denied operations must fail before opening a resource.

Then add a coherent filesystem subset:

- `readFile`, `readTextFile`, `writeFile`, and `writeTextFile`;
- `stat`, `lstat`, `readDir`, `mkdir`, and `remove`;
- `rename`, `copyFile`, `realPath`, and `makeTempFile/Directory`;
- `open` returning a tracked file resource;
- async APIs first, synchronous counterparts only after shared semantics pass.

Use Deno-compatible URL/path handling and structured file-info values. Tokio
workers must never mutate VM state directly.

## Phase 5: subprocesses, signals, and low-level networking

Add these only after permissions and generic resources are stable:

- `Blitz.Command` with bounded stdout/stderr collection and streaming handles;
- child status, kill, cancellation, and drop behavior;
- `addSignalListener`, `removeSignalListener`, and `kill`;
- `connect`, `listen`, and TLS counterparts;
- DNS resolution where the Deno contract can be matched.

Each resource must use the same event registry, liveness rules, cancellation,
and generational stale-event protection as timers and servers.

## Intentional non-goals for the core milestone

Defer until the preceding contracts are stable:

- FFI and dynamic libraries;
- unsafe OS handles;
- npm/package management APIs;
- workers and inspector APIs;
- KV, cron, deploy-specific APIs, and unstable Deno namespaces;
- aliases that merely exist but cannot preserve Deno semantics.

## Delivery order

Use independently reviewable slices:

1. Global declarations, surface manifest, and fixture harness.
2. Server handle with `addr`, `finished`, `shutdown`, `ref`, and `unref`.
3. `signal`, `onListen`, and `onError` server options.
4. Standards-backed request/response bodies and streaming.
5. WebSocket options and shutdown conformance.
6. Byte-oriented stdin/stdout/stderr and stream adapters.
7. Runtime metadata and environment adapter.
8. Permission policy.
9. Filesystem core.
10. Commands, signals, and low-level networking.

The next implementation slice should be items 1 and 2: establish measurable
compatibility, then make `Blitz.serve` return a lifecycle-correct server handle
without changing `blitz:app`'s scoped semantics.
