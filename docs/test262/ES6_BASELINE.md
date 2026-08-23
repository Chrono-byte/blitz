# ES6 Test262 baseline

Reviewed on 2026-08-23 against Test262 commit
`f2d1435644797268dca1f7988cad5a4e89ccd8d2`.

The stable `es6-manifest.txt` selection contains 9,294 tests. Regenerating the
manifest and loading it through `--manifest --write-manifest` produced the same
SHA-256 digest:
`88af8ac5b23184b2888cb197df9a1dcb4b77660f77854f44b0bfd80854aa1c63`.

The reviewed run used:

```powershell
cargo run -p blitz-test262 --bin test262 -- --manifest docs/test262/es6-manifest.txt --summary-json target/test262/es6-summary-current.json --workers 4 --batch-size 20 --subprocess-timeout 35 --quiet
```

## Outcomes

| Outcome | Count |
| --- | ---: |
| Pass | 7,746 |
| Fail | 1,548 |
| Crash | 0 |
| Hang | 0 |
| Unsupported/skip | 0 |
| Total | 9,294 |

The overall pass rate is 83.34%.

## Categories

Category tags overlap. In particular, module, async, and negative tests can
also be normative.

| Category | Total | Pass | Fail | Crash | Hang | Pass rate |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Normative | 9,245 | 7,732 | 1,513 | 0 | 0 | 83.63% |
| Module | 52 | 40 | 12 | 0 | 0 | 76.92% |
| Async | 83 | 69 | 14 | 0 | 0 | 83.13% |
| Negative parse | 741 | 627 | 114 | 0 | 0 | 84.62% |
| Negative runtime | 1 | 1 | 0 | 0 | 0 | 100.00% |
| Annex B | 33 | 4 | 29 | 0 | 0 | 12.12% |
| Staging | 16 | 10 | 6 | 0 | 0 | 62.50% |

## Failure classes

| Root cause | Count |
| --- | ---: |
| Parser or compiler | 187 |
| Assertion or semantics | 972 |
| Missing abstract operation | 54 |
| Missing intrinsic | 109 |
| Exception propagation | 168 |
| Incorrect descriptor | 46 |
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

The largest actionable groups support the roadmap order:

1. TypedArray methods and integer-indexed exotic object semantics.
2. Destructuring in classes, generators, assignments, and `for-of` loops.
3. Generator delegation and iterator-closing behavior.
4. Proxy invariants, exception propagation, and missing intrinsics.
5. Remaining parser/compiler early errors, Promise jobs, modules, and Annex B.
