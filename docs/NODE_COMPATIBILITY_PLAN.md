# Scoped Node compatibility plan

The exhaustive Node 24 built-in inventory and per-module delivery order live in
[`NODE_MODULE_IMPLEMENTATION_PLAN.md`](NODE_MODULE_IMPLEMENTATION_PLAN.md).

## Goal

Allow ESM applications and selected npm packages to run under Blitz when they
import an explicitly supported `node:*` module, without turning
`denko-engine` into a Node host or claiming complete Node.js compatibility.

Bare-package and `node_modules` resolution are already implemented. This plan
uses that resolver and adds a runtime-owned compatibility layer, declarations,
capability policy, and compatibility tests.

A useful first target is an ESM-only package graph that depends on the core
utility modules. Vite is a later integration target because starting its dev
server also requires filesystem watching, streams, HTTP, sockets, crypto, and
parts of the process and module APIs.

## Implementation status

The compatibility graph now registers 36 of Node 24.18.0's 58 public built-in
roots (62.07% root coverage) with explicit declarations and tested initial
slices. This is root-level coverage, not a claim of complete export-level API
parity. The machine-readable inventory is
[`node-compat-support.json`](node-compat-support.json).

- runtime resolution selects the `node` package-export condition before
  `import` and `default`;
- `RuntimeOptions.node_compat` and `--node-compat` support `off`, `core`,
  `tooling`, and `full` profiles with explicit allowlists;
- dynamic imports of registered roots preserve the selected registry/profile;
  filesystem-relative dynamic-import resolution and same-realm evaluation remain
  open engine work;
- canonical module records provide top-level `import.meta.url`; preserving the
  owning module context inside later callbacks remains open;
- pure/process-local modules, an initial byte-backed `Buffer` slice, streams,
  readline, direct-host filesystem, and a secure crypto-random slice are
  prelinked and declaration-backed; and
- runtime tests cover profile rejection, package conditions, strict
  typechecking, identity, filesystem operations, event/stream/readline
  lifecycle, and permission-gated secure randomness.

Vite is not yet supported: HTTP, sockets, watchers, compression, and remaining
module-loader behavior still block that milestone.

## Current baseline

The package resolver should be treated as landed rather than reimplemented:

- `crates/denko-engine/src/resolver.rs` uses `oxc_resolver` for runtime and
  declaration resolution. It supports nearest `node_modules`, package
  `exports`, TypeScript extensions, nearest/inherited `tsconfig.json`, and
  `paths` mappings.
- `crates/denko-engine/src/module.rs` checks registered virtual and prelinked
  modules before filesystem resolution, then delegates filesystem and bare
  specifiers to `resolve_specifier`.
- `crates/blitz-runtime/src/modules.rs` is the hosted entry boundary and has
  executable coverage for a TypeScript package, a transitive package, a
  missing package, and inherited tsconfig paths.
- `crates/denko-engine/src/analysis.rs` uses the same resolver for source and
  declaration navigation.
- `VirtualModuleRegistry` and prelinked public/private roots already provide
  canonical module identities, aliases, declarations, and private dependency
  visibility.
- `blitz-runtime` already owns `process`, console, timers, URL, filesystem,
  terminal, HTTP, WebSocket, and the runtime event/resource boundary. These
  implementations are useful primitives, but their existing Blitz and Web API
  contracts must not be changed merely to imitate Node.

Before Node work begins, add resolver characterization tests for scoped npm
packages, package subpaths, conditional `exports`, package `imports` (`#x`),
symlinked packages, and nested dependency versions. Add `"node"` before
`"import"` and `"default"` to runtime condition names if characterization
shows the current resolver does not select Node branches. This is validation
and small hardening, not a second resolver project.

Package resolution does **not** imply CommonJS execution. `require`, CJS
wrapping, `module.exports`, and ESM/CJS interop are a separate project and are
not hidden inside this plan. Likewise, `.cjs` being a resolver candidate does
not make it execute with CommonJS semantics.

The registry boundary now propagates the selected loader/profile through
dynamic import and implements generic module-record-backed `import.meta.url`.
Remaining metadata work includes callback/frame ownership, the selected
symlink policy, and the optional Node `import.meta.dirname`/`filename` fields.

## Architectural decisions

1. **Keep the engine host-neutral.** `denko-engine` continues to own generic
   module records, virtual/prelinked graphs, and package resolution. It must
   not contain filesystem, process, socket, or Node-specific implementations.
2. **Put compatibility in `blitz-runtime`.** Public `node:*` facades and their
   host primitives live beside the existing runtime host boundary.
3. **Use canonical `node:*` identities.** Initially support only prefixed
   specifiers such as `node:path`. Do not make `path` shadow an installed npm
   package. Unprefixed builtin aliases require a separate, explicit collision
   policy.
4. **Expose an allowlist, not a Node-shaped catch-all.** Unknown or disabled
   `node:*` imports fail during graph construction with a diagnostic that names
   the module and active compatibility profile.
5. **Separate module exposure from permissions.** Enabling `node:fs` makes its
   API resolvable; runtime read/write permissions still decide whether an
   operation is allowed.
6. **Preserve one module identity per realm.** Every importer receives the same
   namespace and stateful singleton where Node requires one. Aliases, if later
   enabled, point to the canonical `node:*` record.
7. **Prelink stable JavaScript facades.** Follow the existing stdlib graph
   model: public facades depend on private host-adapter modules, and source code
   cannot import those private adapters directly.
8. **Do not silently stub APIs.** An implemented export follows its documented
   contract. Unsupported exports are absent; intentionally partial operations
   throw a stable `ERR_NOT_IMPLEMENTED` carrying module and operation names.
9. **Pin a compatibility reference.** Select one Node LTS major for behavior,
   declarations, fixtures, and error codes. Record the version in the node
   compatibility manifest; do not expose Blitz's crate version as a Node
   version.

## Proposed runtime shape

Add a runtime-owned registry, separate from the Effect graph:

```text
NodeCompatConfig
  profile: off | core | tooling | full
  enabled_modules: explicit overrides
  permissions: filesystem / environment / network / subprocess

node-compat manifest
  public node:* roots
  private blitz-internal:node/* adapters
  declaration file per public root
  required host primitive + ABI methods
  reference Node major and compatibility status
```

`RuntimeOptions` receives `node_compat: NodeCompatConfig`. The CLI installs the
selected graph before constructing the application entry. Other embedders can
choose `off` or a narrower module set. The CLI may default to `core` only for
modules that meet the per-module definition of done; adding a module to that
profile is therefore a compatibility release decision.

The generated manifest and graph should have deterministic hashes and ABI
validation equivalent to the existing stdlib assets. A Node facade must never
reach into an ambient mutable Rust object by name. It calls a versioned private
host primitive installed for that realm.

## Initial module scope

Support is delivered by behavior slice, not by creating empty modules for this
whole table.

### Slice A: pure and process-local core

| Module | Initial supported surface |
| --- | --- |
| `node:path` | `basename`, `dirname`, `extname`, `format`, `isAbsolute`, `join`, `normalize`, `parse`, `relative`, `resolve`, `sep`, `delimiter`, `posix`, `win32` |
| `node:url` | `URL`, `URLSearchParams`, `fileURLToPath`, `pathToFileURL`, `URLToHttpOptions` where prerequisites exist |
| `node:process` | the existing process singleton plus `cwd`, `env`, `argv`, `execPath`, `platform`, `arch`, `versions`, exit-code state, and next-tick scheduling |
| `node:util` | `format`, `formatWithOptions`, `inspect`, `types` predicates that the VM can represent, `promisify`, and `callbackify` |
| `node:events` | `EventEmitter`, `once`, `on`, listener limits, error-event behavior |
| `node:assert` and `node:assert/strict` | assertion functions and stable assertion errors |
| `node:os` | immutable/process-local metadata and temp/home directory helpers |
| `node:querystring` | parse/stringify/escape/unescape compatibility |
| `node:timers` and `node:timers/promises` | facades over the runtime scheduler, including cancellation where `AbortSignal` exists |

`node:buffer` begins with an eight-bit `ArrayBuffer`/`Uint8Array`-backed slice
whose hidden engine slots preserve byte aliasing and authoritative transfer
state. A host byte-view API now lets secure crypto operate on Buffer and
nonzero-offset Uint8Array views without inspecting JavaScript properties.
Broader Buffer parity remains gated on wider typed arrays, `DataView`, complete
detached-view metadata, and multi-byte numeric access; ordinary-array emulation
is not accepted.

### Slice B: filesystem and diagnostics

| Module | Initial supported surface |
| --- | --- |
| `node:fs` | a documented subset of synchronous APIs first, with Node-shaped paths, results, and errors |
| `node:fs/promises` | asynchronous equivalents routed through runtime operations and the event loop |
| `node:perf_hooks` | `performance` identity and basic marks/measures after timing semantics are aligned |
| `node:tty` | terminal detection and metadata required by CLI libraries |
| `node:string_decoder` | after `Buffer` is available |
| `node:crypto` | `randomUUID`, secure random bytes, and selected hashes after byte primitives are sound |

Filesystem implementation should share lower-level host operations with
`blitz:fs`, not wrap its Effect service. Node errors need Node fields such as
`code`, `errno`, `syscall`, and `path`, while Blitz APIs retain their own typed
error contracts.

### Slice C: tooling/server prerequisites

Only add these against concrete package fixtures:

- `node:stream` and `node:stream/promises`;
- `node:http` and `node:https` client/server APIs;
- `node:net`, `node:tls`, and `node:dns`;
- `node:zlib`;
- `node:module`, including only the ESM-facing APIs needed by tooling;
- `node:fs` watchers with clear event coalescing and lifecycle rules;
- `node:worker_threads` and `node:child_process` last, behind explicit
  permissions.

This slice is the earliest point at which launching Vite under Blitz should be
an acceptance target. Importing Vite successfully is an earlier milestone than
starting and stopping its server cleanly.

## Implementation phases

### 0. Freeze the contract and characterize resolution

- Choose the reference Node LTS major.
- Add a machine-readable support matrix with module, export, status, host
  primitive, declaration, and test fixture.
- Add the resolver characterization tests listed above.
- Confirm virtual `node:*` roots win before filesystem resolution and that an
  unregistered `node:*` name never falls through to `node_modules`.
- Define diagnostics for disabled modules, unknown modules, denied operations,
  and unsupported exports.

**Exit:** package resolution remains unchanged except for proven condition-name
hardening, and the intended Node scope is executable data rather than prose.

### 1. Node compatibility graph and installation policy

- Add a dedicated generated node-compat manifest/graph under
  `crates/blitz-runtime/assets`.
- Add public/private root validation and deterministic hash checks.
- Add `NodeCompatConfig` and install selected roots from `install_runtime`
  before application graph construction.
- Propagate the immutable registry/profile into dynamic-import child loaders;
  do not reinstall unrelated mutable host state or create duplicate module
  identities in the same realm.
- Ensure private `blitz-internal:node/*` modules cannot be imported by source.
- Generalize typechecker declaration discovery to public registered roots
  rather than hard-coded `effect/*`/`blitz:runtime` prefix checks.

**Exit:** static and dynamic fixtures import a tiny `node:test-fixture` public
root, type-check it, evaluate it once per realm, retain the selected profile,
and cannot import its private adapter.

### 2. Pure modules first

- Implement generic module-record metadata sufficient for `import.meta.url`,
  then implement `node:path`, `node:url`, `node:querystring`, and `node:assert`
  mostly in prelinked JavaScript.
- Reuse VM intrinsics and existing URL behavior only where semantics match.
- Add default and named exports exactly as Node exposes them.
- Test POSIX and Windows path namespaces independently on every host OS.

**Exit:** pure module tests run without host I/O and match the selected Node
version's fixture output.

### 3. Process, scheduling, events, and utilities

- Make `node:process` export the exact same object as global `process`.
- Add next-tick ordering to the runtime turn model and test it against Promise
  jobs and timers.
- Implement `EventEmitter`, utility formatting/inspection, and timer facades.
- Keep callback roots in VM-owned state and release them on completion or
  cancellation.

**Exit:** identity, event ordering, listener errors, timer cancellation, and
  process exit-code tests pass without leaking roots or keeping the runtime
  alive incorrectly.

### 4. Bytes and filesystem

- Land faithful typed-array/`ArrayBuffer` prerequisites and `Buffer`.
- Define shared low-level filesystem operations in the runtime host layer.
- Implement Node-shaped sync and promise APIs with permission checks, coded
  errors, URL paths, and abort/cancellation behavior.
- Keep async completions on the runtime event boundary; worker tasks never
  mutate VM values.

**Exit:** temporary-directory fixtures cover text and binary reads/writes,
metadata, directory operations, error fields, cancellation, permissions, and
resource cleanup.

### 5. Package corpus and compatibility-driven expansion

- Maintain a small pinned corpus of ESM npm packages.
- For each failure, classify it as resolver, syntax/engine, Node module,
  CommonJS, or host-event-loop work before changing code.
- Add only the smallest coherent API slice needed by more than one fixture or
  by an explicit tooling target.
- Report compatibility by package/version and supported Node profile.

**Exit:** the core corpus runs with no package-specific transforms or aliases.

### 6. Vite tooling profile

- Track Vite's direct and transitive `node:*` imports for one pinned version.
- Implement the coherent stream, watcher, HTTP, socket, crypto, compression,
  and module slices it requires.
- Prove server startup, HTTP asset delivery, file invalidation, WebSocket HMR,
  graceful shutdown, and no leaked referenced resources.
- Keep subprocess and worker support disabled unless Vite actually requires
  them for the selected mode.

**Exit:** `blitz run <vite-entry>` starts the pinned Vite server, serves a React
fixture, observes one edit, performs HMR, and exits cleanly on shutdown.

### 7. Optional unprefixed builtin aliases and CommonJS

Treat both as separate compatibility decisions:

- Unprefixed aliases (`path`, `fs`) need Node's builtin-precedence behavior and
  a documented collision policy with npm packages.
- JSON modules and import attributes need an explicit loader contract rather
  than being inferred from filename resolution.
- CommonJS needs a real module record/wrapper, cache, cycle behavior,
  `require.resolve`, `module.exports`, and CJS/ESM interop. It must not be
  simulated by source rewriting in the resolver.

Neither blocks the initial `node:*` ESM scope.

## Testing strategy

1. **Manifest tests:** hashes, ABI methods, visibility, declarations, duplicate
   roots, and profile composition.
2. **Engine tests:** generic virtual/prelinked identity and visibility only;
   no Node behavior tests in `denko-engine`.
3. **Runtime differential fixtures:** execute the same fixture with the pinned
   Node binary and Blitz, serialize observable output, and compare.
4. **Error tests:** constructor/name/message plus stable fields and codes where
   packages branch on them.
5. **Event-loop tests:** next tick, Promise jobs, timers, I/O completions,
   cancellation, `ref`/`unref`, and shutdown ordering.
6. **Type tests:** named/default imports, subpath modules, unsupported exports,
   editor definitions, and declaration/runtime parity.
7. **Package tests:** pinned lockfile, no network during tests, no
   package-specific patches, and explicit failure classification.
8. **Cross-platform CI:** at minimum Linux, macOS, and Windows for paths,
   filesystem errors, process metadata, and watcher behavior.

## Definition of done for each `node:*` module

A module is marked supported only when:

- its canonical and optional subpath specifiers resolve automatically;
- runtime exports and declarations agree;
- default/named export shape and singleton identity match Node;
- implemented operations have differential success and failure coverage;
- asynchronous operations obey runtime ordering, cancellation, liveness, and
  cleanup contracts;
- permissions are enforced at the host-operation boundary;
- unsupported behavior is documented and never silently emulated; and
- at least one real package fixture exercises the module when it is not a
  purely foundational API.

## Non-goals

- Complete Node compatibility in the first release.
- Moving host behavior into `denko-engine`.
- Replacing the existing `oxc_resolver` package-resolution path.
- Treating `@types/node` as proof of runtime compatibility.
- Making `blitz:*` APIs adopt Node coercion, errors, or lifecycle semantics.
- Claiming Vite support after imports work but before watching, HMR, and clean
  shutdown are proven.
