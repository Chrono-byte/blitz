use super::*;

#[test]
fn virtual_registry_keeps_stable_payload_hashes_and_declarations() {
  let first = VirtualModule::javascript(
    "test:module",
    crate::js!(module, "export const value = 1;"),
    SourceType::mjs(),
    Some(crate::js!(typescript, "export declare const value: number;").to_string()),
  );
  let second = VirtualModule::javascript(
    "test:module",
    crate::js!(module, "export const value = 1;"),
    SourceType::mjs(),
    Some(crate::js!(typescript, "export declare const value: number;").to_string()),
  );
  let changed = VirtualModule::javascript(
    "test:module",
    crate::js!(module, "export const value = 2;"),
    SourceType::mjs(),
    None,
  );

  assert_eq!(first.content_hash, second.content_hash);
  assert_ne!(first.content_hash, changed.content_hash);
  assert_eq!(
    first.declaration_source.as_deref(),
    Some(crate::js!(
      typescript,
      "export declare const value: number;"
    ))
  );

  let mut registry = VirtualModuleRegistry::new();
  registry
    .register(first)
    .expect("first registration succeeds");
  assert!(registry.register(second).is_err());
  assert_eq!(registry.len(), 1);
}

#[test]
fn virtual_modules_use_normal_graph_construction_and_evaluation() {
  let mut loader = ModuleLoader::new();
  loader
    .register_virtual_module(VirtualModule::javascript(
      "test:dependency",
      crate::js!(module, "export const answer = 42;"),
      SourceType::mjs(),
      None,
    ))
    .expect("dependency registration succeeds");
  loader
    .register_virtual_module(VirtualModule::javascript(
      "test:entry",
      crate::js!(
        module,
        "import { answer } from 'test:dependency'; globalThis.result = answer;"
      ),
      SourceType::mjs(),
      None,
    ))
    .expect("entry registration succeeds");

  let entry = loader
    .construct(Path::new("."), "test:entry")
    .expect("virtual entry constructs");
  assert_eq!(loader.map.len(), 2);
  loader
    .instantiate_all()
    .expect("virtual graph instantiates");

  let globals = Rc::new(RefCell::new(ObjectData::new()));
  let mut vm = Vm::new(globals);
  let entry_path = entry.borrow().path.clone();
  loader
    .evaluate_entry(&entry_path, &mut vm, false)
    .expect("virtual graph evaluates");
  assert_eq!(vm.get_global("result"), Some(Value::Number(42.0)));
}

#[test]
fn typecheck_distinguishes_source_imports_from_virtual_declarations() {
  let globals = Rc::new(RefCell::new(ObjectData::new()));
  let mut vm = Vm::new(globals);
  vm.register_virtual_module(VirtualModule::javascript(
    "app/value.js",
    crate::js!(module, "export const value = 41;"),
    SourceType::mjs(),
    None,
  ))
  .expect("source dependency registers");
  vm.register_virtual_module(VirtualModule::javascript(
    "effect/api",
    crate::js!(module, "export const answer = 42;"),
    SourceType::mjs(),
    Some(crate::js!(typescript, "export declare const answer: number;").to_string()),
  ))
  .expect("declared virtual module registers");
  vm.register_virtual_module(VirtualModule::javascript(
    "app/entry.js",
    crate::js!(
      module,
      "import './value.js'; import { answer } from 'effect/api'; globalThis.result = answer;"
    ),
    SourceType::mjs(),
    None,
  ))
  .expect("source entry registers");

  let loader = vm.module_loader_handle();
  let entry = loader
    .borrow_mut()
    .construct(Path::new("."), "app/entry.js")
    .expect("multi-source graph constructs");
  loader
    .borrow_mut()
    .instantiate_all()
    .expect("multi-source graph instantiates");
  let entry_path = entry.borrow().path.clone();
  loader
    .borrow_mut()
    .evaluate_entry(&entry_path, &mut vm, true)
    .expect("source imports and virtual declarations typecheck distinctly");
  assert_eq!(vm.get_global("result"), Some(Value::Number(42.0)));
}

#[test]
fn source_pipeline_snapshot_counts_source_work() {
  let globals = Rc::new(RefCell::new(ObjectData::new()));
  let mut vm = Vm::new(globals);
  vm.register_virtual_module(VirtualModule::javascript(
    "test:counters",
    crate::js!(module, "globalThis.result = 42;"),
    SourceType::mjs(),
    None,
  ))
  .expect("source module registers");
  let loader = vm.module_loader_handle();
  let entry = loader
    .borrow_mut()
    .construct(Path::new("."), "test:counters")
    .expect("source module constructs");
  loader
    .borrow_mut()
    .instantiate_all()
    .expect("source module instantiates");
  let entry_path = entry.borrow().path.clone();
  loader
    .borrow_mut()
    .evaluate_entry(&entry_path, &mut vm, true)
    .expect("source module evaluates");

  let expected = ModuleLoaderPipelineSnapshot {
    source_parses: 1,
    import_extraction_parses: 1,
    source_linking_passes: 2,
    source_typechecks: 1,
    source_compilations: 1,
  };
  assert_eq!(loader.borrow().pipeline_snapshot(), expected);
  assert_eq!(vm.module_loader_pipeline_snapshot(), expected);
  assert_eq!(vm.get_global("result"), Some(Value::Number(42.0)));
}

#[test]
fn source_pipeline_snapshot_counts_parallel_compilation() {
  let globals = Rc::new(RefCell::new(ObjectData::new()));
  let mut vm = Vm::new(globals);
  vm.register_virtual_module(VirtualModule::javascript(
    "test:parallel-dependency",
    crate::js!(module, "globalThis.dependencyLoaded = true;"),
    SourceType::mjs(),
    None,
  ))
  .expect("dependency module registers");
  vm.register_virtual_module(VirtualModule::javascript(
    "test:parallel-entry",
    crate::js!(
      module,
      "import 'test:parallel-dependency'; globalThis.parallelResult = 42;"
    ),
    SourceType::mjs(),
    None,
  ))
  .expect("entry module registers");
  let loader = vm.module_loader_handle();
  let entry = loader
    .borrow_mut()
    .construct(Path::new("."), "test:parallel-entry")
    .expect("source graph constructs");
  loader
    .borrow_mut()
    .instantiate_all()
    .expect("source graph instantiates");
  let entry_path = entry.borrow().path.clone();
  evaluate_entry_shared(&loader, &entry_path, &mut vm, true).expect("source graph evaluates");

  assert_eq!(
    loader.borrow().pipeline_snapshot(),
    ModuleLoaderPipelineSnapshot {
      source_parses: 4,
      import_extraction_parses: 2,
      source_linking_passes: 4,
      source_typechecks: 2,
      source_compilations: 2,
    }
  );
  assert_eq!(vm.get_global("parallelResult"), Some(Value::Number(42.0)));
}

#[test]
fn relative_virtual_imports_use_importer_namespace() {
  let mut loader = ModuleLoader::new();
  loader
    .register_virtual_module(VirtualModule::javascript(
      "app/shared/value.js",
      crate::js!(module, "export const value = 9;"),
      SourceType::mjs(),
      None,
    ))
    .expect("dependency registration succeeds");
  loader
    .register_virtual_module(VirtualModule::javascript(
      "app/entry.js",
      crate::js!(
        module,
        "import { value } from './shared/value.js'; globalThis.relativeResult = value;"
      ),
      SourceType::mjs(),
      None,
    ))
    .expect("entry registration succeeds");

  let entry = loader
    .construct(Path::new("."), "app/entry.js")
    .expect("relative virtual import constructs");
  assert!(
    loader
      .map
      .contains_key(&virtual_module_path("app/shared/value.js"))
  );
  loader
    .instantiate_all()
    .expect("relative graph instantiates");
  let globals = Rc::new(RefCell::new(ObjectData::new()));
  let mut vm = Vm::new(globals);
  let entry_path = entry.borrow().path.clone();
  loader
    .evaluate_entry(&entry_path, &mut vm, false)
    .expect("relative graph evaluates");
  assert_eq!(vm.get_global("relativeResult"), Some(Value::Number(9.0)));
}

#[test]
fn virtual_reexports_and_aliases_share_canonical_identity() {
  let mut loader = ModuleLoader::new();
  loader
    .register_virtual_module(VirtualModule::javascript(
      "pkg/value.js",
      crate::js!(module, "export const value = 11;"),
      SourceType::mjs(),
      Some(crate::js!(typescript, "export declare const value: number;").to_string()),
    ))
    .expect("value registration succeeds");
  loader
    .register_virtual_module(VirtualModule::javascript(
      "pkg/reexport.js",
      crate::js!(module, "export { value } from './value.js';"),
      SourceType::mjs(),
      None,
    ))
    .expect("reexport registration succeeds");
  loader
    .register_virtual_module(VirtualModule::javascript(
      "pkg/entry.js",
      crate::js!(
        module,
        "import { value } from 'pkg/alias.js'; globalThis.aliasResult = value;"
      ),
      SourceType::mjs(),
      None,
    ))
    .expect("entry registration succeeds");
  loader
    .register_virtual_alias("pkg/alias.js", "pkg/reexport.js")
    .expect("alias registration succeeds");

  let alias = loader
    .construct(Path::new("."), "pkg/alias.js")
    .expect("alias constructs");
  let canonical = loader
    .construct(Path::new("."), "pkg/reexport.js")
    .expect("canonical constructs");
  assert!(Rc::ptr_eq(&alias, &canonical));
  assert_eq!(
    loader.declaration_source("pkg/value.js"),
    Some(crate::js!(
      typescript,
      "export declare const value: number;"
    ))
  );
  let entry = loader
    .construct(Path::new("."), "pkg/entry.js")
    .expect("entry constructs");
  loader
    .instantiate_all()
    .expect("reexport graph instantiates");
  let globals = Rc::new(RefCell::new(ObjectData::new()));
  let mut vm = Vm::new(globals);
  let entry_path = entry.borrow().path.clone();
  loader
    .evaluate_entry(&entry_path, &mut vm, false)
    .expect("reexport graph evaluates");
  assert_eq!(vm.get_global("aliasResult"), Some(Value::Number(11.0)));
}

#[test]
fn source_namespace_reexports_preserve_namespace_identity_and_liveness() {
  let mut loader = ModuleLoader::new();
  loader
    .register_virtual_module(VirtualModule::javascript(
      "namespace-source/base.js",
      crate::js!(module, "export let value = 2;"),
      SourceType::mjs(),
      None,
    ))
    .expect("base registration succeeds");
  loader
    .register_virtual_module(VirtualModule::javascript(
      "namespace-source/reexport.js",
      crate::js!(module, "export * as ns from './base.js';"),
      SourceType::mjs(),
      None,
    ))
    .expect("namespace re-export registration succeeds");
  loader
      .register_virtual_module(VirtualModule::javascript(
        "namespace-source/entry.js",
        crate::js!(module, "import { ns } from './reexport.js'; import * as direct from './base.js'; globalThis.namespaceSame = ns === direct; globalThis.namespaceValue = ns.value; globalThis.namespaceType = typeof direct;"),
        SourceType::mjs(),
        None,
      ))
      .expect("entry registration succeeds");

  let entry = loader
    .construct(Path::new("."), "namespace-source/entry.js")
    .expect("namespace source graph constructs");
  loader
    .instantiate_all()
    .expect("namespace source graph instantiates");
  let globals = Rc::new(RefCell::new(ObjectData::new()));
  let mut vm = Vm::new(globals);
  let entry_path = entry.borrow().path.clone();
  loader
    .evaluate_entry(&entry_path, &mut vm, false)
    .expect("namespace source graph evaluates");
  assert_eq!(vm.get_global("namespaceSame"), Some(Value::Bool(true)));
  assert_eq!(vm.get_global("namespaceValue"), Some(Value::Number(2.0)));
  assert_eq!(
    vm.get_global("namespaceType"),
    Some(Value::String("object".to_string().into()))
  );
}

#[test]
fn module_namespace_objects_use_exotic_live_binding_semantics() {
  let mut loader = ModuleLoader::new();
  loader
    .register_virtual_module(VirtualModule::javascript(
      "namespace-exotic/base.js",
      crate::js!(
        module,
        "export let value = 1; export const alpha = 3; export const zeta = 4;"
      ),
      SourceType::mjs(),
      None,
    ))
    .expect("base registration succeeds");
  loader
    .register_virtual_module(VirtualModule::javascript(
      "namespace-exotic/entry.js",
      crate::js!(
        module,
        r#"
          import * as ns from './base.js';
          const before = Object.getOwnPropertyDescriptor(ns, "value");
          const compatible = Reflect.defineProperty(ns, "value", {
            value: 1, writable: true, enumerable: true, configurable: false
          });
          const incompatible = Reflect.defineProperty(ns, "value", { value: 2 });
          const added = Reflect.defineProperty(ns, "extra", { value: 1 });
          let assignmentRejected = false;
          try { ns.value = 10; } catch (error) { assignmentRejected = error instanceof TypeError; }
          const after = Object.getOwnPropertyDescriptor(ns, "value");
          globalThis.namespaceExotic = [
            Object.getPrototypeOf(ns) === null,
            Object.keys(ns).join(","),
            Reflect.ownKeys(ns).length,
            before.value, before.writable, before.enumerable, before.configurable,
            compatible, incompatible, added,
            Reflect.set(ns, "value", 9),
            Reflect.deleteProperty(ns, "value"),
            Object.isExtensible(ns),
            ns[Symbol.toStringTag],
            Object.prototype.toString.call(ns),
            assignmentRejected, ns.value, after.value
          ].join("|");
        "#
      ),
      SourceType::mjs(),
      None,
    ))
    .expect("entry registration succeeds");

  let entry = loader
    .construct(Path::new("."), "namespace-exotic/entry.js")
    .expect("entry constructs");
  loader
    .instantiate_all()
    .expect("namespace graph instantiates");
  let globals = Rc::new(RefCell::new(ObjectData::new()));
  let mut vm = Vm::new(globals);
  let entry_path = entry.borrow().path.clone();
  loader
    .evaluate_entry(&entry_path, &mut vm, false)
    .expect("namespace graph evaluates");

  assert_eq!(
    vm.get_global("namespaceExotic"),
    Some(Value::String(
      "true|alpha,value,zeta|4|1|true|true|false|true|false|false|false|false|false|Module|[object Module]|true|1|1".into()
    ))
  );
}

#[test]
fn module_namespace_export_descriptors_track_live_binding_cells() {
  let binding = Rc::new(RefCell::new(Value::Number(1.0)));
  let exports = HashMap::from([("value".to_string(), Rc::clone(&binding))]);
  let namespace = crate::vm::runtime::modules::build_module_namespace_object(&exports);
  let globals = Rc::new(RefCell::new(ObjectData::new()));
  let mut vm = Vm::new(globals);
  let key = crate::value::PropertyKey::String("value".into());

  assert!(matches!(
    vm.get_own_property_value(&namespace, &key),
    Ok(Some(PropertyDescriptor::Data {
      value: Value::Number(1.0),
      writable: true,
      enumerable: true,
      configurable: false,
    }))
  ));

  *binding.borrow_mut() = Value::Number(2.0);
  assert_eq!(
    vm.get_property_host(&namespace, &key)
      .expect("namespace export is readable"),
    Value::Number(2.0)
  );
  assert!(matches!(
    vm.get_own_property_value(&namespace, &key),
    Ok(Some(PropertyDescriptor::Data {
      value: Value::Number(2.0),
      writable: true,
      enumerable: true,
      configurable: false,
    }))
  ));
}

#[test]
fn virtual_dependency_errors_are_not_dropped() {
  let mut loader = ModuleLoader::new();
  loader
    .register_virtual_module(VirtualModule::javascript(
      "broken/entry.js",
      crate::js!(module, "import './missing.js'; export const value = 1;"),
      SourceType::mjs(),
      None,
    ))
    .expect("entry registration succeeds");

  let result = loader.construct(Path::new("."), "broken/entry.js");
  assert!(
    result.is_err(),
    "missing virtual dependency must fail construction"
  );
  let error = match result {
    Ok(_) => BundlerError::from("missing virtual dependency was accepted"),
    Err(error) => error,
  };
  assert!(error.to_string().contains("broken/entry.js"));
  assert!(error.to_string().contains("missing.js"));
}

#[test]
fn virtual_cycles_keep_shared_import_export_slots() {
  let mut loader = ModuleLoader::new();
  loader
    .register_virtual_module(VirtualModule::javascript(
      "cycle/a.js",
      crate::js!(
        module,
        "import { value as bValue } from './b.js'; export const value = bValue;"
      ),
      SourceType::mjs(),
      None,
    ))
    .expect("a registration succeeds");
  loader
    .register_virtual_module(VirtualModule::javascript(
      "cycle/b.js",
      crate::js!(
        module,
        "import { value as aValue } from './a.js'; export const value = aValue;"
      ),
      SourceType::mjs(),
      None,
    ))
    .expect("b registration succeeds");
  loader
    .construct(Path::new("."), "cycle/a.js")
    .expect("cycle constructs");
  loader
    .instantiate_all()
    .expect("cycle instantiates");
  let a = loader
    .records_by_specifier
    .get("cycle/a.js")
    .expect("a record")
    .borrow();
  let b = loader
    .records_by_specifier
    .get("cycle/b.js")
    .expect("b record")
    .borrow();
  assert!(Rc::ptr_eq(
    a.imports.get("bValue").expect("a import"),
    b.exports.get("value").expect("b export")
  ));
  assert!(Rc::ptr_eq(
    b.imports.get("aValue").expect("b import"),
    a.exports.get("value").expect("a export")
  ));
}
