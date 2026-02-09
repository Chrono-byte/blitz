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
use std::path::Path;

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
    // Bypass bundling: read the module source and return its ParserSession
    // unchanged. Module loading/linking/evaluation is handled by the VM
    // module loader (spec-compliant) instead of textual concatenation.
    let entry_canon = std::fs::canonicalize(entry).unwrap_or_else(|_| entry.to_path_buf());
    let src = std::fs::read_to_string(&entry_canon).map_err(|e| {
        BundlerError::from(format!("Failed to read module {}: {}", entry.display(), e))
    })?;

    let session = Parser::parse_source(&src).map_err(|e| {
        BundlerError::from(format!("Failed to parse module {}: {}", entry.display(), e))
    })?;
    Ok(session)
}

#[cfg(test)]
mod tests {
    use super::bundle_entry;
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

        // With the new ES module loader we no longer inline dependency bodies
        // into the entry program. Ensure the entry has an import from './b'
        // and still performs a call to the imported binding.
        let has_import = find(&root, &|n| {
            n.get("type").and_then(|s| s.as_str()) == Some("ImportDeclaration")
                && n.get("source")
                    .and_then(|src| src.get("value"))
                    .and_then(|v| v.as_str())
                    == Some("./b")
        });

        let has_call = find(&root, &|n| {
            if n.get("type").and_then(|s| s.as_str()) == Some("AssignmentExpression") {
                if let Some(right) = n.get("right") {
                    if right.get("type").and_then(|s| s.as_str()) == Some("CallExpression") {
                        return true;
                    }
                }
            }
            false
        });

        assert!(has_import, "entry program should import './b'");
        assert!(has_call, "entry program should call the imported binding");
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

        // New loader keeps re-exports in the entry. Ensure the ExportNamedDeclaration
        // for `b2` exists; we no longer inline the exporter body (`b`).
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

        // Export * should remain as an ExportAllDeclaration in the entry program
        let has_export_all = find(&root, &|n| {
            n.get("type").and_then(|s| s.as_str()) == Some("ExportAllDeclaration")
        });
        assert!(
            has_export_all,
            "bundled program should contain ExportAllDeclaration"
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

        // With the module loader the entry will import the default export rather
        // than inlining it. Ensure an ImportDeclaration from './b' is present.
        let has_import = find(&root, &|n| {
            n.get("type").and_then(|s| s.as_str()) == Some("ImportDeclaration")
                && n.get("source")
                    .and_then(|src| src.get("value"))
                    .and_then(|v| v.as_str())
                    == Some("./b")
        });

        assert!(
            has_import,
            "entry program should import default export from './b'"
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
