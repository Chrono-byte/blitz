# Async ECMAScript compatibility

Measured on 2026-09-05 against engine baseline `20e1e38` and Test262
`f476a3e7b9659f68d04627d3df3a0492a7f9eb00`. These are selected suites, not an
engine-wide conformance percentage. Every selected file runs both variants
required by its Test262 flags; the report counts files.

| Suite | Files | Baseline passes | Current passes |
| --- | ---: | ---: | ---: |
| AsyncGeneratorFunction | 23 | 13 | 22 |
| AsyncGeneratorPrototype | 48 | 10 | 48 |
| Async generator expressions | 623 | 200 | 618 |
| Async generator statements | 301 | 87 | 299 |
| `for await` | 1,234 | 646 | 1,234 |
| Module code | 599 | 280 | 522 |
| **Compatibility total** | **2,828** | **1,236** | **2,743** |
| Promise and ordinary async regression suites | 918 | 855 | 870 |

The compatibility manifest gained 1,507 passing files, with no previously
passing file regressing. Its four baseline hangs now pass. The current run has
85 failures, no skips, no crashes, and no hangs. The separate regression
manifest gained 15 passing files with no regressions, crashes, or hangs.

Async generators now queue Promise-returning `next`, `throw`, and `return`
requests, retain their activation across awaits, and run cleanup on abrupt
completion. Async delegation preserves native iterator result values and
unwraps synchronous iterator values through an Async-from-Sync adapter.
These paths follow the distinct [AsyncGenerator operations](https://tc39.es/ecma262/multipage/control-abstraction-objects.html#sec-asyncgenerator-abstract-operations).

Module evaluation now keeps evaluation promises, schedules dependencies and
cycles, preserves rejection identity, and delays dynamic import and
ShadowRealm import settlement until evaluation finishes. Root-await metadata
survives optimization and bytecode serialization. Ordinary bundle version 5
still reads version 4; prelinked bundle version 7 still reads versions 2–6.
Older bundles default missing root-await metadata to false, preserving their
previous behavior.

Shared Promise fixes preserve constructor lookup, thenable resolution,
resolving-function metadata and realms, and first-resolution wins. Collection
is deferred while a native job executes because its closure captures are not
traced. Completed async generators release their activation; pending requests
retain GC protection. HTTP host APIs now create Promises through the VM so
they have the proper intrinsic prototype.

The remaining compatibility failures are pre-existing: 77 module tests and
eight async-generator early-error tests. Module gaps include instantiation and
export resolution, early errors, and newer import forms. This work does not
claim complete async-generator syntax or module conformance.

Reproduce the runs with the checked-in manifests:

```sh
cargo run --quiet --locked -p blitz-test262 --bin test262 -- \
  --manifest docs/test262/async-compatibility-manifest.txt \
  --summary-json target/test262/async-compatibility.json \
  --workers 6 --batch-size 1 --subprocess-timeout 35 --quiet
cargo run --quiet --locked -p blitz-test262 --bin test262 -- \
  --manifest docs/test262/async-regression-manifest.txt \
  --summary-json target/test262/async-regression.json \
  --workers 6 --batch-size 1 --subprocess-timeout 35 --quiet
```

Both commands exit nonzero while known failures remain. SHA-256 manifests:

- Compatibility: `65172959ec9aeb69c110d1bad730a843fd7d5f7f2a9ed8d3a1a009736eff257d`
- Regression: `cb73040f837ef897ccb960267a3175b4491174ca7605ae20c9aa230160898afd`

Validation also includes 569 passing engine unit tests, the CLI top-level
await example, workspace Clippy with all targets, formatting, and diff checks.
The full workspace test run retains its baseline failures: 12 runtime tests,
one checker negative fixture, and the embedded-source audit. No new audit
violations were introduced.
