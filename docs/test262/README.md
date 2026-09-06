# Test262 conformance baselines

Regenerate the deterministic ES6 selection from the pinned Test262 checkout:

```powershell
cargo run -p blitz-test262 --bin test262 -- --es-version ES6 --write-manifest docs/test262/es6-manifest.txt
```

Run the short, representative ES6 smoke set and write its categorized report:

```powershell
cargo run -p blitz-test262 --bin test262 -- --manifest docs/test262/es6-smoke.txt --summary-json target/test262/es6-smoke-summary.json --quiet
```

The runner exits non-zero while the manifest contains known conformance
failures. CI should still retain the JSON report so crashes, hangs, skips, and
failure-class changes remain visible while compatibility work is in progress.

Run the checked-in full manifest with the same report format:

```powershell
cargo run -p blitz-test262 --bin test262 -- --manifest docs/test262/es6-manifest.txt --summary-json target/test262/es6-summary.json --workers 1 --batch-size 1 --subprocess-timeout 35 --quiet
```

Keep full-manifest runs at one test per subprocess. Larger batches allow engine
heap retention to accumulate within each child process.

Manifest entries are stable forward-slash paths relative to `test262/test`.
Comments and blank lines are ignored; missing or escaping paths are rejected.
The schema-v2 summary always reports `pass`, `fail`, `skip`, `crash`, and
`hang` outcomes, including zero counts. Category tags can overlap: for example,
a normative module test appears in both `normative` and `module`.

The reviewed baseline and interpretation are recorded in `ES6_BASELINE.md`.

Async generators, async iteration, and top-level await are tracked in
[ASYNC_COMPATIBILITY.md](ASYNC_COMPATIBILITY.md), with separate compatibility
and Promise/async-function regression manifests.

Modern object, array, typed-array copying, and stable sorting behavior is tracked
in [MODERN_COLLECTIONS_COMPATIBILITY.md](MODERN_COLLECTIONS_COMPATIBILITY.md),
with separate compatibility and regression manifests.

Native counted-loop decoding and fallback regressions are tracked in
[the native coverage report](../benchmarks/NATIVE_LOOP_COVERAGE.md) and
`native-loop-manifest.txt`.

Array callback validation is tracked in `callback-manifest.txt` and
[the callback execution report](../benchmarks/CALLBACK_EXECUTION.md).

Packed numeric array storage is checked with `packed-array-manifest.txt`, which
covers map species users and array push, fill, copyWithin, reverse, sort, slice,
splice, and concat. Matched outcomes and engine regression tests are recorded in
[the packed numeric map report](../benchmarks/PACKED_NUMERIC_MAP.md).
