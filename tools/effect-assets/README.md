# Deterministic Effect assets

This workspace pins `effect@3.16.7` and vendors the tree-shaken ESM graph used
by Blitz's curated Effect surface. Rollup preserves upstream module boundaries,
esbuild removes comments and whitespace, and public entries remain small
aliases. Internal imports are rewritten to `blitz-internal:effect/...`, so the
VM caches one module identity across root and deep imports.

Generated files are committed at
`crates/blitz-runtime/assets/effect/3.16.7/` and
`crates/blitz-runtime/assets/stdlib/`. Generation never runs at Rust build or
runtime and does not access the network. The full upstream ESM graph is
retained for runtime identity, while declarations are curated and emitted
only for the supported public specifiers; private internal modules do
not carry duplicate upstream declaration graphs.

The stdlib manifest is data-driven from `src/stdlib-manifest.ts`. It records
the implemented `blitz:match`, `blitz:fs`, `blitz:http`, `blitz:websocket`,
`blitz:terminal`, `blitz:runtime`, transitional `blitz:app`, and private host
adapter modules, including ordered dependencies, visibility, source and
declaration hashes, pinned Effect/runtime/host ABIs, and host primitive
requirements. It is input metadata for the committed prelinked runtime graph;
the Rust `effect-bytecode` workspace tool owns that final binary.

## Commands

From this directory:

```sh
pnpm install --frozen-lockfile
pnpm generate
pnpm verify
pnpm typecheck
pnpm test
```

`verify` checks both manifests, SHA-256 hashes, canonical import closure,
visibility and ABI metadata, pinned package integrity, and byte-for-byte
deterministic regeneration.

The generated graph includes Effect's static runtime dependencies required by
the root export (currently `fast-check` and its static dependencies). Optional
or host-specific package imports are rejected rather than silently retained.

JavaScript facades become runtime assets only when listed in
`src/stdlib-manifest.ts`; declarations alone do not make a module public.

### Initial `node:async_hooks` facade

`src/facades/node-async-hooks.js` and `declarations/node-async-hooks.d.ts`
contain a registered, profile-gated Node 24 slice: `AsyncLocalStorage`,
`AsyncResource`, `executionAsyncId`, `triggerAsyncId`,
`executionAsyncResource`, and `createHook`. Synchronous scopes plus explicit
`bind`, `snapshot`, and `AsyncResource` scopes are supported. The VM currently
has no truthful async-resource lifecycle callbacks, so promise continuations
are not implicitly propagated and `createHook()` always throws
`ERR_NOT_IMPLEMENTED`; no lifecycle events or GC destruction are claimed.
The direct runtime coverage is in `scripts/test-async-hooks.ts`.

### Initial `node:crypto` secure-random facade

`src/facades/node-crypto.js` and `declarations/node-crypto.d.ts` define the
initial Node 24 secure-random slice: RFC 4122 v4 `randomUUID`, secure
`randomBytes`, `randomFill`/`randomFillSync`, unbiased secure `randomInt`, and
constant-time `timingSafeEqual`. Byte allocations are bounded at 16 MiB by the
host boundary. Entropy comes only from the Rust host's operating-system source;
there is no insecure fallback. Callback forms resume
on a VM Promise-job boundary and do not retain runtime resources.

Blitz does not yet provide faithful Buffer/ArrayBuffer byte-view aliasing, so
this slice explicitly returns and accepts standalone `Uint8Array` values only.
Buffer, ArrayBuffer-backed views, `webcrypto`, `subtle`, and
`getRandomValues` are intentionally not exposed. The private
`blitz-internal:node-crypto-random` adapter is version-one host plumbing and is
permission-gated by the selected Node compatibility profile. The public root is registered in the `tooling` and `full` profiles.

### Initial `node:string_decoder` facade

`src/facades/node-string-decoder.js` and `declarations/node-string-decoder.d.ts`
provide the registered Node 24 `StringDecoder` export. UTF-8 decoding is
incremental across split `Uint8Array` and number-array chunks, with practical
`lastNeed`, `lastTotal`, and `lastChar` state. `latin1`, `ascii`, and `utf16le`
(and their Node aliases) are implemented in pure JavaScript; other encodings
throw `ERR_UNKNOWN_ENCODING` rather than being approximated. Blitz has no
faithful `Buffer` or `TextDecoder`, so this facade accepts byte chunks as
`Uint8Array` or readonly number arrays and does not claim Buffer compatibility.
It is registered in the `core`, `tooling`, and `full` profiles. Differential
Node/Blitz coverage is in `scripts/test-string-decoder.ts`.

### `node:domain` and `node:sea` compatibility roots

`src/facades/node-domain.js` and `declarations/node-domain.d.ts` provide the
legacy `Domain`/`create`/`createDomain` surface on top of `node:events`,
including explicit `add`/`remove`, synchronous `run`/`bind`/`intercept` scopes,
legacy `dispose`, and `'error'` routing for explicitly added emitters. The
facade does not claim Node's async-context propagation, timer binding, or
process-level uncaught-exception integration.

`src/facades/node-sea.js` and `declarations/node-sea.d.ts` model an ordinary
(non-SEA) Node 24 process: `isSea()` is false, asset accessors and
`getAssetKeys()` raise `ERR_NOT_IN_SINGLE_EXECUTABLE_APPLICATION` after Node's
argument checks, and the default/named export shape does not fabricate
`startupSnapshot` (that API belongs to `node:v8`). Both roots are registered in the explicit `full` profile. The Blitz execution
smoke coverage is in `scripts/test-node-roots.ts`.

After changing either manifest, regenerate and verify the final artifact:

```sh
cargo run -p effect-bytecode -- generate
cargo run -p effect-bytecode -- generate --check
```
