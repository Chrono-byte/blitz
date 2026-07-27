// SPDX-License-Identifier: MPL-2.0
//! Project discovery, package metadata, and hosted environment loading.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// The package module interpretation selected by `package.json`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PackageType {
  /// Treat `.js` files as ECMAScript modules.
  Module,
  /// Treat `.js` files as CommonJS modules.
  Commonjs,
  /// Ignore an unrecognized package type while retaining valid metadata.
  #[serde(other)]
  Unknown,
}

/// Runtime-relevant metadata from a project's `package.json`.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PackageJson {
  /// Package name.
  pub name: Option<String>,
  /// Package version.
  pub version: Option<String>,
  /// Whether registry publication is disabled.
  #[serde(default)]
  pub private: bool,
  /// Default interpretation for `.js` files.
  #[serde(rename = "type")]
  pub package_type: Option<PackageType>,
  /// Package entry point.
  pub main: Option<String>,
  /// Type declaration entry point.
  pub types: Option<String>,
  /// Executable declarations, retained in their original JSON shape.
  pub bin: Option<serde_json::Value>,
  /// Conditional package exports.
  pub exports: Option<serde_json::Value>,
  /// Internal package import aliases.
  pub imports: Option<serde_json::Value>,
  /// Workspace declarations.
  pub workspaces: Option<serde_json::Value>,
  /// Package scripts.
  #[serde(default)]
  pub scripts: BTreeMap<String, String>,
  /// Production dependencies.
  #[serde(default)]
  pub dependencies: BTreeMap<String, String>,
  /// Development dependencies.
  #[serde(default)]
  pub dev_dependencies: BTreeMap<String, String>,
  /// Peer dependencies.
  #[serde(default)]
  pub peer_dependencies: BTreeMap<String, String>,
  /// Optional dependencies.
  #[serde(default)]
  pub optional_dependencies: BTreeMap<String, String>,
}

/// Package metadata discovered for one hosted runtime invocation.
#[derive(Clone, Debug)]
pub struct ProjectContext {
  /// Working directory selected for the invocation.
  pub cwd: PathBuf,
  /// Nearest package root, or the working directory when no package exists.
  pub root: PathBuf,
  /// Path to the discovered `package.json`.
  pub package_json_path: Option<PathBuf>,
  /// Parsed package metadata.
  pub package: Option<PackageJson>,
}

/// One lifecycle command from a package script invocation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageScriptCommand {
  /// Lifecycle event name exposed to the child process.
  pub event: String,
  /// Shell source from `package.json`.
  pub command: String,
  /// Whether user arguments are forwarded to this command.
  pub forwards_arguments: bool,
}

impl ProjectContext {
  /// Discover the nearest `package.json` from a working directory upward.
  ///
  /// # Errors
  ///
  /// Returns an error when the working directory is invalid or a discovered
  /// package manifest cannot be read or decoded.
  pub fn discover(cwd: &Path) -> Result<Self, denko::errors::RuntimeError> {
    let cwd = std::fs::canonicalize(cwd).map_err(|error| {
      denko::errors::RuntimeError::from(format!(
        "Failed to resolve working directory {}: {error}",
        cwd.display()
      ))
    })?;
    let mut directory = cwd.as_path();
    loop {
      let candidate = directory.join("package.json");
      if candidate.is_file() {
        let source = std::fs::read_to_string(&candidate).map_err(|error| {
          denko::errors::RuntimeError::from(format!(
            "Failed to read {}: {error}",
            candidate.display()
          ))
        })?;
        let package = serde_json::from_str(&source).map_err(|error| {
          denko::errors::RuntimeError::from(format!(
            "Failed to parse {}: {error}",
            candidate.display()
          ))
        })?;
        return Ok(Self {
          cwd: cwd.clone(),
          root: directory.to_path_buf(),
          package_json_path: Some(candidate),
          package: Some(package),
        });
      }
      let Some(parent) = directory.parent() else {
        break;
      };
      directory = parent;
    }

    Ok(Self {
      root: cwd.clone(),
      cwd,
      package_json_path: None,
      package: None,
    })
  }

  /// Resolve a package script and its `pre`/`post` lifecycle hooks.
  #[must_use]
  pub fn package_script(
    &self,
    name: &str,
  ) -> Option<Vec<PackageScriptCommand>> {
    let scripts = &self.package.as_ref()?.scripts;
    let command = scripts.get(name)?;
    let mut commands = Vec::with_capacity(3);
    let pre_event = format!("pre{name}");
    if let Some(command) = scripts.get(&pre_event) {
      commands.push(PackageScriptCommand {
        event: pre_event,
        command: command.clone(),
        forwards_arguments: false,
      });
    }
    commands.push(PackageScriptCommand {
      event: name.to_string(),
      command: command.clone(),
      forwards_arguments: true,
    });
    let post_event = format!("post{name}");
    if let Some(command) = scripts.get(&post_event) {
      commands.push(PackageScriptCommand {
        event: post_event,
        command: command.clone(),
        forwards_arguments: false,
      });
    }
    Some(commands)
  }

  /// Merge Bun-style environment files beneath existing host variables.
  ///
  /// Explicit files are loaded in command-line order. Without explicit files,
  /// `.env`, the active `NODE_ENV` variant, and `.env.local` are loaded when
  /// automatic loading is enabled. Existing host variables always win.
  ///
  /// # Errors
  ///
  /// Returns an error when an explicit file is missing or any selected file is
  /// unreadable or malformed.
  pub fn load_environment(
    &self,
    host: &BTreeMap<String, String>,
    explicit_files: &[PathBuf],
    automatic: bool,
  ) -> Result<BTreeMap<String, String>, denko::errors::RuntimeError> {
    let mut loaded = BTreeMap::new();
    if explicit_files.is_empty() {
      if automatic {
        let mut candidates = vec![self.cwd.join(".env")];
        if let Some(mode) = host.get("NODE_ENV") {
          candidates.push(self.cwd.join(format!(".env.{mode}")));
        }
        candidates.push(self.cwd.join(".env.local"));
        for path in candidates
          .into_iter()
          .filter(|path| path.is_file())
        {
          merge_env_file(&path, &mut loaded)?;
        }
      }
    } else {
      for path in explicit_files {
        let path = if path.is_absolute() {
          path.clone()
        } else {
          self.cwd.join(path)
        };
        if !path.is_file() {
          return Err(denko::errors::RuntimeError::from(format!(
            "Environment file does not exist: {}",
            path.display()
          )));
        }
        merge_env_file(&path, &mut loaded)?;
      }
    }
    loaded.extend(
      host
        .iter()
        .map(|(key, value)| (key.clone(), value.clone())),
    );
    Ok(loaded)
  }
}

fn merge_env_file(
  path: &Path,
  environment: &mut BTreeMap<String, String>,
) -> Result<(), denko::errors::RuntimeError> {
  let source = std::fs::read_to_string(path).map_err(|error| {
    denko::errors::RuntimeError::from(format!("Failed to read {}: {error}", path.display()))
  })?;
  for (index, raw_line) in source.lines().enumerate() {
    let mut line = raw_line.trim();
    if line.is_empty() || line.starts_with('#') {
      continue;
    }
    if let Some(rest) = line.strip_prefix("export ") {
      line = rest.trim_start();
    }
    let Some((raw_key, raw_value)) = line.split_once('=') else {
      return Err(denko::errors::RuntimeError::from(format!(
        "Invalid environment entry at {}:{}",
        path.display(),
        index + 1
      )));
    };
    let key = raw_key.trim();
    if key.is_empty()
      || !key.chars().enumerate().all(|(index, character)| {
        character == '_'
          || character.is_ascii_alphanumeric() && (index > 0 || !character.is_ascii_digit())
      })
    {
      return Err(denko::errors::RuntimeError::from(format!(
        "Invalid environment name at {}:{}",
        path.display(),
        index + 1
      )));
    }
    let value = parse_env_value(raw_value.trim());
    environment.insert(key.to_string(), value);
  }
  Ok(())
}

fn parse_env_value(value: &str) -> String {
  if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
    return value[1..value.len() - 1]
      .replace("\\n", "\n")
      .replace("\\r", "\r")
      .replace("\\t", "\t")
      .replace("\\\"", "\"")
      .replace("\\\\", "\\");
  }
  if value.len() >= 2 && value.starts_with('\'') && value.ends_with('\'') {
    return value[1..value.len() - 1].to_string();
  }
  value
    .split_once(" #")
    .map_or(value, |(before_comment, _)| before_comment)
    .trim_end()
    .to_string()
}

#[cfg(test)]
mod tests {
  use super::*;

  fn temporary_directory(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
      "blitz-project-{label}-{}-{}",
      std::process::id(),
      std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos()
    ))
  }

  #[test]
  fn discovers_and_types_the_nearest_package_manifest() {
    let root = temporary_directory("manifest");
    let nested = root.join("src/nested");
    std::fs::create_dir_all(&nested).expect("create project");
    std::fs::write(
      root.join("package.json"),
      r#"{
        "name": "example",
        "type": "module",
        "scripts": { "start": "blitz src/main.ts" },
        "dependencies": { "effect": "3.16.7" }
      }"#,
    )
    .expect("write package manifest");

    let project = ProjectContext::discover(&nested).expect("discover project");
    let package = project.package.expect("package metadata");
    assert_eq!(project.root, std::fs::canonicalize(&root).expect("root"));
    assert_eq!(package.name.as_deref(), Some("example"));
    assert_eq!(package.package_type, Some(PackageType::Module));
    assert_eq!(
      package.scripts.get("start").map(String::as_str),
      Some("blitz src/main.ts")
    );
    std::fs::remove_dir_all(root).expect("remove project");
  }

  #[test]
  fn package_scripts_include_ordered_lifecycle_hooks() {
    let root = temporary_directory("scripts");
    std::fs::create_dir_all(&root).expect("create project");
    std::fs::write(
      root.join("package.json"),
      r#"{
        "scripts": {
          "prestart": "echo pre",
          "start": "blitz app.ts",
          "poststart": "echo post"
        }
      }"#,
    )
    .expect("write package manifest");
    let project = ProjectContext::discover(&root).expect("discover project");

    let commands = project
      .package_script("start")
      .expect("script plan");
    assert_eq!(
      commands
        .iter()
        .map(|command| command.event.as_str())
        .collect::<Vec<_>>(),
      ["prestart", "start", "poststart"]
    );
    assert!(!commands[0].forwards_arguments);
    assert!(commands[1].forwards_arguments);
    assert!(!commands[2].forwards_arguments);
    assert!(project.package_script("missing").is_none());
    std::fs::remove_dir_all(root).expect("remove project");
  }

  #[test]
  fn automatic_environment_uses_mode_and_local_precedence() {
    let root = temporary_directory("automatic-environment");
    std::fs::create_dir_all(&root).expect("create project");
    std::fs::write(root.join(".env"), "BASE=base\nORDER=base\n").expect("base env");
    std::fs::write(
      root.join(".env.development"),
      "MODE=development\nORDER=mode\n",
    )
    .expect("mode env");
    std::fs::write(root.join(".env.local"), "LOCAL=local\nORDER=local\n").expect("local env");
    let project = ProjectContext::discover(&root).expect("discover project");
    let host = BTreeMap::from([
      ("NODE_ENV".to_string(), "development".to_string()),
      ("ORDER".to_string(), "host".to_string()),
    ]);

    let environment = project
      .load_environment(&host, &[], true)
      .expect("load automatic environment");
    assert_eq!(environment.get("BASE").map(String::as_str), Some("base"));
    assert_eq!(
      environment.get("MODE").map(String::as_str),
      Some("development")
    );
    assert_eq!(environment.get("LOCAL").map(String::as_str), Some("local"));
    assert_eq!(environment.get("ORDER").map(String::as_str), Some("host"));

    let disabled = project
      .load_environment(&host, &[], false)
      .expect("disable automatic environment");
    assert!(!disabled.contains_key("BASE"));
    std::fs::remove_dir_all(root).expect("remove project");
  }

  #[test]
  fn environment_files_merge_in_order_beneath_host_values() {
    let root = temporary_directory("environment");
    std::fs::create_dir_all(&root).expect("create project");
    std::fs::write(root.join("one.env"), "FIRST=one\nSHARED=first\n").expect("first env");
    std::fs::write(root.join("two.env"), "SECOND='two'\nSHARED=second\n").expect("second env");
    let project = ProjectContext::discover(&root).expect("discover project");
    let host = BTreeMap::from([("SHARED".to_string(), "host".to_string())]);

    let environment = project
      .load_environment(
        &host,
        &[PathBuf::from("one.env"), PathBuf::from("two.env")],
        true,
      )
      .expect("load environment");
    assert_eq!(environment.get("FIRST").map(String::as_str), Some("one"));
    assert_eq!(environment.get("SECOND").map(String::as_str), Some("two"));
    assert_eq!(environment.get("SHARED").map(String::as_str), Some("host"));
    std::fs::remove_dir_all(root).expect("remove project");
  }
}
