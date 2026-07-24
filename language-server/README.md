# Blitz language server

This Bun workspace is the thin Language Server Protocol adapter for Blitz. It
does not implement a second parser or checker: open documents are sent to
`blitz analyze --path <file> -`, and the compiler's JSON response drives
editor diagnostics, symbols, completions, hover, and go-to-definition.

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
