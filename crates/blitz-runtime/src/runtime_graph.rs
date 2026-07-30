// SPDX-License-Identifier: MPL-2.0
//! Runtime-owned contracts for the final Effect and Blitz stdlib graph.
//!
//! The runtime owns the versioned envelope around the engine's generic
//! prelinked graph codec. This keeps provenance, visibility, ABI, host policy,
//! and cross-validation at the runtime boundary while leaving graph execution
//! and bytecode validation to the engine.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::{Arc, OnceLock};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use denko::prelinked::PrelinkedModuleGraph;

/// Version of the runtime wrapper around an engine prelinked graph.
pub const RUNTIME_GRAPH_FORMAT_VERSION: u16 = 1;
/// ABI version required by the runtime graph artifact.
pub const RUNTIME_ABI_VERSION: u16 = 1;
/// ABI version implemented by the runtime's private host bridge.
pub const HOST_BRIDGE_ABI_VERSION: u16 = 1;
/// Effect release accepted by this runtime.
pub const EFFECT_VERSION: &str = "3.16.7";
/// Stable name of the transitional application transport primitive.
pub const APP_TRANSPORT_PRIMITIVE: &str = "app-transport";
/// Stable name of the Effect-native filesystem host primitive.
pub const FILE_SYSTEM_PRIMITIVE: &str = "file-system";
/// Stable name of the Effect-native HTTP host primitive.
pub const HTTP_PRIMITIVE: &str = "http";
/// Stable name of the Effect-native terminal host primitive.
pub const TERMINAL_PRIMITIVE: &str = "terminal";
/// Stable name of the Effect-native WebSocket host primitive.
pub const WEB_SOCKET_PRIMITIVE: &str = "websocket";
/// Stable name of the Effect root-control host primitive.
pub const RUNTIME_CONTROL_PRIMITIVE: &str = "runtime-control";
/// Package name used by a graph containing Effect and Blitz stdlib modules.
pub const RUNTIME_GRAPH_PACKAGE_NAME: &str = "effect+blitz-stdlib";
/// Package version used by the pinned combined runtime graph.
pub const RUNTIME_GRAPH_PACKAGE_VERSION: &str = "effect-3.16.7+runtime-1";
/// Version of the runtime bundle envelope.
pub const RUNTIME_BUNDLE_WIRE_VERSION: u16 = 1;
/// Methods required by the application transport adapter.
pub const APP_TRANSPORT_METHODS: &[&str] = &["openAppServer", "closeAppServer", "awaitShutdown"];
/// Methods required by the filesystem adapter.
pub const FILE_SYSTEM_METHODS: &[&str] =
  &["readFile", "readTextFile", "writeFile", "writeTextFile"];
/// Stable name of the direct Node filesystem host primitive.
pub const NODE_FILE_SYSTEM_PRIMITIVE: &str = "node-file-system";
/// Methods required by the direct Node filesystem adapter.
pub const NODE_FILE_SYSTEM_METHODS: &[&str] = &[
  "readFile",
  "writeFile",
  "stat",
  "mkdir",
  "readdir",
  "rm",
  "unlink",
  "rename",
  "exists",
];
/// Stable name of the version-one Node crypto random host primitive.
pub const NODE_CRYPTO_RANDOM_PRIMITIVE: &str = "node-crypto-random";
/// ABI version of the Node crypto random host primitive.
pub const NODE_CRYPTO_RANDOM_ABI_VERSION: u16 = 1;
/// Methods required by the initial Node crypto random adapter.
pub const NODE_CRYPTO_RANDOM_METHODS: &[&str] = &[
  "randomUUID",
  "randomBytes",
  "randomFill",
  "randomInt",
  "timingSafeEqual",
];
/// Methods required by the HTTP adapter.
pub const HTTP_METHODS: &[&str] = &["openServer", "closeServer", "awaitShutdown"];
/// Methods required by the terminal adapter.
pub const TERMINAL_METHODS: &[&str] = &[
  "write",
  "writeLine",
  "setPrompt",
  "subscribeLines",
  "close",
  "cancel",
];
/// Methods required by the WebSocket adapter.
pub const WEB_SOCKET_METHODS: &[&str] = &[
  "connect",
  "prepareUpgrade",
  "accept",
  "send",
  "close",
  "subscribe",
  "cancel",
];
/// Methods required by the Effect root-control adapter.
pub const RUNTIME_CONTROL_METHODS: &[&str] = &[
  "reserveRoot",
  "attachRoot",
  "interruptRoot",
  "completeRoot",
  "failRoot",
];

const RUNTIME_GRAPH_ARTIFACT: &[u8] = include_bytes!(concat!(
  env!("CARGO_MANIFEST_DIR"),
  "/assets/runtime/v1/runtime-graph.blitzpg"
));

/// Normalize text manifest line endings before content-addressing them.
///
/// Git may materialize text assets with CRLF on Windows. Runtime artifacts
/// must remain byte-for-byte reproducible across checkout platforms.
pub fn canonical_manifest_bytes(bytes: &[u8]) -> Cow<'_, [u8]> {
  if !bytes.contains(&b'\r') {
    return Cow::Borrowed(bytes);
  }

  let mut canonical = Vec::with_capacity(bytes.len());
  let mut index = 0;
  while index < bytes.len() {
    if bytes[index] == b'\r' {
      canonical.push(b'\n');
      index += usize::from(bytes.get(index + 1) == Some(&b'\n')) + 1;
    } else {
      canonical.push(bytes[index]);
      index += 1;
    }
  }
  Cow::Owned(canonical)
}

/// Compute a platform-independent digest for a checked-in text manifest.
pub fn manifest_sha256(bytes: &[u8]) -> [u8; 32] {
  Sha256::digest(canonical_manifest_bytes(bytes).as_ref()).into()
}

/// Visibility of a module in the runtime graph.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModuleVisibility {
  /// A module that may be resolved by user application modules.
  Public,
  /// A module used only by bundled runtime modules.
  Private,
}

/// Provenance of one runtime graph module.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModuleOrigin {
  /// A module from the pinned Effect package.
  Effect,
  /// A Blitz-owned stdlib facade or private adapter.
  BlitzStdlib,
}

/// One host capability required by a bundled module.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct HostPrimitiveRequirement {
  /// Stable host primitive name.
  pub name: String,
  /// ABI version of this primitive's method surface.
  pub abi_version: u16,
  /// Methods the bundled adapter may call.
  pub methods: Vec<String>,
}

impl HostPrimitiveRequirement {
  /// Describe one required host primitive.
  #[must_use]
  pub fn new<I, S>(
    name: impl Into<String>,
    abi_version: u16,
    methods: I,
  ) -> Self
  where
    I: IntoIterator<Item = S>,
    S: Into<String>,
  {
    Self {
      name: name.into(),
      abi_version,
      methods: methods.into_iter().map(Into::into).collect(),
    }
  }
}

/// Runtime metadata for one module in the final graph.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RuntimeModuleManifest {
  /// Canonical module specifier.
  pub specifier: String,
  /// Effect or Blitz stdlib provenance.
  pub origin: ModuleOrigin,
  /// Whether user resolution may expose this module.
  pub visibility: ModuleVisibility,
  /// SHA-256 of the source input used to generate the module.
  pub source_sha256: [u8; 32],
  /// SHA-256 of the public declaration input, when one exists.
  pub declaration_sha256: Option<[u8; 32]>,
  /// Ordered static module dependencies.
  pub dependencies: Vec<String>,
  /// Host capabilities used by this module's private adapter.
  pub required_host_primitives: Vec<HostPrimitiveRequirement>,
}

/// Provenance and compatibility metadata wrapped around one graph.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RuntimeGraphManifest {
  /// Version of this runtime wrapper contract.
  pub format_version: u16,
  /// Runtime ABI expected by the generated graph.
  pub runtime_abi_version: u16,
  /// Private host bridge ABI expected by the generated graph.
  pub host_bridge_abi_version: u16,
  /// Exact Effect package version used by generation.
  pub effect_version: String,
  /// Digest of the verified Effect asset manifest.
  pub effect_manifest_sha256: [u8; 32],
  /// Digest of the verified Blitz stdlib asset manifest.
  pub stdlib_manifest_sha256: [u8; 32],
  /// Modules in deterministic generator order.
  pub modules: Vec<RuntimeModuleManifest>,
  /// Modules exposed as user-facing runtime roots.
  pub public_roots: Vec<String>,
}

impl RuntimeGraphManifest {
  /// Validate runtime-owned invariants without requiring host state.
  pub fn validate(&self) -> Result<(), RuntimeGraphError> {
    if self.format_version != RUNTIME_GRAPH_FORMAT_VERSION {
      return Err(RuntimeGraphError::UnsupportedGraphFormat(
        self.format_version,
      ));
    }
    if self.runtime_abi_version != RUNTIME_ABI_VERSION {
      return Err(RuntimeGraphError::RuntimeAbiMismatch {
        expected: RUNTIME_ABI_VERSION,
        actual: self.runtime_abi_version,
      });
    }
    if self.host_bridge_abi_version != HOST_BRIDGE_ABI_VERSION {
      return Err(RuntimeGraphError::HostBridgeAbiMismatch {
        expected: HOST_BRIDGE_ABI_VERSION,
        actual: self.host_bridge_abi_version,
      });
    }
    if self.effect_version != EFFECT_VERSION {
      return Err(RuntimeGraphError::EffectVersionMismatch {
        expected: EFFECT_VERSION.to_string(),
        actual: self.effect_version.clone(),
      });
    }
    if self.modules.is_empty() {
      return Err(RuntimeGraphError::EmptyGraph);
    }

    let mut modules = BTreeMap::new();
    for module in &self.modules {
      validate_specifier(&module.specifier)?;
      validate_visibility(module)?;
      if modules
        .insert(module.specifier.as_str(), module)
        .is_some()
      {
        return Err(RuntimeGraphError::DuplicateModule(module.specifier.clone()));
      }

      let mut dependencies = BTreeSet::new();
      for dependency in &module.dependencies {
        validate_specifier(dependency)?;
        if !dependencies.insert(dependency.as_str()) {
          return Err(RuntimeGraphError::DuplicateDependency {
            module: module.specifier.clone(),
            dependency: dependency.clone(),
          });
        }
      }

      let mut primitives = BTreeSet::new();
      for primitive in &module.required_host_primitives {
        if primitive.name.is_empty() || primitive.name.contains(char::is_whitespace) {
          return Err(RuntimeGraphError::InvalidHostPrimitive(
            primitive.name.clone(),
          ));
        }
        if !primitives.insert(primitive.name.as_str()) {
          return Err(RuntimeGraphError::DuplicateHostPrimitive {
            module: module.specifier.clone(),
            primitive: primitive.name.clone(),
          });
        }
        let mut methods = BTreeSet::new();
        for method in &primitive.methods {
          if method.is_empty() || method.contains(char::is_whitespace) {
            return Err(RuntimeGraphError::InvalidHostMethod {
              primitive: primitive.name.clone(),
              method: method.clone(),
            });
          }
          if !methods.insert(method.as_str()) {
            return Err(RuntimeGraphError::DuplicateHostMethod {
              primitive: primitive.name.clone(),
              method: method.clone(),
            });
          }
        }
      }
    }

    for module in &self.modules {
      for dependency in &module.dependencies {
        if !modules.contains_key(dependency.as_str()) {
          return Err(RuntimeGraphError::MissingDependency {
            module: module.specifier.clone(),
            dependency: dependency.clone(),
          });
        }
      }
    }

    let mut roots = BTreeSet::new();
    for root in &self.public_roots {
      if !roots.insert(root.as_str()) {
        return Err(RuntimeGraphError::DuplicatePublicRoot(root.clone()));
      }
      let Some(module) = modules.get(root.as_str()) else {
        return Err(RuntimeGraphError::MissingPublicRoot(root.clone()));
      };
      if module.visibility != ModuleVisibility::Public {
        return Err(RuntimeGraphError::PrivatePublicRoot(root.clone()));
      }
    }
    if self.public_roots.is_empty() {
      return Err(RuntimeGraphError::NoPublicRoots);
    }
    Ok(())
  }

  /// Validate this manifest against the host-neutral engine graph.
  pub fn validate_against_graph(
    &self,
    graph: &PrelinkedModuleGraph,
  ) -> Result<(), RuntimeGraphError> {
    self.validate()?;
    graph
      .validate()
      .map_err(|error| RuntimeGraphError::InvalidEngineGraph(error.to_string()))?;
    if graph.package_name != RUNTIME_GRAPH_PACKAGE_NAME
      || graph.package_version != RUNTIME_GRAPH_PACKAGE_VERSION
    {
      return Err(RuntimeGraphError::PackageMetadataMismatch {
        expected: format!(
          "{}@{}",
          RUNTIME_GRAPH_PACKAGE_NAME, RUNTIME_GRAPH_PACKAGE_VERSION
        ),
        actual: format!("{}@{}", graph.package_name, graph.package_version),
      });
    }

    let manifest_modules = self
      .modules
      .iter()
      .map(|module| (module.specifier.as_str(), module))
      .collect::<BTreeMap<_, _>>();
    let graph_modules = graph
      .modules
      .iter()
      .map(|module| (module.specifier.as_str(), module))
      .collect::<BTreeMap<_, _>>();
    if manifest_modules.len() != graph_modules.len() {
      return Err(RuntimeGraphError::ModuleCountMismatch {
        manifest: manifest_modules.len(),
        graph: graph_modules.len(),
      });
    }
    for (specifier, manifest_module) in &manifest_modules {
      let Some(graph_module) = graph_modules.get(specifier) else {
        return Err(RuntimeGraphError::MissingGraphModule(
          (*specifier).to_string(),
        ));
      };
      if manifest_module.source_sha256 != graph_module.source_sha256 {
        return Err(RuntimeGraphError::SourceHashMismatch {
          module: (*specifier).to_string(),
        });
      }
      let actual_dependencies = graph_module
        .dependencies
        .iter()
        .map(|dependency| {
          graph
            .module(*dependency)
            .map(|module| module.specifier.clone())
            .ok_or_else(|| {
              RuntimeGraphError::InvalidEngineGraph(format!(
                "dependency index {dependency} disappeared during validation"
              ))
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
      if manifest_module.dependencies != actual_dependencies {
        return Err(RuntimeGraphError::DependencyMismatch {
          module: (*specifier).to_string(),
          expected: manifest_module.dependencies.clone(),
          actual: actual_dependencies,
        });
      }
    }
    for specifier in graph_modules.keys() {
      if !manifest_modules.contains_key(specifier) {
        return Err(RuntimeGraphError::UnexpectedGraphModule(
          (*specifier).to_string(),
        ));
      }
    }

    let manifest_roots = self
      .public_roots
      .iter()
      .map(String::as_str)
      .collect::<BTreeSet<_>>();
    let graph_roots = graph
      .public_roots
      .iter()
      .filter_map(|root| {
        graph
          .module(*root)
          .map(|module| module.specifier.as_str())
      })
      .collect::<BTreeSet<_>>();
    if manifest_roots != graph_roots {
      return Err(RuntimeGraphError::PublicRootsMismatch {
        manifest: manifest_roots
          .into_iter()
          .map(str::to_string)
          .collect(),
        graph: graph_roots
          .into_iter()
          .map(str::to_string)
          .collect(),
      });
    }
    Ok(())
  }

  /// Validate the manifest against the host bridge installed by the embedder.
  pub fn validate_for_host(
    &self,
    host: &HostBridgeCapabilities,
  ) -> Result<(), RuntimeGraphError> {
    self.validate()?;
    if host.abi_version != self.host_bridge_abi_version {
      return Err(RuntimeGraphError::HostBridgeAbiMismatch {
        expected: self.host_bridge_abi_version,
        actual: host.abi_version,
      });
    }
    for module in &self.modules {
      for requirement in &module.required_host_primitives {
        let Some(methods) = host.primitives.get(&requirement.name) else {
          return Err(RuntimeGraphError::MissingHostPrimitive {
            module: module.specifier.clone(),
            primitive: requirement.name.clone(),
          });
        };
        let Some(actual_abi_version) = host.primitive_abis.get(&requirement.name) else {
          return Err(RuntimeGraphError::MissingHostPrimitive {
            module: module.specifier.clone(),
            primitive: requirement.name.clone(),
          });
        };
        if *actual_abi_version != requirement.abi_version {
          return Err(RuntimeGraphError::HostPrimitiveAbiMismatch {
            primitive: requirement.name.clone(),
            expected: requirement.abi_version,
            actual: *actual_abi_version,
          });
        }
        for method in &requirement.methods {
          if !methods.contains(method) {
            return Err(RuntimeGraphError::MissingHostMethod {
              module: module.specifier.clone(),
              primitive: requirement.name.clone(),
              method: method.clone(),
            });
          }
        }
      }
    }
    Ok(())
  }
}

fn validate_specifier(specifier: &str) -> Result<(), RuntimeGraphError> {
  if specifier.is_empty() || specifier.contains(char::is_whitespace) || specifier.contains('\0') {
    return Err(RuntimeGraphError::InvalidSpecifier(specifier.to_string()));
  }
  Ok(())
}

fn validate_visibility(module: &RuntimeModuleManifest) -> Result<(), RuntimeGraphError> {
  let is_effect_public = module.specifier == "effect" || module.specifier.starts_with("effect/");
  let is_blitz_public =
    module.specifier.starts_with("blitz:") || module.specifier.starts_with("node:");
  let is_private = module.specifier.starts_with("blitz-internal:");

  match (module.origin, module.visibility) {
    (ModuleOrigin::Effect, ModuleVisibility::Public) if is_effect_public => {}
    (ModuleOrigin::Effect, ModuleVisibility::Private) if is_private => {}
    (ModuleOrigin::BlitzStdlib, ModuleVisibility::Public) if is_blitz_public => {}
    (ModuleOrigin::BlitzStdlib, ModuleVisibility::Private) if is_private => {}
    _ => {
      return Err(RuntimeGraphError::InvalidVisibility {
        module: module.specifier.clone(),
        origin: module.origin,
        visibility: module.visibility,
      });
    }
  }

  if module.visibility == ModuleVisibility::Private && module.declaration_sha256.is_some() {
    return Err(RuntimeGraphError::PrivateDeclaration(
      module.specifier.clone(),
    ));
  }
  Ok(())
}

/// Host bridge ABI and primitive method surfaces available before graph
/// registration.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct HostBridgeCapabilities {
  /// Version of the installed bridge object.
  pub abi_version: u16,
  /// Methods supplied by each primitive.
  pub primitives: BTreeMap<String, BTreeSet<String>>,
  primitive_abis: BTreeMap<String, u16>,
}

impl HostBridgeCapabilities {
  /// Create an empty host bridge capability set.
  #[must_use]
  pub const fn new(abi_version: u16) -> Self {
    Self {
      abi_version,
      primitives: BTreeMap::new(),
      primitive_abis: BTreeMap::new(),
    }
  }

  /// Add one host primitive and its method surface.
  #[must_use]
  pub fn with_primitive<I, S>(
    mut self,
    name: impl Into<String>,
    abi_version: u16,
    methods: I,
  ) -> Self
  where
    I: IntoIterator<Item = S>,
    S: Into<String>,
  {
    let name = name.into();
    self
      .primitive_abis
      .insert(name.clone(), abi_version);
    self
      .primitives
      .insert(name, methods.into_iter().map(Into::into).collect());
    self
  }
}

/// A versioned runtime artifact containing provenance and a prelinked graph.
#[derive(Clone, Debug)]
pub struct RuntimePrelinkedBundle {
  /// Runtime-owned provenance and module manifest.
  pub manifest: RuntimeGraphManifest,
  /// Host-neutral executable graph payload.
  pub graph: PrelinkedModuleGraph,
}

impl RuntimePrelinkedBundle {
  /// Construct a runtime envelope from its manifest and graph payload.
  #[must_use]
  pub const fn new(
    manifest: RuntimeGraphManifest,
    graph: PrelinkedModuleGraph,
  ) -> Self {
    Self { manifest, graph }
  }

  /// Validate the complete wrapper and its engine graph.
  pub fn validate(&self) -> Result<(), RuntimeGraphError> {
    self.manifest.validate_against_graph(&self.graph)
  }

  /// Validate the complete wrapper against host capabilities.
  pub fn validate_for_host(
    &self,
    host: &HostBridgeCapabilities,
  ) -> Result<(), RuntimeGraphError> {
    self.manifest.validate_for_host(host)?;
    self.manifest.validate_against_graph(&self.graph)
  }

  /// Validate graph and the exact embedded input manifests without host state.
  pub fn validate_pinned(
    &self,
    effect_manifest: &[u8],
    stdlib_manifest: &[u8],
  ) -> Result<(), RuntimeGraphError> {
    self.validate()?;
    let effect_hash = manifest_sha256(effect_manifest);
    if effect_hash != self.manifest.effect_manifest_sha256 {
      return Err(RuntimeGraphError::EffectManifestHashMismatch);
    }
    let stdlib_hash = manifest_sha256(stdlib_manifest);
    if stdlib_hash != self.manifest.stdlib_manifest_sha256 {
      return Err(RuntimeGraphError::StdlibManifestHashMismatch);
    }
    Ok(())
  }

  /// Validate graph, host capabilities, and the exact embedded input manifests.
  pub fn validate_pinned_for_host(
    &self,
    host: &HostBridgeCapabilities,
    effect_manifest: &[u8],
    stdlib_manifest: &[u8],
  ) -> Result<(), RuntimeGraphError> {
    self.manifest.validate_for_host(host)?;
    self.validate_pinned(effect_manifest, stdlib_manifest)
  }

  /// Encode this bundle into its deterministic, checksummed wire envelope.
  pub fn encode(&self) -> Result<Vec<u8>, RuntimeBundleCodecError> {
    encode_runtime_prelinked_bundle(self)
  }

  /// Decode and validate a complete runtime bundle before registration.
  pub fn decode(bytes: &[u8]) -> Result<Self, RuntimeBundleCodecError> {
    decode_runtime_prelinked_bundle(bytes)
  }
}

/// A validated, immutable handoff to the eventual engine graph registrar.
///
/// The registration callback is deliberately outside this crate's contract:
/// validation must finish before the engine is asked to make any module
/// visible. The final engine integration can consume this plan without
/// changing the runtime ABI checks.
#[derive(Clone, Debug)]
pub struct RuntimeGraphInstallPlan {
  manifest: RuntimeGraphManifest,
  bundle: Option<RuntimePrelinkedBundle>,
}

impl RuntimeGraphInstallPlan {
  /// Validate all runtime and host requirements before registration.
  pub fn prepare(
    manifest: RuntimeGraphManifest,
    host: &HostBridgeCapabilities,
  ) -> Result<Self, RuntimeGraphError> {
    manifest.validate_for_host(host)?;
    Ok(Self {
      manifest,
      bundle: None,
    })
  }

  /// Validate a complete bundle before any engine registration mutation.
  pub fn prepare_bundle(
    bundle: RuntimePrelinkedBundle,
    host: &HostBridgeCapabilities,
  ) -> Result<Self, RuntimeGraphError> {
    bundle.validate_for_host(host)?;
    Ok(Self {
      manifest: bundle.manifest.clone(),
      bundle: Some(bundle),
    })
  }

  /// Borrow the validated manifest for an atomic engine registration call.
  #[must_use]
  pub const fn manifest(&self) -> &RuntimeGraphManifest {
    &self.manifest
  }

  /// Whether a canonical module may be exposed to user resolution.
  #[must_use]
  pub fn is_user_visible(
    &self,
    specifier: &str,
  ) -> bool {
    self
      .manifest
      .modules
      .iter()
      .any(|module| module.specifier == specifier && module.visibility == ModuleVisibility::Public)
  }

  /// Borrow the validated complete bundle, when this plan was prepared from one.
  #[must_use]
  pub const fn bundle(&self) -> Option<&RuntimePrelinkedBundle> {
    self.bundle.as_ref()
  }

  /// Invoke manifest registration only after all policy checks pass.
  pub fn register_after_validation<T, E>(
    &self,
    register: impl FnOnce(&RuntimeGraphManifest) -> Result<T, E>,
  ) -> Result<T, E> {
    register(&self.manifest)
  }

  /// Invoke complete graph registration only after all policy checks pass.
  pub fn register_bundle_after_validation<T, E>(
    &self,
    register: impl FnOnce(&RuntimePrelinkedBundle) -> Result<T, E>,
  ) -> Result<T, RuntimeGraphInstallError<E>> {
    let Some(bundle) = self.bundle.as_ref() else {
      return Err(RuntimeGraphInstallError::MissingBundle);
    };
    register(bundle).map_err(RuntimeGraphInstallError::Registration)
  }
}

/// Failure from complete bundle registration through an install plan.
#[derive(Debug, Eq, PartialEq)]
pub enum RuntimeGraphInstallError<E> {
  /// The plan was prepared from a manifest without a graph payload.
  MissingBundle,
  /// The embedder's registration callback rejected the already validated bundle.
  Registration(E),
}

/// Runtime graph validation failures.
#[allow(missing_docs)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RuntimeGraphError {
  /// The wrapper format is newer or otherwise unsupported.
  UnsupportedGraphFormat(u16),
  /// The artifact was generated for a different runtime ABI.
  RuntimeAbiMismatch { expected: u16, actual: u16 },
  /// The artifact was generated for a different bridge ABI.
  HostBridgeAbiMismatch { expected: u16, actual: u16 },
  /// The artifact targets a different Effect release.
  EffectVersionMismatch { expected: String, actual: String },
  /// No graph modules were supplied.
  EmptyGraph,
  /// A module specifier is malformed.
  InvalidSpecifier(String),
  /// A module's origin and visibility/prefix do not agree.
  InvalidVisibility {
    module: String,
    origin: ModuleOrigin,
    visibility: ModuleVisibility,
  },
  /// Private modules must not ship public declarations.
  PrivateDeclaration(String),
  /// The graph contains the same module twice.
  DuplicateModule(String),
  /// A module repeats one dependency edge.
  DuplicateDependency { module: String, dependency: String },
  /// A dependency does not name a graph module.
  MissingDependency { module: String, dependency: String },
  /// A public root occurs more than once.
  DuplicatePublicRoot(String),
  /// A public root does not name a graph module.
  MissingPublicRoot(String),
  /// A private module was advertised as a public root.
  PrivatePublicRoot(String),
  /// The graph does not expose any public root.
  NoPublicRoots,
  /// A required host primitive name is malformed.
  InvalidHostPrimitive(String),
  /// A module repeats one host primitive requirement.
  DuplicateHostPrimitive { module: String, primitive: String },
  /// A required host method name is malformed.
  InvalidHostMethod { primitive: String, method: String },
  /// A primitive repeats one method requirement.
  DuplicateHostMethod { primitive: String, method: String },
  /// A required primitive is absent from the installed bridge.
  MissingHostPrimitive { module: String, primitive: String },
  /// A required primitive has a different ABI.
  HostPrimitiveAbiMismatch {
    primitive: String,
    expected: u16,
    actual: u16,
  },
  /// A required method is absent from the installed primitive.
  MissingHostMethod {
    module: String,
    primitive: String,
    method: String,
  },
  /// The engine graph failed host-neutral validation.
  InvalidEngineGraph(String),
  /// The graph package identity is not a supported pinned runtime identity.
  PackageMetadataMismatch { expected: String, actual: String },
  /// The manifest and graph contain different module counts.
  ModuleCountMismatch { manifest: usize, graph: usize },
  /// A manifest module is absent from the graph.
  MissingGraphModule(String),
  /// A graph module is absent from the runtime manifest.
  UnexpectedGraphModule(String),
  /// The artifact does not target the Effect manifest embedded by this runtime.
  EffectManifestHashMismatch,
  /// The artifact does not target the stdlib manifest embedded by this runtime.
  StdlibManifestHashMismatch,
  /// A module's source digest differs between manifest and graph.
  SourceHashMismatch { module: String },
  /// A module's ordered dependency names differ between manifest and graph indices.
  DependencyMismatch {
    module: String,
    expected: Vec<String>,
    actual: Vec<String>,
  },
  /// Manifest public roots differ from graph public-root indices.
  PublicRootsMismatch {
    manifest: Vec<String>,
    graph: Vec<String>,
  },
}

impl fmt::Display for RuntimeGraphError {
  fn fmt(
    &self,
    formatter: &mut fmt::Formatter<'_>,
  ) -> fmt::Result {
    write!(formatter, "invalid runtime graph: {self:?}")
  }
}

impl std::error::Error for RuntimeGraphError {}

/// Decode and cache the committed runtime graph exactly once per process.
///
/// The cached error is converted back into a codec error for each caller so a
/// malformed artifact cannot be retried with partially decoded state.
pub fn embedded_runtime_bundle() -> Result<&'static RuntimePrelinkedBundle, RuntimeBundleCodecError>
{
  static BUNDLE: OnceLock<Result<Arc<RuntimePrelinkedBundle>, String>> = OnceLock::new();
  let cached = BUNDLE.get_or_init(|| {
    RuntimePrelinkedBundle::decode(RUNTIME_GRAPH_ARTIFACT)
      .map(Arc::new)
      .map_err(|error| error.to_string())
  });
  match cached {
    Ok(bundle) => Ok(bundle.as_ref()),
    Err(error) => Err(RuntimeBundleCodecError::InvalidData(error.clone())),
  }
}

/// Validate the committed graph against both manifests embedded by the runtime.
pub fn validate_embedded_runtime_bundle()
-> Result<&'static RuntimePrelinkedBundle, RuntimeBundleCodecError> {
  let bundle = embedded_runtime_bundle()?;
  bundle
    .validate_pinned(
      crate::effect_assets::EFFECT_MANIFEST_BYTES,
      crate::effect_assets::STDLIB_MANIFEST_BYTES,
    )
    .map_err(RuntimeBundleCodecError::Manifest)?;
  Ok(bundle)
}

const RUNTIME_BUNDLE_MAGIC: &[u8; 8] = b"BLITZRT\0";
const RUNTIME_BUNDLE_HEADER_LEN: usize = RUNTIME_BUNDLE_MAGIC.len() + 2 + 8 + 4;
const MAX_RUNTIME_BUNDLE_BYTES: usize = 512 * 1024 * 1024;
const MAX_RUNTIME_MANIFEST_MODULES: usize = 65_536;
const MAX_RUNTIME_MANIFEST_DEPENDENCIES: usize = 4_000_000;
const MAX_RUNTIME_MANIFEST_PRIMITIVES: usize = 4_000_000;
const MAX_RUNTIME_MANIFEST_METHODS: usize = 8_000_000;
const MAX_RUNTIME_NAME_BYTES: usize = 64 * 1024;
const MAX_RUNTIME_TOTAL_NAME_BYTES: usize = 64 * 1024 * 1024;

/// Errors produced by the runtime bundle envelope codec.
#[derive(Debug)]
pub enum RuntimeBundleCodecError {
  /// The input does not begin with the runtime bundle signature.
  InvalidHeader,
  /// The runtime bundle wire version is unsupported.
  UnsupportedVersion(u16),
  /// The bundle exceeds the defensive codec limit.
  TooLarge {
    /// Observed byte count.
    actual: usize,
    /// Maximum accepted byte count.
    maximum: usize,
  },
  /// A bounded field exceeds its defensive limit.
  LimitExceeded {
    /// Limited resource.
    kind: &'static str,
    /// Observed resource amount.
    actual: usize,
    /// Maximum accepted resource amount.
    maximum: usize,
  },
  /// The payload length in the envelope is invalid.
  LengthMismatch,
  /// The envelope checksum does not match its payload.
  ChecksumMismatch,
  /// The payload contains invalid or truncated data.
  InvalidData(String),
  /// The runtime manifest failed validation.
  Manifest(RuntimeGraphError),
  /// The engine graph codec rejected the graph payload.
  GraphCodec(denko::PrelinkedGraphCodecError),
}

impl fmt::Display for RuntimeBundleCodecError {
  fn fmt(
    &self,
    formatter: &mut fmt::Formatter<'_>,
  ) -> fmt::Result {
    match self {
      Self::InvalidHeader => write!(formatter, "invalid runtime bundle header"),
      Self::UnsupportedVersion(version) => {
        write!(
          formatter,
          "unsupported runtime bundle wire version {version}"
        )
      }
      Self::TooLarge { actual, maximum } => write!(
        formatter,
        "runtime bundle is {actual} bytes; maximum supported size is {maximum} bytes"
      ),
      Self::LimitExceeded {
        kind,
        actual,
        maximum,
      } => write!(
        formatter,
        "runtime bundle {kind} is {actual}; maximum supported value is {maximum}"
      ),
      Self::LengthMismatch => write!(formatter, "runtime bundle payload length mismatch"),
      Self::ChecksumMismatch => write!(formatter, "runtime bundle checksum mismatch"),
      Self::InvalidData(message) => write!(formatter, "invalid runtime bundle data: {message}"),
      Self::Manifest(error) => write!(formatter, "invalid runtime bundle manifest: {error}"),
      Self::GraphCodec(error) => write!(formatter, "invalid runtime graph payload: {error}"),
    }
  }
}

impl std::error::Error for RuntimeBundleCodecError {}

impl From<RuntimeGraphError> for RuntimeBundleCodecError {
  fn from(error: RuntimeGraphError) -> Self {
    Self::Manifest(error)
  }
}

impl From<denko::PrelinkedGraphCodecError> for RuntimeBundleCodecError {
  fn from(error: denko::PrelinkedGraphCodecError) -> Self {
    Self::GraphCodec(error)
  }
}

/// Encode a complete runtime graph as a deterministic checksummed envelope.
pub fn encode_runtime_prelinked_bundle(
  bundle: &RuntimePrelinkedBundle
) -> Result<Vec<u8>, RuntimeBundleCodecError> {
  bundle.validate()?;
  let graph_payload = denko::encode_prelinked_graph(&bundle.graph)?;
  let mut writer = RuntimeBundleWriter::default();
  encode_manifest(&mut writer, &bundle.manifest)?;
  writer.bytes_u64(&graph_payload, MAX_RUNTIME_BUNDLE_BYTES, "graph payload")?;
  frame_runtime_payload(&writer.bytes_out)
}

/// Encode a complete runtime bundle.
#[must_use = "the encoded artifact must be retained"]
pub fn encode_runtime_bundle(
  bundle: &RuntimePrelinkedBundle
) -> Result<Vec<u8>, RuntimeBundleCodecError> {
  encode_runtime_prelinked_bundle(bundle)
}

/// Decode and validate a complete runtime graph envelope.
pub fn decode_runtime_prelinked_bundle(
  bytes: &[u8]
) -> Result<RuntimePrelinkedBundle, RuntimeBundleCodecError> {
  if bytes.len() > MAX_RUNTIME_BUNDLE_BYTES {
    return Err(RuntimeBundleCodecError::TooLarge {
      actual: bytes.len(),
      maximum: MAX_RUNTIME_BUNDLE_BYTES,
    });
  }
  if bytes.len() < RUNTIME_BUNDLE_HEADER_LEN
    || &bytes[..RUNTIME_BUNDLE_MAGIC.len()] != RUNTIME_BUNDLE_MAGIC
  {
    return Err(RuntimeBundleCodecError::InvalidHeader);
  }
  let version = u16::from_le_bytes(
    bytes[8..10]
      .try_into()
      .map_err(|_| RuntimeBundleCodecError::InvalidHeader)?,
  );
  if version != RUNTIME_BUNDLE_WIRE_VERSION {
    return Err(RuntimeBundleCodecError::UnsupportedVersion(version));
  }
  let payload_len = u64::from_le_bytes(
    bytes[10..18]
      .try_into()
      .map_err(|_| RuntimeBundleCodecError::InvalidHeader)?,
  );
  let payload_len = usize::try_from(payload_len).map_err(|_| {
    RuntimeBundleCodecError::InvalidData("payload length does not fit usize".into())
  })?;
  if payload_len > MAX_RUNTIME_BUNDLE_BYTES - RUNTIME_BUNDLE_HEADER_LEN
    || bytes.len().checked_sub(RUNTIME_BUNDLE_HEADER_LEN) != Some(payload_len)
  {
    return Err(RuntimeBundleCodecError::LengthMismatch);
  }
  let expected_checksum = u32::from_le_bytes(
    bytes[18..22]
      .try_into()
      .map_err(|_| RuntimeBundleCodecError::InvalidHeader)?,
  );
  let payload = &bytes[RUNTIME_BUNDLE_HEADER_LEN..];
  if runtime_checksum(payload) != expected_checksum {
    return Err(RuntimeBundleCodecError::ChecksumMismatch);
  }

  let mut reader = RuntimeBundleReader::new(payload);
  let manifest = decode_manifest(&mut reader)?;
  let graph_payload = reader.bytes_u64(MAX_RUNTIME_BUNDLE_BYTES, "graph payload")?;
  reader.finish()?;
  let graph = denko::decode_prelinked_graph(&graph_payload)?;
  let bundle = RuntimePrelinkedBundle { manifest, graph };
  bundle.validate()?;
  Ok(bundle)
}

/// Decode a complete runtime bundle.
pub fn decode_runtime_bundle(
  bytes: &[u8]
) -> Result<RuntimePrelinkedBundle, RuntimeBundleCodecError> {
  decode_runtime_prelinked_bundle(bytes)
}

fn encode_manifest(
  writer: &mut RuntimeBundleWriter,
  manifest: &RuntimeGraphManifest,
) -> Result<(), RuntimeBundleCodecError> {
  writer.u16(manifest.format_version);
  writer.u16(manifest.runtime_abi_version);
  writer.u16(manifest.host_bridge_abi_version);
  writer.string(&manifest.effect_version)?;
  writer.hash(&manifest.effect_manifest_sha256);
  writer.hash(&manifest.stdlib_manifest_sha256);
  writer.vector_len(
    manifest.modules.len(),
    MAX_RUNTIME_MANIFEST_MODULES,
    "manifest module count",
  )?;
  for module in &manifest.modules {
    writer.string(&module.specifier)?;
    writer.u8(match module.origin {
      ModuleOrigin::Effect => 0,
      ModuleOrigin::BlitzStdlib => 1,
    });
    writer.u8(match module.visibility {
      ModuleVisibility::Public => 0,
      ModuleVisibility::Private => 1,
    });
    writer.hash(&module.source_sha256);
    match module.declaration_sha256 {
      None => writer.u8(0),
      Some(hash) => {
        writer.u8(1);
        writer.hash(&hash);
      }
    }
    writer.vector_len(
      module.dependencies.len(),
      MAX_RUNTIME_MANIFEST_DEPENDENCIES,
      "manifest dependency count",
    )?;
    for dependency in &module.dependencies {
      writer.string(dependency)?;
    }
    writer.vector_len(
      module.required_host_primitives.len(),
      MAX_RUNTIME_MANIFEST_PRIMITIVES,
      "host primitive requirement count",
    )?;
    for primitive in &module.required_host_primitives {
      writer.string(&primitive.name)?;
      writer.u16(primitive.abi_version);
      writer.vector_len(
        primitive.methods.len(),
        MAX_RUNTIME_MANIFEST_METHODS,
        "host method count",
      )?;
      for method in &primitive.methods {
        writer.string(method)?;
      }
    }
  }
  writer.vector_len(
    manifest.public_roots.len(),
    MAX_RUNTIME_MANIFEST_MODULES,
    "public root count",
  )?;
  for root in &manifest.public_roots {
    writer.string(root)?;
  }
  Ok(())
}

fn decode_manifest(
  reader: &mut RuntimeBundleReader<'_>
) -> Result<RuntimeGraphManifest, RuntimeBundleCodecError> {
  let format_version = reader.u16()?;
  let runtime_abi_version = reader.u16()?;
  let host_bridge_abi_version = reader.u16()?;
  let effect_version = reader.string()?;
  let effect_manifest_sha256 = reader.hash()?;
  let stdlib_manifest_sha256 = reader.hash()?;
  let module_count = reader.vector_len(MAX_RUNTIME_MANIFEST_MODULES, "manifest module count")?;
  let mut modules = Vec::with_capacity(module_count);
  for _ in 0..module_count {
    let specifier = reader.string()?;
    let origin = match reader.u8()? {
      0 => ModuleOrigin::Effect,
      1 => ModuleOrigin::BlitzStdlib,
      value => {
        return Err(RuntimeBundleCodecError::InvalidData(format!(
          "unknown module origin {value}"
        )));
      }
    };
    let visibility = match reader.u8()? {
      0 => ModuleVisibility::Public,
      1 => ModuleVisibility::Private,
      value => {
        return Err(RuntimeBundleCodecError::InvalidData(format!(
          "unknown module visibility {value}"
        )));
      }
    };
    let source_sha256 = reader.hash()?;
    let declaration_sha256 = match reader.u8()? {
      0 => None,
      1 => Some(reader.hash()?),
      value => {
        return Err(RuntimeBundleCodecError::InvalidData(format!(
          "invalid declaration hash tag {value}"
        )));
      }
    };
    let dependency_count = reader.vector_len(
      MAX_RUNTIME_MANIFEST_DEPENDENCIES,
      "manifest dependency count",
    )?;
    let mut dependencies = Vec::with_capacity(dependency_count);
    for _ in 0..dependency_count {
      dependencies.push(reader.string()?);
    }
    let primitive_count = reader.vector_len(
      MAX_RUNTIME_MANIFEST_PRIMITIVES,
      "host primitive requirement count",
    )?;
    let mut required_host_primitives = Vec::with_capacity(primitive_count);
    for _ in 0..primitive_count {
      let name = reader.string()?;
      let abi_version = reader.u16()?;
      let method_count = reader.vector_len(MAX_RUNTIME_MANIFEST_METHODS, "host method count")?;
      let mut methods = Vec::with_capacity(method_count);
      for _ in 0..method_count {
        methods.push(reader.string()?);
      }
      required_host_primitives.push(HostPrimitiveRequirement {
        name,
        abi_version,
        methods,
      });
    }
    modules.push(RuntimeModuleManifest {
      specifier,
      origin,
      visibility,
      source_sha256,
      declaration_sha256,
      dependencies,
      required_host_primitives,
    });
  }
  let root_count = reader.vector_len(MAX_RUNTIME_MANIFEST_MODULES, "public root count")?;
  let mut public_roots = Vec::with_capacity(root_count);
  for _ in 0..root_count {
    public_roots.push(reader.string()?);
  }
  Ok(RuntimeGraphManifest {
    format_version,
    runtime_abi_version,
    host_bridge_abi_version,
    effect_version,
    effect_manifest_sha256,
    stdlib_manifest_sha256,
    modules,
    public_roots,
  })
}

fn frame_runtime_payload(payload: &[u8]) -> Result<Vec<u8>, RuntimeBundleCodecError> {
  let total = RUNTIME_BUNDLE_HEADER_LEN
    .checked_add(payload.len())
    .ok_or_else(|| RuntimeBundleCodecError::InvalidData("bundle length overflow".into()))?;
  if total > MAX_RUNTIME_BUNDLE_BYTES {
    return Err(RuntimeBundleCodecError::TooLarge {
      actual: total,
      maximum: MAX_RUNTIME_BUNDLE_BYTES,
    });
  }
  let payload_len = u64::try_from(payload.len())
    .map_err(|_| RuntimeBundleCodecError::InvalidData("payload length overflow".into()))?;
  let mut bytes = Vec::with_capacity(total);
  bytes.extend_from_slice(RUNTIME_BUNDLE_MAGIC);
  bytes.extend_from_slice(&RUNTIME_BUNDLE_WIRE_VERSION.to_le_bytes());
  bytes.extend_from_slice(&payload_len.to_le_bytes());
  bytes.extend_from_slice(&runtime_checksum(payload).to_le_bytes());
  bytes.extend_from_slice(payload);
  Ok(bytes)
}

fn runtime_checksum(bytes: &[u8]) -> u32 {
  crc32fast::hash(bytes)
}

#[derive(Default)]
struct RuntimeBundleWriter {
  bytes_out: Vec<u8>,
  name_bytes: usize,
}

impl RuntimeBundleWriter {
  fn u8(
    &mut self,
    value: u8,
  ) {
    self.bytes_out.push(value);
  }

  fn u16(
    &mut self,
    value: u16,
  ) {
    self
      .bytes_out
      .extend_from_slice(&value.to_le_bytes());
  }

  fn u32(
    &mut self,
    value: u32,
  ) {
    self
      .bytes_out
      .extend_from_slice(&value.to_le_bytes());
  }

  fn u64(
    &mut self,
    value: u64,
  ) {
    self
      .bytes_out
      .extend_from_slice(&value.to_le_bytes());
  }

  fn hash(
    &mut self,
    value: &[u8; 32],
  ) {
    self.bytes_out.extend_from_slice(value);
  }

  fn vector_len(
    &mut self,
    length: usize,
    maximum: usize,
    kind: &'static str,
  ) -> Result<(), RuntimeBundleCodecError> {
    if length > maximum {
      return Err(RuntimeBundleCodecError::LimitExceeded {
        kind,
        actual: length,
        maximum,
      });
    }
    let length = u32::try_from(length)
      .map_err(|_| RuntimeBundleCodecError::InvalidData(format!("{kind} does not fit u32")))?;
    self.u32(length);
    Ok(())
  }

  fn string(
    &mut self,
    value: &str,
  ) -> Result<(), RuntimeBundleCodecError> {
    if value.len() > MAX_RUNTIME_NAME_BYTES {
      return Err(RuntimeBundleCodecError::LimitExceeded {
        kind: "name byte count",
        actual: value.len(),
        maximum: MAX_RUNTIME_NAME_BYTES,
      });
    }
    self.name_bytes = self
      .name_bytes
      .checked_add(value.len())
      .ok_or_else(|| RuntimeBundleCodecError::InvalidData("name byte count overflow".into()))?;
    if self.name_bytes > MAX_RUNTIME_TOTAL_NAME_BYTES {
      return Err(RuntimeBundleCodecError::LimitExceeded {
        kind: "total name bytes",
        actual: self.name_bytes,
        maximum: MAX_RUNTIME_TOTAL_NAME_BYTES,
      });
    }
    self.vector_len(value.len(), u32::MAX as usize, "name byte count")?;
    self.bytes_out.extend_from_slice(value.as_bytes());
    Ok(())
  }

  fn bytes_u64(
    &mut self,
    value: &[u8],
    maximum: usize,
    kind: &'static str,
  ) -> Result<(), RuntimeBundleCodecError> {
    if value.len() > maximum {
      return Err(RuntimeBundleCodecError::LimitExceeded {
        kind,
        actual: value.len(),
        maximum,
      });
    }
    self.u64(u64::try_from(value.len()).map_err(|_| {
      RuntimeBundleCodecError::InvalidData(format!("{kind} length does not fit u64"))
    })?);
    self.bytes_out.extend_from_slice(value);
    Ok(())
  }
}

struct RuntimeBundleReader<'a> {
  bytes: &'a [u8],
  position: usize,
  name_bytes: usize,
}

impl<'a> RuntimeBundleReader<'a> {
  fn new(bytes: &'a [u8]) -> Self {
    Self {
      bytes,
      position: 0,
      name_bytes: 0,
    }
  }

  fn remaining(&self) -> usize {
    self.bytes.len().saturating_sub(self.position)
  }

  fn take(
    &mut self,
    length: usize,
  ) -> Result<&'a [u8], RuntimeBundleCodecError> {
    let end = self
      .position
      .checked_add(length)
      .ok_or_else(|| RuntimeBundleCodecError::InvalidData("input position overflow".into()))?;
    if end > self.bytes.len() {
      return Err(RuntimeBundleCodecError::InvalidData(
        "unexpected end of runtime bundle payload".into(),
      ));
    }
    let result = &self.bytes[self.position..end];
    self.position = end;
    Ok(result)
  }

  fn u8(&mut self) -> Result<u8, RuntimeBundleCodecError> {
    self
      .take(1)?
      .first()
      .copied()
      .ok_or_else(|| RuntimeBundleCodecError::InvalidData("missing byte".into()))
  }

  fn u16(&mut self) -> Result<u16, RuntimeBundleCodecError> {
    Ok(u16::from_le_bytes(self.take(2)?.try_into().map_err(
      |_| RuntimeBundleCodecError::InvalidData("missing u16".into()),
    )?))
  }

  fn u64(&mut self) -> Result<u64, RuntimeBundleCodecError> {
    Ok(u64::from_le_bytes(self.take(8)?.try_into().map_err(
      |_| RuntimeBundleCodecError::InvalidData("missing u64".into()),
    )?))
  }

  fn hash(&mut self) -> Result<[u8; 32], RuntimeBundleCodecError> {
    self
      .take(32)?
      .try_into()
      .map_err(|_| RuntimeBundleCodecError::InvalidData("missing 32-byte hash".into()))
  }

  fn vector_len(
    &mut self,
    maximum: usize,
    kind: &'static str,
  ) -> Result<usize, RuntimeBundleCodecError> {
    let value = u32::from_le_bytes(
      self
        .take(4)?
        .try_into()
        .map_err(|_| RuntimeBundleCodecError::InvalidData("missing vector length".into()))?,
    ) as usize;
    if value > maximum {
      return Err(RuntimeBundleCodecError::LimitExceeded {
        kind,
        actual: value,
        maximum,
      });
    }
    if value > self.remaining() {
      return Err(RuntimeBundleCodecError::InvalidData(format!(
        "{kind} cannot fit in remaining payload"
      )));
    }
    Ok(value)
  }

  fn string(&mut self) -> Result<String, RuntimeBundleCodecError> {
    let length = self.vector_len(MAX_RUNTIME_NAME_BYTES, "name byte count")?;
    self.name_bytes = self
      .name_bytes
      .checked_add(length)
      .ok_or_else(|| RuntimeBundleCodecError::InvalidData("name byte count overflow".into()))?;
    if self.name_bytes > MAX_RUNTIME_TOTAL_NAME_BYTES {
      return Err(RuntimeBundleCodecError::LimitExceeded {
        kind: "total name bytes",
        actual: self.name_bytes,
        maximum: MAX_RUNTIME_TOTAL_NAME_BYTES,
      });
    }
    String::from_utf8(self.take(length)?.to_vec())
      .map_err(|_| RuntimeBundleCodecError::InvalidData("name is not valid UTF-8".into()))
  }

  fn bytes_u64(
    &mut self,
    maximum: usize,
    kind: &'static str,
  ) -> Result<Vec<u8>, RuntimeBundleCodecError> {
    let length = usize::try_from(self.u64()?).map_err(|_| {
      RuntimeBundleCodecError::InvalidData(format!("{kind} length does not fit usize"))
    })?;
    if length > maximum {
      return Err(RuntimeBundleCodecError::LimitExceeded {
        kind,
        actual: length,
        maximum,
      });
    }
    Ok(self.take(length)?.to_vec())
  }

  fn finish(&self) -> Result<(), RuntimeBundleCodecError> {
    if self.position == self.bytes.len() {
      Ok(())
    } else {
      Err(RuntimeBundleCodecError::InvalidData(
        "trailing bytes in runtime bundle payload".into(),
      ))
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use denko::bytecode::{BytecodeVersion, Chunk, Op, encode_risc_aux16};
  use denko::prelinked::{PRELINKED_MODULE_GRAPH_VERSION, PrelinkedExport, PrelinkedModule};
  use std::sync::Arc;

  fn codec_graph() -> PrelinkedModuleGraph {
    let mut chunk = Chunk::new(Arc::new("debug source".to_string()));
    chunk.write_instr(encode_risc_aux16(Op::Undefined, 0, 1), 0);
    chunk.write_instr(encode_risc_aux16(Op::Return, 0, 1), 0);
    PrelinkedModuleGraph {
      format_version: PRELINKED_MODULE_GRAPH_VERSION,
      engine_bytecode_version: BytecodeVersion::RiscV2,
      package_name: RUNTIME_GRAPH_PACKAGE_NAME.to_string(),
      package_version: RUNTIME_GRAPH_PACKAGE_VERSION.to_string(),
      source_manifest_hash: [9; 32],
      modules: vec![PrelinkedModule {
        specifier: "effect/Effect".to_string(),
        module_index: 0,
        source_sha256: [3; 32],
        dependencies: Vec::new(),
        exports: vec![PrelinkedExport {
          exported_name: "value".to_string(),
          binding_id: 0,
        }],
        imports: Vec::new(),
        chunk,
      }],
      binding_count: 1,
      public_roots: vec![0],
    }
  }

  fn codec_bundle() -> RuntimePrelinkedBundle {
    RuntimePrelinkedBundle {
      manifest: RuntimeGraphManifest {
        format_version: RUNTIME_GRAPH_FORMAT_VERSION,
        runtime_abi_version: RUNTIME_ABI_VERSION,
        host_bridge_abi_version: HOST_BRIDGE_ABI_VERSION,
        effect_version: EFFECT_VERSION.to_string(),
        effect_manifest_sha256: [1; 32],
        stdlib_manifest_sha256: [2; 32],
        modules: vec![RuntimeModuleManifest {
          specifier: "effect/Effect".to_string(),
          origin: ModuleOrigin::Effect,
          visibility: ModuleVisibility::Public,
          source_sha256: [3; 32],
          declaration_sha256: Some([4; 32]),
          dependencies: Vec::new(),
          required_host_primitives: Vec::new(),
        }],
        public_roots: vec!["effect/Effect".to_string()],
      },
      graph: codec_graph(),
    }
  }

  fn requirement() -> HostPrimitiveRequirement {
    HostPrimitiveRequirement::new(
      "app-transport",
      HOST_BRIDGE_ABI_VERSION,
      ["openAppServer", "closeAppServer", "awaitShutdown"],
    )
  }

  fn manifest() -> RuntimeGraphManifest {
    RuntimeGraphManifest {
      format_version: RUNTIME_GRAPH_FORMAT_VERSION,
      runtime_abi_version: RUNTIME_ABI_VERSION,
      host_bridge_abi_version: HOST_BRIDGE_ABI_VERSION,
      effect_version: EFFECT_VERSION.to_string(),
      effect_manifest_sha256: [1; 32],
      stdlib_manifest_sha256: [2; 32],
      modules: vec![
        RuntimeModuleManifest {
          specifier: "effect/Effect".to_string(),
          origin: ModuleOrigin::Effect,
          visibility: ModuleVisibility::Public,
          source_sha256: [3; 32],
          declaration_sha256: Some([4; 32]),
          dependencies: Vec::new(),
          required_host_primitives: Vec::new(),
        },
        RuntimeModuleManifest {
          specifier: "blitz-internal:app-transport".to_string(),
          origin: ModuleOrigin::BlitzStdlib,
          visibility: ModuleVisibility::Private,
          source_sha256: [5; 32],
          declaration_sha256: None,
          dependencies: vec!["effect/Effect".to_string()],
          required_host_primitives: vec![requirement()],
        },
        RuntimeModuleManifest {
          specifier: "blitz:app".to_string(),
          origin: ModuleOrigin::BlitzStdlib,
          visibility: ModuleVisibility::Public,
          source_sha256: [6; 32],
          declaration_sha256: Some([7; 32]),
          dependencies: vec![
            "effect/Effect".to_string(),
            "blitz-internal:app-transport".to_string(),
          ],
          required_host_primitives: Vec::new(),
        },
      ],
      public_roots: vec!["effect/Effect".to_string(), "blitz:app".to_string()],
    }
  }

  fn host() -> HostBridgeCapabilities {
    HostBridgeCapabilities::new(HOST_BRIDGE_ABI_VERSION).with_primitive(
      "app-transport",
      HOST_BRIDGE_ABI_VERSION,
      ["openAppServer", "closeAppServer", "awaitShutdown"],
    )
  }

  #[test]
  fn bundle_round_trip_is_deterministic_and_preserves_graph_payload() {
    let bundle = codec_bundle();
    let first = bundle.encode().expect("encode bundle");
    assert_eq!(first, bundle.encode().expect("encode bundle again"));
    let decoded = RuntimePrelinkedBundle::decode(&first).expect("decode bundle");
    assert_eq!(decoded.manifest, bundle.manifest);
    assert_eq!(decoded.graph.modules[0].source_sha256, [3; 32]);
    assert_eq!(
      decoded.graph.modules[0]
        .chunk
        .source_text
        .as_str(),
      "debug source"
    );
    assert_eq!(decoded.encode().expect("re-encode bundle"), first);
  }

  #[test]
  fn bundle_rejects_corruption_truncation_and_unknown_version() {
    let encoded = codec_bundle().encode().expect("encode bundle");
    let mut corrupt = encoded.clone();
    let last = corrupt.len() - 1;
    corrupt[last] ^= 1;
    assert!(matches!(
      RuntimePrelinkedBundle::decode(&corrupt),
      Err(RuntimeBundleCodecError::ChecksumMismatch)
    ));
    assert!(RuntimePrelinkedBundle::decode(&encoded[..encoded.len() - 1]).is_err());

    let mut version = encoded;
    version[8..10].copy_from_slice(&(RUNTIME_BUNDLE_WIRE_VERSION + 1).to_le_bytes());
    assert!(matches!(
      RuntimePrelinkedBundle::decode(&version),
      Err(RuntimeBundleCodecError::UnsupportedVersion(value))
        if value == RUNTIME_BUNDLE_WIRE_VERSION + 1
    ));
  }

  #[test]
  fn bundle_rejects_manifest_limits_before_allocation() {
    let mut writer = RuntimeBundleWriter::default();
    writer.u16(RUNTIME_GRAPH_FORMAT_VERSION);
    writer.u16(RUNTIME_ABI_VERSION);
    writer.u16(HOST_BRIDGE_ABI_VERSION);
    writer
      .string(EFFECT_VERSION)
      .expect("encode effect version");
    writer.hash(&[1; 32]);
    writer.hash(&[2; 32]);
    writer.u32((MAX_RUNTIME_MANIFEST_MODULES as u32) + 1);
    let encoded = frame_runtime_payload(&writer.bytes_out).expect("frame bundle");
    assert!(matches!(
      RuntimePrelinkedBundle::decode(&encoded),
      Err(RuntimeBundleCodecError::LimitExceeded {
        kind: "manifest module count",
        ..
      })
    ));
  }

  #[test]
  fn bundle_cross_validation_checks_hashes_and_dependency_names() {
    let mut bundle = codec_bundle();
    bundle.manifest.modules[0].source_sha256 = [4; 32];
    assert!(matches!(
      bundle.validate(),
      Err(RuntimeGraphError::SourceHashMismatch { .. })
    ));

    let mut bundle = codec_bundle();
    bundle.manifest.modules[0].dependencies = vec!["effect/Effect".to_string()];
    assert!(matches!(
      bundle.validate(),
      Err(RuntimeGraphError::DependencyMismatch { .. })
    ));
  }

  #[test]
  fn bundle_rejects_unpinned_input_manifests() {
    let effect_manifest = b"effect manifest";
    let stdlib_manifest = b"stdlib manifest";
    let mut bundle = codec_bundle();
    bundle.manifest.effect_manifest_sha256 = Sha256::digest(effect_manifest).into();
    bundle.manifest.stdlib_manifest_sha256 = Sha256::digest(stdlib_manifest).into();
    bundle
      .validate_pinned_for_host(
        &HostBridgeCapabilities::new(HOST_BRIDGE_ABI_VERSION),
        effect_manifest,
        stdlib_manifest,
      )
      .expect("pinned manifests validate");
    assert!(matches!(
      bundle.validate_pinned_for_host(
        &HostBridgeCapabilities::new(HOST_BRIDGE_ABI_VERSION),
        b"stale effect manifest",
        stdlib_manifest,
      ),
      Err(RuntimeGraphError::EffectManifestHashMismatch)
    ));
    assert!(matches!(
      bundle.validate_pinned_for_host(
        &HostBridgeCapabilities::new(HOST_BRIDGE_ABI_VERSION),
        effect_manifest,
        b"stale stdlib manifest",
      ),
      Err(RuntimeGraphError::StdlibManifestHashMismatch)
    ));
  }

  #[test]
  fn bundle_host_policy_is_checked_before_registration() {
    let mut bundle = codec_bundle();
    bundle.manifest.modules[0].required_host_primitives = vec![HostPrimitiveRequirement::new(
      "missing",
      HOST_BRIDGE_ABI_VERSION,
      ["run"],
    )];
    let mut registration_called = false;
    let result = RuntimeGraphInstallPlan::prepare_bundle(
      bundle,
      &HostBridgeCapabilities::new(HOST_BRIDGE_ABI_VERSION),
    )
    .map(|plan| {
      plan.register_bundle_after_validation(|_| {
        registration_called = true;
        Ok::<(), ()>(())
      })
    });
    assert!(matches!(
      result,
      Err(RuntimeGraphError::MissingHostPrimitive { .. })
    ));
    assert!(!registration_called);
  }

  #[test]
  fn validates_provenance_visibility_dependencies_and_host_abi() {
    let graph = manifest();
    graph
      .validate_for_host(&host())
      .expect("valid runtime graph");
    let plan = RuntimeGraphInstallPlan::prepare(graph, &host()).expect("prepare graph");
    assert!(plan.is_user_visible("effect/Effect"));
    assert!(plan.is_user_visible("blitz:app"));
    assert!(!plan.is_user_visible("blitz-internal:app-transport"));
  }

  #[test]
  fn missing_host_method_fails_before_registration_callback() {
    let graph = manifest();
    let host = HostBridgeCapabilities::new(HOST_BRIDGE_ABI_VERSION).with_primitive(
      "app-transport",
      HOST_BRIDGE_ABI_VERSION,
      ["openAppServer", "closeAppServer"],
    );
    let mut registration_called = false;
    let result = RuntimeGraphInstallPlan::prepare(graph, &host).map(|plan| {
      plan.register_after_validation(|_| {
        registration_called = true;
        Ok::<(), ()>(())
      })
    });
    let error = result.expect_err("host is incomplete");
    assert_eq!(
      error,
      RuntimeGraphError::MissingHostMethod {
        module: "blitz-internal:app-transport".to_string(),
        primitive: "app-transport".to_string(),
        method: "awaitShutdown".to_string(),
      }
    );

    assert!(
      !registration_called,
      "validation failed before registration"
    );
  }

  #[test]
  fn abi_and_visibility_fail_before_registration() {
    let mut graph = manifest();
    graph.runtime_abi_version += 1;
    assert!(matches!(
      RuntimeGraphInstallPlan::prepare(graph, &host()),
      Err(RuntimeGraphError::RuntimeAbiMismatch { .. })
    ));

    let mut graph = manifest();
    graph.modules[1].visibility = ModuleVisibility::Public;
    assert!(matches!(
      graph.validate(),
      Err(RuntimeGraphError::InvalidVisibility { .. })
    ));
  }

  #[test]
  fn malformed_and_duplicate_graph_entries_are_rejected() {
    let mut graph = manifest();
    graph.modules[2]
      .dependencies
      .push("effect/Effect".to_string());
    assert!(matches!(
      graph.validate(),
      Err(RuntimeGraphError::DuplicateDependency { .. })
    ));

    let mut graph = manifest();
    graph.modules[2].dependencies = vec!["effect/Missing".to_string()];
    assert!(matches!(
      graph.validate(),
      Err(RuntimeGraphError::MissingDependency { .. })
    ));
  }

  #[test]
  fn manifest_hashes_ignore_checkout_line_endings() {
    let lf = b"{\n  \"format\": 1\n}\n";
    let crlf = b"{\r\n  \"format\": 1\r\n}\r\n";
    assert_eq!(canonical_manifest_bytes(crlf).as_ref(), lf);
    assert_eq!(manifest_sha256(lf), manifest_sha256(crlf));
  }
}
