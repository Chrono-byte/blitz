// SPDX-License-Identifier: MPL-2.0

use oxc_resolver::{Resolver, TsconfigDiscovery, TsconfigOptions, TsconfigReferences};
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
  resolve_specifier_with_conditions(base, specifier, &[])
}

/// Resolve a module specifier with additional package export conditions.
///
/// # Errors
///
/// Returns a `BundlerError` if the specifier cannot be resolved.
pub fn resolve_specifier_with_conditions(
  base: &Path,
  specifier: &str,
  conditions: &[String],
) -> Result<PathBuf, crate::errors::BundlerError> {
  resolve_with_options(base, specifier, module_options(conditions))
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
  containing_file: &Path,
  specifier: &str,
) -> Result<PathBuf, crate::errors::BundlerError> {
  let resolver = Resolver::new(declaration_options());
  resolver
    .resolve_dts(containing_file, specifier)
    .map(|resolution| resolution.full_path())
    .map_err(|error| crate::errors::BundlerError::from(error.to_string()))
}

fn resolve_with_options(
  base: &Path,
  specifier: &str,
  mut options: oxc_resolver::ResolveOptions,
) -> Result<PathBuf, crate::errors::BundlerError> {
  if matches!(options.tsconfig, Some(TsconfigDiscovery::Auto))
    && let Some(config_file) = nearest_tsconfig(base)
  {
    options.tsconfig = Some(TsconfigDiscovery::Manual(TsconfigOptions {
      config_file,
      references: TsconfigReferences::Auto,
    }));
  }
  let resolver = Resolver::new(options);

  let base_dir = std::fs::canonicalize(base).unwrap_or_else(|_| base.to_path_buf());

  resolver
    .resolve(&base_dir, specifier)
    .map(|resolution| resolution.full_path())
    .map_err(|e| crate::errors::BundlerError::from(e.to_string()))
}

fn nearest_tsconfig(base: &Path) -> Option<PathBuf> {
  let mut directory = base;
  loop {
    let candidate = directory.join("tsconfig.json");
    if candidate.is_file() {
      return Some(candidate);
    }
    if directory
      .file_name()
      .is_some_and(|name| name == "node_modules")
    {
      return None;
    }
    directory = directory.parent()?;
  }
}

fn module_options(additional_conditions: &[String]) -> oxc_resolver::ResolveOptions {
  let mut condition_names = additional_conditions.to_vec();
  for condition in ["node", "import", "default"] {
    if !condition_names
      .iter()
      .any(|existing| existing == condition)
    {
      condition_names.push(condition.to_string());
    }
  }
  oxc_resolver::ResolveOptions {
    extensions: vec![
      ".js".into(),
      ".mjs".into(),
      ".cjs".into(),
      ".ts".into(),
      ".mts".into(),
      ".cts".into(),
      ".tsx".into(),
    ],
    condition_names,
    tsconfig: Some(TsconfigDiscovery::Auto),
    node_path: false,
    ..Default::default()
  }
}

fn declaration_options() -> oxc_resolver::ResolveOptions {
  oxc_resolver::ResolveOptions {
    condition_names: vec!["types".into()],
    tsconfig: Some(TsconfigDiscovery::Auto),
    node_path: false,
    ..Default::default()
  }
}
