# Nova-Inspired Heap, Value Migration, and JIT Roadmap

## Objective

Finish the work begun around JIT feasibility, compact runtime values, a
VM-owned heap, and the Nova-inspired memory-safety pivot.

The required dependency order is:

```text
Current prototype
      |
      v
Cross-arena heap and rooting
      |
      v
Primitive and object migration
      |
      v
VM frames, environments, and host roots
      |
      v
Activate compacting GC
      |
      v
Delete the Rc/RefCell value model
      |
      v
Baseline RISC JIT
      |
      v
Inline caches and optimization
```

The collector must be complete before runtime values migrate, and the runtime
value/GC ABI must be stable before generated code is introduced. Otherwise the
rooting API, JIT frame ABI, and stack maps would need to be rebuilt twice.

## Target Architecture

Treat these decisions as fixed unless implementation evidence requires a
change:

- Internal engine values are one-word `Value<'gc>` values.
- Ordinary `f64` numbers remain inline and never allocate merely because they
  cannot use a compressed representation.
- Heap references encode an arena kind and a 32-bit index.
- Every heap-data kind lives in a homogeneous typed arena.
- Ordinary handles borrow a `NoGcScope` epoch and cannot cross a possible GC
  safepoint.
- Heap records, VM frames, suspended execution, and roots store explicitly
  unbound values and are rewritten during compaction.
- A value needed across a safepoint must be explicitly scoped or globally
  rooted.
- Arrays and exotic objects lazily allocate their ordinary-object backing
  data.
- `blitz-engine` exposes explicit memory-safe engine semantics.
  `blitz-runtime` may wrap these APIs in ergonomic persistent host values.
- The interpreter and JIT use exactly the same 64-bit value representation,
  runtime helpers, and GC rules.

Before freezing the value ABI, add an internal `EMPTY`/`HOLE` tag. Using
`Option<Value>` with a NaN-boxed value would probably make each array element
16 bytes; a tagged hole keeps it at eight bytes.

## Current Foundation

The initial groundwork is present in:

- `crates/blitz-engine/src/value/scope.rs`
- `crates/blitz-engine/src/value/heap.rs`
- `crates/blitz-engine/src/value/tagged.rs`
- `Constant::as_tagged_immediate` in `bytecode.rs`

It currently demonstrates:

- One-word tagged values with inline `f64`.
- Arena-kind plus 32-bit-index heap handles.
- Four-byte typed arena indices.
- `GcScope` and `NoGcScope` capability lifetimes.
- Scoped rooting and post-safepoint rebinding.
- Single-arena marking, compaction, and edge rewriting.

It is not yet connected to the production `Vm`, current `Value`, or prototype
collector.

## Phase 1: Harden the Memory Model

### Work

- Choose the final internal name, preferably `Value<'gc>`, and use it
  consistently.
- Add the internal hole sentinel and reserve remaining internal tags.
- Define typed handles for every planned arena.
- Add shared `Bindable`-style behavior for values, handles, options, and
  collections.
- Separate scoped roots from persistent/global roots.
- Restrict unbinding to explicit call, return, root, and heap-storage
  boundaries.
- Add compile-fail UI tests showing that a bound value cannot be used after
  `GcScope::reborrow`.
- Add layout assertions for values, handles, and common storage records.
- Document the safepoint and rooting rules beside the APIs.

### Exit Gate

- Safepoint misuse fails to compile.
- Rooted values rebind correctly after simulated relocation.
- Ordinary heap access cannot bypass the GC lifetime.
- `Value` is eight bytes and typed handles are four bytes.

## Phase 2: Build the Multi-Arena `VmHeap`

Replace the prototype's same-type tracing with a global heap:

```rust
struct VmHeap {
    strings: Arena<StringRecord>,
    bigints: Arena<BigIntRecord>,
    symbols: Arena<SymbolRecord>,

    shapes: Arena<ShapeRecord>,
    property_values: Arena<PropertyValues>,
    objects: Arena<ObjectRecord>,
    arrays: Arena<ArrayRecord>,

    environments: Arena<EnvironmentRecord>,
    closures: Arena<ClosureRecord>,
    builtins: Arena<BuiltinRecord>,
    proxies: Arena<ProxyRecord>,
    promises: Arena<PromiseRecord>,

    gc: GcContext,
    scoped_roots: RootSet,
    global_roots: GlobalRootSet,
}
```

### Work

- Add one mark bitmap and work queue per arena.
- Add a global `WorkQueues` structure for cross-arena tracing.
- Add a `CompactionLists` structure covering every arena.
- Make every record implement exhaustive marking and sweeping.
- Require reference-bearing record implementations to destructure every field
  without `..`, so adding a field breaks compilation until tracing is updated.
- Add weak-edge, finalization, and ephemeron queues.
- Add allocation pressure and explicit safepoint triggering.
- Add debug heap validation and collector statistics.
- Define cache relocation or invalidation hooks.

### Exit Gate

- Mixed-type graphs and cycles trace correctly.
- Every arena compacts and every retained handle is rewritten.
- Invalid strong references are detected immediately.
- Weak references do not retain targets.

## Phase 3: Migrate Primitive Heap Values

Migrate in this order:

1. Strings
2. BigInts
3. Symbols

### Strings

- Keep compiler-owned literals distinct from runtime heap strings.
- Preserve exact UTF-16/`JsString` behavior.
- Add heap string lookup/interning.
- Defer small-string encoding until measurements justify it.

### BigInts

- Store `num_bigint::BigInt` in the BigInt arena.
- Route all BigInt operations through heap-aware helpers.
- Consider small-BigInt encoding only after the main migration.

### Symbols

- Store identity and optional description in symbol records.
- Preserve well-known symbols and the global symbol registry.
- Make symbol-key relocation and identity explicit.

### Exit Gate

- Primitive `Value` variants no longer own or reference-count payloads.
- Constant materialization allocates through `VmHeap`.
- Primitive formatting, equality, and ECMAScript behavior are unchanged.

## Phase 4: Migrate Ordinary Objects and Properties

Use a compact object record:

```rust
struct ObjectRecord {
    shape: ShapeIndex,
    values: PropertyValuesIndex,
}
```

### Work

- Introduce shapes and shape transitions.
- Store common data-property values densely.
- Separate uncommon descriptors, accessors, and cold internal slots.
- Preserve ECMAScript property ordering and descriptor semantics.
- Represent prototypes in shapes or dedicated object metadata.
- Replace pointer-address identity with handle identity.
- Replace pointer-keyed inline caches with shape/object handles.
- Initially flush caches after GC; relocate them later only if beneficial.

### Exit Gate

- Ordinary objects no longer use `Rc<RefCell<ObjectData>>`.
- Prototype changes, descriptors, enumeration, and equality remain correct.
- Object identity never depends on an allocation address.

## Phase 5: Migrate Arrays and Exotic Objects

Use a lazy backing-object structure:

```rust
struct ArrayRecord {
    elements: ElementsIndex,
    backing_object: Option<ObjectIndex>,
    realm: RealmIndex,
}
```

### Work

- Store holes with the internal hole sentinel.
- Preserve Blitz's dense and sparse array behavior.
- Lazily allocate ordinary backing data for named properties, custom
  prototypes, and unusual descriptors.
- Keep `length` behavior in array-specific data.
- Apply the business-data/backing-object split to ArrayBuffers, typed arrays,
  DataViews, RegExps, maps, sets, weak collections, and other exotic objects.
- Introduce capacity-class element pools only after profiling.

### Exit Gate

- Common dense arrays need no ordinary-object allocation.
- Sparse arrays remain supported.
- Exotic internal methods retain correct proxy and receiver behavior.

## Phase 6: Migrate Functions, Environments, and Promises

### Work

- Replace `SharedValue = Rc<RefCell<Value>>` with heap binding-cell handles.
- Move lexical and variable environments into typed arenas.
- Store closure code, captured environment, lexical `this`, and property
  backing separately.
- Store native callbacks in builtin heap records rather than tagged values.
- Migrate promises, reactions, microtasks, and rejection state.
- Migrate proxies and revoked-handler state.
- Migrate modules and live import/export bindings.
- Make generator, async, and other suspended execution state fully traceable
  and relocatable.

### Exit Gate

- Closure/environment cycles are collected without `Rc`.
- Module live bindings preserve identity.
- Suspended generators and async jobs survive compaction.

## Phase 7: Convert VM State and Execution

Persistent interpreter state should store unbound compact values:

- RISC registers and spills.
- Locals and binding cells.
- Call frames.
- Exception and construction state.
- Generator and yield-star state.
- Microtasks and promise reactions.
- Module tables.
- Realm intrinsics.
- Property caches.

The operating pattern should be:

```rust
// Persistent frame storage:
Value<'static>

// During an operation that cannot collect:
let value: Value<'gc> = stored.bind(nogc);

// Across a call that may collect:
let rooted = value.scope(vm, gc.nogc());
runtime_call(vm, gc.reborrow());
let value = rooted.get(vm, gc.nogc());
```

### Host Boundary

- Replace exported long-lived raw values with persistent host roots.
- Preserve store identity and stale-root detection.
- Prevent host values from outliving their owning VM.
- Let `blitz-runtime` wrap roots ergonomically.
- Keep raw-engine/conformance APIs explicit.

### Exit Gate

- Registers and spills use eight bytes per value.
- Active and suspended frames are collector roots.
- Host integrations cannot silently retain stale handles.

## Phase 8: Activate the Compacting Collector

### Work

- Add `VmHeap` and GC context ownership to `Vm`.
- Route every JavaScript heap allocation through `VmHeap`.
- Replace `collect_garbage_prototype`.
- Remove the thread-local weak registries and `VmGcRegistration`.
- Run collection only at declared safepoints.
- Mark all VM, realm, module, host, and scoped roots.
- Compute compactions for all arenas.
- Rewrite heap records, roots, frames, caches, and suspended state.
- Implement weak-map ephemeron fixed-point marking.
- Add a test mode that collects at every legal safepoint.
- Optionally validate all live handles after each test collection.

### Exit Gate

- Aggressive collection passes the engine suite.
- Cycles are reclaimed.
- Weak maps and weak references behave correctly.
- No old collector participates in liveness.

## Phase 9: Remove the Legacy Model

Delete:

- The existing ownership-bearing `Value` enum.
- `Rc<RefCell<ObjectData>>` and associated weak wrappers.
- `SharedValue`.
- Pointer-based identity and cache keys.
- Prototype GC tracking functions and registries.
- Temporary old/new conversion bridges.
- Dead-code allowances on the new heap modules.

### Verification Gate

- Engine unit tests.
- Formatting and Clippy with warnings denied.
- Test262 comparison against the pre-migration baseline.
- Aggressive-GC stress tests.
- Memory and allocation benchmarks.
- Miri on safe heap/root APIs where practical.

This is the value and GC ABI freeze point for the JIT.

## Phase 10: Add Baseline JIT Infrastructure

Use verified RISC v2 bytecode as the only compilation input.

### Initial Design

- Feature-gated JIT within `blitz-engine`.
- Cranelift as the initial backend.
- Per-chunk execution counters.
- Compiled-code cache owned by the VM/runtime.
- Stable internal helper ABI.
- A `JitFrame` containing tagged registers and spill slots.
- Explicit JIT exits for return, throw, call, yield, and fallback.
- No Rust unwinding across generated code.
- Interpreter fallback on unsupported platforms or operations.

At each helper call or safepoint:

1. Spill all live tagged registers to `JitFrame`.
2. Register the frame as a GC root.
3. Allow the helper and collector to run.
4. Let compaction rewrite frame values.
5. Reload values before resuming generated code.

This is initially preferable to keeping arbitrary heap handles in machine
registers and implementing precise native stack maps immediately.

### First Opcode Set

- Constants.
- Moves.
- Spill loads and stores.
- Numeric arithmetic.
- Boolean operations.
- Comparisons.
- Conditional and unconditional branches.
- Locals.
- Return.

### Exit Gate

- A compiled function executes the initial opcode set.
- Interpreter and JIT results match.
- GC can safely run during a compiled helper call.
- Exceptions do not unwind through generated machine code.

## Phase 11: Complete Baseline Opcode Coverage

Implement groups in this order:

1. Numeric and boolean operations.
2. Control flow.
3. Local and environment access.
4. Property operations through runtime helpers.
5. Calls and construction.
6. Exceptions, catch, and `finally`.
7. Iteration.
8. Object, array, and closure creation.
9. Modules and dynamic operations.

Initially keep generators, async suspension, direct `eval`, and especially
complex proxy paths interpreted if necessary. A function containing an
unsupported operation should remain entirely interpreted until mixed-mode
transitions are demonstrably safe.

### Exit Gate

- Every RISC opcode either compiles or has an explicit verified fallback.
- Exceptions cross interpreter/JIT boundaries correctly.
- Unsupported semantics are never approximated or silently replaced.

## Phase 12: Tiering and Optimization

After baseline correctness:

- Add hot-function counters and tiering thresholds.
- Consider background compilation.
- Add shape guards and property inline caches.
- Add monomorphic and polymorphic cache paths.
- Add inline numeric fast paths with runtime slow paths.
- Specialize dense array element access.
- Add direct-call specialization.
- Add deoptimization metadata.
- Add on-stack replacement only after ordinary tiering is stable.
- Replace spill-all safepoints with precise stack maps when justified.

### Exit Gate

- JIT execution improves representative workloads.
- Compilation cost is amortized.
- JIT plus aggressive GC stress mode passes.
- Performance regressions have benchmark thresholds.

## Cross-Cutting Verification

Run these gates throughout the migration:

- Unit tests for each arena and record.
- Compile-fail safepoint lifetime tests.
- Collector tests for cycles, weak edges, ephemerons, and relocation.
- Existing engine tests after every phase.
- Test262 differential checks after every semantic migration.
- Interpreter/JIT differential execution.
- Aggressive GC at every legal safepoint.
- JIT helper-call GC stress.
- Heap validation in debug/test configurations.
- Allocation, memory-density, property-access, array, call, and GC benchmarks.

## Definition of Done

The thread's original and pivoted work is complete when:

- The engine consistently uses the scoped Nova-style memory model.
- `Value` is one word and ordinary numbers remain inline.
- All JavaScript-owned mutable data lives in VM-owned typed arenas.
- Compacting GC covers roots, frames, weak edges, caches, and suspended
  execution.
- The old `Rc` collector and ownership-bearing value representation are gone.
- The RISC interpreter operates on compact values.
- Hot RISC functions tier into baseline native code.
- Interpreter and JIT share semantic runtime helpers.
- Test262 behavior does not regress.
- Aggressive GC and combined JIT/GC stress modes pass.

## Recommended Delivery Slices

Keep changes reviewable and continuously testable:

1. Memory-model API hardening and compile-fail tests.
2. Cross-arena work queues and compaction lists.
3. String, BigInt, and symbol arenas.
4. Shapes, properties, and ordinary objects.
5. Arrays and exotic backing objects.
6. Environments, closures, and binding cells.
7. Proxies, promises, modules, and suspended state.
8. VM frame and host-root conversion.
9. Activate new GC and remove the prototype collector.
10. Remove the legacy `Value` representation.
11. Baseline JIT infrastructure and core opcodes.
12. Full baseline coverage, tiering, and measured optimization.
