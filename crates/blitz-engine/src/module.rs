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
//! - Impact: live values, sorted keys, descriptors, and namespace identity are
//!   preserved, but operations that depend on those exotic internal methods
//!   can still differ from ECMA-262.
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
use crate::bytecode::Chunk;
use crate::compiler::ModuleContext;
use crate::errors::BundlerError;
use crate::parser::ParserSession;
use crate::prelinked::PrelinkedModuleGraph;
use crate::resolver::resolve_specifier;
use crate::value::{SharedValue, Value};
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
    .fold(0xcbf2_9ce4_8422_325u64, |hash, byte| {
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

  fn new_prelinked(
    path: PathBuf,
    specifier: String,
    dependencies: Vec<usize>,
    module_index: usize,
    chunk: Chunk,
    declaration_source: Option<String>,
    content_hash: u64,
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
    Self {
      path,
      specifier,
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
}

#[derive(Clone, Copy, Debug)]
struct PrelinkedModuleLocation {
  graph_id: usize,
  module_position: usize,
}

struct RegisteredPrelinkedGraph {
  graph: Arc<PrelinkedModuleGraph>,
  declarations: HashMap<String, String>,
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
      pipeline_counters: Arc::new(ModuleLoaderPipelineCounters::default()),
    }
  }

  fn virtual_declarations_for_typecheck(&self) -> Vec<crate::typecheck::VirtualDeclaration> {
    let mut declarations = HashMap::new();
    for registration in &self.prelinked_graphs {
      declarations.extend(
        registration
          .declarations
          .iter()
          .filter(|(specifier, _)| {
            specifier.starts_with("effect/") || specifier.as_str() == "blitz:runtime"
          })
          .map(|(specifier, source)| (specifier.clone(), source.clone())),
      );
    }
    for record in self.map.values() {
      let record = record.borrow();
      if let Some(source) = &record.declaration_source {
        declarations
          .entry(record.specifier.clone())
          .or_insert_with(|| source.clone());
      }
    }
    declarations
      .into_iter()
      .map(|(specifier, source)| crate::typecheck::VirtualDeclaration { specifier, source })
      .collect()
  }

  /// Return a snapshot of this loader's source pipeline counters.
  #[must_use]
  pub fn pipeline_snapshot(&self) -> ModuleLoaderPipelineSnapshot {
    self.pipeline_counters.snapshot()
  }

  /// Register a validated, host-neutral prelinked graph for this realm.
  ///
  /// Registration validates the complete graph before mutating the loader,
  /// rejects identity/index collisions, and reserves every bundled module
  /// index without allocating runtime binding cells.
  ///
  /// # Errors
  ///
  /// Returns an error if validation fails or any specifier/module index is
  /// already registered or assigned in this loader.
  pub fn register_prelinked_graph(
    &mut self,
    graph: PrelinkedModuleGraph,
  ) -> Result<(), BundlerError> {
    self.register_prelinked_graph_with_declarations(graph, std::iter::empty::<(String, String)>())
  }

  /// Register a parser-free graph and declaration sources atomically.
  ///
  /// Declaration sources are keyed by canonical specifier. Every target must
  /// be a unique public root of `graph`; declarations for private or unknown
  /// modules are rejected before this loader is mutated.
  ///
  /// # Errors
  ///
  /// Returns an error if graph validation, declaration validation, or the
  /// normal graph registration checks fail.
  pub fn register_prelinked_graph_with_declarations<I, S, T>(
    &mut self,
    graph: PrelinkedModuleGraph,
    declarations: I,
  ) -> Result<(), BundlerError>
  where
    I: IntoIterator<Item = (S, T)>,
    S: Into<String>,
    T: Into<String>,
  {
    graph
      .validate()
      .map_err(|error| BundlerError::from(error.to_string()))?;

    let public_specifiers = graph
      .public_roots
      .iter()
      .filter_map(|root| {
        graph
          .module(*root)
          .map(|module| module.specifier.as_str())
      })
      .collect::<HashSet<_>>();
    let mut declarations_by_specifier = HashMap::new();
    for (specifier, source) in declarations {
      let specifier = specifier.into();
      let source = source.into();
      if !public_specifiers.contains(specifier.as_str()) {
        return Err(BundlerError::from(format!(
          "Prelinked declaration target {specifier:?} is not a public root"
        )));
      }
      if declarations_by_specifier
        .insert(specifier.clone(), source)
        .is_some()
      {
        return Err(BundlerError::from(format!(
          "Duplicate prelinked declaration target: {specifier}"
        )));
      }
    }

    let fixed_indices = self.fixed_module_indices();
    for module in &graph.modules {
      if self
        .virtual_modules
        .resolve(&module.specifier)
        .is_some()
        || self
          .prelinked_by_specifier
          .contains_key(&module.specifier)
        || self
          .records_by_specifier
          .contains_key(&module.specifier)
      {
        return Err(BundlerError::from(format!(
          "Module specifier already registered: {}",
          module.specifier
        )));
      }
      let module_index = usize::try_from(module.module_index).map_err(|_| {
        BundlerError::from(format!(
          "Prelinked module index {} does not fit usize",
          module.module_index
        ))
      })?;
      if let Some(owner) = fixed_indices.get(&module_index) {
        return Err(BundlerError::from(format!(
          "Prelinked module index {module_index} collides with {owner}"
        )));
      }
      if let Some(record) = self
        .map
        .values()
        .find(|record| record.borrow().module_index == Some(module_index))
      {
        return Err(BundlerError::from(format!(
          "Prelinked module index {module_index} collides with already indexed module {}",
          record.borrow().specifier
        )));
      }
    }

    let graph_id = self.prelinked_graphs.len();
    let public_roots = graph
      .public_roots
      .iter()
      .copied()
      .collect::<HashSet<_>>();
    let graph = Arc::new(graph);
    for (module_position, module) in graph.modules.iter().enumerate() {
      let module_index = usize::try_from(module.module_index).map_err(|_| {
        BundlerError::from(format!(
          "Prelinked module index {} does not fit usize",
          module.module_index
        ))
      })?;
      let location = PrelinkedModuleLocation {
        graph_id,
        module_position,
      };
      self
        .prelinked_by_specifier
        .insert(module.specifier.clone(), location);
      if public_roots.contains(&module.module_index) {
        self
          .prelinked_public_by_specifier
          .insert(module.specifier.clone(), location);
      }
      self
        .prelinked_by_index
        .insert(module_index, location);
    }
    self
      .prelinked_graphs
      .push(RegisteredPrelinkedGraph {
        graph,
        declarations: declarations_by_specifier,
        binding_slots: None,
      });
    Ok(())
  }

  /// Register an embedder-provided module for this loader's realm.
  pub fn register_virtual_module(
    &mut self,
    module: VirtualModule,
  ) -> Result<(), BundlerError> {
    if self
      .prelinked_by_specifier
      .contains_key(&module.canonical_specifier)
    {
      return Err(BundlerError::from(format!(
        "Module specifier already registered: {}",
        module.canonical_specifier
      )));
    }
    if let VirtualModuleSource::Precompiled { module_index, .. } = &module.source {
      if let Some(owner) = self.fixed_module_indices().get(module_index) {
        return Err(BundlerError::from(format!(
          "Precompiled module index {module_index} collides with {owner}"
        )));
      }
      if let Some(record) = self
        .map
        .values()
        .find(|record| record.borrow().module_index == Some(*module_index))
      {
        return Err(BundlerError::from(format!(
          "Precompiled module index {module_index} collides with already indexed module {}",
          record.borrow().specifier
        )));
      }
    }
    self.virtual_modules.register(module)
  }

  /// Register all modules from a registry.
  pub fn register_virtual_modules(
    &mut self,
    registry: VirtualModuleRegistry,
  ) -> Result<(), BundlerError> {
    for (_, module) in registry.iter() {
      self.register_virtual_module(module.clone())?;
    }
    for (alias, canonical) in registry.iter_aliases() {
      self.register_virtual_alias(alias, canonical)?;
    }
    Ok(())
  }

  /// Register an alias for an already registered virtual module.
  pub fn register_virtual_alias(
    &mut self,
    alias: impl Into<String>,
    canonical_specifier: impl Into<String>,
  ) -> Result<(), BundlerError> {
    let alias = alias.into();
    if self.prelinked_by_specifier.contains_key(&alias) {
      return Err(BundlerError::from(format!(
        "Module specifier already registered: {alias}"
      )));
    }
    self
      .virtual_modules
      .register_alias(alias, canonical_specifier)
  }

  /// Return declaration source by canonical specifier or alias.
  #[must_use]
  pub fn declaration_source(
    &self,
    specifier: &str,
  ) -> Option<&str> {
    self
      .virtual_modules
      .declaration_source(specifier)
      .or_else(|| {
        self
          .prelinked_graphs
          .iter()
          .find_map(|registration| registration.declarations.get(specifier))
          .map(String::as_str)
      })
  }

  /// Return declaration source using its canonical specifier.
  #[must_use]
  pub fn declaration_source_for_specifier(
    &self,
    specifier: &str,
  ) -> Option<&str> {
    self.declaration_source(specifier)
  }

  fn fixed_module_indices(&self) -> HashMap<usize, String> {
    let mut indices = HashMap::new();
    for (module_index, location) in &self.prelinked_by_index {
      let module = &self.prelinked_graphs[location.graph_id]
        .graph
        .modules[location.module_position];
      indices.insert(*module_index, module.specifier.clone());
    }
    for (_, module) in self.virtual_modules.iter() {
      if let VirtualModuleSource::Precompiled { module_index, .. } = &module.source {
        indices.insert(*module_index, module.canonical_specifier.clone());
      }
    }
    indices
  }

  fn construct_prelinked_at(
    &mut self,
    location: PrelinkedModuleLocation,
  ) -> Result<Rc<RefCell<ModuleRecord>>, BundlerError> {
    let module = self
      .prelinked_graphs
      .get(location.graph_id)
      .and_then(|registration| {
        registration
          .graph
          .modules
          .get(location.module_position)
      })
      .cloned()
      .ok_or_else(|| BundlerError::from("Missing registered prelinked module metadata"))?;
    if let Some(existing) = self.records_by_specifier.get(&module.specifier) {
      return Ok(Rc::clone(existing));
    }

    let module_index = usize::try_from(module.module_index).map_err(|_| {
      BundlerError::from(format!(
        "Prelinked module index {} does not fit usize",
        module.module_index
      ))
    })?;
    let dependencies = module
      .dependencies
      .iter()
      .map(|dependency| {
        usize::try_from(*dependency).map_err(|_| {
          BundlerError::from(format!(
            "Prelinked dependency index {dependency} does not fit usize"
          ))
        })
      })
      .collect::<Result<Vec<_>, _>>()?;
    let virtual_path = virtual_module_path(&module.specifier);
    let content_hash = stable_hash_bytes(module.source_sha256);
    let declaration_source = self
      .prelinked_graphs
      .get(location.graph_id)
      .and_then(|registration| registration.declarations.get(&module.specifier))
      .cloned();
    let record = Rc::new(RefCell::new(ModuleRecord::new_prelinked(
      virtual_path.clone(),
      module.specifier.clone(),
      dependencies.clone(),
      module_index,
      module.chunk,
      declaration_source,
      content_hash,
    )));
    // Preallocate the record identity before traversing bundled edges so
    // cycles always reuse this exact record.
    self
      .records_by_specifier
      .insert(module.specifier.clone(), Rc::clone(&record));
    self
      .map
      .insert(virtual_path.clone(), Rc::clone(&record));

    for dependency in dependencies {
      let Some(dependency_location) = self.prelinked_by_index.get(&dependency).copied() else {
        self
          .records_by_specifier
          .remove(&module.specifier);
        self.map.remove(&virtual_path);
        return Err(BundlerError::from(format!(
          "Prelinked dependency index {dependency} is not registered"
        )));
      };
      if let Err(error) = self.construct_prelinked_at(dependency_location) {
        self
          .records_by_specifier
          .remove(&module.specifier);
        self.map.remove(&virtual_path);
        return Err(BundlerError::from(format!(
          "Failed to construct prelinked dependency {dependency} imported by {}: {error}",
          module.specifier
        )));
      }
    }
    Ok(record)
  }

  /// Construct (fetch and parse) a module and its dependencies.
  ///
  /// The `base` argument is used for filesystem modules. Virtual modules are
  /// resolved by canonical specifier and never fall through to the filesystem.
  pub fn construct(
    &mut self,
    base: &Path,
    spec: &str,
  ) -> Result<Rc<RefCell<ModuleRecord>>, BundlerError> {
    self.construct_from_importer(base, None, spec)
  }

  /// Construct an import using the importing module's canonical identity.
  ///
  /// This is the common resolution entry point for construction, linking,
  /// export resolution, evaluation ordering, and dynamic import.
  pub fn construct_from_importer(
    &mut self,
    base: &Path,
    importer: Option<&str>,
    spec: &str,
  ) -> Result<Rc<RefCell<ModuleRecord>>, BundlerError> {
    let requested = importer.map_or_else(
      || spec.to_string(),
      |name| resolve_virtual_relative(name, spec),
    );
    if let Some(location) = self
      .prelinked_public_by_specifier
      .get(&requested)
      .copied()
    {
      return self.construct_prelinked_at(location);
    }
    if self
      .prelinked_by_specifier
      .contains_key(&requested)
    {
      return Err(BundlerError::from(format!(
        "Prelinked module specifier {requested:?} is private and cannot be imported"
      )));
    }
    if let Some(module) = self.virtual_modules.resolve(&requested).cloned() {
      let canonical = module.canonical_specifier.clone();
      if let Some(existing) = self.records_by_specifier.get(&canonical) {
        return Ok(Rc::clone(existing));
      }

      let source = module.source.as_javascript().unwrap_or_default();
      self.pipeline_counters.source_parse();
      let session = crate::parser::Parser::parse_source_with_type(source, module.source_type)
        .map_err(|error| {
          BundlerError::from(format!("Parse error virtual module {canonical}: {error}"))
        })?;
      self.pipeline_counters.import_extraction_parse();
      let imports =
        crate::parser::Parser::extract_imports_from_source_with_type(source, module.source_type)
          .map_err(|error| {
            BundlerError::from(format!("Parse error virtual module {canonical}: {error}"))
          })?;
      let (bytecode, module_index) = match &module.source {
        VirtualModuleSource::JavaScript(_) => (None, None),
        VirtualModuleSource::Bytecode(chunk) => (Some(chunk.clone()), None),
        VirtualModuleSource::Precompiled {
          chunk,
          module_index,
          ..
        } => (Some(chunk.clone()), Some(*module_index)),
      };
      let virtual_path = virtual_module_path(&canonical);
      let mut record = ModuleRecord::new_source(
        virtual_path.clone(),
        canonical.clone(),
        session,
        module.source_type,
        imports.clone(),
        bytecode,
        module.declaration_source.clone(),
        Some(module.content_hash),
      );
      record.module_index = module_index;
      let record = Rc::new(RefCell::new(record));
      // Insert before walking dependencies so cycles observe the same record.
      self
        .records_by_specifier
        .insert(canonical.clone(), Rc::clone(&record));
      self
        .map
        .insert(virtual_path.clone(), Rc::clone(&record));

      for dependency in imports {
        if let Err(error) = self.construct_from_importer(base, Some(&canonical), &dependency) {
          self.records_by_specifier.remove(&canonical);
          self.map.remove(&virtual_path);
          return Err(BundlerError::from(format!(
            "Failed to construct dependency {dependency:?} imported by {canonical}: {error}"
          )));
        }
      }
      return Ok(record);
    }

    if importer.is_some() {
      return Err(BundlerError::from(format!(
        "Cannot resolve virtual dependency {spec:?} (resolved as {requested:?}) from {}",
        importer.unwrap_or_default()
      )));
    }

    let resolved = resolve_specifier(base, spec)?;
    let resolved_canon = std::fs::canonicalize(&resolved).unwrap_or(resolved);
    let canonical = resolved_canon.to_string_lossy().into_owned();
    if let Some(existing) = self.records_by_specifier.get(&canonical) {
      return Ok(Rc::clone(existing));
    }

    let src = std::fs::read_to_string(&resolved_canon).map_err(|error| {
      BundlerError::from(format!(
        "Failed to read module {}: {error}",
        resolved_canon.display()
      ))
    })?;
    let source_type = source_type_for_module_path(&resolved_canon);
    self.pipeline_counters.source_parse();
    let session =
      crate::parser::Parser::parse_source_with_type(&src, source_type).map_err(|error| {
        BundlerError::from(format!("Parse error {}: {error}", resolved_canon.display()))
      })?;
    self.pipeline_counters.import_extraction_parse();
    let imports = crate::parser::Parser::extract_imports_from_source_with_type(&src, source_type)
      .map_err(|error| {
      BundlerError::from(format!("Parse error {}: {error}", resolved_canon.display()))
    })?;
    let record = Rc::new(RefCell::new(ModuleRecord::new_source(
      resolved_canon.clone(),
      canonical.clone(),
      session,
      source_type,
      imports.clone(),
      None,
      None,
      None,
    )));
    self
      .records_by_specifier
      .insert(canonical, Rc::clone(&record));
    self
      .map
      .insert(resolved_canon.clone(), Rc::clone(&record));
    let base_dir = resolved_canon
      .parent()
      .unwrap_or_else(|| Path::new("."));
    for dependency in imports {
      // Preserve the filesystem loader's handling of optional/bare host
      // dependencies, while making relative and registered virtual imports
      // strict. Virtual-module construction above is strict for every edge.
      if dependency.starts_with('.')
        || dependency.starts_with('/')
        || self
          .virtual_modules
          .resolve(&dependency)
          .is_some()
        || self
          .prelinked_by_specifier
          .contains_key(&dependency)
      {
        self.construct(base_dir, &dependency)?;
      } else {
        drop(self.construct(base_dir, &dependency));
      }
    }

    Ok(record)
  }

  /// Resolve all indirect and star exports before import bindings are wired.
  fn resolve_all_indirect_exports(
    &mut self,
    module_paths: &[PathBuf],
  ) -> Result<(), BundlerError> {
    for path in module_paths {
      let Some(rec_rc) = self.map.get(path).cloned() else {
        continue;
      };
      let (specifier, base_dir, indirect_exports) = {
        let rec = rec_rc.borrow();
        (
          rec.specifier.clone(),
          rec
            .path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf(),
          rec.indirect_exports.clone(),
        )
      };
      let importer = self
        .map
        .get(path)
        .is_some_and(|record| {
          record
            .borrow()
            .path
            .to_string_lossy()
            .starts_with(VIRTUAL_PATH_PREFIX)
        })
        .then_some(specifier.as_str());
      for (exported_name, imported_name, source_spec) in indirect_exports {
        let dependency = self.construct_from_importer(&base_dir, importer, &source_spec)?;
        let dependency_path = dependency.borrow().path.clone();
        let mut resolve_set = HashSet::new();
        let slot = self
          .resolve_export(&dependency_path, &imported_name, &mut resolve_set)?
          .ok_or_else(|| {
            BundlerError::from(format!(
              "Cannot resolve export {imported_name:?} from {source_spec:?} in {specifier}"
            ))
          })?;
        rec_rc
          .borrow_mut()
          .exports
          .insert(exported_name, slot);
      }
    }
    Ok(())
  }

  fn initialize_namespace_reexports(
    &self,
    vm: &mut Vm,
  ) {
    for record in self.map.values() {
      let record = record.borrow();
      for (exported, dependency_index) in &record.namespace_exports {
        let Some(namespace) = vm.module_namespace_value(*dependency_index) else {
          continue;
        };
        if let Some(slot) = record.exports.get(exported) {
          *slot.borrow_mut() = namespace;
        }
      }
    }
  }

  fn instantiate_prelinked_records(&mut self) -> Result<(), BundlerError> {
    let active_graphs = self
      .map
      .values()
      .filter_map(|record| {
        let record = record.borrow();
        if !matches!(record.state, ModuleState::Parsed)
          || !matches!(&record.payload, ModuleRecordPayload::Prelinked { .. })
        {
          return None;
        }
        let module_index = record.module_index?;
        self
          .prelinked_by_index
          .get(&module_index)
          .map(|location| location.graph_id)
      })
      .collect::<HashSet<_>>();

    // Allocate every canonical cell for each active graph before wiring or
    // marking even one of its records as instantiated.
    for graph_id in active_graphs {
      let registration = self
        .prelinked_graphs
        .get_mut(graph_id)
        .ok_or_else(|| BundlerError::from("Missing registered prelinked graph"))?;
      if registration.binding_slots.is_none() {
        let binding_count = usize::try_from(registration.graph.binding_count)
          .map_err(|_| BundlerError::from("Prelinked binding count does not fit usize"))?;
        registration.binding_slots = Some(
          (0..binding_count)
            .map(|_| Rc::new(RefCell::new(Value::Undefined)))
            .collect(),
        );
      }
    }

    let mut records_to_mark = Vec::new();
    for rec_rc in self.map.values() {
      let (module_index, should_instantiate) = {
        let record = rec_rc.borrow();
        (
          record.module_index,
          matches!(record.state, ModuleState::Parsed)
            && matches!(&record.payload, ModuleRecordPayload::Prelinked { .. }),
        )
      };
      if !should_instantiate {
        continue;
      }
      let module_index = module_index
        .ok_or_else(|| BundlerError::from("Prelinked module record has no module index"))?;
      let location = self
        .prelinked_by_index
        .get(&module_index)
        .copied()
        .ok_or_else(|| BundlerError::from("Prelinked module record index is not registered"))?;
      let registration = self
        .prelinked_graphs
        .get(location.graph_id)
        .ok_or_else(|| BundlerError::from("Missing registered prelinked graph"))?;
      let module = registration
        .graph
        .modules
        .get(location.module_position)
        .ok_or_else(|| BundlerError::from("Missing registered prelinked module"))?;
      let slots = registration
        .binding_slots
        .as_ref()
        .ok_or_else(|| BundlerError::from("Prelinked binding slots were not allocated"))?;
      let exports = module
        .exports
        .iter()
        .map(|export| {
          let binding_id = usize::try_from(export.binding_id)
            .map_err(|_| BundlerError::from("Prelinked export binding ID does not fit usize"))?;
          let slot = slots.get(binding_id).cloned().ok_or_else(|| {
            BundlerError::from(format!(
              "Prelinked export binding ID {} is out of range",
              export.binding_id
            ))
          })?;
          Ok((export.exported_name.clone(), slot))
        })
        .collect::<Result<HashMap<_, _>, BundlerError>>()?;
      let imports = module
        .imports
        .iter()
        .map(|import| {
          let binding_id = usize::try_from(import.binding_id)
            .map_err(|_| BundlerError::from("Prelinked import binding ID does not fit usize"))?;
          let slot = slots.get(binding_id).cloned().ok_or_else(|| {
            BundlerError::from(format!(
              "Prelinked import binding ID {} is out of range",
              import.binding_id
            ))
          })?;
          Ok((import.local_name.clone(), slot))
        })
        .collect::<Result<HashMap<_, _>, BundlerError>>()?;
      {
        let mut record = rec_rc.borrow_mut();
        record.exports = exports;
        record.imports = imports;
      }
      records_to_mark.push(Rc::clone(rec_rc));
    }

    for record in records_to_mark {
      record.borrow_mut().state = ModuleState::Instantiated;
    }
    Ok(())
  }

  fn assign_module_indices(
    &mut self,
    module_paths: &[PathBuf],
  ) -> Result<(), BundlerError> {
    let fixed_indices = self.fixed_module_indices();
    let mut assigned_indices = HashMap::new();
    let mut used_indices = fixed_indices
      .keys()
      .copied()
      .collect::<HashSet<_>>();

    for path in module_paths {
      let Some(record) = self.map.get(path) else {
        continue;
      };
      let record = record.borrow();
      let Some(index) = record.module_index else {
        continue;
      };
      if let Some(previous) = assigned_indices.insert(index, record.specifier.clone()) {
        return Err(BundlerError::from(format!(
          "Duplicate module index {index} for {previous} and {}",
          record.specifier
        )));
      }
      if let Some(owner) = fixed_indices.get(&index)
        && owner != &record.specifier
      {
        return Err(BundlerError::from(format!(
          "Module index {index} for {} is reserved by {owner}",
          record.specifier
        )));
      }
      used_indices.insert(index);
    }

    let mut next_index = used_indices
      .iter()
      .copied()
      .max()
      .map_or(Ok(0), |index| {
        index
          .checked_add(1)
          .ok_or_else(|| BundlerError::from("Module index space exhausted"))
      })?;
    for path in module_paths {
      let Some(rec_rc) = self.map.get(path) else {
        continue;
      };
      let mut record = rec_rc.borrow_mut();
      if record.module_index.is_some() {
        continue;
      }
      while used_indices.contains(&next_index) {
        next_index = next_index
          .checked_add(1)
          .ok_or_else(|| BundlerError::from("Module index space exhausted"))?;
      }
      record.module_index = Some(next_index);
      used_indices.insert(next_index);
      next_index = next_index
        .checked_add(1)
        .ok_or_else(|| BundlerError::from("Module index space exhausted"))?;
    }
    Ok(())
  }

  /// Instantiate modules by allocating export slots. This is a simple
  /// implementation that creates shared value slots for exported names so
  /// importers can hold live bindings to the same slot.
  ///
  /// # Errors
  ///
  /// Returns a [`crate::errors::BundlerError`] if a module fails to instantiate.
  #[allow(clippy::too_many_lines)]
  pub fn instantiate_all(&mut self) -> Result<(), BundlerError> {
    // For each module, examine its AST for exported names and allocate slots.
    for rec_rc in self.map.values() {
      let mut rec = rec_rc.borrow_mut();
      if matches!(rec.state, ModuleState::Parsed) {
        // Use ESTree JSON representation to find export declarations only for
        // source-backed records. Prelinked records never own parser state.
        let estree = match &rec.payload {
          ModuleRecordPayload::Source { session, .. } => {
            self.pipeline_counters.source_linking_pass();
            session
              .program()
              .to_pretty_estree_json(true, false)
          }
          ModuleRecordPayload::Prelinked { .. } => continue,
        };
        let root: JsonValue = serde_json::from_str(&estree)
          .map_err(|e| BundlerError::from(format!("Failed to parse ESTree JSON: {}", e)))?;
        let body = root
          .get("body")
          .and_then(|b| b.as_array())
          .ok_or_else(|| BundlerError::from("Expected program body"))?;
        for item in body {
          if let Some(typ) = item.get("type").and_then(|t| t.as_str()) {
            match typ {
              "ExportNamedDeclaration" => {
                if let Some(source) = item
                  .get("source")
                  .and_then(|s| s.get("value"))
                  .and_then(|v| v.as_str())
                {
                  if let Some(specs) = item.get("specifiers").and_then(|s| s.as_array()) {
                    for sp in specs {
                      let exported = sp
                        .get("exported")
                        .and_then(|e| e.get("name"))
                        .and_then(|n| n.as_str())
                        .unwrap_or("")
                        .to_string();
                      let imported = sp
                        .get("local")
                        .and_then(|e| e.get("name"))
                        .and_then(|n| n.as_str())
                        .unwrap_or(&exported)
                        .to_string();
                      rec
                        .indirect_exports
                        .push((exported, imported, source.to_string()));
                    }
                  }
                  continue;
                }
                if let Some(specs) = item.get("specifiers").and_then(|s| s.as_array()) {
                  for sp in specs {
                    if let Some(exported) = sp
                      .get("exported")
                      .and_then(|e| e.get("name"))
                      .and_then(|n| n.as_str())
                    {
                      rec.exports.insert(
                        exported.to_string(),
                        Rc::new(RefCell::new(Value::Undefined)),
                      );
                    }
                  }
                }
                if let Some(decl) = item.get("declaration")
                  && let Some(decl_type) = decl.get("type").and_then(|d| d.as_str())
                {
                  if (decl_type == "FunctionDeclaration" || decl_type == "ClassDeclaration")
                    && let Some(id) = decl
                      .get("id")
                      .and_then(|i| i.get("name"))
                      .and_then(|n| n.as_str())
                  {
                    rec
                      .exports
                      .insert(id.to_string(), Rc::new(RefCell::new(Value::Undefined)));
                  } else if decl_type == "VariableDeclaration"
                    && let Some(decls) = decl
                      .get("declarations")
                      .and_then(|d| d.as_array())
                  {
                    for var_decl in decls {
                      if let Some(name) = var_decl
                        .get("id")
                        .and_then(|id| id.get("name"))
                        .and_then(|n| n.as_str())
                      {
                        rec
                          .exports
                          .insert(name.to_string(), Rc::new(RefCell::new(Value::Undefined)));
                      }
                    }
                  }
                }
              }
              "ExportDefaultDeclaration" => {
                rec.exports.insert(
                  "default".to_string(),
                  Rc::new(RefCell::new(Value::Undefined)),
                );
              }
              "ExportAllDeclaration" => {
                if let Some(source) = item
                  .get("source")
                  .and_then(|s| s.get("value"))
                  .and_then(|v| v.as_str())
                {
                  if let Some(exported) = estree_module_export_name(item.get("exported")) {
                    rec
                      .exports
                      .insert(exported.clone(), Rc::new(RefCell::new(Value::Undefined)));
                    rec
                      .namespace_export_sources
                      .push((exported, source.to_string()));
                  } else {
                    rec.star_exports.push(source.to_string());
                  }
                }
              }
              _ => {}
            }
          }
        }

        rec.state = ModuleState::Instantiated;
      }
    }

    self.instantiate_prelinked_records()?;

    // Second pass: wire source imports to exporter slots so importers hold live
    // bindings to the same cells as source or prelinked exporters. We iterate
    // over a deterministic snapshot because resolution may discover records.
    let mut module_paths: Vec<PathBuf> = self.map.keys().cloned().collect();
    module_paths.sort();
    self.resolve_all_indirect_exports(&module_paths)?;
    // Reserve all registered fixed indices, including prelinked modules which
    // have not been constructed, before assigning any ordinary source index.
    self.assign_module_indices(&module_paths)?;

    // Resolve `export * as name from "module"` to the dependency's module
    // table index. The compiler emits a `GetModuleExport` for the dependency's
    // synthetic `*` namespace export, then stores that object in the named
    // export's live binding cell.
    for path in &module_paths {
      let Some(rec_rc) = self.map.get(path).cloned() else {
        continue;
      };
      let (specifier, base_dir, namespace_sources) = {
        let rec = rec_rc.borrow();
        (
          rec.specifier.clone(),
          rec
            .path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf(),
          rec.namespace_export_sources.clone(),
        )
      };
      let importer = self
        .map
        .get(path)
        .is_some_and(|record| {
          record
            .borrow()
            .path
            .to_string_lossy()
            .starts_with(VIRTUAL_PATH_PREFIX)
        })
        .then_some(specifier.as_str());
      for (exported, source_spec) in namespace_sources {
        let dependency = self.construct_from_importer(&base_dir, importer, &source_spec)?;
        let dependency_index = dependency.borrow().module_index.ok_or_else(|| {
          BundlerError::from("Namespace re-export dependency has no module index")
        })?;
        rec_rc
          .borrow_mut()
          .namespace_exports
          .insert(exported, dependency_index);
      }
    }

    for path in &module_paths {
      let rec_rc = match self.map.get(path) {
        Some(r) => Rc::clone(r),
        None => continue,
      };
      // Walk ESTree JSON import declarations to wire live-binding slots.
      // Borrow the module record only long enough to extract the AST.
      let estree = {
        let rec = rec_rc.borrow();
        match &rec.payload {
          ModuleRecordPayload::Source { session, .. } => {
            self.pipeline_counters.source_linking_pass();
            session
              .program()
              .to_pretty_estree_json(true, false)
          }
          ModuleRecordPayload::Prelinked { .. } => continue,
        }
      };
      let root: JsonValue = serde_json::from_str(&estree)
        .map_err(|e| BundlerError::from(format!("Failed to parse ESTree JSON: {}", e)))?;
      let body = root
        .get("body")
        .and_then(|b| b.as_array())
        .ok_or_else(|| BundlerError::from("Expected program body"))?;
      for item in body {
        if let Some(typ) = item.get("type").and_then(|t| t.as_str()) {
          if typ != "ImportDeclaration" {
            continue;
          }
        } else {
          continue;
        }

        let spec = item
          .get("source")
          .and_then(|s| s.get("value"))
          .and_then(|v| v.as_str())
          .unwrap_or("")
          .to_string();
        if spec.is_empty() {
          continue;
        }
        let (base_dir, importer_specifier) = {
          let rec = rec_rc.borrow();
          (
            rec
              .path
              .parent()
              .map_or_else(|| PathBuf::from("."), Path::to_path_buf),
            rec
              .path
              .to_string_lossy()
              .starts_with(VIRTUAL_PATH_PREFIX)
              .then(|| rec.specifier.clone()),
          )
        };
        let dep_rec_rc =
          self.construct_from_importer(&base_dir, importer_specifier.as_deref(), &spec)?;

        if let Some(specs) = item.get("specifiers").and_then(|s| s.as_array()) {
          for sp in specs {
            if let Some(sptype) = sp.get("type").and_then(|t| t.as_str()) {
              match sptype {
                "ImportSpecifier" => {
                  let local = sp
                    .get("local")
                    .and_then(|l| l.get("name"))
                    .and_then(|n| n.as_str())
                    .unwrap_or("")
                    .to_string();
                  let imported = sp
                    .get("imported")
                    .and_then(|i| i.get("name"))
                    .and_then(|n| n.as_str())
                    .map_or_else(|| Clone::clone(&local), ToString::to_string);
                  let dep_path = dep_rec_rc.borrow().path.clone();
                  let mut resolve_set = HashSet::new();
                  let slot_opt = self.resolve_export(&dep_path, &imported, &mut resolve_set)?;
                  let dep_module_index = dep_rec_rc.borrow().module_index;
                  if let Some(slot) = slot_opt {
                    {
                      let mut rec = rec_rc.borrow_mut();
                      rec
                        .imports
                        .insert(local.clone(), Rc::clone(&slot));
                      if let Some(src_idx) = dep_module_index {
                        rec
                          .import_sources
                          .insert(local, (src_idx, imported));
                      }
                    }
                  } else {
                    return Err(BundlerError::from(format!(
                      "Module {} does not export {imported:?}",
                      rec_rc.borrow().specifier
                    )));
                  }
                }
                "ImportDefaultSpecifier" => {
                  let local = sp
                    .get("local")
                    .and_then(|l| l.get("name"))
                    .and_then(|n| n.as_str())
                    .unwrap_or("")
                    .to_string();
                  let dep_path = dep_rec_rc.borrow().path.clone();
                  let mut resolve_set = HashSet::new();
                  let slot_opt = self.resolve_export(&dep_path, "default", &mut resolve_set)?;
                  let dep_module_index = dep_rec_rc.borrow().module_index;
                  if let Some(slot) = slot_opt {
                    {
                      let mut rec = rec_rc.borrow_mut();
                      rec
                        .imports
                        .insert(local.clone(), Rc::clone(&slot));
                      if let Some(src_idx) = dep_module_index {
                        rec
                          .import_sources
                          .insert(local, (src_idx, "default".to_string()));
                      }
                    }
                  } else {
                    return Err(BundlerError::from(format!(
                      "Module {} does not export \"default\"",
                      rec_rc.borrow().specifier
                    )));
                  }
                }
                "ImportNamespaceSpecifier" => {
                  let local = sp
                    .get("local")
                    .and_then(|l| l.get("name"))
                    .and_then(|n| n.as_str())
                    .unwrap_or("")
                    .to_string();
                  let exports_snapshot = { dep_rec_rc.borrow().exports.clone() };
                  let dep_module_index = dep_rec_rc.borrow().module_index;
                  let boxed = Rc::new(RefCell::new(
                    crate::vm::runtime::execution::build_module_namespace_object(&exports_snapshot),
                  ));
                  {
                    let mut rec = rec_rc.borrow_mut();
                    rec.imports.insert(local.clone(), boxed);
                  }
                  // Namespace imports are represented as plain objects for now.
                  // Record a synthetic import source with module index and special "*" name
                  if let Some(src_idx) = dep_module_index {
                    rec_rc
                      .borrow_mut()
                      .import_sources
                      .insert(local, (src_idx, "*".to_string()));
                  }
                }
                _ => {}
              }
            }
          }
        }
      }
    }

    Ok(())
  }

  /// Compute depth-first, post-order evaluation order starting from `entry`.
  ///
  /// # Errors
  ///
  /// Returns a [`crate::errors::BundlerError`] if the entry cannot be resolved.
  ///
  /// # Panics
  ///
  /// Panics if the entry cannot be resolved to a canonical path.
  pub fn evaluation_order_from(
    &mut self,
    entry: &Path,
  ) -> Result<Vec<PathBuf>, BundlerError> {
    let resolved_canon = if self.map.contains_key(entry) {
      entry.to_path_buf()
    } else if let Some(specifier) = entry.to_str()
      && self
        .prelinked_public_by_specifier
        .contains_key(specifier)
    {
      virtual_module_path(specifier)
    } else if let Some(module) = entry
      .to_str()
      .and_then(|specifier| self.virtual_modules.resolve(specifier))
    {
      virtual_module_path(&module.canonical_specifier)
    } else {
      let resolved = resolve_specifier(
        entry.parent().unwrap_or_else(|| Path::new(".")),
        &format!(
          "./{}",
          entry
            .file_name()
            .expect("entry must have a file name")
            .to_string_lossy()
        ),
      )?;
      std::fs::canonicalize(&resolved).unwrap_or(resolved)
    };
    let mut temp: HashMap<PathBuf, bool> = HashMap::new();
    let mut perm: HashMap<PathBuf, bool> = HashMap::new();
    let mut order: Vec<PathBuf> = Vec::new();

    fn visit(
      loader: &mut ModuleLoader,
      path: &PathBuf,
      temp: &mut HashMap<PathBuf, bool>,
      perm: &mut HashMap<PathBuf, bool>,
      order: &mut Vec<PathBuf>,
    ) -> Result<(), BundlerError> {
      if perm.get(path).copied().unwrap_or(false) {
        return Ok(());
      }
      if temp.get(path).copied().unwrap_or(false) {
        // cycle detected — per spec cycles are allowed; continue without error
        return Ok(());
      }
      temp.insert(path.clone(), true);

      enum OrderedDependencies {
        Source {
          requests: Vec<String>,
          base_dir: PathBuf,
          importer: Option<String>,
        },
        Prelinked(Vec<usize>),
      }

      let dependencies = if let Some(record) = loader.map.get(path) {
        let record = record.borrow();
        match &record.payload {
          ModuleRecordPayload::Source { dependencies, .. } => OrderedDependencies::Source {
            requests: dependencies.clone(),
            base_dir: record
              .path
              .parent()
              .unwrap_or_else(|| Path::new("."))
              .to_path_buf(),
            importer: record
              .path
              .to_string_lossy()
              .starts_with(VIRTUAL_PATH_PREFIX)
              .then(|| record.specifier.clone()),
          },
          ModuleRecordPayload::Prelinked { dependencies } => {
            OrderedDependencies::Prelinked(dependencies.clone())
          }
        }
      } else {
        let source = std::fs::read_to_string(path).map_err(|error| {
          BundlerError::from(format!("Failed to read module {}: {error}", path.display()))
        })?;
        let source_type = source_type_for_module_path(path);
        loader.pipeline_counters.import_extraction_parse();
        let requests =
          crate::parser::Parser::extract_imports_from_source_with_type(&source, source_type)
            .map_err(|error| {
              BundlerError::from(format!("Parse error {}: {error}", path.display()))
            })?;
        OrderedDependencies::Source {
          requests,
          base_dir: path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf(),
          importer: None,
        }
      };

      match dependencies {
        OrderedDependencies::Source {
          requests,
          base_dir,
          importer,
        } => {
          for specifier in requests {
            let dependency =
              loader.construct_from_importer(&base_dir, importer.as_deref(), &specifier)?;
            let dependency_path = dependency.borrow().path.clone();
            visit(loader, &dependency_path, temp, perm, order)?;
          }
        }
        OrderedDependencies::Prelinked(dependencies) => {
          for dependency_index in dependencies {
            let location = loader
              .prelinked_by_index
              .get(&dependency_index)
              .copied()
              .ok_or_else(|| {
                BundlerError::from(format!(
                  "Prelinked dependency index {dependency_index} is not registered"
                ))
              })?;
            let dependency = loader.construct_prelinked_at(location)?;
            let dependency_path = dependency.borrow().path.clone();
            visit(loader, &dependency_path, temp, perm, order)?;
          }
        }
      }

      temp.remove(path);
      perm.insert(path.clone(), true);
      order.push(path.clone());
      Ok(())
    }

    if !self.map.contains_key(&resolved_canon) {
      // Ensure a caller may pass either a virtual specifier or a filesystem
      // entry path.
      let base_dir = entry.parent().unwrap_or_else(|| Path::new("."));
      if self
        .virtual_modules
        .resolve(entry.to_string_lossy().as_ref())
        .is_some()
        || self
          .prelinked_public_by_specifier
          .contains_key(entry.to_string_lossy().as_ref())
      {
        self.construct(base_dir, entry.to_string_lossy().as_ref())?;
      } else {
        self.construct(
          base_dir,
          &format!(
            "./{}",
            entry
              .file_name()
              .expect("entry must have a file name")
              .to_string_lossy()
          ),
        )?;
      }
    }

    visit(self, &resolved_canon, &mut temp, &mut perm, &mut order)?;
    Ok(order)
  }

  /// Resolve an export from a module.
  ///
  /// # Errors
  ///
  /// Returns a [`crate::errors::RuntimeError`] if evaluation fails.
  fn resolve_export(
    &mut self,
    module_path: &Path,
    export_name: &str,
    resolve_set: &mut HashSet<(String, String)>,
  ) -> Result<Option<SharedValue>, BundlerError> {
    let module_path =
      std::fs::canonicalize(module_path).unwrap_or_else(|_| module_path.to_path_buf());
    let rec_rc = match self.map.get(&module_path) {
      Some(r) => Rc::clone(r),
      None => return Ok(None),
    };
    let (specifier, base_dir, direct_slot, indirect_exports, star_exports) = {
      let rec = rec_rc.borrow();
      (
        rec.specifier.clone(),
        rec
          .path
          .parent()
          .unwrap_or_else(|| Path::new("."))
          .to_path_buf(),
        rec.exports.get(export_name).cloned(),
        rec.indirect_exports.clone(),
        rec.star_exports.clone(),
      )
    };
    let key = (specifier.clone(), export_name.to_string());
    if !resolve_set.insert(key) {
      return Ok(None);
    }
    let importer = self
      .map
      .get(&module_path)
      .is_some_and(|record| {
        record
          .borrow()
          .path
          .to_string_lossy()
          .starts_with(VIRTUAL_PATH_PREFIX)
      })
      .then_some(specifier.as_str());

    if let Some(slot) = direct_slot {
      return Ok(Some(slot));
    }

    for (exported_name, imported_name, source_spec) in indirect_exports {
      if exported_name == export_name {
        let dep_rec_rc = self.construct_from_importer(&base_dir, importer, &source_spec)?;
        let dep_path = dep_rec_rc.borrow().path.clone();
        return self.resolve_export(&dep_path, &imported_name, resolve_set);
      }
    }

    let mut found: Option<SharedValue> = None;
    for source_spec in star_exports {
      let dep_rec_rc = self.construct_from_importer(&base_dir, importer, &source_spec)?;
      let dep_path = dep_rec_rc.borrow().path.clone();
      let mut branch_set = resolve_set.clone();
      if let Some(slot) = self.resolve_export(&dep_path, export_name, &mut branch_set)? {
        match &found {
          None => found = Some(slot),
          Some(existing) => {
            if !Rc::ptr_eq(existing, &slot) {
              return Ok(None);
            }
          }
        }
      }
    }

    Ok(found)
  }

  /// Evaluate a module graph starting from entry in DFS post-order.
  ///
  /// # Errors
  ///
  /// Returns a [`crate::errors::RuntimeError`] if evaluation fails.
  ///
  /// # Panics
  ///
  /// Panics if the entry cannot be resolved to a canonical path.
  pub fn evaluate_entry(
    &mut self,
    entry: &Path,
    vm: &mut Vm,
    enable_typecheck: bool,
  ) -> Result<(), crate::errors::RuntimeError> {
    // Populate vm.module_table with the export slot maps for each
    // instantiated module. ModuleRecord.module_index must have been set
    // during instantiate_all(). We clone the HashMap of slots so the VM
    // receives ownership of the map structure but still shares the live
    // binding slots.
    for rec_rc in self.map.values() {
      let rec = rec_rc.borrow();
      if let Some(idx) = rec.module_index {
        let exports_clone = rec.exports.clone();
        vm.install_module_exports(idx, exports_clone);
      }
    }
    self.initialize_namespace_reexports(vm);
    let order = self
      .evaluation_order_from(entry)
      .map_err(|e| crate::errors::RuntimeError::from(format!("Module resolution failed: {}", e)))?;
    let declarations = self.virtual_declarations_for_typecheck();

    for path in order {
      if let Some(rec_rc) = self.map.get(&path) {
        let mut rec = rec_rc.borrow_mut();
        if matches!(rec.state, ModuleState::Evaluating) {
          // Basic cycle-tolerance: if this record is already in-progress,
          // do not recursively re-enter evaluation.
          continue;
        }
        if matches!(rec.state, ModuleState::Instantiated) {
          rec.state = ModuleState::Evaluating;
          let chunk = match &rec.payload {
            ModuleRecordPayload::Prelinked { .. } => rec.bytecode.clone().ok_or_else(|| {
              crate::errors::RuntimeError::from(
                "Prelinked module record is missing its validated bytecode chunk",
              )
            })?,
            ModuleRecordPayload::Source {
              session,
              dependencies,
              ..
            } => {
              let program = session.program();
              if enable_typecheck {
                self.pipeline_counters.source_typecheck();
                crate::typecheck::check_with_declarations_and_source_imports(
                  program,
                  &declarations,
                  dependencies,
                )
                .map_err(|error| {
                  crate::errors::RuntimeError::from(format!("Typecheck failed: {error}"))
                })?;
              }
              let module_index = rec
                .module_index
                .expect("module_index should be set");
              let context = ModuleContext {
                module_index,
                imports: rec.import_sources.clone(),
                exports: rec.exports.keys().cloned().collect(),
                namespace_exports: rec.namespace_exports.clone(),
              };
              let chunk = if let Some(chunk) = rec.bytecode.clone() {
                chunk
              } else {
                self.pipeline_counters.source_compilation();
                crate::compiler::compile_oxc_with_ctx(program, Some(&context), session.source())
                  .map_err(|error| {
                    crate::errors::RuntimeError::from(format!("Compile failed: {error}"))
                  })?
              };
              crate::runtime_utils::clear_slot_types_in_chunk(chunk)
            }
          };
          vm.set_current_module_specifier(Some(rec.specifier.clone()));
          let result = vm.run(chunk);
          vm.set_current_module_specifier(None);
          result?;
          rec.state = ModuleState::Evaluated;
        }
      }
    }

    Ok(())
  }

  /// Evaluate module graph in depth-first, post-order. For now synchronous.
  ///
  /// # Errors
  ///
  /// Returns a [`crate::errors::RuntimeError`] if evaluation fails.
  pub fn evaluate_all(
    &mut self,
    vm: &mut Vm,
    enable_typecheck: bool,
  ) -> Result<(), crate::errors::RuntimeError> {
    for rec_rc in self.map.values() {
      let record = rec_rc.borrow();
      if let Some(module_index) = record.module_index {
        vm.install_module_exports(module_index, record.exports.clone());
      }
    }
    self.initialize_namespace_reexports(vm);
    let declarations = self.virtual_declarations_for_typecheck();
    // For simplicity evaluate modules in insertion order after instantiation.
    for rec_rc in self.map.values() {
      let mut rec = rec_rc.borrow_mut();
      if matches!(rec.state, ModuleState::Instantiated) {
        rec.state = ModuleState::Evaluating;
        let chunk = match &rec.payload {
          ModuleRecordPayload::Prelinked { .. } => rec.bytecode.clone().ok_or_else(|| {
            crate::errors::RuntimeError::from(
              "Prelinked module record is missing its validated bytecode chunk",
            )
          })?,
          ModuleRecordPayload::Source {
            session,
            dependencies,
            ..
          } => {
            let program = session.program();
            if enable_typecheck {
              self.pipeline_counters.source_typecheck();
              crate::typecheck::check_with_declarations_and_source_imports(
                program,
                &declarations,
                dependencies,
              )
              .map_err(|error| {
                crate::errors::RuntimeError::from(format!("Typecheck failed: {error}"))
              })?;
            }
            let chunk = if let Some(chunk) = rec.bytecode.clone() {
              chunk
            } else {
              self.pipeline_counters.source_compilation();
              crate::compiler::compile_oxc_with_ctx(program, None, session.source()).map_err(
                |error| crate::errors::RuntimeError::from(format!("Compile failed: {error}")),
              )?
            };
            crate::runtime_utils::clear_slot_types_in_chunk(chunk)
          }
        };
        vm.set_current_module_specifier(Some(rec.specifier.clone()));
        let result = vm.run(chunk);
        vm.set_current_module_specifier(None);
        result?;
        rec.state = ModuleState::Evaluated;
      }
    }
    Ok(())
  }
}

struct ModuleCompileJob {
  path: PathBuf,
  source: String,
  source_type: SourceType,
  context: ModuleContext,
}

fn compile_modules_parallel(
  jobs: Vec<ModuleCompileJob>,
  pipeline_counters: Arc<ModuleLoaderPipelineCounters>,
) -> Result<HashMap<PathBuf, Chunk>, crate::errors::RuntimeError> {
  if jobs.len() < 2 {
    return Ok(HashMap::new());
  }

  let worker_count = std::thread::available_parallelism()
    .map_or(1, usize::from)
    .min(8)
    .min(jobs.len());
  let jobs = Arc::new(jobs);
  let next_job = AtomicUsize::new(0);
  let (sender, receiver) = mpsc::channel();

  std::thread::scope(|scope| -> Result<(), crate::errors::RuntimeError> {
    for _ in 0..worker_count {
      let jobs = Arc::clone(&jobs);
      let sender = sender.clone();
      let next_job = &next_job;
      let pipeline_counters = Arc::clone(&pipeline_counters);
      std::thread::Builder::new()
        .name("blitz-module-compiler".to_string())
        .stack_size(16 * 1024 * 1024)
        .spawn_scoped(scope, move || {
          loop {
            let index = next_job.fetch_add(1, Ordering::Relaxed);
            let Some(job) = jobs.get(index) else {
              break;
            };
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
              pipeline_counters.source_parse();
              crate::parser::Parser::parse_source_with_type(&job.source, job.source_type)
                .map_err(|error| format!("Parse failed: {error}"))
                .and_then(|session| {
                  pipeline_counters.source_compilation();
                  crate::compiler::compile_oxc_with_ctx(
                    session.program(),
                    Some(&job.context),
                    &job.source,
                  )
                  .map(crate::runtime_utils::clear_slot_types_in_chunk)
                  .map_err(|error| format!("Compile failed: {error}"))
                })
            }))
            .unwrap_or_else(|payload| {
              let message = payload
                .downcast_ref::<&str>()
                .map_or_else(
                  || {
                    payload
                      .downcast_ref::<String>()
                      .map(String::as_str)
                  },
                  |message| Some(*message),
                )
                .unwrap_or("unknown compiler panic");
              Err(format!("Compiler worker panicked: {message}"))
            });
            if sender
              .send((index, job.path.clone(), result))
              .is_err()
            {
              break;
            }
          }
        })
        .map_err(|error| {
          crate::errors::RuntimeError::from(format!("Failed to spawn compiler worker: {error}"))
        })?;
    }
    Ok(())
  })?;
  drop(sender);

  let mut results: Vec<_> = receiver.into_iter().collect();
  results.sort_unstable_by_key(|(index, _, _)| *index);
  let mut compiled = HashMap::with_capacity(jobs.len());
  for (_, path, result) in results {
    let chunk = result.map_err(crate::errors::RuntimeError::from)?;
    compiled.insert(path, chunk);
  }
  Ok(compiled)
}

/// Evaluate a module graph using a shared loader handle.
///
/// Compilation is performed concurrently, while evaluation remains ordered
/// and single-threaded on `vm`. This helper borrows the loader only for short
/// phases so nested dynamic imports can re-enter the same cache while module
/// code is running.
///
/// # Errors
///
/// Returns a [`crate::errors::RuntimeError`] if compilation or evaluation fails.
///
/// # Panics
///
/// Panics if the entry cannot be resolved to a canonical path.
pub fn evaluate_entry_shared(
  loader: &Rc<RefCell<ModuleLoader>>,
  entry: &Path,
  vm: &mut Vm,
  enable_typecheck: bool,
) -> Result<(), crate::errors::RuntimeError> {
  {
    let loader_ref = loader.borrow();
    for rec_rc in loader_ref.map.values() {
      let rec = rec_rc.borrow();
      if let Some(idx) = rec.module_index {
        let exports_clone = rec.exports.clone();
        vm.install_module_exports(idx, exports_clone);
      }
    }
    loader_ref.initialize_namespace_reexports(vm);
  }

  let order = {
    let mut loader_ref = loader.borrow_mut();
    loader_ref
      .evaluation_order_from(entry)
      .map_err(|e| crate::errors::RuntimeError::from(format!("Module resolution failed: {}", e)))?
  };

  let declarations = {
    let loader_ref = loader.borrow();
    loader_ref.virtual_declarations_for_typecheck()
  };

  let compile_jobs = {
    let loader_ref = loader.borrow();
    order
      .iter()
      .filter_map(|path| {
        let record = loader_ref.map.get(path)?.borrow();
        if !matches!(record.state, ModuleState::Instantiated) || record.bytecode.is_some() {
          return None;
        }
        let ModuleRecordPayload::Source {
          session,
          source_type,
          ..
        } = &record.payload
        else {
          return None;
        };
        Some(ModuleCompileJob {
          path: path.clone(),
          source: session.source().to_string(),
          source_type: *source_type,
          context: ModuleContext {
            module_index: record
              .module_index
              .expect("module_index should be set"),
            imports: record.import_sources.clone(),
            exports: record.exports.keys().cloned().collect(),
            namespace_exports: record.namespace_exports.clone(),
          },
        })
      })
      .collect()
  };
  let pipeline_counters = {
    let loader_ref = loader.borrow();
    Arc::clone(&loader_ref.pipeline_counters)
  };
  let mut compiled = compile_modules_parallel(compile_jobs, pipeline_counters)?;

  for path in order {
    let maybe_chunk = {
      let loader_ref = loader.borrow_mut();
      let Some(rec_rc) = loader_ref.map.get(&path).cloned() else {
        continue;
      };
      let rec_rc_return = Rc::clone(&rec_rc);
      let mut rec = rec_rc.borrow_mut();
      if matches!(rec.state, ModuleState::Evaluating) {
        None
      } else if matches!(rec.state, ModuleState::Instantiated) {
        rec.state = ModuleState::Evaluating;
        let chunk = match &rec.payload {
          ModuleRecordPayload::Prelinked { .. } => rec.bytecode.clone().ok_or_else(|| {
            crate::errors::RuntimeError::from(
              "Prelinked module record is missing its validated bytecode chunk",
            )
          })?,
          ModuleRecordPayload::Source {
            session,
            dependencies,
            ..
          } => {
            let program = session.program();
            if enable_typecheck {
              loader_ref.pipeline_counters.source_typecheck();
              crate::typecheck::check_with_declarations_and_source_imports(
                program,
                &declarations,
                dependencies,
              )
              .map_err(|error| {
                crate::errors::RuntimeError::from(format!("Typecheck failed: {error}"))
              })?;
            }
            let module_index = rec
              .module_index
              .expect("module_index should be set");
            let context = ModuleContext {
              module_index,
              imports: rec.import_sources.clone(),
              exports: rec.exports.keys().cloned().collect(),
              namespace_exports: rec.namespace_exports.clone(),
            };
            let chunk = if let Some(chunk) = rec.bytecode.clone() {
              crate::runtime_utils::clear_slot_types_in_chunk(chunk)
            } else if let Some(chunk) = compiled.remove(&path) {
              chunk
            } else {
              loader_ref.pipeline_counters.source_compilation();
              let chunk =
                crate::compiler::compile_oxc_with_ctx(program, Some(&context), session.source())
                  .map_err(|error| {
                    crate::errors::RuntimeError::from(format!("Compile failed: {error}"))
                  })?;
              crate::runtime_utils::clear_slot_types_in_chunk(chunk)
            };
            rec.bytecode = Some(chunk.clone());
            chunk
          }
        };
        Some((rec_rc_return, chunk))
      } else {
        None
      }
    };

    if let Some((rec_rc, chunk)) = maybe_chunk {
      let specifier = rec_rc.borrow().specifier.clone();
      vm.set_current_module_specifier(Some(specifier));
      let result = vm.run(chunk);
      vm.set_current_module_specifier(None);
      result?;
      rec_rc.borrow_mut().state = ModuleState::Evaluated;
    }
  }

  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::prelinked::{
    PRELINKED_MODULE_GRAPH_VERSION, PrelinkedExport, PrelinkedImport, PrelinkedModule,
  };
  use crate::value::{ObjectData, Value};
  use std::cell::RefCell;
  use std::rc::Rc;

  fn replace_debug_source(
    chunk: &mut Chunk,
    debug_source: &Arc<String>,
  ) {
    chunk.source_text = Arc::clone(debug_source);
    for function in &mut chunk.functions {
      replace_debug_source(Arc::make_mut(&mut function.chunk), debug_source);
    }
  }

  fn compile_prelinked_chunk(
    source: &str,
    module_index: usize,
    imports: &[(&str, usize, &str)],
    exports: &[&str],
  ) -> Chunk {
    let session = crate::parser::Parser::parse_source_with_type(source, SourceType::mjs())
      .expect("synthetic prelinked source parses during test setup");
    let context = ModuleContext {
      module_index,
      imports: imports
        .iter()
        .map(|(local, source_index, exported)| {
          (
            (*local).to_string(),
            (*source_index, (*exported).to_string()),
          )
        })
        .collect(),
      exports: exports
        .iter()
        .map(|name| (*name).to_string())
        .collect(),
      namespace_exports: HashMap::new(),
    };
    let mut chunk =
      crate::compiler::compile_oxc_with_ctx(session.program(), Some(&context), session.source())
        .expect("synthetic prelinked source compiles during test setup");
    replace_debug_source(
      &mut chunk,
      &Arc::new(
        crate::js!(
          parse_error(module),
          "!!! invalid debug source; parser/compiler must not read this !!!"
        )
        .to_string(),
      ),
    );
    chunk
  }

  fn compile_prelinked_namespace_reexport_chunk(
    source: &str,
    module_index: usize,
    target_index: usize,
    exported: &str,
  ) -> Chunk {
    let session = crate::parser::Parser::parse_source_with_type(source, SourceType::mjs())
      .expect("synthetic namespace re-export source parses during test setup");
    let context = ModuleContext {
      module_index,
      imports: HashMap::new(),
      exports: HashSet::from([exported.to_string()]),
      namespace_exports: HashMap::from([(exported.to_string(), target_index)]),
    };
    let mut chunk =
      crate::compiler::compile_oxc_with_ctx(session.program(), Some(&context), session.source())
        .expect("synthetic namespace re-export source compiles during test setup");
    replace_debug_source(
      &mut chunk,
      &Arc::new(
        crate::js!(
          parse_error(module),
          "!!! invalid debug source; parser/compiler must not read this !!!"
        )
        .to_string(),
      ),
    );
    chunk
  }

  fn synthetic_graph(
    modules: Vec<PrelinkedModule>,
    binding_count: u32,
    public_roots: Vec<u32>,
  ) -> PrelinkedModuleGraph {
    PrelinkedModuleGraph {
      format_version: PRELINKED_MODULE_GRAPH_VERSION,
      engine_bytecode_version: crate::bytecode::BytecodeVersion::RiscV2,
      package_name: "synthetic".to_string(),
      package_version: "1.0.0".to_string(),
      source_manifest_hash: [0xA5; 32],
      modules,
      binding_count,
      public_roots,
    }
  }

  fn export(
    exported_name: &str,
    binding_id: u32,
  ) -> PrelinkedExport {
    PrelinkedExport {
      exported_name: exported_name.to_string(),
      binding_id,
    }
  }

  fn import(
    local_name: &str,
    binding_id: u32,
  ) -> PrelinkedImport {
    PrelinkedImport {
      local_name: local_name.to_string(),
      binding_id,
    }
  }

  #[test]
  fn virtual_registry_keeps_stable_payload_hashes_and_declarations() {
    let first = VirtualModule::javascript(
      "test:module",
      crate::js!(module, "export const value = 1;"),
      SourceType::mjs(),
      Some(crate::js!(typescript, "export declare const value: number;").to_string()),
    );
    let second = VirtualModule::javascript(
      "test:module",
      crate::js!(module, "export const value = 1;"),
      SourceType::mjs(),
      Some(crate::js!(typescript, "export declare const value: number;").to_string()),
    );
    let changed = VirtualModule::javascript(
      "test:module",
      crate::js!(module, "export const value = 2;"),
      SourceType::mjs(),
      None,
    );

    assert_eq!(first.content_hash, second.content_hash);
    assert_ne!(first.content_hash, changed.content_hash);
    assert_eq!(
      first.declaration_source.as_deref(),
      Some(crate::js!(
        typescript,
        "export declare const value: number;"
      ))
    );

    let mut registry = VirtualModuleRegistry::new();
    registry
      .register(first)
      .expect("first registration succeeds");
    assert!(registry.register(second).is_err());
    assert_eq!(registry.len(), 1);
  }

  #[test]
  fn virtual_modules_use_normal_graph_construction_and_evaluation() {
    let mut loader = ModuleLoader::new();
    loader
      .register_virtual_module(VirtualModule::javascript(
        "test:dependency",
        crate::js!(module, "export const answer = 42;"),
        SourceType::mjs(),
        None,
      ))
      .expect("dependency registration succeeds");
    loader
      .register_virtual_module(VirtualModule::javascript(
        "test:entry",
        crate::js!(
          module,
          "import { answer } from 'test:dependency'; globalThis.result = answer;"
        ),
        SourceType::mjs(),
        None,
      ))
      .expect("entry registration succeeds");

    let entry = loader
      .construct(Path::new("."), "test:entry")
      .expect("virtual entry constructs");
    assert_eq!(loader.map.len(), 2);
    loader
      .instantiate_all()
      .expect("virtual graph instantiates");

    let globals = Rc::new(RefCell::new(ObjectData::new()));
    let mut vm = Vm::new(globals);
    let entry_path = entry.borrow().path.clone();
    loader
      .evaluate_entry(&entry_path, &mut vm, false)
      .expect("virtual graph evaluates");
    assert_eq!(vm.get_global("result"), Some(Value::Number(42.0)));
  }

  #[test]
  fn typecheck_distinguishes_source_imports_from_virtual_declarations() {
    let globals = Rc::new(RefCell::new(ObjectData::new()));
    let mut vm = Vm::new(globals);
    vm.register_virtual_module(VirtualModule::javascript(
      "app/value.js",
      crate::js!(module, "export const value = 41;"),
      SourceType::mjs(),
      None,
    ))
    .expect("source dependency registers");
    vm.register_virtual_module(VirtualModule::javascript(
      "effect/api",
      crate::js!(module, "export const answer = 42;"),
      SourceType::mjs(),
      Some(crate::js!(typescript, "export declare const answer: number;").to_string()),
    ))
    .expect("declared virtual module registers");
    vm.register_virtual_module(VirtualModule::javascript(
      "app/entry.js",
      crate::js!(
        module,
        "import './value.js'; import { answer } from 'effect/api'; globalThis.result = answer;"
      ),
      SourceType::mjs(),
      None,
    ))
    .expect("source entry registers");

    let loader = vm.module_loader_handle();
    let entry = loader
      .borrow_mut()
      .construct(Path::new("."), "app/entry.js")
      .expect("multi-source graph constructs");
    loader
      .borrow_mut()
      .instantiate_all()
      .expect("multi-source graph instantiates");
    let entry_path = entry.borrow().path.clone();
    loader
      .borrow_mut()
      .evaluate_entry(&entry_path, &mut vm, true)
      .expect("source imports and virtual declarations typecheck distinctly");
    assert_eq!(vm.get_global("result"), Some(Value::Number(42.0)));
  }

  #[test]
  fn source_pipeline_snapshot_counts_source_work() {
    let globals = Rc::new(RefCell::new(ObjectData::new()));
    let mut vm = Vm::new(globals);
    vm.register_virtual_module(VirtualModule::javascript(
      "test:counters",
      crate::js!(module, "globalThis.result = 42;"),
      SourceType::mjs(),
      None,
    ))
    .expect("source module registers");
    let loader = vm.module_loader_handle();
    let entry = loader
      .borrow_mut()
      .construct(Path::new("."), "test:counters")
      .expect("source module constructs");
    loader
      .borrow_mut()
      .instantiate_all()
      .expect("source module instantiates");
    let entry_path = entry.borrow().path.clone();
    loader
      .borrow_mut()
      .evaluate_entry(&entry_path, &mut vm, true)
      .expect("source module evaluates");

    let expected = ModuleLoaderPipelineSnapshot {
      source_parses: 1,
      import_extraction_parses: 1,
      source_linking_passes: 2,
      source_typechecks: 1,
      source_compilations: 1,
    };
    assert_eq!(loader.borrow().pipeline_snapshot(), expected);
    assert_eq!(vm.module_loader_pipeline_snapshot(), expected);
    assert_eq!(vm.get_global("result"), Some(Value::Number(42.0)));
  }

  #[test]
  fn source_pipeline_snapshot_counts_parallel_compilation() {
    let globals = Rc::new(RefCell::new(ObjectData::new()));
    let mut vm = Vm::new(globals);
    vm.register_virtual_module(VirtualModule::javascript(
      "test:parallel-dependency",
      crate::js!(module, "globalThis.dependencyLoaded = true;"),
      SourceType::mjs(),
      None,
    ))
    .expect("dependency module registers");
    vm.register_virtual_module(VirtualModule::javascript(
      "test:parallel-entry",
      crate::js!(
        module,
        "import 'test:parallel-dependency'; globalThis.parallelResult = 42;"
      ),
      SourceType::mjs(),
      None,
    ))
    .expect("entry module registers");
    let loader = vm.module_loader_handle();
    let entry = loader
      .borrow_mut()
      .construct(Path::new("."), "test:parallel-entry")
      .expect("source graph constructs");
    loader
      .borrow_mut()
      .instantiate_all()
      .expect("source graph instantiates");
    let entry_path = entry.borrow().path.clone();
    evaluate_entry_shared(&loader, &entry_path, &mut vm, true).expect("source graph evaluates");

    assert_eq!(
      loader.borrow().pipeline_snapshot(),
      ModuleLoaderPipelineSnapshot {
        source_parses: 4,
        import_extraction_parses: 2,
        source_linking_passes: 4,
        source_typechecks: 2,
        source_compilations: 2,
      }
    );
    assert_eq!(vm.get_global("parallelResult"), Some(Value::Number(42.0)));
  }

  #[test]
  fn relative_virtual_imports_use_importer_namespace() {
    let mut loader = ModuleLoader::new();
    loader
      .register_virtual_module(VirtualModule::javascript(
        "app/shared/value.js",
        crate::js!(module, "export const value = 9;"),
        SourceType::mjs(),
        None,
      ))
      .expect("dependency registration succeeds");
    loader
      .register_virtual_module(VirtualModule::javascript(
        "app/entry.js",
        crate::js!(
          module,
          "import { value } from './shared/value.js'; globalThis.relativeResult = value;"
        ),
        SourceType::mjs(),
        None,
      ))
      .expect("entry registration succeeds");

    let entry = loader
      .construct(Path::new("."), "app/entry.js")
      .expect("relative virtual import constructs");
    assert!(
      loader
        .map
        .contains_key(&virtual_module_path("app/shared/value.js"))
    );
    loader
      .instantiate_all()
      .expect("relative graph instantiates");
    let globals = Rc::new(RefCell::new(ObjectData::new()));
    let mut vm = Vm::new(globals);
    let entry_path = entry.borrow().path.clone();
    loader
      .evaluate_entry(&entry_path, &mut vm, false)
      .expect("relative graph evaluates");
    assert_eq!(vm.get_global("relativeResult"), Some(Value::Number(9.0)));
  }

  #[test]
  fn virtual_reexports_and_aliases_share_canonical_identity() {
    let mut loader = ModuleLoader::new();
    loader
      .register_virtual_module(VirtualModule::javascript(
        "pkg/value.js",
        crate::js!(module, "export const value = 11;"),
        SourceType::mjs(),
        Some(crate::js!(typescript, "export declare const value: number;").to_string()),
      ))
      .expect("value registration succeeds");
    loader
      .register_virtual_module(VirtualModule::javascript(
        "pkg/reexport.js",
        crate::js!(module, "export { value } from './value.js';"),
        SourceType::mjs(),
        None,
      ))
      .expect("reexport registration succeeds");
    loader
      .register_virtual_module(VirtualModule::javascript(
        "pkg/entry.js",
        crate::js!(
          module,
          "import { value } from 'pkg/alias.js'; globalThis.aliasResult = value;"
        ),
        SourceType::mjs(),
        None,
      ))
      .expect("entry registration succeeds");
    loader
      .register_virtual_alias("pkg/alias.js", "pkg/reexport.js")
      .expect("alias registration succeeds");

    let alias = loader
      .construct(Path::new("."), "pkg/alias.js")
      .expect("alias constructs");
    let canonical = loader
      .construct(Path::new("."), "pkg/reexport.js")
      .expect("canonical constructs");
    assert!(Rc::ptr_eq(&alias, &canonical));
    assert_eq!(
      loader.declaration_source("pkg/value.js"),
      Some(crate::js!(
        typescript,
        "export declare const value: number;"
      ))
    );
    let entry = loader
      .construct(Path::new("."), "pkg/entry.js")
      .expect("entry constructs");
    loader
      .instantiate_all()
      .expect("reexport graph instantiates");
    let globals = Rc::new(RefCell::new(ObjectData::new()));
    let mut vm = Vm::new(globals);
    let entry_path = entry.borrow().path.clone();
    loader
      .evaluate_entry(&entry_path, &mut vm, false)
      .expect("reexport graph evaluates");
    assert_eq!(vm.get_global("aliasResult"), Some(Value::Number(11.0)));
  }

  #[test]
  fn source_namespace_reexports_preserve_namespace_identity_and_liveness() {
    let mut loader = ModuleLoader::new();
    loader
      .register_virtual_module(VirtualModule::javascript(
        "namespace-source/base.js",
        crate::js!(module, "export let value = 2;"),
        SourceType::mjs(),
        None,
      ))
      .expect("base registration succeeds");
    loader
      .register_virtual_module(VirtualModule::javascript(
        "namespace-source/reexport.js",
        crate::js!(module, "export * as ns from './base.js';"),
        SourceType::mjs(),
        None,
      ))
      .expect("namespace re-export registration succeeds");
    loader
      .register_virtual_module(VirtualModule::javascript(
        "namespace-source/entry.js",
        crate::js!(module, "import { ns } from './reexport.js'; import * as direct from './base.js'; globalThis.namespaceSame = ns === direct; globalThis.namespaceValue = ns.value; globalThis.namespaceType = typeof direct;"),
        SourceType::mjs(),
        None,
      ))
      .expect("entry registration succeeds");

    let entry = loader
      .construct(Path::new("."), "namespace-source/entry.js")
      .expect("namespace source graph constructs");
    loader
      .instantiate_all()
      .expect("namespace source graph instantiates");
    let globals = Rc::new(RefCell::new(ObjectData::new()));
    let mut vm = Vm::new(globals);
    let entry_path = entry.borrow().path.clone();
    loader
      .evaluate_entry(&entry_path, &mut vm, false)
      .expect("namespace source graph evaluates");
    assert_eq!(vm.get_global("namespaceSame"), Some(Value::Bool(true)));
    assert_eq!(vm.get_global("namespaceValue"), Some(Value::Number(2.0)));
    assert_eq!(
      vm.get_global("namespaceType"),
      Some(Value::String("object".to_string()))
    );
  }

  #[test]
  fn virtual_dependency_errors_are_not_dropped() {
    let mut loader = ModuleLoader::new();
    loader
      .register_virtual_module(VirtualModule::javascript(
        "broken/entry.js",
        crate::js!(module, "import './missing.js'; export const value = 1;"),
        SourceType::mjs(),
        None,
      ))
      .expect("entry registration succeeds");

    let result = loader.construct(Path::new("."), "broken/entry.js");
    assert!(
      result.is_err(),
      "missing virtual dependency must fail construction"
    );
    let error = match result {
      Ok(_) => BundlerError::from("missing virtual dependency was accepted"),
      Err(error) => error,
    };
    assert!(error.to_string().contains("broken/entry.js"));
    assert!(error.to_string().contains("missing.js"));
  }

  #[test]
  fn virtual_cycles_keep_shared_import_export_slots() {
    let mut loader = ModuleLoader::new();
    loader
      .register_virtual_module(VirtualModule::javascript(
        "cycle/a.js",
        crate::js!(
          module,
          "import { value as bValue } from './b.js'; export const value = bValue;"
        ),
        SourceType::mjs(),
        None,
      ))
      .expect("a registration succeeds");
    loader
      .register_virtual_module(VirtualModule::javascript(
        "cycle/b.js",
        crate::js!(
          module,
          "import { value as aValue } from './a.js'; export const value = aValue;"
        ),
        SourceType::mjs(),
        None,
      ))
      .expect("b registration succeeds");
    loader
      .construct(Path::new("."), "cycle/a.js")
      .expect("cycle constructs");
    loader
      .instantiate_all()
      .expect("cycle instantiates");
    let a = loader
      .records_by_specifier
      .get("cycle/a.js")
      .expect("a record")
      .borrow();
    let b = loader
      .records_by_specifier
      .get("cycle/b.js")
      .expect("b record")
      .borrow();
    assert!(Rc::ptr_eq(
      a.imports.get("bValue").expect("a import"),
      b.exports.get("value").expect("b export")
    ));
    assert!(Rc::ptr_eq(
      b.imports.get("aValue").expect("b import"),
      a.exports.get("value").expect("a export")
    ));
  }

  #[test]
  fn prelinked_direct_import_is_parser_free_and_uses_canonical_slots() {
    let value_chunk = compile_prelinked_chunk(
      crate::js!(module, "export const answer = 42;"),
      0,
      &[],
      &["answer"],
    );
    let entry_chunk = compile_prelinked_chunk(
      crate::js!(
        module,
        "import { answer } from 'graph:value'; globalThis.prelinkedResult = answer;"
      ),
      1,
      &[("answer", 0, "answer")],
      &[],
    );
    let graph = synthetic_graph(
      vec![
        PrelinkedModule {
          specifier: "graph:value".to_string(),
          module_index: 0,
          source_sha256: [1; 32],
          dependencies: vec![],
          exports: vec![export("answer", 0)],
          imports: vec![],
          chunk: value_chunk,
        },
        PrelinkedModule {
          specifier: "graph:entry".to_string(),
          module_index: 1,
          source_sha256: [2; 32],
          dependencies: vec![0],
          exports: vec![],
          imports: vec![import("answer", 0)],
          chunk: entry_chunk,
        },
      ],
      1,
      vec![1],
    );

    let mut loader = ModuleLoader::new();
    loader
      .register_prelinked_graph(graph)
      .expect("prelinked graph registers");
    assert!(loader.prelinked_graphs[0].binding_slots.is_none());
    let entry = loader
      .construct(Path::new("."), "graph:entry")
      .expect("prelinked graph constructs");
    assert!(entry.borrow().is_prelinked());
    assert!(entry.borrow().parser_session().is_none());
    assert!(
      entry
        .borrow()
        .bytecode
        .as_ref()
        .expect("prelinked bytecode")
        .source_text
        .starts_with("!!! invalid debug source")
    );

    loader
      .instantiate_all()
      .expect("prelinked graph instantiates");
    assert!(loader.prelinked_graphs[0].binding_slots.is_some());
    let value_record = loader
      .records_by_specifier
      .get("graph:value")
      .expect("value record")
      .borrow();
    assert!(Rc::ptr_eq(
      value_record
        .exports
        .get("answer")
        .expect("answer export"),
      entry
        .borrow()
        .imports
        .get("answer")
        .expect("answer import"),
    ));
    drop(value_record);

    let globals = Rc::new(RefCell::new(ObjectData::new()));
    let mut vm = Vm::new(globals);
    let entry_path = entry.borrow().path.clone();
    let loader = Rc::new(RefCell::new(loader));
    // Typechecking is deliberately enabled: the shared parallel-compilation
    // path must omit prelinked records and bypass parser-backed typechecking.
    evaluate_entry_shared(&loader, &entry_path, &mut vm, true)
      .expect("prelinked graph evaluates from bytecode only");
    assert_eq!(vm.get_global("prelinkedResult"), Some(Value::Number(42.0)));
    assert_eq!(
      loader.borrow().pipeline_snapshot(),
      ModuleLoaderPipelineSnapshot::default()
    );
  }

  #[test]
  fn source_import_can_reach_a_public_prelinked_root() {
    let graph = synthetic_graph(
      vec![PrelinkedModule {
        specifier: "visibility:public".to_string(),
        module_index: 0,
        source_sha256: [1; 32],
        dependencies: vec![],
        exports: vec![export("answer", 0)],
        imports: vec![],
        chunk: compile_prelinked_chunk(
          crate::js!(module, "export const answer = 42;"),
          0,
          &[],
          &["answer"],
        ),
      }],
      1,
      vec![0],
    );
    let mut loader = ModuleLoader::new();
    loader
      .register_prelinked_graph(graph)
      .expect("visibility graph registers");
    loader
      .register_virtual_module(VirtualModule::javascript(
        "visibility:source-entry",
        crate::js!(
          module,
          "import { answer } from 'visibility:public'; globalThis.visibilityResult = answer;"
        ),
        SourceType::mjs(),
        None,
      ))
      .expect("source entry registers");

    let entry = loader
      .construct(Path::new("."), "visibility:source-entry")
      .expect("source import of public root constructs");
    loader
      .instantiate_all()
      .expect("source import graph instantiates");
    let globals = Rc::new(RefCell::new(ObjectData::new()));
    let mut vm = Vm::new(globals);
    let entry_path = entry.borrow().path.clone();
    loader
      .evaluate_entry(&entry_path, &mut vm, false)
      .expect("source import graph evaluates");
    assert_eq!(vm.get_global("visibilityResult"), Some(Value::Number(42.0)));
  }

  #[test]
  fn prelinked_public_root_attaches_declaration_source() {
    let graph = synthetic_graph(
      vec![PrelinkedModule {
        specifier: "declarations:public".to_string(),
        module_index: 0,
        source_sha256: [1; 32],
        dependencies: vec![],
        exports: vec![],
        imports: vec![],
        chunk: compile_prelinked_chunk("", 0, &[], &[]),
      }],
      0,
      vec![0],
    );
    let mut loader = ModuleLoader::new();
    loader
      .register_prelinked_graph_with_declarations(
        graph,
        vec![(
          "declarations:public".to_string(),
          crate::js!(typescript, "export declare const answer: number;").to_string(),
        )],
      )
      .expect("graph and declaration register atomically");

    let record = loader
      .construct(Path::new("."), "declarations:public")
      .expect("public root constructs");
    assert_eq!(
      record.borrow().declaration_source.as_deref(),
      Some(crate::js!(
        typescript,
        "export declare const answer: number;"
      ))
    );
    assert_eq!(
      loader.declaration_source("declarations:public"),
      Some(crate::js!(
        typescript,
        "export declare const answer: number;"
      ))
    );
  }

  #[test]
  fn prelinked_declarations_reject_private_and_unknown_targets_without_installing_graph() {
    let graph = synthetic_graph(
      vec![
        PrelinkedModule {
          specifier: "declarations:private".to_string(),
          module_index: 0,
          source_sha256: [1; 32],
          dependencies: vec![],
          exports: vec![],
          imports: vec![],
          chunk: compile_prelinked_chunk("", 0, &[], &[]),
        },
        PrelinkedModule {
          specifier: "declarations:public".to_string(),
          module_index: 1,
          source_sha256: [2; 32],
          dependencies: vec![],
          exports: vec![],
          imports: vec![],
          chunk: compile_prelinked_chunk("", 1, &[], &[]),
        },
      ],
      0,
      vec![1],
    );

    for target in ["declarations:private", "declarations:unknown"] {
      let mut loader = ModuleLoader::new();
      let error = loader
        .register_prelinked_graph_with_declarations(
          graph.clone(),
          vec![(
            target.to_string(),
            crate::js!(typescript, "declare const value: number;").to_string(),
          )],
        )
        .expect_err("invalid declaration target is rejected");
      assert!(error.to_string().contains("not a public root"));
      assert!(loader.prelinked_graphs.is_empty());
      assert!(loader.prelinked_by_specifier.is_empty());
    }
  }

  #[test]
  fn prelinked_declarations_reject_duplicate_targets_without_installing_graph() {
    let graph = synthetic_graph(
      vec![PrelinkedModule {
        specifier: "declarations:public".to_string(),
        module_index: 0,
        source_sha256: [1; 32],
        dependencies: vec![],
        exports: vec![],
        imports: vec![],
        chunk: compile_prelinked_chunk("", 0, &[], &[]),
      }],
      0,
      vec![0],
    );
    let mut loader = ModuleLoader::new();
    let error = loader
      .register_prelinked_graph_with_declarations(
        graph,
        vec![
          (
            "declarations:public".to_string(),
            crate::js!(typescript, "declare const a: 1;").to_string(),
          ),
          (
            "declarations:public".to_string(),
            crate::js!(typescript, "declare const b: 2;").to_string(),
          ),
        ],
      )
      .expect_err("duplicate declaration target is rejected");
    assert!(
      error
        .to_string()
        .contains("Duplicate prelinked declaration target")
    );
    assert!(loader.prelinked_graphs.is_empty());
    assert!(loader.prelinked_by_specifier.is_empty());
  }

  #[test]
  fn source_import_cannot_reach_a_private_prelinked_module() {
    let graph = synthetic_graph(
      vec![
        PrelinkedModule {
          specifier: "visibility:private".to_string(),
          module_index: 0,
          source_sha256: [1; 32],
          dependencies: vec![],
          exports: vec![],
          imports: vec![],
          chunk: compile_prelinked_chunk("", 0, &[], &[]),
        },
        PrelinkedModule {
          specifier: "visibility:public".to_string(),
          module_index: 1,
          source_sha256: [2; 32],
          dependencies: vec![],
          exports: vec![],
          imports: vec![],
          chunk: compile_prelinked_chunk("", 1, &[], &[]),
        },
      ],
      0,
      vec![1],
    );
    let mut loader = ModuleLoader::new();
    loader
      .register_prelinked_graph(graph)
      .expect("visibility graph registers");
    loader
      .register_virtual_module(VirtualModule::javascript(
        "visibility:source-private-entry",
        crate::js!(module, "import 'visibility:private';"),
        SourceType::mjs(),
        None,
      ))
      .expect("source entry registers");

    let error = match loader.construct(Path::new("."), "visibility:source-private-entry") {
      Ok(_) => panic!("source import of private module must fail"),
      Err(error) => error,
    };
    assert!(error.to_string().contains("visibility:private"));
    assert!(error.to_string().contains("private"));
  }

  #[test]
  fn public_prelinked_entry_evaluates_its_private_dependency() {
    let graph = synthetic_graph(
      vec![
        PrelinkedModule {
          specifier: "visibility:private".to_string(),
          module_index: 0,
          source_sha256: [1; 32],
          dependencies: vec![],
          exports: vec![export("answer", 0)],
          imports: vec![],
          chunk: compile_prelinked_chunk(
            crate::js!(module, "export const answer = 42;"),
            0,
            &[],
            &["answer"],
          ),
        },
        PrelinkedModule {
          specifier: "visibility:entry".to_string(),
          module_index: 1,
          source_sha256: [2; 32],
          dependencies: vec![0],
          exports: vec![],
          imports: vec![import("answer", 0)],
          chunk: compile_prelinked_chunk(
            crate::js!(
              module,
              "import { answer } from 'visibility:private'; globalThis.visibilityResult = answer;"
            ),
            1,
            &[("answer", 0, "answer")],
            &[],
          ),
        },
      ],
      1,
      vec![1],
    );
    let mut loader = ModuleLoader::new();
    loader
      .register_prelinked_graph(graph)
      .expect("visibility graph registers");
    let entry = loader
      .construct(Path::new("."), "visibility:entry")
      .expect("public prelinked entry constructs");
    loader
      .instantiate_all()
      .expect("public prelinked entry instantiates");

    let globals = Rc::new(RefCell::new(ObjectData::new()));
    let mut vm = Vm::new(globals);
    let entry_path = entry.borrow().path.clone();
    loader
      .evaluate_entry(Path::new("visibility:entry"), &mut vm, false)
      .expect("public prelinked entry resolves and evaluates");
    assert_eq!(vm.get_global("visibilityResult"), Some(Value::Number(42.0)));
    assert!(loader.map.contains_key(&entry_path));
  }

  #[test]
  fn private_prelinked_specifiers_still_collide() {
    let graph = synthetic_graph(
      vec![
        PrelinkedModule {
          specifier: "visibility:private".to_string(),
          module_index: 0,
          source_sha256: [1; 32],
          dependencies: vec![],
          exports: vec![],
          imports: vec![],
          chunk: compile_prelinked_chunk("", 0, &[], &[]),
        },
        PrelinkedModule {
          specifier: "visibility:public".to_string(),
          module_index: 1,
          source_sha256: [2; 32],
          dependencies: vec![],
          exports: vec![],
          imports: vec![],
          chunk: compile_prelinked_chunk("", 1, &[], &[]),
        },
      ],
      0,
      vec![1],
    );
    let mut loader = ModuleLoader::new();
    loader
      .register_prelinked_graph(graph)
      .expect("first visibility graph registers");
    assert!(
      loader
        .register_virtual_module(VirtualModule::javascript(
          "visibility:private",
          "",
          SourceType::mjs(),
          None,
        ))
        .is_err(),
      "private prelinked specifier must collide with a virtual module"
    );

    let duplicate_graph = synthetic_graph(
      vec![
        PrelinkedModule {
          specifier: "visibility:private".to_string(),
          module_index: 0,
          source_sha256: [3; 32],
          dependencies: vec![],
          exports: vec![],
          imports: vec![],
          chunk: compile_prelinked_chunk("", 0, &[], &[]),
        },
        PrelinkedModule {
          specifier: "visibility:other-public".to_string(),
          module_index: 1,
          source_sha256: [4; 32],
          dependencies: vec![],
          exports: vec![],
          imports: vec![],
          chunk: compile_prelinked_chunk("", 1, &[], &[]),
        },
      ],
      0,
      vec![1],
    );
    assert!(
      loader
        .register_prelinked_graph(duplicate_graph)
        .is_err(),
      "private prelinked specifier must collide with another graph"
    );
  }

  #[test]
  fn prelinked_cycles_read_preallocated_live_bindings() {
    let a_chunk = compile_prelinked_chunk(
      crate::js!(
        module,
        "import { getB } from 'cycle:b'; export const a = 1; export function readB() { return getB(); }"
      ),
      0,
      &[("getB", 1, "getB")],
      &["a", "readB"],
    );
    let b_chunk = compile_prelinked_chunk(
      crate::js!(
        module,
        "import { a } from 'cycle:a'; export function getB() { return a + 1; }"
      ),
      1,
      &[("a", 0, "a")],
      &["getB"],
    );
    let entry_chunk = compile_prelinked_chunk(
      crate::js!(
        module,
        "import { readB } from 'cycle:a'; globalThis.cycleResult = readB();"
      ),
      2,
      &[("readB", 0, "readB")],
      &[],
    );
    let graph = synthetic_graph(
      vec![
        PrelinkedModule {
          specifier: "cycle:a".to_string(),
          module_index: 0,
          source_sha256: [1; 32],
          dependencies: vec![1],
          exports: vec![export("a", 0), export("readB", 1)],
          imports: vec![import("getB", 2)],
          chunk: a_chunk,
        },
        PrelinkedModule {
          specifier: "cycle:b".to_string(),
          module_index: 1,
          source_sha256: [2; 32],
          dependencies: vec![0],
          exports: vec![export("getB", 2)],
          imports: vec![import("a", 0)],
          chunk: b_chunk,
        },
        PrelinkedModule {
          specifier: "cycle:entry".to_string(),
          module_index: 2,
          source_sha256: [3; 32],
          dependencies: vec![0],
          exports: vec![],
          imports: vec![import("readB", 1)],
          chunk: entry_chunk,
        },
      ],
      3,
      vec![2],
    );

    let mut loader = ModuleLoader::new();
    loader
      .register_prelinked_graph(graph)
      .expect("cycle graph registers");
    let entry = loader
      .construct(Path::new("."), "cycle:entry")
      .expect("cycle graph constructs");
    assert_eq!(loader.map.len(), 3, "cycle records are constructed once");
    loader
      .instantiate_all()
      .expect("cycle graph instantiates");
    let a = loader
      .records_by_specifier
      .get("cycle:a")
      .expect("a record")
      .borrow();
    let b = loader
      .records_by_specifier
      .get("cycle:b")
      .expect("b record")
      .borrow();
    assert!(Rc::ptr_eq(
      a.exports.get("a").expect("a export"),
      b.imports.get("a").expect("a import"),
    ));
    assert!(Rc::ptr_eq(
      b.exports.get("getB").expect("getB export"),
      a.imports.get("getB").expect("getB import"),
    ));
    drop(a);
    drop(b);

    let globals = Rc::new(RefCell::new(ObjectData::new()));
    let mut vm = Vm::new(globals);
    let entry_path = entry.borrow().path.clone();
    loader
      .evaluate_entry(&entry_path, &mut vm, false)
      .expect("cycle evaluates");
    assert_eq!(vm.get_global("cycleResult"), Some(Value::Number(2.0)));
  }

  #[test]
  fn prelinked_reexports_and_export_all_share_the_origin_slot() {
    let base_chunk = compile_prelinked_chunk(
      crate::js!(module, "export const value = 5;"),
      0,
      &[],
      &["value"],
    );
    let alias_chunk = compile_prelinked_chunk(
      crate::js!(module, "export { value as renamed } from 'reexport:base';"),
      1,
      &[],
      &["renamed"],
    );
    let star_chunk = compile_prelinked_chunk(
      crate::js!(module, "export * from 'reexport:alias';"),
      2,
      &[],
      &["renamed"],
    );
    let entry_chunk = compile_prelinked_chunk(
      crate::js!(
        module,
        "import { renamed } from 'reexport:star'; globalThis.reexportResult = renamed;"
      ),
      3,
      &[("renamed", 2, "renamed")],
      &[],
    );
    let graph = synthetic_graph(
      vec![
        PrelinkedModule {
          specifier: "reexport:base".to_string(),
          module_index: 0,
          source_sha256: [1; 32],
          dependencies: vec![],
          exports: vec![export("value", 0)],
          imports: vec![],
          chunk: base_chunk,
        },
        PrelinkedModule {
          specifier: "reexport:alias".to_string(),
          module_index: 1,
          source_sha256: [2; 32],
          dependencies: vec![0],
          exports: vec![export("renamed", 0)],
          imports: vec![],
          chunk: alias_chunk,
        },
        PrelinkedModule {
          specifier: "reexport:star".to_string(),
          module_index: 2,
          source_sha256: [3; 32],
          dependencies: vec![1],
          exports: vec![export("renamed", 0)],
          imports: vec![],
          chunk: star_chunk,
        },
        PrelinkedModule {
          specifier: "reexport:entry".to_string(),
          module_index: 3,
          source_sha256: [4; 32],
          dependencies: vec![2],
          exports: vec![],
          imports: vec![import("renamed", 0)],
          chunk: entry_chunk,
        },
      ],
      1,
      vec![3],
    );

    let mut loader = ModuleLoader::new();
    loader
      .register_prelinked_graph(graph)
      .expect("reexport graph registers");
    let entry = loader
      .construct(Path::new("."), "reexport:entry")
      .expect("reexport graph constructs");
    loader
      .instantiate_all()
      .expect("reexport graph instantiates");
    let base_slot = Rc::clone(
      &loader.records_by_specifier["reexport:base"]
        .borrow()
        .exports["value"],
    );
    let alias_slot = Rc::clone(
      &loader.records_by_specifier["reexport:alias"]
        .borrow()
        .exports["renamed"],
    );
    let star_slot = Rc::clone(
      &loader.records_by_specifier["reexport:star"]
        .borrow()
        .exports["renamed"],
    );
    assert!(Rc::ptr_eq(&base_slot, &alias_slot));
    assert!(Rc::ptr_eq(&base_slot, &star_slot));

    let globals = Rc::new(RefCell::new(ObjectData::new()));
    let mut vm = Vm::new(globals);
    let entry_path = entry.borrow().path.clone();
    loader
      .evaluate_entry(&entry_path, &mut vm, false)
      .expect("reexport graph evaluates");
    assert_eq!(vm.get_global("reexportResult"), Some(Value::Number(5.0)));
  }

  #[test]
  fn prelinked_namespace_imports_share_one_live_namespace() {
    let base_chunk = compile_prelinked_chunk(
      crate::js!(module, "export let value = 2;"),
      0,
      &[],
      &["value"],
    );
    let entry_chunk = compile_prelinked_chunk(
      crate::js!(
        module,
        "import * as first from 'namespace:base'; import * as second from 'namespace:base'; globalThis.namespaceSame = first === second; globalThis.namespaceValue = second.value;"
      ),
      1,
      &[("first", 0, "*"), ("second", 0, "*")],
      &[],
    );
    let graph = synthetic_graph(
      vec![
        PrelinkedModule {
          specifier: "namespace:base".to_string(),
          module_index: 0,
          source_sha256: [1; 32],
          dependencies: vec![],
          exports: vec![export("value", 0)],
          imports: vec![],
          chunk: base_chunk,
        },
        PrelinkedModule {
          specifier: "namespace:entry".to_string(),
          module_index: 1,
          source_sha256: [2; 32],
          dependencies: vec![0, 0],
          exports: vec![],
          imports: vec![],
          chunk: entry_chunk,
        },
      ],
      1,
      vec![1],
    );

    let mut loader = ModuleLoader::new();
    loader
      .register_prelinked_graph(graph)
      .expect("namespace graph registers");
    let entry = loader
      .construct(Path::new("."), "namespace:entry")
      .expect("namespace graph constructs");
    loader
      .instantiate_all()
      .expect("namespace graph instantiates");
    let globals = Rc::new(RefCell::new(ObjectData::new()));
    let mut vm = Vm::new(globals);
    let entry_path = entry.borrow().path.clone();
    loader
      .evaluate_entry(&entry_path, &mut vm, false)
      .expect("namespace graph evaluates");
    assert_eq!(vm.get_global("namespaceSame"), Some(Value::Bool(true)));
    assert_eq!(vm.get_global("namespaceValue"), Some(Value::Number(2.0)));
  }

  #[test]
  fn prelinked_namespace_reexports_use_the_target_namespace_without_parsing() {
    let base_chunk = compile_prelinked_chunk(
      crate::js!(module, "export let value = 2;"),
      0,
      &[],
      &["value"],
    );
    let reexport_chunk = compile_prelinked_namespace_reexport_chunk(
      crate::js!(module, "export * as ns from 'namespace-prelinked:base';"),
      1,
      0,
      "ns",
    );
    let entry_chunk = compile_prelinked_chunk(
      crate::js!(
        module,
        "import { ns } from 'namespace-prelinked:reexport'; import * as direct from 'namespace-prelinked:base'; globalThis.prelinkedNamespaceSame = ns === direct; globalThis.prelinkedNamespaceValue = ns.value;"
      ),
      2,
      &[("ns", 1, "ns"), ("direct", 0, "*")],
      &[],
    );
    let graph = synthetic_graph(
      vec![
        PrelinkedModule {
          specifier: "namespace-prelinked:base".to_string(),
          module_index: 0,
          source_sha256: [1; 32],
          dependencies: vec![],
          exports: vec![export("value", 0)],
          imports: vec![],
          chunk: base_chunk,
        },
        PrelinkedModule {
          specifier: "namespace-prelinked:reexport".to_string(),
          module_index: 1,
          source_sha256: [2; 32],
          dependencies: vec![0],
          exports: vec![export("ns", 1)],
          imports: vec![],
          chunk: reexport_chunk,
        },
        PrelinkedModule {
          specifier: "namespace-prelinked:entry".to_string(),
          module_index: 2,
          source_sha256: [3; 32],
          dependencies: vec![1, 0],
          exports: vec![],
          imports: vec![import("ns", 1)],
          chunk: entry_chunk,
        },
      ],
      2,
      vec![2],
    );

    let graph = crate::prelinked_bundle::decode(
      &crate::prelinked_bundle::encode(&graph).expect("namespace graph encodes"),
    )
    .expect("namespace graph decodes");
    let mut loader = ModuleLoader::new();
    loader
      .register_prelinked_graph(graph)
      .expect("prelinked namespace graph registers");
    let entry = loader
      .construct(Path::new("."), "namespace-prelinked:entry")
      .expect("prelinked namespace graph constructs");
    loader
      .instantiate_all()
      .expect("prelinked namespace graph instantiates");
    assert!(
      loader
        .records_by_specifier
        .get("namespace-prelinked:base")
        .expect("base record")
        .borrow()
        .parser_session()
        .is_none()
    );

    let globals = Rc::new(RefCell::new(ObjectData::new()));
    let mut vm = Vm::new(globals);
    let entry_path = entry.borrow().path.clone();
    loader
      .evaluate_entry(&entry_path, &mut vm, false)
      .expect("prelinked namespace graph evaluates");
    assert_eq!(
      vm.get_global("prelinkedNamespaceSame"),
      Some(Value::Bool(true))
    );
    assert_eq!(
      vm.get_global("prelinkedNamespaceValue"),
      Some(Value::Number(2.0))
    );
  }

  #[test]
  fn prelinked_dependency_edges_control_side_effect_order() {
    let modules = vec![
      PrelinkedModule {
        specifier: "order:a".to_string(),
        module_index: 0,
        source_sha256: [1; 32],
        dependencies: vec![],
        exports: vec![],
        imports: vec![],
        chunk: compile_prelinked_chunk(
          crate::js!(module, "globalThis.order = (globalThis.order || '') + 'A';"),
          0,
          &[],
          &[],
        ),
      },
      PrelinkedModule {
        specifier: "order:b".to_string(),
        module_index: 1,
        source_sha256: [2; 32],
        dependencies: vec![],
        exports: vec![],
        imports: vec![],
        chunk: compile_prelinked_chunk(
          crate::js!(module, "globalThis.order = (globalThis.order || '') + 'B';"),
          1,
          &[],
          &[],
        ),
      },
      PrelinkedModule {
        specifier: "order:entry".to_string(),
        module_index: 2,
        source_sha256: [3; 32],
        dependencies: vec![1, 0],
        exports: vec![],
        imports: vec![],
        chunk: compile_prelinked_chunk(
          crate::js!(module, "globalThis.order = (globalThis.order || '') + 'E';"),
          2,
          &[],
          &[],
        ),
      },
    ];
    let mut loader = ModuleLoader::new();
    loader
      .register_prelinked_graph(synthetic_graph(modules, 0, vec![2]))
      .expect("ordered graph registers");
    let entry = loader
      .construct(Path::new("."), "order:entry")
      .expect("ordered graph constructs");
    loader
      .instantiate_all()
      .expect("ordered graph instantiates");
    let entry_path = entry.borrow().path.clone();
    assert_eq!(
      loader
        .evaluation_order_from(&entry_path)
        .expect("evaluation order"),
      vec![
        virtual_module_path("order:b"),
        virtual_module_path("order:a"),
        virtual_module_path("order:entry"),
      ]
    );

    let globals = Rc::new(RefCell::new(ObjectData::new()));
    let mut vm = Vm::new(globals);
    loader
      .evaluate_entry(&entry_path, &mut vm, false)
      .expect("ordered graph evaluates");
    assert_eq!(
      vm.get_global("order"),
      Some(Value::String("BAE".to_string()))
    );
  }

  #[test]
  fn source_modules_mix_with_prelinked_graphs_and_keep_reserved_indices() {
    let graph = synthetic_graph(
      vec![PrelinkedModule {
        specifier: "mixed:value".to_string(),
        module_index: 0,
        source_sha256: [1; 32],
        dependencies: vec![],
        exports: vec![export("answer", 0)],
        imports: vec![],
        chunk: compile_prelinked_chunk(
          crate::js!(module, "export const answer = 42;"),
          0,
          &[],
          &["answer"],
        ),
      }],
      1,
      vec![0],
    );
    let mut loader = ModuleLoader::new();
    loader
      .register_virtual_module(VirtualModule::javascript(
        "mixed:early",
        crate::js!(module, "export const early = 1;"),
        SourceType::mjs(),
        None,
      ))
      .expect("early source registers");
    let early = loader
      .construct(Path::new("."), "mixed:early")
      .expect("source can be constructed before graph registration");
    assert_eq!(early.borrow().module_index, None);

    loader
      .register_prelinked_graph(graph.clone())
      .expect("graph reserves index zero");
    loader
      .register_virtual_module(VirtualModule::javascript(
        "mixed:entry",
        crate::js!(
          module,
          "import { answer } from 'mixed:value'; globalThis.mixedResult = answer;"
        ),
        SourceType::mjs(),
        None,
      ))
      .expect("mixed source registers");
    let entry = loader
      .construct(Path::new("."), "mixed:entry")
      .expect("source imports prelinked module");
    loader
      .instantiate_all()
      .expect("mixed graph instantiates");
    assert!(
      early
        .borrow()
        .module_index
        .is_some_and(|index| index > 0)
    );
    assert!(
      entry
        .borrow()
        .module_index
        .is_some_and(|index| index > 0)
    );
    assert_ne!(early.borrow().module_index, entry.borrow().module_index);

    let globals = Rc::new(RefCell::new(ObjectData::new()));
    let mut vm = Vm::new(globals);
    let entry_path = entry.borrow().path.clone();
    loader
      .evaluate_entry(&entry_path, &mut vm, false)
      .expect("mixed graph evaluates");
    assert_eq!(vm.get_global("mixedResult"), Some(Value::Number(42.0)));

    loader
      .register_virtual_module(VirtualModule::javascript(
        "mixed:late",
        crate::js!(module, "export const late = 2;"),
        SourceType::mjs(),
        None,
      ))
      .expect("late source registers");
    let late = loader
      .construct(Path::new("."), "mixed:late")
      .expect("late source constructs");
    loader
      .instantiate_all()
      .expect("late source instantiates without collision");
    let late_index = late
      .borrow()
      .module_index
      .expect("late source index");
    assert!(late_index > 0);
    assert_ne!(late.borrow().module_index, early.borrow().module_index);
    assert_ne!(late.borrow().module_index, entry.borrow().module_index);

    let mut collision_loader = ModuleLoader::new();
    collision_loader
      .register_virtual_module(VirtualModule::javascript(
        "mixed:indexed-first",
        crate::js!(module, "export const value = 1;"),
        SourceType::mjs(),
        None,
      ))
      .expect("source registers");
    collision_loader
      .construct(Path::new("."), "mixed:indexed-first")
      .expect("source constructs");
    collision_loader
      .instantiate_all()
      .expect("source receives index zero");
    assert!(
      collision_loader
        .register_prelinked_graph(graph)
        .is_err(),
      "late graph registration must reject an already-used reserved index"
    );
  }

  #[test]
  fn precompiled_virtual_modules_keep_source_metadata_and_stable_indices() {
    let source = crate::js!(module, "export const value = 42;");
    let session = crate::parser::Parser::parse_source_with_type(source, SourceType::mjs())
      .expect("precompiled source parses");
    let context = ModuleContext {
      module_index: 7,
      imports: HashMap::new(),
      exports: HashSet::from(["value".to_string()]),
      namespace_exports: HashMap::new(),
    };
    let chunk =
      crate::compiler::compile_oxc_with_ctx(session.program(), Some(&context), session.source())
        .expect("precompiled source compiles");
    let mut loader = ModuleLoader::new();
    loader
      .register_virtual_module(VirtualModule::precompiled(
        "test:precompiled",
        source,
        SourceType::mjs(),
        None,
        7,
        chunk,
      ))
      .expect("precompiled registration succeeds");
    loader
      .register_virtual_module(VirtualModule::javascript(
        "test:source",
        crate::js!(module, "export const other = 1;"),
        SourceType::mjs(),
        None,
      ))
      .expect("source registration succeeds");
    let source_entry = loader
      .construct(Path::new("."), "test:source")
      .expect("source module constructs");
    let entry = loader
      .construct(Path::new("."), "test:precompiled")
      .expect("precompiled module constructs");
    loader
      .instantiate_all()
      .expect("precompiled module instantiates");
    assert_eq!(entry.borrow().module_index, Some(7));
    assert_eq!(source_entry.borrow().module_index, Some(8));

    let globals = Rc::new(RefCell::new(ObjectData::new()));
    let mut vm = Vm::new(globals);
    let entry_path = entry.borrow().path.clone();
    loader
      .evaluate_entry(&entry_path, &mut vm, false)
      .expect("precompiled module evaluates");
    assert_eq!(
      entry
        .borrow()
        .exports
        .get("value")
        .expect("value export")
        .borrow()
        .clone(),
      Value::Number(42.0),
    );
  }

  #[test]
  fn generated_bytecode_virtual_modules_are_loaded_and_run() {
    let session = crate::parser::Parser::parse_source(crate::js!("globalThis.bytecodeResult = 7;"))
      .expect("bytecode source parses");
    let chunk = crate::compiler::compile(session.program(), session.source()).expect("compiles");
    let mut loader = ModuleLoader::new();
    loader
      .register_virtual_module(VirtualModule::new(
        "test:bytecode",
        VirtualModuleSource::Bytecode(chunk),
        SourceType::mjs(),
        None,
      ))
      .expect("bytecode registration succeeds");
    let entry = loader
      .construct(Path::new("."), "test:bytecode")
      .expect("bytecode module constructs");
    loader
      .instantiate_all()
      .expect("bytecode module instantiates");

    let globals = Rc::new(RefCell::new(ObjectData::new()));
    let mut vm = Vm::new(globals);
    let entry_path = entry.borrow().path.clone();
    loader
      .evaluate_entry(&entry_path, &mut vm, false)
      .expect("bytecode module evaluates");
    assert_eq!(vm.get_global("bytecodeResult"), Some(Value::Number(7.0)));
  }
}
