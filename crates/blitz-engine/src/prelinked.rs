//! Host-neutral metadata for parser-free, prelinked ECMAScript module graphs.
//!
//! The types in this module contain only portable graph metadata and sendable
//! bytecode chunks. Runtime live-binding cells are allocated by a
//! [`crate::module::ModuleLoader`] only after a validated graph is constructed
//! in a realm.

use crate::bytecode::{BytecodeVersion, Chunk, Constant, Op, decode_risc_aux16};
use std::collections::{HashMap, HashSet};
use std::mem::size_of;
use thiserror::Error;

/// Current in-memory prelinked module graph format.
pub const PRELINKED_MODULE_GRAPH_VERSION: u16 = 1;

/// Portable numeric identity of a module in a prelinked graph.
pub type ModuleIndex = u32;

/// Portable numeric identity of one canonical live module binding.
pub type BindingId = u32;

const MAX_MODULES: usize = 65_536;
const MAX_BINDINGS: usize = 1_000_000;
const MAX_DEPENDENCY_EDGES: usize = 4_000_000;
const MAX_BINDING_ENTRIES: usize = 8_000_000;
const MAX_PUBLIC_ROOTS: usize = 65_536;
const MAX_NAME_BYTES: usize = 64 * 1024;
const MAX_TOTAL_NAME_BYTES: usize = 64 * 1024 * 1024;
const MAX_TOTAL_SOURCE_BYTES: usize = 64 * 1024 * 1024;
const MAX_TOTAL_BYTECODE_BYTES: usize = 256 * 1024 * 1024;
const MAX_CHUNKS: usize = 1_000_000;
const MAX_CHUNK_DEPTH: usize = 256;

/// A complete parser-free module graph and its canonical binding metadata.
#[derive(Clone, Debug)]
pub struct PrelinkedModuleGraph {
  /// Version of the graph metadata contract.
  pub format_version: u16,
  /// Bytecode version expected for every root and nested chunk.
  pub engine_bytecode_version: BytecodeVersion,
  /// Host-defined package or bundle identity.
  pub package_name: String,
  /// Host-defined package or bundle version.
  pub package_version: String,
  /// Hash of the host's source manifest. The engine preserves this value;
  /// hosts are responsible for comparing it with their pinned manifest.
  pub source_manifest_hash: [u8; 32],
  /// Modules in deterministic artifact order.
  pub modules: Vec<PrelinkedModule>,
  /// Number of canonical live-binding cells required by this graph.
  pub binding_count: u32,
  /// Public entry modules exposed by the bundle.
  pub public_roots: Vec<ModuleIndex>,
}

impl PrelinkedModuleGraph {
  /// Validate all graph, binding, chunk, and bytecode-descriptor invariants.
  ///
  /// Validation is host-neutral: package versions and source hashes are
  /// retained but must be checked against host policy by the embedder.
  ///
  /// # Errors
  ///
  /// Returns [`PrelinkedGraphError`] before any runtime cells are allocated or
  /// module bytecode is executed.
  pub fn validate(&self) -> Result<(), PrelinkedGraphError> {
    if self.format_version != PRELINKED_MODULE_GRAPH_VERSION {
      return Err(PrelinkedGraphError::UnsupportedFormatVersion(
        self.format_version,
      ));
    }
    if self.engine_bytecode_version != BytecodeVersion::RiscV2 {
      return Err(PrelinkedGraphError::UnsupportedBytecodeVersion(
        self.engine_bytecode_version,
      ));
    }
    validate_required_name(&self.package_name, "package name")?;
    validate_required_name(&self.package_version, "package version")?;
    enforce_limit("module count", self.modules.len(), MAX_MODULES)?;
    if self.modules.is_empty() {
      return Err(invalid(
        "a prelinked graph must contain at least one module",
      ));
    }
    let binding_count = usize::try_from(self.binding_count)
      .map_err(|_| invalid("binding count does not fit usize"))?;
    enforce_limit("binding count", binding_count, MAX_BINDINGS)?;
    enforce_limit(
      "public root count",
      self.public_roots.len(),
      MAX_PUBLIC_ROOTS,
    )?;
    if self.public_roots.is_empty() {
      return Err(invalid(
        "a prelinked graph must declare at least one public root",
      ));
    }

    let module_count = u32::try_from(self.modules.len())
      .map_err(|_| invalid("module count does not fit the portable index representation"))?;
    let mut specifiers = HashSet::with_capacity(self.modules.len());
    let mut module_indices = HashSet::with_capacity(self.modules.len());
    let mut exports_by_module = HashMap::with_capacity(self.modules.len());
    let mut imports_by_module = HashMap::with_capacity(self.modules.len());
    let mut dependencies_by_module = HashMap::with_capacity(self.modules.len());
    let mut exported_bindings = HashSet::new();
    let mut total_names = self
      .package_name
      .len()
      .checked_add(self.package_version.len())
      .ok_or_else(|| invalid("name byte count overflow"))?;
    let mut total_dependencies = 0_usize;
    let mut total_binding_entries = 0_usize;

    for module in &self.modules {
      validate_required_name(&module.specifier, "module specifier")?;
      total_names = checked_add(total_names, module.specifier.len(), "name byte count")?;
      if !specifiers.insert(module.specifier.as_str()) {
        return Err(invalid(format!(
          "duplicate module specifier {:?}",
          module.specifier
        )));
      }
      if !module_indices.insert(module.module_index) {
        return Err(invalid(format!(
          "duplicate module index {}",
          module.module_index
        )));
      }
      if module.module_index >= module_count {
        return Err(invalid(format!(
          "module index {} is outside the contiguous range 0..{}",
          module.module_index, module_count
        )));
      }

      total_dependencies = checked_add(
        total_dependencies,
        module.dependencies.len(),
        "dependency edge count",
      )?;
      dependencies_by_module.insert(
        module.module_index,
        module
          .dependencies
          .iter()
          .copied()
          .collect::<HashSet<_>>(),
      );
      total_binding_entries = checked_add(
        total_binding_entries,
        module.exports.len(),
        "binding entry count",
      )?;
      total_binding_entries = checked_add(
        total_binding_entries,
        module.imports.len(),
        "binding entry count",
      )?;

      let mut export_names = HashMap::with_capacity(module.exports.len());
      for export in &module.exports {
        validate_name(&export.exported_name, "export name")?;
        if export.exported_name == "*" {
          return Err(invalid(format!(
            "module {:?} uses the reserved synthetic namespace export name",
            module.specifier
          )));
        }
        total_names = checked_add(total_names, export.exported_name.len(), "name byte count")?;
        if export_names
          .insert(export.exported_name.clone(), export.binding_id)
          .is_some()
        {
          return Err(invalid(format!(
            "module {:?} contains duplicate export name {:?}",
            module.specifier, export.exported_name
          )));
        }
        validate_binding_id(export.binding_id, self.binding_count, &module.specifier)?;
        exported_bindings.insert(export.binding_id);
      }
      exports_by_module.insert(module.module_index, export_names);

      let mut import_names = HashSet::with_capacity(module.imports.len());
      let mut import_bindings = HashSet::with_capacity(module.imports.len());
      for import in &module.imports {
        validate_required_name(&import.local_name, "import local name")?;
        total_names = checked_add(total_names, import.local_name.len(), "name byte count")?;
        if !import_names.insert(import.local_name.as_str()) {
          return Err(invalid(format!(
            "module {:?} contains duplicate import local name {:?}",
            module.specifier, import.local_name
          )));
        }
        validate_binding_id(import.binding_id, self.binding_count, &module.specifier)?;
        import_bindings.insert(import.binding_id);
      }
      imports_by_module.insert(module.module_index, import_bindings);
    }

    // Unique in-range indices with exactly `module_count` entries prove that
    // the initial format's reserved range is contiguous and begins at zero.
    for index in 0..module_count {
      if !module_indices.contains(&index) {
        return Err(invalid(format!(
          "prelinked module index range is not contiguous; missing index {index}"
        )));
      }
    }

    enforce_limit(
      "dependency edge count",
      total_dependencies,
      MAX_DEPENDENCY_EDGES,
    )?;
    enforce_limit(
      "binding entry count",
      total_binding_entries,
      MAX_BINDING_ENTRIES,
    )?;
    enforce_limit("total name bytes", total_names, MAX_TOTAL_NAME_BYTES)?;

    if exported_bindings.len() != binding_count {
      return Err(invalid(format!(
        "binding IDs must be contiguous and exported at least once; expected {} IDs, found {}",
        self.binding_count,
        exported_bindings.len()
      )));
    }
    for binding_id in 0..self.binding_count {
      if !exported_bindings.contains(&binding_id) {
        return Err(invalid(format!(
          "canonical binding ID range is not contiguous; missing binding {binding_id}"
        )));
      }
    }

    for module in &self.modules {
      for dependency in &module.dependencies {
        if !module_indices.contains(dependency) {
          return Err(invalid(format!(
            "module {:?} dependency index {} is not present in the graph",
            module.specifier, dependency
          )));
        }
      }
      for import in &module.imports {
        if !exported_bindings.contains(&import.binding_id) {
          return Err(invalid(format!(
            "module {:?} import {:?} references absent binding {}",
            module.specifier, import.local_name, import.binding_id
          )));
        }
        let supplied_by_dependency = module.dependencies.iter().any(|dependency| {
          exports_by_module
            .get(dependency)
            .is_some_and(|exports| {
              exports
                .values()
                .any(|binding| *binding == import.binding_id)
            })
        });
        if !supplied_by_dependency {
          return Err(invalid(format!(
            "module {:?} import {:?} binding {} is not exported by a declared dependency",
            module.specifier, import.local_name, import.binding_id
          )));
        }
      }
    }

    let mut roots = HashSet::with_capacity(self.public_roots.len());
    for root in &self.public_roots {
      if !module_indices.contains(root) {
        return Err(invalid(format!(
          "public root module index {root} is not present in the graph"
        )));
      }
      if !roots.insert(*root) {
        return Err(invalid(format!(
          "duplicate public root module index {root}"
        )));
      }
    }

    let mut budget = ChunkBudget::default();
    for module in &self.modules {
      validate_chunk_tree(
        &module.chunk,
        module.module_index,
        &module.specifier,
        self.engine_bytecode_version,
        &exports_by_module,
        &imports_by_module,
        &dependencies_by_module,
        0,
        &mut budget,
      )?;
      module
        .chunk
        .verify()
        .map_err(|reason| PrelinkedGraphError::InvalidChunk {
          specifier: module.specifier.clone(),
          reason,
        })?;
    }
    enforce_limit("chunk count", budget.chunks, MAX_CHUNKS)?;
    enforce_limit(
      "total chunk source bytes",
      budget.source_bytes,
      MAX_TOTAL_SOURCE_BYTES,
    )?;
    enforce_limit(
      "total bytecode bytes",
      budget.bytecode_bytes,
      MAX_TOTAL_BYTECODE_BYTES,
    )?;

    Ok(())
  }

  /// Return a module by its portable numeric index.
  #[must_use]
  pub fn module(
    &self,
    module_index: ModuleIndex,
  ) -> Option<&PrelinkedModule> {
    self
      .modules
      .iter()
      .find(|module| module.module_index == module_index)
  }
}

/// Metadata and executable bytecode for one prelinked module.
#[derive(Clone, Debug)]
pub struct PrelinkedModule {
  /// Canonical import specifier and module identity.
  pub specifier: String,
  /// Stable VM module-table index embedded in bytecode descriptors.
  pub module_index: ModuleIndex,
  /// Hash of the original source used by host-side manifest validation.
  pub source_sha256: [u8; 32],
  /// Dependency module indices in source module-request order.
  pub dependencies: Vec<ModuleIndex>,
  /// Export names mapped to canonical live-binding IDs.
  pub exports: Vec<PrelinkedExport>,
  /// Local imported names mapped to canonical live-binding IDs.
  pub imports: Vec<PrelinkedImport>,
  /// Verified bytecode executed without parsing or compiling source text.
  pub chunk: Chunk,
}

/// One exported name's canonical live-binding identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PrelinkedExport {
  /// Name visible to module importers.
  pub exported_name: String,
  /// Canonical graph-wide live-binding ID.
  pub binding_id: BindingId,
}

/// One imported local name's canonical live-binding identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PrelinkedImport {
  /// Local binding name used by the precompiled module.
  pub local_name: String,
  /// Canonical graph-wide live-binding ID supplied by an exporter.
  pub binding_id: BindingId,
}

/// Errors produced by host-neutral prelinked graph validation.
#[derive(Debug, Error)]
pub enum PrelinkedGraphError {
  /// The graph metadata contract is not supported by this engine.
  #[error("unsupported prelinked module graph version {0}")]
  UnsupportedFormatVersion(u16),
  /// The graph requests an unsupported bytecode contract.
  #[error("unsupported prelinked graph bytecode version {0:?}")]
  UnsupportedBytecodeVersion(BytecodeVersion),
  /// A defensive graph or bytecode limit was exceeded.
  #[error("prelinked graph {kind} is {actual}; maximum supported value is {maximum}")]
  LimitExceeded {
    /// Limited resource.
    kind: &'static str,
    /// Observed amount.
    actual: usize,
    /// Maximum accepted amount.
    maximum: usize,
  },
  /// Portable graph metadata is internally inconsistent.
  #[error("invalid prelinked module graph: {0}")]
  InvalidData(String),
  /// A module chunk failed structural bytecode verification.
  #[error("invalid prelinked bytecode chunk for {specifier:?}: {reason}")]
  InvalidChunk {
    /// Module owning the chunk.
    specifier: String,
    /// Structural verification failure.
    reason: String,
  },
  /// A module instruction contains an invalid module-export descriptor.
  #[error("invalid module descriptor in {specifier:?}: {reason}")]
  InvalidDescriptor {
    /// Module owning the instruction.
    specifier: String,
    /// Descriptor validation failure.
    reason: String,
  },
}

#[derive(Default)]
struct ChunkBudget {
  chunks: usize,
  source_bytes: usize,
  bytecode_bytes: usize,
}

#[allow(clippy::too_many_arguments)]
fn validate_chunk_tree(
  chunk: &Chunk,
  owner_index: ModuleIndex,
  specifier: &str,
  expected_version: BytecodeVersion,
  exports_by_module: &HashMap<ModuleIndex, HashMap<String, BindingId>>,
  imports_by_module: &HashMap<ModuleIndex, HashSet<BindingId>>,
  dependencies_by_module: &HashMap<ModuleIndex, HashSet<ModuleIndex>>,
  depth: usize,
  budget: &mut ChunkBudget,
) -> Result<(), PrelinkedGraphError> {
  if depth > MAX_CHUNK_DEPTH {
    return Err(invalid(format!(
      "chunk nesting for {specifier:?} exceeds {MAX_CHUNK_DEPTH} levels"
    )));
  }
  if chunk.version != expected_version {
    return Err(PrelinkedGraphError::InvalidChunk {
      specifier: specifier.to_string(),
      reason: format!(
        "chunk bytecode version {:?} does not match graph version {:?}",
        chunk.version, expected_version
      ),
    });
  }

  budget.chunks = checked_add(budget.chunks, 1, "chunk count")?;
  budget.source_bytes = checked_add(
    budget.source_bytes,
    chunk.source_text.len(),
    "chunk source byte count",
  )?;
  enforce_limit("chunk count", budget.chunks, MAX_CHUNKS)?;
  enforce_limit(
    "total chunk source bytes",
    budget.source_bytes,
    MAX_TOTAL_SOURCE_BYTES,
  )?;
  let instruction_bytes = chunk
    .code
    .len()
    .checked_mul(size_of::<u32>())
    .ok_or_else(|| invalid("bytecode size overflow"))?;
  let line_bytes = chunk
    .lines
    .len()
    .checked_mul(size_of::<u32>())
    .ok_or_else(|| invalid("line table size overflow"))?;
  budget.bytecode_bytes = checked_add(budget.bytecode_bytes, instruction_bytes, "bytecode size")?;
  budget.bytecode_bytes = checked_add(budget.bytecode_bytes, line_bytes, "bytecode size")?;
  budget.bytecode_bytes = checked_add(
    budget.bytecode_bytes,
    chunk.source_text.len(),
    "bytecode size",
  )?;
  for constant in &chunk.constants {
    let constant_bytes = match constant {
      Constant::String(value) => value.len(),
      Constant::BigInt(value) => value.to_signed_bytes_le().len(),
      Constant::Symbol(_, description) => description.as_ref().map_or(0, String::len),
      Constant::Uninitialized
      | Constant::Number(_)
      | Constant::Bool(_)
      | Constant::Undefined
      | Constant::Null => size_of::<Constant>(),
    };
    budget.bytecode_bytes = checked_add(budget.bytecode_bytes, constant_bytes, "bytecode size")?;
    enforce_limit(
      "total bytecode bytes",
      budget.bytecode_bytes,
      MAX_TOTAL_BYTECODE_BYTES,
    )?;
  }
  enforce_limit(
    "total bytecode bytes",
    budget.bytecode_bytes,
    MAX_TOTAL_BYTECODE_BYTES,
  )?;

  for (instruction_index, word) in chunk.code.iter().copied().enumerate() {
    let instruction = chunk
      .decode_at(instruction_index)
      .map_err(|reason| PrelinkedGraphError::InvalidChunk {
        specifier: specifier.to_string(),
        reason,
      })?;
    let constant_index = usize::from(decode_risc_aux16(word));
    if instruction.op == Op::GetBuiltin
      && matches!(
        chunk.constants.get(constant_index),
        Some(Constant::String(name)) if name == "dynamicImport"
      )
    {
      return Err(invalid(format!(
        "module {specifier:?} contains unsupported dynamic import bytecode at instruction {instruction_index}"
      )));
    }
    if !matches!(instruction.op, Op::GetModuleExport | Op::SetModuleExport) {
      continue;
    }
    let descriptor = match chunk.constants.get(constant_index) {
      Some(Constant::String(descriptor)) => descriptor,
      Some(_) => {
        return Err(invalid_descriptor(
          specifier,
          format!("instruction {instruction_index} references a non-string descriptor constant"),
        ));
      }
      None => {
        return Err(invalid_descriptor(
          specifier,
          format!(
            "instruction {instruction_index} descriptor constant {constant_index} is out of range"
          ),
        ));
      }
    };
    let (target_index, target_name) = parse_descriptor(descriptor).map_err(|reason| {
      invalid_descriptor(
        specifier,
        format!("instruction {instruction_index}: {reason}"),
      )
    })?;
    let Some(target_exports) = exports_by_module.get(&target_index) else {
      return Err(invalid_descriptor(
        specifier,
        format!("instruction {instruction_index} targets absent module index {target_index}"),
      ));
    };
    if instruction.op == Op::GetModuleExport
      && target_index != owner_index
      && !dependencies_by_module
        .get(&owner_index)
        .is_some_and(|dependencies| dependencies.contains(&target_index))
    {
      return Err(invalid_descriptor(
        specifier,
        format!(
          "instruction {instruction_index} targets module {target_index}, which is absent from module {owner_index} dependency edges"
        ),
      ));
    }
    if instruction.op == Op::SetModuleExport && target_index != owner_index {
      return Err(invalid_descriptor(
        specifier,
        format!(
          "instruction {instruction_index} writes module {target_index}, but its owner is module {owner_index}"
        ),
      ));
    }
    if instruction.op == Op::SetModuleExport && target_name == "*" {
      return Err(invalid_descriptor(
        specifier,
        format!("instruction {instruction_index} writes the synthetic namespace export"),
      ));
    }
    // The compiler stores top-level module-private bindings in the owner
    // module table so nested functions can access them. They are intentionally
    // absent from the public export map and namespace object. Cross-module
    // descriptors must still target declared exports.
    let target_binding = if target_name == "*"
      || (target_index == owner_index && !target_exports.contains_key(target_name))
    {
      None
    } else {
      Some(*target_exports.get(target_name).ok_or_else(|| {
        invalid_descriptor(
          specifier,
          format!(
            "instruction {instruction_index} targets absent export {target_name:?} in module {target_index}"
          ),
        )
      })?)
    };
    if instruction.op == Op::GetModuleExport
      && let Some(target_binding) = target_binding
      && !imports_by_module
        .get(&owner_index)
        .is_some_and(|imports| imports.contains(&target_binding))
    {
      return Err(invalid_descriptor(
        specifier,
        format!(
          "instruction {instruction_index} reads canonical binding {target_binding}, which is absent from module {owner_index} imports"
        ),
      ));
    }
  }

  for function in &chunk.functions {
    validate_chunk_tree(
      &function.chunk,
      owner_index,
      specifier,
      expected_version,
      exports_by_module,
      imports_by_module,
      dependencies_by_module,
      depth + 1,
      budget,
    )?;
  }
  Ok(())
}

fn parse_descriptor(descriptor: &str) -> Result<(ModuleIndex, &str), String> {
  let (module, name) = descriptor
    .split_once('\0')
    .ok_or_else(|| "module descriptor has no NUL separator".to_string())?;
  let module_index = module
    .parse::<ModuleIndex>()
    .map_err(|_| format!("invalid module index {module:?}"))?;
  if module_index.to_string() != module {
    return Err(format!(
      "module index {module:?} is not canonically encoded"
    ));
  }
  Ok((module_index, name))
}

fn validate_binding_id(
  binding_id: BindingId,
  binding_count: u32,
  specifier: &str,
) -> Result<(), PrelinkedGraphError> {
  if binding_id >= binding_count {
    Err(invalid(format!(
      "module {specifier:?} binding ID {binding_id} is outside 0..{binding_count}"
    )))
  } else {
    Ok(())
  }
}

fn validate_required_name(
  value: &str,
  kind: &'static str,
) -> Result<(), PrelinkedGraphError> {
  if value.is_empty() {
    return Err(invalid(format!("{kind} must not be empty")));
  }
  validate_name(value, kind)
}

fn validate_name(
  value: &str,
  kind: &'static str,
) -> Result<(), PrelinkedGraphError> {
  enforce_limit(kind, value.len(), MAX_NAME_BYTES)
}

fn enforce_limit(
  kind: &'static str,
  actual: usize,
  maximum: usize,
) -> Result<(), PrelinkedGraphError> {
  if actual > maximum {
    Err(PrelinkedGraphError::LimitExceeded {
      kind,
      actual,
      maximum,
    })
  } else {
    Ok(())
  }
}

fn checked_add(
  left: usize,
  right: usize,
  kind: &'static str,
) -> Result<usize, PrelinkedGraphError> {
  left
    .checked_add(right)
    .ok_or_else(|| invalid(format!("{kind} overflow")))
}

fn invalid(message: impl Into<String>) -> PrelinkedGraphError {
  PrelinkedGraphError::InvalidData(message.into())
}

fn invalid_descriptor(
  specifier: &str,
  reason: impl Into<String>,
) -> PrelinkedGraphError {
  PrelinkedGraphError::InvalidDescriptor {
    specifier: specifier.to_string(),
    reason: reason.into(),
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::bytecode::{Op, encode_risc_aux16};
  use std::sync::Arc;

  fn return_chunk(debug_source: &str) -> Chunk {
    let mut chunk = Chunk::new(Arc::new(debug_source.to_string()));
    chunk.write_instr(Op::Undefined as u32, 1);
    chunk.write_instr(Op::Return as u32, 1);
    chunk
  }

  fn graph() -> PrelinkedModuleGraph {
    PrelinkedModuleGraph {
      format_version: PRELINKED_MODULE_GRAPH_VERSION,
      engine_bytecode_version: BytecodeVersion::RiscV2,
      package_name: "synthetic".to_string(),
      package_version: "1.0.0".to_string(),
      source_manifest_hash: [7; 32],
      modules: vec![
        PrelinkedModule {
          specifier: "synthetic:value".to_string(),
          module_index: 0,
          source_sha256: [1; 32],
          dependencies: vec![],
          exports: vec![PrelinkedExport {
            exported_name: "value".to_string(),
            binding_id: 0,
          }],
          imports: vec![],
          chunk: return_chunk("debug value source"),
        },
        PrelinkedModule {
          specifier: "synthetic:entry".to_string(),
          module_index: 1,
          source_sha256: [2; 32],
          dependencies: vec![0],
          exports: vec![PrelinkedExport {
            exported_name: "alias".to_string(),
            binding_id: 0,
          }],
          imports: vec![PrelinkedImport {
            local_name: "value".to_string(),
            binding_id: 0,
          }],
          chunk: return_chunk("debug entry source"),
        },
      ],
      binding_count: 1,
      public_roots: vec![1],
    }
  }

  #[test]
  fn validates_complete_synthetic_graph() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<PrelinkedModuleGraph>();
    graph().validate().expect("graph validates");
  }

  #[test]
  fn rejects_duplicate_and_out_of_range_metadata() {
    let mut duplicate_specifier = graph();
    duplicate_specifier.modules[1].specifier = duplicate_specifier.modules[0].specifier.clone();
    assert!(
      duplicate_specifier
        .validate()
        .expect_err("duplicate specifier rejected")
        .to_string()
        .contains("duplicate module specifier")
    );

    let mut duplicate_index = graph();
    duplicate_index.modules[1].module_index = 0;
    assert!(
      duplicate_index
        .validate()
        .expect_err("duplicate index rejected")
        .to_string()
        .contains("duplicate module index")
    );

    let mut missing_dependency = graph();
    missing_dependency.modules[1].dependencies.push(9);
    assert!(
      missing_dependency
        .validate()
        .expect_err("missing dependency rejected")
        .to_string()
        .contains("dependency index 9")
    );

    let mut duplicate_export = graph();
    duplicate_export.modules[0]
      .exports
      .push(PrelinkedExport {
        exported_name: "value".to_string(),
        binding_id: 0,
      });
    assert!(
      duplicate_export
        .validate()
        .expect_err("duplicate export rejected")
        .to_string()
        .contains("duplicate export name")
    );

    let mut invalid_binding = graph();
    invalid_binding.modules[1].imports[0].binding_id = 1;
    assert!(
      invalid_binding
        .validate()
        .expect_err("binding range rejected")
        .to_string()
        .contains("outside 0..1")
    );

    let mut duplicate_root = graph();
    duplicate_root.public_roots.push(1);
    assert!(
      duplicate_root
        .validate()
        .expect_err("duplicate root rejected")
        .to_string()
        .contains("duplicate public root")
    );
  }

  #[test]
  fn rejects_invalid_chunks_and_descriptor_targets() {
    let mut invalid_chunk = graph();
    invalid_chunk.modules[0].chunk.lines.pop();
    assert!(matches!(
      invalid_chunk.validate(),
      Err(PrelinkedGraphError::InvalidChunk { .. })
    ));

    let mut invalid_descriptor = graph();
    let chunk = &mut invalid_descriptor.modules[1].chunk;
    chunk
      .constants
      .push(Constant::String("0\0missing".to_string()));
    chunk.code[0] = encode_risc_aux16(Op::GetModuleExport, 0, 0);
    assert!(matches!(
      invalid_descriptor.validate(),
      Err(PrelinkedGraphError::InvalidDescriptor { reason, .. })
        if reason.contains("absent export")
    ));

    let mut missing_edge = graph();
    missing_edge.modules[1].dependencies.clear();
    let chunk = &mut missing_edge.modules[1].chunk;
    chunk
      .constants
      .push(Constant::String("0\0value".to_string()));
    chunk.code[0] = encode_risc_aux16(Op::GetModuleExport, 0, 0);
    assert!(
      missing_edge
        .validate()
        .expect_err("missing import dependency rejected")
        .to_string()
        .contains("not exported by a declared dependency")
    );

    let mut binding_drift = graph();
    binding_drift.binding_count = 2;
    binding_drift.modules[0]
      .exports
      .push(PrelinkedExport {
        exported_name: "other".to_string(),
        binding_id: 1,
      });
    binding_drift.modules[1].imports[0].binding_id = 1;
    let chunk = &mut binding_drift.modules[1].chunk;
    chunk
      .constants
      .push(Constant::String("0\0value".to_string()));
    chunk.code[0] = encode_risc_aux16(Op::GetModuleExport, 0, 0);
    assert!(matches!(
      binding_drift.validate(),
      Err(PrelinkedGraphError::InvalidDescriptor { reason, .. })
        if reason.contains("absent from module 1 imports")
    ));

    let mut foreign_write = graph();
    let chunk = &mut foreign_write.modules[1].chunk;
    chunk
      .constants
      .push(Constant::String("0\0value".to_string()));
    chunk.code[0] = encode_risc_aux16(Op::SetModuleExport, 0, 0);
    assert!(matches!(
      foreign_write.validate(),
      Err(PrelinkedGraphError::InvalidDescriptor { reason, .. })
        if reason.contains("writes module 0")
    ));

    let mut dynamic_import = graph();
    let chunk = &mut dynamic_import.modules[1].chunk;
    chunk
      .constants
      .push(Constant::String("dynamicImport".to_string()));
    chunk.code[0] = encode_risc_aux16(Op::GetBuiltin, 0, 0);
    assert!(
      dynamic_import
        .validate()
        .expect_err("dynamic import bytecode rejected")
        .to_string()
        .contains("unsupported dynamic import")
    );
  }

  #[test]
  fn rejects_unsupported_graph_version_before_chunks() {
    let mut value = graph();
    value.format_version += 1;
    assert!(matches!(
      value.validate(),
      Err(PrelinkedGraphError::UnsupportedFormatVersion(_))
    ));
  }
}
