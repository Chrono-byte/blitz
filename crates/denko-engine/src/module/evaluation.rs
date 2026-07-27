use super::*;

impl ModuleLoader {
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
  pub(super) fn resolve_export(
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
          if rec.host_initialized {
            rec.state = ModuleState::Evaluated;
            continue;
          }
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

  /// Evaluate all instantiated modules in loader insertion order.
  ///
  /// This bulk path is synchronous; entry-point evaluation uses the
  /// dependency-derived order in [`Self::evaluate_entry`].
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
        if rec.host_initialized {
          rec.state = ModuleState::Evaluated;
          continue;
        }
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
