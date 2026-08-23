// SPDX-License-Identifier: MPL-2.0

use std::fs;
use std::io::ErrorKind;
use std::path::PathBuf;

fn main() {
  let manifest_dir = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("manifest dir"));
  let source = manifest_dir.join("assets/runtime/v1/runtime.blitzbundle");
  let output = PathBuf::from(std::env::var_os("OUT_DIR").expect("build output dir"))
    .join("runtime.blitzbundle");

  println!("cargo::rerun-if-changed={}", source.display());
  let bytes = match fs::read(&source) {
    Ok(bytes) => bytes,
    Err(error) if error.kind() == ErrorKind::NotFound => Vec::new(),
    Err(error) => panic!("read {}: {error}", source.display()),
  };
  fs::write(&output, bytes).unwrap_or_else(|error| panic!("write {}: {error}", output.display()));
}
