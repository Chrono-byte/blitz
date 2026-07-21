# Test262 conformance baselines

Regenerate the deterministic ES6 selection from the pinned Test262 checkout:

```powershell
cargo run -p blitz-engine --bin test262 -- --es-version ES6 --write-manifest docs/test262/es6-manifest.txt
```

Run the short, representative ES6 smoke set and write its categorized report:

```powershell
cargo run -p blitz-engine --bin test262 -- --manifest docs/test262/es6-smoke.txt --summary-json target/test262/es6-smoke-summary.json --quiet
```

The runner exits non-zero while the manifest contains known conformance
failures. CI should still retain the JSON report so crashes, hangs, skips, and
failure-class changes remain visible while compatibility work is in progress.

Run the checked-in full manifest with the same report format:

```powershell
cargo run -p blitz-engine --bin test262 -- --manifest docs/test262/es6-manifest.txt --summary-json target/test262/es6-summary.json --workers 4 --batch-size 20 --subprocess-timeout 35 --quiet
```

Manifest entries are stable forward-slash paths relative to `test262/test`.
Comments and blank lines are ignored; missing or escaping paths are rejected.
The schema-v2 summary always reports `pass`, `fail`, `skip`, `crash`, and
`hang` outcomes, including zero counts. Category tags can overlap: for example,
a normative module test appears in both `normative` and `module`.

The reviewed baseline and interpretation are recorded in `ES6_BASELINE.md`.
