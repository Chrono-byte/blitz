# ES6 Test262 baseline

Reviewed on 2026-08-23 against Test262 commit
`f2d1435644797268dca1f7988cad5a4e89ccd8d2`.

The stable `es6-manifest.txt` selection contains 9,294 tests. Regenerating the
manifest and loading it through `--manifest --write-manifest` produced the same
SHA-256 digest:
`88af8ac5b23184b2888cb197df9a1dcb4b77660f77854f44b0bfd80854aa1c63`.

The reviewed run used:

```powershell
cargo run -p blitz-test262 --bin test262 -- --manifest docs/test262/es6-manifest.txt --summary-json target/test262/es6-summary-current.json --workers 4 --batch-size 20 --subprocess-timeout 150 --quiet
```

## Outcomes

| Outcome | Count |
| --- | ---: |
| Pass | 8,841 |
| Fail | 453 |
| Crash | 0 |
| Hang | 0 |
| Unsupported/skip | 0 |
| Total | 9,294 |

The overall pass rate is 95.13%, an improvement of 1,095 passing tests over
the previously documented 7,746-pass baseline.

## Categories

Category tags overlap. In particular, module, async, and negative tests can
also be normative.

| Category | Total | Pass | Fail | Crash | Hang | Pass rate |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Normative | 9,245 | 8,800 | 445 | 0 | 0 | 95.19% |
| Module | 52 | 42 | 10 | 0 | 0 | 80.77% |
| Async | 83 | 69 | 14 | 0 | 0 | 83.13% |
| Negative parse | 741 | 741 | 0 | 0 | 0 | 100.00% |
| Negative runtime | 1 | 1 | 0 | 0 | 0 | 100.00% |
| Annex B | 33 | 28 | 5 | 0 | 0 | 84.85% |
| Staging | 16 | 13 | 3 | 0 | 0 | 81.25% |

## Failure classes

| Root cause | Count |
| --- | ---: |
| Parser or compiler | 32 |
| Assertion or semantics | 250 |
| Missing abstract operation | 30 |
| Missing intrinsic | 37 |
| Exception propagation | 71 |
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

The largest actionable groups support the roadmap order:

1. Destructuring iterator completion and `IteratorClose` behavior in assignment
   and `for-of` paths.
2. RegExp protocol methods, especially `Symbol.split` and `Symbol.replace`.
3. Proxy invariants and exception propagation.
4. Direct and indirect eval environments plus generator parameter environments.
5. Function construction and source text, Promise keyed methods, modules, and
   remaining Annex B behavior.
