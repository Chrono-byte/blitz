// SPDX-License-Identifier: MPL-2.0

use std::path::PathBuf;
use std::process::Command;

fn temporary_project(label: &str) -> PathBuf {
  let directory = std::env::temp_dir().join(format!(
    "blitz-cli-application-{label}-{}-{}",
    std::process::id(),
    std::time::SystemTime::now()
      .duration_since(std::time::UNIX_EPOCH)
      .expect("clock")
      .as_nanos()
  ));
  std::fs::create_dir_all(&directory).expect("create project");
  directory
}

#[test]
fn runs_a_package_scoped_esm_project_like_a_node_entry() {
  let root = temporary_project("esm");
  std::fs::write(root.join("package.json"), r#"{"type":"module"}"#)
    .expect("write package manifest");
  std::fs::write(root.join("message.js"), "export const message = 'hello';")
    .expect("write dependency");
  std::fs::write(
    root.join("main.js"),
    "import { message } from './message.js'; console.log(message);",
  )
  .expect("write entry");

  let output = Command::new(env!("CARGO_BIN_EXE_blitz"))
    .current_dir(&root)
    .arg("main.js")
    .output()
    .expect("run blitz");

  assert!(
    output.status.success(),
    "{}",
    String::from_utf8_lossy(&output.stderr)
  );
  assert_eq!(String::from_utf8_lossy(&output.stdout), "hello\n");
  std::fs::remove_dir_all(root).expect("remove project");
}

#[test]
fn rejects_commonjs_without_parsing_it_as_an_esm_or_classic_script() {
  let root = temporary_project("commonjs");
  std::fs::write(root.join("main.cjs"), "module.exports = 42;").expect("write entry");

  let output = Command::new(env!("CARGO_BIN_EXE_blitz"))
    .current_dir(&root)
    .arg("main.cjs")
    .output()
    .expect("run blitz");
  let stderr = String::from_utf8_lossy(&output.stderr);

  assert!(!output.status.success());
  assert!(
    stderr.contains("BLITZ_MODULE_FORMAT_UNSUPPORTED"),
    "{stderr}"
  );
  assert!(stderr.contains("CommonJS"), "{stderr}");
  std::fs::remove_dir_all(root).expect("remove project");
}
