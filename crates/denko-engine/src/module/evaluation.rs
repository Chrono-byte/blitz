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
      let resolved = resolve_specifier_with_conditions(
        entry.parent().unwrap_or_else(|| Path::new(".")),
        &format!(
          "./{}",
          entry
            .file_name()
            .expect("entry must have a file name")
            .to_string_lossy()
        ),
        &self.condition_names,
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

  pub(super) fn resolve_export(
    &mut self,
    module_path: &Path,
    export_name: &str,
    resolve_set: &mut HashSet<(String, String)>,
  ) -> Result<Option<BindingCell>, BundlerError> {
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

    let mut found: Option<BindingCell> = None;
    for source_spec in star_exports {
      let dep_rec_rc = self.construct_from_importer(&base_dir, importer, &source_spec)?;
      let dep_path = dep_rec_rc.borrow().path.clone();
      let mut branch_set = resolve_set.clone();
      if let Some(slot) = self.resolve_export(&dep_path, export_name, &mut branch_set)? {
        match &found {
          None => found = Some(slot),
          Some(existing) => {
            if !existing.same_storage(&slot) {
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
    let entry = self.prepare_evaluation(entry, vm, enable_typecheck)?;
    let promise = async_evaluation::evaluate(vm, &entry)?;
    async_evaluation::finish_host_evaluation(vm, &promise)
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
    let paths = self.map.keys().cloned().collect::<Vec<_>>();
    for path in paths {
      self.evaluate_entry(&path, vm, enable_typecheck)?;
    }
    Ok(())
  }
}
