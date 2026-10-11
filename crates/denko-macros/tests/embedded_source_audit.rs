// SPDX-License-Identifier: MPL-2.0

use std::fs;
use std::path::{Path, PathBuf};

use syn::visit::{self, Visit};
use syn::{Attribute, ExprLit, Lit};
use walkdir::WalkDir;

#[derive(Default)]
struct UnwrappedSourceVisitor {
  sources: Vec<String>,
}

impl<'ast> Visit<'ast> for UnwrappedSourceVisitor {
  fn visit_attribute(
    &mut self,
    _attribute: &'ast Attribute,
  ) {
  }

  fn visit_expr_lit(
    &mut self,
    expression: &'ast ExprLit,
  ) {
    if let Lit::Str(literal) = &expression.lit {
      let value = literal.value();
      if looks_like_embedded_source(&value) {
        self.sources.push(value);
      }
    }
    visit::visit_expr_lit(self, expression);
  }
}

fn looks_like_embedded_source(value: &str) -> bool {
  let source = value.trim_start();
  if source.contains("[native code]") {
    return false;
  }
  let statement_like = source.contains(';') || source.contains('\n');

  source.starts_with("globalThis.")
    || source.starts_with("\"use strict\"")
    || source.starts_with("'use strict'")
    || ((source.starts_with("var ")
      || source.starts_with("let ")
      || source.starts_with("const ")
      || source.starts_with("declare ")
      || source.starts_with("export ")
      || source.starts_with("import ")
      || source.starts_with("interface ")
      || source.starts_with("type "))
      && statement_like)
    || (source.starts_with("function ") && source.contains('{') && source.contains('}'))
    || (source.starts_with("class ") && source.contains('{') && source.contains('}'))
}

fn rust_sources(root: &Path) -> impl Iterator<Item = PathBuf> + '_ {
  WalkDir::new(root.join("crates"))
    .into_iter()
    .filter_map(Result::ok)
    .filter(|entry| entry.file_type().is_file())
    .map(walkdir::DirEntry::into_path)
    .filter(|path| {
      path
        .extension()
        .is_some_and(|extension| extension == "rs")
    })
    .filter(|path| {
      !path
        .components()
        .any(|component| component.as_os_str() == "target")
    })
    .filter(|path| !path.starts_with(root.join("crates/denko-macros")))
}

#[test]
fn rust_source_literals_use_the_javascript_macro() {
  let root = Path::new(env!("CARGO_MANIFEST_DIR"))
    .parent()
    .and_then(Path::parent)
    .expect("macro crate is inside the workspace");
  let mut failures = Vec::new();

  for path in rust_sources(root) {
    let source = fs::read_to_string(&path).expect("read Rust source");
    let syntax = syn::parse_file(&source).expect("parse Rust source");
    let mut visitor = UnwrappedSourceVisitor::default();
    visitor.visit_file(&syntax);
    for embedded in visitor.sources {
      failures.push(format!(
        "{}: {}",
        path.strip_prefix(root).unwrap_or(&path).display(),
        embedded.escape_debug()
      ));
    }

    for marker in [
      "format!(\"'use strict",
      "format!(\"\\\"use strict",
      "format!(\"(function",
      "format!(\"import ",
      "format!(\"globalThis.",
    ] {
      if source.contains(marker) {
        failures.push(format!(
          "{}: unwrapped JavaScript format template containing {marker:?}",
          path.strip_prefix(root).unwrap_or(&path).display()
        ));
      }
    }
  }

  assert!(
    failures.is_empty(),
    "embedded JavaScript/TypeScript must use denko::js! or crate::js!:\n{}",
    failures.join("\n")
  );
}
