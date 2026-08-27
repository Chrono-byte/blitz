# Node-style application development

Blitz should let a developer enter an existing JavaScript project through the
same small surface they expect from Node:

```text
blitz app.js
blitz run start -- --port 3000
```

That familiarity is an application-runtime concern. It must not weaken Denko's
ECMAScript semantics or turn the CLI into a second module system.

## Architecture

`blitz-cli` parses commands and presents results. It submits an
`ApplicationRequest` to `HostedApplication`, which owns the complete hosted
lifecycle: preloads, entry preparation, evaluation, resource draining, and the
exit result.

`ProjectLoader` is the first runtime-owned project boundary. It produces one
canonical entry identity and keeps syntax media type separate from evaluation
format. Package scope, explicit extensions, and TypeScript module syntax are
therefore interpreted once instead of independently by the CLI and runtime.

Unsupported formats are represented honestly. A `.cjs` entry or a project with
`"type": "commonjs"` is classified as CommonJS and rejected before parsing;
JSON receives the same treatment. Neither is silently executed with classic or
ESM semantics.

## Why this shape

Three designs were considered:

- grow CLI orchestration around the existing session API;
- build a project/module graph abstraction first;
- organize compatibility as independently selectable host capabilities.

The application lifecycle is the useful base because every CLI mode and future
embedder needs it. The project-loader types come forward from the graph design
because format and identity must have one owner. A compatibility catalog remains
valuable, but it belongs behind this application boundary after the developer
workflow is coherent.

This initial slice deliberately classifies only the application entry. Static
dependency loading still passes through Denko's current module construction
path. Moving every file dependency through the same `ProjectLoader` contract is
the next architectural step; implementing CommonJS before that seam exists
would bake another temporary loader into the system.

## Next slices

1. Route filesystem dependencies through runtime-owned resolved-module records.
2. Add JSON modules and CommonJS as real formats, including interop rules and
   cache identity.
3. Expand `process`, signals, timers, streams, and outbound networking behind an
   explicit compatibility capability catalog.
4. Add watch and test workflows on top of `HostedApplication`, without teaching
   the engine about command-line concerns.
