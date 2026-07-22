# Runtime examples

Run a JavaScript example through the application runtime:

```sh
cargo run -p blitz-cli -- crates/blitz-runtime/examples/hello.js
```

## Runnable examples

- `hello.js` — minimal console output.
- `console.js` — host-backed console methods and counters.
- `runtime_process.js` — runtime version, platform, working directory, and arguments.
- `simple_demo.js` — functions, objects, iteration, arrays, and error handling.

## TypeScript fixtures

TypeScript files execute directly with annotations erased by OXC:

```sh
cargo run -p blitz-cli -- run crates/blitz-runtime/examples/typed.ts
```

They can also exercise the in-process checker:

```sh
cargo run -p blitz-cli -- check crates/blitz-runtime/examples/typed.ts
cargo run -p blitz-cli -- check --strict crates/blitz-runtime/examples/implicit_any_error.ts
```

- `typed.ts` — basic TypeScript annotations.
- `assignment_type_error.ts` — an incompatible typed assignment.
- `implicit_any_error.ts` — an untyped parameter; accepted normally and rejected by `--strict`.
- `return_type_error.ts` — a function returning the wrong type.

## Intentional errors

These examples are expected to fail and exercise diagnostics:

- `const_assignment_error.js` — assignment to a constant.
- `strict_undeclared_error.js` — assignment to an undeclared strict-mode binding.
