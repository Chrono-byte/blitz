# Blitz CLI

`blitz-cli` owns the user-facing `blitz` executable and routes the `run`,
`check`, and `analyze` commands. With no command or file, it launches the
runtime-owned Blitz REPL. Package management, dependency resolution, bundling,
and release workflows are future toolchain work, not implemented commands in
this crate today.

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
```
