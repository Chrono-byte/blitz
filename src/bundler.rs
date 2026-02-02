//! Simple module bundler / module-graph loader.
//!
//! This implements a minimal, pragmatic bundling strategy used by the CLI:
//! - Resolve static top-level `import` declarations with relative specifiers.
//! - Walk the module graph (DFS), inline module declarations in dependency
//!   order so that dependencies appear before importers.
//! - Skip bare (non-relative) imports (treated as external).
//!
//! This is intentionally small and conservative: it avoids textual source
//! rewriting and relies on the parser to produce an AST for each module. The
//! bundler does not perform renaming/namespacing or advanced symbol resolution;
//! it simply concatenates module bodies in an order that satisfies direct
//! dependencies. Cycles are handled by visiting nodes once.

use crate::ast::{Program, ProgramKind, Statement, TypeStrictness};
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
pub fn bundle_entry(entry: &Path) -> Result<Program, String> {
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
    ) -> Result<(), String> {
        if visited.contains(&path) {
            return Ok(());
        }
        // Mark as visited early to avoid cycles causing infinite recursion.
        visited.insert(path.clone());

        // Read source
        let src = std::fs::read_to_string(&path)
            .map_err(|e| format!("Failed to read module {}: {}", path.display(), e))?;

        // Extract import specifiers (static top-level imports)
        let imports = Parser::extract_imports_from_source(&src)?;

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

    // Now parse each module (again) and collect statements into a single Program.
    let mut combined: Vec<Statement> = Vec::new();
    for path in &order {
        let src = std::fs::read_to_string(path)
            .map_err(|e| format!("Failed to read module {}: {}", path.display(), e))?;

        // Debug: print bundling order
        eprintln!("Loading module: {}", path.display());

        let program = Parser::parse_source(&src)?;
        for stmt in program.body {
            combined.push(stmt);
        }
    }

    Ok(Program {
        kind: ProgramKind::Module,
        type_strictness: TypeStrictness::Soft,
        body: combined,
    })
}

#[cfg(test)]
mod tests {
    use super::bundle_entry;
    use crate::ast::Statement;
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

        // Expect that the bundled program contains the function `b`.
        let mut found_b = false;
        let mut found_consumer = false;
        for s in &program.body {
            match s {
                Statement::FunctionDecl(fd) => {
                    if fd.name == "b" {
                        found_b = true;
                    }
                }
                Statement::ExpressionStatement(_) => {
                    // simple heuristic to detect the consumer expression was preserved
                    found_consumer = true;
                }
                _ => {}
            }
        }
        assert!(found_b, "bundled program should contain function 'b'");
        assert!(
            found_consumer,
            "bundled program should contain the consumer expression"
        );
    }

    #[test]
    fn missing_file_emits_error() {
        let tmp = make_temp_dir();
        let mod_a = tmp.join("nonexistent_main.ts");
        let err = bundle_entry(&mod_a).expect_err("should fail on missing entry");
        assert!(
            err.contains("Failed to read module"),
            "unexpected error: {}",
            err
        );
    }
}
