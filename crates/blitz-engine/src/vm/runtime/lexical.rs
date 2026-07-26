// SPDX-License-Identifier: MPL-2.0
use crate::environment::EnvironmentRecord;
use crate::errors::RuntimeError;
use crate::value::{PropertyDescriptor, PropertyKey, SymbolId, Value};
use crate::vm::Vm;
use std::cell::RefCell;
use std::rc::Rc;

impl Vm {
  /// Enter a new declarative lexical environment whose outer is the current environment.
  pub fn enter_lexical_scope(&mut self) {
    self.execution.active.lexical_env = Rc::new(RefCell::new(EnvironmentRecord::new(Some(
      Rc::clone(&self.execution.active.lexical_env),
    ))));
  }

  /// Leave the current lexical environment and restore its outer.
  pub fn exit_lexical_scope(&mut self) {
    let outer = self.execution.active.lexical_env.borrow().outer();
    if let Some(outer) = outer {
      self.execution.active.lexical_env = outer;
    }
  }

  /// Check if any root-level slot names in a chunk conflict with the current lexical environment.
  ///
  /// Used during direct eval to detect whether any declarations in the code being evaluated
  /// would shadow existing lexical bindings, which is disallowed in strict mode.
  ///
  /// # Errors
  ///
  /// Returns `None` if no conflicts are found, or `Some(name)` if a conflicting binding name is detected.
  pub(crate) fn direct_eval_lexical_conflict_name(
    &self,
    chunk: &crate::bytecode::Chunk,
  ) -> Option<String> {
    for maybe_name in &chunk.root_slot_names {
      let Some(name) = maybe_name.as_deref() else {
        continue;
      };
      let mut env = Some(Rc::clone(&self.execution.active.lexical_env));
      while let Some(current) = env {
        let (conflict, outer) = {
          let borrowed = current.borrow();
          (
            !borrowed.is_with_environment() && borrowed.has_binding(name),
            borrowed.outer(),
          )
        };
        if conflict {
          return Some(name.to_string());
        }
        env = outer;
      }
    }
    None
  }

  /// Replace the current lexical environment.
  pub(crate) fn set_lexical_env(
    &mut self,
    env: Rc<RefCell<EnvironmentRecord>>,
  ) {
    self.execution.active.lexical_env = env;
  }

  /// Adopt the caller's live bindings and slot metadata for a nested eval run.
  pub(crate) fn adopt_eval_context(
    &mut self,
    parent: &Vm,
  ) {
    self.set_lexical_env(Rc::clone(&parent.execution.active.lexical_env));
  }

  pub(super) fn is_unscopable_binding(
    &mut self,
    binding_object: &Value,
    name: &str,
  ) -> Result<bool, RuntimeError> {
    let unscopables = self.get_property_host(
      binding_object,
      &PropertyKey::Symbol(SymbolId::well_known_unscopables()),
    )?;
    if !unscopables.is_object() {
      return Ok(false);
    }
    Ok(
      self
        .get_property_host(&unscopables, &PropertyKey::String(name.to_string().into()))?
        .is_truthy(),
    )
  }

  fn resolve_lexical_identifier_value(
    &mut self,
    name: &str,
  ) -> Result<Option<Value>, RuntimeError> {
    let mut env = Some(Rc::clone(&self.execution.active.lexical_env));
    while let Some(current) = env {
      let (binding_value, binding_object, with_environment, outer) = {
        let borrowed = current.borrow();
        (
          borrowed.get_own_binding_value(name)?,
          borrowed.binding_object(),
          borrowed.is_with_environment(),
          borrowed.outer(),
        )
      };
      if let Some(value) = binding_value {
        return Ok(Some(value));
      }
      if let Some(binding_object) = binding_object {
        let key = PropertyKey::String(name.to_string().into());
        let found = self.has_property_host(&binding_object, &key)?;
        if found && (!with_environment || !self.is_unscopable_binding(&binding_object, name)?) {
          return Ok(Some(self.get_property_host(&binding_object, &key)?));
        }
      }
      env = outer;
    }

    if let Some(frame) = self.execution.call_stack.last()
      && let Value::Closure { proto, .. } = &frame.callee
      && proto.name == name
    {
      return Ok(Some(frame.callee.clone()));
    }

    Ok(None)
  }

  pub(super) fn set_lexical_identifier_value(
    &mut self,
    name: &str,
    value: Value,
  ) -> Result<bool, RuntimeError> {
    let mut env = Some(Rc::clone(&self.execution.active.lexical_env));
    while let Some(current) = env {
      let (has_decl_binding, binding_object, with_environment, outer) = {
        let borrowed = current.borrow();
        (
          borrowed.has_binding(name),
          borrowed.binding_object(),
          borrowed.is_with_environment(),
          borrowed.outer(),
        )
      };
      if has_decl_binding {
        let current_slot = self
          .execution
          .active
          .slots
          .as_ref()
          .and_then(|slots| {
            slots
              .names
              .iter()
              .enumerate()
              .find_map(|(idx, maybe_name)| {
                maybe_name
                  .as_deref()
                  .filter(|n| *n == name)
                  .map(|_| idx)
              })
          });
        let outer_slot = self
          .execution
          .direct_eval_outer
          .as_ref()
          .and_then(|outer| {
            outer
              .names
              .iter()
              .enumerate()
              .find_map(|(idx, maybe_name)| {
                maybe_name
                  .as_deref()
                  .filter(|n| *n == name)
                  .map(|_| idx)
              })
          });
        if let Some((slot, use_outer)) = current_slot
          .map(|slot| (slot, false))
          .or_else(|| outer_slot.map(|slot| (slot, true)))
        {
          let immutable = if use_outer {
            self
              .execution
              .direct_eval_outer
              .as_ref()
              .and_then(|outer| outer.immutable.get(slot))
              .copied()
              .unwrap_or(false)
          } else {
            self
              .execution
              .active
              .slots
              .as_ref()
              .and_then(|slots| slots.immutable.get(slot))
              .copied()
              .unwrap_or(false)
          };
          if immutable {
            let sloppy_silent = if use_outer {
              self
                .execution
                .direct_eval_outer
                .as_ref()
                .and_then(|outer| outer.sloppy_immutable_silent.get(slot))
                .copied()
                .unwrap_or(false)
            } else {
              self
                .execution
                .active
                .slots
                .as_ref()
                .and_then(|slots| slots.sloppy_immutable_silent.get(slot))
                .copied()
                .unwrap_or(false)
            };
            if self.is_strict_mode() || !sloppy_silent {
              return Err(self.throw_type_error("Assignment to constant variable"));
            }
            return Ok(true);
          }
        }
        let outer_env = outer;
        let result = current
          .borrow_mut()
          .set_mutable_binding(name, value.clone());
        let should_sync_global = outer_env
          .as_ref()
          .is_none_or(|outer_rc| Rc::ptr_eq(outer_rc, &self.realm_state.global_env));
        if should_sync_global {
          self.sync_named_binding_to_global_and_frames(name, &value);
        }
        return result;
      }
      if let Some(binding_object) = binding_object {
        let name_str = name.to_string();
        let key = PropertyKey::String(name_str.clone().into());
        let found = self.has_property_host(&binding_object, &key)?;
        if found && (!with_environment || !self.is_unscopable_binding(&binding_object, name)?) {
          drop(self.set_property_host(&binding_object, &Value::String(name_str.into()), value)?);
          return Ok(true);
        }
      }
      env = outer;
    }
    Ok(false)
  }

  pub(super) fn set_with_env_binding_value(
    &mut self,
    name: &str,
    value: Value,
  ) -> Result<bool, RuntimeError> {
    let mut env = Some(Rc::clone(&self.execution.active.lexical_env));
    while let Some(current) = env {
      let (binding_object, with_environment, outer) = {
        let borrowed = current.borrow();
        (
          borrowed.binding_object(),
          borrowed.is_with_environment(),
          borrowed.outer(),
        )
      };
      if let Some(binding_object) = binding_object {
        let name_str = name.to_string();
        let key = PropertyKey::String(name_str.clone().into());
        let found = self.has_property_host(&binding_object, &key)?;
        if found && (!with_environment || !self.is_unscopable_binding(&binding_object, name)?) {
          drop(self.set_property_host(&binding_object, &Value::String(name_str.into()), value)?);
          self.bump_object_generation(&binding_object);
          return Ok(true);
        }
      }
      env = outer;
    }
    Ok(false)
  }

  pub(super) fn set_current_frame_local_by_name(
    &mut self,
    name: &str,
    value: Value,
  ) -> Result<bool, RuntimeError> {
    let slot = if let Some(slots) = &self.execution.active.slots {
      slots
        .names
        .iter()
        .enumerate()
        .find_map(|(idx, maybe_name)| {
          maybe_name
            .as_deref()
            .filter(|n| *n == name)
            .map(|_| idx)
        })
    } else {
      self
        .execution
        .active
        .chunk
        .root_slot_names
        .iter()
        .enumerate()
        .find_map(|(idx, maybe_name)| {
          maybe_name
            .as_deref()
            .filter(|n| *n == name)
            .map(|_| idx)
        })
    };

    let Some(slot) = slot else {
      return Ok(false);
    };

    super::execution::ensure_slot(&mut self.execution.active.locals, slot);
    *self.execution.active.locals[slot].borrow_mut() = value.clone();
    let _ = self
      .execution
      .active
      .lexical_env
      .borrow_mut()
      .set_binding_initialized_in_chain(name, true);
    self.sync_named_binding_to_global_and_frames(name, &value);

    if self.execution.active.slots.is_none()
      && self
        .execution
        .active
        .chunk
        .root_slot_names
        .get(slot)
        .and_then(Clone::clone)
        .is_some()
    {
      let key = PropertyKey::String(name.to_string().into());
      self
        .realm_state
        .globals
        .borrow_mut()
        .properties
        .insert(key, PropertyDescriptor::ordinary_data(value));
    }

    if let Some(arguments_object) = self.execution.active.arguments.object.clone()
      && slot < self.execution.active.arguments.mapped_count
    {
      let key = PropertyKey::String(slot.to_string().into());
      if let Some((PropertyDescriptor::Data { writable: true, .. }, _)) =
        self.lookup_property_descriptor(&arguments_object, &key, 0)?
      {
        let value = self.execution.active.locals[slot]
          .borrow()
          .clone();
        drop(self.set_property_host(
          &arguments_object,
          &Value::String(slot.to_string().into()),
          value,
        )?);
        self.bump_object_generation(&arguments_object);
      }
    }

    Ok(true)
  }

  pub(super) fn sync_named_binding_to_global_and_frames(
    &mut self,
    name: &str,
    value: &Value,
  ) {
    let key = PropertyKey::String(name.to_string().into());
    self
      .realm_state
      .globals
      .borrow_mut()
      .properties
      .insert(key, PropertyDescriptor::ordinary_data(value.clone()));
    for frame in self.execution.call_stack.iter_mut() {
      if let Some(slots) = &frame.context.slots
        && let Some(slot) = slots
          .names
          .iter()
          .enumerate()
          .find_map(|(idx, maybe_name)| {
            maybe_name
              .as_deref()
              .filter(|n| *n == name)
              .map(|_| idx)
          })
        && let Some(cell) = frame.context.locals.get(slot)
      {
        *cell.borrow_mut() = value.clone();
      }
    }
  }

  /// Resolve a bare identifier against the current lexical/global environment.
  ///
  /// # Errors
  ///
  /// Returns a [`RuntimeError`] if the identifier lookup fails during binding resolution.
  pub fn resolve_identifier_value(
    &mut self,
    name: &str,
  ) -> Result<Option<Value>, RuntimeError> {
    if let Some(value) = self.resolve_lexical_identifier_value(name)? {
      return Ok(Some(value));
    }
    Ok(self.get_global(name))
  }
}
