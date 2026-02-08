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

    Ok(prog)
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
    fn missing_file_emits_error() {
        use crate::errors::Error;
        let tmp = make_temp_dir();
        let mod_a = tmp.join("nonexistent_main.ts");
        let result = bundle_entry(&mod_a);
        assert!(result.is_err(), "should fail on missing entry");
        let err = result.unwrap_err();
        match err {
            crate::errors::Error::Bundler(be) => {
                assert!(
                    be.message.contains("Failed to read module"),
                    "unexpected error: {}",
                    be.message
                );
            }
            _ => panic!("expected BundlerError, got: {:?}", err),
        }
    }
}
