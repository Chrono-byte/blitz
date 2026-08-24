# ES6 Test262 baseline

Reviewed on 2026-08-23 against Test262 commit
`f476a3e7b9659f68d04627d3df3a0492a7f9eb00`.

The stable `es6-manifest.txt` selection contains 9,294 tests and has SHA-256
digest:
`88af8ac5b23184b2888cb197df9a1dcb4b77660f77854f44b0bfd80854aa1c63`.

The manifest is the reproducibility boundary for this baseline. The current
Test262 metadata selects 9,281 tests when regenerated with `--es-version ES6`,
so regeneration is not expected to reproduce this historical selection.

The reviewed run used:

```powershell
cargo run -p blitz-test262 --bin test262 -- --manifest docs/test262/es6-manifest.txt --summary-json target/test262/es6-summary-current.json --workers 1 --batch-size 1 --subprocess-timeout 150 --quiet
```

The one-test subprocess batches bound engine memory across the full run. Do not
increase the batch size until the runner has a measured heap-retention test.

## Outcomes

| Outcome | Count |
| --- | ---: |
| Pass | 8,913 |
| Fail | 381 |
| Crash | 0 |
| Hang | 0 |
| Unsupported/skip | 0 |
| Total | 9,294 |

The overall pass rate is 95.90%, an improvement of 1,167 passing tests over
the previously documented 7,746-pass baseline.

## Categories

Category tags overlap. In particular, module, async, and negative tests can
also be normative.

| Category | Total | Pass | Fail | Crash | Hang | Pass rate |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Normative | 9,245 | 8,871 | 374 | 0 | 0 | 95.95% |
| Module | 52 | 42 | 10 | 0 | 0 | 80.77% |
| Async | 83 | 69 | 14 | 0 | 0 | 83.13% |
| Negative parse | 741 | 741 | 0 | 0 | 0 | 100.00% |
| Negative runtime | 1 | 1 | 0 | 0 | 0 | 100.00% |
| Annex B | 33 | 28 | 5 | 0 | 0 | 84.85% |
| Staging | 16 | 14 | 2 | 0 | 0 | 87.50% |

## Failure classes

| Diagnostic class | Count |
| --- | ---: |
| Parser or compiler | 32 |
| Assertion or semantics | 192 |
| Missing abstract operation | 27 |
| Missing intrinsic | 37 |
| Exception propagation | 60 |
| Incorrect descriptor | 21 |
| Realm or job queue | 12 |
| Stack or control flow | 0 |
| Hang | 0 |

These classes are diagnostic heuristics for prioritization, not normative
Test262 labels. Classification examines the diagnostic text without the test
path, preventing feature-directory names from contaminating root-cause totals.

## Crash and hang review

The reviewed run completed with no crashes or individually attributed hangs.
The subprocess runner did recursively split timed-out batches, but every
individual test completed once isolated. The previous 31 native stack-overflow
crashes and the Promise hang are no longer present.

No tests were skipped as unsupported. The remaining failures are therefore
test-specific implementation work, not broad exclusions or unexplained runner
omissions.

## Phase 2 priorities

The largest remaining actionable groups support the roadmap order:

1. Realm ownership for dynamic functions, intrinsic fallback prototypes, and
   cross-realm accessors.
2. Proxy invariants and exception propagation.
3. Remaining destructuring iterator completion and `IteratorClose` behavior in
   assignment and `for-of` paths.
4. RegExp protocol methods, especially `Symbol.replace`, plus Annex B
   `RegExp.prototype.compile` behavior.
5. Direct and indirect eval environments, generator parameter environments,
   modules, and remaining Promise behavior.
