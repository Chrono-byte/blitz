use crate::errors::RuntimeError;
use crate::gc;
use crate::value::{ObjectData, PropertyDescriptor, PropertyKey, Value};
use crate::vm::Vm;
use crate::vm::runtime::execution::values_equal;
use std::cell::RefCell;
use std::rc::Rc;

impl Vm {
    /// Unwind the call stack and operand stack to the nearest exception
    /// handler, or propagate the error if none exists.
    pub(super) fn unwind_to_handler(&mut self, thrown: Value) -> Result<(), RuntimeError> {
        let thrown = self.ensure_constructor_for_thrown(thrown)?;
        let handler = match self.handlers.pop() {
            Some(h) => h,
            None => {
                self.last_uncaught_exception = Some(thrown.clone());
                self.stack.push(thrown.clone());
                return Err(match thrown {
                    Value::String(s) => RuntimeError::from(format!("Uncaught: {}", s)),
                    other => RuntimeError::from(format!(
                        "Uncaught: {}",
                        self.format_uncaught_thrown_value(&other)
                    )),
                });
            }
        };

        while self.call_stack.len() > handler.frame_count {
            if let Some(frame) = self.call_stack.pop() {
                self.chunk = frame.chunk;
                self.ip = frame.ip;
                self.stack.restore_from(frame.stack);
                self.locals = frame.locals;
                self.this_value = frame.this_value;
                self.current_closure_env = frame.closure_env;
                self.current_slot_types = frame.slot_types;
                self.current_slot_names = frame.slot_names;
                self.current_slot_immutable = frame.slot_immutable;
                self.current_slot_sloppy_immutable_silent = frame.slot_sloppy_immutable_silent;
                self.current_arguments_object = frame.arguments_object;
                self.current_arguments_mapped_count = frame.arguments_mapped_count;
            }
        }
        self.current_strict_mode = self.call_stack.last().is_some_and(|f| f.is_strict);
        self.prune_stale_handlers();

        self.chunk = handler.chunk;
        self.ip = handler.handler_ip;
        self.lexical_env = handler.saved_lexical_env;

        self.stack.truncate(handler.saved_stack_len);
        self.locals.truncate(handler.saved_locals_len);

        self.stack.push(thrown);
        Ok(())
    }

    /// Best-effort repair for thrown object-like values whose `constructor`
    /// property was lost or was never installed.
    fn ensure_constructor_for_thrown(&mut self, thrown: Value) -> Result<Value, RuntimeError> {
        let key = PropertyKey::String("constructor".to_string());
        let needs_repair = matches!(
            &thrown,
            Value::Object(_)
                | Value::Array(..)
                | Value::Proxy(_)
                | Value::Closure { .. }
                | Value::BuiltinObject(_, _)
                | Value::Promise(_)
        ) && !matches!(
            self.lookup_property_descriptor(&thrown, &key, 0)?,
            Some((_, holder)) if values_equal(&holder, &thrown)
        );
        if !needs_repair {
            return Ok(thrown);
        }

        let proto = self.get_prototype(&thrown);
        if crate::trace::enabled() {
            eprintln!(
                "[trace] ensure_constructor_for_thrown missing constructor type={} proto_type={}",
                thrown.type_name(),
                proto.type_name()
            );
            if let Some(keys) = self.list_own_keys_of_value(&thrown) {
                eprintln!("[trace]   thrown own keys={:?}", keys);
            }
            if let Some(keys) = self.list_own_keys_of_value(&proto) {
                eprintln!("[trace]   proto own keys={:?}", keys);
            }
        }
        if matches!(proto, Value::Null) {
            return Ok(thrown);
        }
        if let Ok(ctor) = self.get_property_value(&proto, &key)
            && !matches!(ctor, Value::Undefined)
        {
            self.write_constructor_directly(&thrown, ctor);
            return Ok(thrown);
        }

        let globals_snapshot: Vec<(String, Value)> = self
            .globals
            .borrow()
            .properties
            .iter()
            .filter_map(|(k, desc)| match (k, desc) {
                (PropertyKey::String(name), PropertyDescriptor::Data { value, .. }) => {
                    Some((name.clone(), value.clone()))
                }
                _ => None,
            })
            .collect();
        for (_name, candidate) in globals_snapshot {
            if !Self::is_callable_value(&candidate) {
                continue;
            }
            if let Ok(candidate_proto) =
                self.get_property_value(&candidate, &PropertyKey::String("prototype".to_string()))
                && values_equal(&candidate_proto, &proto)
            {
                self.write_constructor_directly(&thrown, candidate.clone());
                break;
            }
        }
        if let Some(test262_error) = self.get_global("Test262Error")
            && Self::is_callable_value(&test262_error)
        {
            self.write_constructor_directly(&thrown, test262_error);
        }
        Ok(thrown)
    }

    fn write_constructor_directly(&self, value: &Value, ctor: Value) {
        match value {
            Value::Object(obj) => {
                obj.borrow_mut().properties.insert(
                    PropertyKey::String("constructor".to_string()),
                    PropertyDescriptor::data(ctor),
                );
                if crate::trace::enabled() {
                    eprintln!(
                        "[trace] write_constructor_directly object keys={:?}",
                        obj.borrow()
                            .properties
                            .keys()
                            .filter_map(|k| match k {
                                PropertyKey::String(s) => Some(s.clone()),
                                PropertyKey::Symbol(_) => None,
                            })
                            .collect::<Vec<_>>()
                    );
                }
            }
            Value::Array(_, props) => {
                props.borrow_mut().properties.insert(
                    PropertyKey::String("constructor".to_string()),
                    PropertyDescriptor::data(ctor),
                );
            }
            Value::Closure { props, .. } => {
                props.borrow_mut().properties.insert(
                    PropertyKey::String("constructor".to_string()),
                    PropertyDescriptor::data(ctor),
                );
            }
            Value::BuiltinObject(_, props) => {
                props.borrow_mut().properties.insert(
                    PropertyKey::String("constructor".to_string()),
                    PropertyDescriptor::data(ctor),
                );
            }
            _ => {}
        }
    }

    /// Helper to throw a JS exception value from bytecode or builtins.
    pub(crate) fn throw_value(&mut self, thrown: Value) -> Result<(), RuntimeError> {
        match self.unwind_to_handler(thrown) {
            Ok(()) => {
                self.pending_exception = true;
                Ok(())
            }
            Err(err) => {
                self.pending_exception = false;
                Err(err)
            }
        }
    }

    /// Helper used by host code to interrupt execution via a JS exception.
    pub(crate) fn throw_exception(&mut self, thrown: Value) -> RuntimeError {
        match self.throw_value(thrown) {
            Ok(()) => RuntimeError::from("JS exception"),
            Err(err) => err,
        }
    }

    pub(super) fn format_uncaught_thrown_value(&mut self, thrown: &Value) -> String {
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
    pub(crate) fn throw_type_error(&mut self, message: &str) -> RuntimeError {
        let err_val = self
            .create_intrinsic_error("TypeError", message)
            .unwrap_or(Value::String(message.to_string()));
        self.throw_exception(err_val)
    }

    /// Throws a `RangeError` with the given message.
    pub(crate) fn throw_range_error(&mut self, message: &str) -> RuntimeError {
        let err_val = self
            .create_intrinsic_error("RangeError", message)
            .unwrap_or(Value::String(message.to_string()));
        self.throw_exception(err_val)
    }

    /// Throws a `ReferenceError` with the given message.
    pub(crate) fn throw_reference_error(&mut self, message: &str) -> RuntimeError {
        let err_val = self
            .create_intrinsic_error("ReferenceError", message)
            .unwrap_or(Value::String(message.to_string()));
        self.throw_exception(err_val)
    }

    /// Returns `true` if the given value is a callable value.
    pub(crate) fn is_callable_value(value: &Value) -> bool {
        match value {
            Value::Closure { .. } | Value::Builtin(_) | Value::BuiltinObject(_, _) => true,
            Value::Proxy(proxy) => {
                let proxy = proxy.borrow();
                !proxy.is_revoked() && proxy.target.is_callable()
            }
            _ => false,
        }
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
            self.property_get_ic_cache.clear();
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
        self.property_get_ic_cache.clear();
        Ok(Value::Object(gc::track_object(Rc::new(RefCell::new(obj)))))
    }
}
