# First-party Effect API migration

## Problem

Blitz vendors `effect@3.22.1` and publishes its own `blitz:fs/FileSystem` tag. The Rust host behind that tag has four synchronous whole-file operations. A separate Node compatibility host has a wider but still incomplete set of filesystem calls. Moving to Effect 4's first-party `FileSystem.FileSystem` therefore crosses three contracts at once: the vendored Effect graph, Blitz's runtime host ABI, and existing `blitz:*` application imports. The provider must satisfy Effect's actual interface and `PlatformError` model. A partial service presented as complete would move missing behavior into defects and violate the engine/runtime boundary. This design keeps old applications running while making the first-party Effect service the only new application contract.

## Usage (caller's view)

New code imports the service from `effect`. Blitz only appears at the process boundary.

```ts
import { Effect, FileSystem } from "effect"
import * as BlitzRuntime from "blitz:runtime"

const program = Effect.gen(function*() {
  const fs = yield* FileSystem.FileSystem
  return yield* fs.readFileString("./config.json")
})

BlitzRuntime.runMain(BlitzRuntime.provideLive(program))
```

Library code remains portable and has no Blitz import.

```ts
import { Effect, FileSystem } from "effect"

export const saveSnapshot = Effect.fn("saveSnapshot")(function*(bytes: Uint8Array) {
  const fs = yield* FileSystem.FileSystem
  yield* fs.makeDirectory(".cache/blitz", { recursive: true })
  yield* fs.writeFile(".cache/blitz/snapshot.bin", bytes)
})
```

Tests provide the upstream tag using upstream test helpers.

```ts
import { Effect, FileSystem, Layer } from "effect"

const TestFileSystem = Layer.succeed(
  FileSystem.FileSystem,
  FileSystem.makeNoop({
    readFileString: () => Effect.succeed("{\"mode\":\"test\"}")
  })
)

const result = program.pipe(Effect.provide(TestFileSystem))
```

Existing applications continue to work during one compatibility window.

```ts
import { readTextFile } from "blitz:fs"
import { provideLive, runMain } from "blitz:runtime"

runMain(provideLive(readTextFile("./config.json")))
```

`blitz:fs` is deprecated in its declaration docs. It is not required by `BlitzRuntime.layerLive` as a separate live implementation. Its accessors obtain the first-party service and translate the first-party error into the legacy error shape.

## Shape

### Public types and signatures

Blitz does not declare another filesystem interface.

```ts
// blitz:runtime, declaration sketch
import type { Cause, Effect, FileSystem, Layer } from "effect"

export type RuntimeServices =
  | FileSystem.FileSystem
  | HttpServer // transitional Blitz service
  | Terminal  // transitional Blitz service
  | WebSocket // transitional Blitz service

export declare const layerLive: Layer.Layer<RuntimeServices, RuntimeInitError>

export declare const provideLive: <A, E, R>(
  program: Effect.Effect<A, E, R>
) => Effect.Effect<A, E | RuntimeInitError, Exclude<R, RuntimeServices>>

export declare const runMain: <A, E>(
  program: Effect.Effect<A, E>,
  options?: {
    readonly disableSignalHandlers?: boolean
    readonly reportFailure?: (cause: Cause.Cause<unknown>) => void
  }
) => void
```

The source implementation is small. The upstream constructor derives string reads and writes, existence checks, streams, and sinks from lower-level operations.

```ts
// private module, blitz-internal:effect-file-system
import { FileSystem, Layer } from "effect"
import * as Host from "blitz-internal:file-system-v2"

const service: FileSystem.FileSystem = FileSystem.make({
  access: Host.access,
  chmod: Host.chmod,
  chown: Host.chown,
  copy: Host.copy,
  copyFile: Host.copyFile,
  glob: Host.glob,
  link: Host.link,
  makeDirectory: Host.makeDirectory,
  makeTempDirectory: Host.makeTempDirectory,
  makeTempDirectoryScoped: Host.makeTempDirectoryScoped,
  makeTempFile: Host.makeTempFile,
  makeTempFileScoped: Host.makeTempFileScoped,
  open: Host.open,
  readDirectory: Host.readDirectory,
  readFile: Host.readFile,
  readLink: Host.readLink,
  realPath: Host.realPath,
  remove: Host.remove,
  rename: Host.rename,
  stat: Host.stat,
  symlink: Host.symlink,
  truncate: Host.truncate,
  utimes: Host.utimes,
  watch: Host.watch,
  writeFile: Host.writeFile
})

export const layer = Layer.succeed(FileSystem.FileSystem, service)
```

This is a sketch, not a promise that the current beta keeps every spelling. Implementation pins one exact Effect 4 release and derives this list from that release's `FileSystem.make` parameter type. The checked upstream source currently makes `FileSystem` a `Context.Service("effect/platform/FileSystem")` and derives `exists`, `readFileString`, `writeFileString`, `stream`, and `sink`.

The legacy facade adapts instead of installing a second host service.

```ts
// public compatibility module, blitz:fs
import { Effect, FileSystem, Layer, PlatformError } from "effect"

/** @deprecated Use FileSystem.FileSystem from effect. */
export const FileSystemLegacy = /* existing tag identity */

const toLegacyError = (error: PlatformError.PlatformError): FileSystemError => {
  // Pure exhaustive normalization. No host calls.
  throw new Error("not implemented")
}

const legacyService = Effect.gen(function*() {
  const fs = yield* FileSystem.FileSystem
  return {
    readFile: (path) => fs.readFile(normalizeLegacyPath(path)).pipe(Effect.mapError(toLegacyError)),
    readTextFile: (path) => fs.readFileString(normalizeLegacyPath(path)).pipe(Effect.mapError(toLegacyError)),
    writeFile: (path, bytes) => fs.writeFile(normalizeLegacyPath(path), bytes).pipe(Effect.mapError(toLegacyError)),
    writeTextFile: (path, text) => fs.writeFileString(normalizeLegacyPath(path), text).pipe(Effect.mapError(toLegacyError))
  }
})

export const layerLegacy = Layer.effect(FileSystemLegacy, legacyService)
```

`normalizeLegacyPath` is the sole `string | URL` compatibility boundary. The first-party API takes strings. A `URL` must use `file:` and converts before the provider sees it. This preserves the old contract without widening Effect's interface.

### Private host contract

The v2 host bridge reports inert data. It does not construct Effect values in Rust and does not expose Rust or Node error representations.

```ts
type HostResult<A> =
  | { readonly _tag: "Success"; readonly value: A }
  | { readonly _tag: "Failure"; readonly error: HostFileSystemError }

type HostFileSystemError = {
  readonly kind:
    | "AlreadyExists"
    | "BadResource"
    | "Busy"
    | "InvalidData"
    | "InvalidInput"
    | "NotFound"
    | "PermissionDenied"
    | "TimedOut"
    | "UnexpectedEof"
    | "Unknown"
    | "WouldBlock"
    | "WriteZero"
  readonly operation: string
  readonly pathOrDescriptor?: string | number
  readonly description?: string
}
```

The private JavaScript adapter validates this result once, maps `InvalidInput` to `PlatformError.badArgument`, maps the remaining kinds to `PlatformError.systemError`, and then returns `Effect`, `Stream`, or scoped handle values. Rust owns syscalls and resource handles. JavaScript owns Effect types, interruption hooks, scopes, and `PlatformError`. Per boundary-discipline, malformed host results become defects at this private ABI boundary; ordinary OS failures stay typed failures.

Open files use opaque numeric IDs allocated and stored by the Blitz runtime. The JavaScript `FileSystem.File` closes its ID with `Effect.acquireRelease`, and close is idempotent. Reads, writes, seeks, and stats send only the ID and operation data back to the host. Watch subscriptions follow the existing websocket and terminal subscription pattern: Rust stores the subscription, JavaScript exposes a `Stream`, cancellation removes the stored subscription once.

The Rust module structure is knowledge-based rather than call-order-based:

```text
crates/blitz-runtime/src/host/file_system/
  mod.rs                 shared filesystem kernel and error normalization
  handles.rs             runtime-owned file handle table
  watches.rs             runtime-owned watch subscriptions
  effect_bridge.rs       file-system-v2 object installed for Effect
  node_bridge.rs         node:fs argument/result compatibility
```

`effect_bridge.rs` and `node_bridge.rs` call the shared filesystem kernel. Neither bridge calls the other. This avoids making Node behavior part of Effect semantics and removes today's duplicated filesystem knowledge.

### Module map and ownership

```text
Application
  imports effect/FileSystem
       |
blitz:runtime
  provides Effect's FileSystem.FileSystem tag
       |
blitz-internal:effect-file-system
  owns Effect constructors, PlatformError mapping, Scope and Stream adaptation
       |
blitz-internal:file-system-v2
  owns ABI validation and host invocation
       |
Rust filesystem kernel
  owns OS calls, bounded resources, handles, watches and permissions

blitz:fs
  temporary compatibility adapter over Effect's tag
```

The public interface is deep because application code learns no Blitz filesystem API at all. `blitz:runtime` hides provider construction, host ABI checks, error translation, resource cleanup, and layer composition behind `runMain` and `layerLive`. The legacy module contains policy rather than pass-throughs: URL normalization and error compatibility justify its temporary existence. It must not grow new methods.

### Migration sequence

1. Pin one exact Effect 4 version. Regenerate the full package graph and declarations needed by `FileSystem`, `PlatformError`, `Sink`, `Stream`, and their transitive imports. Run the current Effect foundation suite before touching platform services. Do not combine a floating beta upgrade with host changes.
2. Add the v2 private host ABI and shared Rust filesystem kernel beside ABI v1. Keep `blitz:fs` on v1 until the v2 provider passes its contract tests.
3. Implement and test every operation required by that pinned release's `FileSystem.make`. Do not use `makeNoop` for the production layer. A method that dies with "not implemented" blocks publication of the provider.
4. Switch `blitz:runtime` to provide the upstream `FileSystem.FileSystem` tag. Merge `layerLegacy` into the default runtime layer for exactly one compatibility release so existing closed-program and `provideLive` flows retain the old service requirement.
5. Rewrite examples and docs to import `FileSystem` from `effect`. Add a compile-negative fixture proving that new docs do not require `blitz:fs`.
6. Remove the public `blitz:fs` manifest entry and ABI v1 after the compatibility window. Keep the private provider module because it is Blitz's platform implementation, not an application API.

Each step leaves a runnable runtime. The cutover happens only after the new provider is complete, so there is no interval where `FileSystem.FileSystem` exists but lies about its capabilities.

## Synthesis decision

The compatibility-first design is the base. The v4-first alternative
contributed the concrete host ABI, VM-owned handle and subscription tables,
generated Effect-version authority, pure-v4 execution gate, and method-category
contract tests. The public runtime cutover remains atomic even though ABI v2 is
built beside ABI v1. After that cutover, legacy `blitz:fs` runs on Effect 4 and
the first-party provider. It does not retain a separate v1 implementation.

Immediate removal of `blitz:fs` was rejected because a renamed compatibility
module and changed error type would still force all callers to migrate at the
Effect major-version boundary. The legacy layer is merged into the default
runtime layer for one compatibility release, then removed in the next breaking
release.

## Tradeoffs accepted

- We accept one compatibility release with two service tags in exchange for preserving existing `blitz:fs` programs.
- We accept a larger first filesystem milestone in exchange for truthfully implementing the first-party service instead of publishing a production `makeNoop` provider.
- We accept a private ABI v2 beside v1 in exchange for separating raw host results from Effect's JavaScript error and resource types.
- We accept pinning a pre-stable Effect 4 build in exchange for targeting the first-party APIs now. Every pin update must rerun provider contract tests because the interface may change.
- We accept string-only paths in new code in exchange for matching Effect's public contract. The deprecated facade alone retains `URL` input.

## Alternatives considered

- Keep `blitz:fs` and re-export Effect names from it. This leaves portable libraries dependent on Blitz and duplicates upstream service ownership. The interface remains shallow because callers still need to know which filesystem tag a dependency expects.
- Replace `blitz:fs` in place and break old imports. This has fewer transition files but exposes version coordination to every caller. It also prevents isolating the Effect 4 upgrade from filesystem conformance work.
- Implement only four methods with `FileSystem.makeNoop`. This is easy to ship, but missing methods fail as defects and the advertised service is false. Test-only defaults do not define a production capability model.
- Build the Effect provider on `node:fs`. That shortens implementation but leaks Node flags, errors, and callback behavior into the portable provider. Both adapters should share the Rust filesystem kernel instead.

## Open questions and risks

- Which exact Effect 4 prerelease will Blitz pin, and is the project willing to update that pin only through explicit compatibility reviews?
- Does "compatibility window" mean one tagged Blitz release, or should removal wait for usage evidence from downstream programs?
- Must the first provider support Windows at launch? `chmod`, `chown`, links, recursive watches, and file flags need explicit cross-platform semantics in the Rust kernel.
- Should the existing 16 MiB whole-file limit remain? If so, how will Blitz document and encode it when Effect's portable API does not advertise that limit?
- Can Denko execute the full Effect 4 dependency graph and the `FileSystem.make` derived stream/sink code before filesystem work begins? The upgrade gate must answer this with execution, not typechecking alone.
- Does the runtime currently have enough asynchronous host suspension and interruption support for file handles and watches, or must those event-loop capabilities land first?
- Should `RuntimeInitError` remain on `layerLive` if filesystem layer construction is pure, or do the other transitional services still require it?

## Design red-flag screen

- Shallow module: passed. New applications get the upstream interface, while the Blitz provider hides ABI, OS, error, and lifetime policy.
- Information leakage: passed. Only the private adapter knows both host errors and `PlatformError`; only the compatibility facade knows both service tags.
- Temporal decomposition: passed. Rust modules group filesystem rules, handles, and watches by owned knowledge rather than acquire, operate, and release stages.
- Pass-through method: passed with one deliberate exception. The compatibility methods add URL and error adaptation and have a removal date. The Effect bridge adds error, scope, stream, and cancellation semantics rather than forwarding unchanged arguments.

## Next implementation step

Create an isolated Effect 4 asset-generation spike that pins one exact release, vendors the upstream `FileSystem` declaration graph, and executes a `FileSystem.makeNoop` program in Denko without changing the default runtime.
