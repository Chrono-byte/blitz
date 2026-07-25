# Node 24 module implementation plan

## Scope

This document plans every public built-in module reported by Node.js
`v24.18.0`. It complements `docs/NODE_COMPATIBILITY_PLAN.md`, which owns the
architecture, resolver policy, profiles, permissions, and general definition
of done.

The inventory is exhaustive for the selected reference release, but support is
incremental. A module is not exposed merely because it appears here. Each
public root enters the generated compatibility manifest only when its declared
slice passes runtime, declaration, differential, lifecycle, and permission
tests.

Canonical support begins with `node:*` specifiers. Unprefixed aliases are a
separate compatibility phase because they require Node's builtin precedence
over same-named npm packages.

## Status labels

- **Landed**: available in the generated runtime graph and tested.
- **Next**: next coherent implementation slice.
- **Planned**: dependencies and initial contract are defined.
- **Deferred**: intentionally scheduled after the general package/tooling
  target.
- **Compatibility-only**: deprecated or legacy API implemented only when real
  packages require it.
- **Platform-gated**: available only where the host platform can implement the
  Node contract faithfully.

## Delivery phases

| Phase | Goal |
| --- | --- |
| N0 | Registry propagation, declarations, `import.meta.url`, errors, profiles, and permissions |
| N1 | Pure modules and process-local utilities |
| N2 | Bytes, events, async context, streams, and scheduler semantics |
| N3 | Filesystem, terminal, readline, module metadata, and watching |
| N4 | Crypto, compression, DNS, sockets, and HTTP |
| N5 | Subprocesses, workers, VM contexts, testing, and developer tooling |
| N6 | Specialized, deprecated, and platform-specific modules |

A phase indicates dependency order, not one large pull request. Every row is a
separately reviewable vertical slice.

## Shared prerequisites

### N0.1 Module registry

- Preserve the selected Node compatibility profile in static and dynamic
  imports.
- Keep canonical module identity per realm.
- Reject disabled and unknown `node:*` modules before filesystem resolution.
- Keep private `blitz-internal:node/*` adapters inaccessible to source.
- Drive runtime and editor declarations from public registered roots rather
  than hard-coded specifier prefixes.

### N0.2 Module metadata

- Implement `import.meta.url` from the canonical module record.
- Add `import.meta.dirname` and `import.meta.filename` only if the pinned Node
  release exposes and tests them.
- Preserve file URLs across symlinks according to the selected resolver policy.
- Define JSON module and import-attribute behavior separately from filename
  resolution.

### N0.3 Common runtime contracts

- Node-shaped errors with stable `name`, `message`, `code`, and operation fields.
- VM-thread callback/root ownership; Rust worker tasks never mutate JS values.
- Event-loop ordering for next ticks, Promise jobs, timers, I/O completions,
  immediates, close callbacks, and shutdown.
- `ref`, `unref`, `hasRef`, cancellation, abort, and resource cleanup semantics.
- Permission checks at host-operation boundaries for filesystem, environment,
  network, subprocess, inspector, workers, native loading, and databases.

### N0.4 Required engine primitives

Implement these before modules that depend on them:

- faithful `ArrayBuffer`, `SharedArrayBuffer`, typed arrays, and `DataView`;
- byte-view aliasing, detachment, transfer, and encoding primitives;
- weak references and finalization where observable APIs require them;
- module-context isolation for `node:vm` and workers;
- async stack/context hooks needed by `node:async_hooks`;
- Web Streams alignment sufficient for `node:stream/web`.

## Profiles

| Profile | Intended use | Modules |
| --- | --- | --- |
| `off` | Raw engine and conformance environments | None |
| `core` | Small ESM CLI/services | Completed N1/N2 modules without ambient I/O beyond process metadata |
| `tooling` | Vite and development tools | `core` plus filesystem, watchers, streams, crypto, compression, DNS, sockets, HTTP, module metadata, terminal |
| `full` | Explicit Node-oriented runtime | Every completed module, still permission-gated |

Deprecated modules never enter `core` or `tooling` merely for completeness.

## Complete module inventory

### Assertions, utilities, and pure data

| Module | Status / phase | Initial contract | Prerequisites and notes |
| --- | --- | --- | --- |
| `node:assert` | Next / N1 | callable default assertion, `ok`, equality/deep equality, rejects/throws, `AssertionError`, match helpers | Deep comparison must handle cycles, symbols, prototypes, collections, errors, and typed arrays before claiming parity |
| `node:assert/strict` | Next / N1 | strict-default alias with the same singleton exports as `node:assert`.strict | Depends on `node:assert`; alias identity tests required |
| `node:path` | Next / N1 | complete POSIX-default path API, `posix`, `win32`, `matchesGlob` when stable in Node 24 | Pure prelinked JS; differential corpus on all host OSes |
| `node:path/posix` | Next / N1 | same object/API as `node:path`.posix | Canonical subpath root and identity tests |
| `node:path/win32` | Next / N1 | same object/API as `node:path`.win32 | Must behave as Windows paths even on Unix |
| `node:querystring` | Planned / N1 | parse/stringify/escape/unescape and configurable separators | Legacy but common and pure |
| `node:punycode` | Compatibility-only / N6 | Node 24 deprecated builtin behavior | Prefer packages using userland `punycode`; do not add to default profiles without fixture demand |
| `node:url` | Planned / N1 | URL globals, URL conversion helpers, domain conversion, URL-to-options helpers, legacy URL API only as needed | Requires `import.meta.url`, correct file URL and Windows path handling |
| `node:util` | Planned / N1-N2 | format/inspect, callbackify/promisify, debug helpers, MIME/text utilities as prerequisites permit | Inspection must share policy with console without changing language coercion |
| `node:util/types` | Planned / N1-N2 | every predicate representable by Blitz; explicit false/unsupported policy for absent VM kinds | Depends on faithful runtime brands, typed arrays, promises, proxies, iterators |
| `node:sys` | Compatibility-only / N6 | exact deprecated alias of `node:util` | Alias identity; never a separate implementation |
| `node:constants` | Planned / N3-N4 | immutable constants exported by Node 24 | Generated from filesystem, crypto, OS, signal, TLS and zlib support; platform differential tests |

### Process, console, timing, and diagnostics

| Module | Status / phase | Initial contract | Prerequisites and notes |
| --- | --- | --- | --- |
| `node:process` | Landed, partial / N1-N2 | default process singleton plus metadata currently landed; add exit state, signals, warnings, stdio, resource usage and next ticks incrementally | Named/default exports must share global `process`; `versions.node` must not claim Node unless compatibility policy explicitly defines it |
| `node:console` | Planned / N1 | `Console` plus default console identity and formatting behavior | Reuse low-level output/inspection primitives, not a wrapper around global methods |
| `node:os` | Planned / N1-N3 | platform/arch/EOL, home/temp dirs, CPUs, memory/load, network interfaces, priorities and user info | Host snapshots plus platform-specific error/permission tests |
| `node:timers` | Planned / N2 | timeout, interval, immediate handles with ref/unref/refresh/close semantics | Requires immediate queue and Node event-loop ordering |
| `node:timers/promises` | Planned / N2 | promise timers, scheduler iterator, abort and ref options | Shares timer resources; cancellation must release roots |
| `node:perf_hooks` | Planned / N2 | performance singleton, marks/measures, observer, event-loop utilization where measurable | Requires monotonic timing and bounded observer records |
| `node:diagnostics_channel` | Planned / N2 | channels, subscribers, tracing channels, AsyncLocalStorage binding | No-op fast path and exception behavior must match Node |
| `node:async_hooks` | Planned / N2-N5 | AsyncLocalStorage first; async resource/hooks only after runtime IDs and lifecycle events are sound | High-risk cross-cutting work; differential ordering tests mandatory |
| `node:trace_events` | Deferred / N6 | tracing categories and enabled-state surface | Needs runtime tracing sink and lifecycle; permission/config gated |

### Bytes, events, and streams

| Module | Status / phase | Initial contract | Prerequisites and notes |
| --- | --- | --- | --- |
| `node:buffer` | Planned / N2 | Buffer constructor/factories, encodings, views, search/compare, integer/float/bigint access, transcode constants | Blocked on faithful byte primitives; global `Buffer` and module export identity must match |
| `node:string_decoder` | Planned / N2 | incremental decoder across split multibyte sequences | Depends on Buffer and encoding tables |
| `node:events` | Planned / N2 | EventEmitter, EventEmitterAsyncResource, once/on, abort integration, listener limits and error events | Foundational for streams, networking, process and tooling |
| `node:stream` | Planned / N2 | Readable, Writable, Duplex, Transform, PassThrough, pipeline, finished, compose and adapters | Implement state machines and backpressure, not superficial wrappers |
| `node:stream/promises` | Planned / N2 | promise pipeline and finished | Same resource/cancellation semantics as callback APIs |
| `node:stream/consumers` | Planned / N2 | arrayBuffer, blob where available, buffer, json, text | Bounded collection limits and abort behavior |
| `node:stream/web` | Planned / N2 | Node's Web Streams exports and identity | Prefer shared engine/runtime Web Streams implementation; conversion parity tests |

### Filesystem and terminal interaction

| Module | Status / phase | Initial contract | Prerequisites and notes |
| --- | --- | --- | --- |
| `node:fs` | Planned / N3 | stats and paths, sync file/directory operations, callbacks, file handles, streams, watchers, constants | Share low-level host operations with `blitz:fs`; preserve Node coercion/errors separately |
| `node:fs/promises` | Planned / N3 | FileHandle and promise equivalents, abort where Node supports it | Async completion via runtime event boundary; bounded worker work |
| `node:tty` | Planned / N3 | ReadStream/WriteStream metadata, color depth, raw mode, `isatty` | Terminal resource integration and platform tests |
| `node:readline` | Planned / N3 | Interface, promises-compatible lifecycle, cursor/screen helpers, async iteration | Built on streams/TTY; prompt-safe output and signal handling |
| `node:readline/promises` | Planned / N3 | promise Readline and Interface APIs | Shares terminal ownership with callback API |
| `node:repl` | Deferred / N5 | start/recoverable parsing/writer/completion/context | Build on Blitz parser and CLI REPL without changing raw parser semantics |

### Module loading and execution

| Module | Status / phase | Initial contract | Prerequisites and notes |
| --- | --- | --- | --- |
| `node:module` | Planned / N3-N5 | builtinModules/isBuiltin, createRequire, findPackageJSON, register hooks only when loader supports their semantics | `createRequire` is blocked on real CommonJS; ESM metadata can land first |
| `node:vm` | Deferred / N5 | contexts, Script, SourceTextModule/SyntheticModule where Node 24 exposes them | Requires realm/context isolation, timeout/interruption and code-generation policy |
| `node:worker_threads` | Deferred / N5 | Worker, MessagePort/Channel, structured clone, transfer lists, worker data and environment | Requires independent VM ownership and bounded owned-message IPC |
| `node:child_process` | Deferred / N5 | spawn/exec/execFile/fork and sync variants with stdio, signals and abort | Explicit subprocess permission; `fork` blocked on compatible module execution |
| `node:cluster` | Deferred / N6 | primary/worker process orchestration | Depends on child processes, net handle sharing and IPC; server applications should prefer platform-native scaling |

### Networking and protocols

| Module | Status / phase | Initial contract | Prerequisites and notes |
| --- | --- | --- | --- |
| `node:dns` | Planned / N4 | lookup and resolver APIs, order/default-result controls | Tokio/host resolver, cancellation and platform error mapping |
| `node:dns/promises` | Planned / N4 | promise resolver and lookup APIs | Shared resolver resources and error identity |
| `node:net` | Planned / N4 | Socket/Server, connect/listen, half-close, timeout, ref/unref and block lists | Bounded transport queues and VM-thread events |
| `node:tls` | Planned / N4 | TLS sockets/servers, secure contexts, certificate metadata | Pin TLS backend/policy; secret and trust-store handling |
| `node:dgram` | Planned / N4 | UDP sockets, multicast/broadcast, connected mode | Network permission and platform-specific option tests |
| `node:http` | Planned / N4 | client requests, Agent, server, messages, headers, upgrade/connect and streaming bodies | Separate Node facade over shared transport, not coercion changes to `blitz:http` |
| `node:https` | Planned / N4 | HTTPS client/server and Agent | Depends on HTTP and TLS |
| `node:http2` | Deferred / N4-N6 | client/server sessions, streams, settings and flow control | Requires a proven HTTP/2 backend and backpressure; not needed for first Vite target |

### Security, compression, and native data

| Module | Status / phase | Initial contract | Prerequisites and notes |
| --- | --- | --- | --- |
| `node:crypto` | Planned / N2-N4 | secure random and UUID first, then hashes/HMAC, key APIs, ciphers, signatures, X.509 and WebCrypto identity | Never use insecure fallbacks; algorithms follow reference Node/OpenSSL policy |
| `node:zlib` | Planned / N4 | sync/callback/stream compression and Brotli where backend supports parity | Buffer and streams; output limits protect against decompression bombs |
| `node:sqlite` | Deferred / N6 | DatabaseSync and Node 24 SQLite surface, sessions/backups only with backend parity | Database/file permissions, deterministic bundled SQLite version, extension loading off by default |

### Inspector, tests, and runtime internals

| Module | Status / phase | Initial contract | Prerequisites and notes |
| --- | --- | --- | --- |
| `node:inspector` | Deferred / N5-N6 | Session and inspector URL/open/close/wait APIs | Requires Blitz debugger protocol and explicit inspector permission |
| `node:inspector/promises` | Deferred / N5-N6 | promise Session API | Same debugger transport and lifecycle |
| `node:test` | Deferred / N5 | test/subtest lifecycle, hooks, assertions integration, mocks/timers and snapshots in coherent slices | Build on runtime scheduler and reporters; do not mimic only the happy path |
| `node:test/reporters` | Deferred / N5 | dot/spec/tap/junit/lcov reporters exposed by reference Node | Depends on complete test event protocol and streams |
| `node:v8` | Deferred / N6 | only APIs with honest Blitz equivalents; heap/code statistics and serialization require separate contracts | Never fabricate V8 heap/version data; unsupported V8-specific operations remain unavailable |
| `node:wasi` | Platform-gated / N6 | WASI constructor/start/initialize where a selected WASM engine supports Node's contract | Requires a WASM engine and capability-safe preopens; unavailable otherwise |
| `node:sea` | Deferred / N6 | single-executable application metadata/assets when Blitz gains an equivalent packaging format | Must not claim Node SEA state for ordinary Blitz binaries |

### Legacy compatibility

| Module | Status / phase | Initial contract | Prerequisites and notes |
| --- | --- | --- | --- |
| `node:domain` | Compatibility-only / N6 | Node 24 deprecated domain behavior sufficient for pinned legacy fixtures | Depends on events, async context and process error routing; excluded from default profiles |

## Module dependency order

```text
registry + declarations + import.meta.url
  ├─ path ── url ── fs ── module metadata
  ├─ assert
  ├─ process ── timers ── perf_hooks
  ├─ typed arrays ── buffer ── string_decoder
  │                         ├─ crypto
  │                         └─ zlib
  ├─ events ── streams ── fs streams/readline
  │                    └─ HTTP bodies
  ├─ async context ── diagnostics_channel/async_hooks
  └─ runtime resources
       ├─ DNS ── net ── TLS ── HTTP/HTTPS/HTTP2
       ├─ UDP
       ├─ subprocess/cluster
       ├─ workers
       └─ inspector
```

## Vite milestone

The `tooling` profile targets one pinned Vite release. Its acceptance path is:

1. Import Vite and load its dependency graph.
2. Resolve all required `node:*` modules without package-specific aliases.
3. Read configuration and source files.
4. Start an HTTP server on an allowed address.
5. Serve transformed assets through streaming responses.
6. Watch files and invalidate the module graph.
7. deliver WebSocket HMR.
8. Handle terminal/color detection.
9. Shut down watchers, sockets, timers and workers without leaked referenced
   resources.

Likely required modules are `node:path`, `node:url`, `node:fs`,
`node:fs/promises`, `node:events`, `node:buffer`, `node:stream`, `node:util`,
`node:os`, `node:tty`, `node:crypto`, `node:zlib`, `node:dns`, `node:net`,
`node:http`, `node:https`, `node:module`, `node:perf_hooks`, and
`node:process`. The pinned dependency graph, not this estimate, decides the
final list.

## Per-module work package

Every module implementation uses the same ordered work package:

1. Capture Node 24 runtime export keys, descriptors, default-export shape and
   documented platform differences.
2. Add differential fixtures for normal values, coercion, errors and identity.
3. Declare engine/runtime prerequisites and refuse to implement around missing
   semantics.
4. Add public facade, private adapter when needed, declaration asset, manifest
   entry and compatibility profile assignment.
5. Regenerate and verify deterministic prelinked assets.
6. Add static import, dynamic import, strict typecheck and editor-navigation
   tests.
7. Add permission, cancellation, ref/unref, cleanup and cross-platform tests
   for resource modules.
8. Add at least one pinned real-package fixture for non-foundational modules.
9. Mark support in the machine-readable matrix only after runtime/declaration
   parity passes.

## Completion order

Recommended pull-request sequence:

1. N0 registry propagation and `import.meta.url`.
2. `path`, `path/posix`, `path/win32`.
3. `assert`, `assert/strict`, `querystring`, `url`.
4. Complete `process`; add `console`, `os`, `util`, `util/types`.
5. Events and Node scheduler ordering; timers and diagnostics channel.
6. Typed arrays and Buffer; string decoder.
7. Streams and Web Streams interop.
8. Filesystem sync, promises, streams and watchers.
9. TTY and readline.
10. Crypto random/hash slice and zlib.
11. DNS, net and TLS.
12. HTTP and HTTPS; run the Vite milestone.
13. Module/CommonJS, subprocesses, workers and VM contexts.
14. Test runner and inspector.
15. HTTP/2, SQLite, WASI, SEA, V8-specific and deprecated compatibility.

## Inventory maintenance

- Regenerate the reference inventory from `module.builtinModules` whenever the
  pinned Node version changes.
- Fail CI when the generated inventory and this document/support matrix differ.
- Record added, removed, deprecated and experimental modules during upgrades.
- Experimental modules (`node:sqlite`, `node:sea`, or future additions) require
  an explicit stability and versioning decision before entering a default
  profile.
