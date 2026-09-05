use super::*;

#[test]
fn import_meta_uses_the_canonical_record_url_and_preserves_identity() {
  let globals = Rc::new(RefCell::new(ObjectData::new()));
  let mut vm = Vm::new(globals);
  let loader = vm.module_loader_handle();
  let entry = {
    let mut loader = loader.borrow_mut();
    loader
      .register_virtual_module(VirtualModule::javascript(
        "test:meta-entry",
        crate::js!(
          module,
          "globalThis.metaUrl = import.meta.url; globalThis.metaSame = import.meta === import.meta;"
        ),
        SourceType::mjs(),
        None,
      ))
      .expect("metadata module registration succeeds");
    let entry = loader
      .construct(Path::new("."), "test:meta-entry")
      .expect("metadata module constructs");
    loader
      .instantiate_all()
      .expect("metadata module instantiates");
    entry
  };
  let entry_path = entry.borrow().path.clone();
  evaluate_entry_shared(&loader, &entry_path, &mut vm, false).expect("metadata module evaluates");

  assert_eq!(
    vm.get_global("metaUrl"),
    Some(Value::String("test:meta-entry".to_string().into()))
  );
  assert_eq!(vm.get_global("metaSame"), Some(Value::Bool(true)));
}

#[test]
fn anonymous_default_class_exports_its_constructor_and_default_name() {
  let mut vm = Vm::new(Rc::new(RefCell::new(ObjectData::new())));
  for (specifier, source) in [
    (
      "test:default-class",
      crate::js!(module, "export default class { read() { return 42; } }"),
    ),
    (
      "test:class-importer",
      crate::js!(
        module,
        r#"
      import Constructor from 'test:default-class';
      globalThis.className = Constructor.name;
      globalThis.classValue = new Constructor().read();
      globalThis.classRejectsCall = false;
      try { Constructor(); } catch (error) { globalThis.classRejectsCall = error instanceof TypeError; }
    "#
      ),
    ),
  ] {
    vm.register_virtual_module(VirtualModule::javascript(
      specifier,
      source,
      SourceType::mjs(),
      None,
    ))
    .expect("register class fixture");
  }
  let loader = vm.module_loader_handle();
  let entry = loader
    .borrow_mut()
    .construct(Path::new("."), "test:class-importer")
    .expect("construct importer");
  loader
    .borrow_mut()
    .instantiate_all()
    .expect("instantiate graph");
  let path = entry.borrow().path.clone();
  evaluate_entry_shared(&loader, &path, &mut vm, false).expect("evaluate imported class");
  assert_eq!(
    vm.get_global("className"),
    Some(Value::String("default".into()))
  );
  assert_eq!(vm.get_global("classValue"), Some(Value::Number(42.0)));
  assert_eq!(vm.get_global("classRejectsCall"), Some(Value::Bool(true)));
}

#[test]
fn dynamic_import_reuses_the_realm_module_registry() {
  let globals = Rc::new(RefCell::new(ObjectData::new()));
  let mut vm = Vm::new(globals);
  let loader = vm.module_loader_handle();
  let entry = {
    let mut loader = loader.borrow_mut();
    loader
      .register_virtual_module(VirtualModule::javascript(
        "test:dynamic-dependency",
        crate::js!(module, "globalThis.dependencyEvaluations = (globalThis.dependencyEvaluations || 0) + 1; export const value = 42;"),
        SourceType::mjs(),
        None,
      ))
      .expect("dynamic dependency registration succeeds");
    loader
      .register_virtual_module(VirtualModule::javascript(
        "test:dynamic-entry",
        crate::js!(
          module,
          r#"
              import * as existing from 'test:dynamic-dependency';
              Promise.all([import('test:dynamic-dependency'), import('test:dynamic-dependency')])
                .then(([first, second]) => {
                  globalThis.dynamicValue = first.value;
                  globalThis.sameNamespace = first === second && first === existing;
                });
            "#
        ),
        SourceType::mjs(),
        None,
      ))
      .expect("dynamic entry registration succeeds");
    let entry = loader
      .construct(Path::new("."), "test:dynamic-entry")
      .expect("dynamic entry constructs");
    loader
      .instantiate_all()
      .expect("dynamic entry instantiates");
    entry
  };
  let entry_path = entry.borrow().path.clone();
  evaluate_entry_shared(&loader, &entry_path, &mut vm, false).expect("dynamic entry evaluates");
  assert_eq!(vm.get_global("dynamicValue"), Some(Value::Number(42.0)));
  assert_eq!(vm.get_global("sameNamespace"), Some(Value::Bool(true)));
  assert_eq!(
    vm.get_global("dependencyEvaluations"),
    Some(Value::Number(1.0))
  );
}

#[test]
fn dynamic_import_defers_construction_and_evaluation_to_its_job() {
  let globals = Rc::new(RefCell::new(ObjectData::new()));
  let mut vm = Vm::new(globals);
  vm.module_loader_handle()
    .borrow_mut()
    .register_virtual_module(VirtualModule::javascript(
      "test:deferred-dynamic-import",
      crate::js!(
        module,
        "globalThis.dynamicImportEvaluated = true; export const value = 1;"
      ),
      SourceType::mjs(),
      None,
    ))
    .expect("dynamic dependency registration succeeds");

  let promise = crate::runtime_utils::dynamic_import_value(
    &mut vm,
    &[Value::String(
      "test:deferred-dynamic-import".to_string().into(),
    )],
  )
  .expect("dynamic import returns its promise");
  assert!(matches!(promise, Value::Promise(..)));
  assert_eq!(vm.get_global("dynamicImportEvaluated"), None);

  vm.flush_microtasks()
    .expect("dynamic import job completes");
  assert_eq!(
    vm.get_global("dynamicImportEvaluated"),
    Some(Value::Bool(true))
  );
}

#[test]
fn generated_bytecode_virtual_modules_are_loaded_and_run() {
  let session = crate::parser::Parser::parse_source(crate::js!("globalThis.bytecodeResult = 7;"))
    .expect("bytecode source parses");
  let code = crate::compiler::compile(session.program(), session.source()).expect("compiles");
  let mut loader = ModuleLoader::new();
  loader
    .register_virtual_module(VirtualModule::new(
      "test:bytecode",
      VirtualModuleSource::Bytecode(code),
      SourceType::mjs(),
      None,
    ))
    .expect("bytecode registration succeeds");
  let entry = loader
    .construct(Path::new("."), "test:bytecode")
    .expect("bytecode module constructs");
  loader
    .instantiate_all()
    .expect("bytecode module instantiates");

  let globals = Rc::new(RefCell::new(ObjectData::new()));
  let mut vm = Vm::new(globals);
  let entry_path = entry.borrow().path.clone();
  loader
    .evaluate_entry(&entry_path, &mut vm, false)
    .expect("bytecode module evaluates");
  assert_eq!(vm.get_global("bytecodeResult"), Some(Value::Number(7.0)));
}

fn prepare_async_graph(
  vm: &mut Vm,
  modules: &[(&str, &str)],
  entry: &str,
) -> PathBuf {
  let loader = vm.module_loader_handle();
  let mut loader = loader.borrow_mut();
  for (specifier, source) in modules {
    loader
      .register_virtual_module(VirtualModule::javascript(
        *specifier,
        *source,
        SourceType::mjs(),
        None,
      ))
      .expect("register async module");
  }
  let record = loader
    .construct(Path::new("."), entry)
    .expect("construct async graph");
  loader
    .instantiate_all()
    .expect("instantiate async graph");
  record.borrow().path.clone()
}

fn assert_evaluation_fulfilled(promise: &Value) {
  let Value::Promise(promise, _) = promise else {
    panic!("evaluation returns a promise")
  };
  assert!(matches!(
    *promise.borrow(),
    crate::value::PromiseState::Fulfilled(Value::Undefined)
  ));
}

#[test]
fn top_level_await_suspends_dependencies_and_preserves_import_meta() {
  let mut vm = Vm::new(Rc::new(RefCell::new(ObjectData::new())));
  let gate = vm.create_host_promise();
  vm.realm().set_global("gate", gate.clone());
  let entry = prepare_async_graph(
    &mut vm,
    &[
      (
        "test:await-dependency",
        crate::js!(
          module,
          "globalThis.started = true; await gate; export const value = 42; globalThis.referrer = import.meta.url;"
        ),
      ),
      (
        "test:await-entry",
        crate::js!(
          module,
          "import { value } from 'test:await-dependency'; globalThis.imported = value;"
        ),
      ),
    ],
    "test:await-entry",
  );
  let loader = vm.module_loader_handle();
  let promise =
    evaluate_entry_promise_shared(&loader, &entry, &mut vm, false).expect("start evaluation");
  assert_eq!(vm.get_global("started"), Some(Value::Bool(true)));
  assert_eq!(vm.get_global("imported"), None);
  vm.flush_microtasks()
    .expect("pending await does not throw");
  assert_eq!(vm.get_global("imported"), None);
  vm.resolve_host_promise(&gate, Value::Undefined)
    .expect("release gate");
  vm.flush_microtasks()
    .expect("finish dependency and importer");
  assert_eq!(vm.get_global("imported"), Some(Value::Number(42.0)));
  assert_eq!(
    vm.get_global("referrer"),
    Some(Value::String("test:await-dependency".into()))
  );
  assert_evaluation_fulfilled(&promise);
}

#[test]
fn async_module_siblings_start_before_a_pending_dependency_finishes() {
  let mut vm = Vm::new(Rc::new(RefCell::new(ObjectData::new())));
  let entry = prepare_async_graph(
    &mut vm,
    &[
      (
        "test:order-a",
        crate::js!(
          module,
          "globalThis.order = 'a'; await 0; globalThis.order += 'A';"
        ),
      ),
      (
        "test:order-b",
        crate::js!(
          module,
          "globalThis.order += 'b'; await 0; globalThis.order += 'B';"
        ),
      ),
      (
        "test:order-entry",
        crate::js!(
          module,
          "import 'test:order-a'; import 'test:order-b'; globalThis.order += 'E';"
        ),
      ),
    ],
    "test:order-entry",
  );
  let loader = vm.module_loader_handle();
  let promise =
    evaluate_entry_promise_shared(&loader, &entry, &mut vm, false).expect("start siblings");
  assert_eq!(vm.get_global("order"), Some(Value::String("ab".into())));
  vm.flush_microtasks().expect("finish siblings");
  assert_eq!(vm.get_global("order"), Some(Value::String("abABE".into())));
  assert_evaluation_fulfilled(&promise);
}

#[test]
fn async_module_cycles_share_evaluation_and_do_not_wait_on_back_edges() {
  let mut vm = Vm::new(Rc::new(RefCell::new(ObjectData::new())));
  let entry = prepare_async_graph(
    &mut vm,
    &[
      (
        "test:cycle-a",
        crate::js!(
          module,
          "import 'test:cycle-b'; globalThis.cycleOrder += 'a';"
        ),
      ),
      (
        "test:cycle-b",
        crate::js!(
          module,
          "import 'test:cycle-a'; globalThis.cycleOrder = 'b'; await 0; globalThis.cycleOrder += 'B';"
        ),
      ),
    ],
    "test:cycle-a",
  );
  let loader = vm.module_loader_handle();
  let promise =
    evaluate_entry_promise_shared(&loader, &entry, &mut vm, false).expect("start cycle");
  let member = loader
    .borrow_mut()
    .construct(Path::new("."), "test:cycle-b")
    .expect("cached cycle member");
  let member_path = member.borrow().path.clone();
  let repeated = evaluate_entry_promise_shared(&loader, &member_path, &mut vm, false)
    .expect("reuse cycle evaluation");
  assert!(crate::runtime_utils::strict_equal(&promise, &repeated));
  vm.flush_microtasks().expect("finish cycle");
  assert_eq!(
    vm.get_global("cycleOrder"),
    Some(Value::String("bBa".into()))
  );
  assert_evaluation_fulfilled(&promise);
}

#[test]
fn async_module_rejection_is_cached_and_prevents_parent_execution() {
  let mut vm = Vm::new(Rc::new(RefCell::new(ObjectData::new())));
  let entry = prepare_async_graph(
    &mut vm,
    &[
      (
        "test:reject-dependency",
        crate::js!(
          module,
          "globalThis.moduleError = {}; await 0; throw globalThis.moduleError;"
        ),
      ),
      (
        "test:reject-entry",
        crate::js!(
          module,
          "import 'test:reject-dependency'; globalThis.shouldNotRun = true;"
        ),
      ),
    ],
    "test:reject-entry",
  );
  let loader = vm.module_loader_handle();
  let promise =
    evaluate_entry_promise_shared(&loader, &entry, &mut vm, false).expect("start rejecting graph");
  vm.flush_microtasks().expect("record rejection");
  assert_eq!(vm.get_global("shouldNotRun"), None);
  let Value::Promise(state, _) = &promise else {
    panic!("evaluation promise")
  };
  let reason = match &*state.borrow() {
    crate::value::PromiseState::Rejected { reason, .. } => reason.clone(),
    _ => panic!("evaluation must reject"),
  };
  assert!(crate::runtime_utils::strict_equal(
    &reason,
    &vm.get_global("moduleError").unwrap()
  ));
  let repeated =
    evaluate_entry_promise_shared(&loader, &entry, &mut vm, false).expect("cached evaluation");
  assert!(crate::runtime_utils::strict_equal(&promise, &repeated));
  assert_eq!(vm.get_global("shouldNotRun"), None);
}

#[test]
fn module_evaluation_does_not_flush_jobs_between_synchronous_dependencies() {
  let mut vm = Vm::new(Rc::new(RefCell::new(ObjectData::new())));
  let entry = prepare_async_graph(
    &mut vm,
    &[
      (
        "test:sync-order-a",
        crate::js!(
          module,
          "globalThis.syncOrder = 'a'; Promise.resolve().then(() => { globalThis.syncOrder += 'j'; });"
        ),
      ),
      (
        "test:sync-order-b",
        crate::js!(module, "globalThis.syncOrder += 'b';"),
      ),
      (
        "test:sync-order-entry",
        crate::js!(
          module,
          "import 'test:sync-order-a'; import 'test:sync-order-b'; globalThis.syncOrder += 'e';"
        ),
      ),
    ],
    "test:sync-order-entry",
  );
  let loader = vm.module_loader_handle();
  let promise =
    evaluate_entry_promise_shared(&loader, &entry, &mut vm, false).expect("evaluate sync graph");
  assert_eq!(
    vm.get_global("syncOrder"),
    Some(Value::String("abe".into()))
  );
  assert_evaluation_fulfilled(&promise);
  vm.flush_microtasks()
    .expect("run scheduled reaction");
  assert_eq!(
    vm.get_global("syncOrder"),
    Some(Value::String("abej".into()))
  );
}

#[test]
fn unreachable_top_level_await_still_makes_module_evaluation_asynchronous() {
  let mut vm = Vm::new(Rc::new(RefCell::new(ObjectData::new())));
  let entry = prepare_async_graph(
    &mut vm,
    &[
      (
        "test:unreachable-await",
        crate::js!(
          module,
          "if (false) await 0; globalThis.beforeUnreachable = true;"
        ),
      ),
      (
        "test:unreachable-entry",
        crate::js!(
          module,
          "import 'test:unreachable-await'; globalThis.afterUnreachable = true;"
        ),
      ),
    ],
    "test:unreachable-entry",
  );
  let loader = vm.module_loader_handle();
  let promise = evaluate_entry_promise_shared(&loader, &entry, &mut vm, false)
    .expect("evaluate unreachable await");
  assert_eq!(vm.get_global("beforeUnreachable"), Some(Value::Bool(true)));
  assert_eq!(vm.get_global("afterUnreachable"), None);
  vm.flush_microtasks()
    .expect("finish syntactically async dependency");
  assert_eq!(vm.get_global("afterUnreachable"), Some(Value::Bool(true)));
  assert_evaluation_fulfilled(&promise);
}

#[test]
fn dynamic_import_waits_for_top_level_await_and_shares_pending_evaluation() {
  let mut vm = Vm::new(Rc::new(RefCell::new(ObjectData::new())));
  let gate = vm.create_host_promise();
  vm.realm().set_global("importGate", gate.clone());
  prepare_async_graph(
    &mut vm,
    &[(
      "test:dynamic-await",
      crate::js!(
        module,
        "globalThis.dynamicStarts = (globalThis.dynamicStarts || 0) + 1; await importGate; export const answer = 42;"
      ),
    )],
    "test:dynamic-await",
  );
  let first = crate::runtime_utils::dynamic_import_value(
    &mut vm,
    &[Value::String("test:dynamic-await".into())],
  )
  .expect("first import");
  let second = crate::runtime_utils::dynamic_import_value(
    &mut vm,
    &[Value::String("test:dynamic-await".into())],
  )
  .expect("second import");
  vm.flush_microtasks().expect("start imports");
  assert_eq!(vm.get_global("dynamicStarts"), Some(Value::Number(1.0)));
  for promise in [&first, &second] {
    let Value::Promise(state, _) = promise else {
      panic!("import promise")
    };
    assert!(matches!(
      *state.borrow(),
      crate::value::PromiseState::Pending { .. }
    ));
  }
  vm.resolve_host_promise(&gate, Value::Undefined)
    .expect("release import");
  vm.flush_microtasks().expect("settle imports");
  let namespace = |promise: &Value| {
    let Value::Promise(state, _) = promise else {
      panic!("import promise")
    };
    match &*state.borrow() {
      crate::value::PromiseState::Fulfilled(value) => value.clone(),
      _ => panic!("import must fulfill after module evaluation"),
    }
  };
  let first = namespace(&first);
  assert!(crate::runtime_utils::strict_equal(
    &first,
    &namespace(&second)
  ));
  assert_eq!(
    vm.get_property_host(&first, &PropertyKey::String("answer".into()))
      .expect("export"),
    Value::Number(42.0)
  );
}

#[test]
fn dynamic_import_preserves_top_level_await_rejection_identity() {
  let mut vm = Vm::new(Rc::new(RefCell::new(ObjectData::new())));
  prepare_async_graph(
    &mut vm,
    &[(
      "test:dynamic-reject",
      crate::js!(
        module,
        "globalThis.dynamicReason = {}; await 0; throw globalThis.dynamicReason;"
      ),
    )],
    "test:dynamic-reject",
  );
  let promise = crate::runtime_utils::dynamic_import_value(
    &mut vm,
    &[Value::String("test:dynamic-reject".into())],
  )
  .expect("import");
  vm.flush_microtasks().expect("reject import");
  let Value::Promise(state, _) = promise else {
    panic!("import promise")
  };
  let reason = match &*state.borrow() {
    crate::value::PromiseState::Rejected { reason, .. } => reason.clone(),
    _ => panic!("import must reject"),
  };
  assert!(crate::runtime_utils::strict_equal(
    &reason,
    &vm.get_global("dynamicReason").unwrap()
  ));
}

#[test]
fn shadowrealm_import_value_waits_for_async_module_evaluation() {
  let mut vm = Vm::new(Rc::new(RefCell::new(ObjectData::new())));
  let session = crate::parser::Parser::parse_source(crate::js!(
    module,
    "globalThis.testRealm = new ShadowRealm();"
  ))
  .expect("parse realm creation");
  vm.run(crate::compiler::compile_session(&session).expect("compile realm creation"))
    .expect("create realm");
  let realm = vm.get_global("testRealm").expect("realm wrapper");
  let record =
    crate::runtime_utils::require_shadow_realm_record(&mut vm, &realm).expect("realm record");
  let gate = {
    let mut inner = record.borrow_vm();
    let gate = inner.create_host_promise();
    inner
      .realm()
      .set_global("realmGate", gate.clone());
    prepare_async_graph(
      &mut inner,
      &[(
        "test:realm-await",
        crate::js!(module, "await realmGate; export const answer = 42;"),
      )],
      "test:realm-await",
    );
    gate
  };
  let import = vm
    .get_property_host(&realm, &PropertyKey::String("importValue".into()))
    .expect("importValue method");
  let promise = vm
    .call_value_with_this(
      &import,
      &realm,
      &[
        Value::String("test:realm-await".into()),
        Value::String("answer".into()),
      ],
    )
    .expect("start realm import");
  vm.flush_microtasks()
    .expect("start pending module");
  let Value::Promise(state, _) = &promise else {
    panic!("importValue promise")
  };
  assert!(matches!(
    *state.borrow(),
    crate::value::PromiseState::Pending { .. }
  ));
  vm.resolve_host_promise(&gate, Value::Undefined)
    .expect("release realm gate");
  vm.flush_microtasks()
    .expect("finish realm import");
  assert!(matches!(
    *state.borrow(),
    crate::value::PromiseState::Fulfilled(Value::Number(42.0))
  ));
}
