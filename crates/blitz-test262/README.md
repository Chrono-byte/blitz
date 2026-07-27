# Blitz Test262 runner

`blitz-test262` owns Test262 discovery, metadata parsing, harness execution,
subprocess isolation, result classification, and reporting for the Blitz
ECMAScript engine.

Run the workspace's vendored Test262 checkout with:

```sh
cargo run -p blitz-test262 --bin test262
```

Use `--help` for filters, edition selection, worker limits, manifests, and
report output options. The package depends on `denko-engine` as a conformance
consumer; it is not part of the engine's runtime surface.
