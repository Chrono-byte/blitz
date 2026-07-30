# Blitz CLI

`blitz-cli` owns the user-facing `blitz` executable and routes the `run`,
`check`, `analyze`, and `precompile` commands. With no command or file, it
launches the runtime-owned Blitz REPL. Package management and release workflows
remain future toolchain work.

Runtime capabilities and the hosted REPL belong in `blitz-runtime`;
ECMAScript semantics belong in `denko-engine`.

The reviewed Bun-to-Blitz command and flag mapping is maintained in
[`docs/CLI_SURFACE.md`](../../docs/CLI_SURFACE.md).

```sh
blitz -e 'console.log("hello")'
blitz -p 'const value: number = 21; value * 2'
blitz -r ./setup.ts -r ./tracing.ts ./app.ts
blitz repl --preload ./setup.ts
blitz --cwd ./project --conditions development ./src/app.ts
blitz --env-file .env.production ./src/app.ts
blitz run start -- --port 3000
blitz run --if-present optional-script
blitz precompile project ./src/main.ts --output ./dist/app.blitzbundle
blitz precompile project ./src/main.ts --no-runtime
blitz precompile runtime --check
```

`precompile project` resolves the entry's complete static module closure,
compiles and links it to Denko bytecode, and emits a deterministic prelinked
bundle. By default the bundle includes Blitz's embedded Effect, stdlib, and
Node-compatible graph so `effect/*`, `blitz:*`, and `node:*` imports work.
`--no-runtime` emits a smaller host-neutral graph for projects using only
filesystem and package modules.
