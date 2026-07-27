// SPDX-License-Identifier: MPL-2.0
//! Module loading and evaluation.
//!
//! Conformance notes and known deviations from ECMA-262
//!
//! `ModuleLoader` supports source, virtual, and prelinked module graphs. It
//! preserves live bindings across direct imports, indirect exports, star
//! exports, namespace exports, and cycles, including ambiguous-star detection.
//! The remaining differences from ECMA-262 are primarily in the representation
//! of namespace objects, host scheduling for dynamic import, and the algorithms
//! used to link and evaluate source graphs.
//!
//! 1) Module namespace objects
//! - Current behaviour: namespace imports (`import * as ns from 'mod'`) share
//!   one non-extensible object whose sorted accessor properties read live
//!   export slots. Export properties are enumerable and non-configurable, and
//!   the object exposes the `Module` toString tag.
//! - Spec behaviour: module namespace objects are exotic objects with
//!   specialized `[[GetOwnProperty]]`, `[[DefineOwnProperty]]`, `[[Set]]`,
//!   `[[Delete]]`, and `[[OwnPropertyKeys]]` behavior.
//! - Impact: live values, sorted keys, and namespace identity are preserved,
//!   while descriptor representation and operations that depend on the exotic
//!   internal methods can still differ from ECMA-262.
//!
//! 2) Dynamic import timing and async behaviour
//! - Current behaviour: `import()` synchronously constructs, instantiates, and
//!   evaluates the requested graph in a child VM, then queues fulfillment or
//!   rejection on the VM's dynamic-import job queue. Import options are
//!   evaluated but otherwise ignored.
//! - Spec behaviour: the host performs loading, linking, and evaluation through
//!   the dynamic-import host hooks and resolves or rejects the returned Promise
//!   through the prescribed job sequencing.
//! - Impact: the Promise settles asynchronously, but module work and its side
//!   effects happen before the queued settlement job. Host-hook timing, import
//!   attributes, and some Promise ordering remain observably different.
//!
//! 3) Linking/instantiation simplifications
//! - Current behaviour: source records are linked by scanning serialized ESTree
//!   to allocate slots and collect import/export metadata. `resolve_export`
//!   handles direct, indirect, and star exports, cycles, shared origin slots,
//!   and ambiguous star exports. Evaluation uses a synchronous DFS post-order
//!   with basic cycle tolerance.
//! - Spec behaviour: cyclic module records use the specification's linking and
//!   evaluation state machines, including their precise status and error
//!   propagation rules.
//! - Impact: the common static-module cases above are supported, but the
//!   implementation is not yet a complete Cyclic Module Record algorithm and
//!   source linking still depends on an ESTree serialization pass.
//!
//! Recommendations:
//! - Represent module namespaces as a dedicated exotic object kind while
//!   retaining the existing live export slots and shared identity.
//! - Move dynamic-import loading and evaluation behind the host job boundary
//!   and implement import attributes and host-hook error propagation.
//! - Replace serialized-ESTree linking and DFS evaluation with spec-oriented
//!   module-record linking and evaluation state machines.
mod compilation;
mod construction;
mod evaluation;
mod linking;
mod registration;
#[cfg(test)]
mod tests;

pub use compilation::evaluate_entry_shared;

use crate::bytecode::Chunk;
use crate::compiler::ModuleContext;
use crate::errors::BundlerError;
use crate::gc;
use crate::parser::ParserSession;
use crate::prelinked::PrelinkedModuleGraph;
use crate::resolver::resolve_specifier_with_conditions;
use crate::value::{ObjectData, PropertyDescriptor, PropertyKey, SharedValue, Value};
use crate::vm::Vm;

use oxc_span::SourceType;
use serde_json::Value as JsonValue;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};

/// A point-in-time count of source-module work performed by a module loader.
///
/// The counters belong to one [`ModuleLoader`] and do not include work for
/// records created from a prelinked module graph.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ModuleLoaderPipelineSnapshot {
  /// Number of source AST parses.
  pub source_parses: usize,
  /// Number of parses performed to extract import requests.
  pub import_extraction_parses: usize,
  /// Number of source AST/ESTree passes used for linking and instantiation.
  pub source_linking_passes: usize,
  /// Number of source typechecks.
  pub source_typechecks: usize,
  /// Number of source-to-bytecode compilations.
  pub source_compilations: usize,
}

#[derive(Default)]
struct ModuleLoaderPipelineCounters {
  source_parses: AtomicUsize,
  import_extraction_parses: AtomicUsize,
  source_linking_passes: AtomicUsize,
  source_typechecks: AtomicUsize,
  source_compilations: AtomicUsize,
}

impl ModuleLoaderPipelineCounters {
  fn snapshot(&self) -> ModuleLoaderPipelineSnapshot {
    ModuleLoaderPipelineSnapshot {
      source_parses: self.source_parses.load(Ordering::Relaxed),
      import_extraction_parses: self
        .import_extraction_parses
        .load(Ordering::Relaxed),
      source_linking_passes: self.source_linking_passes.load(Ordering::Relaxed),
      source_typechecks: self.source_typechecks.load(Ordering::Relaxed),
      source_compilations: self.source_compilations.load(Ordering::Relaxed),
    }
  }

  fn source_parse(&self) {
    self.source_parses.fetch_add(1, Ordering::Relaxed);
  }

  fn import_extraction_parse(&self) {
    self
      .import_extraction_parses
      .fetch_add(1, Ordering::Relaxed);
  }

  fn source_linking_pass(&self) {
    self
      .source_linking_passes
      .fetch_add(1, Ordering::Relaxed);
  }

  fn source_typecheck(&self) {
    self
      .source_typechecks
      .fetch_add(1, Ordering::Relaxed);
  }

  fn source_compilation(&self) {
    self
      .source_compilations
      .fetch_add(1, Ordering::Relaxed);
  }
}

fn source_type_for_module_path(path: &Path) -> SourceType {
  crate::parser::Parser::source_type_for_path(path, true)
}

const VIRTUAL_PATH_PREFIX: &str = "virtual://";

fn virtual_module_path(specifier: &str) -> PathBuf {
  PathBuf::from(format!("{VIRTUAL_PATH_PREFIX}{specifier}"))
}

/// Encode a filesystem path as the URL used by a module record.
///
/// This deliberately lives in the engine: it only describes a module's
/// canonical identity and does not consult any host services. Virtual module
/// identities already have URL-like specifiers and are returned unchanged.
fn module_url(
  path: &Path,
  specifier: &str,
) -> String {
  if path
    .to_string_lossy()
    .starts_with(VIRTUAL_PATH_PREFIX)
  {
    return specifier.to_string();
  }

  let path = path.to_string_lossy().replace('\\', "/");
  let path = if path.as_bytes().get(1) == Some(&b':') {
    format!("/{path}")
  } else if path.starts_with('/') {
    path
  } else {
    format!("/{path}")
  };
  let mut encoded = String::with_capacity(path.len());
  for byte in path.as_bytes() {
    if matches!(byte, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' | b':')
    {
      encoded.push(char::from(*byte));
    } else {
      encoded.push('%');
      encoded.push_str(&format!("{byte:02X}"));
    }
  }
  format!("file://{encoded}")
}

/// Resolve a relative module specifier in the namespace of a virtual module.
///
/// Virtual specifiers are URL-like names, not filesystem paths. In particular,
/// the part before `:` is a namespace and must remain part of the identity.
fn resolve_virtual_relative(
  importer: &str,
  requested: &str,
) -> String {
  if !(requested.starts_with("./") || requested.starts_with("../") || requested == ".") {
    return requested.to_string();
  }

  let (namespace, importer_path) = importer
    .split_once(':')
    .map_or(("", importer), |(namespace, path)| (namespace, path));
  let mut segments: Vec<&str> = importer_path.split('/').collect();
  if !segments.is_empty() {
    segments.pop();
  }
  for segment in requested.split('/') {
    match segment {
      "" | "." => {}
      ".." => {
        segments.pop();
      }
      other => segments.push(other),
    }
  }
  let path = segments.join("/");
  if namespace.is_empty() {
    path
  } else {
    format!("{namespace}:{path}")
  }
}

fn estree_module_export_name(node: Option<&JsonValue>) -> Option<String> {
  node
    .and_then(|node| {
      node
        .get("name")
        .and_then(JsonValue::as_str)
        .or_else(|| node.get("value").and_then(JsonValue::as_str))
    })
    .map(ToString::to_string)
}

fn stable_hash_bytes(bytes: impl IntoIterator<Item = u8>) -> u64 {
  // FNV-1a is deliberately used instead of the process-randomized standard
  // hasher: generated assets must have the same hash across hosts and runs.
  bytes
    .into_iter()
    .fold(0x0cbf_29ce_4842_2325_u64, |hash, byte| {
      (hash ^ u64::from(byte)).wrapping_mul(0x1000_0000_01b3)
    })
}

/// The executable payload supplied for a virtual module.
#[derive(Clone, Debug)]
pub enum VirtualModuleSource {
  /// JavaScript or TypeScript source which the engine parses and compiles.
  JavaScript(String),
  /// A source-free chunk generated by an embedder.
  Bytecode(Chunk),
  /// Source metadata plus bytecode generated for a stable module index.
  Precompiled {
    /// Original module source used for dependency and export discovery.
    source: String,
    /// Compiled executable chunk.
    chunk: Chunk,
    /// Stable module index embedded in import/export descriptors.
    module_index: usize,
  },
}

impl VirtualModuleSource {
  /// Return the source text when this payload contains source text.
  #[must_use]
  pub fn as_javascript(&self) -> Option<&str> {
    match self {
      Self::JavaScript(source) | Self::Precompiled { source, .. } => Some(source),
      Self::Bytecode(_) => None,
    }
  }

  fn content_hash(&self) -> u64 {
    match self {
      Self::JavaScript(source) => stable_hash_bytes(source.bytes()),
      Self::Bytecode(chunk) => {
        let code_bytes = chunk
          .code
          .iter()
          .flat_map(|word| word.to_le_bytes());
        stable_hash_bytes(chunk.source_text.bytes().chain(code_bytes))
      }
      Self::Precompiled {
        source,
        chunk,
        module_index,
      } => {
        let code_bytes = chunk
          .code
          .iter()
          .flat_map(|word| word.to_le_bytes());
        stable_hash_bytes(
          source
            .bytes()
            .chain(module_index.to_le_bytes())
            .chain(code_bytes),
        )
      }
    }
  }
}

/// A host-neutral module supplied by an embedder rather than the filesystem.
#[derive(Clone, Debug)]
pub struct VirtualModule {
  /// The exact specifier used by import and export declarations.
  pub canonical_specifier: String,
  /// The executable source or generated bytecode.
  pub source: VirtualModuleSource,
  /// The parser mode for JavaScript source.
  pub source_type: SourceType,
  /// Optional declaration text retained for an embedder's type checker.
  pub declaration_source: Option<String>,
  /// Stable FNV-1a content hash of the executable payload.
  pub content_hash: u64,
}

impl VirtualModule {
  /// Create a virtual module and derive its stable content hash.
  #[must_use]
  pub fn new(
    canonical_specifier: impl Into<String>,
    source: VirtualModuleSource,
    source_type: SourceType,
    declaration_source: Option<String>,
  ) -> Self {
    let canonical_specifier = canonical_specifier.into();
    let content_hash = stable_hash_bytes(
      source
        .content_hash()
        .to_le_bytes()
        .into_iter()
        .chain(
          declaration_source
            .as_deref()
            .unwrap_or_default()
            .bytes(),
        )
        .chain([
          u8::from(source_type.is_script()),
          u8::from(source_type.is_module()),
          u8::from(source_type.is_unambiguous()),
          u8::from(source_type.is_commonjs()),
          u8::from(source_type.is_javascript()),
          u8::from(source_type.is_typescript()),
          u8::from(source_type.is_jsx()),
          u8::from(source_type.is_strict()),
        ]),
    );
    Self {
      canonical_specifier,
      source,
      source_type,
      declaration_source,
      content_hash,
    }
  }

  /// Construct a source-backed virtual module.
  #[must_use]
  pub fn javascript(
    canonical_specifier: impl Into<String>,
    source: impl Into<String>,
    source_type: SourceType,
    declaration_source: Option<String>,
  ) -> Self {
    Self::new(
      canonical_specifier,
      VirtualModuleSource::JavaScript(source.into()),
      source_type,
      declaration_source,
    )
  }

  /// Construct a precompiled virtual module with a stable linked index.
  #[must_use]
  pub fn precompiled(
    canonical_specifier: impl Into<String>,
    source: impl Into<String>,
    source_type: SourceType,
    declaration_source: Option<String>,
    module_index: usize,
    chunk: Chunk,
  ) -> Self {
    Self::new(
      canonical_specifier,
      VirtualModuleSource::Precompiled {
        source: source.into(),
        chunk,
        module_index,
      },
      source_type,
      declaration_source,
    )
  }
}

/// Registry of embedder-provided virtual modules.
#[derive(Clone, Debug, Default)]
pub struct VirtualModuleRegistry {
  modules: HashMap<String, VirtualModule>,
  aliases: HashMap<String, String>,
}

impl VirtualModuleRegistry {
  /// Create an empty registry.
  #[must_use]
  pub fn new() -> Self {
    Self::default()
  }

  /// Register a module under its canonical specifier.
  ///
  /// Registration rejects empty, whitespace-containing, and duplicate
  /// specifiers so a module graph has one unambiguous identity.
  pub fn register(
    &mut self,
    module: VirtualModule,
  ) -> Result<(), BundlerError> {
    let specifier = module.canonical_specifier.clone();
    if specifier.is_empty() || specifier.chars().any(char::is_whitespace) {
      return Err(BundlerError::from(format!(
        "Invalid virtual module specifier: {specifier:?}"
      )));
    }
    if self.modules.contains_key(&specifier) || self.aliases.contains_key(&specifier) {
      return Err(BundlerError::from(format!(
        "Virtual module specifier already registered: {specifier}"
      )));
    }
    self.modules.insert(specifier, module);
    Ok(())
  }

  /// Register an alternate specifier for an existing canonical module.
  ///
  /// Aliases resolve to the same module record and preserve module identity.
  pub fn register_alias(
    &mut self,
    alias: impl Into<String>,
    canonical_specifier: impl Into<String>,
  ) -> Result<(), BundlerError> {
    let alias = alias.into();
    let canonical_specifier = canonical_specifier.into();
    if alias.is_empty()
      || alias.chars().any(char::is_whitespace)
      || canonical_specifier.is_empty()
      || canonical_specifier
        .chars()
        .any(char::is_whitespace)
    {
      return Err(BundlerError::from(format!(
        "Invalid virtual module alias: {alias:?}"
      )));
    }
    if !self.modules.contains_key(&canonical_specifier) {
      return Err(BundlerError::from(format!(
        "Virtual module alias target is not registered: {canonical_specifier}"
      )));
    }
    if self.modules.contains_key(&alias) || self.aliases.contains_key(&alias) {
      return Err(BundlerError::from(format!(
        "Virtual module specifier already registered: {alias}"
      )));
    }
    self.aliases.insert(alias, canonical_specifier);
    Ok(())
  }

  /// Return a registered module by canonical specifier.
  #[must_use]
  pub fn get(
    &self,
    specifier: &str,
  ) -> Option<&VirtualModule> {
    self.modules.get(specifier)
  }

  /// Resolve a canonical specifier or alias to its module.
  #[must_use]
  pub fn resolve(
    &self,
    specifier: &str,
  ) -> Option<&VirtualModule> {
    self.modules.get(specifier).or_else(|| {
      self
        .aliases
        .get(specifier)
        .and_then(|target| self.modules.get(target))
    })
  }

  /// Return the canonical identity for a specifier or alias.
  #[must_use]
  pub fn canonical_specifier(
    &self,
    specifier: &str,
  ) -> Option<String> {
    if self.modules.contains_key(specifier) {
      Some(specifier.to_string())
    } else {
      self.aliases.get(specifier).cloned()
    }
  }

  /// Return declaration source by canonical specifier or alias.
  #[must_use]
  pub fn declaration_source(
    &self,
    specifier: &str,
  ) -> Option<&str> {
    self
      .resolve(specifier)
      .and_then(|module| module.declaration_source.as_deref())
  }

  /// Iterate over registered aliases.
  pub fn iter_aliases(&self) -> impl Iterator<Item = (&str, &str)> {
    self
      .aliases
      .iter()
      .map(|(alias, canonical)| (alias.as_str(), canonical.as_str()))
  }

  /// Return the number of registered modules.
  #[must_use]
  pub fn len(&self) -> usize {
    self.modules.len()
  }

  /// Return whether the registry contains no modules.
  #[must_use]
  pub fn is_empty(&self) -> bool {
    self.modules.is_empty()
  }

  /// Iterate over modules in unspecified order.
  pub fn iter(&self) -> impl Iterator<Item = (&str, &VirtualModule)> {
    self
      .modules
      .iter()
      .map(|(specifier, module)| (specifier.as_str(), module))
  }
}

#[derive(Debug, PartialEq, Eq)]
enum ModuleState {
  Parsed,
  Instantiated,
  Evaluating,
  Evaluated,
}

/// Source-derived or parser-free data owned by a module record.
pub enum ModuleRecordPayload {
  /// A source module with its parser arena and dependency requests captured in
  /// source order during construction.
  Source {
    /// Parsed module session containing the AST and parser artifacts.
    session: ParserSession,
    /// Parser mode used for this module.
    source_type: SourceType,
    /// Requested module specifiers in deterministic source order.
    dependencies: Vec<String>,
  },
  /// A fully prelinked module requiring neither parser nor compiler state.
  Prelinked {
    /// Stable dependency module indices in source request order.
    dependencies: Vec<usize>,
  },
}

/// A cached source-backed or parser-free module and its runtime linking state.
pub struct ModuleRecord {
  /// Canonical path or virtual identity for the module.
  pub path: PathBuf,
  /// Canonical specifier for virtual modules, or the path string for files.
  pub specifier: String,
  /// URL derived from this record's canonical identity.
  pub url: String,
  /// Lazily-created `import.meta` object for this module record.
  import_meta: Option<Value>,
  /// Explicit source-backed or prelinked record payload.
  pub payload: ModuleRecordPayload,
  /// Optional generated bytecode payload for a virtual module.
  pub bytecode: Option<Chunk>,
  /// Optional declaration source supplied with a virtual module.
  pub declaration_source: Option<String>,
  /// Stable content hash supplied by the virtual module asset.
  pub content_hash: Option<u64>,
  /// Current loader lifecycle state for this module.
  state: ModuleState,
  /// Assigned numeric module index used by the VM module table. Set during instantiation.
  pub module_index: Option<usize>,
  /// Export slots: mapping export name -> shared slot.
  pub exports: HashMap<String, SharedValue>,
  /// Import bindings: local name -> exporter slot
  pub imports: HashMap<String, SharedValue>,
  /// Import metadata: local name -> (module_index, exported_name)
  pub import_sources: HashMap<String, (usize, String)>,
  /// Indirect export metadata: exported name -> (imported name, source specifier)
  pub indirect_exports: Vec<(String, String, String)>,
  /// Namespace re-export source metadata: (exported name, source specifier).
  pub namespace_export_sources: Vec<(String, String)>,
  /// Resolved namespace re-export targets: exported name -> module index.
  pub namespace_exports: HashMap<String, usize>,
  /// Star export sources.
  pub star_exports: Vec<String>,
  /// Whether this prelinked module is initialized entirely by its embedder.
  host_initialized: bool,
}

impl ModuleRecord {
  #[allow(clippy::too_many_arguments)]
  fn new_source(
    path: PathBuf,
    specifier: String,
    session: ParserSession,
    source_type: SourceType,
    dependencies: Vec<String>,
    bytecode: Option<Chunk>,
    declaration_source: Option<String>,
    content_hash: Option<u64>,
  ) -> Self {
    Self::new(
      path,
      specifier,
      ModuleRecordPayload::Source {
        session,
        source_type,
        dependencies,
      },
      bytecode,
      declaration_source,
      content_hash,
    )
  }

  #[allow(clippy::too_many_arguments)]
  fn new_prelinked(
    path: PathBuf,
    specifier: String,
    dependencies: Vec<usize>,
    module_index: usize,
    chunk: Chunk,
    declaration_source: Option<String>,
    content_hash: u64,
    host_initialized: bool,
  ) -> Self {
    let mut record = Self::new(
      path,
      specifier,
      ModuleRecordPayload::Prelinked { dependencies },
      Some(chunk),
      declaration_source,
      Some(content_hash),
    );
    record.module_index = Some(module_index);
    record.host_initialized = host_initialized;
    record
  }

  fn new(
    path: PathBuf,
    specifier: String,
    payload: ModuleRecordPayload,
    bytecode: Option<Chunk>,
    declaration_source: Option<String>,
    content_hash: Option<u64>,
  ) -> Self {
    let url = module_url(&path, &specifier);
    Self {
      path,
      specifier,
      url,
      import_meta: None,
      payload,
      bytecode,
      declaration_source,
      content_hash,
      state: ModuleState::Parsed,
      module_index: None,
      exports: HashMap::new(),
      imports: HashMap::new(),
      import_sources: HashMap::new(),
      indirect_exports: Vec::new(),
      namespace_export_sources: Vec::new(),
      namespace_exports: HashMap::new(),
      star_exports: Vec::new(),
      host_initialized: false,
    }
  }

  /// Return this record's parser session, if it is source-backed.
  #[must_use]
  pub fn parser_session(&self) -> Option<&ParserSession> {
    match &self.payload {
      ModuleRecordPayload::Source { session, .. } => Some(session),
      ModuleRecordPayload::Prelinked { .. } => None,
    }
  }

  /// Return this record's source parser mode, if it is source-backed.
  #[must_use]
  pub fn source_type(&self) -> Option<SourceType> {
    match &self.payload {
      ModuleRecordPayload::Source { source_type, .. } => Some(*source_type),
      ModuleRecordPayload::Prelinked { .. } => None,
    }
  }

  /// Return whether this record was constructed from a fully prelinked graph.
  #[must_use]
  pub fn is_prelinked(&self) -> bool {
    matches!(&self.payload, ModuleRecordPayload::Prelinked { .. })
  }

  fn import_meta(&mut self) -> Value {
    if let Some(meta) = &self.import_meta {
      return meta.clone();
    }

    let meta = Value::Object(gc::track_object(Rc::new(RefCell::new({
      let mut object = ObjectData::new();
      object.properties.insert(
        PropertyKey::String("url".to_string().into()),
        PropertyDescriptor::ordinary_data(Value::String(self.url.clone().into())),
      );
      object
    }))));
    self.import_meta = Some(meta.clone());
    meta
  }
}

#[derive(Clone, Copy, Debug)]
struct PrelinkedModuleLocation {
  graph_id: usize,
  module_position: usize,
}

/// A private prelinked module whose exports are supplied by the embedder.
///
/// Its bundled bytecode is retained for artifact validation but is not
/// executed. Every declared export must be initialized by the embedder.
#[derive(Clone)]
pub struct PrelinkedHostModule {
  /// Canonical private module specifier present in the prelinked graph.
  pub specifier: String,
  /// Complete export-name to realm-value mapping for the module.
  pub exports: HashMap<String, Value>,
}

struct RegisteredPrelinkedGraph {
  graph: Arc<PrelinkedModuleGraph>,
  declarations: HashMap<String, String>,
  host_module_bindings: HashMap<u32, Vec<(u32, Value)>>,
  binding_slots: Option<Vec<SharedValue>>,
}

/// ModuleLoader holds a per-VM module map and implements Construction,
/// Instantiation, and Evaluation per the ECMAScript module semantics.
pub struct ModuleLoader {
  /// Compatibility index of constructed records by path-shaped identity.
  pub map: HashMap<PathBuf, Rc<RefCell<ModuleRecord>>>,
  /// Embedder-provided modules checked before filesystem resolution.
  pub virtual_modules: VirtualModuleRegistry,
  /// Canonical graph identity index. Virtual modules are keyed only by their
  /// specifier here, never by a synthetic filesystem path.
  records_by_specifier: HashMap<String, Rc<RefCell<ModuleRecord>>>,
  /// Validated prelinked graphs registered for this realm.
  prelinked_graphs: Vec<RegisteredPrelinkedGraph>,
  /// Canonical prelinked module identity index, including private modules.
  prelinked_by_specifier: HashMap<String, PrelinkedModuleLocation>,
  /// Prelinked modules that source-facing resolution is allowed to reach.
  prelinked_public_by_specifier: HashMap<String, PrelinkedModuleLocation>,
  /// Reserved prelinked module-index lookup, including unconstructed modules.
  prelinked_by_index: HashMap<usize, PrelinkedModuleLocation>,
  /// Additional package export conditions selected by the embedder.
  condition_names: Vec<String>,
  /// Per-loader source pipeline counters shared with parallel compile workers.
  pipeline_counters: Arc<ModuleLoaderPipelineCounters>,
}

impl Default for ModuleLoader {
  fn default() -> Self {
    Self::new()
  }
}

impl ModuleLoader {
  /// Create an empty module loader with no cached records.
  pub fn new() -> Self {
    Self {
      map: HashMap::new(),
      virtual_modules: VirtualModuleRegistry::new(),
      records_by_specifier: HashMap::new(),
      prelinked_graphs: Vec::new(),
      prelinked_by_specifier: HashMap::new(),
      prelinked_public_by_specifier: HashMap::new(),
      prelinked_by_index: HashMap::new(),
      condition_names: Vec::new(),
      pipeline_counters: Arc::new(ModuleLoaderPipelineCounters::default()),
    }
  }

  /// Configure additional package export conditions for subsequent source resolution.
  pub fn set_condition_names(
    &mut self,
    condition_names: Vec<String>,
  ) {
    self.condition_names = condition_names;
  }

  /// Return the configured additional package export conditions.
  #[must_use]
  pub fn condition_names(&self) -> &[String] {
    &self.condition_names
  }
}
