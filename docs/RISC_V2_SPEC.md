# RISC v2 bytecode contract

This document is the authoritative contract for the RISC v2 backend. Older
notes on `risc-docs` are design inputs; where they disagree with this file,
this file wins.

## Execution boundary

- Bytecode instructions are fixed-width 32-bit words.
- Serialized or cached bytecode carries the RISC format version.
- Every public compiler frontend emits `BytecodeVersion::RiscV2` or returns a compiler error.
  There is no compiler or executable fallback format.
- The verifier, executor, disassembler, profiler, hotpatch helpers, and tests
  decode through the same instruction API.

## Register machine

Every active execution context owns 16 value registers. A bytecode call
suspends the caller's register and spill state in its call frame, so nested
ordinary closure calls remain inside one iterative interpreter loop. There is
no caller-saved/callee-saved split and no runtime operand stack; lexical cells
and frame metadata are distinct from registers and spill slots. Strict
proper tail calls replace the active execution context without growing the VM
frame stack when no language-mandated cleanup remains.

- `r0`: return-value register
- `r1`: receiver (`this`)
- `r2..r7`: first six arguments
- `r8..r15`: compiler temporaries
- Additional arguments and compiler spills use frame slots

The active function is execution metadata. It is not assigned a dedicated
value register. An ordinary callee has no required incoming value in `r0`.
Call entry initializes `r1` and up to six arguments in `r2..r7`; return writes
the result to `r0` before restoring the caller.

RISC v2 call words predate this entry ABI and encode the caller-side callee
operand implicitly in `r0`. The interpreter captures that transient operand
before it installs the callee execution context. It never carries the active
function in the callee's incoming `r0`. This preserves the serialized v2 word
format without retaining the old execution-state convention.

The compiler may use frame slots for lexical cells, captured bindings, values
live across suspension, and register spills. Observable ECMAScript semantics
must not depend on whether a value resides in a register or slot.

## Instruction and metadata rules

- `Function` is a compiled ECMAScript function with executable code and
  function metadata. Its `chunk` field is a `Chunk` value.
- `Instruction` is one decoded semantic instruction. A raw `u32` in
  `Chunk::words` is an instruction word.
- `Opcode` is the serialized opcode enum.
- `pc`, `target_pc`, and `resume_pc` are instruction indices. Byte offsets are
  named as such and only appear where storage is byte-addressed.
- `VirtualValue` exists only during compiler lowering. `AllocatedValue`
  distinguishes a physical register from a spill slot.

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

The frontend builds explicit virtual values, labels, branches, and operand
windows for the root and every nested function. The compiler still uses a
compiler-only virtual value stack for a small set of lowering operations; it is
eliminated before serialization. Finalization allocates temporary values to
`r8..r15` or spill slots, patches control-flow and side metadata, and emits
verified instruction words. Generators preserve an explicit register/spill
continuation across `yield`; exceptions, async functions, direct eval, and
ordinary calls all use the same executor.

## Backend invariants

The bytecode backend is guarded by all of the following:

1. Execution preserves values, thrown errors, and observable side effects
   for the unit corpus and targeted Test262 cases.
2. Calls, constructors, closures, direct eval, exceptions, generators, async
   functions, and modules have explicit compatibility tests.
3. Bytecode verification rejects malformed opcodes, operands, branches, slot
   indices, and metadata references before execution.
4. Tracing, disassembly, profiling, hotpatching, region recognition, and JIT
   compilation use the shared instruction layouts and decoder.
5. `cargo test -p denko-engine --lib` has no new failures and the agreed Test262 compatibility
   baseline has no regressions.
