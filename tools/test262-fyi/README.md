# Blitz adapter for test262.fyi data

These files plug Blitz into the `test262-fyi/data` runner while retaining
Blitz's native, batched Test262 execution path.

Set `BLITZ_ROOT` when the two repositories are not sibling directories. From
the test262-fyi working directory (the directory containing `test262/`), run:

```powershell
$env:BLITZ_ROOT = 'C:\Users\Ellie\Documents\blitz'
node ..\engines\blitz\suite.js --filter language\expressions\exponentiation --workers 2
```

Omit the filter for a full run. The result is written to
`results/blitz.json` using the same `{ passes, time, version }` shape consumed
by `controller/generate.js`.

The stock per-file runner can use `run.js` on POSIX systems:

```powershell
node ..\runner\index.js blitz
```

That path starts one Blitz CLI process per test and is substantially slower
than `suite.js`. The upstream reader currently assumes POSIX path separators,
so use `suite.js` on Windows.
