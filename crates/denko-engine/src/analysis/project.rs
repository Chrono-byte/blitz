// SPDX-License-Identifier: MPL-2.0

use super::*;

#[derive(Clone, Copy)]
pub(super) struct AnalysisSources<'a> {
  pub(super) overlays: &'a HashMap<PathBuf, String>,
  pub(super) module_declarations: &'a HashMap<String, String>,
  pub(super) module_documentation: &'a HashMap<(String, String), String>,
  pub(super) declaration_paths: &'a HashMap<String, PathBuf>,
}

#[derive(Clone)]
struct CachedProjectAnalysis {
  source: String,
  strict: bool,
  analysis: Analysis,
}

/// Persistent compiler state for an editor workspace.
#[derive(Default)]
pub struct AnalysisProject {
  overlays: HashMap<PathBuf, String>,
  module_declarations: HashMap<String, String>,
  module_documentation: HashMap<(String, String), String>,
  pub(super) declaration_cache: HashMap<PathBuf, String>,
  declaration_paths: HashMap<String, PathBuf>,
  analyses: HashMap<PathBuf, CachedProjectAnalysis>,
  dependencies: HashMap<PathBuf, HashSet<PathBuf>>,
  reverse_dependencies: HashMap<PathBuf, HashSet<PathBuf>>,
}

impl AnalysisProject {
  /// Create project state with declarations for runtime or package modules.
  #[must_use]
  pub fn with_module_declarations(
    declarations: impl IntoIterator<Item = (String, String)>
  ) -> Self {
    Self {
      module_declarations: declarations.into_iter().collect(),
      ..Self::default()
    }
  }

  /// Add documentation for declarations supplied by virtual modules.
  #[must_use]
  pub fn with_module_documentation(
    mut self,
    documentation: impl IntoIterator<Item = (String, String, String)>,
  ) -> Self {
    self.module_documentation = documentation
      .into_iter()
      .map(|(specifier, symbol, documentation)| ((specifier, symbol), documentation))
      .collect();
    self
  }

  /// Add or update an open document overlay.
  pub fn update_document(
    &mut self,
    source: String,
    path: &Path,
  ) {
    let path = normalize_path(path);
    if self.overlays.get(&path) != Some(&source) {
      self.overlays.insert(path.clone(), source);
      let declaration_changed = self.refresh_declaration_overlay(&path);
      if declaration_changed {
        self.analyses.clear();
      }
      self.invalidate(&path);
    }
  }

  /// Analyze and retain an open document, invalidating affected dependents when it changes.
  pub fn analyze_document(
    &mut self,
    source: String,
    path: &Path,
    strict: bool,
  ) -> Analysis {
    let path = normalize_path(path);
    self.update_document(source.clone(), &path);
    if self.discover_declarations(&source, &path) {
      self.analyses.clear();
    }
    if let Some(cached) = self.analyses.get(&path)
      && cached.source == source
      && cached.strict == strict
    {
      return cached.analysis.clone();
    }

    let mut dependencies = HashSet::new();
    let analysis = analyze_with_sources(
      &source,
      &path,
      strict,
      AnalysisSources {
        overlays: &self.overlays,
        module_declarations: &self.module_declarations,
        module_documentation: &self.module_documentation,
        declaration_paths: &self.declaration_paths,
      },
      &mut dependencies,
    );
    self.record_dependencies(&path, dependencies);
    self.analyses.insert(
      path,
      CachedProjectAnalysis {
        source,
        strict,
        analysis: analysis.clone(),
      },
    );
    analysis
  }

  /// Remove an editor overlay and invalidate analyses that depended on it.
  pub fn close_document(
    &mut self,
    path: &Path,
  ) {
    let path = normalize_path(path);
    if self.overlays.remove(&path).is_some() {
      let declaration_changed = self.refresh_declaration_overlay(&path);
      if declaration_changed {
        self.analyses.clear();
      }
      self.invalidate(&path);
    }
    self.remove_dependencies(&path);
  }

  fn discover_declarations(
    &mut self,
    source: &str,
    importer: &Path,
  ) -> bool {
    let mut changed = false;
    for specifier in source_imports(source, importer) {
      changed |= self.discover_declaration(importer, &specifier);
    }
    changed
  }

  fn discover_declaration(
    &mut self,
    importer: &Path,
    specifier: &str,
  ) -> bool {
    if self.module_declarations.contains_key(specifier) {
      return false;
    }
    let Some(path) = resolve_declaration_path(importer, specifier, &self.overlays) else {
      return false;
    };
    let Some(source) = self.read_declaration(&path) else {
      return false;
    };
    self
      .declaration_paths
      .insert(specifier.to_string(), path.clone());
    self
      .module_declarations
      .insert(specifier.to_string(), source.clone());

    let mut changed = true;
    for dependency in source_imports(&source, &path) {
      changed |= self.discover_declaration(&path, &dependency);
    }
    changed
  }

  fn read_declaration(
    &mut self,
    path: &Path,
  ) -> Option<String> {
    if let Some(source) = self.overlays.get(path) {
      return Some(source.clone());
    }
    if let Some(source) = self.declaration_cache.get(path) {
      return Some(source.clone());
    }
    let source = std::fs::read_to_string(path).ok()?;
    self
      .declaration_cache
      .insert(path.to_path_buf(), source.clone());
    Some(source)
  }

  fn refresh_declaration_overlay(
    &mut self,
    path: &Path,
  ) -> bool {
    let affected = self
      .declaration_paths
      .iter()
      .filter(|(_, declaration_path)| declaration_path.as_path() == path)
      .map(|(specifier, _)| specifier.clone())
      .collect::<Vec<_>>();
    let changed = !affected.is_empty();
    for specifier in affected {
      if let Some(source) = self.read_declaration(path) {
        self.module_declarations.insert(specifier, source);
      } else {
        self.module_declarations.remove(&specifier);
        self.declaration_paths.remove(&specifier);
      }
    }
    changed
  }

  fn invalidate(
    &mut self,
    changed: &Path,
  ) {
    let mut pending = vec![changed.to_path_buf()];
    let mut invalidated = HashSet::new();
    while let Some(path) = pending.pop() {
      if !invalidated.insert(path.clone()) {
        continue;
      }
      self.analyses.remove(&path);
      if let Some(dependents) = self.reverse_dependencies.get(&path) {
        pending.extend(dependents.iter().cloned());
      }
    }
  }

  fn record_dependencies(
    &mut self,
    importer: &Path,
    dependencies: HashSet<PathBuf>,
  ) {
    self.remove_dependencies(importer);
    self
      .dependencies
      .insert(importer.to_path_buf(), dependencies.clone());
    for dependency in dependencies {
      self
        .reverse_dependencies
        .entry(dependency)
        .or_default()
        .insert(importer.to_path_buf());
    }
  }

  fn remove_dependencies(
    &mut self,
    importer: &Path,
  ) {
    let Some(previous) = self.dependencies.remove(importer) else {
      return;
    };
    for dependency in previous {
      let remove_entry = self
        .reverse_dependencies
        .get_mut(&dependency)
        .is_some_and(|importers| {
          importers.remove(importer);
          importers.is_empty()
        });
      if remove_entry {
        self.reverse_dependencies.remove(&dependency);
      }
    }
  }
}
