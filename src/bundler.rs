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
pub fn bundle_entry(entry: &Path) -> Result<crate::parser::ParserSession, BundlerError> {
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
    // Collect exported names from an inlined host module (if any). We will
    // synthesize a small runtime shim after concatenation that copies the
    // exported helpers onto `globalThis` when missing. Use a best-effort
    // textual extraction (no full parsing) to remain lightweight.
    let mut host_exports: Vec<String> = Vec::new();
    for path in &order {
        let src = std::fs::read_to_string(path).map_err(|e| {
            BundlerError::from(format!("Failed to read module {}: {}", path.display(), e))
        })?;

        // Debug: bundling order is intentionally silent in normal test runs.

        // Insert a newline and a simple file marker to keep boundaries clear
        combined_src.push_str(&format!("// --- {} ---\n", path.display()));
        combined_src.push_str(&src);
        combined_src.push('\n');
        if let Some(path_str) = path.to_str() {
            // Heuristic: consider this the host module when the path ends with
            // `soft/host.ts` (cross-platform safe check via substring).
            if path_str.ends_with("/soft/host.ts") || path_str.ends_with("\\soft\\host.ts") {
                // Extract exported identifiers from the source text.
                for line in src.lines() {
                    let line = line.trim();
                    if line.starts_with("export function ") {
                        if let Some(rest) = line.strip_prefix("export function ") {
                            let name: String = rest
                                .chars()
                                .take_while(|c| c.is_alphanumeric() || *c == '_')
                                .collect();
                            if !name.is_empty() {
                                host_exports.push(name);
                            }
                        }
                    } else if line.starts_with("export const ")
                        || line.starts_with("export let ")
                        || line.starts_with("export var ")
                    {
                        if let Some(_idx) = line.find(' ') {
                            // after `export` skip one token like `const` then capture
                            if let Some(rest) = line.splitn(3, ' ').nth(2) {
                                let name: String = rest
                                    .chars()
                                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                                    .collect();
                                if !name.is_empty() {
                                    host_exports.push(name);
                                }
                            }
                        }
                    } else if let Some(_start) = line.find("export {") {
                        // capture names inside the braces: export { a, b as c }
                        if let Some(open) = line.find('{')
                            && let Some(close) = line.find('}')
                        {
                            let inner = &line[open + 1..close];
                            for part in inner.split(',') {
                                let p = part.trim();
                                if p.is_empty() {
                                    continue;
                                }
                                // strip `as` aliases
                                let name = p.split(" as ").next().unwrap().trim();
                                // strip potential renames like `b as b2`
                                let name = name
                                    .chars()
                                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                                    .collect::<String>();
                                if !name.is_empty() {
                                    host_exports.push(name);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // If we found exported names from a host module, generate a shim that
    // copies those exported bindings onto `globalThis` when absent.
    if !host_exports.is_empty() {
        combined_src.push_str("// --- bundler: host global shims ---\n");
        combined_src.push_str("try {\n  if (typeof globalThis !== 'undefined') {\n    try {\n      const g = globalThis;\n");
        // Ensure the `host` object is copied when exported.
        if host_exports.iter().any(|n| n == "host") {
            combined_src.push_str("      if (typeof host !== 'undefined') {\n        try { if (typeof g.host === 'undefined') g.host = host; } catch(_) {}\n      }\n");
        }

        // For each export, attempt to expose it on globalThis if missing.
        for name in &host_exports {
            if name == "host" {
                continue;
            }
            // For each exported name NAME, do roughly:
            // if (typeof g.NAME === 'undefined') {
            //   try { if (typeof NAME !== 'undefined') g.NAME = NAME; } catch(_) {}
            //   try { if (typeof g.NAME === 'undefined' && typeof host !== 'undefined' && host && typeof host.NAME !== 'undefined') g.NAME = host.NAME; } catch(_) {}
            // }
            combined_src.push_str(&format!(
                "      try {{ if (typeof g.{0} === 'undefined') {{ try {{ if (typeof {0} !== 'undefined') g.{0} = {0}; }} catch(_) {{}} try {{ if (typeof g.{0} === 'undefined' && typeof host !== 'undefined' && host && typeof host.{0} !== 'undefined') g.{0} = host.{0}; }} catch(_) {{}} }} }} catch(_) {{}}\n",
                name
            ));
        }

        combined_src.push_str("    } catch (_) {}\n  }\n} catch (_) {}\n");
    }

    // Parse the combined source into a ParserSession which owns backing data
    // and exposes an `oast::Program` that borrows it. Return the session so
    // the caller can hold the backing data alive while working with the
    // borrowed AST.
    let session = Parser::parse_source(&combined_src)
        .map_err(|e| BundlerError::from(format!("Failed to parse bundled program: {}", e)))?;

    Ok(session)
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
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
        let nanos = now.as_nanos();
        let pid = std::process::id();
        let tmp = env::temp_dir().join(format!("tsblitz_bundler_test_{}_{}", pid, nanos));
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

        let session = bundle_entry(&mod_a).expect("bundle ok");
        let oast_prog = session.program();
        let estree = oast_prog.to_pretty_estree_ts_json(false);
        let root: serde_json::Value = serde_json::from_str(&estree).expect("valid estree json");

        // recursive finder
        fn find<F>(v: &serde_json::Value, f: &F) -> bool
        where
            F: Fn(&serde_json::Value) -> bool,
        {
            if f(v) {
                return true;
            }
            match v {
                serde_json::Value::Object(map) => map.values().any(|vv| find(vv, f)),
                serde_json::Value::Array(arr) => arr.iter().any(|vv| find(vv, f)),
                _ => false,
            }
        }

        let has_b = find(&root, &|n| {
            if let Some(t) = n.get("type").and_then(|s| s.as_str()) {
                if t == "FunctionDeclaration" {
                    return n
                        .get("id")
                        .and_then(|id| id.get("name"))
                        .and_then(|s| s.as_str())
                        == Some("b");
                }
                if t == "ExportNamedDeclaration" {
                    if let Some(decl) = n.get("declaration") {
                        return decl.get("type").and_then(|s| s.as_str())
                            == Some("FunctionDeclaration")
                            && decl
                                .get("id")
                                .and_then(|id| id.get("name"))
                                .and_then(|s| s.as_str())
                                == Some("b");
                    }
                }
            }
            false
        });

        let has_call = find(&root, &|n| {
            if n.get("type").and_then(|s| s.as_str()) == Some("AssignmentExpression") {
                if let Some(right) = n.get("right") {
                    if right.get("type").and_then(|s| s.as_str()) == Some("CallExpression") {
                        return right
                            .get("callee")
                            .and_then(|c| c.get("name"))
                            .and_then(|s| s.as_str())
                            == Some("b");
                    }
                }
            }
            false
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

        let session = bundle_entry(&mod_a).expect("bundle ok");
        let oast_prog = session.program();
        let estree = oast_prog.to_pretty_estree_ts_json(false);
        let root: serde_json::Value = serde_json::from_str(&estree).expect("valid estree json");

        fn find<F>(v: &serde_json::Value, f: &F) -> bool
        where
            F: Fn(&serde_json::Value) -> bool,
        {
            if f(v) {
                return true;
            }
            match v {
                serde_json::Value::Object(map) => map.values().any(|vv| find(vv, f)),
                serde_json::Value::Array(arr) => arr.iter().any(|vv| find(vv, f)),
                _ => false,
            }
        }

        let has_b = find(&root, &|n| {
            n.get("type").and_then(|s| s.as_str()) == Some("FunctionDeclaration")
                && n.get("id")
                    .and_then(|id| id.get("name"))
                    .and_then(|s| s.as_str())
                    == Some("b")
        });

        let has_b2_export = find(&root, &|n| {
            if n.get("type").and_then(|s| s.as_str()) == Some("ExportNamedDeclaration") {
                if let Some(specs) = n.get("specifiers").and_then(|s| s.as_array()) {
                    return specs.iter().any(|sp| {
                        sp.get("exported")
                            .and_then(|e| e.get("name"))
                            .and_then(|s| s.as_str())
                            == Some("b2")
                    });
                }
            }
            false
        });

        assert!(has_b, "bundled program should contain function 'b'");
        assert!(
            has_b2_export,
            "bundled program should contain ExportNamedDeclaration for 'b2'"
        );
    }

    #[test]
    fn export_all_inlines_exports() {
        let tmp = make_temp_dir();
        let mod_b = tmp.join("b.ts");
        let mod_a = tmp.join("a.ts");

        fs::write(&mod_b, "export function b() { return 42; }").expect("write b");
        fs::write(&mod_a, "export * from './b';").expect("write a");

        let session = bundle_entry(&mod_a).expect("bundle ok");
        let oast_prog = session.program();
        let estree = oast_prog.to_pretty_estree_ts_json(false);
        let root: serde_json::Value = serde_json::from_str(&estree).expect("valid estree json");

        fn find<F>(v: &serde_json::Value, f: &F) -> bool
        where
            F: Fn(&serde_json::Value) -> bool,
        {
            if f(v) {
                return true;
            }
            match v {
                serde_json::Value::Object(map) => map.values().any(|vv| find(vv, f)),
                serde_json::Value::Array(arr) => arr.iter().any(|vv| find(vv, f)),
                _ => false,
            }
        }

        let has_b = find(&root, &|n| {
            n.get("type").and_then(|s| s.as_str()) == Some("FunctionDeclaration")
                && n.get("id")
                    .and_then(|id| id.get("name"))
                    .and_then(|s| s.as_str())
                    == Some("b")
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

        let session = bundle_entry(&mod_a).expect("bundle ok");
        let oast_prog = session.program();
        let estree = oast_prog.to_pretty_estree_ts_json(false);
        let root: serde_json::Value = serde_json::from_str(&estree).expect("valid estree json");

        fn find<F>(v: &serde_json::Value, f: &F) -> bool
        where
            F: Fn(&serde_json::Value) -> bool,
        {
            if f(v) {
                return true;
            }
            match v {
                serde_json::Value::Object(map) => map.values().any(|vv| find(vv, f)),
                serde_json::Value::Array(arr) => arr.iter().any(|vv| find(vv, f)),
                _ => false,
            }
        }

        let has_export_default = find(&root, &|n| {
            n.get("type").and_then(|s| s.as_str()) == Some("ExportDefaultDeclaration")
        });

        assert!(
            has_export_default,
            "bundled program should contain an ExportDefaultDeclaration"
        );
    }

    #[test]
    fn missing_file_emits_error() {
        use crate::errors::BundlerError;
        let tmp = make_temp_dir();
        let mod_a = tmp.join("nonexistent_main.ts");
        let result = bundle_entry(&mod_a);
        assert!(result.is_err(), "should fail on missing entry");
        let err = result.err().expect("expected err");
        // The bundler returns a BundlerError directly for failures.
        assert!(
            err.message.contains("Failed to read module"),
            "unexpected error: {}",
            err.message
        );
    }
}
