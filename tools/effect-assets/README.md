# Deterministic Effect assets

This workspace pins `effect@3.16.7` and vendors the tree-shaken ESM graph used
by Blitz's curated Effect surface. Rollup preserves upstream module boundaries,
esbuild removes comments and whitespace, and public entries remain small
aliases. Internal imports are rewritten to `blitz-internal:effect/...`, so the
VM caches one module identity across root and deep imports.

Generated files are committed at
`crates/blitz-runtime/assets/effect/3.16.7/`. Generation never runs at Rust
build or runtime and does not access the network. The full upstream ESM graph
is retained for runtime identity, while declarations are curated and emitted
only for the eight supported public specifiers; private internal modules do
not carry duplicate upstream declaration graphs.

## Commands

From this directory:

```sh
pnpm install --frozen-lockfile
pnpm generate
pnpm verify
pnpm typecheck
pnpm test
```

`verify` checks the manifest, SHA-256 hashes, canonical import closure, pinned
package integrity, and byte-for-byte deterministic regeneration.

The generated graph includes Effect's static runtime dependencies required by
the root export (currently `fast-check` and its static dependencies). Optional
or host-specific package imports are rejected rather than silently retained.

The `src/` and `declarations/` Blitz files are Phase 1 type boundaries for the
later application and RPC phases; they are not installed as runtime assets yet.
