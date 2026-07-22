# Blitz Runtime

`blitz-runtime` provides host capabilities and ergonomic behavior around the spec-compliant engine.

- Host globals such as `console`, `process`, and `Blitz` may prioritize a useful developer and end-user experience.
- Keep runtime additions outside raw-engine globals and conformance environments unless a harness explicitly requires them.
- Host presentation may inspect objects more usefully than ECMAScript string coercion. Explicit coercion paths such as `%s` must continue to use engine semantics.
- Inspection and diagnostics must not unexpectedly execute user code. Do not invoke getters, proxy traps, custom coercion hooks, or other callbacks merely to display a value.
- Do not use runtime shims to conceal parser, compiler, bytecode, completion-value, or VM bugs. Repair those in `blitz-engine`.
- Preserve the dependency boundary: runtime builds on engine behavior rather than redefining it.
