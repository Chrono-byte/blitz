use super::*;

impl ModuleLoader {
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

  pub(super) fn initialize_namespace_reexports(
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
        let slots = registration
          .binding_slots
          .as_ref()
          .expect("prelinked binding slots were just allocated");
        for bindings in registration.host_module_bindings.values() {
          for (binding_id, value) in bindings {
            let binding_id = usize::try_from(*binding_id)
              .map_err(|_| BundlerError::from("Prelinked host binding ID does not fit usize"))?;
            let slot = slots
              .get(binding_id)
              .ok_or_else(|| BundlerError::from("Prelinked host binding ID is out of range"))?;
            *slot.borrow_mut() = value.clone();
          }
        }
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
    // For each parsed source module, examine its AST for exported names and
    // allocate slots. Prelinked records already carry their export metadata.
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
                    crate::vm::runtime::modules::build_module_namespace_object(&exports_snapshot),
                  ));
                  {
                    let mut rec = rec_rc.borrow_mut();
                    rec.imports.insert(local.clone(), boxed);
                  }
                  // Record a synthetic import source with the module index and
                  // special "*" name; namespace object wiring happens later.
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
}
