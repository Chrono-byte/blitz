//! Metadata for the pinned Effect and Blitz stdlib module graphs.
//!
//! Upstream Effect and Blitz stdlib files remain generated assets owned by the
//! asset-generation workspace. Runtime registration is provided by the
//! prelinked artifact.

/// The pinned manifests are policy inputs, not source-module registrations.
pub(crate) const EFFECT_MANIFEST_BYTES: &[u8] = include_bytes!(concat!(
  env!("CARGO_MANIFEST_DIR"),
  "/assets/effect/3.16.7/manifest.json"
));
/// Generated Blitz stdlib manifest pinned by the runtime graph wrapper.
pub(crate) const STDLIB_MANIFEST_BYTES: &[u8] = include_bytes!(concat!(
  env!("CARGO_MANIFEST_DIR"),
  "/assets/stdlib/manifest.json"
));

/// One generated Effect module embedded in the runtime binary.
#[derive(Clone, Copy, Debug)]
pub struct EmbeddedEffectModule {
  /// Canonical public or private virtual-module specifier.
  pub specifier: &'static str,
  /// Generated upstream ESM source.
  pub source: &'static str,
  /// Curated declaration source for supported public entries only.
  pub declaration: Option<&'static str>,
  /// Generator-recorded source SHA-256.
  pub source_sha256: &'static str,
  /// Generator-recorded declaration SHA-256 for a public entry.
  pub declaration_sha256: Option<&'static str>,
}

include!(concat!(
  env!("CARGO_MANIFEST_DIR"),
  "/assets/effect/3.16.7/embedded.rs"
));

#[allow(dead_code, missing_docs)]
mod generated_stdlib {
  include!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/stdlib/embedded.rs"
  ));
}

use generated_stdlib::EMBEDDED_BLITZ_STDLIB_MODULES;

/// The declarations intentionally supported by the first facade milestone.
pub const BLITZ_MATCH_SUPPORTED_EXPORTS: &[&str] =
  &["value", "type", "valueTags", "typeTags", "exhaustive"];

/// Look up declarations without registering bundled JavaScript as source.
#[must_use]
pub fn declaration_source(specifier: &str) -> Option<&'static str> {
  EMBEDDED_EFFECT_MODULES
    .iter()
    .find(|module| module.specifier == specifier)
    .and_then(|module| module.declaration)
    .or_else(|| {
      EMBEDDED_BLITZ_STDLIB_MODULES
        .iter()
        .find(|module| module.specifier == specifier)
        .and_then(|module| module.declaration)
    })
}

/// Return generated declarations for every public Effect and stdlib root.
///
/// The engine validates that each returned target belongs to the graph's
/// public roots as part of the atomic registration call.
#[must_use]
pub(crate) fn public_declaration_sources() -> Vec<(String, String)> {
  EMBEDDED_EFFECT_MODULES
    .iter()
    .filter_map(|module| {
      module
        .declaration
        .map(|source| (module.specifier.to_string(), source.to_string()))
    })
    .chain(
      EMBEDDED_BLITZ_STDLIB_MODULES
        .iter()
        .filter_map(|module| {
          (module.visibility == "public")
            .then_some(module.declaration)
            .flatten()
            .map(|source| (module.specifier.to_string(), source.to_string()))
        }),
    )
    .collect()
}

#[cfg(test)]
mod tests {
  use super::*;
  use blitz::value::{ObjectData, PropertyDescriptor, PropertyKey, Value};
  use std::cell::RefCell;
  use std::path::Path;
  use std::rc::Rc;

  #[test]
  fn generated_manifests_cover_embedded_assets() {
    #[derive(serde::Deserialize)]
    struct Manifest {
      entries: Vec<Entry>,
    }
    #[derive(serde::Deserialize)]
    struct Entry {
      specifier: String,
      source: Asset,
      declaration: Option<Asset>,
    }
    #[derive(serde::Deserialize)]
    struct Asset {
      sha256: String,
    }

    let effect = serde_json::from_slice::<Manifest>(EFFECT_MANIFEST_BYTES)
      .expect("generated Effect manifest is valid JSON");
    assert_eq!(effect.entries.len(), EMBEDDED_EFFECT_MODULES.len());
    for module in EMBEDDED_EFFECT_MODULES {
      let entry = effect
        .entries
        .iter()
        .find(|entry| entry.specifier == module.specifier)
        .expect("every embedded Effect module is in its generated manifest");
      assert_eq!(entry.source.sha256, module.source_sha256);
      assert_eq!(
        entry
          .declaration
          .as_ref()
          .map(|asset| asset.sha256.as_str()),
        module.declaration_sha256
      );
    }

    let stdlib = serde_json::from_slice::<Manifest>(STDLIB_MANIFEST_BYTES)
      .expect("generated stdlib manifest is valid JSON");
    assert_eq!(stdlib.entries.len(), EMBEDDED_BLITZ_STDLIB_MODULES.len());
    for module in EMBEDDED_BLITZ_STDLIB_MODULES {
      let entry = stdlib
        .entries
        .iter()
        .find(|entry| entry.specifier == module.specifier)
        .expect("every embedded stdlib module is in its generated manifest");
      assert_eq!(entry.source.sha256, module.source_sha256);
      assert_eq!(
        entry
          .declaration
          .as_ref()
          .map(|asset| asset.sha256.as_str()),
        module.declaration_sha256
      );
    }
  }

  #[test]
  fn declaration_lookup_uses_generated_public_assets() {
    for module in EMBEDDED_EFFECT_MODULES
      .iter()
      .filter(|module| module.declaration.is_some())
    {
      assert_eq!(declaration_source(module.specifier), module.declaration);
    }
    for module in EMBEDDED_BLITZ_STDLIB_MODULES
      .iter()
      .filter(|module| module.declaration.is_some())
    {
      assert_eq!(declaration_source(module.specifier), module.declaration);
    }
    let private_module = EMBEDDED_BLITZ_STDLIB_MODULES
      .iter()
      .find(|module| module.declaration.is_none())
      .expect("generated stdlib manifest contains a private module");
    assert!(declaration_source(private_module.specifier).is_none());
    assert!(declaration_source("effect/not-generated").is_none());
  }

  #[test]
  fn embedded_effect_assets_are_complete_generated_records() {
    assert!(!EMBEDDED_EFFECT_MODULES.is_empty());
    for asset in EMBEDDED_EFFECT_MODULES {
      assert!(!asset.source.is_empty());
      assert_eq!(asset.source_sha256.len(), 64);
      assert_eq!(
        asset.declaration.is_some(),
        asset.declaration_sha256.is_some()
      );
      if let Some(declaration) = asset.declaration {
        assert!(!declaration.is_empty());
        assert_eq!(
          asset
            .declaration_sha256
            .expect("declaration hash")
            .len(),
          64
        );
      }
    }
  }

  #[test]
  fn runtime_artifact_covers_generated_public_assets() {
    fn hash_bytes(value: &str) -> [u8; 32] {
      let mut result = [0; 32];
      for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        result[index] = u8::from_str_radix(
          std::str::from_utf8(pair).expect("generated hash is UTF-8"),
          16,
        )
        .expect("generated hash is hexadecimal");
      }
      result
    }

    let bundle = crate::runtime_graph::validate_embedded_runtime_bundle()
      .expect("embedded runtime artifact is valid and pinned");
    let artifact_modules = &bundle.manifest.modules;
    for module in EMBEDDED_EFFECT_MODULES
      .iter()
      .filter(|module| module.declaration.is_some())
    {
      let artifact = artifact_modules
        .iter()
        .find(|entry| entry.specifier == module.specifier)
        .expect("every generated public Effect asset is in the runtime artifact");
      assert_eq!(artifact.source_sha256, hash_bytes(module.source_sha256));
      assert_eq!(artifact.origin, crate::runtime_graph::ModuleOrigin::Effect);
      assert_eq!(
        artifact.visibility,
        crate::runtime_graph::ModuleVisibility::Public
      );
    }
    for module in EMBEDDED_BLITZ_STDLIB_MODULES
      .iter()
      .filter(|module| module.visibility == "public")
    {
      let artifact = artifact_modules
        .iter()
        .find(|entry| entry.specifier == module.specifier)
        .expect("every generated public stdlib asset is in the runtime artifact");
      assert_eq!(artifact.source_sha256, hash_bytes(module.source_sha256));
      assert_eq!(
        artifact.origin,
        crate::runtime_graph::ModuleOrigin::BlitzStdlib
      );
      assert_eq!(
        artifact.visibility,
        crate::runtime_graph::ModuleVisibility::Public
      );
    }
    for artifact in artifact_modules {
      if let Some(module) = EMBEDDED_EFFECT_MODULES
        .iter()
        .find(|module| module.specifier == artifact.specifier)
      {
        assert_eq!(artifact.source_sha256, hash_bytes(module.source_sha256));
        assert_eq!(
          artifact.declaration_sha256,
          module.declaration_sha256.map(hash_bytes)
        );
        assert_eq!(artifact.origin, crate::runtime_graph::ModuleOrigin::Effect);
        assert_eq!(
          artifact.visibility == crate::runtime_graph::ModuleVisibility::Public,
          module.declaration.is_some()
        );
      } else if let Some(module) = EMBEDDED_BLITZ_STDLIB_MODULES
        .iter()
        .find(|module| module.specifier == artifact.specifier)
      {
        assert_eq!(artifact.source_sha256, hash_bytes(module.source_sha256));
        assert_eq!(
          artifact.declaration_sha256,
          module.declaration_sha256.map(hash_bytes)
        );
        assert_eq!(
          artifact.origin,
          crate::runtime_graph::ModuleOrigin::BlitzStdlib
        );
        assert_eq!(
          artifact.visibility == crate::runtime_graph::ModuleVisibility::Public,
          module.visibility == "public"
        );
      } else {
        panic!(
          "runtime artifact module is absent from generated assets: {}",
          artifact.specifier
        );
      }
    }
  }

  #[test]
  fn effect_graph_executes_a_synchronous_effect() {
    std::thread::Builder::new()
      .name("effect-smoke".to_string())
      .stack_size(16 * 1024 * 1024)
      .spawn(|| {
        let globals = Rc::new(RefCell::new(ObjectData::new()));
        globals.borrow_mut().properties.insert(
          PropertyKey::String("globalThis".to_string()),
          PropertyDescriptor::data(Value::Object(Rc::clone(&globals))),
        );
        let mut vm = blitz::Vm::new(globals);
        let _runtime = crate::host::install_runtime(
          &mut vm,
          crate::host::RuntimeOptions::from_host(vec!["blitz-test".to_string()]),
        );
        assert_eq!(
          vm.module_loader_pipeline_snapshot(),
          blitz::module::ModuleLoaderPipelineSnapshot::default(),
          "runtime graph installation must not invoke source tooling"
        );

        let directory =
          std::env::temp_dir().join(format!("blitz-effect-smoke-{}", std::process::id()));
        std::fs::create_dir_all(&directory).expect("create Effect smoke directory");
        let entry = directory.join("entry.mjs");
        std::fs::write(
          &entry,
          r#"
        import * as Effect from 'effect/Effect';
        const fiber = Effect.runFork(
          Effect.map(Effect.succeed(41), value => value + 1)
        );
        const exit = fiber.unsafePoll();
        globalThis.effect_result = exit === null ? undefined : exit.value;
      "#,
        )
        .expect("write Effect smoke entry");

        let loader_handle = vm.module_loader_handle();
        {
          let mut loader = loader_handle.borrow_mut();
          loader
            .construct(&directory, "./entry.mjs")
            .expect("construct Effect graph");
          loader
            .instantiate_all()
            .expect("instantiate Effect graph");
        }
        blitz::module::evaluate_entry_shared(&loader_handle, &entry, &mut vm, false)
          .expect("execute Effect graph");
        std::fs::remove_dir_all(&directory).expect("remove Effect smoke directory");

        assert_eq!(vm.get_global("effect_result"), Some(Value::Number(42.0)));
        assert_eq!(
          vm.module_loader_pipeline_snapshot(),
          blitz::module::ModuleLoaderPipelineSnapshot {
            source_parses: 1,
            import_extraction_parses: 1,
            source_linking_passes: 2,
            source_typechecks: 0,
            source_compilations: 1,
          },
          "only the user entry may use source tooling"
        );
      })
      .expect("spawn Effect smoke thread")
      .join()
      .expect("Effect smoke thread succeeds");
  }

  #[test]
  fn source_importing_effect_retains_declaration_metadata_through_typecheck() {
    let globals = Rc::new(RefCell::new(ObjectData::new()));
    globals.borrow_mut().properties.insert(
      PropertyKey::String("globalThis".to_string()),
      PropertyDescriptor::data(Value::Object(Rc::clone(&globals))),
    );
    let mut vm = blitz::Vm::new(globals);
    let _runtime = crate::host::install_runtime(
      &mut vm,
      crate::host::RuntimeOptions::from_host(vec!["blitz-test".to_string()]),
    );

    let directory = std::env::temp_dir().join(format!(
      "blitz-effect-declaration-typecheck-{}",
      std::process::id()
    ));
    std::fs::create_dir_all(&directory).expect("create declaration test directory");
    let entry_path = directory.join("entry.mjs");
    std::fs::write(
      &entry_path,
      "import * as Effect from 'effect/Effect'; globalThis.effect_typecheck = typeof Effect.succeed;",
    )
    .expect("write declaration test entry");

    let loader_handle = vm.module_loader_handle();
    let entry = {
      let mut loader = loader_handle.borrow_mut();
      let entry = loader
        .construct(&directory, "./entry.mjs")
        .expect("construct source entry importing Effect");
      loader
        .instantiate_all()
        .expect("instantiate source entry importing Effect");
      let effect_record = loader
        .map
        .values()
        .find(|record| record.borrow().specifier == "effect/Effect")
        .expect("constructed Effect record");
      assert_eq!(
        effect_record
          .borrow()
          .declaration_source
          .as_deref(),
        declaration_source("effect/Effect")
      );
      entry
    };

    let entry_path = entry.borrow().path.clone();
    blitz::module::evaluate_entry_shared(&loader_handle, &entry_path, &mut vm, true)
      .expect("typecheck and evaluate source entry");
    std::fs::remove_dir_all(&directory).expect("remove declaration test directory");

    assert_eq!(
      vm.get_global("effect_typecheck"),
      Some(Value::String("function".to_string()))
    );
    assert_eq!(
      vm.module_loader_pipeline_snapshot()
        .source_typechecks,
      1,
      "the source importer takes the typecheck path while Effect stays prelinked"
    );
  }

  #[test]
  fn runtime_cutover_exposes_facades_without_source_backed_bundled_records() {
    let globals = Rc::new(RefCell::new(ObjectData::new()));
    let mut vm = blitz::Vm::new(Rc::clone(&globals));
    let _runtime = crate::host::install_runtime(
      &mut vm,
      crate::host::RuntimeOptions::from_host(vec!["blitz-test".to_string()]),
    );
    let directory =
      std::env::temp_dir().join(format!("blitz-runtime-cutover-{}", std::process::id()));
    std::fs::create_dir_all(&directory).expect("create cutover directory");
    let entry = directory.join("entry.mjs");
    std::fs::write(
      &entry,
      "import * as Facade from 'blitz:app'; globalThis.facade_type = typeof Facade;",
    )
    .expect("write facade entry");
    let loader_handle = vm.module_loader_handle();
    {
      let mut loader = loader_handle.borrow_mut();
      loader
        .construct(&directory, "./entry.mjs")
        .expect("construct facade entry");
      loader
        .instantiate_all()
        .expect("instantiate facade entry");
      assert!(loader.virtual_modules.is_empty());
      let app_record = loader
        .map
        .values()
        .find(|record| record.borrow().specifier == "blitz:app")
        .expect("blitz facade record");
      assert!(app_record.borrow().exports.contains_key("App"));
      assert!(
        loader
          .map
          .values()
          .filter(|record| {
            let specifier = &record.borrow().specifier;
            specifier.starts_with("blitz") || specifier.starts_with("effect")
          })
          .all(|record| record.borrow().is_prelinked())
      );
    }
    assert!(
      loader_handle
        .borrow()
        .map
        .values()
        .any(|record| record.borrow().specifier == "blitz:app")
    );
    std::fs::remove_dir_all(&directory).expect("remove cutover directory");
  }

  #[test]
  fn private_runtime_modules_are_not_user_importable() {
    let globals = Rc::new(RefCell::new(ObjectData::new()));
    let mut vm = blitz::Vm::new(globals);
    let _runtime = crate::host::install_runtime(
      &mut vm,
      crate::host::RuntimeOptions::from_host(vec!["blitz-test".to_string()]),
    );
    let loader_handle = vm.module_loader_handle();
    let mut loader = loader_handle.borrow_mut();
    let result = loader.construct(Path::new("."), "blitz-internal:app-transport");
    assert!(matches!(result, Err(error) if error.to_string().contains("private")));
  }

  #[test]
  fn raw_engine_has_no_runtime_modules() {
    let globals = Rc::new(RefCell::new(ObjectData::new()));
    let vm = blitz::Vm::new(globals);
    let loader_handle = vm.module_loader_handle();
    let mut loader = loader_handle.borrow_mut();
    assert!(loader.virtual_modules.is_empty());
    assert!(
      loader
        .construct(Path::new("."), "effect/Effect")
        .is_err()
    );
  }

  #[test]
  fn match_facade_reexports_upstream_runtime_and_curates_exports() {
    let facade = EMBEDDED_BLITZ_STDLIB_MODULES
      .iter()
      .find(|module| module.specifier == "blitz:match")
      .expect("generated stdlib manifest contains blitz:match");

    assert_eq!(facade.visibility, "public");
    assert!(facade.source.contains("effect/Match"));
    assert!(facade.declaration.is_some());
    assert!(BLITZ_MATCH_SUPPORTED_EXPORTS.contains(&"exhaustive"));
  }
}
