//! Module loading and evaluation.
//!
//! Conformance notes and known deviations from ECMA-262
//!
//! This ModuleLoader implements a practical subset of ECMAScript module
//! semantics suitable for running test harnesses and small module graphs, but
//! it intentionally simplifies or omits some of the spec's details. The most
//! important divergences are documented here so callers and reviewers can make
//! informed decisions about correctness and prioritise future work.
//!
//! 1) Module namespace objects
//! - Current behaviour: namespace imports (`import * as ns from 'mod'`) are
//!   represented as plain `Object` values whose own properties are populated
//!   by copying the current export slots' values at time of construction.
//! - Spec behaviour: module namespace objects must be exotic objects that
//!   present live bindings to exporter slots, be non-writable and
//!   non-configurable for exported names, and be non-extensible. The
//!   runtime should expose the same live-binding view that importers see.
//! - Impact: code that modifies exported bindings after namespace creation
//!   will not be observed by existing namespace objects. Replace the plain
//!   object with an exotic namespace object backed by the export slots to
//!   become spec-compliant.
//!
//! 2) Dynamic import timing and async behaviour
//! - Current behaviour: dynamic import resolution/evaluation is performed
//!   synchronously in some paths and the returned `Promise` may be resolved
//!   immediately rather than being resolved via a host job/microtask.
//! - Spec behaviour: `import()` returns a Promise that is resolved/rejected
//!   asynchronously via the host job queue; module linking/evaluation must be
//!   performed according to the module linking steps and errors surface as
//!   rejected promises.
//! - Impact: timing differences are observable in Promise ordering and
//!   microtask sequencing tests.
//!
//! 3) Linking/instantiation simplifications
//! - Current behaviour: instantiate_all uses a simplified ESTree-based scan
//!   to allocate export slots and wire imports. Re-exports (export *),
//!   indirect exports, and some symbol/export resolution edge cases are not
//!   fully implemented.
//! - Spec behaviour: module linking includes a precise export resolution
//!   algorithm that handles indirect exports, re-exports, and cycles.
//! - Impact: complex module graphs and advanced re-export patterns may not
//!   behave as the spec describes.
//!
//! Recommendations:
//! - Implement module namespace objects backed by Rc slots and mark
//!   properties non-configurable/non-writable; ensure the object is
//!   non-extensible.
//! - Update dynamic import to schedule resolution via microtasks/jobs and
//!   follow the module linking steps exactly (Construct, Instantiate,
//!   Evaluate) with proper error propagation.
//! - Replace ESTree-copy wiring with a full export resolution pass that
//!   handles indirect and export-all semantics.
use crate::bytecode::Chunk;
use crate::compiler::ModuleContext;
use crate::errors::BundlerError;
use crate::parser::ParserSession;
use crate::resolver::resolve_specifier;
use crate::value::{SharedValue, Value};
use crate::vm::Vm;

use oxc_span::SourceType;
use serde_json::Value as JsonValue;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};

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

/// A ModuleRecord represents a source text module parsed from disk and
/// holds cached artifacts required for linking and evaluation.
pub struct ModuleRecord {
  /// Canonical path or virtual identity for the module.
  pub path: PathBuf,
  /// Canonical specifier for virtual modules, or the path string for files.
  pub specifier: String,
  /// Parsed module session containing the AST and parser artifacts.
  pub session: ParserSession,
  /// Parser mode used for this module.
  pub source_type: SourceType,
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
  /// Star export sources.
  pub star_exports: Vec<String>,
  // pub chunk: Option<crate::bytecode::Chunk>,
}

impl ModuleRecord {
  fn new(
    path: PathBuf,
    specifier: String,
    session: ParserSession,
    source_type: SourceType,
    bytecode: Option<Chunk>,
    declaration_source: Option<String>,
    content_hash: Option<u64>,
  ) -> Self {
    Self {
      path,
      specifier,
      session,
      source_type,
      bytecode,
      declaration_source,
      content_hash,
      state: ModuleState::Parsed,
      module_index: None,
      exports: HashMap::new(),
      imports: HashMap::new(),
      import_sources: HashMap::new(),
      indirect_exports: Vec::new(),
      star_exports: Vec::new(),
    }
  }
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
    }
  }

  /// Register an embedder-provided module for this loader's realm.
  pub fn register_virtual_module(
    &mut self,
    module: VirtualModule,
  ) -> Result<(), BundlerError> {
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
      self
        .virtual_modules
        .register_alias(alias, canonical)?;
    }
    Ok(())
  }

  /// Register an alias for an already registered virtual module.
  pub fn register_virtual_alias(
    &mut self,
    alias: impl Into<String>,
    canonical_specifier: impl Into<String>,
  ) -> Result<(), BundlerError> {
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
    self.virtual_modules.declaration_source(specifier)
  }

  /// Return declaration source using its canonical specifier.
  #[must_use]
  pub fn declaration_source_for_specifier(
    &self,
    specifier: &str,
  ) -> Option<&str> {
    self.declaration_source(specifier)
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
    if let Some(module) = self.virtual_modules.resolve(&requested).cloned() {
      let canonical = module.canonical_specifier.clone();
      if let Some(existing) = self.records_by_specifier.get(&canonical) {
        return Ok(Rc::clone(existing));
      }

      let source = module.source.as_javascript().unwrap_or_default();
      let session = crate::parser::Parser::parse_source_with_type(source, module.source_type)
        .map_err(|error| {
          BundlerError::from(format!("Parse error virtual module {canonical}: {error}"))
        })?;
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
      let mut record = ModuleRecord::new(
        virtual_path.clone(),
        canonical.clone(),
        session,
        module.source_type,
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
    let session =
      crate::parser::Parser::parse_source_with_type(&src, source_type).map_err(|error| {
        BundlerError::from(format!("Parse error {}: {error}", resolved_canon.display()))
      })?;
    let record = Rc::new(RefCell::new(ModuleRecord::new(
      resolved_canon.clone(),
      canonical.clone(),
      session,
      source_type,
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

    let imports = crate::parser::Parser::extract_imports_from_source_with_type(&src, source_type)
      .map_err(|error| {
      BundlerError::from(format!("Parse error {}: {error}", resolved_canon.display()))
    })?;
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
        let mut resolve_set = std::collections::HashSet::new();
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
        // Use ESTree JSON representation to find export declarations
        let estree = rec
          .session
          .program()
          .to_pretty_estree_ts_json(false);
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
                      rec
                        .exports
                        .insert(exported.to_string(), Rc::new(Mutex::new(Value::Undefined)));
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
                      .insert(id.to_string(), Rc::new(Mutex::new(Value::Undefined)));
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
                          .insert(name.to_string(), Rc::new(Mutex::new(Value::Undefined)));
                      }
                    }
                  }
                }
              }
              "ExportDefaultDeclaration" => {
                rec
                  .exports
                  .insert("default".to_string(), Rc::new(Mutex::new(Value::Undefined)));
              }
              "ExportAllDeclaration" => {
                if let Some(source) = item
                  .get("source")
                  .and_then(|s| s.get("value"))
                  .and_then(|v| v.as_str())
                {
                  rec.star_exports.push(source.to_string());
                }
              }
              _ => {}
            }
          }
        }

        rec.state = ModuleState::Instantiated;
      }
    }

    // Second pass: wire imports to exporter slots so importers hold live
    // bindings to the same Rc<RefCell<Value>> as exporters. We iterate over
    // a snapshot because resolution may discover additional filesystem records.
    let mut module_paths: Vec<PathBuf> = self.map.keys().cloned().collect();
    module_paths.sort();
    self.resolve_all_indirect_exports(&module_paths)?;
    // Assign a numeric module index to each module and prepare an export map
    // vector that the VM will consume. We don't have access to the Vm here,
    // so we'll store the index in the ModuleRecord and the caller should
    // populate Vm.module_table from ModuleLoader.map after instantiation.
    let mut assigned_indices = HashMap::new();
    for path in &module_paths {
      let Some(record) = self.map.get(path) else {
        continue;
      };
      if let Some(index) = record.borrow().module_index
        && let Some(previous) = assigned_indices.insert(index, path)
      {
        return Err(BundlerError::from(format!(
          "Duplicate precompiled module index {index} for {} and {}",
          previous.display(),
          path.display(),
        )));
      }
    }
    let mut next_index = self
      .map
      .values()
      .filter_map(|record| record.borrow().module_index)
      .chain(
        self
          .virtual_modules
          .iter()
          .filter_map(|(_, module)| match &module.source {
            VirtualModuleSource::Precompiled { module_index, .. } => Some(*module_index),
            VirtualModuleSource::JavaScript(_) | VirtualModuleSource::Bytecode(_) => None,
          }),
      )
      .max()
      .map_or(0, |index| index.saturating_add(1));
    for path in &module_paths {
      if let Some(rec_rc) = self.map.get(path) {
        let mut rec = rec_rc.borrow_mut();
        if rec.module_index.is_none() {
          rec.module_index = Some(next_index);
          next_index += 1;
        }
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
        rec
          .session
          .program()
          .to_pretty_estree_ts_json(false)
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
                  let mut resolve_set = std::collections::HashSet::new();
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
                  let mut resolve_set = std::collections::HashSet::new();
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
                  let boxed = Rc::new(Mutex::new(
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

      // Read source from the record so virtual modules use the same graph walk
      // as filesystem modules.
      let (src, source_type) = if let Some(record) = loader.map.get(path) {
        let record = record.borrow();
        (record.session.source().to_string(), record.source_type)
      } else {
        let src = std::fs::read_to_string(path).map_err(|e| {
          BundlerError::from(format!("Failed to read module {}: {}", path.display(), e))
        })?;
        (src, source_type_for_module_path(path))
      };
      let imports = crate::parser::Parser::extract_imports_from_source_with_type(&src, source_type)
        .map_err(|e| BundlerError::from(format!("Parse error {}: {}", path.display(), e)))?;
      let (base_dir, importer) = if let Some(record) = loader.map.get(path) {
        let record = record.borrow();
        (
          record
            .path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf(),
          record
            .path
            .to_string_lossy()
            .starts_with(VIRTUAL_PATH_PREFIX)
            .then(|| record.specifier.clone()),
        )
      } else {
        (
          path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf(),
          None,
        )
      };
      for spec in imports {
        // attempt to ensure dependency constructed
        let dep = loader.construct_from_importer(&base_dir, importer.as_deref(), &spec)?;
        let dep_path = dep.borrow().path.clone();
        visit(loader, &dep_path, temp, perm, order)?;
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
    resolve_set: &mut std::collections::HashSet<(String, String)>,
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
    let order = self
      .evaluation_order_from(entry)
      .map_err(|e| crate::errors::RuntimeError::from(format!("Module resolution failed: {}", e)))?;

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
          let program = rec.session.program();
          if enable_typecheck {
            crate::typecheck::check(program)
              .map_err(|e| crate::errors::RuntimeError::from(format!("Typecheck failed: {}", e)))?;
          }
          // Build ModuleContext from the instantiated record so the compiler
          // can emit module ops for imports/exports.
          let module_idx = rec
            .module_index
            .expect("module_index should be set");
          let ctx = ModuleContext {
            module_index: module_idx,
            imports: rec.import_sources.clone(),
            exports: rec.exports.keys().cloned().collect(),
          };
          let chunk = if let Some(chunk) = rec.bytecode.clone() {
            chunk
          } else {
            crate::compiler::compile_oxc_with_ctx(program, Some(&ctx), rec.session.source())
              .map_err(|e| crate::errors::RuntimeError::from(format!("Compile failed: {}", e)))?
          };
          let chunk = crate::runtime_utils::clear_slot_types_in_chunk(chunk);
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
    // For simplicity evaluate modules in insertion order after instantiation.
    for rec_rc in self.map.values() {
      let mut rec = rec_rc.borrow_mut();
      if matches!(rec.state, ModuleState::Instantiated) {
        rec.state = ModuleState::Evaluating;
        // Compile program
        let program = rec.session.program();
        if enable_typecheck {
          crate::typecheck::check(program)
            .map_err(|e| crate::errors::RuntimeError::from(format!("Typecheck failed: {}", e)))?;
        }
        let chunk = if let Some(chunk) = rec.bytecode.clone() {
          chunk
        } else {
          crate::compiler::compile_oxc_with_ctx(program, None, rec.session.source())
            .map_err(|e| crate::errors::RuntimeError::from(format!("Compile failed: {}", e)))?
        };
        // Clear slot types to allow dynamic prelude patterns
        let chunk = crate::runtime_utils::clear_slot_types_in_chunk(chunk);
        // Run chunk
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
  jobs: Vec<ModuleCompileJob>
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
              crate::parser::Parser::parse_source_with_type(&job.source, job.source_type)
                .map_err(|error| format!("Parse failed: {error}"))
                .and_then(|session| {
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
  }

  let order = {
    let mut loader_ref = loader.borrow_mut();
    loader_ref
      .evaluation_order_from(entry)
      .map_err(|e| crate::errors::RuntimeError::from(format!("Module resolution failed: {}", e)))?
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
        Some(ModuleCompileJob {
          path: path.clone(),
          source: record.session.source().to_string(),
          source_type: record.source_type,
          context: ModuleContext {
            module_index: record
              .module_index
              .expect("module_index should be set"),
            imports: record.import_sources.clone(),
            exports: record.exports.keys().cloned().collect(),
          },
        })
      })
      .collect()
  };
  let mut compiled = compile_modules_parallel(compile_jobs)?;

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
        let program = rec.session.program();
        if enable_typecheck {
          crate::typecheck::check(program)
            .map_err(|e| crate::errors::RuntimeError::from(format!("Typecheck failed: {}", e)))?;
        }
        let module_idx = rec
          .module_index
          .expect("module_index should be set");
        let ctx = ModuleContext {
          module_index: module_idx,
          imports: rec.import_sources.clone(),
          exports: rec.exports.keys().cloned().collect(),
        };
        let chunk = if let Some(chunk) = rec.bytecode.clone() {
          crate::runtime_utils::clear_slot_types_in_chunk(chunk)
        } else if let Some(chunk) = compiled.remove(&path) {
          chunk
        } else {
          let chunk =
            crate::compiler::compile_oxc_with_ctx(program, Some(&ctx), rec.session.source())
              .map_err(|e| crate::errors::RuntimeError::from(format!("Compile failed: {}", e)))?;
          crate::runtime_utils::clear_slot_types_in_chunk(chunk)
        };
        rec.bytecode = Some(chunk.clone());
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
  use crate::value::{ObjectData, Value};
  use std::cell::RefCell;
  use std::rc::Rc;

  #[test]
  fn virtual_registry_keeps_stable_payload_hashes_and_declarations() {
    let first = VirtualModule::javascript(
      "test:module",
      "export const value = 1;",
      SourceType::mjs(),
      Some("export declare const value: number;".to_string()),
    );
    let second = VirtualModule::javascript(
      "test:module",
      "export const value = 1;",
      SourceType::mjs(),
      Some("export declare const value: number;".to_string()),
    );
    let changed = VirtualModule::javascript(
      "test:module",
      "export const value = 2;",
      SourceType::mjs(),
      None,
    );

    assert_eq!(first.content_hash, second.content_hash);
    assert_ne!(first.content_hash, changed.content_hash);
    assert_eq!(
      first.declaration_source.as_deref(),
      Some("export declare const value: number;")
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
        "export const answer = 42;",
        SourceType::mjs(),
        None,
      ))
      .expect("dependency registration succeeds");
    loader
      .register_virtual_module(VirtualModule::javascript(
        "test:entry",
        "import { answer } from 'test:dependency'; globalThis.result = answer;",
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
  fn relative_virtual_imports_use_importer_namespace() {
    let mut loader = ModuleLoader::new();
    loader
      .register_virtual_module(VirtualModule::javascript(
        "app/shared/value.js",
        "export const value = 9;",
        SourceType::mjs(),
        None,
      ))
      .expect("dependency registration succeeds");
    loader
      .register_virtual_module(VirtualModule::javascript(
        "app/entry.js",
        "import { value } from './shared/value.js'; globalThis.relativeResult = value;",
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
        "export const value = 11;",
        SourceType::mjs(),
        Some("export declare const value: number;".to_string()),
      ))
      .expect("value registration succeeds");
    loader
      .register_virtual_module(VirtualModule::javascript(
        "pkg/reexport.js",
        "export { value } from './value.js';",
        SourceType::mjs(),
        None,
      ))
      .expect("reexport registration succeeds");
    loader
      .register_virtual_module(VirtualModule::javascript(
        "pkg/entry.js",
        "import { value } from 'pkg/alias.js'; globalThis.aliasResult = value;",
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
      Some("export declare const value: number;")
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
  fn virtual_dependency_errors_are_not_dropped() {
    let mut loader = ModuleLoader::new();
    loader
      .register_virtual_module(VirtualModule::javascript(
        "broken/entry.js",
        "import './missing.js'; export const value = 1;",
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
        "import { value as bValue } from './b.js'; export const value = bValue;",
        SourceType::mjs(),
        None,
      ))
      .expect("a registration succeeds");
    loader
      .register_virtual_module(VirtualModule::javascript(
        "cycle/b.js",
        "import { value as aValue } from './a.js'; export const value = aValue;",
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
  fn precompiled_virtual_modules_keep_source_metadata_and_stable_indices() {
    let source = "export const value = 42;";
    let session = crate::parser::Parser::parse_source_with_type(source, SourceType::mjs())
      .expect("precompiled source parses");
    let context = ModuleContext {
      module_index: 7,
      imports: HashMap::new(),
      exports: std::collections::HashSet::from(["value".to_string()]),
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
        "export const other = 1;",
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
        .lock()
        .expect("value export lock")
        .clone(),
      Value::Number(42.0),
    );
  }

  #[test]
  fn generated_bytecode_virtual_modules_are_loaded_and_run() {
    let session = crate::parser::Parser::parse_source("globalThis.bytecodeResult = 7;")
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
