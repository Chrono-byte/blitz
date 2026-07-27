use super::*;

impl ModuleLoader {
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

    let resolved = resolve_specifier_with_conditions(base, spec, &self.condition_names)?;
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
      // Every static import is a required graph edge. This includes bare
      // package specifiers resolved through the nearest node_modules tree.
      self.construct(base_dir, &dependency)?;
    }

    Ok(record)
  }
}
