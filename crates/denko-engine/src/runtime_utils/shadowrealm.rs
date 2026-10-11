// SPDX-License-Identifier: MPL-2.0
use super::*;
use std::rc::Weak;

type ShadowRealmRegistryEntry = (Weak<RefCell<ObjectData>>, Rc<ShadowRealmRecord>);

thread_local! {
    static SHADOW_REALM_REGISTRY: RefCell<HashMap<usize, ShadowRealmRegistryEntry>> =
        RefCell::new(HashMap::new());
}

pub(crate) struct ShadowRealmRecord {
  vm: RefCell<Vm>,
  base_dir: PathBuf,
}

impl ShadowRealmRecord {
  pub(crate) fn new(base_dir: PathBuf) -> Self {
    let globals = Rc::new(RefCell::new(ObjectData::new()));
    Self {
      vm: RefCell::new(Vm::new(globals)),
      base_dir,
    }
  }

  pub(crate) fn borrow_vm(&self) -> RefMut<'_, Vm> {
    self.vm.borrow_mut()
  }

  pub(crate) fn base_dir(&self) -> PathBuf {
    self.base_dir.clone()
  }
}

pub(crate) fn register_shadow_realm(
  obj: &Rc<RefCell<ObjectData>>,
  record: Rc<ShadowRealmRecord>,
) {
  let key = Rc::as_ptr(obj) as usize;
  SHADOW_REALM_REGISTRY.with(|registry| {
    let mut registry = registry.borrow_mut();
    registry.retain(|_, (wrapper, _)| wrapper.upgrade().is_some());
    registry.insert(key, (Rc::downgrade(obj), record));
  });
}

fn get_shadow_realm_record(value: &Value) -> Option<Rc<ShadowRealmRecord>> {
  if let Value::Object(obj) = value {
    let key = Rc::as_ptr(obj) as usize;
    SHADOW_REALM_REGISTRY.with(|registry| {
      let mut registry = registry.borrow_mut();
      let entry = registry.get(&key).cloned();
      match entry {
        Some((wrapper, record))
          if wrapper
            .upgrade()
            .is_some_and(|wrapper| Rc::ptr_eq(&wrapper, obj)) =>
        {
          Some(record)
        }
        Some(_) => {
          registry.remove(&key);
          None
        }
        None => None,
      }
    })
  } else {
    None
  }
}

pub(crate) fn require_shadow_realm_record(
  vm: &mut Vm,
  value: &Value,
) -> Result<Rc<ShadowRealmRecord>, RuntimeError> {
  get_shadow_realm_record(value)
    .ok_or_else(|| vm.throw_type_error("ShadowRealm method called on incompatible receiver"))
}

fn current_test_path(vm: &Vm) -> Option<PathBuf> {
  vm.get_global("__TSBLITZ_TEST_PATH")
    .and_then(|value| match value {
      Value::String(s) => Some(PathBuf::from(s.as_str_lossy())),
      _ => None,
    })
}

pub(crate) fn resolve_shadowrealm_base_dir(vm: &Vm) -> PathBuf {
  if let Some(path) = current_test_path(vm)
    && let Some(parent) = path.parent()
  {
    return parent.to_path_buf();
  }
  env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn registry_releases_records_for_dead_wrappers() {
    SHADOW_REALM_REGISTRY.with(|registry| registry.borrow_mut().clear());
    let wrapper = Rc::new(RefCell::new(ObjectData::new()));
    let record = Rc::new(ShadowRealmRecord::new(PathBuf::new()));
    register_shadow_realm(&wrapper, Rc::clone(&record));
    assert_eq!(Rc::strong_count(&record), 2);

    drop(wrapper);
    let replacement_wrapper = Rc::new(RefCell::new(ObjectData::new()));
    let replacement_record = Rc::new(ShadowRealmRecord::new(PathBuf::new()));
    register_shadow_realm(&replacement_wrapper, replacement_record);

    assert_eq!(Rc::strong_count(&record), 1);
    SHADOW_REALM_REGISTRY.with(|registry| registry.borrow_mut().clear());
  }
}
