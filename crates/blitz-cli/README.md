# Blitz CLI

`blitz-cli` owns the user-facing executable and currently routes the `run`,
`check`, and `analyze` commands. Package management, dependency resolution,
bundling, and release workflows are future toolchain work, not implemented
commands in this crate today.

Runtime capabilities belong in `blitz-runtime`; ECMAScript semantics belong in
`blitz-engine`.
