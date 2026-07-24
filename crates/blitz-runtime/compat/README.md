# Deno compatibility fixtures

`deno-surface.json` records the intentional `Blitz.*` compatibility surface.
Statuses are `compatible`, `partial`, `extension`, or `planned`.

Fixtures define environment-neutral observation functions. Runtime tests invoke
them with `Blitz`; the current reference is Deno 2.9.3 and the same fixture can
be checked against it, for example:

```sh
deno eval "$(cat crates/blitz-runtime/compat/fixtures/server-lifecycle.js); console.log(await observeServerLifecycle(Deno))"
```

Normalize platform-selected values such as ports before comparing observations.
A member must not be marked `compatible` until its normalized behavior, failure
timing, cancellation, and lifecycle agree with the pinned Deno fixture.
