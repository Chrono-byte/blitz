Performance / Hot Path Block-Out
================================

Purpose
-------
This document tracks the remaining interpreter hot paths identified from the
benchmark review. The property-access and numeric-key regressions have already
been addressed, so this is the remaining work list.

Current status
--------------
- Property access: fixed.
- Numeric key handling: fixed.
- Conditional / ternary lowering: back on the safe `JumpIfFalse` path and
  passing the representative tests.
- Remaining hotspots below still need profiling or targeted optimization.

Work blocks
-----------
1. Method dispatch
   - Profile `method_call_chain` in `crates/blitz-engine/src/vm/runtime/execution.rs`.
   - Check:
     - call frame allocation and teardown,
     - argument setup and copying,
     - `this` binding handling,
     - unnecessary cloning of callable values.

2. Variable lookup
   - Profile nested scope access in `crates/blitz-engine/src/vm/runtime/execution.rs`.
   - Check:
     - scope-chain walking on every read,
     - captured binding cloning,
     - whether bytecode can carry a faster slot or upvalue path.

3. String methods
   - Review `String.prototype` method implementations.
   - Check:
     - receiver cloning,
     - `&str` vs owned `String` use,
     - repeated coercion in hot method paths.

4. Array operations
   - Profile `array_push_loop` and array element access.
   - Check:
     - dense-element fast paths,
     - bounds and capacity behavior,
     - cloning during push/read,
     - indexed access to sparse vs dense arrays.

5. Global function calls
   - Audit `globals.rs` call helpers such as `parseInt`, `parseFloat`, and
     `encodeURI`.
   - Check:
     - `args.first().cloned()`-style overhead,
     - unnecessary argument vector allocation,
     - repeated coercion of the first argument.

Separate semantic follow-up
---------------------------
These are not part of the hot-path todo list, but they showed up in broader
Test262 runs and should be tracked separately:
- `Array.prototype.map/create-species-abrupt`
- `Array.prototype.map/create-species-non-ctor`
- `Array.prototype.map/create-species-poisoned`
- `Array.prototype.map/create-species-undef-invalid-len`

Suggested order
---------------
1) Method dispatch
2) Array operations
3) Variable lookup
4) String methods
5) Global function calls

