//! Operating-system host APIs for the Blitz runtime.
//!
//! The ECMAScript engine is intentionally host-agnostic. This module is the
//! boundary where the Blitz executable installs capabilities supplied by the
//! operating system, such as process metadata, filesystem access, and async
//! I/O. Keeping that boundary explicit lets embedders select a smaller or
//! sandboxed host surface without changing VM semantics.

mod console;
mod process;

use std::collections::BTreeMap;
use std::env;

use blitz::Vm;

/// Host configuration installed into a Blitz VM.
#[derive(Clone, Debug)]
pub struct RuntimeOptions {
  /// Process arguments exposed to JavaScript.
  pub argv: Vec<String>,
  /// Environment variables exposed to JavaScript.
  pub env: BTreeMap<String, String>,
  /// Path of the running Blitz executable.
  pub exec_path: String,
}

impl RuntimeOptions {
  /// Capture host process state with the supplied JavaScript argument vector.
  pub fn from_host(argv: Vec<String>) -> Self {
    let env = env::vars_os()
      .map(|(key, value)| {
        (
          key.to_string_lossy().into_owned(),
          value.to_string_lossy().into_owned(),
        )
      })
      .collect();
    let exec_path = env::current_exe()
      .map(|path| path.to_string_lossy().into_owned())
      .unwrap_or_else(|_| argv.first().cloned().unwrap_or_default());

    Self {
      argv,
      env,
      exec_path,
    }
  }
}

/// Install the default Blitz runtime APIs into an initialized VM.
pub fn install_runtime(
  vm: &mut Vm,
  options: RuntimeOptions,
) {
  console::install(vm);
  process::install(vm, options);
}
