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
fn dynamic_import_reuses_the_realm_module_registry() {
  let globals = Rc::new(RefCell::new(ObjectData::new()));
  let mut vm = Vm::new(globals);
  let loader = vm.module_loader_handle();
  let entry = {
    let mut loader = loader.borrow_mut();
    loader
      .register_virtual_module(VirtualModule::javascript(
        "test:dynamic-dependency",
        crate::js!(module, "export const value = 42;"),
        SourceType::mjs(),
        None,
      ))
      .expect("dynamic dependency registration succeeds");
    loader
        .register_virtual_module(VirtualModule::javascript(
          "test:dynamic-entry",
          crate::js!(
            module,
            "import('test:dynamic-dependency').then(() => { globalThis.dynamicValue = 42; }, () => { globalThis.dynamicValue = -1; });"
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
