use crate::errors::BundlerError;
use crate::parser::ParserSession;
use crate::resolver::resolve_specifier;
use crate::value::Value;
use crate::vm::Vm;

use serde_json::Value as JsonValue;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

#[derive(Debug, PartialEq, Eq)]
enum ModuleState {
    Parsed,
    Instantiated,
    Evaluating,
    Evaluated,
    Errored,
}

/// A ModuleRecord represents a source text module parsed from disk and
/// holds cached artifacts required for linking and evaluation.
pub struct ModuleRecord {
    pub path: PathBuf,
    pub session: ParserSession,
    pub state: ModuleState,
    /// Export slots: mapping export name -> shared slot (Rc<RefCell<Value>>)
    pub exports: HashMap<String, Rc<RefCell<Value>>>,
    /// Import bindings: local name -> exporter slot
    pub imports: HashMap<String, Rc<RefCell<Value>>>,
    // pub chunk: Option<crate::bytecode::Chunk>,
}

impl ModuleRecord {
    fn new(path: PathBuf, session: ParserSession) -> Self {
        Self {
            path,
            session,
            state: ModuleState::Parsed,
            exports: HashMap::new(),
            imports: HashMap::new(),
        }
    }
}

/// ModuleLoader holds a per-VM module map and implements Construction,
/// Instantiation, and Evaluation per the ECMAScript module semantics.
pub struct ModuleLoader {
    pub map: HashMap<PathBuf, Rc<RefCell<ModuleRecord>>>,
}

impl ModuleLoader {
    pub fn new() -> Self {
        Self {
            map: HashMap::new(),
        }
    }

    /// Construct (fetch & parse) a module and its dependencies.
    pub fn construct(
        &mut self,
        base: &Path,
        spec: &str,
    ) -> Result<Rc<RefCell<ModuleRecord>>, BundlerError> {
        let resolved = resolve_specifier(base, spec)?;
        let resolved_canon = std::fs::canonicalize(&resolved).unwrap_or(resolved.clone());
        if let Some(existing) = self.map.get(&resolved_canon) {
            return Ok(Rc::clone(existing));
        }

        let src = std::fs::read_to_string(&resolved_canon).map_err(|e| {
            BundlerError::from(format!(
                "Failed to read module {}: {}",
                resolved_canon.display(),
                e
            ))
        })?;
        let session = crate::parser::Parser::parse_source(&src).map_err(|e| {
            BundlerError::from(format!("Parse error {}: {}", resolved_canon.display(), e))
        })?;
        let rec = Rc::new(RefCell::new(ModuleRecord::new(
            resolved_canon.clone(),
            session,
        )));
        self.map.insert(resolved_canon.clone(), Rc::clone(&rec));

        // Recurse into dependencies discovered by parser helper
        let imports = crate::parser::Parser::extract_imports_from_source(&src).map_err(|e| {
            BundlerError::from(format!("Parse error {}: {}", resolved_canon.display(), e))
        })?;
        for spec in imports {
            if spec.starts_with('.') || spec.starts_with('/') {
                let base_dir = resolved_canon.parent().unwrap_or(Path::new("."));
                self.construct(base_dir, &spec)?;
            } else {
                // Bare specifiers handled via resolver; attempt to construct too
                let base_dir = resolved_canon.parent().unwrap_or(Path::new("."));
                let _ = self.construct(base_dir, &spec);
            }
        }

        Ok(rec)
    }

    /// Instantiate modules by allocating export slots. This is a simple
    /// implementation that creates shared Rc<RefCell<Value>> slots for
    /// exported names so importers can hold live bindings to the same slot.
    pub fn instantiate_all(&mut self) -> Result<(), BundlerError> {
        // For each module, examine its AST for exported names and allocate slots.
        for (_path, rec_rc) in &self.map {
            let mut rec = rec_rc.borrow_mut();
            if matches!(rec.state, ModuleState::Parsed) {
                // Use ESTree JSON representation to find export declarations
                let estree = rec.session.program().to_pretty_estree_ts_json(false);
                let root: JsonValue = serde_json::from_str(&estree).map_err(|e| {
                    BundlerError::from(format!("Failed to parse ESTree JSON: {}", e))
                })?;
                let body = root
                    .get("body")
                    .and_then(|b| b.as_array())
                    .ok_or_else(|| BundlerError::from("Expected program body"))?;
                for item in body {
                    if let Some(typ) = item.get("type").and_then(|t| t.as_str()) {
                        match typ {
                            "ExportNamedDeclaration" => {
                                if let Some(specs) =
                                    item.get("specifiers").and_then(|s| s.as_array())
                                {
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
                                } else if let Some(decl) = item.get("declaration") {
                                    if let Some(decl_type) =
                                        decl.get("type").and_then(|d| d.as_str())
                                    {
                                        if decl_type == "FunctionDeclaration"
                                            || decl_type == "ClassDeclaration"
                                        {
                                            if let Some(id) = decl
                                                .get("id")
                                                .and_then(|i| i.get("name"))
                                                .and_then(|n| n.as_str())
                                            {
                                                rec.exports.insert(
                                                    id.to_string(),
                                                    Rc::new(RefCell::new(Value::Undefined)),
                                                );
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
                                // handled during linking/evaluation
                            }
                            _ => {}
                        }
                    }
                }

                rec.state = ModuleState::Instantiated;
            }
        }

        // Second pass: wire imports to exporter slots so importers hold live
        // bindings to the same Rc<RefCell<Value>> as exporters.
        let module_paths: Vec<PathBuf> = self.map.keys().cloned().collect();
        for path in module_paths {
            // Re-borrow per-module record (we clone the Rc so we don't hold a
            // borrow across calls that may mutably modify the loader map).
            let rec_rc = match self.map.get(&path) {
                Some(r) => Rc::clone(r),
                None => continue,
            };
            let mut rec = rec_rc.borrow_mut();
            // Parse the ESTree to find import declarations
            let estree = rec.session.program().to_pretty_estree_ts_json(false);
            let root: JsonValue = serde_json::from_str(&estree).map_err(|e| {
                BundlerError::from(format!("Failed to parse ESTree JSON: {}", e))
            })?;
            let body = root
                .get("body")
                .and_then(|b| b.as_array())
                .ok_or_else(|| BundlerError::from("Expected program body"))?;
            for item in body {
                if let Some(typ) = item.get("type").and_then(|t| t.as_str()) {
                    if typ == "ImportDeclaration" {
                        if let Some(source_node) = item.get("source") {
                            if let Some(spec) = source_node.get("value").and_then(|v| v.as_str()) {
                                // Resolve the specifier relative to this module
                                let base_dir = rec.path.parent().unwrap_or(Path::new("."));
                                // Ensure dependency is constructed; this may insert into self.map
                                let dep_rec_rc = match self.construct(base_dir, spec) {
                                    Ok(r) => r,
                                    Err(_) => continue,
                                };

                                if let Some(specs) = item.get("specifiers").and_then(|s| s.as_array()) {
                                    for sp in specs {
                                        if let Some(kind) = sp.get("type").and_then(|k| k.as_str()) {
                                            match kind {
                                                "ImportSpecifier" => {
                                                    // imported.name and local.name
                                                    if let Some(imported) = sp.get("imported").and_then(|i| i.get("name")).and_then(|n| n.as_str())
                                                        && let Some(local) = sp.get("local").and_then(|l| l.get("name")).and_then(|n| n.as_str())
                                                    {
                                                        // Try to get exporter slot without holding a borrow while mutating
                                                        let slot_opt = {
                                                            let dep = dep_rec_rc.borrow();
                                                            dep.exports.get(imported).cloned()
                                                        };
                                                        if let Some(slot) = slot_opt {
                                                            rec.imports.insert(local.to_string(), Rc::clone(&slot));
                                                        } else {
                                                            // Create a new slot on exporter and link to it
                                                            let new_slot = Rc::new(RefCell::new(Value::Undefined));
                                                            {
                                                                let mut dep_mut = dep_rec_rc.borrow_mut();
                                                                dep_mut.exports.insert(imported.to_string(), Rc::clone(&new_slot));
                                                            }
                                                            rec.imports.insert(local.to_string(), Rc::clone(&new_slot));
                                                        }
                                                    }
                                                }
                                                "ImportDefaultSpecifier" => {
                                                    if let Some(local) = sp.get("local").and_then(|l| l.get("name")).and_then(|n| n.as_str()) {
                                                        let slot_opt = {
                                                            let dep = dep_rec_rc.borrow();
                                                            dep.exports.get("default").cloned()
                                                        };
                                                        if let Some(slot) = slot_opt {
                                                            rec.imports.insert(local.to_string(), Rc::clone(&slot));
                                                        } else {
                                                            let new_slot = Rc::new(RefCell::new(Value::Undefined));
                                                            {
                                                                let mut dep_mut = dep_rec_rc.borrow_mut();
                                                                dep_mut.exports.insert("default".to_string(), Rc::clone(&new_slot));
                                                            }
                                                            rec.imports.insert(local.to_string(), Rc::clone(&new_slot));
                                                        }
                                                    }
                                                }
                                                "ImportNamespaceSpecifier" => {
                                                    if let Some(local) = sp.get("local").and_then(|l| l.get("name")).and_then(|n| n.as_str()) {
                                                        // Create namespace object (snapshot of current exports)
                                                        let ns_obj = Value::Object(Rc::new(RefCell::new(crate::value::ObjectData::new())));
                                                        let exports_snapshot = {
                                                            let dep = dep_rec_rc.borrow();
                                                            dep.exports.clone()
                                                        };
                                                        for (k, slot) in exports_snapshot.iter() {
                                                            let v = slot.borrow().clone();
                                                            if let Value::Object(o) = &ns_obj {
                                                                o.borrow_mut().properties.insert(
                                                                    crate::value::PropertyKey::String(k.clone()),
                                                                    crate::value::PropertyDescriptor::data(v),
                                                                );
                                                            }
                                                        }
                                                        let boxed = Rc::new(RefCell::new(ns_obj));
                                                        rec.imports.insert(local.to_string(), boxed);
                                                    }
                                                }
                                                _ => {}
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Compute depth-first, post-order evaluation order starting from `entry`.
    pub fn evaluation_order_from(&mut self, entry: &Path) -> Result<Vec<PathBuf>, BundlerError> {
        let resolved = resolve_specifier(entry.parent().unwrap_or(Path::new(".")), &format!("./{}", entry.file_name().unwrap().to_string_lossy()))?;
        let resolved_canon = std::fs::canonicalize(&resolved).unwrap_or(resolved.clone());
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

            // Read source and extract imports
            let src = std::fs::read_to_string(path).map_err(|e| {
                BundlerError::from(format!("Failed to read module {}: {}", path.display(), e))
            })?;
            let imports = crate::parser::Parser::extract_imports_from_source(&src).map_err(|e| {
                BundlerError::from(format!("Parse error {}: {}", path.display(), e))
            })?;
            for spec in imports {
                let base_dir = path.parent().unwrap_or(Path::new("."));
                // attempt to ensure dependency constructed
                let dep = loader.construct(base_dir, &spec)?;
                let dep_path = dep.borrow().path.clone();
                visit(loader, &dep_path, temp, perm, order)?;
            }

            temp.remove(path);
            perm.insert(path.clone(), true);
            order.push(path.clone());
            Ok(())
        }

        if !self.map.contains_key(&resolved_canon) {
            // ensure constructed
            let base_dir = entry.parent().unwrap_or(Path::new("."));
            self.construct(base_dir, &format!("./{}", entry.file_name().unwrap().to_string_lossy()))?;
        }

        visit(self, &resolved_canon, &mut temp, &mut perm, &mut order)?;
        Ok(order)
    }

    /// Evaluate a module graph starting from entry in DFS post-order.
    pub fn evaluate_entry(&mut self, entry: &Path, vm: &mut Vm) -> Result<(), crate::errors::RuntimeError> {
        let order = self
            .evaluation_order_from(entry)
            .map_err(|e| crate::errors::RuntimeError::from(format!("Module resolution failed: {}", e)))?;

        for path in order {
            if let Some(rec_rc) = self.map.get(&path) {
                let mut rec = rec_rc.borrow_mut();
                if matches!(rec.state, ModuleState::Instantiated) {
                    rec.state = ModuleState::Evaluating;
                    let program = rec.session.program();
                    crate::typecheck::check_oxc(program).map_err(|e| {
                        crate::errors::RuntimeError::from(format!("Typecheck failed: {}", e))
                    })?;
                    let chunk = crate::compiler::compile_oxc(program).map_err(|e| {
                        crate::errors::RuntimeError::from(format!("Compile failed: {}", e))
                    })?;
                    let chunk = crate::runtime_utils::clear_slot_types_in_chunk(chunk);
                    vm.run(chunk)?;
                    rec.state = ModuleState::Evaluated;
                }
            }
        }

        Ok(())
    }

    /// Evaluate module graph in depth-first, post-order. For now synchronous.
    pub fn evaluate_all(&mut self, vm: &mut Vm) -> Result<(), crate::errors::RuntimeError> {
        // For simplicity evaluate modules in insertion order after instantiation.
        for (_p, rec_rc) in &self.map {
            let mut rec = rec_rc.borrow_mut();
            if matches!(rec.state, ModuleState::Instantiated) {
                rec.state = ModuleState::Evaluating;
                // Compile program
                let program = rec.session.program();
                crate::typecheck::check_oxc(program).map_err(|e| {
                    crate::errors::RuntimeError::from(format!("Typecheck failed: {}", e))
                })?;
                let chunk = crate::compiler::compile_oxc(program).map_err(|e| {
                    crate::errors::RuntimeError::from(format!("Compile failed: {}", e))
                })?;
                // Clear slot types to allow dynamic prelude patterns
                let chunk = crate::runtime_utils::clear_slot_types_in_chunk(chunk);
                // Run chunk
                vm.run(chunk)?;
                rec.state = ModuleState::Evaluated;
            }
        }
        Ok(())
    }
}
