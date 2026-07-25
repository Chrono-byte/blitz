# Blitz language server

This Bun workspace is the thin Language Server Protocol adapter for Blitz. It
does not implement a second parser or checker: it keeps one
`blitz analyze --server` process alive and exchanges newline-delimited,
version-cached analysis requests with it. The compiler retains open-document
overlays and dependency/reverse-dependency edges, invalidating affected
analyses when an imported document changes or closes. Runtime and Effect
declaration modules are loaded into the project so explicitly typed interface
members, such as `WebSocket.Connection.send`, support hover and definition
navigation. Effect-returning calls also retain enough declaration provenance
for `connect(...).pipe` and `send(...).pipe` navigation. The project discovers
filesystem package declarations through package `types`, `typings`, and
`exports` metadata as well as relative declaration files.

Contextual object-literal typing instantiates generics, ranks compatible
overloads, and flows selected handler signatures into unannotated callback
parameters. Compiler-owned inlay hints cover callback parameters, safe local
and return types, and selected call-argument parameter names. Function-valued
object properties are included in the nested symbol tree for editor
breadcrumbs. The compiler response drives editor diagnostics, nested symbols,
completions, hover, inlay hints, and same-file or cross-file go-to-definition. ECMAScript globals such as `Map` resolve to compiler-owned
virtual declaration documents, which the adapter materializes as read-only
files for standard editor navigation. The compiler supplies an AST-derived
navigation tree, module-export targets, and OXC semantic references; the LSP
adapter only manages documents and converts byte spans into protocol ranges.

## Development

```sh
bun install
bun run check
bun run lint
bun test
```

Set `BLITZ_COMPILER` to select a particular `blitz` executable. During local
repository development the server finds `target/debug/blitz` or falls back to
`cargo run -p blitz-cli`. To install the `blitz-language-server` executable on
your path, run `bun run build && bun link` in this directory.
