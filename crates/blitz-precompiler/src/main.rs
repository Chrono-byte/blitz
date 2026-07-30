#![allow(missing_docs)]
#![allow(
  clippy::default_numeric_fallback,
  clippy::missing_docs_in_private_items,
  clippy::missing_errors_doc
)]

use blitz_runtime::runtime_graph::{
  EFFECT_VERSION, HOST_BRIDGE_ABI_VERSION, HostPrimitiveRequirement, ModuleOrigin,
  ModuleVisibility, RUNTIME_ABI_VERSION, RUNTIME_GRAPH_FORMAT_VERSION, RuntimeGraphManifest,
  RuntimeModuleManifest, RuntimePrecompiledBundle, canonical_manifest_bytes, manifest_sha256,
};
use clap::{Args, Parser, Subcommand};
use denko::bytecode::BytecodeVersion;
use denko::compiler::core::ModuleContext;
use denko::module::{
  ModuleLoader, ModuleRecord, ModuleRecordPayload, VirtualModule, VirtualModuleRegistry,
};
use denko::prelinked::{PrelinkedExport, PrelinkedImport, PrelinkedModule, PrelinkedModuleGraph};
use denko::{Chunk, compile_oxc_with_ctx};
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

#[derive(Parser)]
#[command(
  name = "blitz-precompiler",
  about = "Build the deterministic precompiled Blitz runtime bundle"
)]
struct Cli {
  #[command(subcommand)]
  command: Command,
}

#[derive(Subcommand)]
enum Command {
  Generate(GenerateArgs),
}

#[derive(Args)]
struct GenerateArgs {
  /// Regenerate in memory and compare with the committed artifact.
  #[arg(long)]
  check: bool,
  /// Override the versioned artifact path.
  #[arg(long)]
  output: Option<PathBuf>,
}

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

fn main() {
  let worker = std::thread::Builder::new()
    .name("blitz-precompiler".to_string())
    .stack_size(GENERATOR_STACK_SIZE)
    .spawn(run);
  let result = match worker {
    Ok(worker) => match worker.join() {
      Ok(result) => result,
      Err(_) => Err("generator thread panicked".to_string()),
    },
    Err(error) => Err(format!("could not start generator thread: {error}")),
  };
  if let Err(error) = result {
    eprintln!("blitz-precompiler: {error}");
    std::process::exit(1);
  }
}

fn run() -> Result<(), String> {
  let cli = Cli::parse();
  match cli.command {
    Command::Generate(args) => generate(args),
  }
}

fn generate(args: GenerateArgs) -> Result<(), String> {
  let root = repository_root();
  let output = args.output.map_or_else(
    || root.join(ARTIFACT_RELATIVE_PATH),
    |path| absolutize(&root, path),
  );
  let bytes = build_artifact(&root)?;
  if args.check {
    let existing = fs::read(&output)
      .map_err(|error| format!("cannot read {} for --check: {error}", output.display()))?;
    if existing != bytes {
      return Err(format!(
        "{} is stale; run `cargo run -p blitz-precompiler -- generate`",
        output.display()
      ));
    }
    println!("checked {} ({} bytes)", output.display(), bytes.len());
  } else {
    write_atomically(&output, &bytes)?;
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
  let records = sorted_records(&loader)?;
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
  let mut next_binding_id = 0_u32;
  for record in &records {
    let record = record.record.borrow();
    let mut exports = record.exports.iter().collect::<Vec<_>>();
    exports.sort_by(|left, right| left.0.cmp(right.0));
    for (_, slot) in exports {
      let key = BindingKey(Rc::as_ptr(slot));
      binding_ids.entry(key).or_insert_with(|| {
        let id = next_binding_id;
        next_binding_id = next_binding_id.saturating_add(1);
        id
      });
    }
  }

  let mut modules = Vec::with_capacity(records.len());
  for record_view in &records {
    let record = record_view.record.borrow();
    let input = input_by_specifier
      .get(&record.specifier)
      .ok_or_else(|| format!("unmanifested constructed module {}", record.specifier))?;
    let dependencies = record_dependencies(&record, &index_by_specifier)?;
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

    // Namespace imports compile to the validated `(module_index, "*")`
    // descriptor. The VM creates one live namespace object per module-table
    // entry, so they deliberately do not consume scalar graph binding IDs.
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
      specifier: record.specifier.clone(),
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

  let mut source_manifest_hasher = Sha256::new();
  let canonical_effect_manifest = canonical_manifest_bytes(&effect_manifest_bytes);
  let canonical_stdlib_manifest = canonical_manifest_bytes(&stdlib_manifest_bytes);
  source_manifest_hasher.update((canonical_effect_manifest.len() as u64).to_le_bytes());
  source_manifest_hasher.update(canonical_effect_manifest.as_ref());
  source_manifest_hasher.update((canonical_stdlib_manifest.len() as u64).to_le_bytes());
  source_manifest_hasher.update(canonical_stdlib_manifest.as_ref());
  let graph = PrelinkedModuleGraph {
    format_version: denko::prelinked::PRELINKED_MODULE_GRAPH_VERSION,
    engine_bytecode_version: BytecodeVersion::RiscV2,
    package_name: "effect+blitz-stdlib".to_string(),
    package_version: format!("effect-{EFFECT_VERSION}+runtime-1"),
    source_manifest_hash: source_manifest_hasher.finalize().into(),
    modules,
    binding_count: next_binding_id,
    public_roots: public_root_indices,
  };
  graph
    .validate()
    .map_err(|error| format!("generated graph validation failed: {error}"))?;
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

struct RecordView {
  record: Rc<RefCell<ModuleRecord>>,
}

fn sorted_records(loader: &ModuleLoader) -> Result<Vec<RecordView>, String> {
  let mut records = loader
    .map
    .values()
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
    if !matches!(
      record.record.borrow().payload,
      ModuleRecordPayload::Source { .. }
    ) {
      return Err(format!(
        "unexpected prelinked source record {} during generation",
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
  let chunk = compile_oxc_with_ctx(session.program(), Some(&context), session.source())
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
) -> Result<Vec<usize>, String> {
  let ModuleRecordPayload::Source { dependencies, .. } = &record.payload else {
    return Err(format!("{} is not source-backed", record.specifier));
  };
  let mut seen = HashSet::new();
  dependencies
    .iter()
    .filter(|specifier| seen.insert((*specifier).clone()))
    .map(|specifier| {
      index_by_specifier
        .get(specifier)
        .copied()
        .ok_or_else(|| {
          format!(
            "closed graph violation: {} -> {specifier}",
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
    let dependencies = record_dependencies(&record, &index_by_specifier)?
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
    .expect("blitz-precompiler is nested below the repository root")
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
