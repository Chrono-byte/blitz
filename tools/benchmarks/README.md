# Engine comparison

Run identical, checked JavaScript functions on Denko and whichever of Node and
Bun are installed. This measures function execution after loading the source;
it excludes process startup and parsing. Node embeds V8 and Bun embeds JSC, so
these are runtime-hosted comparisons, not standalone V8/JSC shell measurements.

Prepare the optimized Denko adapter, then run the suite:

```sh
cargo run --release --locked -p denko-engine --example engine_benchmark -- --help
python3 tools/benchmarks/compare.py --output target/benchmarks/engines.json
```

The adapter calls the engine through its public embedding API. The Node/Bun
adapter loads the same function body with `new Function`. Each function creates
its own input, checks its result, and returns a numeric checksum. Both adapters
retain their engine instance and function between invocations. The default is
five warmup invocations followed by eleven measured invocations. These are
fixed warmups, not a claim that every engine has reached its final JIT tier.
Timing includes input allocation, validation, and any GC occurring during the
invocation. This suite does not measure peak memory or amortize setup costs.

The workloads cover arithmetic, property chains, dense arrays, callbacks,
retained closures, Unicode string methods, JSON, a record-processing pipeline,
and typed-array sorting. They are a diagnostic sample, not a substitute for
Speedometer, a full application corpus, or Test262. Constant inputs and repeated
work may be optimized by each engine; no JIT tier is disabled.

Runs are serial, with engine order rotated between cases. Keep other CPU-heavy
work idle. JSON retains all samples, medians, source hashes, the Denko executable
hash, platform information, and installed runtime/compiler versions. A timeout,
nonzero exit, or wrong checksum fails the run rather than recording a fast time.
For revision comparisons, preserve the earlier adapter and supply `--denko`:

```sh
python3 tools/benchmarks/compare.py --denko /path/to/previous/engine_benchmark \
  --output target/benchmarks/before.json
```

Use Criterion separately to isolate regressions in precompiled script execution:

```sh
cargo bench --locked -p denko-engine --bench vm_suite -- \
  'execute_(fast|slow|stress)/' --save-baseline engine-before
```

After a change, replace `--save-baseline engine-before` with
`--baseline engine-before`. Preserve the benchmark executable for alternating
runs. Do not compare Criterion's script timings directly with this harness's
function timings: lexical layout, invocation, warmup, and reset behavior differ.

Denko results also include `native_execution`: baseline activity, native loop
batches/iterations, region guard fallbacks, and currently cached plans. Counts
include warmups and nested calls. They do not form a percentage of all execution:
`interpreter_resumes` counts baseline exits, while `native_region_fallbacks`
counts declined recognized regions. The adapter accepts `--disassemble` to print
the input benchmark function's bytecode to stderr before execution.

The suite includes both repeated small-array maps and a 100,000-element map.

Use the map-path diagnostics to separate fresh callback allocation, reused
callbacks, precreated numeric input, and chained maps:

```sh
python3 tools/benchmarks/map_paths.py --before /path/to/previous/engine_benchmark \
  --output target/benchmarks/map-paths.json
```

This runs each engine twice in opposite orders with ten warmups and 51 samples.
The large diagnostics construct their input before timing. Their results must
not replace the full-input timings in `compare.py`. JSON includes the exact
source, expected checksums, executable hashes, samples, and callback counters.
