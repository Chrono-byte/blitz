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

These files intentionally retain explicit type syntax for parser and type-checking work. The
application runtime does not currently execute them directly:

- `typed.ts` — basic TypeScript annotations.
- `assignment_type_error.ts` — an incompatible typed assignment.
- `implicit_any_error.ts` — an untyped parameter under strict TypeScript checks.
- `return_type_error.ts` — a function returning the wrong type.

## Intentional errors

These examples are expected to fail and exercise diagnostics:

- `const_assignment_error.js` — assignment to a constant.
- `strict_undeclared_error.js` — assignment to an undeclared strict-mode binding.
