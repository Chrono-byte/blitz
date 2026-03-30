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
use crate::compiler::ModuleContext;
use crate::errors::BundlerError;
use crate::parser::ParserSession;
use crate::resolver::resolve_specifier;
use crate::value::Value;
use crate::vm::Vm;

use oxc_span::SourceType;
use serde_json::Value as JsonValue;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

fn source_type_for_path(path: &Path) -> SourceType {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if name.ends_with(".tsx") {
        SourceType::tsx()
    } else if name.ends_with(".ts") || name.ends_with(".mts") || name.ends_with(".cts") {
        SourceType::ts()
    } else {
        SourceType::unambiguous()
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
    /// Canonical absolute path to the module source file.
    pub path: PathBuf,
    /// Parsed module session containing the AST and parser artifacts.
    pub session: ParserSession,
    /// Current loader lifecycle state for this module.
    state: ModuleState,
    /// Assigned numeric module index used by the VM module table. Set during instantiation.
    pub module_index: Option<usize>,
    /// Export slots: mapping export name -> shared slot (`Rc<RefCell<Value>>`)
    pub exports: HashMap<String, Rc<RefCell<Value>>>,
    /// Import bindings: local name -> exporter slot
    pub imports: HashMap<String, Rc<RefCell<Value>>>,
    /// Import metadata: local name -> (module_index, exported_name)
    pub import_sources: HashMap<String, (usize, String)>,
    // pub chunk: Option<crate::bytecode::Chunk>,
}

impl ModuleRecord {
    fn new(path: PathBuf, session: ParserSession) -> Self {
        Self {
            path,
            session,
            state: ModuleState::Parsed,
            module_index: None,
            exports: HashMap::new(),
            imports: HashMap::new(),
            import_sources: HashMap::new(),
        }
    }
}

/// ModuleLoader holds a per-VM module map and implements Construction,
/// Instantiation, and Evaluation per the ECMAScript module semantics.
pub struct ModuleLoader {
    /// Cache of constructed module records keyed by canonical module path.
    pub map: HashMap<PathBuf, Rc<RefCell<ModuleRecord>>>,
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
        }
    }

    /// Construct (fetch & parse) a module and its dependencies.
    pub fn construct(
        &mut self,
        base: &Path,
        spec: &str,
    ) -> Result<Rc<RefCell<ModuleRecord>>, BundlerError> {
        let resolved = resolve_specifier(base, spec)?;
        let resolved_canon = std::fs::canonicalize(&resolved).unwrap_or(resolved);
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
        let source_type = source_type_for_path(&resolved_canon);
        let session =
            crate::parser::Parser::parse_source_with_type(&src, source_type).map_err(|e| {
                BundlerError::from(format!("Parse error {}: {}", resolved_canon.display(), e))
            })?;
        let rec = Rc::new(RefCell::new(ModuleRecord::new(
            resolved_canon.clone(),
            session,
        )));
        self.map.insert(resolved_canon.clone(), Rc::clone(&rec));

        // Recurse into dependencies discovered by parser helper
        let imports =
            crate::parser::Parser::extract_imports_from_source_with_type(&src, source_type)
                .map_err(|e| {
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
    /// implementation that creates shared `Rc<RefCell<Value>>` slots for
    /// exported names so importers can hold live bindings to the same slot.
    pub fn instantiate_all(&mut self) -> Result<(), BundlerError> {
        // For each module, examine its AST for exported names and allocate slots.
        for rec_rc in self.map.values() {
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
                                }
                                if let Some(decl) = item.get("declaration")
                                    && let Some(decl_type) =
                                        decl.get("type").and_then(|d| d.as_str())
                                {
                                    if (decl_type == "FunctionDeclaration"
                                        || decl_type == "ClassDeclaration")
                                        && let Some(id) = decl
                                            .get("id")
                                            .and_then(|i| i.get("name"))
                                            .and_then(|n| n.as_str())
                                    {
                                        rec.exports.insert(
                                            id.to_string(),
                                            Rc::new(RefCell::new(Value::Undefined)),
                                        );
                                    } else if decl_type == "VariableDeclaration"
                                        && let Some(decls) =
                                            decl.get("declarations").and_then(|d| d.as_array())
                                    {
                                        for var_decl in decls {
                                            if let Some(name) = var_decl
                                                .get("id")
                                                .and_then(|id| id.get("name"))
                                                .and_then(|n| n.as_str())
                                            {
                                                rec.exports.insert(
                                                    name.to_string(),
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
        // bindings to the same Rc<RefCell<Value>> as exporters. We iterate over
        // a snapshot of module paths so mutation of self.map during construct
        // doesn't interfere with iteration.
        let module_paths: Vec<PathBuf> = self.map.keys().cloned().collect();
        // Assign a numeric module index to each module and prepare an export map
        // vector that the VM will consume. We don't have access to the Vm here,
        // so we'll store the index in the ModuleRecord and the caller should
        // populate Vm.module_table from ModuleLoader.map after instantiation.
        let mut next_index: usize = 0;
        for path in &module_paths {
            if let Some(rec_rc) = self.map.get(path) {
                let mut rec = rec_rc.borrow_mut();
                if rec.module_index.is_none() {
                    rec.module_index = Some(next_index);
                    next_index += 1;
                }
            }
        }
        for path in module_paths {
            let rec_rc = match self.map.get(&path) {
                Some(r) => Rc::clone(r),
                None => continue,
            };
            let mut rec = rec_rc.borrow_mut();
            // Walk ESTree JSON import declarations to wire live-binding slots
            let estree = rec.session.program().to_pretty_estree_ts_json(false);
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
                let base_dir = rec.path.parent().unwrap_or(Path::new("."));
                let dep_rec_rc = match self.construct(base_dir, &spec) {
                    Ok(r) => r,
                    Err(_) => continue,
                };

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
                                        .map(ToString::to_string)
                                        .unwrap_or(local.clone());
                                    let slot_opt =
                                        { dep_rec_rc.borrow().exports.get(&imported).cloned() };
                                    if let Some(slot) = slot_opt {
                                        rec.imports.insert(local.clone(), Rc::clone(&slot));
                                        if let Some(src_idx) = dep_rec_rc.borrow().module_index {
                                            rec.import_sources.insert(local, (src_idx, imported));
                                        }
                                    } else {
                                        let new_slot = Rc::new(RefCell::new(Value::Undefined));
                                        dep_rec_rc
                                            .borrow_mut()
                                            .exports
                                            .insert(imported.clone(), Rc::clone(&new_slot));
                                        rec.imports.insert(local.clone(), Rc::clone(&new_slot));
                                        if let Some(src_idx) = dep_rec_rc.borrow().module_index {
                                            rec.import_sources.insert(local, (src_idx, imported));
                                        }
                                    }
                                }
                                "ImportDefaultSpecifier" => {
                                    let local = sp
                                        .get("local")
                                        .and_then(|l| l.get("name"))
                                        .and_then(|n| n.as_str())
                                        .unwrap_or("")
                                        .to_string();
                                    let slot_opt =
                                        { dep_rec_rc.borrow().exports.get("default").cloned() };
                                    if let Some(slot) = slot_opt {
                                        rec.imports.insert(local.clone(), Rc::clone(&slot));
                                        if let Some(src_idx) = dep_rec_rc.borrow().module_index {
                                            rec.import_sources
                                                .insert(local, (src_idx, "default".to_string()));
                                        }
                                    } else {
                                        let new_slot = Rc::new(RefCell::new(Value::Undefined));
                                        dep_rec_rc
                                            .borrow_mut()
                                            .exports
                                            .insert("default".to_string(), Rc::clone(&new_slot));
                                        rec.imports.insert(local.clone(), Rc::clone(&new_slot));
                                        if let Some(src_idx) = dep_rec_rc.borrow().module_index {
                                            rec.import_sources
                                                .insert(local, (src_idx, "default".to_string()));
                                        }
                                    }
                                }
                                "ImportNamespaceSpecifier" => {
                                    let local = sp
                                        .get("local")
                                        .and_then(|l| l.get("name"))
                                        .and_then(|n| n.as_str())
                                        .unwrap_or("")
                                        .to_string();
                                    // Create a module namespace object backed by live export slots.
                                    // Per ECMAScript semantics the namespace should present
                                    // live bindings to the exported slots. We implement
                                    // that by installing accessor properties whose getter
                                    // reads the underlying shared `Rc<RefCell<Value>>`
                                    // slot.
                                    let ns_map: Rc<RefCell<crate::value::ObjectData>> =
                                        Rc::new(RefCell::new(crate::value::ObjectData::new()));

                                    // For each exported name, install an accessor that
                                    // returns the current value of the export slot. The
                                    // accessor has no setter (immutable from JS), and is
                                    // non-configurable to match namespace invariants.
                                    let exports_snapshot = { dep_rec_rc.borrow().exports.clone() };
                                    for (k, slot) in &exports_snapshot {
                                        let slot = Rc::clone(slot);
                                        let getter: crate::value::BuiltinFn =
                                            Rc::new(move |_vm: &mut Vm, _args: &[Value]| {
                                                Ok(slot.borrow().clone())
                                            });
                                        ns_map.borrow_mut().properties.insert(
                                            crate::value::PropertyKey::String(k.clone()),
                                            crate::value::PropertyDescriptor::Accessor {
                                                get: Some(Value::Builtin(getter)),
                                                set: None,
                                                enumerable: true,
                                                configurable: false,
                                            },
                                        );
                                    }

                                    // Namespace objects are not extensible per the spec.
                                    ns_map.borrow_mut().extensible = false;

                                    let boxed = Rc::new(RefCell::new(Value::Object(ns_map)));
                                    rec.imports.insert(local.clone(), boxed);
                                    // Namespace imports are represented as plain objects for now.
                                    // Record a synthetic import source with module index and special "*" name
                                    if let Some(src_idx) = dep_rec_rc.borrow().module_index {
                                        rec.import_sources
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
    pub fn evaluation_order_from(&mut self, entry: &Path) -> Result<Vec<PathBuf>, BundlerError> {
        let resolved = resolve_specifier(
            entry.parent().unwrap_or(Path::new(".")),
            &format!("./{}", entry.file_name().unwrap().to_string_lossy()),
        )?;
        let resolved_canon = std::fs::canonicalize(&resolved).unwrap_or(resolved);
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
            let imports = crate::parser::Parser::extract_imports_from_source_with_type(
                &src,
                source_type_for_path(path),
            )
            .map_err(|e| BundlerError::from(format!("Parse error {}: {}", path.display(), e)))?;
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
            self.construct(
                base_dir,
                &format!("./{}", entry.file_name().unwrap().to_string_lossy()),
            )?;
        }

        visit(self, &resolved_canon, &mut temp, &mut perm, &mut order)?;
        Ok(order)
    }

    /// Evaluate a module graph starting from entry in DFS post-order.
    pub fn evaluate_entry(
        &mut self,
        entry: &Path,
        vm: &mut Vm,
        enable_typecheck: bool,
    ) -> Result<(), crate::errors::RuntimeError> {
        // Populate vm.module_table with the export slot maps for each
        // instantiated module. ModuleRecord.module_index must have been set
        // during instantiate_all(). We clone the HashMap of Rc slots so the
        // VM receives ownership of the map structure but still shares the
        // underlying Rc<RefCell<Value>> live binding slots.
        for rec_rc in self.map.values() {
            let rec = rec_rc.borrow();
            if let Some(idx) = rec.module_index {
                let exports_clone = rec.exports.clone();
                vm.install_module_exports(idx, exports_clone);
            }
        }
        let order = self.evaluation_order_from(entry).map_err(|e| {
            crate::errors::RuntimeError::from(format!("Module resolution failed: {}", e))
        })?;

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
                        crate::typecheck::check_oxc(program).map_err(|e| {
                            crate::errors::RuntimeError::from(format!("Typecheck failed: {}", e))
                        })?;
                    }
                    // Build ModuleContext from the instantiated record so the compiler
                    // can emit module ops for imports/exports.
                    let module_idx = rec.module_index.expect("module_index should be set");
                    let ctx = ModuleContext {
                        module_index: module_idx,
                        imports: rec.import_sources.clone(),
                        exports: rec.exports.keys().cloned().collect(),
                    };
                    let chunk = crate::compiler::compile_oxc_with_ctx(
                        program,
                        Some(&ctx),
                        rec.session.source(),
                    )
                    .map_err(|e| {
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
                    crate::typecheck::check_oxc(program).map_err(|e| {
                        crate::errors::RuntimeError::from(format!("Typecheck failed: {}", e))
                    })?;
                }
                let chunk =
                    crate::compiler::compile_oxc_with_ctx(program, None, rec.session.source())
                        .map_err(|e| {
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

/// Evaluate a module graph using a shared loader handle.
///
/// This helper borrows the loader only for short phases so nested dynamic
/// imports can re-enter the same cache while module code is running.
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
        loader_ref.evaluation_order_from(entry).map_err(|e| {
            crate::errors::RuntimeError::from(format!("Module resolution failed: {}", e))
        })?
    };

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
                    crate::typecheck::check_oxc(program).map_err(|e| {
                        crate::errors::RuntimeError::from(format!("Typecheck failed: {}", e))
                    })?;
                }
                let module_idx = rec.module_index.expect("module_index should be set");
                let ctx = ModuleContext {
                    module_index: module_idx,
                    imports: rec.import_sources.clone(),
                    exports: rec.exports.keys().cloned().collect(),
                };
                let chunk = crate::compiler::compile_oxc_with_ctx(
                    program,
                    Some(&ctx),
                    rec.session.source(),
                )
                .map_err(|e| crate::errors::RuntimeError::from(format!("Compile failed: {}", e)))?;
                Some((
                    rec_rc_return,
                    crate::runtime_utils::clear_slot_types_in_chunk(chunk),
                ))
            } else {
                None
            }
        };

        if let Some((rec_rc, chunk)) = maybe_chunk {
            vm.run(chunk)?;
            rec_rc.borrow_mut().state = ModuleState::Evaluated;
        }
    }

    Ok(())
}
