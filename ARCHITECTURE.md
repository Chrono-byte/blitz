# Architecture

## Goals

- Move as much of the runtime surface into the TypeScript "soft" layer as practical to enable fast iteration, easier testing, and clearer, self-hosted semantics.
- Keep Rust as a minimal, stable kernel that implements bytecode execution, memory management (GC/allocator), and a small, well-documented host/native ABI.
- Make TypeScript (the soft layer) authoritative for user-visible APIs, builtins, and higher-level runtime behavior.

## High-level split: kernel vs soft layer

- **Kernel (Rust)** — the implementation of the actual runtime: lexer, parser, bytecode compiler, bytecode interpreter, VM, heaps/stacks, opcodes, and garbage collector/allocator. The kernel enforces low-level invariants (object identity, memory safety, deterministic execution model) and exposes a narrow host ABI. The kernel must defensively validate all inputs crossing the boundary.

- **Soft layer (TypeScript)** — everything above the kernel: builtin prototypes and methods (Array, Object, String, Function, etc.), `Error` classes and stack formatting, the `Promise` job queue, the module loader, REPL/CLI, polyfills, standard library modules (`console`, `fs`, `timers`, ...), and other runtime helpers. These are written in TypeScript, compiled to our bytecode, and executed by the VM. The soft layer is the canonical source of user-visible semantics.

## Kernel <-> Soft boundary principles

- Thin, stable kernel ABI: keep the native surface minimal and well documented. Candidate native hooks include `__native_print`, `__native_now`, file primitives, and timer primitives. Treat these as the stable contract between kernel and soft layer.
- Self-host first: prefer TS implementations for correctness and auditability; introduce native fast-paths only when profiling demonstrates clear, reproducible gains.
- Defensive kernel: never trust TS values. Validate types, ranges, and invariants at the kernel boundary.

## Host ABI (recommended minimum)

These are the primitives the kernel should provide for the soft layer. Each hook should be documented with argument/return semantics and failure modes.

- I/O & console: `__native_print(level, msg)`
- Time: `__native_now()` -> high-resolution timestamp
- Files: `__native_read(path)`, `__native_write(path, bytes)` (minimal set; higher-level FS lives in TS)
- Timers: `__native_set_timer(id, ms)`, `__native_clear_timer(id)`
- Process / exit: `__native_exit(code)`
- Host ticks / job pump: a mechanism for the kernel to drive the TS microtask/job queue (e.g., `tick()` or explicit callback registration)

Note: this list is intentionally small. Each function must define error propagation and ownership (who frees buffers, what GC roots are exposed, etc.). A separate ABI.md should enumerate signatures, error codes, and stability guarantees.

## Bootstrapping & startup order

Typical startup sequence:

1. Kernel initializes primitives and native hooks (Rust).
2. Kernel loads a minimal bootstrap bytecode/module that wires host hooks into the soft layer.
3. `soft/core/bootstrap` runs and configures the runtime scaffolding.
4. `soft/core/*` modules provide `errors`, `stack`, `job_queue`, and `module_loader` implementations in TS.
5. `soft/std/*` exposes higher-level conveniences (`console`, `fs`, `timers`, `array`, `string`, ...).
6. Application/user code executes on top of the soft layer.

The kernel should expose raw stack/call info to the soft layer so TS code can format readable stack traces and errors.

## Module loader & REPL

- Implement module resolution, evaluation, and caching in TypeScript to allow flexible behaviors (custom resolvers, loaders, mocking) and simpler testing.
- Implement the REPL and CLI in TS so the user-facing interface can iterate quickly without kernel changes.

## Error handling, stack traces & diagnostics

- Soft layer owns error shapes, stack formatting, and user-facing messages.
- Kernel provides raw frames and symbol information (file/line/pc) but should not construct user messages. TS code should map raw frames to readable stacks.
- Use snapshot tests for error messages and stack traces to lock expected behavior across changes.

## Performance strategy & native fast-paths

- TS implementations are the source of truth. Native-backed optimizations are allowed only when benchmarks show clear benefit and behavior remains identical.
- When adding native fast-paths, keep a reference TS implementation for correctness and tests.

## Testing & validation

- TS-level unit tests: every migrated feature must include unit tests that run on the VM (exercise semantics end-to-end).
- Rust integration tests: add them when verifying kernel invariants (GC, memory safety, ABI contracts).
- Snapshot tests: lock error messages, stacks, and formatted outputs.
- Performance tests: add benchmarks when introducing native fast-paths.

## Migration roadmap (recommended)

1. Define and stabilize the minimal kernel host ABI. Publish ABI.md with signatures and stability policy.

## Proposed repository layout

- `src/*.rs` — Rust kernel and native hooks (lexer, parser, compiler, VM, GC)
- `soft/core/` — runtime scaffolding written in TS (`bootstrap.ts`, `errors.ts`, `stack.ts`, `job_queue.ts`, `module_loader.ts`)
- `soft/std/` — standard library in TS (`console.ts`, `array.ts`, `string.ts`, `fs.ts`, `timers.ts`, ...)
- `soft/tests/` — TS-level tests that run on the VM (unit + integration + snapshot)
- `tools/` — build & test tooling, ABI generators, or helpers

Rename note: the `soft/` directory is conceptually the host library referred to as "blitzlib" in design docs. Do not rename the folder in this repo until the agent coordinating that change completes their work.

## Governance & PR checklist

Migration PRs should include:

- TS tests runnable on the VM (unit + snapshot where applicable).
- Any necessary Rust integration tests for kernel invariants.
- Benchmark/measurement notes if the change touches hot paths.
- Documentation updates (this ARCHITECTURE.md, module-level READMEs, or ABI.md).

## Notes & constraints

- The kernel remains responsible for memory management and execution invariants; the soft layer is authoritative for user-visible semantics and should be the first place to land runtime bug fixes.

This approach keeps the kernel small and stable while moving more of the runtime into the TypeScript layer for faster iteration, better testability, and clearer semantics.

## Next actions (recommended)

1. Add `ABI.md` documenting each native hook (signatures, ownership, errors, stability guarantees).
2. Create `soft/core/bootstrap.ts` and add minimal tests exercising VM bootstrap flow.
3. Add a CONTRIBUTING.md snippet describing the PR checklist and how to run TS tests on the VM.
