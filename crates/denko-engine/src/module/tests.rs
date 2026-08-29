use super::*;
use crate::prelinked::{
  PRELINKED_MODULE_GRAPH_VERSION, PrelinkedExport, PrelinkedImport, PrelinkedModule,
};
use crate::value::{ObjectData, Value};
use std::cell::RefCell;
use std::rc::Rc;

fn replace_debug_source(
  code: &mut Chunk,
  debug_source: &Arc<String>,
) {
  code.source_text = Arc::clone(debug_source);
  for function in &mut code.functions {
    replace_debug_source(Arc::make_mut(&mut function.chunk), debug_source);
  }
}

fn compile_prelinked_chunk(
  source: &str,
  module_index: usize,
  imports: &[(&str, usize, &str)],
  exports: &[&str],
) -> Chunk {
  let session = crate::parser::Parser::parse_source_with_type(source, SourceType::mjs())
    .expect("synthetic prelinked source parses during test setup");
  let context = ModuleContext {
    module_index,
    imports: imports
      .iter()
      .map(|(local, source_index, exported)| {
        (
          (*local).to_string(),
          (*source_index, (*exported).to_string()),
        )
      })
      .collect(),
    exports: exports
      .iter()
      .map(|name| (*name).to_string())
      .collect(),
    namespace_exports: HashMap::new(),
  };
  let mut code =
    crate::compiler::compile_oxc_with_ctx(session.program(), Some(&context), session.source())
      .expect("synthetic prelinked source compiles during test setup");
  replace_debug_source(
    &mut code,
    &Arc::new(
      crate::js!(
        parse_error(module),
        "!!! invalid debug source; parser/compiler must not read this !!!"
      )
      .to_string(),
    ),
  );
  code
}

fn compile_prelinked_namespace_reexport_chunk(
  source: &str,
  module_index: usize,
  target_index: usize,
  exported: &str,
) -> Chunk {
  let session = crate::parser::Parser::parse_source_with_type(source, SourceType::mjs())
    .expect("synthetic namespace re-export source parses during test setup");
  let context = ModuleContext {
    module_index,
    imports: HashMap::new(),
    exports: HashSet::from([exported.to_string()]),
    namespace_exports: HashMap::from([(exported.to_string(), target_index)]),
  };
  let mut code =
    crate::compiler::compile_oxc_with_ctx(session.program(), Some(&context), session.source())
      .expect("synthetic namespace re-export source compiles during test setup");
  replace_debug_source(
    &mut code,
    &Arc::new(
      crate::js!(
        parse_error(module),
        "!!! invalid debug source; parser/compiler must not read this !!!"
      )
      .to_string(),
    ),
  );
  code
}

fn synthetic_graph(
  modules: Vec<PrelinkedModule>,
  binding_count: u32,
  public_roots: Vec<u32>,
) -> PrelinkedModuleGraph {
  PrelinkedModuleGraph {
    format_version: PRELINKED_MODULE_GRAPH_VERSION,
    engine_bytecode_version: crate::bytecode::BytecodeVersion::RiscV2,
    package_name: "synthetic".to_string(),
    package_version: "1.0.0".to_string(),
    source_manifest_hash: [0xA5; 32],
    modules,
    binding_count,
    public_roots,
  }
}

fn export(
  exported_name: &str,
  binding_id: u32,
) -> PrelinkedExport {
  PrelinkedExport {
    exported_name: exported_name.to_string(),
    binding_id,
  }
}

fn import(
  local_name: &str,
  binding_id: u32,
) -> PrelinkedImport {
  PrelinkedImport {
    local_name: local_name.to_string(),
    binding_id,
  }
}

mod prelinked;
mod runtime;
mod virtual_modules;
