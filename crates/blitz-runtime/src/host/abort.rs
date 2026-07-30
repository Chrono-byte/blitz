// SPDX-License-Identifier: MPL-2.0
//! Host-owned AbortController and AbortSignal compatibility.
//!
//! These objects are deliberately installed by the runtime host rather than
//! the engine's raw realm.  They are web-platform capabilities used by Effect
//! and are not ECMAScript intrinsics.

use std::cell::RefCell;
use std::rc::Rc;

use denko::Vm;
use denko::errors::RuntimeError;
use denko::gc;
use denko::value::{ArrayData, BuiltinFn, ObjectData, PropertyDescriptor, PropertyKey, Value};

use super::{host_constructor, host_function};

const ABORTED: &str = "aborted";
const REASON: &str = "reason";
const SIGNAL: &str = "signal";
const ON_ABORT: &str = "onabort";
const LISTENERS_SLOT: &str = "[[AbortListeners]]";

/// Install the host's AbortController and AbortSignal constructors.
pub fn install(vm: &mut Vm) {
  let object_prototype = vm.realm().object_prototype();
  let signal_prototype = object(object_prototype.clone());
  let controller_prototype = object(object_prototype);

  install_signal_methods(vm, &signal_prototype);
  install_controller_method(vm, &controller_prototype);

  let signal_constructor = make_signal_constructor(vm, signal_prototype);
  let controller_constructor =
    make_controller_constructor(vm, controller_prototype, signal_constructor.clone());
  let global = vm.realm().global();
  let mut global = global.borrow_mut();
  global.properties.insert(
    PropertyKey::String("AbortSignal".to_string().into()),
    PropertyDescriptor::ordinary_data(signal_constructor),
  );
  global.properties.insert(
    PropertyKey::String("AbortController".to_string().into()),
    PropertyDescriptor::ordinary_data(controller_constructor),
  );
}

fn make_signal_constructor(
  vm: &Vm,
  signal_prototype: Value,
) -> Value {
  let prototype = signal_prototype.clone();
  let constructor_prototype = prototype.clone();
  let constructor: BuiltinFn = Rc::new(move |vm, args| {
    if vm.current_new_target().is_none() {
      return Err(RuntimeError::from(
        "TypeError: Constructor AbortSignal requires 'new'",
      ));
    }
    let reason = args
      .get(1)
      .cloned()
      .unwrap_or_else(default_reason);
    Ok(make_signal(constructor_prototype.clone(), true, reason))
  });
  let value = host_constructor(vm, "AbortSignal", 0, signal_prototype, constructor);
  let Value::BuiltinObject(_, constructor_map) = &value else {
    unreachable!("host constructors are builtin objects");
  };
  let constructor_map_value = Value::Object(Rc::clone(constructor_map));

  let static_abort = Rc::new({
    move |vm: &mut Vm, args: &[Value]| {
      let offset = usize::from(vm.builtin_this_arg_prepended());
      let reason = args
        .get(offset)
        .cloned()
        .unwrap_or_else(default_reason);
      Ok(make_signal(prototype.clone(), true, reason))
    }
  });
  put(
    &constructor_map_value,
    "abort",
    host_function(vm, "abort", 0, static_abort),
  );
  value
}

fn make_controller_constructor(
  vm: &Vm,
  controller_prototype: Value,
  signal_constructor: Value,
) -> Value {
  let signal_prototype = get_property(&signal_constructor, "prototype");
  let controller_instance_prototype = controller_prototype.clone();
  let constructor: BuiltinFn = Rc::new(move |vm, _args| {
    if vm.current_new_target().is_none() {
      return Err(RuntimeError::from(
        "TypeError: Constructor AbortController requires 'new'",
      ));
    }
    let signal = make_signal(signal_prototype.clone(), false, Value::Undefined);
    let controller = object(controller_instance_prototype.clone());
    put(&controller, SIGNAL, signal);
    Ok(controller)
  });
  host_constructor(vm, "AbortController", 0, controller_prototype, constructor)
}

fn install_signal_methods(
  vm: &Vm,
  prototype: &Value,
) {
  let proto = object_data(prototype);
  let proto_value = Value::Object(Rc::clone(&proto));
  put(
    &proto_value,
    "addEventListener",
    host_function(vm, "addEventListener", 2, Rc::new(add_event_listener)),
  );
  put(
    &proto_value,
    "removeEventListener",
    host_function(vm, "removeEventListener", 2, Rc::new(remove_event_listener)),
  );
  put(
    &proto_value,
    "dispatchEvent",
    host_function(vm, "dispatchEvent", 1, Rc::new(dispatch_event)),
  );
  put(
    &proto_value,
    "throwIfAborted",
    host_function(vm, "throwIfAborted", 0, Rc::new(throw_if_aborted)),
  );
  put(&proto_value, "onabort", Value::Undefined);
  put(&proto_value, "constructor", prototype.clone());
}

fn install_controller_method(
  vm: &Vm,
  prototype: &Value,
) {
  let proto = object_data(prototype);
  let proto_value = Value::Object(Rc::clone(&proto));
  put(
    &proto_value,
    "abort",
    host_function(vm, "abort", 0, Rc::new(abort_controller)),
  );
}

fn make_signal(
  prototype: Value,
  aborted: bool,
  reason: Value,
) -> Value {
  let signal = object(prototype);
  put(&signal, ABORTED, Value::Bool(aborted));
  put(&signal, REASON, reason);
  put(&signal, ON_ABORT, Value::Undefined);
  object_data(&signal)
    .borrow_mut()
    .set_internal_slot(LISTENERS_SLOT, listener_list());
  signal
}

fn abort_controller(
  vm: &mut Vm,
  args: &[Value],
) -> Result<Value, RuntimeError> {
  let receiver = vm.this_value();
  let signal = get_property(&receiver, SIGNAL);
  if !signal.is_object() {
    return Err(RuntimeError::from(
      "TypeError: AbortController.prototype.abort called on incompatible receiver",
    ));
  }
  if matches!(get_property(&signal, ABORTED), Value::Bool(true)) {
    return Ok(Value::Undefined);
  }
  let reason = args
    .get(1)
    .cloned()
    .unwrap_or_else(default_reason);
  put(&signal, ABORTED, Value::Bool(true));
  put(&signal, REASON, reason);
  dispatch_abort(vm, &signal)?;
  Ok(Value::Undefined)
}

fn add_event_listener(
  vm: &mut Vm,
  args: &[Value],
) -> Result<Value, RuntimeError> {
  let signal = vm.this_value();
  let event_type = args.get(1).cloned().unwrap_or(Value::Undefined);
  if vm.to_string(&event_type)?.as_str_lossy() != "abort" {
    return Ok(Value::Undefined);
  }
  let Some(listener) = args
    .get(2)
    .filter(|value| value.is_callable())
    .cloned()
  else {
    return Ok(Value::Undefined);
  };
  if matches!(get_property(&signal, ABORTED), Value::Bool(true)) {
    vm.call_value_with_this(&listener, &Value::Undefined, &[abort_event(&signal)])?;
    return Ok(Value::Undefined);
  }
  let once = args
    .get(3)
    .and_then(|options| get_property(options, "once").as_bool())
    .unwrap_or(false);
  if let Some(listeners) = signal_listeners(&signal) {
    listeners
      .borrow_mut()
      .push(Some(listener_record(listener, once)));
  }
  Ok(Value::Undefined)
}

fn remove_event_listener(
  vm: &mut Vm,
  args: &[Value],
) -> Result<Value, RuntimeError> {
  let signal = vm.this_value();
  let event_type = args.get(1).cloned().unwrap_or(Value::Undefined);
  if vm.to_string(&event_type)?.as_str_lossy() != "abort" {
    return Ok(Value::Undefined);
  }
  let Some(listener) = args.get(2) else {
    return Ok(Value::Undefined);
  };
  let Some(listeners) = signal_listeners(&signal) else {
    return Ok(Value::Undefined);
  };
  for record in listeners.borrow_mut().iter_mut() {
    if record
      .as_ref()
      .is_some_and(|record| same_value(&get_property(record, "listener"), listener))
    {
      *record = None;
    }
  }
  Ok(Value::Undefined)
}

fn dispatch_event(
  vm: &mut Vm,
  args: &[Value],
) -> Result<Value, RuntimeError> {
  let signal = vm.this_value();
  let event = args.get(1).cloned().unwrap_or(Value::Undefined);
  if matches!(get_property(&event, "type"), Value::String(ref value) if value == "abort") {
    dispatch_abort_with_event(vm, &signal, event)?;
  }
  Ok(Value::Bool(true))
}

fn throw_if_aborted(
  vm: &mut Vm,
  _args: &[Value],
) -> Result<Value, RuntimeError> {
  let signal = vm.this_value();
  if matches!(get_property(&signal, ABORTED), Value::Bool(true)) {
    return Err(RuntimeError::from("AbortError: This operation was aborted"));
  }
  Ok(Value::Undefined)
}

fn dispatch_abort(
  vm: &mut Vm,
  signal: &Value,
) -> Result<(), RuntimeError> {
  dispatch_abort_with_event(vm, signal, abort_event(signal))
}

fn dispatch_abort_with_event(
  vm: &mut Vm,
  signal: &Value,
  event: Value,
) -> Result<(), RuntimeError> {
  let onabort = get_property(signal, ON_ABORT);
  if onabort.is_callable() {
    drop(vm.call_value_with_this(&onabort, signal, std::slice::from_ref(&event)));
  }
  let Some(listeners) = signal_listeners(signal) else {
    return Ok(());
  };
  let records = listeners.borrow().clone_elements();
  for (index, record) in records.into_iter().enumerate() {
    let Some(record) = record else {
      continue;
    };
    let callback = get_property(&record, "listener");
    if callback.is_callable() {
      drop(vm.call_value_with_this(&callback, signal, std::slice::from_ref(&event)));
    }
    if get_property(&record, "once")
      .as_bool()
      .unwrap_or(false)
      && let Some(slot) = listeners.borrow_mut().get_mut(index)
    {
      *slot = None;
    }
  }
  Ok(())
}

fn listener_list() -> Value {
  Value::Array(
    gc::track_array(Rc::new(RefCell::new(ArrayData::new(Vec::new())))),
    gc::track_object(Rc::new(RefCell::new(ObjectData::new()))),
  )
}

fn signal_listeners(signal: &Value) -> Option<Rc<RefCell<ArrayData>>> {
  let Value::Object(signal) = signal else {
    return None;
  };
  let listeners = signal
    .borrow()
    .internal_slot(LISTENERS_SLOT)?
    .clone();
  match listeners {
    Value::Array(listeners, _) => Some(listeners),
    _ => None,
  }
}

fn listener_record(
  listener: Value,
  once: bool,
) -> Value {
  let record = object(Value::Null);
  put(&record, "listener", listener);
  put(&record, "once", Value::Bool(once));
  record
}

fn abort_event(signal: &Value) -> Value {
  let event = object(Value::Null);
  put(&event, "type", Value::String("abort".to_string().into()));
  put(&event, "target", signal.clone());
  put(&event, "currentTarget", signal.clone());
  event
}

fn default_reason() -> Value {
  let reason = object(Value::Null);
  put(
    &reason,
    "name",
    Value::String("AbortError".to_string().into()),
  );
  put(
    &reason,
    "message",
    Value::String("This operation was aborted".to_string().into()),
  );
  reason
}

fn object(prototype: Value) -> Value {
  Value::Object(gc::track_object(Rc::new(RefCell::new(
    ObjectData::with_prototype(prototype),
  ))))
}

fn object_data(value: &Value) -> Rc<RefCell<ObjectData>> {
  match value {
    Value::Object(object) => Rc::clone(object),
    _ => unreachable!("Abort host object expected"),
  }
}

fn put(
  target: &Value,
  name: &str,
  value: Value,
) {
  if let Value::Object(object) = target {
    object.borrow_mut().properties.insert(
      PropertyKey::String(name.to_string().into()),
      PropertyDescriptor::Data {
        value,
        writable: true,
        enumerable: false,
        configurable: true,
      },
    );
  }
}

fn get_property(
  target: &Value,
  name: &str,
) -> Value {
  match target {
    Value::Object(object) => object
      .borrow()
      .properties
      .get(&PropertyKey::String(name.to_string().into()))
      .and_then(|descriptor| match descriptor {
        PropertyDescriptor::Data { value, .. } => Some(value.clone()),
        PropertyDescriptor::Accessor { .. } => None,
      })
      .unwrap_or(Value::Undefined),
    Value::BuiltinObject(_, object) => object
      .borrow()
      .properties
      .get(&PropertyKey::String(name.to_string().into()))
      .and_then(|descriptor| match descriptor {
        PropertyDescriptor::Data { value, .. } => Some(value.clone()),
        PropertyDescriptor::Accessor { .. } => None,
      })
      .unwrap_or(Value::Undefined),
    _ => Value::Undefined,
  }
}

fn same_value(
  left: &Value,
  right: &Value,
) -> bool {
  match (left, right) {
    (Value::Object(left), Value::Object(right)) => Rc::ptr_eq(left, right),
    (Value::String(left), Value::String(right)) => left == right,
    (Value::Bool(left), Value::Bool(right)) => left == right,
    (Value::Undefined, Value::Undefined) | (Value::Null, Value::Null) => true,
    _ => false,
  }
}

trait BoolValue {
  fn as_bool(&self) -> Option<bool>;
}

impl BoolValue for Value {
  fn as_bool(&self) -> Option<bool> {
    match self {
      Value::Bool(value) => Some(*value),
      _ => None,
    }
  }
}
