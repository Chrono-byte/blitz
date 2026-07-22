# Blitz Engine

`blitz-engine` is the ECMAScript semantics boundary.

- Implement parser, compiler, bytecode, and VM behavior according to the ECMAScript specification. Treat Test262 failures as engine correctness work, not opportunities for compatibility shortcuts.
- Do not change `ToString`, property access, proxy traps, evaluation completion values, or other language-visible behavior for REPL or console convenience.
- Keep execution state inside the VM and operate on that state through VM mechanisms rather than mutating language state externally.
- Host-facing inspection of `Value` may expose a side-effect-free traversal primitive, but it must not invoke getters, proxy traps, coercion hooks, or other user code. Formatting policy belongs to runtime or CLI presentation.
- A binary or REPL in this crate is still a presentation shell. Its output may be ergonomic, but code evaluated by it must retain the same engine semantics.
- Fix semantic bugs in the parser, compiler, or VM layer that owns them; never compensate with presentation logic.
