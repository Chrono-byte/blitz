// SPDX-License-Identifier: MPL-2.0
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
  resolve_with_options(base, specifier, module_options())
}

/// Resolve a module specifier to a TypeScript declaration file.
///
/// This follows the normal module resolver, but gives package declarations
/// precedence through `types`/`typings` main fields and the `types` exports
/// condition. Packages that resolve to JavaScript can still provide a nearby
/// declaration file, so sibling `.d.ts` variants are checked as a fallback.
///
/// # Errors
///
/// Returns a `BundlerError` if no declaration file can be resolved.
pub fn resolve_declaration_specifier(
  base: &Path,
  specifier: &str,
) -> Result<PathBuf, crate::errors::BundlerError> {
  let resolved = resolve_with_options(base, specifier, declaration_options())
    .or_else(|_| resolve_with_options(base, specifier, declaration_fallback_options()))?;
  if is_declaration_path(&resolved) {
    return Ok(resolved);
  }

  declaration_siblings(&resolved)
    .find(|path| path.is_file())
    .ok_or_else(|| {
      crate::errors::BundlerError::from(format!(
        "No declaration file found near {}",
        resolved.display()
      ))
    })
}

fn resolve_with_options(
  base: &Path,
  specifier: &str,
  options: oxc_resolver::ResolveOptions,
) -> Result<PathBuf, crate::errors::BundlerError> {
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

fn module_options() -> oxc_resolver::ResolveOptions {
  oxc_resolver::ResolveOptions {
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
  }
}

fn declaration_options() -> oxc_resolver::ResolveOptions {
  oxc_resolver::ResolveOptions {
    extensions: vec![
      ".d.ts".into(),
      ".d.mts".into(),
      ".d.cts".into(),
      ".ts".into(),
      ".mts".into(),
      ".cts".into(),
      ".tsx".into(),
      ".js".into(),
      ".mjs".into(),
      ".cjs".into(),
    ],
    condition_names: vec!["types".into()],
    main_fields: vec!["types".into(), "typings".into(), "main".into()],
    ..Default::default()
  }
}

fn declaration_fallback_options() -> oxc_resolver::ResolveOptions {
  oxc_resolver::ResolveOptions {
    condition_names: vec!["types".into(), "import".into()],
    ..declaration_options()
  }
}

fn is_declaration_path(path: &Path) -> bool {
  let path = path.to_string_lossy();
  path.ends_with(".d.ts") || path.ends_with(".d.mts") || path.ends_with(".d.cts")
}

fn declaration_siblings(path: &Path) -> impl Iterator<Item = PathBuf> + '_ {
  ["d.ts", "d.mts", "d.cts"]
    .into_iter()
    .map(move |extension| {
      let mut sibling = path.to_path_buf();
      sibling.set_extension(extension);
      sibling
    })
}
