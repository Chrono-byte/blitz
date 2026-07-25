// SPDX-License-Identifier: MPL-2.0
//! Hosted filesystem-module execution.
//!
//! Package and tsconfig resolution remain VM-owned state in the engine's
//! module loader. This module is the runtime boundary that constructs and
//! evaluates an application entry, including reachable `node_modules` ESM and
//! TypeScript sources.

use blitz::Vm;
use std::path::Path;

/// Construct, instantiate, and evaluate a hosted application module graph.
///
/// Bare package imports use the nearest `node_modules` directory. The nearest
/// `tsconfig.json` is discovered for each importer, including inherited
/// `paths` mappings. Path targets are resolved relative to the config that
/// defines them, matching the TypeScript 7 model without requiring `baseUrl`.
///
/// # Errors
///
/// Returns a parser, compiler, resolver, linker, or runtime error from the
/// application graph.
pub fn evaluate_entry(
  vm: &mut Vm,
  path: &Path,
) -> Result<(), blitz::errors::Error> {
  let loader = vm.module_loader_handle();
  {
    let mut loader = loader.borrow_mut();
    loader.construct(
      path.parent().unwrap_or_else(|| Path::new(".")),
      &format!(
        "./{}",
        path
          .file_name()
          .ok_or_else(|| blitz::errors::RuntimeError::from("Module entry has no file name"))?
          .to_string_lossy()
      ),
    )?;
    loader.instantiate_all()?;
  }
  blitz::module::evaluate_entry_shared(&loader, path, vm, false)?;
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;
  use blitz::value::{ObjectData, Value};
  use std::cell::RefCell;
  use std::path::PathBuf;
  use std::rc::Rc;

  fn temporary_directory(label: &str) -> PathBuf {
    let directory = std::env::temp_dir().join(format!(
      "blitz-runtime-{label}-{}-{}",
      std::process::id(),
      std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock")
        .as_nanos()
    ));
    std::fs::create_dir_all(&directory).expect("create temporary module project");
    directory
  }

  #[test]
  fn evaluates_typescript_packages_and_nested_node_modules_dependencies() {
    let directory = temporary_directory("node-modules");
    let package = directory.join("node_modules/fixture-package");
    let dependency = directory.join("node_modules/fixture-dependency");
    std::fs::create_dir_all(&package).expect("create package");
    std::fs::create_dir_all(&dependency).expect("create dependency");
    std::fs::write(
      package.join("package.json"),
      r#"{ "type": "module", "exports": "./index.ts" }"#,
    )
    .expect("write package manifest");
    std::fs::write(
      package.join("index.ts"),
      "import { base } from 'fixture-dependency'; export const answer: number = base + 1;",
    )
    .expect("write package source");
    std::fs::write(
      dependency.join("package.json"),
      r#"{ "type": "module", "exports": "./index.js" }"#,
    )
    .expect("write dependency manifest");
    std::fs::write(dependency.join("index.js"), "export const base = 41;")
      .expect("write dependency source");
    let entry = directory.join("entry.ts");
    std::fs::write(
      &entry,
      "import { answer } from 'fixture-package'; globalThis.packageAnswer = answer;",
    )
    .expect("write entry");

    let globals = Rc::new(RefCell::new(ObjectData::new()));
    let mut vm = Vm::new(globals);
    evaluate_entry(&mut vm, &entry).expect("evaluate package graph");

    assert_eq!(vm.get_global("packageAnswer"), Some(Value::Number(42.0)));
    std::fs::remove_dir_all(directory).expect("remove temporary module project");
  }

  #[test]
  fn rejects_missing_package_dependencies_during_graph_construction() {
    let directory = temporary_directory("missing-package-dependency");
    let package = directory.join("node_modules/fixture-package");
    std::fs::create_dir_all(&package).expect("create package");
    std::fs::write(
      package.join("package.json"),
      r#"{ "type": "module", "exports": "./index.js" }"#,
    )
    .expect("write package manifest");
    std::fs::write(
      package.join("index.js"),
      "import 'missing-dependency'; export const loaded = true;",
    )
    .expect("write package source");
    let entry = directory.join("entry.ts");
    std::fs::write(&entry, "import 'fixture-package';").expect("write entry");

    let globals = Rc::new(RefCell::new(ObjectData::new()));
    let mut vm = Vm::new(globals);
    let error = evaluate_entry(&mut vm, &entry).expect_err("missing dependency must fail");

    assert!(error.to_string().contains("missing-dependency"), "{error}");
    std::fs::remove_dir_all(directory).expect("remove temporary module project");
  }

  #[test]
  fn evaluates_tsconfig_path_mappings() {
    let directory = temporary_directory("tsconfig-paths");
    std::fs::create_dir_all(directory.join("src/lib")).expect("create source directory");
    std::fs::write(
      directory.join("tsconfig.base.json"),
      r#"{
        "compilerOptions": {
          "paths": { "@lib/*": ["./src/lib/*"] }
        }
      }"#,
    )
    .expect("write base tsconfig");
    std::fs::write(
      directory.join("tsconfig.json"),
      r#"{ "extends": "./tsconfig.base.json" }"#,
    )
    .expect("write project tsconfig");
    std::fs::write(
      directory.join("src/lib/value.ts"),
      "export const mapped: number = 42;",
    )
    .expect("write mapped source");
    let entry = directory.join("src/entry.ts");
    std::fs::write(
      &entry,
      "import { mapped } from '@lib/value'; globalThis.mappedAnswer = mapped;",
    )
    .expect("write entry");

    let globals = Rc::new(RefCell::new(ObjectData::new()));
    let mut vm = Vm::new(globals);
    evaluate_entry(&mut vm, &entry).expect("evaluate mapped graph");

    assert_eq!(vm.get_global("mappedAnswer"), Some(Value::Number(42.0)));
    std::fs::remove_dir_all(directory).expect("remove temporary module project");
  }
}
