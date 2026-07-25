Post-Iterator/Generator Feature Block-Out
========================================

This is the planned sequence after landing iterator/generator functionality.

1) Class semantics minimal vertical slice
-----------------------------------------
- Parse/compile class declarations with:
  - constructor methods,
  - prototype method installation,
  - `extends` + `super()` in derived constructors,
  - method `[[HomeObject]]` metadata for `super` property lookup.
- Explicitly out of slice: private fields, decorators, static blocks.

2) Microtask checkpoint formalization
-------------------------------------
- Define host checkpoints:
  - after script/module evaluation completion,
  - after each host callback returns to the VM,
  - before yielding control back to host event loop.
- Keep Promise job queue execution only at checkpoints (not opportunistically
  after every opcode), preserving deterministic order.

3) ESM correctness pass
-----------------------
- Strengthen cycle handling:
  - instantiate strongly-connected components before evaluation,
  - validate live-binding updates across cycles.
- Expand module test coverage:
  - cycle graphs (`a -> b -> a`),
  - re-export chains,
  - temporal ordering of initialized exports.

4) GC plan (mark/sweep prototype)
---------------------------------
- Introduce mark/sweep collector prototype while preserving current runtime:
  - root set: globals, stacks, frames, module slots, pending jobs,
  - tri-color mark phase over object graphs,
  - sweep phase with free-list reuse.
- Run GC behind a feature flag first, keep existing behavior as fallback.
