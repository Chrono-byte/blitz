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
  use blitz::value::{ObjectData, PropertyDescriptor, PropertyKey, Value};
  use std::cell::RefCell;
  use std::path::PathBuf;
  use std::rc::Rc;

  fn runtime_vm(profile: crate::host::NodeCompatProfile) -> (Vm, crate::host::RuntimeHandle) {
    let globals = Rc::new(RefCell::new(ObjectData::new()));
    globals.borrow_mut().properties.insert(
      PropertyKey::String("globalThis".to_string()),
      PropertyDescriptor::data(Value::Object(Rc::clone(&globals))),
    );
    let mut vm = Vm::new(globals);
    let mut options = crate::host::RuntimeOptions::from_host(vec!["blitz-test".to_string()]);
    options.node_compat = profile;
    let runtime = crate::host::install_runtime(&mut vm, options);
    (vm, runtime)
  }

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
  fn evaluates_the_core_node_process_module() {
    let directory = temporary_directory("node-process");
    let entry = directory.join("entry.ts");
    std::fs::write(
      &entry,
      "import process, { argv, cwd, platform } from 'node:process'; if (argv !== process.argv || cwd() !== process.cwd() || platform !== process.platform) throw new Error('node:process exports do not share process state');",
    )
    .expect("write node:process entry");

    let (mut vm, _runtime) = runtime_vm(crate::host::NodeCompatProfile::Core);
    let loader = vm.module_loader_handle();
    {
      let mut loader = loader.borrow_mut();
      loader
        .construct(&directory, "./entry.ts")
        .expect("construct node:process graph");
      loader
        .instantiate_all()
        .expect("instantiate node:process graph");
    }
    blitz::module::evaluate_entry_shared(&loader, &entry, &mut vm, true)
      .expect("typecheck and evaluate node:process entry");

    std::fs::remove_dir_all(directory).expect("remove node:process project");
  }

  #[test]
  fn rejects_node_modules_outside_the_selected_profile() {
    let directory = temporary_directory("node-process-off");
    let entry = directory.join("entry.ts");
    std::fs::write(&entry, "import 'node:process';").expect("write disabled node entry");

    let (mut vm, _runtime) = runtime_vm(crate::host::NodeCompatProfile::Off);
    let error = evaluate_entry(&mut vm, &entry).expect_err("disabled node module must fail");

    assert!(error.to_string().contains("node:process"), "{error}");
    std::fs::remove_dir_all(directory).expect("remove disabled node project");
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
  fn resolves_scoped_packages_with_node_export_conditions() {
    let directory = temporary_directory("scoped-node-condition");
    let package = directory.join("node_modules/@fixture/condition");
    std::fs::create_dir_all(&package).expect("create scoped package");
    std::fs::write(
      package.join("package.json"),
      r#"{
        "type": "module",
        "exports": {
          ".": {
            "node": "./node.js",
            "import": "./import.js",
            "default": "./default.js"
          }
        }
      }"#,
    )
    .expect("write scoped package manifest");
    std::fs::write(package.join("node.js"), "export const selected = 'node';")
      .expect("write node export");
    std::fs::write(
      package.join("import.js"),
      "export const selected = 'import';",
    )
    .expect("write import export");
    std::fs::write(
      package.join("default.js"),
      "export const selected = 'default';",
    )
    .expect("write default export");
    let entry = directory.join("entry.ts");
    std::fs::write(
      &entry,
      "import { selected } from '@fixture/condition'; globalThis.selectedCondition = selected;",
    )
    .expect("write scoped package entry");

    let globals = Rc::new(RefCell::new(ObjectData::new()));
    let mut vm = Vm::new(globals);
    evaluate_entry(&mut vm, &entry).expect("evaluate scoped package graph");

    assert_eq!(
      vm.get_global("selectedCondition"),
      Some(Value::String("node".to_string()))
    );
    std::fs::remove_dir_all(directory).expect("remove scoped package project");
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
