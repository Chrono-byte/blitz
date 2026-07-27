# ES6 Test262 baseline

Reviewed on 2026-07-15 against Test262 commit
`f2d1435644797268dca1f7988cad5a4e89ccd8d2`.

The stable `es6-manifest.txt` selection contains 9,294 tests. Regenerating the
manifest and loading it through `--manifest --write-manifest` produced the same
SHA-256 digest:
`88af8ac5b23184b2888cb197df9a1dcb4b77660f77854f44b0bfd80854aa1c63`.

The reviewed run used:

```powershell
cargo run -p blitz-test262 --bin test262 -- --manifest docs/test262/es6-manifest.txt --summary-json target/test262/es6-summary.json --workers 4 --batch-size 20 --subprocess-timeout 35 --quiet
```

## Outcomes

| Outcome | Count |
| --- | ---: |
| Pass | 3,741 |
| Fail | 5,521 |
| Crash | 31 |
| Hang | 1 |
| Unsupported/skip | 0 |
| Total | 9,294 |

The overall pass rate is 40.25%.

## Categories

Category tags overlap. In particular, module, async, and negative tests can
also be normative.

| Category | Total | Pass | Fail | Crash | Hang | Pass rate |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Normative | 9,245 | 3,739 | 5,474 | 31 | 1 | 40.44% |
| Module | 52 | 37 | 15 | 0 | 0 | 71.15% |
| Async | 83 | 7 | 76 | 0 | 0 | 8.43% |
| Negative parse | 741 | 599 | 142 | 0 | 0 | 80.84% |
| Negative runtime | 1 | 1 | 0 | 0 | 0 | 100.00% |
| Annex B | 33 | 0 | 33 | 0 | 0 | 0.00% |
| Staging | 16 | 2 | 14 | 0 | 0 | 12.50% |

## Failure classes

| Root cause | Count |
| --- | ---: |
| Parser or compiler | 2,134 |
| Assertion or semantics | 2,190 |
| Missing abstract operation | 363 |
| Missing intrinsic | 340 |
| Exception propagation | 343 |
| Incorrect descriptor | 120 |
| Realm or job queue | 31 |
| Stack or control flow | 31 |
| Hang | 1 |

These classes are diagnostic heuristics for prioritization, not normative
Test262 labels. Classification examines the diagnostic text without the test
path, preventing feature-directory names from contaminating root-cause totals.

## Crash and hang review

All 31 crashes are native stack overflows in ES6 proper-tail-call tests. They
span call, comma, conditional, logical, tagged-template, and statement tail
positions. The subprocess runner isolates each test and records it as
`stack-or-control-flow`; the parent completes and writes all neighboring
results.

The single hang is
`built-ins/Promise/all/invoke-resolve-error-close.js`. A parent-enforced
35-second deadline terminates and recursively isolates the batch, then records
the individual test with outcome and failure class `hang`.

No tests were skipped as unsupported. The remaining failures are therefore
test-specific implementation work, not broad exclusions or unexplained runner
omissions.

## Phase 2 priorities

The largest actionable groups support the roadmap order:

1. Parser/compiler failures and stack-balance defects.
2. Shared call, construct, property, conversion, and iterator operations.
3. Exception propagation through coercion and property hooks.
4. Missing intrinsics and descriptor correctness.
5. Promise job behavior, followed separately by proper-tail-call execution.
