# Blitz CLI

`blitz-cli` owns the user-facing `blitz` executable and routes the `run`,
`check`, and `analyze` commands. With no command or file, it launches the
runtime-owned Blitz REPL. Package management, dependency resolution, bundling,
and release workflows are future toolchain work, not implemented commands in
this crate today.

Runtime capabilities and the hosted REPL belong in `blitz-runtime`;
ECMAScript semantics belong in `denko-engine`.

The reviewed Bun-to-Blitz command and flag mapping is maintained in
[`docs/CLI_SURFACE.md`](../../docs/CLI_SURFACE.md).
