Iterator / Generator Completion Block-Out
=========================================

Purpose
-------
This document blocks out the next implementation slice after lexical
environment records: completing iterator and generator execution semantics.

Current status
--------------
- Bytecode has `Yield` / `YieldStar` opcodes.
- VM now supports a baseline generator object with `next/throw/return`.
- Remaining work focuses on delegation (`yield*`) and stronger protocol
  semantics/edge cases.

Work blocks
-----------
1. Generator object runtime type
   - Add internal generator record (`state`, `resume_pc`, `frame snapshot`,
     `done`, `last_sent`).
   - Expose `next(value)`, `throw(error)`, `return(value)` methods.

2. VM suspension/resume
   - Teach `Opcode::Yield` to produce `{ value, done: false }` and suspend.
   - Teach `Opcode::YieldStar` delegation protocol.
   - Add resume entrypoint that restores frame/locals/PC and continues.

3. Function call semantics
   - Calls to generator functions should return generator objects and not run
     the body to completion immediately.
   - Constructor and async-generator restrictions should follow ECMAScript
     rules.

4. Iterator protocol completion
   - Normalize iterator acquisition (`GetIterator`) and completion handling.
   - Ensure abrupt completion closes iterators where required.
   - Use protocol for `for..of`, spread, and destructuring.

5. Conformance tests
   - Add focused tests for:
     - `function*` + `.next()`
     - `yield*` delegation
     - `for..of` on generators
     - iterator closing on throw/break

Suggested order
---------------
1) Generator runtime type + `next` scaffolding.
2) `Opcode::Yield` suspension path.
3) `yield*` and iterator-close semantics.
4) Compiler/runtime updates for `for..of` and spread to rely on protocol.
5) Test262 subset enablement for generator/iterator features.
