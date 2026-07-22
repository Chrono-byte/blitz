//! Metadata and small runtime facades for the pinned Effect module graph.
//!
//! Upstream Effect files remain generated assets owned by the asset-generation
//! workspace. The application facade is kept here because it is Blitz-owned
//! source, while registration remains an embedder/runtime integration hook.

use oxc_span::SourceType;

/// One generated Effect module embedded in the runtime binary.
#[derive(Clone, Copy, Debug)]
pub struct EmbeddedEffectModule {
  /// Canonical public or private virtual-module specifier.
  pub specifier: &'static str,
  /// Generated upstream ESM source.
  pub source: &'static str,
  /// Curated declaration source for supported public entries only.
  pub declaration: Option<&'static str>,
  /// Generator-recorded source SHA-256.
  pub source_sha256: &'static str,
  /// Generator-recorded declaration SHA-256 for a public entry.
  pub declaration_sha256: Option<&'static str>,
}

include!(concat!(
  env!("CARGO_MANIFEST_DIR"),
  "/assets/effect/3.16.7/embedded.rs"
));

/// The exact upstream Effect version supported by the first application
/// runtime release.
pub const EFFECT_VERSION: &str = "3.16.7";

/// The origin of a virtual module's JavaScript and declaration assets.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AssetOrigin {
  /// An asset generated from the pinned upstream `effect` package.
  UpstreamEffect,
  /// A small Blitz declaration/runtime facade over a supported upstream API.
  BlitzFacade,
}

/// The kind of virtual module exposed by the runtime contract.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VirtualBuiltinKind {
  /// The package root: `effect`.
  EffectRoot,
  /// A supported upstream Effect deep import.
  EffectSubpath,
  /// The supported Blitz facade for Effect's `Match` API.
  BlitzMatchFacade,
  /// Immutable application description and scoped Effect facade.
  BlitzAppFacade,
  /// Private host bridge consumed by the application facade.
  BlitzAppTransport,
}

/// Description of one generated virtual-module asset.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GeneratedAsset {
  /// Canonical ESM specifier presented to the module resolver.
  pub specifier: &'static str,
  /// Where the generated JavaScript asset comes from.
  pub origin: AssetOrigin,
  /// Classification used by the virtual-module installer.
  pub kind: VirtualBuiltinKind,
  /// Stable logical identifier for the generated JavaScript artifact.
  pub source_asset: &'static str,
  /// Stable logical identifier for the generated declaration artifact.
  pub declaration_asset: &'static str,
}

/// The runtime-supported virtual modules for Effect 3.16.7.
///
/// The asset identifiers are intentionally logical rather than filesystem
/// paths. A generator may store or package the artifacts differently while
/// keeping this resolver-facing metadata stable.
pub const SUPPORTED_VIRTUAL_BUILTINS: &[GeneratedAsset] = &[
  GeneratedAsset {
    specifier: "effect",
    origin: AssetOrigin::UpstreamEffect,
    kind: VirtualBuiltinKind::EffectRoot,
    source_asset: "effect@3.16.7/index.js",
    declaration_asset: "effect@3.16.7/index.d.ts",
  },
  GeneratedAsset {
    specifier: "effect/Effect",
    origin: AssetOrigin::UpstreamEffect,
    kind: VirtualBuiltinKind::EffectSubpath,
    source_asset: "effect@3.16.7/Effect.js",
    declaration_asset: "effect@3.16.7/Effect.d.ts",
  },
  GeneratedAsset {
    specifier: "effect/Context",
    origin: AssetOrigin::UpstreamEffect,
    kind: VirtualBuiltinKind::EffectSubpath,
    source_asset: "effect@3.16.7/Context.js",
    declaration_asset: "effect@3.16.7/Context.d.ts",
  },
  GeneratedAsset {
    specifier: "effect/Layer",
    origin: AssetOrigin::UpstreamEffect,
    kind: VirtualBuiltinKind::EffectSubpath,
    source_asset: "effect@3.16.7/Layer.js",
    declaration_asset: "effect@3.16.7/Layer.d.ts",
  },
  GeneratedAsset {
    specifier: "effect/Stream",
    origin: AssetOrigin::UpstreamEffect,
    kind: VirtualBuiltinKind::EffectSubpath,
    source_asset: "effect@3.16.7/Stream.js",
    declaration_asset: "effect@3.16.7/Stream.d.ts",
  },
  GeneratedAsset {
    specifier: "effect/Scope",
    origin: AssetOrigin::UpstreamEffect,
    kind: VirtualBuiltinKind::EffectSubpath,
    source_asset: "effect@3.16.7/Scope.js",
    declaration_asset: "effect@3.16.7/Scope.d.ts",
  },
  GeneratedAsset {
    specifier: "effect/Schedule",
    origin: AssetOrigin::UpstreamEffect,
    kind: VirtualBuiltinKind::EffectSubpath,
    source_asset: "effect@3.16.7/Schedule.js",
    declaration_asset: "effect@3.16.7/Schedule.d.ts",
  },
  GeneratedAsset {
    specifier: "effect/Match",
    origin: AssetOrigin::UpstreamEffect,
    kind: VirtualBuiltinKind::EffectSubpath,
    source_asset: "effect@3.16.7/Match.js",
    declaration_asset: "effect@3.16.7/Match.d.ts",
  },
  GeneratedAsset {
    specifier: "blitz:match",
    origin: AssetOrigin::BlitzFacade,
    kind: VirtualBuiltinKind::BlitzMatchFacade,
    source_asset: "blitz:match.js",
    declaration_asset: "blitz:match.d.ts",
  },
  GeneratedAsset {
    specifier: "blitz:app",
    origin: AssetOrigin::BlitzFacade,
    kind: VirtualBuiltinKind::BlitzAppFacade,
    source_asset: "blitz:app.js",
    declaration_asset: "blitz:app.d.ts",
  },
  GeneratedAsset {
    specifier: "blitz-internal:app-transport",
    origin: AssetOrigin::BlitzFacade,
    kind: VirtualBuiltinKind::BlitzAppTransport,
    source_asset: "blitz-internal:app-transport.js",
    declaration_asset: "blitz-internal:app-transport.d.ts",
  },
];

/// The runtime source of the `blitz:match` facade.
///
/// Keeping the facade as a re-export preserves upstream Effect runtime
/// behavior; the declaration asset can curate the supported type surface
/// independently.
pub const BLITZ_MATCH_FACADE_SOURCE: &str = "export * from 'effect/Match';\n";

/// Runtime source for the immutable, Effect-backed application facade.
///
/// The bridge is looked up only when the acquired Effect runs. Consequently,
/// importing this module and constructing an application are both free of
/// host I/O. The host integration supplies `globalThis.__blitz_app_transport`
/// with the private bridge methods declared below.
pub const BLITZ_APP_SOURCE: &str = r#"import * as Effect from 'effect/Effect';
import * as Layer from 'effect/Layer';
import * as Transport from 'blitz-internal:app-transport';

const buildLayers = (layers) => layers.length === 0
  ? Effect.succeed(undefined)
  : Layer.build(Layer.mergeAll(...layers));

const start = (app, options) => Effect.flatMap(
  buildLayers(app.layers),
  () => Transport.openAppServer({
    ...options,
    rpc: app.rpc,
    layers: app.layers
  })
);

const makeDescription = (rpc, layers) => {
  const description = {
    rpc,
    layers,
    provide(layer) {
      return makeDescription(this.rpc, Object.freeze(this.layers.concat([layer])));
    },
    serve(options = {}) {
      const transportOptions = Object.freeze({
        hostname: options.hostname === undefined ? '127.0.0.1' : options.hostname,
        port: options.port === undefined ? 8000 : options.port,
        rpcPath: options.rpcPath === undefined ? '/rpc' : options.rpcPath
      });
      return Effect.scoped(
        Effect.acquireRelease(
          start(this, transportOptions),
          (server) => Transport.closeAppServer(server)
        ).pipe(Effect.flatMap((server) => Transport.awaitShutdown(server)))
      );
    }
  };
  return Object.freeze(description);
};

export const App = Object.freeze({
  make({ rpc }) {
    return makeDescription(rpc, Object.freeze([]));
  },
  provide(layer) {
    return (app) => app.provide(layer);
  }
});
"#;

/// Curated declaration source for `blitz:app`.
pub const BLITZ_APP_DECLARATIONS: &str = r#"import type * as Effect from 'effect/Effect';
import type * as Layer from 'effect/Layer';

export interface Router {
  readonly [key: string]: unknown;
}

export interface ServeOptions {
  readonly hostname?: string;
  readonly port?: number;
  readonly rpcPath?: string;
}

export interface ServeError {
  readonly _tag: 'ServeError';
  readonly reason: 'InvalidOptions' | 'Bind' | 'Transport';
  readonly message: string;
}

export type RequirementReduction<Provided, Required> = Exclude<Required, Provided>;

export interface AppDescription<Services = unknown> {
  readonly rpc: Router;
  readonly layers: ReadonlyArray<Layer.Layer<Services, unknown, unknown>>;
  readonly provide<Provided>(
    layer: Layer.Layer<Services & Provided, unknown, Provided>
  ): AppDescription<Services & Provided>;
  readonly serve(
    options?: ServeOptions
  ): Effect.Effect<never, ServeError, Services>;
}

export declare const App: {
  readonly make<Services = unknown>(input: {
    readonly rpc: Router;
  }): AppDescription<Services>;
  readonly provide<Provided, Services>(
    layer: Layer.Layer<Services & Provided, unknown, Provided>
  ): (app: AppDescription<Services>) => AppDescription<Services & Provided>;
};
"#;

/// Private bridge source. Host methods return upstream Effects and are looked
/// up lazily so the facade can be imported before host installation.
pub const BLITZ_APP_TRANSPORT_SOURCE: &str = r#"const bridge = () => {
  const value = globalThis.__blitz_app_transport;
  if (value === undefined) {
    throw new Error('blitz:app transport bridge is not installed');
  }
  return value;
};

export const openAppServer = (options) => bridge().openAppServer(options);
export const closeAppServer = (server) => bridge().closeAppServer(server);
export const awaitShutdown = (server) => bridge().awaitShutdown(server);
"#;

/// Declaration source for the private bridge. It is not part of the public
/// application API and deliberately exposes no `any` type.
pub const BLITZ_APP_TRANSPORT_DECLARATIONS: &str = r#"import type * as Effect from 'effect/Effect';

export interface AppServer {
  readonly localAddr: string;
}

export interface AppTransportOptions {
  readonly hostname: string;
  readonly port: number;
  readonly rpcPath: string;
  readonly rpc: unknown;
  readonly layers: ReadonlyArray<unknown>;
}

export declare const openAppServer: (
  options: AppTransportOptions
) => Effect.Effect<AppServer, unknown, unknown>;
export declare const closeAppServer: (
  server: AppServer
) => Effect.Effect<void, never, never>;
export declare const awaitShutdown: (
  server: AppServer
) => Effect.Effect<never, never, never>;
"#;

/// The declarations intentionally supported by the first facade milestone.
pub const BLITZ_MATCH_SUPPORTED_EXPORTS: &[&str] =
  &["value", "type", "valueTags", "typeTags", "exhaustive"];

/// Build the complete generated upstream Effect virtual-module graph.
///
/// # Errors
///
/// Returns a bundler error if generated assets contain an invalid or duplicate
/// canonical specifier. The deterministic asset verifier checks this invariant.
pub fn effect_virtual_modules() -> Result<blitz::VirtualModuleRegistry, blitz::BundlerError> {
  let mut modules = blitz::VirtualModuleRegistry::new();
  for asset in EMBEDDED_EFFECT_MODULES {
    modules.register(blitz::VirtualModule::javascript(
      asset.specifier,
      asset.source,
      SourceType::mjs(),
      asset.declaration.map(str::to_string),
    ))?;
  }
  Ok(modules)
}

/// Install generated Effect and Blitz application facades into a VM.
///
/// # Errors
///
/// Returns a bundler error if an embedded module is invalid or duplicates an
/// already registered embedder module.
pub fn install_runtime_modules(vm: &blitz::Vm) -> Result<(), blitz::BundlerError> {
  vm.register_virtual_modules(effect_virtual_modules()?)?;
  vm.register_virtual_modules(application_virtual_modules()?)?;
  Ok(())
}

/// Build the Blitz-owned virtual modules required by `blitz:app`.
///
/// This intentionally does not include generated upstream Effect assets. The
/// runtime integration must install those assets first (or in the same module
/// registry) and then register this returned registry before constructing user
/// modules.
///
/// # Errors
///
/// Returns a bundler error if one of the fixed facade specifiers is duplicated
/// while constructing the registry.
pub fn application_virtual_modules() -> Result<blitz::VirtualModuleRegistry, blitz::BundlerError> {
  let mut modules = blitz::VirtualModuleRegistry::new();
  modules.register(blitz::VirtualModule::javascript(
    "blitz:match",
    BLITZ_MATCH_FACADE_SOURCE,
    SourceType::mjs(),
    None,
  ))?;
  modules.register(blitz::VirtualModule::javascript(
    "blitz:app",
    BLITZ_APP_SOURCE,
    SourceType::mjs(),
    Some(BLITZ_APP_DECLARATIONS.to_string()),
  ))?;
  modules.register(blitz::VirtualModule::javascript(
    "blitz-internal:app-transport",
    BLITZ_APP_TRANSPORT_SOURCE,
    SourceType::mjs(),
    Some(BLITZ_APP_TRANSPORT_DECLARATIONS.to_string()),
  ))?;
  Ok(modules)
}

/// Read-only lookup over the pinned virtual-module manifest.
#[derive(Clone, Copy, Debug)]
pub struct VirtualBuiltinRegistry {
  assets: &'static [GeneratedAsset],
}

impl Default for VirtualBuiltinRegistry {
  fn default() -> Self {
    Self::new()
  }
}

impl VirtualBuiltinRegistry {
  /// Construct the registry for the supported Effect release.
  #[must_use]
  pub const fn new() -> Self {
    Self {
      assets: SUPPORTED_VIRTUAL_BUILTINS,
    }
  }

  /// Return the pinned upstream package version.
  #[must_use]
  pub const fn effect_version(self) -> &'static str {
    EFFECT_VERSION
  }

  /// Return all supported virtual built-ins in stable manifest order.
  #[must_use]
  pub const fn assets(self) -> &'static [GeneratedAsset] {
    self.assets
  }

  /// Find metadata for a canonical virtual-module specifier.
  #[must_use]
  pub fn get(
    self,
    specifier: &str,
  ) -> Option<&'static GeneratedAsset> {
    self
      .assets
      .iter()
      .find(|asset| asset.specifier == specifier)
  }

  /// Whether `specifier` is part of the supported virtual-module contract.
  #[must_use]
  pub fn contains(
    self,
    specifier: &str,
  ) -> bool {
    self.get(specifier).is_some()
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn manifest_is_pinned_and_has_unique_specifiers() {
    let registry = VirtualBuiltinRegistry::new();

    assert_eq!(registry.effect_version(), "3.16.7");
    assert_eq!(registry.assets().len(), 11);
    for (index, asset) in registry.assets().iter().enumerate() {
      assert!(!asset.source_asset.is_empty());
      assert!(!asset.declaration_asset.is_empty());
      assert!(
        !registry.assets()[..index]
          .iter()
          .any(|previous| previous.specifier == asset.specifier)
      );
    }
  }

  #[test]
  fn lookup_accepts_only_supported_canonical_specifiers() {
    let registry = VirtualBuiltinRegistry::new();

    assert_eq!(
      registry
        .get("effect/Match")
        .map(|asset| asset.kind),
      Some(VirtualBuiltinKind::EffectSubpath)
    );
    assert!(registry.contains("effect"));
    assert!(registry.contains("blitz:match"));
    assert_eq!(
      registry.get("blitz:app").map(|asset| asset.kind),
      Some(VirtualBuiltinKind::BlitzAppFacade)
    );
    assert_eq!(
      registry
        .get("blitz-internal:app-transport")
        .map(|asset| asset.kind),
      Some(VirtualBuiltinKind::BlitzAppTransport)
    );
    assert!(!registry.contains("effect/match"));
    assert!(!registry.contains("blitz:match/extra"));
  }

  #[test]
  fn app_facade_is_lazy_immutable_and_scoped() {
    assert!(BLITZ_APP_SOURCE.contains("Object.freeze"));
    assert!(BLITZ_APP_SOURCE.contains("provide(layer)"));
    assert!(BLITZ_APP_SOURCE.contains("Effect.acquireRelease"));
    assert!(BLITZ_APP_SOURCE.contains("Effect.scoped"));
    assert!(BLITZ_APP_SOURCE.contains("Transport.awaitShutdown"));
    assert!(!BLITZ_APP_DECLARATIONS.contains(" any"));

    let _session = blitz::Parser::parse_source_with_type(BLITZ_APP_SOURCE, SourceType::mjs())
      .expect("blitz:app source parses as an ESM module");
    let modules = application_virtual_modules().expect("facade modules register");
    assert_eq!(modules.len(), 3);
    assert!(modules.get("blitz:match").is_some());
    assert!(modules.get("blitz:app").is_some());
    assert!(
      modules
        .get("blitz-internal:app-transport")
        .is_some()
    );
  }

  #[test]
  fn embedded_effect_graph_contains_public_entries_and_declarations() {
    assert!(EMBEDDED_EFFECT_MODULES.len() > 8);
    for specifier in ["effect", "effect/Effect", "effect/Layer", "effect/Match"] {
      let asset = EMBEDDED_EFFECT_MODULES
        .iter()
        .find(|asset| asset.specifier == specifier)
        .expect("public Effect entry is embedded");
      assert!(!asset.source.is_empty());
      assert!(
        asset
          .declaration
          .is_some_and(|source| !source.is_empty())
      );
      assert_eq!(asset.source_sha256.len(), 64);
      assert!(
        asset
          .declaration_sha256
          .is_some_and(|hash| hash.len() == 64)
      );
    }
    assert_eq!(
      effect_virtual_modules()
        .expect("generated graph registers")
        .len(),
      EMBEDDED_EFFECT_MODULES.len()
    );
  }

  #[test]
  fn match_facade_reexports_upstream_runtime_and_curates_exports() {
    let facade = VirtualBuiltinRegistry::new()
      .get("blitz:match")
      .expect("manifest must contain blitz:match");

    assert_eq!(facade.origin, AssetOrigin::BlitzFacade);
    assert_eq!(facade.kind, VirtualBuiltinKind::BlitzMatchFacade);
    assert_eq!(BLITZ_MATCH_FACADE_SOURCE, "export * from 'effect/Match';\n");
    assert!(BLITZ_MATCH_SUPPORTED_EXPORTS.contains(&"exhaustive"));
  }
}
