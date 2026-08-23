# Denko

Denko is a host-neutral ECMAScript engine written in Rust. It owns parsing,
bytecode compilation, execution, garbage collection, built-ins, and language
semantics. Denko powers the Blitz application runtime but can be embedded or
run independently without Blitz host APIs.

The name comes from the romanized Japanese **denkō** (電光), meaning “lightning flash.”

## Raw engine harness

```sh
cargo run -p denko-engine --bin denko -- path/to/file.js
```

The harness intentionally excludes operating-system APIs such as `console`,
`process`, filesystem access, and networking. Those capabilities belong to the
Blitz runtime.

## Conformance

Denko uses the repository's dedicated Test262 runner:

```sh
cargo run -p blitz-test262 --bin test262
```
