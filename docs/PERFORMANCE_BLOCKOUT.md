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

Measured collector optimization, 2026-09-05
------------------------------------------
The method-call fixture exposed GC identity hashing as a measurable cost:
`perf` attributed 5.15% of cycle samples to standard randomized hashing of
`usize` identities, plus time in hash-set insertion and rehashing.

The collector now uses `rustc_hash::FxHashSet` for its internal pointer and
arena-handle sets, including finalization-cleanup deduplication. These sets
use membership and length checks, not iteration order. JavaScript property
keys still use their existing hashing, and marking/sweeping logic is unchanged.

On a Ryzen 9 5900X, the first full Criterion comparison measured
`execute_fast/method_call_chain` at 61.62 ms before and 55.46 ms after, about
10% less execution time. Alternating quick runs measured 61.83/61.85 ms before
and 56.40/55.26 ms after. A second full comparison measured 63.26 ms before
and 56.06 ms after. The follow-up profile no longer showed randomized
`usize` hashing among the hotspots. These are local fixture results, not a
comparison against V8 or JSC. Arithmetic and property-access timings varied
across runs; no general speedup is claimed for those workloads.

To compare revisions, run the following before changing the collector, then
replace `--save-baseline gc-before` with `--baseline gc-before` afterward:

```sh
cargo bench -p denko-engine --bench vm_suite -- \
  'execute_fast/(arithmetic_multi_ops|property_access_chain|array_element_access|nested_scope|method_call_chain)$' \
  --save-baseline gc-before
```

Keep the original benchmark executable for alternating before/after runs.
Run CPU-heavy checks separately from timing measurements.

Call state and shared-scope collection, 2026-09-05
------------------------------------------------
Three further changes reduce copying and repeated GC work:

- Closure environments share their arena/handle data through `Rc`. On this
  64-bit target, `Value` shrinks from 64 to 40 bytes and a 16-register array
  shrinks from 1,024 to 640 bytes. This is a 37.5% reduction in register-value
  storage, not a measurement of total process memory. Each distinct environment
  gains a shared allocation; clones reuse it. Weak upgrades still validate the
  arena generation and can recover arena-owned cells after ordinary handles drop.
- Ordinary and tail calls initialize callee registers directly from prepared
  arguments, removing an intermediate vector and redundant register replacement.
  Receiver normalization still happens before installing the callee receiver.
- GC scans each lexical environment's own bindings and visits its outer scope
  through the existing visited set. Previously every captured environment copied
  the whole ancestor chain without marking those ancestor environments visited.
  In the retained-closure profile, binding collection alone took 10.8% of samples.
  The new walk avoids repeated ancestor scans and temporary binding vectors.

The comparison below starts **after** the earlier identity-hashing change.
Both versions include the new `closure_environment_creation` fixture, which
retains 5,000 separately captured closures and checks their returned sum.
Measurements use the existing Criterion settings, Rust 1.97.1's optimized bench
profile, and the same Ryzen 9 5900X. CPU-heavy checks ran separately.

| Fixture | GC-only baseline, ms | Final, ms | Less execution time |
| --- | ---: | ---: | ---: |
| `arithmetic_multi_ops` | 0.740 | 0.690 | 6.6% |
| `property_access_chain` | 41.654 | 38.929 | 6.5% |
| `array_element_access` | 21.412 | 21.172 | 1.1% |
| `nested_scope` | 2.150 | 2.098 | 2.4% |
| `method_call_chain` | 55.618 | 51.022 | 8.3% |
| `string_concat_loop` | 0.838 | 0.818 | 2.4% |
| `closure_environment_creation` | 384.730 | 227.450 | 40.9% |
| `recursive_fib` | 407.700 | 351.900 | 13.7% |
| `large_array_creation` | 373.590 | 336.780 | 9.9% |

These are fixture-specific point estimates. Small differences can be noise:
an intermediate run showed a roughly 3% nested-scope slowdown, which was not
consistent across the later full sweep and alternating-binary checks. Array reads
were essentially unchanged. The closure-creation and recursive-call gains
also repeated with binaries run in before/after/after/before order.

To reproduce, keep the new fixture and benchmark registration in both trees,
then run this on the GC-only version. On the final version, replace
`--save-baseline compact-before` with `--baseline compact-before`:

```sh
cargo bench -p denko-engine --bench vm_suite -- \
  'execute_(fast/(arithmetic_multi_ops|property_access_chain|array_element_access|nested_scope|string_concat_loop|method_call_chain|closure_environment_creation)|stress/(recursive_fib|large_array_creation))$' \
  --save-baseline compact-before
```

The layout test now caps `Value` at 40 bytes. Added regressions cover recovery
of arena-owned environments after shared handles drop and marking a shared
outer scope when reached through multiple child scopes. Existing generation
reuse, closure, GC, recursion, exception, and generator tests remain relevant.

Final validation: `cargo fmt --all -- --check` and
`cargo clippy --workspace --all-targets --locked` pass.
`cargo test --workspace --no-fail-fast` reports 830 passing tests, including
all 572 engine unit tests, and 13 failures. Restoring the original engine
files reproduces the exact same 13 failing test names: 11 runtime compatibility
tests, the Effect foundation negative-fixture checker, and the embedded-source
audit. The original engine has 828 passing tests; the final version includes
two additional passing tests. No new workspace failure was observed.

Work blocks
-----------
1. Method dispatch
   - Profile `method_call_chain` in `crates/denko-engine/src/vm/runtime/execution.rs`.
   - Check:
     - call frame allocation and teardown,
     - argument setup and copying,
     - `this` binding handling,
     - unnecessary cloning of callable values.

2. Variable lookup
   - Profile nested scope access in `crates/denko-engine/src/vm/runtime/execution.rs`.
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


Benchmark tooling
-----------------
Use the [shared comparison harness](../tools/benchmarks/README.md) for checked
Denko, Node, and Bun measurements. Store generated results under `target/benchmarks/`.
