# Blitz

This project is a combination Stack/Register based JavaScript VM. It is preferred to keep things operated upon. Always store state inside the VM, and operate upon it rather than mutating it directly.

## Engine, Runtime, and CLI Boundaries

- `blitz-engine` must implement ECMAScript behavior faithfully. Do not relax or replace language semantics to improve usability.
- `blitz-runtime` may provide ergonomic host APIs and developer-facing behavior, provided those additions do not alter engine semantics or leak into raw-engine and conformance environments.
- `blitz-cli` owns command-line and REPL presentation. Presentation may be more helpful than ECMAScript coercion as long as it does not change the evaluated value or observable language behavior.
- Keep spec operations such as coercion, property access, proxy behavior, and completion values distinct from host-side inspection and formatting.
- Fix engine correctness issues at their source. Do not hide parser, compiler, or VM bugs behind runtime or CLI workarounds.
