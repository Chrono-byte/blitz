//! Module bundler for resolving and combining TypeScript modules.
//!
//! The bundler implements a minimal, pragmatic module resolution strategy for the CLI:
//! - Resolves static top-level `import` declarations with relative specifiers
//! - Performs depth-first search (DFS) to walk the module graph
//! - Inlines modules in dependency-first order (dependencies before importers)
//! - Skips bare (non-relative) imports, treating them as external dependencies
//! - Handles circular dependencies by tracking visited nodes
//!
//! # Design
//!
//! This bundler is intentionally conservative and avoids:
//! - Textual source rewriting
//! - Renaming or namespacing of symbols
//! - Advanced symbol resolution
//!
//! Instead, it relies on the parser to produce an AST and simply concatenates
//! module bodies in an order that satisfies direct dependencies.
//!
//! # Example
//!
//! ```no_run
//! use tsblitz::bundler::bundle_entry;
//! use std::path::Path;
//!
//! let program = bundle_entry(Path::new("src/main.ts"))?;
//! // The bundled program contains all dependencies inlined
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

use crate::errors::BundlerError;
use crate::local_ast as la;
use crate::parser::Parser;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Bundle an entry module (filesystem `Path`) and its relative imports into a
/// single `Program` suitable for typechecking/compilation.
///
/// Returns a `Program` whose `body` is the concatenation of all modules'
/// top-level statements in a dependency-first order (dependencies appear
/// before consumers).
///
/// # Errors
/// Returns an error if the entry module or any of its dependencies cannot be
/// read or parsed.
pub fn bundle_entry(entry: &Path) -> Result<la::Program, BundlerError> {
    // Resolve the entry to a canonical path where possible to avoid duplicates.
    let entry_canon = std::fs::canonicalize(entry).unwrap_or_else(|_| entry.to_path_buf());

    // Track visited files and produce an ordering (push on finish -> deps first).
    let mut visited: HashSet<PathBuf> = HashSet::new();
    let mut order: Vec<PathBuf> = Vec::new();

    // DFS traversal
    fn visit(
        path: PathBuf,
        visited: &mut HashSet<PathBuf>,
        order: &mut Vec<PathBuf>,
    ) -> Result<(), BundlerError> {
        if visited.contains(&path) {
            return Ok(());
        }
        // Mark as visited early to avoid cycles causing infinite recursion.
        visited.insert(path.clone());

        // Read source
        let src = std::fs::read_to_string(&path).map_err(|e| {
            BundlerError::from(format!("Failed to read module {}: {}", path.display(), e))
        })?;

        // Extract import specifiers (static top-level imports)
        let imports = Parser::extract_imports_from_source(&src).map_err(|e| {
            BundlerError::from(format!(
                "Failed to parse imports in {}: {}",
                path.display(),
                e
            ))
        })?;

        // Resolve relative imports and visit them
        for spec in imports {
            if spec.starts_with('.') {
                let base = path.parent().unwrap_or_else(|| Path::new(""));
                let mut target = base.join(&spec);
                // Prefer `.ts` local files: map `.js` -> `.ts` and default extension to `.ts`
                if target.extension().and_then(|s| s.to_str()) == Some("js") {
                    target.set_extension("ts");
                } else if target.extension().is_none() {
                    target.set_extension("ts");
                }

                // Canonicalize where possible so the visited set is consistent.
                let target_canon = std::fs::canonicalize(&target).unwrap_or(target);

                // Recurse
                visit(target_canon, visited, order)?;
            } else {
                // Bare / non-relative import - treat as external and ignore.
                continue;
            }
        }

        // Finished exploring dependencies; add to ordering.
        order.push(path);
        Ok(())
    }

    // Build the module ordering
    visit(entry_canon, &mut visited, &mut order)?;

    // Concatenate module sources in dependency-first order and parse the
    // combined source to produce a single oxc AST `Program`.
    let mut combined_src = String::new();
    for path in &order {
        let src = std::fs::read_to_string(path).map_err(|e| {
            BundlerError::from(format!("Failed to read module {}: {}", path.display(), e))
        })?;

        // Debug: print bundling order
        eprintln!("Loading module: {}", path.display());

        // Insert a newline and a simple file marker to keep boundaries clear
        combined_src.push_str(&format!("// --- {} ---\n", path.display()));
        combined_src.push_str(&src);
        combined_src.push('\n');
    }

    // Parse the combined source into a ParserSession which owns backing data,
    // then convert Oxc's ESTree JSON into the owned `local_ast::Program` used
    // by the compiler/typechecker. This keeps bundler callers working with an
    // owned AST and avoids widespread changes across the codebase.
    let session = Parser::parse_source(&combined_src)
        .map_err(|e| BundlerError::from(format!("Failed to parse bundled program: {}", e)))?;

    let estree = session.program().to_pretty_estree_ts_json(false);
    let prog = crate::estree_bridge::estree_json_to_local_program(&estree)
        .map_err(|e| BundlerError::from(format!("ESTree -> local AST conversion failed: {}", e)))?;

    // Post-process the converted local AST to remove Import/Export wrapper
    // statements and to hoist any declarations carried inside export nodes.
    // This keeps the bundled `Program` simple (top-level declarations present
    // as direct statements) and allows the compiler/typechecker to operate on
    // a flattened list of statements.
    let mut new_body: Vec<la::Statement> = Vec::new();
    for stmt in prog.body.into_iter() {
        match stmt {
            la::Statement::ImportDecl(id) => {
                // Handle import declarations conservatively while keeping the
                // bundle flattened. For namespace imports (`import * as ns`)
                // synthesize a namespace object from previously inlined
                // declarations. For default and named imports create simple
                // aliasing var declarations where possible so local names are
                // available to the consumer module. If the imported symbol is
                // not found, emit a `Declare` so the typechecker sees it.
                fn has_decl(body: &Vec<la::Statement>, name: &str) -> bool {
                    body.iter().any(|s| match s {
                        la::Statement::FunctionDecl(f) => f.name == name,
                        la::Statement::VarDecl(v) => v.name == name,
                        la::Statement::ClassDecl(c) => c.name == name,
                        _ => false,
                    })
                }

                for (local, imported) in id.specifiers.into_iter() {
                    if imported == "*" {
                        // Build namespace object from earlier declarations.
                        let mut props: Vec<la::Property> = Vec::new();
                        // Include functions/vars/classes declared so far.
                        for s in &new_body {
                            match s {
                                la::Statement::FunctionDecl(f) => props.push(la::Property {
                                    key: f.name.clone(),
                                    value: la::Expr::Identifier(la::Identifier {
                                        name: f.name.clone(),
                                    }),
                                }),
                                la::Statement::VarDecl(v) => props.push(la::Property {
                                    key: v.name.clone(),
                                    value: la::Expr::Identifier(la::Identifier {
                                        name: v.name.clone(),
                                    }),
                                }),
                                la::Statement::ClassDecl(c) => props.push(la::Property {
                                    key: c.name.clone(),
                                    value: la::Expr::Identifier(la::Identifier {
                                        name: c.name.clone(),
                                    }),
                                }),
                                _ => {}
                            }
                        }
                        // Also include a default export if present
                        if has_decl(&new_body, "__default_export") {
                            props.push(la::Property {
                                key: "default".to_string(),
                                value: la::Expr::Identifier(la::Identifier {
                                    name: "__default_export".to_string(),
                                }),
                            });
                        }

                        let ns_obj = la::Statement::VarDecl(la::VarDecl {
                            name: local.clone(),
                            type_ann: None,
                            init: Some(la::Expr::ObjectExpr(la::ObjectExpr { properties: props })),
                        });
                        new_body.push(ns_obj);
                    } else if imported == "default" {
                        if has_decl(&new_body, "__default_export") {
                            let alias = la::Statement::VarDecl(la::VarDecl {
                                name: local.clone(),
                                type_ann: None,
                                init: Some(la::Expr::Identifier(la::Identifier {
                                    name: "__default_export".to_string(),
                                })),
                            });
                            new_body.push(alias);
                        } else {
                            new_body.push(la::Statement::Declare(la::Declare {
                                name: local.clone(),
                                type_ann: None,
                            }));
                        }
                    } else {
                        // Named import: `import { imported as local }` or `import { name }`.
                        if has_decl(&new_body, &imported) {
                            // create alias var: local = imported;
                            let alias = la::Statement::VarDecl(la::VarDecl {
                                name: local.clone(),
                                type_ann: None,
                                init: Some(la::Expr::Identifier(la::Identifier {
                                    name: imported.clone(),
                                })),
                            });
                            new_body.push(alias);
                        } else {
                            // Not found: emit a declare for the local name so the
                            // typechecker does not error on usage.
                            new_body.push(la::Statement::Declare(la::Declare {
                                name: local.clone(),
                                type_ann: None,
                            }));
                        }
                    }
                }

                continue;
            }
            la::Statement::ExportDecl(ed) => {
                if let Some(decl_box) = ed.declaration {
                    // hoist the contained declaration (e.g. `export function f(){}`).
                    new_body.push(*decl_box);
                    // Also, if the export declared a specifier mapping (e.g.
                    // `export default function f(){}` or `export {x as y}`),
                    // we may need to create a variable alias in the consumer
                    // module when the exported local name differs from the
                    // exported name. For now, synthesize simple aliasing
                    // assignments for `exported != local` cases to ensure the
                    // exported name exists in the flattened scope.
                    for (exported, local_opt) in &ed.specifiers {
                        if let Some(local_name) = local_opt {
                            if exported != local_name {
                                // create `var <exported> = <local_name>;` as a VarDecl
                                let alias = la::Statement::VarDecl(la::VarDecl {
                                    name: exported.clone(),
                                    type_ann: None,
                                    init: Some(la::Expr::Identifier(la::Identifier {
                                        name: local_name.clone(),
                                    })),
                                });
                                new_body.push(alias);
                            }
                        } else if ed.source.is_some() {
                            // re-export from another module: create a placeholder
                            // `declare` for the exported name so downstream type
                            // checking sees the symbol. We use `Declare` where
                            // possible to avoid runtime initialization.
                            let name = exported.clone();
                            new_body.push(la::Statement::Declare(la::Declare {
                                name,
                                type_ann: None,
                            }));
                        }
                    }
                } else {
                    // export without local declaration (re-export or named export)
                    // e.g. `export { x as y } from './m'` or `export * from './m'`.
                    // For named re-exports we create `Declare` entries for each
                    // exported symbol so the typechecker sees them; for `export *`
                    // we leave it as-is (no-op) because the source module is
                    // already inlined and its exports should be present.
                    if !ed.specifiers.is_empty() {
                        for (exported, local_opt) in &ed.specifiers {
                            if let Some(local_name) = local_opt {
                                // Try to find a declaration with the local name in
                                // the already-built new_body (dependency modules are
                                // inlined earlier). If found, create an alias so the
                                // exported name refers to that local symbol.
                                let found = new_body.iter().any(|s| match s {
                                    la::Statement::FunctionDecl(f) => &f.name == local_name,
                                    la::Statement::VarDecl(v) => &v.name == local_name,
                                    la::Statement::ClassDecl(c) => &c.name == local_name,
                                    _ => false,
                                });
                                if found {
                                    let alias = la::Statement::VarDecl(la::VarDecl {
                                        name: exported.clone(),
                                        type_ann: None,
                                        init: Some(la::Expr::Identifier(la::Identifier {
                                            name: local_name.clone(),
                                        })),
                                    });
                                    new_body.push(alias);
                                } else {
                                    new_body.push(la::Statement::Declare(la::Declare {
                                        name: exported.clone(),
                                        type_ann: None,
                                    }));
                                }
                            } else {
                                // No local name available; emit a Declare so the
                                // exported symbol is visible to the typechecker.
                                new_body.push(la::Statement::Declare(la::Declare {
                                    name: exported.clone(),
                                    type_ann: None,
                                }));
                            }
                        }
                    }
                    continue;
                }
            }
            other => new_body.push(other),
        }
    }

    Ok(la::Program::new(new_body))
}

#[cfg(test)]
mod tests {
    use super::bundle_entry;
    use crate::local_ast as la;
    use std::env;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    /// Create a unique temporary directory for tests.
    fn make_temp_dir() -> PathBuf {
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis();
        let tmp = env::temp_dir().join(format!("tsblitz_bundler_test_{}", millis));
        let _ = fs::create_dir_all(&tmp);
        tmp
    }

    #[test]
    fn basic_bundling_inlines_dependency_and_consumer() {
        let tmp = make_temp_dir();
        let mod_b = tmp.join("b.ts");
        let mod_a = tmp.join("a.ts");

        fs::write(&mod_b, "export function b() { return 42; }").expect("write b");
        fs::write(&mod_a, "import { b } from './b'; globalX = b();").expect("write a");

        let program = bundle_entry(&mod_a).expect("bundle ok");

        // Expect that the bundled program contains the function `b` and the
        // consumer expression `globalX = b();` by inspecting the local AST.
        let has_b = program.body.iter().any(|s| match s {
            la::Statement::FunctionDecl(fd) => fd.name == "b",
            _ => false,
        });

        let has_call = program.body.iter().any(|s| match s {
            la::Statement::ExpressionStatement(es) => match &es.expression {
                la::Expr::Assignment(a) => match *a.value.clone() {
                    la::Expr::CallExpr(ref call) => match *call.callee.clone() {
                        la::Expr::Identifier(ref id) => id.name == "b",
                        _ => false,
                    },
                    _ => false,
                },
                _ => false,
            },
            _ => false,
        });

        assert!(has_b, "bundled program should contain function 'b'");
        assert!(
            has_call,
            "bundled program should contain a call to 'b' assigned to globalX"
        );
    }

    #[test]
    fn reexport_named_creates_declare_for_exported_name() {
        let tmp = make_temp_dir();
        let mod_b = tmp.join("b.ts");
        let mod_a = tmp.join("a.ts");

        fs::write(&mod_b, "export function b() { return 42; }").expect("write b");
        fs::write(&mod_a, "export { b as b2 } from './b';").expect("write a");

        let program = bundle_entry(&mod_a).expect("bundle ok");

        let has_b = program.body.iter().any(|s| match s {
            la::Statement::FunctionDecl(fd) => fd.name == "b",
            _ => false,
        });

        let has_b2_decl = program.body.iter().any(|s| match s {
            la::Statement::Declare(d) => d.name == "b2",
            _ => false,
        });

        assert!(has_b, "bundled program should contain function 'b'");
        assert!(
            has_b2_decl,
            "bundled program should declare re-exported name 'b2'"
        );
    }

    #[test]
    fn export_all_inlines_exports() {
        let tmp = make_temp_dir();
        let mod_b = tmp.join("b.ts");
        let mod_a = tmp.join("a.ts");

        fs::write(&mod_b, "export function b() { return 42; }").expect("write b");
        fs::write(&mod_a, "export * from './b';").expect("write a");

        let program = bundle_entry(&mod_a).expect("bundle ok");
        let has_b = program.body.iter().any(|s| match s {
            la::Statement::FunctionDecl(fd) => fd.name == "b",
            _ => false,
        });
        assert!(
            has_b,
            "bundled program should inline exported function 'b' from export *"
        );
    }

    #[test]
    fn default_export_anonymous_becomes_named_and_alias_created() {
        let tmp = make_temp_dir();
        let mod_b = tmp.join("b.ts");
        let mod_a = tmp.join("a.ts");

        fs::write(&mod_b, "export default 42;").expect("write b");
        fs::write(&mod_a, "import def from './b'; globalX = def;").expect("write a");

        let program = bundle_entry(&mod_a).expect("bundle ok");

        let has_default_var = program.body.iter().any(|s| match s {
            la::Statement::VarDecl(v) => v.name == "__default_export",
            _ => false,
        });

        // The bridge creates an alias var named "default" pointing to __default_export
        let has_alias = program.body.iter().any(|s| match s {
            la::Statement::VarDecl(v) => v.name == "default",
            _ => false,
        });

        assert!(
            has_default_var,
            "bundled program should contain __default_export var"
        );
        assert!(
            has_alias,
            "bundled program should contain alias var 'default'"
        );
    }

    #[test]
    fn missing_file_emits_error() {
        use crate::errors::BundlerError;
        let tmp = make_temp_dir();
        let mod_a = tmp.join("nonexistent_main.ts");
        let result = bundle_entry(&mod_a);
        assert!(result.is_err(), "should fail on missing entry");
        let err = result.unwrap_err();
        // The bundler returns a BundlerError directly for failures.
        assert!(
            err.message.contains("Failed to read module"),
            "unexpected error: {}",
            err.message
        );
    }
}
