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
    // pub chunk: Option<crate::bytecode::Chunk>,
}

impl ModuleRecord {
    fn new(path: PathBuf, session: ParserSession) -> Self {
        Self {
            path,
            session,
            state: ModuleState::Parsed,
            exports: HashMap::new(),
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
                let chunk = crate::main::clear_slot_types_in_chunk(chunk);
                // Run chunk
                vm.run(chunk)?;
                rec.state = ModuleState::Evaluated;
            }
        }
        Ok(())
    }
}
