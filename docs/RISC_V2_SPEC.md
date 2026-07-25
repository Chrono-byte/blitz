# RISC v2 bytecode contract

This document is the authoritative contract for the RISC v2 backend. Older
notes on `risc-docs` are design inputs; where they disagree with this file,
this file wins.

## Execution boundary

- Bytecode instructions are fixed-width 32-bit words.
- Serialized or cached bytecode carries the RISC format version.
- Every public compiler frontend emits `RiscV2` or returns a compiler error.
  There is no compiler or executable fallback format.
- The verifier, executor, disassembler, profiler, hotpatch helpers, and tests
  decode through the same instruction API.

## Register machine

Every active RISC execution state owns 16 value registers. Registers are not
preserved across frames, so there is no caller-saved/callee-saved split. The
interpreter has no runtime operand stack; lexical cells and frame metadata are
distinct from RISC registers and spill slots.

- `r0`: callee on call entry and result on return
- `r1`: receiver (`this`)
- `r2..r7`: first six arguments
- `r8..r15`: compiler temporaries
- Additional arguments and spills live in frame slots

The compiler may use frame slots for lexical cells, captured bindings, values
live across suspension, and register spills. Observable ECMAScript semantics
must not depend on whether a value resides in a register or slot.

## Instruction and metadata rules

- The low byte is the opcode; the remaining 24 bits are its packed operands.
- The normal payload layout is `rd:4 | rs:4 | aux:8 | aux2:8` from low to
  high payload bits. `CompareJump` instead uses
  `condition:3 | left:4 | right:4 | signed-offset:13`; long conditional
  branches lower to an inverted short branch followed by a long jump.
- Branch targets are instruction indices, never byte offsets.
- Operand layouts are defined once and shared by encoding and decoding.
- Property inline-cache sites use side metadata indexed by an instruction-site
  identifier. The bytecode format does not impose a 16-site function limit.
- Source locations are metadata keyed by instruction index.

## Compiler pipeline

The frontend builds explicit RISC values, labels, branches, and operand
windows for the root and every nested function. The compiler still uses a
compiler-only virtual value stack for a small set of lowering operations; it is
eliminated before serialization. Finalization allocates temporary values to
`r8..r15` or spill slots, patches control-flow and side metadata, and emits
verified RISC v2 words. Generators preserve an explicit register/spill
continuation across `yield`; exceptions, async functions, direct eval, and
ordinary calls all use the same RISC executor.

## Backend invariants

The RISC-only backend is guarded by all of the following:

1. RISC execution preserves values, thrown errors, and observable side effects
   for the unit corpus and targeted Test262 cases.
2. Calls, constructors, closures, direct eval, exceptions, generators, async
   functions, and modules have explicit compatibility tests.
3. Bytecode verification rejects malformed opcodes, operands, branches, slot
   indices, and metadata references before execution.
4. Tracing, disassembly, profiling, and hotpatching use the shared decoder.
5. `cargo test -p blitz-engine --lib` has no new failures and the agreed Test262 compatibility
   baseline has no regressions.
