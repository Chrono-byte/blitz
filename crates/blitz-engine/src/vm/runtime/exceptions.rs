// SPDX-License-Identifier: MPL-2.0
use crate::errors::RuntimeError;
use crate::gc;
use crate::value::{ObjectData, PropertyDescriptor, PropertyKey, Value};
use crate::vm::Vm;
use std::cell::RefCell;
use std::rc::Rc;

impl Vm {
  /// Unwind call frames to the nearest exception handler, or propagate the
  /// error if none exists.
  pub(super) fn unwind_to_handler(
    &mut self,
    thrown: Value,
  ) -> Result<(), RuntimeError> {
    let handler = match self.exceptions.handlers.pop() {
      Some(h) => h,
      None => {
        self.exceptions.pending_frame_count = None;
        self.exceptions.last_uncaught = Some(thrown.clone());
        return Err(match thrown {
          Value::String(s) => RuntimeError::from(format!("Uncaught: {}", s)),
          other => RuntimeError::from(format!(
            "Uncaught: {}",
            self.format_uncaught_thrown_value(&other)
          )),
        });
      }
    };

    while self.execution.call_stack.len() > handler.frame_count {
      if let Some(frame) = self.execution.call_stack.pop() {
        self.exceptions.unwound_risc_state = frame.risc_state;
        self.execution.active = frame.context;
      }
    }
    self.prune_stale_handlers();

    self.exceptions.pending_frame_count = Some(handler.frame_count);
    self.execution.active.chunk = handler.chunk;
    self.execution.active.ip = handler.handler_ip;
    self.execution.active.lexical_env = handler.saved_lexical_env;

    self
      .execution
      .active
      .locals
      .truncate(handler.saved_locals_len);

    Ok(())
  }

  /// Helper to throw a JS exception value from bytecode or builtins.
  pub(crate) fn throw_value(
    &mut self,
    thrown: Value,
  ) -> Result<(), RuntimeError> {
    match self.unwind_to_handler(thrown.clone()) {
      Ok(()) => {
        self.exceptions.pending = Some(thrown);
        Ok(())
      }
      Err(err) => {
        self.exceptions.pending = None;
        self.exceptions.pending_frame_count = None;
        Err(err)
      }
    }
  }

  /// Helper used by host code to interrupt execution via a JS exception.
  pub(crate) fn throw_exception(
    &mut self,
    thrown: Value,
  ) -> RuntimeError {
    match self.throw_value(thrown) {
      Ok(()) => RuntimeError::from("JS exception"),
      Err(err) => err,
    }
  }

  pub(super) fn format_uncaught_thrown_value(
    &mut self,
    thrown: &Value,
  ) -> String {
    let name: Option<String> = self
      .get_property_value(thrown, &PropertyKey::String("name".to_string().into()))
      .ok()
      .and_then(|value| match value {
        Value::String(s) => Some(s.to_string_lossy()),
        _ => None,
      });
    let message: Option<String> = self
      .get_property_value(thrown, &PropertyKey::String("message".to_string().into()))
      .ok()
      .and_then(|value| match value {
        Value::String(s) => Some(s.to_string_lossy()),
        _ => None,
      });

    match (name, message) {
      (Some(name), Some(message)) if !message.is_empty() => format!("{name}: {message}"),
      (Some(name), _) => name,
      (None, Some(message)) if !message.is_empty() => message,
      _ => self
        .to_string(thrown)
        .unwrap_or_else(|_| format!("{}", thrown).into())
        .to_string(),
    }
  }

  /// Throws a `TypeError` with the given message.
  pub(crate) fn throw_type_error(
    &mut self,
    message: &str,
  ) -> RuntimeError {
    let err_val = self
      .create_intrinsic_error("TypeError", message)
      .unwrap_or(Value::String(message.to_string().into()));
    self.throw_exception(err_val)
  }

  /// Throws a `RangeError` with the given message.
  pub(crate) fn throw_range_error(
    &mut self,
    message: &str,
  ) -> RuntimeError {
    let err_val = self
      .create_intrinsic_error("RangeError", message)
      .unwrap_or(Value::String(message.to_string().into()));
    self.throw_exception(err_val)
  }

  /// Throws a `ReferenceError` with the given message.
  pub(crate) fn throw_reference_error(
    &mut self,
    message: &str,
  ) -> RuntimeError {
    let err_val = self
      .create_intrinsic_error("ReferenceError", message)
      .unwrap_or(Value::String(message.to_string().into()));
    self.throw_exception(err_val)
  }

  /// Returns `true` if the given value is a callable value.
  pub(crate) fn is_callable_value(value: &Value) -> bool {
    value.is_callable()
  }

  /// Creates an intrinsic error value with the given name and message.
  pub(crate) fn create_intrinsic_error(
    &mut self,
    name: &str,
    message: &str,
  ) -> Result<Value, RuntimeError> {
    let ctor = self
      .realm_state
      .realm
      .get_intrinsic_constructor(name)
      .or_else(|| self.get_global(name))
      .unwrap_or(Value::Undefined);
    if matches!(ctor, Value::Undefined) {
      let mut obj = ObjectData::new();
      obj.properties.insert(
        PropertyKey::String("message".to_string().into()),
        PropertyDescriptor::ordinary_data(Value::String(message.to_string().into())),
      );
      return Ok(Value::Object(gc::track_object(Rc::new(RefCell::new(obj)))));
    }
    let proto = self
      .get_property_host(&ctor, &PropertyKey::String("prototype".to_string().into()))
      .unwrap_or(Value::Null);
    let mut obj = ObjectData::with_prototype(match proto {
      Value::Object(_)
      | Value::Array(..)
      | Value::Proxy(_)
      | Value::Closure { .. }
      | Value::BuiltinObject(_, _) => proto,
      _ => Value::Null,
    });
    obj.properties.insert(
      PropertyKey::String("constructor".to_string().into()),
      PropertyDescriptor::ordinary_data(ctor),
    );
    obj.properties.insert(
      PropertyKey::String("message".to_string().into()),
      PropertyDescriptor::ordinary_data(Value::String(message.to_string().into())),
    );
    obj.properties.insert(
      PropertyKey::String("name".to_string().into()),
      PropertyDescriptor::ordinary_data(Value::String(name.to_string().into())),
    );
    Ok(Value::Object(gc::track_object(Rc::new(RefCell::new(obj)))))
  }
}
