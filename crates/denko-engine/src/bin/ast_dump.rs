// SPDX-License-Identifier: MPL-2.0
#![allow(missing_docs)]

use clap::Parser as ClapParser;
use denko::parser::Parser;
use std::fs;

#[derive(ClapParser)]
/// Parses a file and dumps the AST to stdout.
struct Args {
  /// The path to the file to parse.
  path: String,
}

struct Path {
  file: String,
  dir: String,
}

impl Path {
  fn new(path: &str) -> Self {
    let file = std::path::Path::new(path)
      .file_name()
      .and_then(|n| n.to_str())
      .unwrap_or("");
    let dir = std::path::Path::new(path)
      .parent()
      .and_then(|p| p.to_str())
      .unwrap_or("");
    Self {
      file: file.to_string(),
      dir: dir.to_string(),
    }
  }
}

fn main() {
  let args = Args::parse();
  let path = &args.path;
  let src = fs::read_to_string(path).expect("read test");
  let session = Parser::parse_source(&src).expect("parse ok");
  let program = session.program();
  println!("{:#?}", program);
  let p = Path::new(path);
  println!("file: {}", p.file);
  println!("dir: {}", p.dir);
}
