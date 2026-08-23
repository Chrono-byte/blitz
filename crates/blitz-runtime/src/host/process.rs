// SPDX-License-Identifier: MPL-2.0
//! Node-compatible process metadata exposed by the Blitz host.

use std::cell::RefCell;
use std::rc::Rc;

use denko::Vm;
use denko::errors::RuntimeError;
use denko::gc;
use denko::value::{ObjectData, PropertyDescriptor, PropertyKey, Value};

use super::{RuntimeOptions, host_function};

pub(super) fn install(
  vm: &mut Vm,
  options: RuntimeOptions,
) {
  let object_prototype = vm.realm().object_prototype();
  let process = object(object_prototype.clone());
  let environment = object(object_prototype.clone());
  for (name, value) in options.env {
    set(&environment, &name, Value::String(value.into()));
  }

  let versions = object(object_prototype);
  set(
    &versions,
    "blitz",
    Value::String(env!("CARGO_PKG_VERSION").to_string().into()),
  );

  let argv = vm.new_array(
    options
      .argv
      .into_iter()
      .map(|argument| Some(Value::String(argument.into())))
      .collect(),
  );

  set(&process, "argv", argv);
  set(&process, "env", environment);
  set(
    &process,
    "execPath",
    Value::String(options.exec_path.into()),
  );
  set(
    &process,
    "platform",
    Value::String(platform().to_string().into()),
  );
  set(
    &process,
    "arch",
    Value::String(architecture().to_string().into()),
  );
  set(
    &process,
    "version",
    Value::String(format!("v{}", env!("CARGO_PKG_VERSION")).into()),
  );
  set(&process, "versions", versions);
  set(
    &process,
    "cwd",
    host_function(
      vm,
      "cwd",
      0,
      Rc::new(|_vm, _args| {
        std::env::current_dir()
          .map(|path| Value::String(path.to_string_lossy().into_owned().into()))
          .map_err(|error| RuntimeError::from(format!("Failed to get current directory: {error}")))
      }),
    ),
  );

  vm.realm().set_global("process", process);
  vm.realm()
    .set_global("global", Value::Object(vm.globals_rc()));
}

fn object(prototype: Value) -> Value {
  Value::Object(gc::track_object(Rc::new(RefCell::new(
    ObjectData::with_prototype(prototype),
  ))))
}

fn set(
  target: &Value,
  name: &str,
  value: Value,
) {
  let Value::Object(properties) = target else {
    return;
  };
  properties.borrow_mut().properties.insert(
    PropertyKey::String(name.to_string().into()),
    PropertyDescriptor::ordinary_data(value),
  );
}

const fn platform() -> &'static str {
  if cfg!(target_os = "windows") {
    "win32"
  } else if cfg!(target_os = "macos") {
    "darwin"
  } else if cfg!(target_os = "linux") {
    "linux"
  } else {
    std::env::consts::OS
  }
}

const fn architecture() -> &'static str {
  if cfg!(target_arch = "x86_64") {
    "x64"
  } else if cfg!(target_arch = "aarch64") {
    "arm64"
  } else if cfg!(target_arch = "x86") {
    "ia32"
  } else {
    std::env::consts::ARCH
  }
}

#[cfg(test)]
mod tests {
  use std::collections::BTreeMap;

  use super::*;
  use crate::host::NodeCompatProfile;
  use denko::compiler;
  use denko::parser::Parser as JsParser;

  #[test]
  fn installs_process_metadata_for_scripts() {
    let globals = Rc::new(RefCell::new(ObjectData::new()));
    globals.borrow_mut().properties.insert(
      PropertyKey::String("globalThis".to_string().into()),
      PropertyDescriptor::ordinary_data(Value::Object(Rc::clone(&globals))),
    );
    let mut vm = Vm::new(globals);
    install(
      &mut vm,
      RuntimeOptions {
        argv: vec![
          "blitz".to_string(),
          "app.js".to_string(),
          "hello".to_string(),
        ],
        env: BTreeMap::from([("BLITZ_HOST_TEST".to_string(), "works".to_string())]),
        exec_path: "/runtime/blitz".to_string(),
        listen_addr: "127.0.0.1:0"
          .parse()
          .expect("test listen address"),
        install_compatibility_globals: false,
        node_compat: NodeCompatProfile::Core,
        conditions: Vec::new(),
      },
    );

    let source = denko::js!(
      r#"
      globalThis.hostResult = process.argv[2] + ":" + process.env.BLITZ_HOST_TEST;
      globalThis.hostCwdIsString = typeof process.cwd() === "string";
      globalThis.hostVersion = process.versions.blitz;
      globalThis.hostAlias = global === globalThis;
    "#
    );
    let session = JsParser::parse_source(source).expect("parse host API probe");
    let chunk = compiler::compile_session(&session).expect("compile host API probe");
    vm.run(chunk).expect("run host API probe");

    assert_eq!(
      vm.get_global("hostResult"),
      Some(Value::String("hello:works".to_string().into()))
    );
    assert!(matches!(
      vm.get_global("hostCwdIsString"),
      Some(Value::Bool(true))
    ));
    assert_eq!(
      vm.get_global("hostVersion"),
      Some(Value::String(env!("CARGO_PKG_VERSION").to_string().into()))
    );
    assert!(matches!(
      vm.get_global("hostAlias"),
      Some(Value::Bool(true))
    ));
  }
}
