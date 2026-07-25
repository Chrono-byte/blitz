// SPDX-License-Identifier: MPL-2.0
//! Binary used to dump AST for debugging.
#![allow(missing_docs)]

use blitz::parser::Parser;
use clap::Parser as ClapParser;
use std::fs;

#[derive(ClapParser)]
/// Parses a file and dumps the AST to stdout.
struct Args {
  /// The path to the file to parse.
  path: String,
}

/// Represents a file path, with the file name and directory path separated.
struct Path {
  /// The file name.
  file: String,
  /// The directory path.
  dir: String,
}

impl Path {
  /// Create a new `Path` from the given path string.
  ///
  /// # Examples
  ///
  /// ```
  /// let path = Path::new("/path/to/file.txt");
  /// assert_eq!(path.file, "file.txt");
  /// assert_eq!(path.dir, "/path/to");
  /// ```
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
