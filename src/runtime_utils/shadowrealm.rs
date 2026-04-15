use super::*;

thread_local! {
    /// Thread-local registry of ShadowRealm records, keyed by object pointer.
    static SHADOW_REALM_REGISTRY: RefCell<HashMap<usize, Rc<ShadowRealmRecord>>> =
        RefCell::new(HashMap::new());
}

/// Represents a ShadowRealm execution context with its own global environment.
pub(crate) struct ShadowRealmRecord {
  /// The VM for this ShadowRealm's isolated context.
  vm: RefCell<Vm>,
  /// The base directory for resolving module paths in this realm.
  base_dir: PathBuf,
}

impl ShadowRealmRecord {
  /// Creates a new ShadowRealm record with a specified base directory.
  pub(crate) fn new(base_dir: PathBuf) -> Self {
    let globals = Rc::new(RefCell::new(ObjectData::new()));
    Self {
      vm: RefCell::new(Vm::new(globals)),
      base_dir,
    }
  }

  /// Borrows the VM mutably for code execution.
  pub(crate) fn borrow_vm(&self) -> RefMut<'_, Vm> {
    self.vm.borrow_mut()
  }

  /// Returns the base directory for this realm.
  pub(crate) fn base_dir(&self) -> PathBuf {
    self.base_dir.clone()
  }
}

/// Registers a ShadowRealm record for an object.
pub(crate) fn register_shadow_realm(
  obj: &Rc<RefCell<ObjectData>>,
  record: Rc<ShadowRealmRecord>,
) {
  let key = Rc::as_ptr(obj) as usize;
  SHADOW_REALM_REGISTRY.with(|registry| {
    registry.borrow_mut().insert(key, record);
  });
}

/// Retrieves the ShadowRealm record associated with a value, if any.
fn get_shadow_realm_record(value: &Value) -> Option<Rc<ShadowRealmRecord>> {
  if let Value::Object(obj) = value {
    let key = Rc::as_ptr(obj) as usize;
    SHADOW_REALM_REGISTRY.with(|registry| registry.borrow().get(&key).cloned())
  } else {
    None
  }
}

/// Retrieves and validates the ShadowRealm record for a value, throwing if not found.
pub(crate) fn require_shadow_realm_record(
  vm: &mut Vm,
  value: &Value,
) -> Result<Rc<ShadowRealmRecord>, RuntimeError> {
  get_shadow_realm_record(value)
    .ok_or_else(|| vm.throw_type_error("ShadowRealm method called on incompatible receiver"))
}

/// Gets the test path from the VM's global context, if set.
fn current_test_path(vm: &Vm) -> Option<PathBuf> {
  vm.get_global("__TSBLITZ_TEST_PATH")
    .and_then(|value| match value {
      Value::String(s) => Some(PathBuf::from(s)),
      _ => None,
    })
}

/// Resolves the base directory for a ShadowRealm, using the test path if available.
pub(crate) fn resolve_shadowrealm_base_dir(vm: &Vm) -> PathBuf {
  if let Some(path) = current_test_path(vm)
    && let Some(parent) = path.parent()
  {
    return parent.to_path_buf();
  }
  env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}
