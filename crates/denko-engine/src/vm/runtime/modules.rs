// SPDX-License-Identifier: MPL-2.0

use crate::binding::BindingCell;
use crate::gc;
use crate::module::ModuleLoader;
use crate::value::{ObjectData, PropertyDescriptor, PropertyKey, SymbolId, Value};
use crate::vm::Vm;
use indexmap::IndexMap;
use js_string::JsString;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

impl Vm {
  /// Provide mutable access to the per-VM module loader, creating it if missing.
  ///
  /// # Panics
  ///
  /// Panics if the module loader cannot be borrowed mutably.
  pub fn module_loader_mut(&self) -> std::cell::RefMut<'_, ModuleLoader> {
    self.modules.loader.borrow_mut()
  }

  /// Return a snapshot of this VM realm's module-loader pipeline counters.
  #[must_use]
  pub fn module_loader_pipeline_snapshot(&self) -> crate::module::ModuleLoaderPipelineSnapshot {
    self.modules.loader.borrow().pipeline_snapshot()
  }

  /// Validate and register a parser-free prelinked graph in this VM's realm.
  ///
  /// # Errors
  ///
  /// Returns an error for invalid graph metadata or a specifier/index
  /// collision with an existing module registration.
  pub fn register_prelinked_graph(
    &self,
    graph: crate::prelinked::PrelinkedModuleGraph,
  ) -> Result<(), crate::errors::BundlerError> {
    self
      .modules
      .loader
      .borrow_mut()
      .register_prelinked_graph(graph)
  }

  /// Register a parser-free graph and declaration sources atomically.
  ///
  /// Declaration targets must be unique public roots in `graph`; the loader
  /// remains unchanged when declaration validation fails.
  ///
  /// # Errors
  ///
  /// Returns an error if graph validation, declaration validation, or an
  /// existing specifier/index collision rejects registration.
  pub fn register_prelinked_graph_with_declarations<I, S, T>(
    &self,
    graph: crate::prelinked::PrelinkedModuleGraph,
    declarations: I,
  ) -> Result<(), crate::errors::BundlerError>
  where
    I: IntoIterator<Item = (S, T)>,
    S: Into<String>,
    T: Into<String>,
  {
    self
      .modules
      .loader
      .borrow_mut()
      .register_prelinked_graph_with_declarations(graph, declarations)
  }

  /// Register a parser-free graph, declarations, and private host modules atomically.
  ///
  /// # Errors
  ///
  /// Returns an error if graph, declaration, host-module, or collision
  /// validation fails.
  pub fn register_prelinked_graph_with_declarations_and_host_modules<I, S, T>(
    &mut self,
    graph: crate::prelinked::PrelinkedModuleGraph,
    declarations: I,
    host_modules: Vec<crate::module::PrelinkedHostModule>,
  ) -> Result<(), crate::errors::BundlerError>
  where
    I: IntoIterator<Item = (S, T)>,
    S: Into<String>,
    T: Into<String>,
  {
    let host_roots = host_modules
      .iter()
      .flat_map(|module| module.exports.values().cloned())
      .collect::<Vec<_>>();
    self
      .modules
      .loader
      .borrow_mut()
      .register_prelinked_graph_with_declarations_and_host_modules(
        graph,
        declarations,
        host_modules,
      )?;
    for value in host_roots {
      self.create_host_root(value);
    }
    Ok(())
  }

  /// Register one host-provided virtual module in this VM's realm loader.
  ///
  /// # Errors
  ///
  /// Returns an error for an invalid or duplicate specifier.
  pub fn register_virtual_module(
    &self,
    module: crate::module::VirtualModule,
  ) -> Result<(), crate::errors::BundlerError> {
    self
      .modules
      .loader
      .borrow_mut()
      .register_virtual_module(module)
  }

  /// Register an alias for a host-provided virtual module.
  ///
  /// # Errors
  ///
  /// Returns an error when the target is not registered or the alias collides.
  pub fn register_virtual_module_alias(
    &self,
    alias: impl Into<String>,
    canonical_specifier: impl Into<String>,
  ) -> Result<(), crate::errors::BundlerError> {
    self
      .modules
      .loader
      .borrow_mut()
      .register_virtual_alias(alias, canonical_specifier)
  }

  /// Register a host-provided virtual module registry in this VM's realm loader.
  ///
  /// # Errors
  ///
  /// Returns an error for an invalid or duplicate module or alias.
  pub fn register_virtual_modules(
    &self,
    registry: crate::module::VirtualModuleRegistry,
  ) -> Result<(), crate::errors::BundlerError> {
    self
      .modules
      .loader
      .borrow_mut()
      .register_virtual_modules(registry)
  }

  /// Return the canonical specifier of the module currently being evaluated.
  pub(crate) fn current_module_specifier(&self) -> Option<&str> {
    self.modules.current_specifier.as_deref()
  }

  /// Set the canonical specifier of the module currently being evaluated.
  pub(crate) fn set_current_module_specifier(
    &mut self,
    specifier: Option<String>,
  ) {
    self.modules.current_specifier = specifier;
  }

  /// Return a shared handle to the module loader for this VM's realm.
  pub fn module_loader_handle(&self) -> Rc<RefCell<ModuleLoader>> {
    Rc::clone(&self.modules.loader)
  }

  /// Install a module's export slot map into the VM module table at the
  /// specified numeric module index. The provided `exports` map contains
  /// shared value slots allocated by the ModuleLoader so both VM and
  /// ModuleLoader share the same live binding slots.
  pub fn install_module_exports(
    &mut self,
    module_index: usize,
    mut exports: HashMap<String, BindingCell>,
  ) {
    let namespace = build_module_namespace_object(&exports);
    exports.insert("*".to_string(), BindingCell::new(namespace));
    while self.modules.table.len() <= module_index {
      self.modules.table.push(HashMap::new());
    }
    self.modules.table[module_index] = exports;
  }

  /// Install exports only when this VM has not seen the module yet.
  ///
  /// Dynamic imports evaluate in a child execution context, but the namespace
  /// returned to source code belongs to the caller's realm. Preserving an
  /// existing table entry keeps static and dynamic access paths on one
  /// namespace object.
  pub(crate) fn ensure_module_exports(
    &mut self,
    module_index: usize,
    exports: HashMap<String, BindingCell>,
  ) {
    let installed = self
      .modules
      .table
      .get(module_index)
      .is_some_and(|module| module.contains_key("*"));
    if !installed {
      self.install_module_exports(module_index, exports);
    }
  }

  /// Return the current namespace object for a module, if available.
  pub fn module_namespace_value(
    &self,
    module_index: usize,
  ) -> Option<Value> {
    self
      .modules
      .table
      .get(module_index)
      .and_then(|exports| exports.get("*"))
      .map(BindingCell::value)
  }
}

/// Build a module namespace object from exported bindings.
///
/// Creates an object with a dedicated live export map, excluding the "*"
/// wildcard export. Namespace export properties are materialized by the
/// module-namespace exotic internal methods rather than stored as ordinary
/// descriptors.
pub(crate) fn build_module_namespace_object(exports: &HashMap<String, BindingCell>) -> Value {
  let namespace = gc::track_object(Rc::new(RefCell::new(ObjectData::new())));
  let mut names: Vec<JsString> = exports
    .keys()
    .filter(|name| name.as_str() != "*")
    .map(|name| JsString::from(name.as_str()))
    .collect();
  names.sort();

  let mut namespace_exports = IndexMap::new();
  for name in names {
    let Some(slot) = exports.get(name.as_str_lossy()).cloned() else {
      continue;
    };
    namespace_exports.insert(name, slot);
  }
  namespace.borrow_mut().module_namespace_exports = Some(namespace_exports);
  namespace.borrow_mut().properties.insert(
    PropertyKey::Symbol(SymbolId::well_known_to_string_tag()),
    PropertyDescriptor::Data {
      value: Value::String("Module".to_string().into()),
      writable: false,
      enumerable: false,
      configurable: false,
    },
  );
  namespace.borrow_mut().extensible = false;
  Value::Object(namespace)
}
