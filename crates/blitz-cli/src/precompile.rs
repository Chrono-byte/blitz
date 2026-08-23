#![allow(missing_docs)]
#![allow(
  clippy::default_numeric_fallback,
  clippy::missing_docs_in_private_items,
  clippy::missing_errors_doc
)]

use crate::args::{
  PrecompileArgs, PrecompileCommand, PrecompileProjectArgs, PrecompileRuntimeArgs,
};
use blitz_runtime::runtime_graph::{
  EFFECT_VERSION, HOST_BRIDGE_ABI_VERSION, HostPrimitiveRequirement, ModuleOrigin,
  ModuleVisibility, RUNTIME_ABI_VERSION, RUNTIME_GRAPH_FORMAT_VERSION, RuntimeGraphManifest,
  RuntimeModuleManifest, RuntimePrecompiledBundle, canonical_manifest_bytes, manifest_sha256,
};
use denko::Chunk;
use denko::bytecode::BytecodeVersion;
use denko::compiler::compile_session_with_context;
use denko::compiler::core::ModuleContext;
use denko::module::{
  ModuleLoader, ModuleRecord, ModuleRecordPayload, VirtualModule, VirtualModuleRegistry,
};
use denko::prelinked::{PrelinkedExport, PrelinkedImport, PrelinkedModule, PrelinkedModuleGraph};
use oxc_span::SourceType;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::rc::Rc;

const ARTIFACT_RELATIVE_PATH: &str = "crates/blitz-runtime/assets/runtime/v1/runtime.blitzbundle";
const GENERATOR_STACK_SIZE: usize = 32 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize)]
struct HashReference {
  path: String,
  sha256: String,
}

#[derive(Clone, Debug, Deserialize)]
struct EffectPackage {
  name: String,
  version: String,
  metadata: String,
  license: String,
}

#[derive(Clone, Debug, Deserialize)]
struct EffectEntry {
  specifier: String,
  source: HashReference,
  declaration: Option<HashReference>,
}

#[derive(Clone, Debug, Deserialize)]
struct EffectManifest {
  format: u32,
  package: EffectPackage,
  entries: Vec<EffectEntry>,
  files: Vec<HashReference>,
}

#[derive(Clone, Debug, Deserialize)]
struct HostPrimitive {
  name: String,
  abi_version: u16,
  methods: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
struct StdlibPackage {
  name: String,
  version: String,
  effect_version: String,
  runtime_abi_version: u16,
  host_abi_version: u16,
}

#[derive(Clone, Debug, Deserialize)]
struct StdlibEntry {
  specifier: String,
  visibility: String,
  source: HashReference,
  declaration: Option<HashReference>,
  dependencies: Vec<String>,
  host_primitives: Vec<HostPrimitive>,
}

#[derive(Clone, Debug, Deserialize)]
struct StdlibManifest {
  format: u32,
  package: StdlibPackage,
  host_primitive_catalog: Vec<HostPrimitive>,
  entries: Vec<StdlibEntry>,
  files: Vec<HashReference>,
}

#[derive(Clone, Debug)]
struct InputModule {
  specifier: String,
  source: String,
  source_sha256: [u8; 32],
  declaration_sha256: Option<[u8; 32]>,
  origin: ModuleOrigin,
  visibility: ModuleVisibility,
  declared_dependencies: Option<Vec<String>>,
  host_primitives: Vec<HostPrimitiveRequirement>,
}

struct GraphInput {
  emitted_specifier: String,
  source_sha256: [u8; 32],
  declared_dependencies: Option<Vec<String>>,
}

#[derive(Clone, Copy)]
struct BindingKey(*const RefCell<denko::Value>);

impl std::hash::Hash for BindingKey {
  fn hash<H: std::hash::Hasher>(
    &self,
    state: &mut H,
  ) {
    self.0.hash(state);
  }
}

impl PartialEq for BindingKey {
  fn eq(
    &self,
    other: &Self,
  ) -> bool {
    std::ptr::eq(self.0, other.0)
  }
}

impl Eq for BindingKey {}

pub(crate) fn run(
  args: PrecompileArgs,
  conditions: Vec<String>,
) -> Result<(), String> {
  let worker = std::thread::Builder::new()
    .name("blitz-precompile".to_string())
    .stack_size(GENERATOR_STACK_SIZE)
    .spawn(move || match args.command {
      PrecompileCommand::Runtime(args) => generate_runtime(args),
      PrecompileCommand::Project(args) => generate_project(args, conditions),
    });
  match worker {
    Ok(worker) => match worker.join() {
      Ok(result) => result,
      Err(_) => Err("precompiler thread panicked".to_string()),
    },
    Err(error) => Err(format!("could not start precompiler thread: {error}")),
  }
}

fn generate_runtime(args: PrecompileRuntimeArgs) -> Result<(), String> {
  let root = repository_root();
  let output = args.output.map_or_else(
    || root.join(ARTIFACT_RELATIVE_PATH),
    |path| absolutize(&root, path),
  );
  let bytes = build_artifact(&root)?;
  write_or_check(&output, &bytes, args.check, "blitz precompile runtime")
}

fn generate_project(
  args: PrecompileProjectArgs,
  conditions: Vec<String>,
) -> Result<(), String> {
  let entry = fs::canonicalize(&args.entry)
    .map_err(|error| format!("resolve project entry {}: {error}", args.entry.display()))?;
  if !entry.is_file() {
    return Err(format!("project entry is not a file: {}", entry.display()));
  }
  let entry_directory = entry
    .parent()
    .ok_or_else(|| format!("project entry has no parent: {}", entry.display()))?;
  let project = blitz_runtime::project::ProjectContext::discover(entry_directory)
    .map_err(|error| error.to_string())?;
  let package_name = args
    .name
    .or_else(|| {
      project
        .package
        .as_ref()
        .and_then(|package| package.name.clone())
    })
    .or_else(|| {
      project
        .root
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
    })
    .unwrap_or_else(|| "blitz-project".to_string());
  let package_version = args
    .version
    .or_else(|| {
      project
        .package
        .as_ref()
        .and_then(|package| package.version.clone())
    })
    .unwrap_or_else(|| "0.0.0".to_string());
  let output = args.output.map_or_else(
    || entry.with_extension("blitzbundle"),
    |path| {
      if path.is_absolute() {
        path
      } else {
        project.cwd.join(path)
      }
    },
  );
  let bytes = build_project_artifact(
    &entry,
    &project.root,
    package_name,
    package_version,
    conditions,
    !args.no_runtime,
  )?;
  write_or_check(
    &output,
    &bytes,
    args.check,
    &format!("blitz precompile project {}", args.entry.display()),
  )
}

fn write_or_check(
  output: &Path,
  bytes: &[u8],
  check: bool,
  regenerate_command: &str,
) -> Result<(), String> {
  if check {
    let existing = fs::read(output)
      .map_err(|error| format!("cannot read {} for --check: {error}", output.display()))?;
    if existing != bytes {
      return Err(format!(
        "{} is stale; run `{regenerate_command}`",
        output.display(),
      ));
    }
    println!("checked {} ({} bytes)", output.display(), bytes.len());
  } else {
    write_atomically(output, bytes)?;
    println!("generated {} ({} bytes)", output.display(), bytes.len());
  }
  Ok(())
}

fn build_artifact(root: &Path) -> Result<Vec<u8>, String> {
  let effect_root = root.join("crates/blitz-runtime/assets/effect/3.16.7");
  let stdlib_root = root.join("crates/blitz-runtime/assets/stdlib");
  let effect_manifest_path = effect_root.join("manifest.json");
  let stdlib_manifest_path = stdlib_root.join("manifest.json");
  let effect_manifest_bytes = read_file(&effect_manifest_path)?;
  let stdlib_manifest_bytes = read_file(&stdlib_manifest_path)?;
  let effect_manifest: EffectManifest = parse_json(&effect_manifest_bytes, &effect_manifest_path)?;
  let stdlib_manifest: StdlibManifest = parse_json(&stdlib_manifest_bytes, &stdlib_manifest_path)?;

  verify_effect_manifest(&effect_root, &effect_manifest)?;
  verify_stdlib_manifest(&stdlib_root, &stdlib_manifest, &effect_manifest)?;

  let effect_manifest_hash = manifest_sha256(&effect_manifest_bytes);
  let stdlib_manifest_hash = manifest_sha256(&stdlib_manifest_bytes);
  let inputs = collect_inputs(
    &effect_root,
    &stdlib_root,
    &effect_manifest,
    &stdlib_manifest,
  )?;
  let expected_specifiers = inputs
    .iter()
    .map(|module| module.specifier.clone())
    .collect::<BTreeSet<_>>();
  if expected_specifiers.len() != inputs.len() {
    return Err("Effect and stdlib manifests contain duplicate module specifiers".into());
  }

  let mut registry = VirtualModuleRegistry::new();
  for module in &inputs {
    registry
      .register(VirtualModule::javascript(
        module.specifier.clone(),
        module.source.clone(),
        SourceType::mjs(),
        None,
      ))
      .map_err(|error| format!("register {}: {error}", module.specifier))?;
  }

  let public_roots = inputs
    .iter()
    .filter(|module| module.visibility == ModuleVisibility::Public)
    .map(|module| module.specifier.clone())
    .collect::<Vec<_>>();
  if public_roots.is_empty() {
    return Err("the combined runtime manifests have no public roots".into());
  }

  let mut loader = ModuleLoader::new();
  loader
    .register_virtual_modules(registry)
    .map_err(|error| format!("register runtime source graph: {error}"))?;
  // Construct every public root first, then every manifest entry. The latter
  // makes coverage explicit while still using the loader's normal graph walk.
  for specifier in &public_roots {
    loader
      .construct(Path::new("."), specifier)
      .map_err(|error| format!("construct public root {specifier}: {error}"))?;
  }
  let mut all_specifiers = expected_specifiers
    .iter()
    .cloned()
    .collect::<Vec<_>>();
  all_specifiers.sort();
  for specifier in &all_specifiers {
    loader
      .construct(Path::new("."), specifier)
      .map_err(|error| format!("construct runtime module {specifier}: {error}"))?;
  }

  let actual_specifiers = loader
    .map
    .values()
    .map(|record| record.borrow().specifier.clone())
    .collect::<BTreeSet<_>>();
  if actual_specifiers != expected_specifiers {
    return Err(format!(
      "runtime graph coverage mismatch: expected {} modules, constructed {}",
      expected_specifiers.len(),
      actual_specifiers.len()
    ));
  }

  loader
    .instantiate_all()
    .map_err(|error| format!("instantiate complete runtime graph: {error}"))?;

  let input_by_specifier = inputs
    .into_iter()
    .map(|module| (module.specifier.clone(), module))
    .collect::<BTreeMap<_, _>>();
  let graph_inputs = input_by_specifier
    .iter()
    .map(|(specifier, input)| {
      (
        specifier.clone(),
        GraphInput {
          emitted_specifier: specifier.clone(),
          source_sha256: input.source_sha256,
          declared_dependencies: input.declared_dependencies.clone(),
        },
      )
    })
    .collect::<BTreeMap<_, _>>();
  let mut source_manifest_hasher = Sha256::new();
  let canonical_effect_manifest = canonical_manifest_bytes(&effect_manifest_bytes);
  let canonical_stdlib_manifest = canonical_manifest_bytes(&stdlib_manifest_bytes);
  source_manifest_hasher.update((canonical_effect_manifest.len() as u64).to_le_bytes());
  source_manifest_hasher.update(canonical_effect_manifest.as_ref());
  source_manifest_hasher.update((canonical_stdlib_manifest.len() as u64).to_le_bytes());
  source_manifest_hasher.update(canonical_stdlib_manifest.as_ref());
  let (graph, records) = precompile_graph(
    &loader,
    &graph_inputs,
    &public_roots,
    "effect+blitz-stdlib".to_string(),
    format!("effect-{EFFECT_VERSION}+runtime-1"),
    source_manifest_hasher.finalize().into(),
    None,
  )?;
  let runtime_manifest = runtime_manifest(
    &records,
    &input_by_specifier,
    effect_manifest_hash,
    stdlib_manifest_hash,
    public_roots,
  )?;
  runtime_manifest
    .validate()
    .map_err(|error| format!("runtime envelope metadata is invalid: {error}"))?;
  let bundle = RuntimePrecompiledBundle {
    manifest: runtime_manifest,
    graph,
  };
  bundle
    .validate()
    .map_err(|error| format!("generated runtime bundle validation failed: {error}"))?;
  let first = bundle
    .encode()
    .map_err(|error| format!("encode generated runtime bundle: {error}"))?;
  let second = bundle
    .encode()
    .map_err(|error| format!("encode generated runtime bundle twice: {error}"))?;
  if first != second {
    return Err("prelinked graph encoding is not deterministic in memory".into());
  }
  Ok(first)
}

fn build_project_artifact(
  entry: &Path,
  project_root: &Path,
  package_name: String,
  package_version: String,
  conditions: Vec<String>,
  include_runtime: bool,
) -> Result<Vec<u8>, String> {
  let mut loader = ModuleLoader::new();
  loader.set_condition_names(conditions);
  let base_graph = include_runtime
    .then(|| {
      blitz_runtime::runtime_graph::validate_embedded_runtime_bundle()
        .map(|bundle| bundle.graph.clone())
        .map_err(|error| format!("load embedded runtime bundle: {error}"))
    })
    .transpose()?;
  if let Some(graph) = &base_graph {
    loader
      .register_prelinked_graph(graph.clone())
      .map_err(|error| format!("register embedded runtime graph: {error}"))?;
  }
  let entry_directory = entry
    .parent()
    .ok_or_else(|| format!("project entry has no parent: {}", entry.display()))?;
  let entry_request = format!(
    "./{}",
    entry
      .file_name()
      .ok_or_else(|| format!("project entry has no file name: {}", entry.display()))?
      .to_string_lossy()
  );
  let entry_record = loader
    .construct(entry_directory, &entry_request)
    .map_err(|error| format!("construct project entry {}: {error}", entry.display()))?;
  let entry_specifier = entry_record.borrow().specifier.clone();
  loader
    .instantiate_all()
    .map_err(|error| format!("instantiate project graph: {error}"))?;

  let mut graph_inputs = BTreeMap::new();
  for record_view in sorted_source_records(&loader)? {
    let record = record_view.record.borrow();
    let relative = record.path.strip_prefix(project_root).map_err(|_| {
      format!(
        "project dependency {} is outside package root {}; move it under the project or precompile from a common package root",
        record.path.display(),
        project_root.display()
      )
    })?;
    let emitted_specifier = format!("./{}", relative.to_string_lossy().replace('\\', "/"));
    let ModuleRecordPayload::Source { session, .. } = &record.payload else {
      return Err(format!(
        "project module {} is not source-backed",
        record.specifier
      ));
    };
    let input = GraphInput {
      emitted_specifier,
      source_sha256: sha256(session.source().as_bytes()),
      declared_dependencies: None,
    };
    if graph_inputs
      .insert(record.specifier.clone(), input)
      .is_some()
    {
      return Err(format!("duplicate project module {}", record.specifier));
    }
  }

  let mut source_manifest_hasher = Sha256::new();
  if let Some(base_graph) = &base_graph {
    source_manifest_hasher.update(base_graph.source_manifest_hash);
  }
  let mut emitted_specifiers = BTreeSet::new();
  for input in graph_inputs.values() {
    if !emitted_specifiers.insert(input.emitted_specifier.as_str()) {
      return Err(format!(
        "duplicate portable project specifier {}",
        input.emitted_specifier
      ));
    }
    source_manifest_hasher.update((input.emitted_specifier.len() as u64).to_le_bytes());
    source_manifest_hasher.update(input.emitted_specifier.as_bytes());
    source_manifest_hasher.update(input.source_sha256);
  }

  let (graph, _) = precompile_graph(
    &loader,
    &graph_inputs,
    &[entry_specifier],
    package_name,
    package_version,
    source_manifest_hasher.finalize().into(),
    base_graph.as_ref(),
  )?;
  let first = denko::encode_prelinked_graph(&graph)
    .map_err(|error| format!("encode project bundle: {error}"))?;
  let second = denko::encode_prelinked_graph(&graph)
    .map_err(|error| format!("encode project bundle twice: {error}"))?;
  if first != second {
    return Err("project bundle encoding is not deterministic in memory".into());
  }
  Ok(first)
}

fn precompile_graph(
  loader: &ModuleLoader,
  inputs: &BTreeMap<String, GraphInput>,
  public_roots: &[String],
  package_name: String,
  package_version: String,
  source_manifest_hash: [u8; 32],
  base_graph: Option<&PrelinkedModuleGraph>,
) -> Result<(PrelinkedModuleGraph, Vec<RecordView>), String> {
  let records = sorted_source_records(loader)?;
  let mut index_by_specifier = loader
    .map
    .values()
    .map(|record| {
      let record = record.borrow();
      (
        record.specifier.clone(),
        record.module_index.unwrap_or_default(),
      )
    })
    .collect::<HashMap<_, _>>();
  if let Some(base_graph) = base_graph {
    index_by_specifier.extend(base_graph.modules.iter().map(|module| {
      (
        module.specifier.clone(),
        usize::try_from(module.module_index).unwrap_or_default(),
      )
    }));
  }
  let public_root_indices = public_roots
    .iter()
    .map(|specifier| {
      index_by_specifier
        .get(specifier)
        .copied()
        .ok_or_else(|| format!("public root {specifier} was not assigned a module index"))
        .and_then(|index| {
          u32::try_from(index)
            .map_err(|_| format!("public root {specifier} index does not fit u32"))
        })
    })
    .collect::<Result<Vec<_>, _>>()?;

  let mut binding_ids = HashMap::new();
  let mut next_binding_id = base_graph.map_or(0, |graph| graph.binding_count);
  if let Some(base_graph) = base_graph {
    for record in loader.map.values() {
      let record = record.borrow();
      let Some(module_index) = record.module_index else {
        continue;
      };
      let Ok(module_index) = u32::try_from(module_index) else {
        continue;
      };
      let Some(module) = base_graph.module(module_index) else {
        continue;
      };
      for export in &module.exports {
        let slot = record
          .exports
          .get(&export.exported_name)
          .ok_or_else(|| {
            format!(
              "embedded runtime export {} is absent from {}",
              export.exported_name, record.specifier
            )
          })?;
        binding_ids.insert(BindingKey(Rc::as_ptr(slot)), export.binding_id);
      }
    }
  }
  for record in &records {
    let record = record.record.borrow();
    let mut exports = record.exports.iter().collect::<Vec<_>>();
    exports.sort_by(|left, right| left.0.cmp(right.0));
    for (_, slot) in exports {
      let key = BindingKey(Rc::as_ptr(slot));
      if let std::collections::hash_map::Entry::Vacant(entry) = binding_ids.entry(key) {
        entry.insert(next_binding_id);
        next_binding_id = next_binding_id
          .checked_add(1)
          .ok_or_else(|| "project has too many live bindings".to_string())?;
      }
    }
  }

  let mut modules = base_graph.map_or_else(Vec::new, |graph| graph.modules.clone());
  modules.reserve(records.len());
  for record_view in &records {
    let record = record_view.record.borrow();
    let input = inputs.get(&record.specifier).ok_or_else(|| {
      format!(
        "constructed module has no precompiler input: {}",
        record.specifier
      )
    })?;
    let dependencies = record_dependencies(&record, &index_by_specifier, loader.condition_names())?;
    if let Some(declared) = &input.declared_dependencies {
      let dependency_specifiers = dependencies
        .iter()
        .map(|index| records[*index].record.borrow().specifier.clone())
        .collect::<Vec<_>>();
      if declared != &dependency_specifiers {
        return Err(format!(
          "dependency order is stale for {}",
          record.specifier
        ));
      }
    }

    let mut exports = record.exports.iter().collect::<Vec<_>>();
    exports.sort_by(|left, right| left.0.cmp(right.0));
    let exports = exports
      .into_iter()
      .map(|(name, slot)| {
        let binding_id = binding_ids
          .get(&BindingKey(Rc::as_ptr(slot)))
          .copied()
          .ok_or_else(|| format!("export {name} in {} has no binding ID", record.specifier))?;
        Ok(PrelinkedExport {
          exported_name: name.clone(),
          binding_id,
        })
      })
      .collect::<Result<Vec<_>, String>>()?;

    let mut imports = record
      .imports
      .iter()
      .filter(|(name, _)| {
        record
          .import_sources
          .get(*name)
          .is_none_or(|(_, exported_name)| exported_name != "*")
      })
      .collect::<Vec<_>>();
    imports.sort_by(|left, right| left.0.cmp(right.0));
    let imports = imports
      .into_iter()
      .map(|(name, slot)| {
        let binding_id = binding_ids
          .get(&BindingKey(Rc::as_ptr(slot)))
          .copied()
          .ok_or_else(|| {
            format!(
              "import {name} in {} is not backed by a canonical exported slot",
              record.specifier
            )
          })?;
        Ok(PrelinkedImport {
          local_name: name.clone(),
          binding_id,
        })
      })
      .collect::<Result<Vec<_>, String>>()?;

    let chunk = compile_record(&record)?;
    modules.push(PrelinkedModule {
      specifier: input.emitted_specifier.clone(),
      module_index: u32::try_from(record.module_index.unwrap_or_default())
        .map_err(|_| format!("module index for {} does not fit u32", record.specifier))?,
      source_sha256: input.source_sha256,
      dependencies: dependencies
        .into_iter()
        .map(|index| {
          u32::try_from(index)
            .map_err(|_| format!("dependency index for {} does not fit u32", record.specifier))
        })
        .collect::<Result<Vec<_>, _>>()?,
      exports,
      imports,
      chunk,
    });
  }

  let graph = PrelinkedModuleGraph {
    format_version: denko::prelinked::PRELINKED_MODULE_GRAPH_VERSION,
    engine_bytecode_version: BytecodeVersion::RiscV2,
    package_name,
    package_version,
    source_manifest_hash,
    modules,
    binding_count: next_binding_id,
    public_roots: public_root_indices,
  };
  graph
    .validate()
    .map_err(|error| format!("generated graph validation failed: {error}"))?;
  Ok((graph, records))
}

struct RecordView {
  record: Rc<RefCell<ModuleRecord>>,
}

fn sorted_source_records(loader: &ModuleLoader) -> Result<Vec<RecordView>, String> {
  let mut records = loader
    .map
    .values()
    .filter(|record| matches!(record.borrow().payload, ModuleRecordPayload::Source { .. }))
    .cloned()
    .map(|record| RecordView { record })
    .collect::<Vec<_>>();
  records.sort_by_key(|record| record.record.borrow().module_index);
  for record in &records {
    if record.record.borrow().module_index.is_none() {
      return Err(format!(
        "module {} has no assigned index",
        record.record.borrow().specifier
      ));
    }
  }
  Ok(records)
}

fn compile_record(record: &ModuleRecord) -> Result<Chunk, String> {
  let ModuleRecordPayload::Source { session, .. } = &record.payload else {
    return Err(format!("{} is not source-backed", record.specifier));
  };
  let context = ModuleContext {
    module_index: record.module_index.unwrap_or_default(),
    imports: record.import_sources.clone(),
    exports: record.exports.keys().cloned().collect(),
    namespace_exports: record.namespace_exports.clone(),
  };
  let chunk = compile_session_with_context(session, &context)
    .map_err(|error| format!("compile {}: {error}", record.specifier))?;
  let chunk = denko::runtime_utils::clear_slot_types_in_chunk(chunk);
  chunk
    .verify()
    .map_err(|error| format!("verify compiled {}: {error}", record.specifier))?;
  Ok(chunk)
}

fn record_dependencies(
  record: &ModuleRecord,
  index_by_specifier: &HashMap<String, usize>,
  conditions: &[String],
) -> Result<Vec<usize>, String> {
  let ModuleRecordPayload::Source { dependencies, .. } = &record.payload else {
    return Err(format!("{} is not source-backed", record.specifier));
  };
  let mut seen = HashSet::new();
  dependencies
    .iter()
    .filter(|specifier| seen.insert((*specifier).clone()))
    .map(|specifier| {
      if let Some(index) = index_by_specifier.get(specifier).copied() {
        return Ok(index);
      }
      let base = record
        .path
        .parent()
        .unwrap_or_else(|| Path::new("."));
      let resolved = denko::resolve_specifier_with_conditions(base, specifier, conditions)
        .map_err(|error| {
          format!(
            "resolve dependency {} -> {specifier}: {error}",
            record.specifier
          )
        })?;
      let resolved = fs::canonicalize(&resolved).unwrap_or(resolved);
      let canonical = resolved.to_string_lossy().into_owned();
      index_by_specifier
        .get(&canonical)
        .copied()
        .ok_or_else(|| {
          format!(
            "closed graph violation: {} -> {specifier} (resolved as {canonical})",
            record.specifier
          )
        })
    })
    .collect()
}

fn runtime_manifest(
  records: &[RecordView],
  inputs: &BTreeMap<String, InputModule>,
  effect_manifest_hash: [u8; 32],
  stdlib_manifest_hash: [u8; 32],
  public_roots: Vec<String>,
) -> Result<RuntimeGraphManifest, String> {
  let mut modules = Vec::with_capacity(records.len());
  let index_by_specifier = records
    .iter()
    .map(|record| {
      let record = record.record.borrow();
      (
        record.specifier.clone(),
        record.module_index.unwrap_or_default(),
      )
    })
    .collect::<HashMap<_, _>>();
  for record_view in records {
    let record = record_view.record.borrow();
    let input = inputs
      .get(&record.specifier)
      .ok_or_else(|| format!("missing runtime metadata for {}", record.specifier))?;
    let dependencies = record_dependencies(&record, &index_by_specifier, &[])?
      .into_iter()
      .map(|index| records[index].record.borrow().specifier.clone())
      .collect();
    modules.push(RuntimeModuleManifest {
      specifier: record.specifier.clone(),
      origin: input.origin,
      visibility: input.visibility,
      source_sha256: input.source_sha256,
      declaration_sha256: input.declaration_sha256,
      dependencies,
      required_host_primitives: input.host_primitives.clone(),
    });
  }
  modules.sort_by(|left, right| left.specifier.cmp(&right.specifier));
  Ok(RuntimeGraphManifest {
    format_version: RUNTIME_GRAPH_FORMAT_VERSION,
    runtime_abi_version: RUNTIME_ABI_VERSION,
    host_bridge_abi_version: HOST_BRIDGE_ABI_VERSION,
    effect_version: EFFECT_VERSION.to_string(),
    effect_manifest_sha256: effect_manifest_hash,
    stdlib_manifest_sha256: stdlib_manifest_hash,
    modules,
    public_roots,
  })
}

fn collect_inputs(
  effect_root: &Path,
  stdlib_root: &Path,
  effect_manifest: &EffectManifest,
  stdlib_manifest: &StdlibManifest,
) -> Result<Vec<InputModule>, String> {
  let mut inputs =
    Vec::with_capacity(effect_manifest.entries.len() + stdlib_manifest.entries.len());
  for entry in &effect_manifest.entries {
    inputs.push(InputModule {
      specifier: entry.specifier.clone(),
      source: read_file_string(&effect_root.join(&entry.source.path))?,
      source_sha256: decode_hash(&entry.source.sha256)?,
      declaration_sha256: entry
        .declaration
        .as_ref()
        .map(|reference| decode_hash(&reference.sha256))
        .transpose()?,
      origin: ModuleOrigin::Effect,
      visibility: if entry.specifier == "effect" || entry.specifier.starts_with("effect/") {
        ModuleVisibility::Public
      } else {
        ModuleVisibility::Private
      },
      declared_dependencies: None,
      host_primitives: Vec::new(),
    });
  }
  for entry in &stdlib_manifest.entries {
    let visibility = match entry.visibility.as_str() {
      "public" => ModuleVisibility::Public,
      "private" => ModuleVisibility::Private,
      value => return Err(format!("invalid stdlib visibility {value:?}")),
    };
    inputs.push(InputModule {
      specifier: entry.specifier.clone(),
      source: read_file_string(&stdlib_root.join(&entry.source.path))?,
      source_sha256: decode_hash(&entry.source.sha256)?,
      declaration_sha256: entry
        .declaration
        .as_ref()
        .map(|reference| decode_hash(&reference.sha256))
        .transpose()?,
      origin: ModuleOrigin::BlitzStdlib,
      visibility,
      declared_dependencies: Some(entry.dependencies.clone()),
      host_primitives: entry
        .host_primitives
        .iter()
        .map(|primitive| {
          HostPrimitiveRequirement::new(
            primitive.name.clone(),
            primitive.abi_version,
            primitive.methods.clone(),
          )
        })
        .collect(),
    });
  }
  Ok(inputs)
}

fn verify_effect_manifest(
  root: &Path,
  manifest: &EffectManifest,
) -> Result<(), String> {
  if manifest.format != 1
    || manifest.package.name != "effect"
    || manifest.package.version != EFFECT_VERSION
  {
    return Err("pinned Effect manifest metadata is incompatible".into());
  }
  verify_file_list(root, &manifest.files)?;
  verify_reference(
    root,
    &manifest.files,
    &HashReference {
      path: manifest.package.metadata.clone(),
      sha256: manifest
        .files
        .iter()
        .find(|file| file.path == manifest.package.metadata)
        .ok_or_else(|| "Effect metadata file is not listed".to_string())?
        .sha256
        .clone(),
    },
    "Effect package metadata",
  )?;
  verify_reference(
    root,
    &manifest.files,
    &HashReference {
      path: manifest.package.license.clone(),
      sha256: manifest
        .files
        .iter()
        .find(|file| file.path == manifest.package.license)
        .ok_or_else(|| "Effect license file is not listed".to_string())?
        .sha256
        .clone(),
    },
    "Effect license",
  )?;
  let mut specifiers = BTreeSet::new();
  for entry in &manifest.entries {
    if !specifiers.insert(entry.specifier.as_str()) {
      return Err(format!("duplicate Effect specifier {}", entry.specifier));
    }
    verify_reference(root, &manifest.files, &entry.source, &entry.specifier)?;
    if let Some(declaration) = &entry.declaration {
      verify_reference(root, &manifest.files, declaration, &entry.specifier)?;
    }
  }
  Ok(())
}

fn verify_stdlib_manifest(
  root: &Path,
  manifest: &StdlibManifest,
  effect_manifest: &EffectManifest,
) -> Result<(), String> {
  if manifest.format != 1
    || manifest.package.name != "blitz-stdlib"
    || manifest.package.version != "1"
    || manifest.package.effect_version != EFFECT_VERSION
    || manifest.package.runtime_abi_version != RUNTIME_ABI_VERSION
    || manifest.package.host_abi_version != HOST_BRIDGE_ABI_VERSION
  {
    return Err("stdlib manifest metadata is incompatible".into());
  }
  verify_file_list(root, &manifest.files)?;
  let effect_specifiers = effect_manifest
    .entries
    .iter()
    .map(|entry| entry.specifier.as_str())
    .collect::<HashSet<_>>();
  let mut catalog = BTreeMap::new();
  for primitive in &manifest.host_primitive_catalog {
    if catalog
      .insert(primitive.name.as_str(), primitive)
      .is_some()
    {
      return Err(format!(
        "duplicate stdlib host primitive {}",
        primitive.name
      ));
    }
  }
  let mut specifiers = BTreeSet::new();
  for entry in &manifest.entries {
    if !specifiers.insert(entry.specifier.as_str()) {
      return Err(format!("duplicate stdlib specifier {}", entry.specifier));
    }
    if entry.visibility == "public"
      && !entry.specifier.starts_with("blitz:")
      && !entry.specifier.starts_with("node:")
    {
      return Err(format!(
        "public stdlib module has invalid prefix {}",
        entry.specifier
      ));
    }
    if entry.visibility == "private" && entry.declaration.is_some() {
      return Err(format!(
        "private stdlib module has a declaration {}",
        entry.specifier
      ));
    }
    for dependency in &entry.dependencies {
      if !effect_specifiers.contains(dependency.as_str())
        && !specifiers.contains(dependency.as_str())
      {
        // Entries are sorted, so a later stdlib entry is checked below after
        // the complete set is known.
        if !manifest
          .entries
          .iter()
          .any(|candidate| candidate.specifier == *dependency)
        {
          return Err(format!("unresolved stdlib dependency {dependency}"));
        }
      }
    }
    for primitive in &entry.host_primitives {
      let Some(catalog_primitive) = catalog.get(primitive.name.as_str()) else {
        return Err(format!(
          "host primitive {} is absent from catalog",
          primitive.name
        ));
      };
      if catalog_primitive.abi_version != primitive.abi_version
        || catalog_primitive.methods != primitive.methods
      {
        return Err(format!(
          "host primitive {} differs from catalog",
          primitive.name
        ));
      }
    }
    verify_reference(root, &manifest.files, &entry.source, &entry.specifier)?;
    if let Some(declaration) = &entry.declaration {
      verify_reference(root, &manifest.files, declaration, &entry.specifier)?;
    }
  }
  Ok(())
}

fn verify_file_list(
  root: &Path,
  listed: &[HashReference],
) -> Result<(), String> {
  let listed_paths = listed
    .iter()
    .map(|file| file.path.clone())
    .collect::<BTreeSet<_>>();
  if listed_paths.len() != listed.len() {
    return Err(format!("duplicate file in manifest for {}", root.display()));
  }
  let actual = files_under(root)?;
  if actual
    .into_iter()
    .filter(|path| path != "manifest.json")
    .collect::<BTreeSet<_>>()
    != listed_paths
  {
    return Err(format!("file list is stale for {}", root.display()));
  }
  for file in listed {
    verify_reference(root, listed, file, "manifest file")?;
  }
  Ok(())
}

fn verify_reference(
  root: &Path,
  listed: &[HashReference],
  reference: &HashReference,
  label: &str,
) -> Result<(), String> {
  if reference.path.is_empty()
    || reference.path.starts_with('/')
    || reference
      .path
      .split('/')
      .any(|part| part.is_empty() || part == "..")
  {
    return Err(format!("{label} has an unsafe path {}", reference.path));
  }
  let listed_hash = listed
    .iter()
    .find(|file| file.path == reference.path)
    .map(|file| file.sha256.as_str());
  if listed_hash != Some(reference.sha256.as_str()) {
    return Err(format!("{label} is not backed by its manifest file entry"));
  }
  let path = root.join(&reference.path);
  if !path.starts_with(root) {
    return Err(format!("{label} escapes asset root"));
  }
  let actual = sha256(&read_file(&path)?);
  if actual != decode_hash(&reference.sha256)? {
    return Err(format!("hash mismatch for {}", reference.path));
  }
  Ok(())
}

fn files_under(root: &Path) -> Result<Vec<String>, String> {
  fn visit(
    root: &Path,
    current: &Path,
    output: &mut Vec<String>,
  ) -> Result<(), String> {
    for entry in
      fs::read_dir(current).map_err(|error| format!("read {}: {error}", current.display()))?
    {
      let entry = entry.map_err(|error| format!("read directory entry: {error}"))?;
      let path = entry.path();
      if path.is_dir() {
        visit(root, &path, output)?;
      } else {
        let relative = path
          .strip_prefix(root)
          .map_err(|error| format!("relative asset path: {error}"))?
          .to_string_lossy()
          .replace('\\', "/");
        output.push(relative);
      }
    }
    Ok(())
  }
  let mut output = Vec::new();
  visit(root, root, &mut output)?;
  output.sort();
  Ok(output)
}

fn read_file(path: &Path) -> Result<Vec<u8>, String> {
  fs::read(path).map_err(|error| format!("read {}: {error}", path.display()))
}

fn read_file_string(path: &Path) -> Result<String, String> {
  let bytes = read_file(path)?;
  String::from_utf8(canonical_manifest_bytes(&bytes).into_owned())
    .map_err(|error| format!("{} is not UTF-8: {error}", path.display()))
}

fn parse_json<T: for<'de> Deserialize<'de>>(
  bytes: &[u8],
  path: &Path,
) -> Result<T, String> {
  serde_json::from_slice(bytes).map_err(|error| format!("parse {}: {error}", path.display()))
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
  Sha256::digest(canonical_manifest_bytes(bytes).as_ref()).into()
}

fn decode_hash(value: &str) -> Result<[u8; 32], String> {
  if value.len() != 64 {
    return Err(format!("invalid SHA-256 length for {value:?}"));
  }
  let mut output = [0_u8; 32];
  for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
    let high = hex_digit(pair[0]).ok_or_else(|| format!("invalid SHA-256 {value:?}"))?;
    let low = hex_digit(pair[1]).ok_or_else(|| format!("invalid SHA-256 {value:?}"))?;
    output[index] = (high << 4) | low;
  }
  Ok(output)
}

fn hex_digit(value: u8) -> Option<u8> {
  match value {
    b'0'..=b'9' => Some(value - b'0'),
    b'a'..=b'f' => Some(value - b'a' + 10),
    _ => None,
  }
}

fn write_atomically(
  path: &Path,
  bytes: &[u8],
) -> Result<(), String> {
  let parent = path
    .parent()
    .ok_or_else(|| format!("artifact has no parent: {}", path.display()))?;
  fs::create_dir_all(parent).map_err(|error| format!("create {}: {error}", parent.display()))?;
  let temporary = parent.join(format!(
    ".{}.tmp-{}",
    path
      .file_name()
      .unwrap_or_default()
      .to_string_lossy(),
    std::process::id()
  ));
  let result = (|| {
    let mut file = File::create(&temporary)
      .map_err(|error| format!("create {}: {error}", temporary.display()))?;
    file
      .write_all(bytes)
      .map_err(|error| format!("write {}: {error}", temporary.display()))?;
    file
      .sync_all()
      .map_err(|error| format!("sync {}: {error}", temporary.display()))?;
    fs::rename(&temporary, path).map_err(|error| {
      format!(
        "rename {} to {}: {error}",
        temporary.display(),
        path.display()
      )
    })
  })();
  if result.is_err() {
    drop(fs::remove_file(&temporary));
  }
  result
}

fn repository_root() -> PathBuf {
  PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    .parent()
    .and_then(Path::parent)
    .map(Path::to_path_buf)
    .expect("blitz-cli is nested below the repository root")
}

fn absolutize(
  root: &Path,
  path: PathBuf,
) -> PathBuf {
  if path.is_absolute() {
    path
  } else {
    root.join(path)
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::time::{SystemTime, UNIX_EPOCH};

  fn temporary_directory(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
      "blitz-precompile-{label}-{}-{}",
      std::process::id(),
      SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock")
        .as_nanos()
    ))
  }

  #[test]
  fn project_bundle_is_portable_deterministic_and_registerable() {
    let root = temporary_directory("project");
    let source_root = root.join("src");
    fs::create_dir_all(&source_root).expect("create source directory");
    fs::write(
      root.join("package.json"),
      r#"{"name":"portable-project","version":"1.2.3","type":"module"}"#,
    )
    .expect("write package manifest");
    fs::write(
      source_root.join("value.ts"),
      denko::js!(typescript, "export const value: number = 41;\n"),
    )
    .expect("write dependency");
    fs::write(
      source_root.join("main.ts"),
      denko::js!(
        typescript,
        "import { value } from './value.ts';\nexport const answer = value + 1;\n"
      ),
    )
    .expect("write entry");

    let entry = fs::canonicalize(source_root.join("main.ts")).expect("canonical entry");
    let project_root = fs::canonicalize(&root).expect("canonical project root");
    let first = build_project_artifact(
      &entry,
      &project_root,
      "portable-project".to_string(),
      "1.2.3".to_string(),
      Vec::new(),
      false,
    )
    .expect("precompile project");
    let second = build_project_artifact(
      &entry,
      &project_root,
      "portable-project".to_string(),
      "1.2.3".to_string(),
      Vec::new(),
      false,
    )
    .expect("precompile project again");
    assert_eq!(first, second);

    let graph = denko::decode_prelinked_graph(&first).expect("decode project bundle");
    assert_eq!(graph.package_name, "portable-project");
    assert_eq!(graph.package_version, "1.2.3");
    assert_eq!(graph.modules.len(), 2);
    assert!(
      graph
        .modules
        .iter()
        .any(|module| module.specifier == "./src/main.ts")
    );
    assert!(
      graph
        .modules
        .iter()
        .any(|module| module.specifier == "./src/value.ts")
    );
    assert!(graph.modules.iter().all(|module| {
      !module
        .specifier
        .contains(root.to_string_lossy().as_ref())
    }));

    let mut loader = ModuleLoader::new();
    loader
      .register_prelinked_graph(graph)
      .expect("register project bundle");
    loader
      .construct(Path::new("."), "./src/main.ts")
      .expect("construct bundled entry");

    fs::remove_dir_all(root).expect("remove temporary project");
  }
}
