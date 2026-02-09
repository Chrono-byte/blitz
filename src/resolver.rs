// Lightweight wrapper around oxc_resolver that provides resolution logic
// used by the ModuleLoader. Exposes a minimal API for resolving module
// specifiers relative to a base directory.

use oxc_resolver::{ResolveOptions, Resolver};
use std::path::{Path, PathBuf};

/// Resolve a module specifier relative to `base` using `oxc_resolver` and
/// return a canonical PathBuf or a `BundlerError` on failure.
pub fn resolve_specifier(
    base: &Path,
    specifier: &str,
) -> Result<PathBuf, crate::errors::BundlerError> {
    let mut options = ResolveOptions::default();
    // Keep resolution simple: prefer .ts and .js extensions when resolving.
    options.extensions = vec![".ts".into(), ".js".into()];
    // Condition names appropriate for ESM in node-like env
    options.condition_names = vec!["node".into(), "import".into()];

    let resolver = Resolver::new(options);
    // `resolve` expects an absolute base directory; canonicalize if possible.
    let base_dir = if base.is_absolute() {
        base.to_path_buf()
    } else {
        std::fs::canonicalize(base).unwrap_or_else(|_| base.to_path_buf())
    };
    match resolver.resolve(&base_dir, specifier) {
        Ok(resolution) => Ok(resolution.full_path()),
        Err(e) => Err(crate::errors::BundlerError::from(format!(
            "resolve error: {}",
            e
        ))),
    }
}
