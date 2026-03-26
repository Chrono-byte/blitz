//! Lightweight wrapper around oxc_resolver that provides resolution logic
//! used by the ModuleLoader. Exposes a minimal API for resolving module
//! specifiers relative to a base directory.

use oxc_resolver::Resolver;
use std::path::{Path, PathBuf};

/// Resolve a module specifier relative to `base` using `oxc_resolver` and
/// return a canonical PathBuf or a `BundlerError` on failure.
///
/// # Errors
///
/// Returns a `BundlerError` if the given specifier cannot be resolved.
pub fn resolve_specifier(
    base: &Path,
    specifier: &str,
) -> Result<PathBuf, crate::errors::BundlerError> {
    let options = oxc_resolver::ResolveOptions {
        extensions: vec![".ts".into(), ".js".into()],
        condition_names: vec!["node".into(), "import".into()],
        ..Default::default()
    };

    let resolver = Resolver::new(options);
    // `resolve` expects an absolute base directory; canonicalize if possible.
    let base_dir = if base.is_absolute() {
        base.to_path_buf()
    } else {
        std::fs::canonicalize(base).unwrap_or_else(|_| base.to_path_buf())
    };
    match resolver.resolve(&base_dir, specifier) {
        Ok(resolution) => Ok(resolution.full_path()),
        Err(e) => {
            // Try a small compatibility fallback: imports authored as `./foo.js`
            // may actually exist as `./foo.ts` in the source tree. Attempt to
            // resolve a `.ts` variant before failing.
            if specifier.ends_with(".js") {
                let mut alt = specifier.to_string();
                alt.truncate(alt.len() - 3);
                alt.push_str(".ts");
                if let Ok(resolution) = resolver.resolve(&base_dir, &alt) {
                    return Ok(resolution.full_path());
                }
            }

            // Also try appending `.ts` when no explicit extension was provided.
            if !specifier.contains('.') {
                let mut alt = specifier.to_string();
                alt.push_str(".ts");
                if let Ok(resolution) = resolver.resolve(&base_dir, &alt) {
                    return Ok(resolution.full_path());
                }
            }

            Err(crate::errors::BundlerError::from(format!(
                "resolve error: {}",
                e
            )))
        }
    }
}
