// SPDX-License-Identifier: MPL-2.0

use crate::binding::BindingCell;
use crate::environment::EnvironmentRecord;
use crate::value::{PropertyDescriptor, PropertyKey, Value};
use crate::vm::Vm;
use crate::vm::state::SlotMetadata;
use std::cell::RefCell;
use std::rc::Rc;

impl Vm {
  pub(super) fn persist_root_bindings(&mut self) {
    self.persist_root_bindings_with_filter(false);
  }

  pub(super) fn persist_root_var_bindings(&mut self) {
    self.persist_root_bindings_with_filter(true);
  }

  fn persist_root_bindings_with_filter(
    &mut self,
    skip_lexical: bool,
  ) {
    let Some(slots) = self.execution.active.slots.as_ref() else {
      return;
    };
    let names = slots.names.clone();
    let immutable = slots.immutable.clone();
    let locals = self.execution.active.locals.clone();
    let lexical = self
      .execution
      .active
      .chunk
      .root_lexical_tdz
      .clone();
    let mut global_env = self.realm_state.global_env.borrow_mut();
    let mut global_object = self.realm_state.globals.borrow_mut();

    for (index, name) in names.into_iter().enumerate() {
      if skip_lexical && lexical.get(index).copied().unwrap_or(false) {
        continue;
      }
      let (Some(name), Some(cell)) = (name, locals.get(index)) else {
        continue;
      };
      let property_name = name.clone();
      let initialized = !cell.is_uninitialized();
      if immutable.get(index).copied().unwrap_or(false) {
        global_env.create_immutable_binding(name, cell.clone(), initialized);
      } else if lexical.get(index).copied().unwrap_or(false) {
        global_env.create_mutable_lexical_binding(name, cell.clone(), initialized);
      } else {
        global_env.create_mutable_binding(name, cell.clone(), initialized);
      }
      if !lexical.get(index).copied().unwrap_or(false) && initialized {
        let property_key = PropertyKey::String(property_name.into());
        match global_object.properties.get_mut(&property_key) {
          Some(PropertyDescriptor::Data { value, .. }) => *value = cell.value(),
          Some(PropertyDescriptor::Accessor { .. }) => {}
          None => {
            global_object.properties.insert(
              property_key,
              PropertyDescriptor::ordinary_data(cell.value()),
            );
          }
        }
      }
    }
  }

  pub(super) fn initialize_root_bindings(
    &mut self,
    publish_globals: bool,
  ) {
    let slot_names = self
      .execution
      .active
      .chunk
      .root_slot_names
      .clone();
    let slot_tdz = self
      .execution
      .active
      .chunk
      .root_lexical_tdz
      .clone();
    let slot_types = self
      .execution
      .active
      .chunk
      .root_slot_types
      .clone();
    let slot_immutable = self
      .execution
      .active
      .chunk
      .root_slot_immutable
      .clone();
    let slot_sloppy_immutable_silent = vec![false; slot_names.len()];
    let slot_count = slot_names
      .len()
      .max(slot_tdz.len())
      .max(slot_types.len())
      .max(slot_immutable.len());
    if slot_count == 0 {
      return;
    }

    self.execution.active.locals = std::iter::repeat_with(|| BindingCell::new(Value::Undefined))
      .take(slot_count)
      .collect();
    for (idx, is_tdz) in slot_tdz.iter().copied().enumerate() {
      if is_tdz && idx < self.execution.active.locals.len() {
        self.execution.active.locals[idx].set(Value::Uninitialized);
      }
    }

    let root_env = Rc::new(RefCell::new(EnvironmentRecord::new(Some(Rc::clone(
      &self.execution.active.lexical_env,
    )))));
    for (idx, maybe_name) in slot_names.iter().enumerate() {
      if let Some(name) = maybe_name
        && idx < self.execution.active.locals.len()
      {
        let initialized = !slot_tdz.get(idx).copied().unwrap_or(false);
        if slot_immutable.get(idx).copied().unwrap_or(false) {
          root_env.borrow_mut().create_immutable_binding(
            name.clone(),
            self.execution.active.locals[idx].clone(),
            initialized,
          );
        } else if slot_tdz.get(idx).copied().unwrap_or(false) {
          root_env
            .borrow_mut()
            .create_mutable_lexical_binding(
              name.clone(),
              self.execution.active.locals[idx].clone(),
              initialized,
            );
        } else {
          root_env.borrow_mut().create_mutable_binding(
            name.clone(),
            self.execution.active.locals[idx].clone(),
            initialized,
          );
        }
        if initialized && publish_globals && self.current_module_specifier().is_none() {
          let value = self.execution.active.locals[idx].value();
          self
            .realm_state
            .globals
            .borrow_mut()
            .properties
            .insert(
              PropertyKey::String(name.clone().into()),
              PropertyDescriptor::ordinary_data(value),
            );
        }
      }
    }
    self.execution.active.lexical_env = root_env;
    self.execution.active.slots = Some(Rc::new(SlotMetadata {
      names: slot_names,
      immutable: slot_immutable,
      sloppy_immutable_silent: slot_sloppy_immutable_silent,
    }));
  }
}
