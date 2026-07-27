// SPDX-License-Identifier: MPL-2.0
//! Hosted filesystem-module execution.
//!
//! Package and tsconfig resolution remain VM-owned state in the engine's
//! module loader. This module is the runtime boundary that constructs and
//! evaluates an application entry, including reachable `node_modules` ESM and
//! TypeScript sources.

use denko::Vm;
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
) -> Result<(), denko::errors::Error> {
  let loader = vm.module_loader_handle();
  {
    let mut loader = loader.borrow_mut();
    loader.construct(
      path.parent().unwrap_or_else(|| Path::new(".")),
      &format!(
        "./{}",
        path
          .file_name()
          .ok_or_else(|| denko::errors::RuntimeError::from("Module entry has no file name"))?
          .to_string_lossy()
      ),
    )?;
    loader.instantiate_all()?;
  }
  denko::module::evaluate_entry_shared(&loader, path, vm, false)?;
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;
  use denko::value::{ObjectData, PropertyDescriptor, PropertyKey, Value};
  use std::cell::RefCell;
  use std::path::PathBuf;
  use std::rc::Rc;

  fn runtime_vm(profile: crate::host::NodeCompatProfile) -> (Vm, crate::host::RuntimeHandle) {
    let globals = Rc::new(RefCell::new(ObjectData::new()));
    globals.borrow_mut().properties.insert(
      PropertyKey::String("globalThis".to_string().into()),
      PropertyDescriptor::ordinary_data(Value::Object(Rc::clone(&globals))),
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
  fn node_profiles_only_allow_completed_roots() {
    assert_eq!(
      crate::host::NodeCompatProfile::Off.allowed_modules(),
      &[] as &[&str]
    );
    let core_modules = &[
      "node:assert",
      "node:assert/strict",
      "node:async_hooks",
      "node:buffer",
      "node:console",
      "node:constants",
      "node:diagnostics_channel",
      "node:events",
      "node:module",
      "node:os",
      "node:path",
      "node:path/posix",
      "node:path/win32",
      "node:perf_hooks",
      "node:process",
      "node:querystring",
      "node:stream",
      "node:stream/consumers",
      "node:stream/promises",
      "node:string_decoder",
      "node:timers",
      "node:timers/promises",
      "node:tty",
      "node:url",
      "node:util",
      "node:util/types",
    ];
    assert_eq!(
      crate::host::NodeCompatProfile::Core.allowed_modules(),
      core_modules
    );
    let tooling_modules = crate::host::NodeCompatProfile::Tooling.allowed_modules();
    assert_eq!(tooling_modules.len(), core_modules.len() + 5);
    for module in [
      "node:crypto",
      "node:fs",
      "node:fs/promises",
      "node:readline",
      "node:readline/promises",
    ] {
      assert!(tooling_modules.contains(&module));
    }
    let full_modules = crate::host::NodeCompatProfile::Full.allowed_modules();
    assert_eq!(full_modules.len(), core_modules.len() + 10);
    for module in [
      "node:crypto",
      "node:domain",
      "node:fs",
      "node:fs/promises",
      "node:punycode",
      "node:readline",
      "node:readline/promises",
      "node:sea",
      "node:sys",
      "node:trace_events",
    ] {
      assert!(full_modules.contains(&module));
    }
  }

  #[test]
  fn evaluates_the_core_node_process_module() {
    let directory = temporary_directory("node-process");
    let entry = directory.join("entry.ts");
    std::fs::write(
      &entry,
      denko::js!(
        module,
        "import process, { argv, cwd, platform } from 'node:process'; if (argv !== process.argv || cwd() !== process.cwd() || platform !== process.platform) throw new Error('node:process exports do not share process state');"
      ),
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
    denko::module::evaluate_entry_shared(&loader, &entry, &mut vm, true)
      .expect("typecheck and evaluate node:process entry");

    std::fs::remove_dir_all(directory).expect("remove node:process project");
  }

  #[test]
  fn evaluates_pure_node_core_modules_with_alias_identity() {
    let directory = temporary_directory("node-pure-core");
    let entry = directory.join("entry.ts");
    std::fs::write(
      &entry,
      denko::js!(module, r#"
        import assert from "node:assert";
        import strictAssert from "node:assert/strict";
        import asyncHooks from "node:async_hooks";
        import bufferModule, { Buffer as ModuleBuffer } from "node:buffer";
        import consoleModule, { log as consoleLog } from "node:console";
        import constants, { COPYFILE_EXCL } from "node:constants";
        import diagnostics, { channel } from "node:diagnostics_channel";
        import EventEmitter from "node:events";
        import moduleApi, { isBuiltin } from "node:module";
        import os, { arch, platform } from "node:os";
        import path, { posix, win32 } from "node:path";
        import posixPath from "node:path/posix";
        import win32Path from "node:path/win32";
        import perfHooks, { performance } from "node:perf_hooks";
        import querystring, { parse, stringify } from "node:querystring";
        import stream from "node:stream";
        import consumers, { text as consumeText } from "node:stream/consumers";
        import streamPromises, { pipeline as promisePipeline } from "node:stream/promises";
        import stringDecoder from "node:string_decoder";
        import timers, { setTimeout as scheduleTimeout } from "node:timers";
        import timersPromises, { setTimeout as promiseTimeout } from "node:timers/promises";
        import tty, { isatty } from "node:tty";
        import url, { fileURLToPath, pathToFileURL } from "node:url";
        import util, { format as utilFormat } from "node:util";
        import types from "node:util/types";

        if (strictAssert !== assert.strict) throw new Error("strict assertion alias mismatch");
        if (typeof asyncHooks !== "object") throw new Error("async hooks mismatch");
        if (bufferModule.Buffer !== ModuleBuffer) throw new Error("buffer identity mismatch");
        const bytes = ModuleBuffer.from("hé", "utf8");
        if (bytes.toString("hex") !== "68c3a9" || bytes.toString() !== "hé") throw new Error("buffer encoding mismatch");
        if (typeof consoleModule !== "object" || typeof consoleLog !== "function") throw new Error("console export mismatch");
        if (constants.COPYFILE_EXCL !== COPYFILE_EXCL) throw new Error("constants mismatch");
        if (typeof diagnostics !== "object" || typeof channel !== "function") throw new Error("diagnostics export mismatch");
        if (typeof EventEmitter !== "function") throw new Error("event emitter export mismatch");
        if (typeof moduleApi !== "function" || !isBuiltin("node:path")) throw new Error("module metadata mismatch");
        if (typeof os !== "object" || typeof platform() !== "string" || typeof arch() !== "string") throw new Error("os metadata mismatch");
        if (typeof perfHooks !== "object" || typeof performance.now() !== "number") throw new Error("performance mismatch");
        if (typeof stream !== "function" || typeof consumers !== "object" || typeof consumeText !== "function" || typeof streamPromises !== "object" || typeof promisePipeline !== "function") throw new Error("stream exports mismatch");
        if (typeof stringDecoder !== "object") throw new Error("string decoder mismatch");
        if (typeof timers !== "object" || typeof timersPromises !== "object" || typeof scheduleTimeout !== "function" || typeof promiseTimeout !== "function") throw new Error("timer exports mismatch");
        if (typeof tty !== "object" || isatty(0) !== false) throw new Error("tty mismatch");
        if (typeof url !== "object" || typeof fileURLToPath !== "function" || typeof pathToFileURL !== "function") throw new Error("url exports mismatch");
        if (utilFormat("%s:%d", "x", 2) !== "x:2" || util.types !== types) throw new Error("util mismatch");
        if (path.posix !== posixPath || path.win32 !== win32Path) throw new Error("path alias mismatch");
        if (posix.join("/tmp", "a", "..", "b") !== "/tmp/b") throw new Error("posix path mismatch");
        if (win32.normalize("C:\\temp\\..\\app\\") !== "C:\\app\\") throw new Error("win32 path mismatch");
        if (stringify({ a: ["1", "2"], empty: "" }) !== "a=1&a=2&empty=") throw new Error("query stringify mismatch");
        const parsed = parse("a=1&a=2");
        if (JSON.stringify(parsed) !== '{"a":["1","2"]}') throw new Error("query parse mismatch");
        if (querystring.decode !== parse) throw new Error("query alias mismatch");
      "#),
    )
    .expect("write pure node entry");

    let (mut vm, _runtime) = runtime_vm(crate::host::NodeCompatProfile::Core);
    let loader = vm.module_loader_handle();
    {
      let mut loader = loader.borrow_mut();
      loader
        .construct(&directory, "./entry.ts")
        .expect("construct pure node graph");
      loader
        .instantiate_all()
        .expect("instantiate pure node graph");
    }
    denko::module::evaluate_entry_shared(&loader, &entry, &mut vm, true)
      .expect("typecheck and evaluate pure node entry");
    std::fs::remove_dir_all(directory).expect("remove pure node project");
  }

  #[test]
  fn typechecks_tooling_node_module_declarations() {
    let directory = temporary_directory("node-tooling-types");
    let entry = directory.join("entry.ts");
    std::fs::write(
      &entry,
      denko::js!(module, r#"
        import crypto, { randomUUID } from "node:crypto";
        import fs, { readFileSync } from "node:fs";
        import fsPromises, { readFile } from "node:fs/promises";
        import readline, { createInterface } from "node:readline";
        import readlinePromises, { createInterface as createPromiseInterface } from "node:readline/promises";
        if (typeof crypto !== "object" || typeof randomUUID !== "function") throw new Error("crypto declarations");
        if (typeof fs !== "object" || typeof readFileSync !== "function") throw new Error("fs declarations");
        if (typeof fsPromises !== "object" || typeof readFile !== "function") throw new Error("fs promise declarations");
        if (typeof readline !== "object" || typeof createInterface !== "function") throw new Error("readline declarations");
        if (typeof readlinePromises !== "object" || typeof createPromiseInterface !== "function") throw new Error("readline promise declarations");
      "#),
    )
    .expect("write tooling type entry");

    let (mut vm, _runtime) = runtime_vm(crate::host::NodeCompatProfile::Tooling);
    let loader = vm.module_loader_handle();
    {
      let mut loader = loader.borrow_mut();
      loader
        .construct(&directory, "./entry.ts")
        .expect("construct tooling type graph");
      loader
        .instantiate_all()
        .expect("instantiate tooling type graph");
    }
    denko::module::evaluate_entry_shared(&loader, &entry, &mut vm, true)
      .expect("typecheck and evaluate tooling node declarations");

    std::fs::remove_dir_all(directory).expect("remove tooling type project");
  }

  #[test]
  fn evaluates_node_event_emitter_lifecycle() {
    let directory = temporary_directory("node-events");
    let entry = directory.join("entry.mjs");
    std::fs::write(
      &entry,
      denko::js!(module, r#"
        import EventEmitter, { getEventListeners, once } from "node:events";
        const emitter = new EventEmitter();
        let result = "";
        emitter.on("data", (value) => { result += value; });
        emitter.once("data", (value) => { result += value.toUpperCase(); });
        emitter.emit("data", "a");
        emitter.emit("data", "b");
        if (result !== "aAb" || getEventListeners(emitter, "data").length !== 1) throw new Error("event listener lifecycle mismatch");
        once(emitter, "done").then((args) => {
          globalThis.nodeEventsPassed = args[0] === 42;
        });
        emitter.emit("done", 42);
      "#),
    )
    .expect("write node events entry");

    let (mut vm, _runtime) = runtime_vm(crate::host::NodeCompatProfile::Core);
    evaluate_entry(&mut vm, &entry).expect("evaluate node events entry");
    assert_eq!(vm.get_global("nodeEventsPassed"), Some(Value::Bool(true)));

    std::fs::remove_dir_all(directory).expect("remove node events project");
  }

  #[test]
  fn evaluates_tooling_filesystem_sync_and_promises() {
    let directory = temporary_directory("node-fs");
    let entry = directory.join("entry.mjs");
    let data = directory.join("data.txt");
    let source = denko::js!(
      format(module),
      r#"
        import crypto, {{ randomUUID }} from "node:crypto";
        import fs from "node:fs";
        import fsPromises from "node:fs/promises";
        const uuid = randomUUID();
        if (crypto.randomUUID !== randomUUID || uuid.length !== 36 || uuid[14] !== "4") throw new Error("crypto UUID mismatch");
        const path = {path:?};
        fs.writeFileSync(path, "first", "utf8");
        if (fs.readFileSync(path, "utf8") !== "first") throw new Error("sync read mismatch");
        if (!fs.existsSync(path) || !fs.statSync(path).isFile()) throw new Error("sync metadata mismatch");
        fsPromises.writeFile(path, "second", "utf8").then(() => fsPromises.readFile(path, "utf8")).then((value) => {{
          globalThis.nodeFsPassed = value === "second";
        }});
      "#,
      path = data.to_string_lossy(),
    );
    std::fs::write(&entry, source).expect("write filesystem entry");

    let (mut vm, _runtime) = runtime_vm(crate::host::NodeCompatProfile::Tooling);
    evaluate_entry(&mut vm, &entry).expect("evaluate filesystem entry");
    assert_eq!(vm.get_global("nodeFsPassed"), Some(Value::Bool(true)));

    std::fs::remove_dir_all(directory).expect("remove filesystem project");
  }

  #[test]
  fn evaluates_tooling_readline_callbacks_and_promises() {
    let directory = temporary_directory("node-readline");
    let entry = directory.join("entry.mjs");
    std::fs::write(
      &entry,
      denko::js!(module, r#"
        import EventEmitter from "node:events";
        import readline from "node:readline";
        import readlinePromises from "node:readline/promises";
        const writes = [];
        const output = { write(value, callback) { writes.push(value); if (callback) callback(); return true; } };
        const input = new EventEmitter();
        const rl = readline.createInterface({ input, output });
        let callbackAnswer;
        rl.question("answer: ", (answer) => { callbackAnswer = answer; });
        input.emit("data", "yes\n");
        if (callbackAnswer !== "yes" || writes[0] !== "answer: ") throw new Error("readline callback mismatch");
        rl.close();
        const promiseInput = new EventEmitter();
        const promiseRl = readlinePromises.createInterface({ input: promiseInput, output });
        promiseRl.question("promise: ").then((answer) => { globalThis.nodeReadlinePassed = answer === "ok"; });
        promiseInput.emit("data", "ok\n");
      "#),
    )
    .expect("write readline entry");

    let (mut vm, _runtime) = runtime_vm(crate::host::NodeCompatProfile::Tooling);
    evaluate_entry(&mut vm, &entry).expect("evaluate readline entry");
    assert_eq!(vm.get_global("nodeReadlinePassed"), Some(Value::Bool(true)));

    std::fs::remove_dir_all(directory).expect("remove readline project");
  }

  #[test]
  fn evaluates_full_profile_compatibility_roots() {
    let directory = temporary_directory("node-full-compat");
    let entry = directory.join("entry.mjs");
    std::fs::write(
      &entry,
      denko::js!(module, r#"
        import domain from "node:domain";
        import punycode from "node:punycode";
        import sea from "node:sea";
        import sys from "node:sys";
        import traceEvents from "node:trace_events";
        import util from "node:util";
        if (sys !== util) throw new Error("sys alias mismatch");
        if (punycode.toASCII("mañana.com") !== "xn--maana-pta.com") throw new Error("punycode mismatch");
        if (sea.isSea() !== false) throw new Error("SEA state mismatch");
        const scope = domain.create();
        if (scope.run((value) => value + 1, 41) !== 42) throw new Error("domain scope mismatch");
        const tracing = traceEvents.createTracing({ categories: ["blitz"] });
        if (tracing.enabled !== false || traceEvents.getEnabledCategories() !== "") throw new Error("trace state mismatch");
        globalThis.nodeFullCompatPassed = true;
      "#),
    )
    .expect("write full compatibility entry");

    let (mut vm, _runtime) = runtime_vm(crate::host::NodeCompatProfile::Full);
    evaluate_entry(&mut vm, &entry).expect("evaluate full compatibility entry");
    assert_eq!(
      vm.get_global("nodeFullCompatPassed"),
      Some(Value::Bool(true))
    );

    std::fs::remove_dir_all(directory).expect("remove full compatibility project");
  }

  #[test]
  fn rejects_node_modules_outside_the_selected_profile() {
    let directory = temporary_directory("node-process-off");
    let entry = directory.join("entry.ts");
    std::fs::write(&entry, denko::js!(module, "import 'node:process';"))
      .expect("write disabled node entry");

    let (mut vm, _runtime) = runtime_vm(crate::host::NodeCompatProfile::Off);
    let error = evaluate_entry(&mut vm, &entry).expect_err("disabled node module must fail");

    assert!(error.to_string().contains("node:process"), "{error}");
    std::fs::remove_dir_all(directory).expect("remove disabled node project");
  }

  #[test]
  fn rejects_unknown_node_modules_with_the_requested_specifier() {
    let directory = temporary_directory("node-unknown");
    let entry = directory.join("entry.mjs");
    std::fs::write(&entry, denko::js!(module, "import 'node:future-module';"))
      .expect("write unknown node entry");

    for profile in [
      crate::host::NodeCompatProfile::Off,
      crate::host::NodeCompatProfile::Core,
      crate::host::NodeCompatProfile::Tooling,
      crate::host::NodeCompatProfile::Full,
    ] {
      let (mut vm, _runtime) = runtime_vm(profile);
      let error = evaluate_entry(&mut vm, &entry).expect_err("unknown node module must fail");
      let message = error.to_string();
      assert!(message.contains("node:future-module"), "{message}");
      assert!(
        message.contains("Cannot find module")
          || message.contains("Cannot resolve virtual dependency"),
        "{message}"
      );
    }

    std::fs::remove_dir_all(directory).expect("remove unknown node project");
  }

  #[test]
  fn dynamic_import_keeps_the_selected_node_profile_and_registry() {
    let directory = temporary_directory("node-process-dynamic");
    let entry = directory.join("entry.mjs");
    std::fs::write(
      &entry,
      "import('node:process').then(() => { globalThis.dynamicNodeIdentity = true; }, () => { globalThis.dynamicNodeIdentity = false; });",
    )
    .expect("write dynamic import entry");

    let (mut vm, _runtime) = runtime_vm(crate::host::NodeCompatProfile::Core);
    evaluate_entry(&mut vm, &entry).expect("evaluate dynamic node import");
    assert_eq!(
      vm.get_global("dynamicNodeIdentity"),
      Some(Value::Bool(true))
    );
    std::fs::remove_dir_all(&directory).expect("remove dynamic import project");

    let directory = temporary_directory("node-process-dynamic-off");
    let entry = directory.join("entry.mjs");
    std::fs::write(
      &entry,
      "import('node:process').then(() => { globalThis.dynamicNodeDisabled = false; }, () => { globalThis.dynamicNodeDisabled = true; });",
    )
    .expect("write disabled dynamic import entry");
    let (mut vm, _runtime) = runtime_vm(crate::host::NodeCompatProfile::Off);
    evaluate_entry(&mut vm, &entry).expect("evaluate disabled dynamic import rejection");
    assert_eq!(
      vm.get_global("dynamicNodeDisabled"),
      Some(Value::Bool(true))
    );
    std::fs::remove_dir_all(directory).expect("remove disabled dynamic import project");
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
      denko::js!(
        typescript,
        "import { base } from 'fixture-dependency'; export const answer: number = base + 1;"
      ),
    )
    .expect("write package source");
    std::fs::write(
      dependency.join("package.json"),
      r#"{ "type": "module", "exports": "./index.js" }"#,
    )
    .expect("write dependency manifest");
    std::fs::write(
      dependency.join("index.js"),
      denko::js!(module, "export const base = 41;"),
    )
    .expect("write dependency source");
    let entry = directory.join("entry.ts");
    std::fs::write(
      &entry,
      denko::js!(
        module,
        "import { answer } from 'fixture-package'; globalThis.packageAnswer = answer;"
      ),
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
    std::fs::write(
      package.join("node.js"),
      denko::js!(module, "export const selected = 'node';"),
    )
    .expect("write node export");
    std::fs::write(
      package.join("import.js"),
      denko::js!(module, "export const selected = 'import';"),
    )
    .expect("write import export");
    std::fs::write(
      package.join("default.js"),
      denko::js!(module, "export const selected = 'default';"),
    )
    .expect("write default export");
    let entry = directory.join("entry.ts");
    std::fs::write(
      &entry,
      denko::js!(
        module,
        "import { selected } from '@fixture/condition'; globalThis.selectedCondition = selected;"
      ),
    )
    .expect("write scoped package entry");

    let globals = Rc::new(RefCell::new(ObjectData::new()));
    let mut vm = Vm::new(globals);
    evaluate_entry(&mut vm, &entry).expect("evaluate scoped package graph");

    assert_eq!(
      vm.get_global("selectedCondition"),
      Some(Value::String("node".to_string().into()))
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
      denko::js!(
        module,
        "import 'missing-dependency'; export const loaded = true;"
      ),
    )
    .expect("write package source");
    let entry = directory.join("entry.ts");
    std::fs::write(&entry, denko::js!(module, "import 'fixture-package';")).expect("write entry");

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
      denko::js!(typescript, "export const mapped: number = 42;"),
    )
    .expect("write mapped source");
    let entry = directory.join("src/entry.ts");
    std::fs::write(
      &entry,
      denko::js!(
        module,
        "import { mapped } from '@lib/value'; globalThis.mappedAnswer = mapped;"
      ),
    )
    .expect("write entry");

    let globals = Rc::new(RefCell::new(ObjectData::new()));
    let mut vm = Vm::new(globals);
    evaluate_entry(&mut vm, &entry).expect("evaluate mapped graph");

    assert_eq!(vm.get_global("mappedAnswer"), Some(Value::Number(42.0)));
    std::fs::remove_dir_all(directory).expect("remove temporary module project");
  }
}
