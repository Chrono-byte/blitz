# Effect virtual-module manifest

`blitz-runtime::effect_assets` is the small, host-side manifest for the first
application-runtime Effect surface. It pins the generated assets to
`effect@3.16.7` and keeps the engine unaware of Effect-specific specifiers.

The manifest currently supports:

| Specifier | Origin |
| --- | --- |
| `effect` | Effect 3.16.7 root asset |
| `effect/Effect` | Effect 3.16.7 generated subpath |
| `effect/Context` | Effect 3.16.7 generated subpath |
| `effect/Layer` | Effect 3.16.7 generated subpath |
| `effect/Stream` | Effect 3.16.7 generated subpath |
| `effect/Scope` | Effect 3.16.7 generated subpath |
| `effect/Schedule` | Effect 3.16.7 generated subpath |
| `effect/Match` | Effect 3.16.7 generated subpath |
| `blitz:match` | Blitz facade over `effect/Match` |

`VirtualBuiltinRegistry::get` performs exact, case-sensitive lookup. It does
not normalize or fall back to arbitrary `effect/*` imports, so adding a new
supported subpath is an explicit manifest change.

## Asset contract

The pnpm workspace at `tools/effect-assets/` downloads the exact package only
during explicit generation. It commits a canonical ESM graph, Effect's MIT
license, package integrity, deterministic SHA-256 manifest, and curated
public declarations under `crates/blitz-runtime/assets/effect/3.16.7/`.
Private runtime modules intentionally have no declaration assets; Blitz does
not vendor Effect's complete upstream declaration graph.

`blitz:match` is represented separately as `AssetOrigin::BlitzFacade`. Its
runtime source is the literal re-export of upstream `effect/Match`, while
`BLITZ_MATCH_SUPPORTED_EXPORTS` identifies the initial curated declaration
surface (`value`, `type`, `valueTags`, `typeTags`, and `exhaustive`). This keeps
runtime Match behavior upstream-owned and leaves type-surface expansion
reviewable.

`install_runtime` embeds and registers the generated graph plus the
`blitz:match` and `blitz:app` facades. The raw engine does not install these
modules, preserving the engine/runtime boundary.

Asset generation, registration, construction, and compilation are landed.
Executing the Effect smoke graph currently reaches a remaining engine closure
capture error (`Bad upvalue index`); scheduler and `App.serve` transport work
must not treat the compatibility milestone as complete until that engine bug
is fixed.
