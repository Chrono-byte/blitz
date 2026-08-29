use super::*;

#[test]
fn prelinked_direct_import_is_parser_free_and_uses_canonical_slots() {
  let value_chunk = compile_prelinked_chunk(
    crate::js!(module, "export const answer = 42;"),
    0,
    &[],
    &["answer"],
  );
  let entry_chunk = compile_prelinked_chunk(
    crate::js!(
      module,
      "import { answer } from 'graph:value'; globalThis.prelinkedResult = answer;"
    ),
    1,
    &[("answer", 0, "answer")],
    &[],
  );
  let graph = synthetic_graph(
    vec![
      PrelinkedModule {
        specifier: "graph:value".to_string(),
        module_index: 0,
        source_sha256: [1; 32],
        dependencies: vec![],
        exports: vec![export("answer", 0)],
        imports: vec![],
        code: value_chunk,
      },
      PrelinkedModule {
        specifier: "graph:entry".to_string(),
        module_index: 1,
        source_sha256: [2; 32],
        dependencies: vec![0],
        exports: vec![],
        imports: vec![import("answer", 0)],
        code: entry_chunk,
      },
    ],
    1,
    vec![1],
  );

  let mut loader = ModuleLoader::new();
  loader
    .register_prelinked_graph(graph)
    .expect("prelinked graph registers");
  assert!(loader.prelinked_graphs[0].binding_slots.is_none());
  let entry = loader
    .construct(Path::new("."), "graph:entry")
    .expect("prelinked graph constructs");
  assert!(entry.borrow().is_prelinked());
  assert!(entry.borrow().parser_session().is_none());
  assert!(
    entry
      .borrow()
      .bytecode
      .as_ref()
      .expect("prelinked bytecode")
      .source_text
      .starts_with("!!! invalid debug source")
  );

  loader
    .instantiate_all()
    .expect("prelinked graph instantiates");
  assert!(loader.prelinked_graphs[0].binding_slots.is_some());
  let value_record = loader
    .records_by_specifier
    .get("graph:value")
    .expect("value record")
    .borrow();
  assert!(
    value_record
      .exports
      .get("answer")
      .expect("answer export")
      .same_storage(
        entry
          .borrow()
          .imports
          .get("answer")
          .expect("answer import")
      )
  );
  drop(value_record);

  let globals = Rc::new(RefCell::new(ObjectData::new()));
  let mut vm = Vm::new(globals);
  let entry_path = entry.borrow().path.clone();
  let loader = Rc::new(RefCell::new(loader));
  evaluate_entry_shared(&loader, &entry_path, &mut vm, true)
    .expect("prelinked graph evaluates from bytecode only");
  assert_eq!(vm.get_global("prelinkedResult"), Some(Value::Number(42.0)));
  assert_eq!(
    loader.borrow().pipeline_snapshot(),
    ModuleLoaderPipelineSnapshot::default()
  );
}

#[test]
fn source_import_can_reach_a_public_prelinked_root() {
  let graph = synthetic_graph(
    vec![PrelinkedModule {
      specifier: "visibility:public".to_string(),
      module_index: 0,
      source_sha256: [1; 32],
      dependencies: vec![],
      exports: vec![export("answer", 0)],
      imports: vec![],
      code: compile_prelinked_chunk(
        crate::js!(module, "export const answer = 42;"),
        0,
        &[],
        &["answer"],
      ),
    }],
    1,
    vec![0],
  );
  let mut loader = ModuleLoader::new();
  loader
    .register_prelinked_graph(graph)
    .expect("visibility graph registers");
  loader
    .register_virtual_module(VirtualModule::javascript(
      "visibility:source-entry",
      crate::js!(
        module,
        "import { answer } from 'visibility:public'; globalThis.visibilityResult = answer;"
      ),
      SourceType::mjs(),
      None,
    ))
    .expect("source entry registers");

  let entry = loader
    .construct(Path::new("."), "visibility:source-entry")
    .expect("source import of public root constructs");
  loader
    .instantiate_all()
    .expect("source import graph instantiates");
  let globals = Rc::new(RefCell::new(ObjectData::new()));
  let mut vm = Vm::new(globals);
  let entry_path = entry.borrow().path.clone();
  loader
    .evaluate_entry(&entry_path, &mut vm, false)
    .expect("source import graph evaluates");
  assert_eq!(vm.get_global("visibilityResult"), Some(Value::Number(42.0)));
}

#[test]
fn prelinked_public_root_attaches_declaration_source() {
  let graph = synthetic_graph(
    vec![PrelinkedModule {
      specifier: "declarations:public".to_string(),
      module_index: 0,
      source_sha256: [1; 32],
      dependencies: vec![],
      exports: vec![],
      imports: vec![],
      code: compile_prelinked_chunk("", 0, &[], &[]),
    }],
    0,
    vec![0],
  );
  let mut loader = ModuleLoader::new();
  loader
    .register_prelinked_graph_with_declarations(
      graph,
      vec![(
        "declarations:public".to_string(),
        crate::js!(typescript, "export declare const answer: number;").to_string(),
      )],
    )
    .expect("graph and declaration register atomically");

  let record = loader
    .construct(Path::new("."), "declarations:public")
    .expect("public root constructs");
  assert_eq!(
    record.borrow().declaration_source.as_deref(),
    Some(crate::js!(
      typescript,
      "export declare const answer: number;"
    ))
  );
  assert_eq!(
    loader.declaration_source("declarations:public"),
    Some(crate::js!(
      typescript,
      "export declare const answer: number;"
    ))
  );
}

#[test]
fn prelinked_declarations_reject_private_and_unknown_targets_without_installing_graph() {
  let graph = synthetic_graph(
    vec![
      PrelinkedModule {
        specifier: "declarations:private".to_string(),
        module_index: 0,
        source_sha256: [1; 32],
        dependencies: vec![],
        exports: vec![],
        imports: vec![],
        code: compile_prelinked_chunk("", 0, &[], &[]),
      },
      PrelinkedModule {
        specifier: "declarations:public".to_string(),
        module_index: 1,
        source_sha256: [2; 32],
        dependencies: vec![],
        exports: vec![],
        imports: vec![],
        code: compile_prelinked_chunk("", 1, &[], &[]),
      },
    ],
    0,
    vec![1],
  );

  for target in ["declarations:private", "declarations:unknown"] {
    let mut loader = ModuleLoader::new();
    let error = loader
      .register_prelinked_graph_with_declarations(
        graph.clone(),
        vec![(
          target.to_string(),
          crate::js!(typescript, "declare const value: number;").to_string(),
        )],
      )
      .expect_err("invalid declaration target is rejected");
    assert!(error.to_string().contains("not a public root"));
    assert!(loader.prelinked_graphs.is_empty());
    assert!(loader.prelinked_by_specifier.is_empty());
  }
}

#[test]
fn prelinked_declarations_reject_duplicate_targets_without_installing_graph() {
  let graph = synthetic_graph(
    vec![PrelinkedModule {
      specifier: "declarations:public".to_string(),
      module_index: 0,
      source_sha256: [1; 32],
      dependencies: vec![],
      exports: vec![],
      imports: vec![],
      code: compile_prelinked_chunk("", 0, &[], &[]),
    }],
    0,
    vec![0],
  );
  let mut loader = ModuleLoader::new();
  let error = loader
    .register_prelinked_graph_with_declarations(
      graph,
      vec![
        (
          "declarations:public".to_string(),
          crate::js!(typescript, "declare const a: 1;").to_string(),
        ),
        (
          "declarations:public".to_string(),
          crate::js!(typescript, "declare const b: 2;").to_string(),
        ),
      ],
    )
    .expect_err("duplicate declaration target is rejected");
  assert!(
    error
      .to_string()
      .contains("Duplicate prelinked declaration target")
  );
  assert!(loader.prelinked_graphs.is_empty());
  assert!(loader.prelinked_by_specifier.is_empty());
}

#[test]
fn source_import_cannot_reach_a_private_prelinked_module() {
  let graph = synthetic_graph(
    vec![
      PrelinkedModule {
        specifier: "visibility:private".to_string(),
        module_index: 0,
        source_sha256: [1; 32],
        dependencies: vec![],
        exports: vec![],
        imports: vec![],
        code: compile_prelinked_chunk("", 0, &[], &[]),
      },
      PrelinkedModule {
        specifier: "visibility:public".to_string(),
        module_index: 1,
        source_sha256: [2; 32],
        dependencies: vec![],
        exports: vec![],
        imports: vec![],
        code: compile_prelinked_chunk("", 1, &[], &[]),
      },
    ],
    0,
    vec![1],
  );
  let mut loader = ModuleLoader::new();
  loader
    .register_prelinked_graph(graph)
    .expect("visibility graph registers");
  loader
    .register_virtual_module(VirtualModule::javascript(
      "visibility:source-private-entry",
      crate::js!(module, "import 'visibility:private';"),
      SourceType::mjs(),
      None,
    ))
    .expect("source entry registers");

  let error = match loader.construct(Path::new("."), "visibility:source-private-entry") {
    Ok(_) => panic!("source import of private module must fail"),
    Err(error) => error,
  };
  assert!(error.to_string().contains("visibility:private"));
  assert!(error.to_string().contains("private"));
}

#[test]
fn public_prelinked_entry_evaluates_its_private_dependency() {
  let graph = synthetic_graph(
    vec![
      PrelinkedModule {
        specifier: "visibility:private".to_string(),
        module_index: 0,
        source_sha256: [1; 32],
        dependencies: vec![],
        exports: vec![export("answer", 0)],
        imports: vec![],
        code: compile_prelinked_chunk(
          crate::js!(module, "export const answer = 42;"),
          0,
          &[],
          &["answer"],
        ),
      },
      PrelinkedModule {
        specifier: "visibility:entry".to_string(),
        module_index: 1,
        source_sha256: [2; 32],
        dependencies: vec![0],
        exports: vec![],
        imports: vec![import("answer", 0)],
        code: compile_prelinked_chunk(
          crate::js!(
            module,
            "import { answer } from 'visibility:private'; globalThis.visibilityResult = answer;"
          ),
          1,
          &[("answer", 0, "answer")],
          &[],
        ),
      },
    ],
    1,
    vec![1],
  );
  let mut loader = ModuleLoader::new();
  loader
    .register_prelinked_graph(graph)
    .expect("visibility graph registers");
  let entry = loader
    .construct(Path::new("."), "visibility:entry")
    .expect("public prelinked entry constructs");
  loader
    .instantiate_all()
    .expect("public prelinked entry instantiates");

  let globals = Rc::new(RefCell::new(ObjectData::new()));
  let mut vm = Vm::new(globals);
  let entry_path = entry.borrow().path.clone();
  loader
    .evaluate_entry(Path::new("visibility:entry"), &mut vm, false)
    .expect("public prelinked entry resolves and evaluates");
  assert_eq!(vm.get_global("visibilityResult"), Some(Value::Number(42.0)));
  assert!(loader.map.contains_key(&entry_path));
}

#[test]
fn private_prelinked_host_module_injects_exports_without_executing_bytecode() {
  let graph = synthetic_graph(
    vec![
      PrelinkedModule {
        specifier: "host:private".to_string(),
        module_index: 0,
        source_sha256: [1; 32],
        dependencies: vec![],
        exports: vec![export("host", 0)],
        imports: vec![],
        code: compile_prelinked_chunk(
          crate::js!(
            module,
            "export const host = undefined; globalThis.hostInitializerRan = true;"
          ),
          0,
          &[],
          &["host"],
        ),
      },
      PrelinkedModule {
        specifier: "host:entry".to_string(),
        module_index: 1,
        source_sha256: [2; 32],
        dependencies: vec![0],
        exports: vec![],
        imports: vec![import("host", 0)],
        code: compile_prelinked_chunk(
          crate::js!(
            module,
            "import { host } from 'host:private'; globalThis.injectedHost = host;"
          ),
          1,
          &[("host", 0, "host")],
          &[],
        ),
      },
    ],
    1,
    vec![1],
  );
  let host_module = PrelinkedHostModule {
    specifier: "host:private".to_string(),
    exports: HashMap::from([("host".to_string(), Value::Number(7.0))]),
  };
  let mut loader = ModuleLoader::new();
  loader
    .register_prelinked_graph_with_declarations_and_host_modules(
      graph,
      std::iter::empty::<(String, String)>(),
      vec![host_module],
    )
    .expect("host graph registers");
  loader
    .construct(Path::new("."), "host:entry")
    .expect("host entry constructs");
  loader
    .instantiate_all()
    .expect("host entry instantiates");

  let globals = Rc::new(RefCell::new(ObjectData::new()));
  let mut vm = Vm::new(globals);
  loader
    .evaluate_entry(Path::new("host:entry"), &mut vm, false)
    .expect("host entry evaluates");
  assert_eq!(vm.get_global("injectedHost"), Some(Value::Number(7.0)));
  assert_eq!(vm.get_global("hostInitializerRan"), None);
}

#[test]
fn private_prelinked_specifiers_still_collide() {
  let graph = synthetic_graph(
    vec![
      PrelinkedModule {
        specifier: "visibility:private".to_string(),
        module_index: 0,
        source_sha256: [1; 32],
        dependencies: vec![],
        exports: vec![],
        imports: vec![],
        code: compile_prelinked_chunk("", 0, &[], &[]),
      },
      PrelinkedModule {
        specifier: "visibility:public".to_string(),
        module_index: 1,
        source_sha256: [2; 32],
        dependencies: vec![],
        exports: vec![],
        imports: vec![],
        code: compile_prelinked_chunk("", 1, &[], &[]),
      },
    ],
    0,
    vec![1],
  );
  let mut loader = ModuleLoader::new();
  loader
    .register_prelinked_graph(graph)
    .expect("first visibility graph registers");
  assert!(
    loader
      .register_virtual_module(VirtualModule::javascript(
        "visibility:private",
        "",
        SourceType::mjs(),
        None,
      ))
      .is_err(),
    "private prelinked specifier must collide with a virtual module"
  );

  let duplicate_graph = synthetic_graph(
    vec![
      PrelinkedModule {
        specifier: "visibility:private".to_string(),
        module_index: 0,
        source_sha256: [3; 32],
        dependencies: vec![],
        exports: vec![],
        imports: vec![],
        code: compile_prelinked_chunk("", 0, &[], &[]),
      },
      PrelinkedModule {
        specifier: "visibility:other-public".to_string(),
        module_index: 1,
        source_sha256: [4; 32],
        dependencies: vec![],
        exports: vec![],
        imports: vec![],
        code: compile_prelinked_chunk("", 1, &[], &[]),
      },
    ],
    0,
    vec![1],
  );
  assert!(
    loader
      .register_prelinked_graph(duplicate_graph)
      .is_err(),
    "private prelinked specifier must collide with another graph"
  );
}

#[test]
fn prelinked_cycles_read_preallocated_live_bindings() {
  let a_chunk = compile_prelinked_chunk(
    crate::js!(
      module,
      "import { getB } from 'cycle:b'; export const a = 1; export function readB() { return getB(); }"
    ),
    0,
    &[("getB", 1, "getB")],
    &["a", "readB"],
  );
  let b_chunk = compile_prelinked_chunk(
    crate::js!(
      module,
      "import { a } from 'cycle:a'; export function getB() { return a + 1; }"
    ),
    1,
    &[("a", 0, "a")],
    &["getB"],
  );
  let entry_chunk = compile_prelinked_chunk(
    crate::js!(
      module,
      "import { readB } from 'cycle:a'; globalThis.cycleResult = readB();"
    ),
    2,
    &[("readB", 0, "readB")],
    &[],
  );
  let graph = synthetic_graph(
    vec![
      PrelinkedModule {
        specifier: "cycle:a".to_string(),
        module_index: 0,
        source_sha256: [1; 32],
        dependencies: vec![1],
        exports: vec![export("a", 0), export("readB", 1)],
        imports: vec![import("getB", 2)],
        code: a_chunk,
      },
      PrelinkedModule {
        specifier: "cycle:b".to_string(),
        module_index: 1,
        source_sha256: [2; 32],
        dependencies: vec![0],
        exports: vec![export("getB", 2)],
        imports: vec![import("a", 0)],
        code: b_chunk,
      },
      PrelinkedModule {
        specifier: "cycle:entry".to_string(),
        module_index: 2,
        source_sha256: [3; 32],
        dependencies: vec![0],
        exports: vec![],
        imports: vec![import("readB", 1)],
        code: entry_chunk,
      },
    ],
    3,
    vec![2],
  );

  let mut loader = ModuleLoader::new();
  loader
    .register_prelinked_graph(graph)
    .expect("cycle graph registers");
  let entry = loader
    .construct(Path::new("."), "cycle:entry")
    .expect("cycle graph constructs");
  assert_eq!(loader.map.len(), 3, "cycle records are constructed once");
  loader
    .instantiate_all()
    .expect("cycle graph instantiates");
  let a = loader
    .records_by_specifier
    .get("cycle:a")
    .expect("a record")
    .borrow();
  let b = loader
    .records_by_specifier
    .get("cycle:b")
    .expect("b record")
    .borrow();
  assert!(
    a.exports
      .get("a")
      .expect("a export")
      .same_storage(b.imports.get("a").expect("a import"))
  );
  assert!(
    b.exports
      .get("getB")
      .expect("getB export")
      .same_storage(a.imports.get("getB").expect("getB import"))
  );
  drop(a);
  drop(b);

  let globals = Rc::new(RefCell::new(ObjectData::new()));
  let mut vm = Vm::new(globals);
  let entry_path = entry.borrow().path.clone();
  loader
    .evaluate_entry(&entry_path, &mut vm, false)
    .expect("cycle evaluates");
  assert_eq!(vm.get_global("cycleResult"), Some(Value::Number(2.0)));
}

#[test]
fn prelinked_reexports_and_export_all_share_the_origin_slot() {
  let base_chunk = compile_prelinked_chunk(
    crate::js!(module, "export const value = 5;"),
    0,
    &[],
    &["value"],
  );
  let alias_chunk = compile_prelinked_chunk(
    crate::js!(module, "export { value as renamed } from 'reexport:base';"),
    1,
    &[],
    &["renamed"],
  );
  let star_chunk = compile_prelinked_chunk(
    crate::js!(module, "export * from 'reexport:alias';"),
    2,
    &[],
    &["renamed"],
  );
  let entry_chunk = compile_prelinked_chunk(
    crate::js!(
      module,
      "import { renamed } from 'reexport:star'; globalThis.reexportResult = renamed;"
    ),
    3,
    &[("renamed", 2, "renamed")],
    &[],
  );
  let graph = synthetic_graph(
    vec![
      PrelinkedModule {
        specifier: "reexport:base".to_string(),
        module_index: 0,
        source_sha256: [1; 32],
        dependencies: vec![],
        exports: vec![export("value", 0)],
        imports: vec![],
        code: base_chunk,
      },
      PrelinkedModule {
        specifier: "reexport:alias".to_string(),
        module_index: 1,
        source_sha256: [2; 32],
        dependencies: vec![0],
        exports: vec![export("renamed", 0)],
        imports: vec![],
        code: alias_chunk,
      },
      PrelinkedModule {
        specifier: "reexport:star".to_string(),
        module_index: 2,
        source_sha256: [3; 32],
        dependencies: vec![1],
        exports: vec![export("renamed", 0)],
        imports: vec![],
        code: star_chunk,
      },
      PrelinkedModule {
        specifier: "reexport:entry".to_string(),
        module_index: 3,
        source_sha256: [4; 32],
        dependencies: vec![2],
        exports: vec![],
        imports: vec![import("renamed", 0)],
        code: entry_chunk,
      },
    ],
    1,
    vec![3],
  );

  let mut loader = ModuleLoader::new();
  loader
    .register_prelinked_graph(graph)
    .expect("reexport graph registers");
  let entry = loader
    .construct(Path::new("."), "reexport:entry")
    .expect("reexport graph constructs");
  loader
    .instantiate_all()
    .expect("reexport graph instantiates");
  let base_slot = loader.records_by_specifier["reexport:base"]
    .borrow()
    .exports["value"]
    .clone();
  let alias_slot = loader.records_by_specifier["reexport:alias"]
    .borrow()
    .exports["renamed"]
    .clone();
  let star_slot = loader.records_by_specifier["reexport:star"]
    .borrow()
    .exports["renamed"]
    .clone();
  assert!(base_slot.same_storage(&alias_slot));
  assert!(base_slot.same_storage(&star_slot));

  let globals = Rc::new(RefCell::new(ObjectData::new()));
  let mut vm = Vm::new(globals);
  let entry_path = entry.borrow().path.clone();
  loader
    .evaluate_entry(&entry_path, &mut vm, false)
    .expect("reexport graph evaluates");
  assert_eq!(vm.get_global("reexportResult"), Some(Value::Number(5.0)));
}

#[test]
fn prelinked_namespace_imports_share_one_live_namespace() {
  let base_chunk = compile_prelinked_chunk(
    crate::js!(module, "export let value = 2;"),
    0,
    &[],
    &["value"],
  );
  let entry_chunk = compile_prelinked_chunk(
    crate::js!(
      module,
      "import * as first from 'namespace:base'; import * as second from 'namespace:base'; globalThis.namespaceSame = first === second; globalThis.namespaceValue = second.value;"
    ),
    1,
    &[("first", 0, "*"), ("second", 0, "*")],
    &[],
  );
  let graph = synthetic_graph(
    vec![
      PrelinkedModule {
        specifier: "namespace:base".to_string(),
        module_index: 0,
        source_sha256: [1; 32],
        dependencies: vec![],
        exports: vec![export("value", 0)],
        imports: vec![],
        code: base_chunk,
      },
      PrelinkedModule {
        specifier: "namespace:entry".to_string(),
        module_index: 1,
        source_sha256: [2; 32],
        dependencies: vec![0, 0],
        exports: vec![],
        imports: vec![],
        code: entry_chunk,
      },
    ],
    1,
    vec![1],
  );

  let mut loader = ModuleLoader::new();
  loader
    .register_prelinked_graph(graph)
    .expect("namespace graph registers");
  let entry = loader
    .construct(Path::new("."), "namespace:entry")
    .expect("namespace graph constructs");
  loader
    .instantiate_all()
    .expect("namespace graph instantiates");
  let globals = Rc::new(RefCell::new(ObjectData::new()));
  let mut vm = Vm::new(globals);
  let entry_path = entry.borrow().path.clone();
  loader
    .evaluate_entry(&entry_path, &mut vm, false)
    .expect("namespace graph evaluates");
  assert_eq!(vm.get_global("namespaceSame"), Some(Value::Bool(true)));
  assert_eq!(vm.get_global("namespaceValue"), Some(Value::Number(2.0)));
}

#[test]
fn prelinked_namespace_reexports_use_the_target_namespace_without_parsing() {
  let base_chunk = compile_prelinked_chunk(
    crate::js!(module, "export let value = 2;"),
    0,
    &[],
    &["value"],
  );
  let reexport_chunk = compile_prelinked_namespace_reexport_chunk(
    crate::js!(module, "export * as ns from 'namespace-prelinked:base';"),
    1,
    0,
    "ns",
  );
  let entry_chunk = compile_prelinked_chunk(
    crate::js!(
      module,
      "import { ns } from 'namespace-prelinked:reexport'; import * as direct from 'namespace-prelinked:base'; globalThis.prelinkedNamespaceSame = ns === direct; globalThis.prelinkedNamespaceValue = ns.value;"
    ),
    2,
    &[("ns", 1, "ns"), ("direct", 0, "*")],
    &[],
  );
  let graph = synthetic_graph(
    vec![
      PrelinkedModule {
        specifier: "namespace-prelinked:base".to_string(),
        module_index: 0,
        source_sha256: [1; 32],
        dependencies: vec![],
        exports: vec![export("value", 0)],
        imports: vec![],
        code: base_chunk,
      },
      PrelinkedModule {
        specifier: "namespace-prelinked:reexport".to_string(),
        module_index: 1,
        source_sha256: [2; 32],
        dependencies: vec![0],
        exports: vec![export("ns", 1)],
        imports: vec![],
        code: reexport_chunk,
      },
      PrelinkedModule {
        specifier: "namespace-prelinked:entry".to_string(),
        module_index: 2,
        source_sha256: [3; 32],
        dependencies: vec![1, 0],
        exports: vec![],
        imports: vec![import("ns", 1)],
        code: entry_chunk,
      },
    ],
    2,
    vec![2],
  );

  let graph = crate::prelinked_bundle::decode(
    &crate::prelinked_bundle::encode(&graph).expect("namespace graph encodes"),
  )
  .expect("namespace graph decodes");
  let mut loader = ModuleLoader::new();
  loader
    .register_prelinked_graph(graph)
    .expect("prelinked namespace graph registers");
  let entry = loader
    .construct(Path::new("."), "namespace-prelinked:entry")
    .expect("prelinked namespace graph constructs");
  loader
    .instantiate_all()
    .expect("prelinked namespace graph instantiates");
  assert!(
    loader
      .records_by_specifier
      .get("namespace-prelinked:base")
      .expect("base record")
      .borrow()
      .parser_session()
      .is_none()
  );

  let globals = Rc::new(RefCell::new(ObjectData::new()));
  let mut vm = Vm::new(globals);
  let entry_path = entry.borrow().path.clone();
  loader
    .evaluate_entry(&entry_path, &mut vm, false)
    .expect("prelinked namespace graph evaluates");
  assert_eq!(
    vm.get_global("prelinkedNamespaceSame"),
    Some(Value::Bool(true))
  );
  assert_eq!(
    vm.get_global("prelinkedNamespaceValue"),
    Some(Value::Number(2.0))
  );
}

#[test]
fn prelinked_dependency_edges_control_side_effect_order() {
  let modules = vec![
    PrelinkedModule {
      specifier: "order:a".to_string(),
      module_index: 0,
      source_sha256: [1; 32],
      dependencies: vec![],
      exports: vec![],
      imports: vec![],
      code: compile_prelinked_chunk(
        crate::js!(module, "globalThis.order = (globalThis.order || '') + 'A';"),
        0,
        &[],
        &[],
      ),
    },
    PrelinkedModule {
      specifier: "order:b".to_string(),
      module_index: 1,
      source_sha256: [2; 32],
      dependencies: vec![],
      exports: vec![],
      imports: vec![],
      code: compile_prelinked_chunk(
        crate::js!(module, "globalThis.order = (globalThis.order || '') + 'B';"),
        1,
        &[],
        &[],
      ),
    },
    PrelinkedModule {
      specifier: "order:entry".to_string(),
      module_index: 2,
      source_sha256: [3; 32],
      dependencies: vec![1, 0],
      exports: vec![],
      imports: vec![],
      code: compile_prelinked_chunk(
        crate::js!(module, "globalThis.order = (globalThis.order || '') + 'E';"),
        2,
        &[],
        &[],
      ),
    },
  ];
  let mut loader = ModuleLoader::new();
  loader
    .register_prelinked_graph(synthetic_graph(modules, 0, vec![2]))
    .expect("ordered graph registers");
  let entry = loader
    .construct(Path::new("."), "order:entry")
    .expect("ordered graph constructs");
  loader
    .instantiate_all()
    .expect("ordered graph instantiates");
  let entry_path = entry.borrow().path.clone();
  assert_eq!(
    loader
      .evaluation_order_from(&entry_path)
      .expect("evaluation order"),
    vec![
      virtual_module_path("order:b"),
      virtual_module_path("order:a"),
      virtual_module_path("order:entry"),
    ]
  );

  let globals = Rc::new(RefCell::new(ObjectData::new()));
  let mut vm = Vm::new(globals);
  loader
    .evaluate_entry(&entry_path, &mut vm, false)
    .expect("ordered graph evaluates");
  assert_eq!(
    vm.get_global("order"),
    Some(Value::String("BAE".to_string().into()))
  );
}

#[test]
fn source_modules_mix_with_prelinked_graphs_and_keep_reserved_indices() {
  let graph = synthetic_graph(
    vec![PrelinkedModule {
      specifier: "mixed:value".to_string(),
      module_index: 0,
      source_sha256: [1; 32],
      dependencies: vec![],
      exports: vec![export("answer", 0)],
      imports: vec![],
      code: compile_prelinked_chunk(
        crate::js!(module, "export const answer = 42;"),
        0,
        &[],
        &["answer"],
      ),
    }],
    1,
    vec![0],
  );
  let mut loader = ModuleLoader::new();
  loader
    .register_virtual_module(VirtualModule::javascript(
      "mixed:early",
      crate::js!(module, "export const early = 1;"),
      SourceType::mjs(),
      None,
    ))
    .expect("early source registers");
  let early = loader
    .construct(Path::new("."), "mixed:early")
    .expect("source can be constructed before graph registration");
  assert_eq!(early.borrow().module_index, None);

  loader
    .register_prelinked_graph(graph.clone())
    .expect("graph reserves index zero");
  loader
    .register_virtual_module(VirtualModule::javascript(
      "mixed:entry",
      crate::js!(
        module,
        "import { answer } from 'mixed:value'; globalThis.mixedResult = answer;"
      ),
      SourceType::mjs(),
      None,
    ))
    .expect("mixed source registers");
  let entry = loader
    .construct(Path::new("."), "mixed:entry")
    .expect("source imports prelinked module");
  loader
    .instantiate_all()
    .expect("mixed graph instantiates");
  assert!(
    early
      .borrow()
      .module_index
      .is_some_and(|index| index > 0)
  );
  assert!(
    entry
      .borrow()
      .module_index
      .is_some_and(|index| index > 0)
  );
  assert_ne!(early.borrow().module_index, entry.borrow().module_index);

  let globals = Rc::new(RefCell::new(ObjectData::new()));
  let mut vm = Vm::new(globals);
  let entry_path = entry.borrow().path.clone();
  loader
    .evaluate_entry(&entry_path, &mut vm, false)
    .expect("mixed graph evaluates");
  assert_eq!(vm.get_global("mixedResult"), Some(Value::Number(42.0)));

  loader
    .register_virtual_module(VirtualModule::javascript(
      "mixed:late",
      crate::js!(module, "export const late = 2;"),
      SourceType::mjs(),
      None,
    ))
    .expect("late source registers");
  let late = loader
    .construct(Path::new("."), "mixed:late")
    .expect("late source constructs");
  loader
    .instantiate_all()
    .expect("late source instantiates without collision");
  let late_index = late
    .borrow()
    .module_index
    .expect("late source index");
  assert!(late_index > 0);
  assert_ne!(late.borrow().module_index, early.borrow().module_index);
  assert_ne!(late.borrow().module_index, entry.borrow().module_index);

  let mut collision_loader = ModuleLoader::new();
  collision_loader
    .register_virtual_module(VirtualModule::javascript(
      "mixed:indexed-first",
      crate::js!(module, "export const value = 1;"),
      SourceType::mjs(),
      None,
    ))
    .expect("source registers");
  collision_loader
    .construct(Path::new("."), "mixed:indexed-first")
    .expect("source constructs");
  collision_loader
    .instantiate_all()
    .expect("source receives index zero");
  assert!(
    collision_loader
      .register_prelinked_graph(graph)
      .is_err(),
    "late graph registration must reject an already-used reserved index"
  );
}

#[test]
fn precompiled_virtual_modules_keep_source_metadata_and_stable_indices() {
  let source = crate::js!(module, "export const value = 42;");
  let session = crate::parser::Parser::parse_source_with_type(source, SourceType::mjs())
    .expect("precompiled source parses");
  let context = ModuleContext {
    module_index: 7,
    imports: HashMap::new(),
    exports: HashSet::from(["value".to_string()]),
    namespace_exports: HashMap::new(),
  };
  let code =
    crate::compiler::compile_oxc_with_ctx(session.program(), Some(&context), session.source())
      .expect("precompiled source compiles");
  let mut loader = ModuleLoader::new();
  loader
    .register_virtual_module(VirtualModule::precompiled(
      "test:precompiled",
      source,
      SourceType::mjs(),
      None,
      7,
      code,
    ))
    .expect("precompiled registration succeeds");
  loader
    .register_virtual_module(VirtualModule::javascript(
      "test:source",
      crate::js!(module, "export const other = 1;"),
      SourceType::mjs(),
      None,
    ))
    .expect("source registration succeeds");
  let source_entry = loader
    .construct(Path::new("."), "test:source")
    .expect("source module constructs");
  let entry = loader
    .construct(Path::new("."), "test:precompiled")
    .expect("precompiled module constructs");
  loader
    .instantiate_all()
    .expect("precompiled module instantiates");
  assert_eq!(entry.borrow().module_index, Some(7));
  assert_eq!(source_entry.borrow().module_index, Some(8));

  let globals = Rc::new(RefCell::new(ObjectData::new()));
  let mut vm = Vm::new(globals);
  let entry_path = entry.borrow().path.clone();
  loader
    .evaluate_entry(&entry_path, &mut vm, false)
    .expect("precompiled module evaluates");
  assert_eq!(
    entry
      .borrow()
      .exports
      .get("value")
      .expect("value export")
      .value(),
    Value::Number(42.0),
  );
}
