// SPDX-License-Identifier: MPL-2.0
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
use serde::Deserialize;
use std::collections::HashMap;

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
pub fn public_declaration_sources() -> Vec<(String, String)> {
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

/// Return upstream documentation for every curated public Effect symbol.
#[must_use]
pub fn public_documentation() -> Vec<(String, String, String)> {
  #[derive(Deserialize)]
  struct Documentation {
    format: u8,
    modules: HashMap<String, HashMap<String, String>>,
  }

  let documentation: Documentation =
    serde_json::from_str(EFFECT_DOCUMENTATION_JSON).expect("generated Effect documentation");
  assert_eq!(documentation.format, 1, "Effect documentation format");
  documentation
    .modules
    .into_iter()
    .flat_map(|(specifier, symbols)| {
      symbols
        .into_iter()
        .map(move |(symbol, documentation)| (specifier.clone(), symbol, documentation))
    })
    .collect()
}

#[cfg(test)]
mod tests {
  use super::*;
  use blitz::value::{ObjectData, PropertyDescriptor, PropertyKey, Value};
  use std::cell::RefCell;
  use std::path::Path;
  use std::rc::Rc;
  use std::sync::atomic::{AtomicUsize, Ordering};

  static NEXT_EFFECT_COMPATIBILITY_TEST_ID: AtomicUsize = AtomicUsize::new(0);

  fn run_effect_compatibility_entry(
    source: impl Into<String>,
    assertion: impl FnOnce(&blitz::Vm) + Send + 'static,
  ) {
    let source = source.into();
    let test_id = NEXT_EFFECT_COMPATIBILITY_TEST_ID.fetch_add(1, Ordering::Relaxed);
    std::thread::Builder::new()
      .name("effect-compatibility".to_string())
      .stack_size(16 * 1024 * 1024)
      .spawn(move || {
        let globals = Rc::new(RefCell::new(ObjectData::new()));
        globals.borrow_mut().properties.insert(
          PropertyKey::String("globalThis".to_string()),
          PropertyDescriptor::data(Value::Object(Rc::clone(&globals))),
        );
        let mut vm = blitz::Vm::new(globals);
        let _runtime = crate::host::install_runtime(
          &mut vm,
          crate::host::RuntimeOptions::from_host(vec!["blitz-effect-compatibility".to_string()]),
        );
        assert_eq!(
          vm.module_loader_pipeline_snapshot(),
          blitz::module::ModuleLoaderPipelineSnapshot::default(),
          "installing bundled Effect roots must not use the source pipeline"
        );

        let directory = std::env::temp_dir().join(format!(
          "blitz-effect-compatibility-{}-{}",
          std::process::id(),
          test_id
        ));
        std::fs::create_dir_all(&directory).expect("create Effect compatibility directory");
        let entry = directory.join("entry.mjs");
        std::fs::write(&entry, source).expect("write Effect compatibility entry");
        let loader_handle = vm.module_loader_handle();
        {
          let mut loader = loader_handle.borrow_mut();
          loader
            .construct(&directory, "./entry.mjs")
            .expect("construct Effect compatibility graph");
          loader
            .instantiate_all()
            .expect("instantiate Effect compatibility graph");
        }
        blitz::module::evaluate_entry_shared(&loader_handle, &entry, &mut vm, false)
          .expect("execute Effect compatibility graph");
        std::fs::remove_dir_all(&directory).expect("remove Effect compatibility directory");

        assert_eq!(
          vm.module_loader_pipeline_snapshot(),
          blitz::module::ModuleLoaderPipelineSnapshot {
            source_parses: 1,
            import_extraction_parses: 1,
            source_linking_passes: 2,
            source_typechecks: 0,
            source_compilations: 1,
          },
          "only the compatibility entry may use source tooling"
        );
        assertion(&vm);
      })
      .expect("spawn Effect compatibility thread")
      .join()
      .expect("Effect compatibility thread succeeds");
  }

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
  fn generated_effect_documentation_covers_curated_functions() {
    let documentation = public_documentation();
    assert!(
      documentation
        .iter()
        .any(|(specifier, symbol, text)| {
          specifier == "effect/Effect"
            && symbol == "succeed"
            && text.contains("always succeeds with a given value")
        })
    );
    assert!(
      documentation
        .iter()
        .any(|(specifier, symbol, text)| {
          specifier == "effect/Layer"
            && symbol == "effect"
            && text.contains("Constructs a layer from the specified effect")
        })
    );
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
          blitz::js!(
            module,
            r#"
        import * as Effect from 'effect/Effect';
        const fiber = Effect.runFork(
          Effect.map(Effect.succeed(41), value => value + 1)
        );
        const exit = fiber.unsafePoll();
        globalThis.effect_result = exit === null ? undefined : exit.value;
      "#
          ),
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
      blitz::js!(
        module,
        r#"
        import * as Effect from 'effect/Effect';
        import * as Ref from 'effect/Ref';
        globalThis.effect_typecheck =
          typeof Effect.try === 'function' &&
          typeof Effect.catchTag === 'function' &&
          typeof Effect.runSync === 'function' &&
          typeof Ref.make === 'function';
      "#
      ),
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

    assert_eq!(vm.get_global("effect_typecheck"), Some(Value::Bool(true)));
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
      blitz::js!(
        module,
        "import * as Facade from 'blitz:app'; globalThis.facade_type = typeof Facade;"
      ),
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
  fn expanded_generated_public_roots_are_importable() {
    let roots = public_declaration_sources();
    let imports = roots
      .iter()
      .enumerate()
      .map(|(index, (specifier, _))| {
        blitz::js!(
          format(module),
          "import * as root{index} from '{specifier}';\nglobalThis.effect_public_root_count += typeof root{index} === 'object' ? 1 : 0;\n"
        )
      })
      .collect::<String>();
    let source = blitz::js!(
      format(module),
      "globalThis.effect_public_root_count = 0;\n{imports}"
    );
    let expected_count = roots.len();
    run_effect_compatibility_entry(source, move |vm| {
      assert_eq!(
        vm.get_global("effect_public_root_count"),
        Some(Value::Number(expected_count as f64))
      );
    });
  }

  #[test]
  fn effect_root_and_fiber_await_preserve_identity() {
    run_effect_compatibility_entry(
      blitz::js!(
        module,
        r#"
        import * as Root from 'effect';
        import * as Effect from 'effect/Effect';
        import * as Fiber from 'effect/Fiber';
        import * as Exit from 'effect/Exit';
        const fiber = Effect.runFork(Effect.succeed(41));
        const awaitedFiber = Effect.runFork(Fiber.await(fiber));
        const awaited = awaitedFiber.unsafePoll();
        globalThis.effect_root_identity = Root.Effect.succeed === Effect.succeed;
        globalThis.fiber_root_identity = Root.Fiber.await === Fiber.await;
        globalThis.fiber_is_real = Fiber.isFiber(fiber) && Fiber.isRuntimeFiber(fiber);
        globalThis.fiber_await_observed =
          awaited !== null &&
          Exit.isSuccess(awaited) &&
          Exit.isSuccess(awaited.value) &&
          awaited.value.value === 41;
      "#
      ),
      |vm| {
        assert_eq!(
          vm.get_global("effect_root_identity"),
          Some(Value::Bool(true))
        );
        assert_eq!(
          vm.get_global("fiber_root_identity"),
          Some(Value::Bool(true))
        );
        assert_eq!(vm.get_global("fiber_is_real"), Some(Value::Bool(true)));
        assert_eq!(
          vm.get_global("fiber_await_observed"),
          Some(Value::Bool(true))
        );
      },
    );
  }

  #[test]
  fn ref_and_synchronous_error_boundaries_are_usable() {
    run_effect_compatibility_entry(
      blitz::js!(
        module,
        r#"
        import * as Root from 'effect';
        import * as Effect from 'effect/Effect';
        import * as Exit from 'effect/Exit';
        import * as Ref from 'effect/Ref';

        const counter = Effect.runSync(Ref.make(1));
        Effect.runSync(Ref.update(counter, value => value + 1));
        const modified = Effect.runSync(
          Ref.modify(counter, value => [value * 2, value + 3])
        );
        const recovered = Effect.runSync(
          Effect.try({
            try: () => { throw new Error('host failure'); },
            catch: cause => ({ _tag: 'BoundaryError', cause })
          }).pipe(
            Effect.catchTag('BoundaryError', () => Effect.succeed('recovered'))
          )
        );
        const failed = Effect.runSyncExit(
          Effect.try({
            try: () => { throw new Error('expected'); },
            catch: () => ({ _tag: 'ExpectedFailure' })
          })
        );
        const sequenced = Effect.runSync(
          Effect.zipRight(Effect.succeed('left'), Effect.as(Effect.succeed(1), 'right'))
        );

        globalThis.effect_ref_root_identity = Root.Ref.make === Ref.make;
        globalThis.effect_ref_value = Effect.runSync(Ref.get(counter));
        globalThis.effect_ref_modified = modified;
        globalThis.effect_boundary_recovered = recovered;
        globalThis.effect_boundary_failed = Exit.isFailure(failed);
        globalThis.effect_boundary_sequenced = sequenced;
      "#
      ),
      |vm| {
        assert_eq!(
          vm.get_global("effect_ref_root_identity"),
          Some(Value::Bool(true))
        );
        assert_eq!(vm.get_global("effect_ref_value"), Some(Value::Number(5.0)));
        assert_eq!(
          vm.get_global("effect_ref_modified"),
          Some(Value::Number(4.0))
        );
        assert_eq!(
          vm.get_global("effect_boundary_recovered"),
          Some(Value::String("recovered".to_string()))
        );
        assert_eq!(
          vm.get_global("effect_boundary_failed"),
          Some(Value::Bool(true))
        );
        assert_eq!(
          vm.get_global("effect_boundary_sequenced"),
          Some(Value::String("right".to_string()))
        );
      },
    );
  }

  #[test]
  fn exit_and_cause_observation_preserve_failure_shape() {
    run_effect_compatibility_entry(
      blitz::js!(
        module,
        r#"
        import * as Effect from 'effect/Effect';
        import * as Exit from 'effect/Exit';
        import * as Cause from 'effect/Cause';
        const outer = Effect.runFork(Effect.exit(Effect.fail('boom'))).unsafePoll();
        const observed = outer === null ? Exit.fail('missing') : outer.value;
        const failure = Cause.failureOption(observed.cause);
        globalThis.exit_is_exit = Exit.isExit(observed);
        globalThis.exit_is_failure = Exit.isFailure(observed);
        globalThis.cause_is_cause = Cause.isCause(observed.cause);
        globalThis.cause_is_failure = Cause.isFailure(observed.cause);
        globalThis.cause_failure_observed =
          failure._tag === 'Some' && failure.value === 'boom';
        globalThis.exit_cause_observed =
          Exit.causeOption(observed)._tag === 'Some';
      "#
      ),
      |vm| {
        for name in [
          "exit_is_exit",
          "exit_is_failure",
          "cause_is_cause",
          "cause_is_failure",
          "cause_failure_observed",
          "exit_cause_observed",
        ] {
          assert_eq!(vm.get_global(name), Some(Value::Bool(true)), "{name}");
        }
      },
    );
  }

  #[test]
  fn context_tags_keep_key_identity_and_service_identity() {
    run_effect_compatibility_entry(
      blitz::js!(
        module,
        r#"
        import * as Effect from 'effect/Effect';
        import * as Context from 'effect/Context';
        const firstTag = Context.GenericTag('blitz/effect-compatibility/service');
        const equivalentTag = Context.GenericTag('blitz/effect-compatibility/service');
        const service = { answer: 7 };
        const context = Context.make(firstTag, service);
        const observed = Effect.runFork(
          Effect.provide(
            Effect.contextWith(current => Context.get(current, equivalentTag)),
            context
          )
        ).unsafePoll();
        globalThis.context_tag_key_identity = firstTag.key === equivalentTag.key;
        globalThis.context_tag_shape = Context.isTag(firstTag);
        globalThis.context_service_identity =
          observed !== null && observed.value === service;
      "#
      ),
      |vm| {
        assert_eq!(
          vm.get_global("context_tag_key_identity"),
          Some(Value::Bool(true))
        );
        assert_eq!(vm.get_global("context_tag_shape"), Some(Value::Bool(true)));
        assert_eq!(
          vm.get_global("context_service_identity"),
          Some(Value::Bool(true))
        );
      },
    );
  }

  #[test]
  fn layers_build_and_provide_context() {
    run_effect_compatibility_entry(
      blitz::js!(
        module,
        r#"
        import * as Effect from 'effect/Effect';
        import * as Context from 'effect/Context';
        import * as Exit from 'effect/Exit';
        import * as Layer from 'effect/Layer';
        const tag = Context.GenericTag('blitz/effect-compatibility/layer');
        const layer = Layer.succeed(tag, { answer: 13 });
        const built = Effect.runFork(Effect.scoped(Layer.build(layer))).unsafePoll();
        globalThis.layer_built = built !== null && Exit.isSuccess(built);
      "#
      ),
      |vm| {
        assert_eq!(vm.get_global("layer_built"), Some(Value::Bool(true)));
      },
    );
  }

  #[test]
  fn scoped_acquire_release_receives_success_exit() {
    run_effect_compatibility_entry(
      blitz::js!(
        module,
        r#"
        import * as Effect from 'effect/Effect';
        import * as Exit from 'effect/Exit';
        let acquired = 0;
        let released = 0;
        let releaseSawExit = false;
        let releaseSawSuccess = false;
        const resource = Effect.acquireRelease(
          Effect.sync(() => {
            acquired += 1;
            return 'resource';
          }),
          (value, exit) => Effect.sync(() => {
            released += value === 'resource' ? 1 : 0;
            releaseSawExit = Exit.isExit(exit);
            releaseSawSuccess = Exit.isSuccess(exit);
          })
        );
        const result = Effect.runFork(
          Effect.exit(Effect.scoped(Effect.map(resource, value => value + '-used')))
        ).unsafePoll();
        globalThis.scoped_result =
          result !== null && Exit.isSuccess(result) && result.value.value === 'resource-used';
        globalThis.scoped_acquired = acquired === 1;
        globalThis.scoped_released = released === 1;
        globalThis.scoped_finalizer_exit = releaseSawExit && releaseSawSuccess;
      "#
      ),
      |vm| {
        assert_eq!(vm.get_global("scoped_result"), Some(Value::Bool(true)));
        assert_eq!(vm.get_global("scoped_acquired"), Some(Value::Bool(true)));
        assert_eq!(vm.get_global("scoped_released"), Some(Value::Bool(true)));
        assert_eq!(
          vm.get_global("scoped_finalizer_exit"),
          Some(Value::Bool(true))
        );
      },
    );
  }

  #[test]
  fn effect_file_system_reads_writes_and_reports_typed_failures() {
    let path = std::env::temp_dir().join(format!(
      "blitz-effect-fs-{}-{}.txt",
      std::process::id(),
      NEXT_EFFECT_COMPATIBILITY_TEST_ID.fetch_add(1, Ordering::Relaxed)
    ));
    let missing = path.with_extension("missing");
    let source = blitz::js!(
      format(module),
      r#"
        import * as Cause from 'effect/Cause';
        import * as Effect from 'effect/Effect';
        import * as Fs from 'blitz:fs';
        import * as Runtime from 'blitz:runtime';

        const path = {};
        const missing = {};
        const program = Fs.writeTextFile(path, 'hello filesystem').pipe(
          Effect.zipRight(Fs.readTextFile(path))
        );
        globalThis.fs_text = Effect.runSync(Runtime.provideLive(program));
        const missingExit = Effect.runSyncExit(
          Runtime.provideLive(Fs.readTextFile(missing))
        );
        const failure = missingExit._tag === 'Failure'
          ? Cause.failureOption(missingExit.cause)
          : {{ _tag: 'None' }};
        globalThis.fs_typed_error = failure._tag === 'Some' &&
          failure.value._tag === 'FileSystemError' &&
          failure.value.reason === 'NotFound';
      "#,
      serde_json::to_string(&path.to_string_lossy()).expect("serialize fs path"),
      serde_json::to_string(&missing.to_string_lossy()).expect("serialize missing path")
    );
    run_effect_compatibility_entry(source, |vm| {
      assert_eq!(
        vm.get_global("fs_text"),
        Some(Value::String("hello filesystem".to_string()))
      );
      assert_eq!(vm.get_global("fs_typed_error"), Some(Value::Bool(true)));
    });
    let _result = std::fs::remove_file(path);
  }

  #[test]
  fn effect_terminal_operations_use_the_live_service() {
    run_effect_compatibility_entry(
      blitz::js!(
        module,
        r#"
        import * as Effect from 'effect/Effect';
        import * as Terminal from 'blitz:terminal';
        import * as Runtime from 'blitz:runtime';

        const program = Terminal.setPrompt('[effect] > ').pipe(
          Effect.zipRight(Terminal.close()),
          Effect.as('closed')
        );
        globalThis.terminal_result = Effect.runSync(Runtime.provideLive(program));
      "#
      ),
      |vm| {
        assert_eq!(
          vm.get_global("terminal_result"),
          Some(Value::String("closed".to_string()))
        );
      },
    );
  }

  #[test]
  fn app_transport_is_lazy_typed_and_scoped() {
    run_effect_compatibility_entry(
      blitz::js!(
        module,
        r#"
        import * as Cause from 'effect/Cause';
        import * as Effect from 'effect/Effect';
        import * as Exit from 'effect/Exit';
        import * as Fiber from 'effect/Fiber';
        import { App } from 'blitz:app';

        const transport = globalThis.__blitz_host_v1.appTransport;
        const options = { hostname: '127.0.0.1', port: 0, rpcPath: '/rpc' };
        const app = App.make({ rpc: { _tag: 'BlitzRouter' } });
        const lazyServe = app.serve({ hostname: '127.0.0.1', port: 0 });
        const server = Effect.runSync(transport.openAppServer(options));
        const pending = Effect.runFork(transport.awaitShutdown(server));
        const boundPort = Number(server.localAddr.split(':')[1]);
        const bindFailure = Effect.runSyncExit(
          transport.openAppServer({
            hostname: '127.0.0.1',
            port: boundPort,
            rpcPath: '/rpc'
          })
        );
        const bindFailureOption = Exit.isFailure(bindFailure)
          ? Cause.failureOption(bindFailure.cause)
          : { _tag: 'None' };
        const scopedAddress = Effect.runSync(
          Effect.scoped(
            Effect.flatMap(
              Effect.acquireRelease(
                transport.openAppServer(options),
                acquired => transport.closeAppServer(acquired)
              ),
              acquired => Effect.succeed(acquired.localAddr)
            )
          )
        );
        Effect.runSync(transport.closeAppServer(server));
        Effect.runSync(transport.closeAppServer(server));

        const appFiber = Effect.runFork(
          app.serve({
            hostname: '127.0.0.1',
            port: boundPort,
            rpcPath: '/rpc'
          })
        );
        const interruptedApp = Effect.runSync(Fiber.interrupt(appFiber));
        const rebound = Effect.runSync(
          transport.openAppServer({
            hostname: '127.0.0.1',
            port: boundPort,
            rpcPath: '/rpc'
          })
        );
        Effect.runSync(transport.closeAppServer(rebound));

        const invalid = Effect.runSyncExit(
          app.serve({
            hostname: '127.0.0.1',
            port: 0,
            rpcPath: '/'
          })
        );
        const invalidFailure = Exit.isFailure(invalid)
          ? Cause.failureOption(invalid.cause)
          : { _tag: 'None' };

        globalThis.app_serve_is_lazy_effect = typeof lazyServe.pipe === 'function';
        globalThis.app_transport_address = server.localAddr;
        globalThis.app_transport_await_is_non_blocking =
          typeof pending.unsafePoll === 'function';
        globalThis.app_transport_scoped_address = scopedAddress;
        globalThis.app_transport_invalid_is_typed =
          invalidFailure._tag === 'Some' &&
          invalidFailure.value._tag === 'ServeError' &&
          invalidFailure.value.reason === 'InvalidOptions';
        globalThis.app_transport_bind_is_typed =
          bindFailureOption._tag === 'Some' &&
          bindFailureOption.value._tag === 'ServeError' &&
          bindFailureOption.value.reason === 'Bind';
        globalThis.app_transport_interrupt_released =
          Exit.isInterrupted(interruptedApp) && rebound.localAddr === server.localAddr;
      "#
      ),
      |vm| {
        assert_eq!(
          vm.get_global("app_serve_is_lazy_effect"),
          Some(Value::Bool(true))
        );
        assert!(matches!(
          vm.get_global("app_transport_address"),
          Some(Value::String(address)) if address.starts_with("127.0.0.1:")
        ));
        assert_eq!(
          vm.get_global("app_transport_await_is_non_blocking"),
          Some(Value::Bool(true))
        );
        assert!(matches!(
          vm.get_global("app_transport_scoped_address"),
          Some(Value::String(address)) if address.starts_with("127.0.0.1:")
        ));
        assert_eq!(
          vm.get_global("app_transport_invalid_is_typed"),
          Some(Value::Bool(true))
        );
        assert_eq!(
          vm.get_global("app_transport_bind_is_typed"),
          Some(Value::Bool(true))
        );
        assert_eq!(
          vm.get_global("app_transport_interrupt_released"),
          Some(Value::Bool(true))
        );
      },
    );
  }

  #[test]
  fn abort_controller_signal_is_single_shot_and_observable() {
    run_effect_compatibility_entry(
      blitz::js!(
        module,
        r#"
        const controller = new AbortController();
        let calls = 0;
        controller.signal.__blitz_abort_listeners = { length: Infinity };
        controller.signal.addEventListener('abort', () => calls++, { once: true });
        controller.abort('stop');
        controller.abort('ignored');
        const detachedAbort = AbortSignal.abort;
        const detachedSignal = detachedAbort('detached');
        globalThis.abort_signal_detached_reason = detachedSignal.reason;
        globalThis.abort_signal_aborted = controller.signal.aborted;
        globalThis.abort_signal_reason = controller.signal.reason;
        globalThis.abort_listener_calls = calls;
      "#
      ),
      |vm| {
        assert_eq!(
          vm.get_global("abort_signal_detached_reason"),
          Some(Value::String("detached".to_string()))
        );
        assert_eq!(
          vm.get_global("abort_signal_aborted"),
          Some(Value::Bool(true))
        );
        assert_eq!(
          vm.get_global("abort_signal_reason"),
          Some(Value::String("stop".to_string()))
        );
        assert_eq!(
          vm.get_global("abort_listener_calls"),
          Some(Value::Number(1.0))
        );
      },
    );
  }

  #[test]
  fn synchronous_effect_async_resumes_without_host_timer() {
    run_effect_compatibility_entry(
      blitz::js!(
        module,
        r#"
        import * as Effect from 'effect/Effect';
        import * as Exit from 'effect/Exit';
        let callbackCalls = 0;
        const effect = Effect.async((resume, signal) => {
          callbackCalls += 1;
          resume(Effect.succeed(73));
          globalThis.effect_async_signal_supplied = signal !== undefined;
        });
        const result = Effect.runFork(Effect.exit(effect)).unsafePoll();
        globalThis.effect_async_result =
          result !== null && Exit.isSuccess(result) && result.value.value === 73;
        globalThis.effect_async_callback_once = callbackCalls === 1;
      "#
      ),
      |vm| {
        assert_eq!(
          vm.get_global("effect_async_result"),
          Some(Value::Bool(true))
        );
        assert_eq!(
          vm.get_global("effect_async_callback_once"),
          Some(Value::Bool(true))
        );
      },
    );
  }

  #[test]
  fn fiber_interrupt_cancellation_uses_supported_scheduler_path() {
    run_effect_compatibility_entry(
      blitz::js!(
        module,
        r#"
        import * as Effect from 'effect/Effect';
        import * as Exit from 'effect/Exit';
        import * as Fiber from 'effect/Fiber';
        import * as Cause from 'effect/Cause';
        const fiber = Effect.runFork(Effect.never);
        const interrupted = Effect.runFork(Fiber.interrupt(fiber)).unsafePoll();
        const exit = interrupted === null ? Exit.succeed(undefined) : interrupted.value;
        globalThis.fiber_cancelled =
          interrupted !== null &&
          Exit.isSuccess(interrupted) &&
          Exit.isInterrupted(exit) &&
          Cause.isInterrupted(exit.cause);
      "#
      ),
      |vm| {
        assert_eq!(vm.get_global("fiber_cancelled"), Some(Value::Bool(true)));
      },
    );
  }

  #[test]
  fn runtime_installation_has_zero_bundled_pipeline_counters() {
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
      blitz::module::ModuleLoaderPipelineSnapshot::default()
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
