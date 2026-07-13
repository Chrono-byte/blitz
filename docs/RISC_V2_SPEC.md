# RISC v2 bytecode contract

This document is the authoritative contract for the RISC migration. Older
notes on `risc-docs` are design inputs; where they disagree with this file,
this file wins.

## Compatibility boundary

- Bytecode instructions are fixed-width 32-bit words.
- Serialized or cached bytecode must carry an explicit format version.
- `StackV1` remains readable as the compatibility format. The default script
  frontend selects `RiscV2` when every root instruction is supported and
  otherwise preserves the verified `StackV1` chunk. `compile_risc` is the
  strict API for callers that require RISC output and rejects fallback.
- The verifier, executor, disassembler, profiler, hotpatch helpers, and tests
  must decode through the same version-aware instruction API.

## Register machine

Every VM call frame owns 16 value registers. Registers are not preserved
across frames, so there is no caller-saved/callee-saved split.

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

The existing stack compiler is retained as `StackV1` IR and as the versioned
compatibility executor for suspension and exception machinery. RISC
lowering performs control-flow discovery, exact stack-height verification,
virtual-register construction, allocation/spilling, and final encoding. A
merge with conflicting stack heights is a compiler error, not a runtime fixup.

## Cutover gates

RISC selection at the public script frontend is guarded by all of the
following:

1. Stack and RISC execution agree on values, thrown errors, and observable
   side effects for the unit corpus and targeted Test262 cases.
2. Calls, constructors, closures, direct eval, exceptions, generators, async
   functions, and modules have explicit compatibility tests.
3. Bytecode verification rejects malformed opcodes, operands, branches, slot
   indices, and metadata references before execution.
4. Tracing, disassembly, profiling, and hotpatching use the shared decoder.
5. `cargo test --lib` has no new failures and the agreed Test262 compatibility
   baseline has no regressions.
