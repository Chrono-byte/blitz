# Blitz CLI

`blitz-cli` owns the user-facing executable and the project-level toolchain.
The crate is the home for command routing, package and workspace management,
dependency resolution, lockfiles, task execution, formatting, linting,
testing, bundling, compilation, and release/update workflows.

Runtime capabilities belong in `blitz-runtime`; ECMAScript semantics belong in
`blitz-engine`.
