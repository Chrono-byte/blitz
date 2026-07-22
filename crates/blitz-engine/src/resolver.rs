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
    // Keep JavaScript precedence while allowing TypeScript module graphs.
    extensions: vec![
      ".js".into(),
      ".mjs".into(),
      ".cjs".into(),
      ".ts".into(),
      ".mts".into(),
      ".cts".into(),
      ".tsx".into(),
    ],
    condition_names: vec!["import".into()],
    ..Default::default()
  };

  let resolver = Resolver::new(options);

  // Resolve expects an absolute base directory; prefer to canonicalize, but
  // fall back to the original path if canonicalization fails (e.g. path
  // doesn't exist).
  let base_dir = std::fs::canonicalize(base).unwrap_or_else(|_| base.to_path_buf());

  resolver
    .resolve(&base_dir, specifier)
    .map(|resolution| resolution.full_path())
    .map_err(|e| crate::errors::BundlerError::from(e.to_string()))
}
