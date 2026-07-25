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
    let handler = match self.handlers.pop() {
      Some(h) => h,
      None => {
        self.pending_exception_frame_count = None;
        self.last_uncaught_exception = Some(thrown.clone());
        return Err(match thrown {
          Value::String(s) => RuntimeError::from(format!("Uncaught: {}", s)),
          other => RuntimeError::from(format!(
            "Uncaught: {}",
            self.format_uncaught_thrown_value(&other)
          )),
        });
      }
    };

    let mut restored_strict_mode = None;
    while self.call_stack.len() > handler.frame_count {
      if let Some(frame) = self.call_stack.pop() {
        self.chunk = frame.chunk;
        self.ip = frame.ip;
        self.unwound_risc_state = frame.risc_state;
        self.locals = frame.locals;
        self.this_value = frame.this_value;
        self.current_closure_env = frame.closure_env;
        self.current_slot_types = frame.slot_types;
        self.current_slot_names = frame.slot_names;
        self.current_slot_immutable = frame.slot_immutable;
        self.current_slot_sloppy_immutable_silent = frame.slot_sloppy_immutable_silent;
        self.current_arguments_object = frame.arguments_object;
        self.current_arguments_mapped_count = frame.arguments_mapped_count;
        restored_strict_mode = Some(frame.is_strict);
      }
    }
    if let Some(strict) = restored_strict_mode {
      self.current_strict_mode = strict;
    }
    self.prune_stale_handlers();

    self.pending_exception_frame_count = Some(handler.frame_count);
    self.chunk = handler.chunk;
    self.ip = handler.handler_ip;
    self.lexical_env = handler.saved_lexical_env;

    self.locals.truncate(handler.saved_locals_len);

    Ok(())
  }

  /// Helper to throw a JS exception value from bytecode or builtins.
  pub(crate) fn throw_value(
    &mut self,
    thrown: Value,
  ) -> Result<(), RuntimeError> {
    match self.unwind_to_handler(thrown.clone()) {
      Ok(()) => {
        self.pending_exception = Some(thrown);
        Ok(())
      }
      Err(err) => {
        self.pending_exception = None;
        self.pending_exception_frame_count = None;
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
      .get_property_value(thrown, &PropertyKey::String("name".to_string()))
      .ok()
      .and_then(|value| match value {
        Value::String(s) => Some(s),
        _ => None,
      });
    let message: Option<String> = self
      .get_property_value(thrown, &PropertyKey::String("message".to_string()))
      .ok()
      .and_then(|value| match value {
        Value::String(s) => Some(s),
        _ => None,
      });

    match (name, message) {
      (Some(name), Some(message)) if !message.is_empty() => format!("{name}: {message}"),
      (Some(name), _) => name,
      (None, Some(message)) if !message.is_empty() => message,
      _ => self
        .to_string(thrown)
        .unwrap_or_else(|_| format!("{}", thrown)),
    }
  }

  /// Throws a `TypeError` with the given message.
  pub(crate) fn throw_type_error(
    &mut self,
    message: &str,
  ) -> RuntimeError {
    let err_val = self
      .create_intrinsic_error("TypeError", message)
      .unwrap_or(Value::String(message.to_string()));
    self.throw_exception(err_val)
  }

  /// Throws a `RangeError` with the given message.
  pub(crate) fn throw_range_error(
    &mut self,
    message: &str,
  ) -> RuntimeError {
    let err_val = self
      .create_intrinsic_error("RangeError", message)
      .unwrap_or(Value::String(message.to_string()));
    self.throw_exception(err_val)
  }

  /// Throws a `ReferenceError` with the given message.
  pub(crate) fn throw_reference_error(
    &mut self,
    message: &str,
  ) -> RuntimeError {
    let err_val = self
      .create_intrinsic_error("ReferenceError", message)
      .unwrap_or(Value::String(message.to_string()));
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
      .realm
      .get_intrinsic_constructor(name)
      .or_else(|| self.get_global(name))
      .unwrap_or(Value::Undefined);
    if matches!(ctor, Value::Undefined) {
      let mut obj = ObjectData::new();
      obj.properties.insert(
        PropertyKey::String("message".to_string()),
        PropertyDescriptor::data(Value::String(message.to_string())),
      );
      return Ok(Value::Object(gc::track_object(Rc::new(RefCell::new(obj)))));
    }
    let proto = self
      .get_property_host(&ctor, &PropertyKey::String("prototype".to_string()))
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
      PropertyKey::String("constructor".to_string()),
      PropertyDescriptor::data(ctor),
    );
    obj.properties.insert(
      PropertyKey::String("message".to_string()),
      PropertyDescriptor::data(Value::String(message.to_string())),
    );
    obj.properties.insert(
      PropertyKey::String("name".to_string()),
      PropertyDescriptor::data(Value::String(name.to_string())),
    );
    Ok(Value::Object(gc::track_object(Rc::new(RefCell::new(obj)))))
  }
}
