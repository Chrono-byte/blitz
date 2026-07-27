use super::*;

impl ModuleLoader {
  pub(super) fn virtual_declarations_for_typecheck(
    &self
  ) -> Vec<crate::typecheck::VirtualDeclaration> {
    let mut declarations = HashMap::new();
    for registration in &self.prelinked_graphs {
      // Declaration imports can reference public roots that are not runtime
      // dependencies of the importing JavaScript module. Include every
      // declaration validated for this registration so those type-only edges
      // remain resolvable without relying on hard-coded specifier prefixes.
      declarations.extend(
        registration
          .declarations
          .iter()
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
    self.register_prelinked_graph_with_declarations_and_host_modules(
      graph,
      std::iter::empty::<(String, String)>(),
      Vec::new(),
    )
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
    self.register_prelinked_graph_with_declarations_and_host_modules(
      graph,
      declarations,
      Vec::new(),
    )
  }

  /// Register a parser-free graph, declarations, and private host modules atomically.
  ///
  /// Host-module targets must be private modules in `graph`, and the embedder
  /// must provide every declared export exactly once. Host-module bytecode is
  /// validated with the graph but is never executed.
  ///
  /// # Errors
  ///
  /// Returns an error before registration for invalid graph metadata,
  /// declarations, host modules, or existing identity/index collisions.
  pub fn register_prelinked_graph_with_declarations_and_host_modules<I, S, T>(
    &mut self,
    graph: PrelinkedModuleGraph,
    declarations: I,
    host_modules: Vec<PrelinkedHostModule>,
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

    let public_module_indices = graph
      .public_roots
      .iter()
      .copied()
      .collect::<HashSet<_>>();
    let modules_by_specifier = graph
      .modules
      .iter()
      .map(|module| (module.specifier.as_str(), module))
      .collect::<HashMap<_, _>>();
    let mut host_module_bindings = HashMap::new();
    for host_module in host_modules {
      let module = modules_by_specifier
        .get(host_module.specifier.as_str())
        .ok_or_else(|| {
          BundlerError::from(format!(
            "Prelinked host module target {:?} is not in the graph",
            host_module.specifier
          ))
        })?;
      if public_module_indices.contains(&module.module_index) {
        return Err(BundlerError::from(format!(
          "Prelinked host module target {:?} is public",
          host_module.specifier
        )));
      }
      if host_module_bindings.contains_key(&module.module_index) {
        return Err(BundlerError::from(format!(
          "Duplicate prelinked host module target: {}",
          host_module.specifier
        )));
      }
      let declared_exports = module
        .exports
        .iter()
        .map(|export| (export.exported_name.as_str(), export.binding_id))
        .collect::<HashMap<_, _>>();
      if host_module.exports.len() != declared_exports.len()
        || host_module
          .exports
          .keys()
          .any(|name| !declared_exports.contains_key(name.as_str()))
      {
        return Err(BundlerError::from(format!(
          "Prelinked host module {:?} must initialize exactly its declared exports",
          host_module.specifier
        )));
      }
      let bindings = host_module
        .exports
        .into_iter()
        .map(|(name, value)| (declared_exports[name.as_str()], value))
        .collect();
      host_module_bindings.insert(module.module_index, bindings);
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
        host_module_bindings,
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

  /// Return the canonical URL and metadata object for a module record.
  ///
  /// The object is cached on the record, so repeated `import.meta` accesses
  /// in one realm preserve identity. The loader owns the record and therefore
  /// also owns the metadata lifetime; no host state is involved.
  pub(crate) fn import_meta_for(
    &mut self,
    specifier: &str,
  ) -> Option<Value> {
    self
      .records_by_specifier
      .get(specifier)
      .cloned()
      .map(|record| record.borrow_mut().import_meta())
  }

  pub(super) fn fixed_module_indices(&self) -> HashMap<usize, String> {
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

  pub(super) fn construct_prelinked_at(
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
    let host_initialized = self
      .prelinked_graphs
      .get(location.graph_id)
      .is_some_and(|registration| {
        registration
          .host_module_bindings
          .contains_key(&module.module_index)
      });
    let record = Rc::new(RefCell::new(ModuleRecord::new_prelinked(
      virtual_path.clone(),
      module.specifier.clone(),
      dependencies.clone(),
      module_index,
      module.chunk,
      declaration_source,
      content_hash,
      host_initialized,
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
}
