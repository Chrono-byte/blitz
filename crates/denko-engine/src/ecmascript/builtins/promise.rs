// SPDX-License-Identifier: MPL-2.0

use std::cell::RefCell;
use std::rc::Rc;

use crate::errors::RuntimeError;
use crate::runtime_utils::{
  builtin_object, get_iterator_record, get_method, iterator_step, promise_constructor_matches,
  reject_pending_promise, resolve_promise_with_value, set_builtin_length, set_builtin_name,
  static_builtin_arg_offset,
};
use crate::value::{
  BuiltinFn, ObjectData, PromiseState, PropertyDescriptor, PropertyKey, SymbolId, Value,
};
use crate::vm::Vm;

#[derive(Clone)]
struct PromiseCapability {
  promise: Value,
  resolve: Value,
  reject: Value,
}

fn new_promise_capability(
  vm: &mut Vm,
  constructor: &Value,
  function_prototype: &Rc<RefCell<ObjectData>>,
) -> Result<PromiseCapability, RuntimeError> {
  let resolve_slot: Rc<RefCell<Option<Value>>> = Rc::new(RefCell::new(None));
  let reject_slot: Rc<RefCell<Option<Value>>> = Rc::new(RefCell::new(None));
  let resolve_slot_capture = Rc::clone(&resolve_slot);
  let reject_slot_capture = Rc::clone(&reject_slot);
  let executor = builtin_object(
    Rc::new(move |vm, args| {
      let resolve_already_set = resolve_slot_capture
        .borrow()
        .as_ref()
        .is_some_and(|value| !matches!(value, Value::Undefined));
      let reject_already_set = reject_slot_capture
        .borrow()
        .as_ref()
        .is_some_and(|value| !matches!(value, Value::Undefined));
      if resolve_already_set || reject_already_set {
        return Err(vm.throw_type_error("Promise capability executor called more than once"));
      }
      let off = if args.len() > 2 { 1 } else { 0 };
      *resolve_slot_capture.borrow_mut() = args.get(off).cloned();
      *reject_slot_capture.borrow_mut() = args.get(off + 1).cloned();
      Ok(Value::Undefined)
    }),
    function_prototype,
  );
  set_builtin_length(&executor, 2.0);
  set_builtin_name(&executor, "");

  let promise = vm.construct_value(constructor, &[executor])?;
  let resolve = resolve_slot
    .borrow()
    .clone()
    .ok_or_else(|| vm.throw_type_error("Promise capability resolve missing"))?;
  let reject = reject_slot
    .borrow()
    .clone()
    .ok_or_else(|| vm.throw_type_error("Promise capability reject missing"))?;
  if !Vm::is_callable_value(&resolve) || !Vm::is_callable_value(&reject) {
    return Err(vm.throw_type_error("Promise capability resolve and reject must be callable"));
  }
  Ok(PromiseCapability {
    promise,
    resolve,
    reject,
  })
}

fn abrupt_reason(
  error: &RuntimeError,
  thrown: Option<Value>,
) -> Value {
  thrown.unwrap_or_else(|| Value::String(error.to_string().into()))
}

fn type_error_completion(
  vm: &mut Vm,
  message: &str,
) -> (RuntimeError, Option<Value>) {
  vm.capture_completion(|vm| Err::<(), _>(vm.throw_type_error(message)))
    .expect_err("throw_type_error always returns an abrupt completion")
}

fn reject_capability(
  vm: &mut Vm,
  capability: &PromiseCapability,
  reason: Value,
) -> Result<Value, RuntimeError> {
  vm.exceptions.last_uncaught = None;
  vm.call_value_with_this(
    &capability.reject,
    &Value::Undefined,
    std::slice::from_ref(&reason),
  )?;
  Ok(capability.promise.clone())
}

fn close_and_reject(
  vm: &mut Vm,
  capability: &PromiseCapability,
  iterator: &Value,
  reason: Value,
) -> Result<Value, RuntimeError> {
  drop(vm.capture_completion(|vm| {
    let Some(return_method) = get_method(
      vm,
      iterator,
      &PropertyKey::String("return".to_string().into()),
    )?
    else {
      return Ok(());
    };
    vm.call_value_with_this(&return_method, iterator, &[])?;
    Ok(())
  }));
  reject_capability(vm, capability, reason)
}

fn promise_species_constructor(
  vm: &mut Vm,
  promise: &Value,
) -> Result<Value, RuntimeError> {
  let default_constructor = vm
    .realm()
    .intrinsics
    .promise_constructor
    .clone()
    .ok_or_else(|| vm.throw_type_error("Promise constructor is unavailable"))?;
  let constructor = vm.get_property_host(
    promise,
    &PropertyKey::String("constructor".to_string().into()),
  )?;
  if matches!(constructor, Value::Undefined) {
    return Ok(default_constructor);
  }
  if !constructor.is_object() {
    return Err(vm.throw_type_error("Promise constructor property is not an object"));
  }
  let species = vm.get_property_host(
    &constructor,
    &PropertyKey::Symbol(SymbolId::well_known_species()),
  )?;
  if matches!(species, Value::Undefined | Value::Null) {
    return Ok(default_constructor);
  }
  if !species.is_constructable() {
    return Err(vm.throw_type_error("Promise constructor[Symbol.species] is not a constructor"));
  }
  Ok(species)
}

fn settled_result_object(
  object_prototype: &Rc<RefCell<ObjectData>>,
  status: &str,
  key: &str,
  value: Value,
) -> Value {
  let mut result = ObjectData::with_prototype(Value::Object(Rc::clone(object_prototype)));
  for (key, value) in [
    ("status", Value::String(status.to_string().into())),
    (key, value),
  ] {
    result.properties.insert(
      PropertyKey::String(key.to_string().into()),
      PropertyDescriptor::Data {
        value,
        writable: true,
        enumerable: true,
        configurable: true,
      },
    );
  }
  Value::Object(Rc::new(RefCell::new(result)))
}

fn keyed_result_object(entries: &[(PropertyKey, Option<Value>)]) -> Value {
  let mut result = ObjectData::with_prototype(Value::Null);
  for (key, value) in entries {
    result.properties.insert(
      key.clone(),
      PropertyDescriptor::ordinary_data(value.clone().unwrap_or(Value::Undefined)),
    );
  }
  Value::Object(Rc::new(RefCell::new(result)))
}

fn perform_promise_all_keyed(
  vm: &mut Vm,
  args: &[Value],
  function_prototype: &Rc<RefCell<ObjectData>>,
  object_prototype: &Rc<RefCell<ObjectData>>,
  settled: bool,
) -> Result<Value, RuntimeError> {
  let offset = static_builtin_arg_offset(vm, args, "Promise");
  let promises = args
    .get(offset)
    .cloned()
    .unwrap_or(Value::Undefined);
  let constructor = vm.this_value();
  let capability = new_promise_capability(vm, &constructor, function_prototype)?;

  if !promises.is_object() {
    let (error, thrown) = type_error_completion(vm, "Promise keyed argument is not an object");
    return reject_capability(vm, &capability, abrupt_reason(&error, thrown));
  }
  let promise_resolve = match vm
    .capture_completion(|vm| get_method(vm, &constructor, &PropertyKey::String("resolve".into())))
  {
    Ok(Some(resolve)) => resolve,
    Ok(None) => {
      let (error, thrown) =
        type_error_completion(vm, "Promise constructor resolve is not callable");
      return reject_capability(vm, &capability, abrupt_reason(&error, thrown));
    }
    Err((error, thrown)) => {
      return reject_capability(vm, &capability, abrupt_reason(&error, thrown));
    }
  };
  let keys = match vm.capture_completion(|vm| vm.own_property_keys_value(&promises)) {
    Ok(keys) => keys,
    Err((error, thrown)) => {
      return reject_capability(vm, &capability, abrupt_reason(&error, thrown));
    }
  };
  let entries = Rc::new(RefCell::new(Vec::<(PropertyKey, Option<Value>)>::new()));
  let remaining = Rc::new(RefCell::new(1usize));

  for key in keys {
    let descriptor = match vm.capture_completion(|vm| vm.get_own_property_value(&promises, &key)) {
      Ok(descriptor) => descriptor,
      Err((error, thrown)) => {
        return reject_capability(vm, &capability, abrupt_reason(&error, thrown));
      }
    };
    let enumerable = descriptor.is_some_and(|descriptor| match descriptor {
      PropertyDescriptor::Data { enumerable, .. }
      | PropertyDescriptor::Accessor { enumerable, .. } => enumerable,
    });
    if !enumerable {
      continue;
    }
    let value = match vm.capture_completion(|vm| vm.get_property_host(&promises, &key)) {
      Ok(value) => value,
      Err((error, thrown)) => {
        return reject_capability(vm, &capability, abrupt_reason(&error, thrown));
      }
    };
    let index = entries.borrow().len();
    entries.borrow_mut().push((key, None));
    *remaining.borrow_mut() += 1;
    let next_promise = match vm
      .capture_completion(|vm| vm.call_value_with_this(&promise_resolve, &constructor, &[value]))
    {
      Ok(promise) => promise,
      Err((error, thrown)) => {
        return reject_capability(vm, &capability, abrupt_reason(&error, thrown));
      }
    };
    let then = match vm.capture_completion(|vm| {
      vm.get_property_host(&next_promise, &PropertyKey::String("then".into()))
    }) {
      Ok(then) if Vm::is_callable_value(&then) => then,
      Ok(_) => {
        let (error, thrown) =
          type_error_completion(vm, "Promise resolve result then is not callable");
        return reject_capability(vm, &capability, abrupt_reason(&error, thrown));
      }
      Err((error, thrown)) => {
        return reject_capability(vm, &capability, abrupt_reason(&error, thrown));
      }
    };

    let already_called = Rc::new(RefCell::new(false));
    let fulfilled_entries = Rc::clone(&entries);
    let fulfilled_remaining = Rc::clone(&remaining);
    let fulfilled_resolve = capability.resolve.clone();
    let fulfilled_called = Rc::clone(&already_called);
    let fulfilled_object_prototype = Rc::clone(object_prototype);
    let on_fulfilled = builtin_object(
      Rc::new(move |vm, args| {
        if *fulfilled_called.borrow() {
          return Ok(Value::Undefined);
        }
        *fulfilled_called.borrow_mut() = true;
        let offset = usize::from(args.len() > 1);
        let value = args
          .get(offset)
          .cloned()
          .unwrap_or(Value::Undefined);
        let value = if settled {
          settled_result_object(&fulfilled_object_prototype, "fulfilled", "value", value)
        } else {
          value
        };
        fulfilled_entries.borrow_mut()[index].1 = Some(value);
        let mut remaining = fulfilled_remaining.borrow_mut();
        *remaining = remaining.saturating_sub(1);
        if *remaining == 0 {
          let result = keyed_result_object(&fulfilled_entries.borrow());
          vm.call_value_with_this(&fulfilled_resolve, &Value::Undefined, &[result])?;
        }
        Ok(Value::Undefined)
      }),
      function_prototype,
    );
    set_builtin_length(&on_fulfilled, 1.0);
    set_builtin_name(&on_fulfilled, "");

    let on_rejected = if settled {
      let rejected_entries = Rc::clone(&entries);
      let rejected_remaining = Rc::clone(&remaining);
      let rejected_resolve = capability.resolve.clone();
      let rejected_object_prototype = Rc::clone(object_prototype);
      let rejected_called = Rc::clone(&already_called);
      let function = builtin_object(
        Rc::new(move |vm, args| {
          if *rejected_called.borrow() {
            return Ok(Value::Undefined);
          }
          *rejected_called.borrow_mut() = true;
          let offset = usize::from(args.len() > 1);
          let reason = args
            .get(offset)
            .cloned()
            .unwrap_or(Value::Undefined);
          rejected_entries.borrow_mut()[index].1 = Some(settled_result_object(
            &rejected_object_prototype,
            "rejected",
            "reason",
            reason,
          ));
          let mut remaining = rejected_remaining.borrow_mut();
          *remaining = remaining.saturating_sub(1);
          if *remaining == 0 {
            let result = keyed_result_object(&rejected_entries.borrow());
            vm.call_value_with_this(&rejected_resolve, &Value::Undefined, &[result])?;
          }
          Ok(Value::Undefined)
        }),
        function_prototype,
      );
      set_builtin_length(&function, 1.0);
      set_builtin_name(&function, "");
      function
    } else {
      capability.reject.clone()
    };
    if let Err((error, thrown)) = vm.capture_completion(|vm| {
      vm.call_value_with_this(&then, &next_promise, &[on_fulfilled, on_rejected])
    }) {
      return reject_capability(vm, &capability, abrupt_reason(&error, thrown));
    }
  }

  let mut remaining = remaining.borrow_mut();
  *remaining = remaining.saturating_sub(1);
  if *remaining == 0 {
    let result = keyed_result_object(&entries.borrow());
    if let Err((error, thrown)) = vm.capture_completion(|vm| {
      vm.call_value_with_this(&capability.resolve, &Value::Undefined, &[result])
    }) {
      return reject_capability(vm, &capability, abrupt_reason(&error, thrown));
    }
  }
  Ok(capability.promise)
}

fn aggregate_error(
  vm: &mut Vm,
  errors: Vec<Option<Value>>,
) -> Result<Value, RuntimeError> {
  let constructor = vm
    .realm()
    .intrinsics
    .aggregate_error_constructor
    .clone()
    .ok_or_else(|| vm.throw_type_error("AggregateError constructor is unavailable"))?;
  let errors = vm.new_array(errors);
  vm.construct_value(&constructor, &[errors])
}

#[allow(clippy::too_many_lines)]
pub(crate) fn create_promise_intrinsics(
  object_proto_map: &Rc<RefCell<ObjectData>>,
  function_proto_map: &Rc<RefCell<ObjectData>>,
) -> (Rc<RefCell<ObjectData>>, Rc<RefCell<ObjectData>>, Value) {
  use PropertyKey as PKey;

  let promise_proto_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
    ObjectData::with_prototype(Value::Object(Rc::clone(object_proto_map))),
  ));
  let promise_ctor_map: Rc<RefCell<ObjectData>> = Rc::new(RefCell::new(
    ObjectData::with_prototype(Value::Object(Rc::clone(function_proto_map))),
  ));

  let function_proto_for_constructor = Rc::clone(function_proto_map);
  let promise_ctor_fn: BuiltinFn = {
    Rc::new(move |vm, args| {
      let off = static_builtin_arg_offset(vm, args, "Promise");
      let undefined = Value::Undefined;
      let executor = args.get(off).unwrap_or(&undefined);
      if !Vm::is_callable_value(executor) {
        return Err(vm.throw_type_error("Promise executor is not callable"));
      }
      let new_target = vm
        .pending_constructor_new_target()
        .unwrap_or_else(|| vm.this_value());
      let prototype = vm.get_property_host(&new_target, &PropertyKey::prototype())?;
      let prototype = if prototype.is_object() {
        prototype
      } else {
        vm.promise_prototype_value()
      };

      let promise = crate::gc::track_promise(Rc::new(RefCell::new(PromiseState::Pending {
        on_fulfilled: Vec::new(),
        on_rejected: Vec::new(),
      })));

      let promise_for_resolve = Rc::clone(&promise);
      let resolve_called = Rc::new(RefCell::new(false));
      let resolve_called_for_resolve = Rc::clone(&resolve_called);
      let resolve_fn = builtin_object(
        Rc::new(move |vm, args| {
          if *resolve_called_for_resolve.borrow() {
            return Ok(Value::Undefined);
          }
          *resolve_called_for_resolve.borrow_mut() = true;
          let offset = usize::from(vm.builtin_this_arg_prepended());
          let value = args
            .get(offset)
            .cloned()
            .unwrap_or(Value::Undefined);
          if let Err(err) = resolve_promise_with_value(vm, &promise_for_resolve, value) {
            let reason = vm
              .exceptions
              .last_uncaught
              .take()
              .unwrap_or(Value::String(format!("{}", err).into()));
            reject_pending_promise(vm, &promise_for_resolve, reason);
          }
          Ok(Value::Undefined)
        }),
        &function_proto_for_constructor,
      );
      set_builtin_length(&resolve_fn, 1.0);
      set_builtin_name(&resolve_fn, "");

      let promise_for_reject = Rc::clone(&promise);
      let reject_called = resolve_called;
      let reject_fn = builtin_object(
        Rc::new(move |vm, args| {
          if *reject_called.borrow() {
            return Ok(Value::Undefined);
          }
          *reject_called.borrow_mut() = true;
          let offset = usize::from(vm.builtin_this_arg_prepended());
          let reason = args
            .get(offset)
            .cloned()
            .unwrap_or(Value::Undefined);
          reject_pending_promise(vm, &promise_for_reject, reason);
          Ok(Value::Undefined)
        }),
        &function_proto_for_constructor,
      );
      set_builtin_length(&reject_fn, 1.0);
      set_builtin_name(&reject_fn, "");

      vm.exceptions.last_uncaught = None;
      if let Err(err) =
        vm.call_value_with_this(executor, &Value::Undefined, &[resolve_fn, reject_fn])
      {
        let reason = vm
          .exceptions
          .last_uncaught
          .take()
          .unwrap_or(Value::String(format!("{}", err).into()));
        reject_pending_promise(vm, &promise, reason);
      }

      Ok(vm.promise_value_with_prototype(promise, prototype))
    })
  };

  promise_ctor_map.borrow_mut().properties.insert(
    PKey::String("prototype".to_string().into()),
    PropertyDescriptor::builtin_prototype(Value::Object(Rc::clone(&promise_proto_map))),
  );
  promise_ctor_map
    .borrow_mut()
    .set_internal_slot("[[DefersInstanceAllocation]]", Value::Bool(true));
  let promise_ctor_val = Value::BuiltinObject(promise_ctor_fn, Rc::clone(&promise_ctor_map));
  set_builtin_name(&promise_ctor_val, "Promise");
  set_builtin_length(&promise_ctor_val, 1.0);
  let species_getter = builtin_object(
    Rc::new(|_vm, args| Ok(args.first().cloned().unwrap_or(Value::Undefined))),
    function_proto_map,
  );
  set_builtin_name(&species_getter, "get [Symbol.species]");
  set_builtin_length(&species_getter, 0.0);
  promise_ctor_map.borrow_mut().properties.insert(
    PKey::Symbol(SymbolId::well_known_species()),
    PropertyDescriptor::Accessor {
      get: Some(species_getter),
      set: None,
      enumerable: false,
      configurable: true,
    },
  );

  promise_proto_map.borrow_mut().properties.insert(
    PKey::String("constructor".to_string().into()),
    PropertyDescriptor::builtin_data(promise_ctor_val.clone()),
  );
  promise_proto_map.borrow_mut().properties.insert(
    PKey::Symbol(crate::value::SymbolId::well_known_to_string_tag()),
    PropertyDescriptor::Data {
      value: Value::String("Promise".into()),
      writable: false,
      enumerable: false,
      configurable: true,
    },
  );
  let promise_then_function_proto = Rc::clone(function_proto_map);
  promise_proto_map.borrow_mut().properties.insert(
    PKey::String("then".to_string().into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new(move |vm, args| {
        let undefined = Value::Undefined;
        let this = args.first().unwrap_or(&undefined);
        let on_fulfilled = args.get(1).cloned().unwrap_or(Value::Undefined);
        let on_rejected = args.get(2).cloned().unwrap_or(Value::Undefined);
        let on_fulfilled = if Vm::is_callable_value(&on_fulfilled) {
          on_fulfilled
        } else {
          Value::Undefined
        };
        let on_rejected = if Vm::is_callable_value(&on_rejected) {
          on_rejected
        } else {
          Value::Undefined
        };

        if let Value::Promise(promise_rc, _) = this {
          let constructor = promise_species_constructor(vm, this)?;
          let capability = new_promise_capability(vm, &constructor, &promise_then_function_proto)?;

          let mut state = promise_rc.borrow_mut();
          match &mut *state {
            PromiseState::Pending {
              on_fulfilled: existing_fulfilled,
              on_rejected: existing_rejected,
            } => {
              let user_on_fulfilled = on_fulfilled;
              let resolve = capability.resolve.clone();
              let reject = capability.reject.clone();
              let on_fulfilled_fn = Value::Builtin(Rc::new(move |vm, args| {
                let off = if args.len() > 1 { 1 } else { 0 };
                let value = args.get(off).cloned().unwrap_or(Value::Undefined);
                if matches!(user_on_fulfilled, Value::Undefined) {
                  vm.call_value_with_this(&resolve, &Value::Undefined, &[value])?;
                  return Ok(Value::Undefined);
                }
                vm.exceptions.last_uncaught = None;
                match vm.call_value_with_this(&user_on_fulfilled, &Value::Undefined, &[value]) {
                  Ok(out) => {
                    vm.call_value_with_this(&resolve, &Value::Undefined, &[out])?;
                  }
                  Err(err) => {
                    let reason = vm
                      .exceptions
                      .last_uncaught
                      .take()
                      .unwrap_or(Value::String(format!("{}", err).into()));
                    vm.call_value_with_this(&reject, &Value::Undefined, &[reason])?;
                  }
                }
                Ok(Value::Undefined)
              }));

              let user_on_rejected = on_rejected;
              let reject = capability.reject.clone();
              let resolve = capability.resolve.clone();
              let on_rejected_fn = Value::Builtin(Rc::new(move |vm, args| {
                let off = if args.len() > 1 { 1 } else { 0 };
                let reason = args.get(off).cloned().unwrap_or(Value::Undefined);
                if matches!(user_on_rejected, Value::Undefined) {
                  vm.call_value_with_this(&reject, &Value::Undefined, &[reason])?;
                  return Ok(Value::Undefined);
                }
                vm.exceptions.last_uncaught = None;
                match vm.call_value_with_this(&user_on_rejected, &Value::Undefined, &[reason]) {
                  Ok(out) => {
                    vm.call_value_with_this(&resolve, &Value::Undefined, &[out])?;
                  }
                  Err(err) => {
                    let reject_reason = vm
                      .exceptions
                      .last_uncaught
                      .take()
                      .unwrap_or(Value::String(format!("{}", err).into()));
                    vm.call_value_with_this(&reject, &Value::Undefined, &[reject_reason])?;
                  }
                }
                Ok(Value::Undefined)
              }));

              existing_fulfilled.push(on_fulfilled_fn);
              existing_rejected.push(on_rejected_fn);
            }
            PromiseState::Fulfilled(value) => {
              let value = value.clone();
              let user_on_fulfilled = on_fulfilled;
              let resolve = capability.resolve.clone();
              let reject = capability.reject.clone();
              vm.queue_microtask(move |vm| {
                if matches!(user_on_fulfilled, Value::Undefined) {
                  vm.call_value_with_this(
                    &resolve,
                    &Value::Undefined,
                    std::slice::from_ref(&value),
                  )?;
                } else {
                  vm.exceptions.last_uncaught = None;
                  match vm.call_value_with_this(
                    &user_on_fulfilled,
                    &Value::Undefined,
                    std::slice::from_ref(&value),
                  ) {
                    Ok(out) => {
                      vm.call_value_with_this(&resolve, &Value::Undefined, &[out])?;
                    }
                    Err(err) => {
                      let reason = vm
                        .exceptions
                        .last_uncaught
                        .take()
                        .unwrap_or(Value::String(format!("{}", err).into()));
                      vm.call_value_with_this(&reject, &Value::Undefined, &[reason])?;
                    }
                  }
                }
                Ok(())
              });
            }
            PromiseState::Rejected { reason, handled } => {
              *handled = true;
              let reason = reason.clone();
              let user_on_rejected = on_rejected;
              let resolve = capability.resolve.clone();
              let reject = capability.reject.clone();
              vm.queue_microtask(move |vm| {
                if matches!(user_on_rejected, Value::Undefined) {
                  vm.call_value_with_this(
                    &reject,
                    &Value::Undefined,
                    std::slice::from_ref(&reason),
                  )?;
                } else {
                  vm.exceptions.last_uncaught = None;
                  match vm.call_value_with_this(
                    &user_on_rejected,
                    &Value::Undefined,
                    std::slice::from_ref(&reason),
                  ) {
                    Ok(out) => {
                      vm.call_value_with_this(&resolve, &Value::Undefined, &[out])?;
                    }
                    Err(err) => {
                      let reject_reason = vm
                        .exceptions
                        .last_uncaught
                        .take()
                        .unwrap_or(Value::String(format!("{}", err).into()));
                      vm.call_value_with_this(&reject, &Value::Undefined, &[reject_reason])?;
                    }
                  }
                }
                Ok(())
              });
            }
          }

          Ok(capability.promise)
        } else {
          Err(vm.throw_type_error("Promise.prototype.then called on non-promise"))
        }
      }),
      function_proto_map,
    )),
  );
  promise_proto_map.borrow_mut().properties.insert(
    PKey::String("catch".to_string().into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new(|vm, args| {
        let undefined = Value::Undefined;
        let this = args.first().unwrap_or(&undefined);
        let on_rejected = args.get(1).cloned().unwrap_or(Value::Undefined);
        let then_method =
          vm.get_property_host(this, &PropertyKey::String("then".to_string().into()))?;
        vm.call_value_with_this(&then_method, this, &[Value::Undefined, on_rejected])
      }),
      function_proto_map,
    )),
  );
  let promise_finally_function_proto = Rc::clone(function_proto_map);
  let promise_finally = builtin_object(
    Rc::new(move |vm, args| {
      let undefined = Value::Undefined;
      let this = args.first().unwrap_or(&undefined);
      let on_finally = args.get(1).cloned().unwrap_or(Value::Undefined);
      if !matches!(this, Value::Promise(..)) {
        return Err(vm.throw_type_error("Promise.prototype.finally called on non-promise"));
      }

      let constructor = promise_species_constructor(vm, this)?;
      let then = vm.get_property_host(this, &PKey::String("then".to_string().into()))?;
      if !Vm::is_callable_value(&on_finally) {
        return vm.call_value_with_this(&then, this, &[on_finally.clone(), on_finally]);
      }

      let on_finally_for_fulfilled = on_finally.clone();
      let constructor_for_fulfilled = constructor.clone();
      let function_proto_for_fulfilled = Rc::clone(&promise_finally_function_proto);
      let then_finally = builtin_object(
        Rc::new(move |vm, args| {
          let off = if args.len() > 1 { 1 } else { 0 };
          let value = args.get(off).cloned().unwrap_or(Value::Undefined);
          let result =
            vm.call_value_with_this(&on_finally_for_fulfilled, &Value::Undefined, &[])?;
          let resolve = vm.get_property_host(
            &constructor_for_fulfilled,
            &PKey::String("resolve".to_string().into()),
          )?;
          let promise = vm.call_value_with_this(&resolve, &constructor_for_fulfilled, &[result])?;
          let then = vm.get_property_host(&promise, &PKey::String("then".to_string().into()))?;
          let value_thunk = builtin_object(
            Rc::new(move |_vm, _args| Ok(value.clone())),
            &function_proto_for_fulfilled,
          );
          vm.call_value_with_this(&then, &promise, &[value_thunk])
        }),
        &promise_finally_function_proto,
      );
      set_builtin_length(&then_finally, 1.0);
      set_builtin_name(&then_finally, "");

      let on_finally_for_rejected = on_finally;
      let constructor_for_rejected = constructor;
      let function_proto_for_rejected = Rc::clone(&promise_finally_function_proto);
      let catch_finally = builtin_object(
        Rc::new(move |vm, args| {
          let off = if args.len() > 1 { 1 } else { 0 };
          let reason = args.get(off).cloned().unwrap_or(Value::Undefined);
          let result = vm.call_value_with_this(&on_finally_for_rejected, &Value::Undefined, &[])?;
          let resolve = vm.get_property_host(
            &constructor_for_rejected,
            &PKey::String("resolve".to_string().into()),
          )?;
          let promise = vm.call_value_with_this(&resolve, &constructor_for_rejected, &[result])?;
          let then = vm.get_property_host(&promise, &PKey::String("then".to_string().into()))?;
          let thrower = builtin_object(
            Rc::new(move |vm, _args| Err(vm.throw_exception(reason.clone()))),
            &function_proto_for_rejected,
          );
          vm.call_value_with_this(&then, &promise, &[thrower])
        }),
        &promise_finally_function_proto,
      );
      set_builtin_length(&catch_finally, 1.0);
      set_builtin_name(&catch_finally, "");
      vm.call_value_with_this(&then, this, &[then_finally, catch_finally])
    }),
    function_proto_map,
  );
  set_builtin_length(&promise_finally, 1.0);
  set_builtin_name(&promise_finally, "finally");
  promise_proto_map.borrow_mut().properties.insert(
    PKey::String("finally".to_string().into()),
    PropertyDescriptor::builtin_data(promise_finally),
  );

  promise_ctor_map.borrow_mut().properties.insert(
    PKey::String("resolve".to_string().into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new(|vm, args| {
        let off = static_builtin_arg_offset(vm, args, "Promise");
        let undefined = Value::Undefined;
        let value = args.get(off).unwrap_or(&undefined);
        let this_val = vm.this_value();
        if !this_val.is_object() {
          return Err(vm.throw_type_error("Promise.resolve called on non-object"));
        }
        if let Value::Promise(..) = value
          && promise_constructor_matches(vm, value, &this_val)?
        {
          return Ok(value.clone());
        }

        let resolve_slot: Rc<RefCell<Option<Value>>> = Rc::new(RefCell::new(None));
        let reject_slot: Rc<RefCell<Option<Value>>> = Rc::new(RefCell::new(None));
        let resolve_slot_capture = Rc::clone(&resolve_slot);
        let reject_slot_capture = reject_slot;
        let executor = Value::Builtin(Rc::new(move |_vm, executor_args| {
          let off = if executor_args.len() > 2 { 1 } else { 0 };
          *resolve_slot_capture.borrow_mut() = executor_args.get(off).cloned();
          *reject_slot_capture.borrow_mut() = executor_args.get(off + 1).cloned();
          Ok(Value::Undefined)
        }));

        let promise = vm.construct_value(&this_val, &[executor])?;
        let resolve_fn = resolve_slot
          .borrow()
          .clone()
          .ok_or_else(|| vm.throw_type_error("Promise capability resolve missing"))?;
        if let Err(err) =
          vm.call_value_with_this(&resolve_fn, &Value::Undefined, std::slice::from_ref(value))
        {
          let reason = vm
            .exceptions
            .last_uncaught
            .take()
            .unwrap_or(Value::String(format!("{}", err).into()));
          if let Value::Promise(promise_state, _) = &promise {
            reject_pending_promise(vm, promise_state, reason);
          }
        }
        Ok(promise)
      }),
      function_proto_map,
    )),
  );

  let promise_all_function_proto = Rc::clone(function_proto_map);
  let promise_all = builtin_object(
    Rc::new(move |vm, args| {
      let off = static_builtin_arg_offset(vm, args, "Promise");
      let undefined = Value::Undefined;
      let iterable = args.get(off).unwrap_or(&undefined);
      let this_val = vm.this_value();
      if !this_val.is_object() {
        return Err(vm.throw_type_error("Promise.all called on non-object"));
      }

      let capability = new_promise_capability(vm, &this_val, &promise_all_function_proto)?;
      let promise_resolve = match vm.capture_completion(|vm| {
        get_method(vm, &this_val, &PKey::String("resolve".to_string().into()))
      }) {
        Ok(Some(resolve)) => resolve,
        Ok(None) => {
          let (error, thrown) =
            type_error_completion(vm, "Promise constructor resolve is not callable");
          let reason = abrupt_reason(&error, thrown);
          return reject_capability(vm, &capability, reason);
        }
        Err((error, thrown)) => {
          let reason = abrupt_reason(&error, thrown);
          return reject_capability(vm, &capability, reason);
        }
      };

      let iterator_method = match vm.capture_completion(|vm| {
        get_method(vm, iterable, &PKey::Symbol(SymbolId::well_known_iterator()))
      }) {
        Ok(Some(method)) => method,
        Ok(None) => {
          let (error, thrown) = type_error_completion(vm, "Promise.all argument is not iterable");
          let reason = abrupt_reason(&error, thrown);
          return reject_capability(vm, &capability, reason);
        }
        Err((error, thrown)) => {
          let reason = abrupt_reason(&error, thrown);
          return reject_capability(vm, &capability, reason);
        }
      };
      let (iterator, next) =
        match vm.capture_completion(|vm| get_iterator_record(vm, iterable, &iterator_method)) {
          Ok(record) => record,
          Err((error, thrown)) => {
            let reason = abrupt_reason(&error, thrown);
            return reject_capability(vm, &capability, reason);
          }
        };

      let values: Rc<RefCell<Vec<Option<Value>>>> = Rc::new(RefCell::new(Vec::new()));
      let remaining = Rc::new(RefCell::new(1usize));

      loop {
        let next_value = match vm.capture_completion(|vm| iterator_step(vm, &iterator, &next)) {
          Ok(Some(value)) => value,
          Ok(None) => {
            let mut remaining_ref = remaining.borrow_mut();
            *remaining_ref = remaining_ref.saturating_sub(1);
            if *remaining_ref == 0 {
              let array = vm.new_array(values.borrow().clone());
              if let Err((error, thrown)) = vm.capture_completion(|vm| {
                vm.call_value_with_this(&capability.resolve, &Value::Undefined, &[array])
              }) {
                let reason = abrupt_reason(&error, thrown);
                return reject_capability(vm, &capability, reason);
              }
            }
            return Ok(capability.promise);
          }
          Err((error, thrown)) => {
            let reason = abrupt_reason(&error, thrown);
            return reject_capability(vm, &capability, reason);
          }
        };

        let index = values.borrow().len();
        if index >= 9_007_199_254_740_991usize {
          let (error, thrown) = vm
            .capture_completion(|vm| {
              Err::<(), _>(vm.throw_range_error("Promise.all input contains too many elements"))
            })
            .expect_err("throw_range_error always returns an abrupt completion");
          let reason = abrupt_reason(&error, thrown);
          return close_and_reject(vm, &capability, &iterator, reason);
        }
        values.borrow_mut().push(None);
        *remaining.borrow_mut() += 1;

        let next_promise = match vm.capture_completion(|vm| {
          vm.call_value_with_this(&promise_resolve, &this_val, &[next_value])
        }) {
          Ok(promise) => promise,
          Err((error, thrown)) => {
            let reason = abrupt_reason(&error, thrown);
            return close_and_reject(vm, &capability, &iterator, reason);
          }
        };

        let then = match vm.capture_completion(|vm| {
          vm.get_property_host(&next_promise, &PKey::String("then".to_string().into()))
        }) {
          Ok(then) => then,
          Err((error, thrown)) => {
            let reason = abrupt_reason(&error, thrown);
            return close_and_reject(vm, &capability, &iterator, reason);
          }
        };

        let values_for_element = Rc::clone(&values);
        let remaining_for_element = Rc::clone(&remaining);
        let resolve = capability.resolve.clone();
        let already_called = Rc::new(RefCell::new(false));
        let resolve_element = builtin_object(
          Rc::new(move |vm, args| {
            if *already_called.borrow() {
              return Ok(Value::Undefined);
            }
            *already_called.borrow_mut() = true;
            let off = if args.len() > 1 { 1 } else { 0 };
            let value = args.get(off).cloned().unwrap_or(Value::Undefined);
            values_for_element.borrow_mut()[index] = Some(value);
            let mut remaining_ref = remaining_for_element.borrow_mut();
            *remaining_ref = remaining_ref.saturating_sub(1);
            if *remaining_ref == 0 {
              let array = vm.new_array(values_for_element.borrow().clone());
              vm.call_value_with_this(&resolve, &Value::Undefined, &[array])?;
            }
            Ok(Value::Undefined)
          }),
          &promise_all_function_proto,
        );
        set_builtin_length(&resolve_element, 1.0);
        set_builtin_name(&resolve_element, "");

        if let Err((error, thrown)) = vm.capture_completion(|vm| {
          vm.call_value_with_this(
            &then,
            &next_promise,
            &[resolve_element, capability.reject.clone()],
          )
        }) {
          let reason = abrupt_reason(&error, thrown);
          return close_and_reject(vm, &capability, &iterator, reason);
        }
      }
    }),
    function_proto_map,
  );
  set_builtin_length(&promise_all, 1.0);
  set_builtin_name(&promise_all, "all");
  promise_ctor_map.borrow_mut().properties.insert(
    PKey::String("all".to_string().into()),
    PropertyDescriptor::builtin_data(promise_all),
  );

  let promise_race_function_proto = Rc::clone(function_proto_map);
  let promise_race = builtin_object(
    Rc::new(move |vm, args| {
      let off = static_builtin_arg_offset(vm, args, "Promise");
      let undefined = Value::Undefined;
      let iterable = args.get(off).unwrap_or(&undefined);
      let this_val = vm.this_value();
      if !this_val.is_object() {
        return Err(vm.throw_type_error("Promise.race called on non-object"));
      }
      let capability = new_promise_capability(vm, &this_val, &promise_race_function_proto)?;
      let promise_resolve = match vm.capture_completion(|vm| {
        get_method(vm, &this_val, &PKey::String("resolve".to_string().into()))
      }) {
        Ok(Some(resolve)) => resolve,
        Ok(None) => {
          let (error, thrown) =
            type_error_completion(vm, "Promise constructor resolve is not callable");
          return reject_capability(vm, &capability, abrupt_reason(&error, thrown));
        }
        Err((error, thrown)) => {
          return reject_capability(vm, &capability, abrupt_reason(&error, thrown));
        }
      };
      let iterator_method = match vm.capture_completion(|vm| {
        get_method(vm, iterable, &PKey::Symbol(SymbolId::well_known_iterator()))
      }) {
        Ok(Some(method)) => method,
        Ok(None) => {
          let (error, thrown) = type_error_completion(vm, "Promise.race argument is not iterable");
          return reject_capability(vm, &capability, abrupt_reason(&error, thrown));
        }
        Err((error, thrown)) => {
          return reject_capability(vm, &capability, abrupt_reason(&error, thrown));
        }
      };
      let (iterator, next) =
        match vm.capture_completion(|vm| get_iterator_record(vm, iterable, &iterator_method)) {
          Ok(record) => record,
          Err((error, thrown)) => {
            return reject_capability(vm, &capability, abrupt_reason(&error, thrown));
          }
        };

      loop {
        let next_value = match vm.capture_completion(|vm| iterator_step(vm, &iterator, &next)) {
          Ok(Some(value)) => value,
          Ok(None) => return Ok(capability.promise),
          Err((error, thrown)) => {
            return reject_capability(vm, &capability, abrupt_reason(&error, thrown));
          }
        };
        let next_promise = match vm.capture_completion(|vm| {
          vm.call_value_with_this(&promise_resolve, &this_val, &[next_value])
        }) {
          Ok(promise) => promise,
          Err((error, thrown)) => {
            return close_and_reject(vm, &capability, &iterator, abrupt_reason(&error, thrown));
          }
        };
        let then = match vm.capture_completion(|vm| {
          vm.get_property_host(&next_promise, &PKey::String("then".to_string().into()))
        }) {
          Ok(then) => then,
          Err((error, thrown)) => {
            return close_and_reject(vm, &capability, &iterator, abrupt_reason(&error, thrown));
          }
        };
        if let Err((error, thrown)) = vm.capture_completion(|vm| {
          vm.call_value_with_this(
            &then,
            &next_promise,
            &[capability.resolve.clone(), capability.reject.clone()],
          )
        }) {
          return close_and_reject(vm, &capability, &iterator, abrupt_reason(&error, thrown));
        }
      }
    }),
    function_proto_map,
  );
  set_builtin_length(&promise_race, 1.0);
  set_builtin_name(&promise_race, "race");
  promise_ctor_map.borrow_mut().properties.insert(
    PKey::String("race".to_string().into()),
    PropertyDescriptor::builtin_data(promise_race),
  );

  let promise_all_settled_function_proto = Rc::clone(function_proto_map);
  let promise_all_settled_object_proto = Rc::clone(object_proto_map);
  let promise_all_settled = builtin_object(
    Rc::new(move |vm, args| {
      let off = static_builtin_arg_offset(vm, args, "Promise");
      let undefined = Value::Undefined;
      let iterable = args.get(off).unwrap_or(&undefined);
      let this_val = vm.this_value();
      if !this_val.is_object() {
        return Err(vm.throw_type_error("Promise.allSettled called on non-object"));
      }
      let capability = new_promise_capability(vm, &this_val, &promise_all_settled_function_proto)?;
      let promise_resolve = match vm.capture_completion(|vm| {
        get_method(vm, &this_val, &PKey::String("resolve".to_string().into()))
      }) {
        Ok(Some(resolve)) => resolve,
        Ok(None) => {
          let (error, thrown) =
            type_error_completion(vm, "Promise constructor resolve is not callable");
          return reject_capability(vm, &capability, abrupt_reason(&error, thrown));
        }
        Err((error, thrown)) => {
          return reject_capability(vm, &capability, abrupt_reason(&error, thrown));
        }
      };
      let iterator_method = match vm.capture_completion(|vm| {
        get_method(vm, iterable, &PKey::Symbol(SymbolId::well_known_iterator()))
      }) {
        Ok(Some(method)) => method,
        Ok(None) => {
          let (error, thrown) =
            type_error_completion(vm, "Promise.allSettled argument is not iterable");
          return reject_capability(vm, &capability, abrupt_reason(&error, thrown));
        }
        Err((error, thrown)) => {
          return reject_capability(vm, &capability, abrupt_reason(&error, thrown));
        }
      };
      let (iterator, next) =
        match vm.capture_completion(|vm| get_iterator_record(vm, iterable, &iterator_method)) {
          Ok(record) => record,
          Err((error, thrown)) => {
            return reject_capability(vm, &capability, abrupt_reason(&error, thrown));
          }
        };
      let values: Rc<RefCell<Vec<Option<Value>>>> = Rc::new(RefCell::new(Vec::new()));
      let remaining = Rc::new(RefCell::new(1usize));

      loop {
        let next_value = match vm.capture_completion(|vm| iterator_step(vm, &iterator, &next)) {
          Ok(Some(value)) => value,
          Ok(None) => {
            let mut remaining_ref = remaining.borrow_mut();
            *remaining_ref = remaining_ref.saturating_sub(1);
            if *remaining_ref == 0 {
              let array = vm.new_array(values.borrow().clone());
              if let Err((error, thrown)) = vm.capture_completion(|vm| {
                vm.call_value_with_this(&capability.resolve, &Value::Undefined, &[array])
              }) {
                return reject_capability(vm, &capability, abrupt_reason(&error, thrown));
              }
            }
            return Ok(capability.promise);
          }
          Err((error, thrown)) => {
            return reject_capability(vm, &capability, abrupt_reason(&error, thrown));
          }
        };
        let index = values.borrow().len();
        if index >= 9_007_199_254_740_991usize {
          let (error, thrown) = vm
            .capture_completion(|vm| {
              Err::<(), _>(
                vm.throw_range_error("Promise.allSettled input contains too many elements"),
              )
            })
            .expect_err("throw_range_error always returns an abrupt completion");
          return close_and_reject(vm, &capability, &iterator, abrupt_reason(&error, thrown));
        }
        values.borrow_mut().push(None);
        *remaining.borrow_mut() += 1;
        let next_promise = match vm.capture_completion(|vm| {
          vm.call_value_with_this(&promise_resolve, &this_val, &[next_value])
        }) {
          Ok(promise) => promise,
          Err((error, thrown)) => {
            return close_and_reject(vm, &capability, &iterator, abrupt_reason(&error, thrown));
          }
        };
        let then = match vm.capture_completion(|vm| {
          vm.get_property_host(&next_promise, &PKey::String("then".to_string().into()))
        }) {
          Ok(then) => then,
          Err((error, thrown)) => {
            return close_and_reject(vm, &capability, &iterator, abrupt_reason(&error, thrown));
          }
        };
        let already_called = Rc::new(RefCell::new(false));
        let values_for_fulfilled = Rc::clone(&values);
        let remaining_for_fulfilled = Rc::clone(&remaining);
        let resolve_for_fulfilled = capability.resolve.clone();
        let already_called_for_fulfilled = Rc::clone(&already_called);
        let object_proto_for_fulfilled = Rc::clone(&promise_all_settled_object_proto);
        let on_fulfilled = builtin_object(
          Rc::new(move |vm, args| {
            if *already_called_for_fulfilled.borrow() {
              return Ok(Value::Undefined);
            }
            *already_called_for_fulfilled.borrow_mut() = true;
            let off = if args.len() > 1 { 1 } else { 0 };
            let value = args.get(off).cloned().unwrap_or(Value::Undefined);
            values_for_fulfilled.borrow_mut()[index] = Some(settled_result_object(
              &object_proto_for_fulfilled,
              "fulfilled",
              "value",
              value,
            ));
            let mut remaining_ref = remaining_for_fulfilled.borrow_mut();
            *remaining_ref = remaining_ref.saturating_sub(1);
            if *remaining_ref == 0 {
              let array = vm.new_array(values_for_fulfilled.borrow().clone());
              vm.call_value_with_this(&resolve_for_fulfilled, &Value::Undefined, &[array])?;
            }
            Ok(Value::Undefined)
          }),
          &promise_all_settled_function_proto,
        );
        let values_for_rejected = Rc::clone(&values);
        let remaining_for_rejected = Rc::clone(&remaining);
        let resolve_for_rejected = capability.resolve.clone();
        let object_proto_for_rejected = Rc::clone(&promise_all_settled_object_proto);
        let on_rejected = builtin_object(
          Rc::new(move |vm, args| {
            if *already_called.borrow() {
              return Ok(Value::Undefined);
            }
            *already_called.borrow_mut() = true;
            let off = if args.len() > 1 { 1 } else { 0 };
            let reason = args.get(off).cloned().unwrap_or(Value::Undefined);
            values_for_rejected.borrow_mut()[index] = Some(settled_result_object(
              &object_proto_for_rejected,
              "rejected",
              "reason",
              reason,
            ));
            let mut remaining_ref = remaining_for_rejected.borrow_mut();
            *remaining_ref = remaining_ref.saturating_sub(1);
            if *remaining_ref == 0 {
              let array = vm.new_array(values_for_rejected.borrow().clone());
              vm.call_value_with_this(&resolve_for_rejected, &Value::Undefined, &[array])?;
            }
            Ok(Value::Undefined)
          }),
          &promise_all_settled_function_proto,
        );
        if let Err((error, thrown)) = vm.capture_completion(|vm| {
          vm.call_value_with_this(&then, &next_promise, &[on_fulfilled, on_rejected])
        }) {
          return close_and_reject(vm, &capability, &iterator, abrupt_reason(&error, thrown));
        }
      }
    }),
    function_proto_map,
  );
  set_builtin_length(&promise_all_settled, 1.0);
  set_builtin_name(&promise_all_settled, "allSettled");
  promise_ctor_map.borrow_mut().properties.insert(
    PKey::String("allSettled".to_string().into()),
    PropertyDescriptor::builtin_data(promise_all_settled),
  );

  let all_keyed_function_prototype = Rc::clone(function_proto_map);
  let all_keyed_object_prototype = Rc::clone(object_proto_map);
  let all_keyed = builtin_object(
    Rc::new(move |vm, args| {
      perform_promise_all_keyed(
        vm,
        args,
        &all_keyed_function_prototype,
        &all_keyed_object_prototype,
        false,
      )
    }),
    function_proto_map,
  );
  set_builtin_length(&all_keyed, 1.0);
  set_builtin_name(&all_keyed, "allKeyed");
  promise_ctor_map.borrow_mut().properties.insert(
    PKey::String("allKeyed".into()),
    PropertyDescriptor::builtin_data(all_keyed),
  );

  let all_settled_keyed_function_prototype = Rc::clone(function_proto_map);
  let all_settled_keyed_object_prototype = Rc::clone(object_proto_map);
  let all_settled_keyed = builtin_object(
    Rc::new(move |vm, args| {
      perform_promise_all_keyed(
        vm,
        args,
        &all_settled_keyed_function_prototype,
        &all_settled_keyed_object_prototype,
        true,
      )
    }),
    function_proto_map,
  );
  set_builtin_length(&all_settled_keyed, 1.0);
  set_builtin_name(&all_settled_keyed, "allSettledKeyed");
  promise_ctor_map.borrow_mut().properties.insert(
    PKey::String("allSettledKeyed".into()),
    PropertyDescriptor::builtin_data(all_settled_keyed),
  );

  let promise_any_function_proto = Rc::clone(function_proto_map);
  let promise_any = builtin_object(
    Rc::new(move |vm, args| {
      let off = static_builtin_arg_offset(vm, args, "Promise");
      let undefined = Value::Undefined;
      let iterable = args.get(off).unwrap_or(&undefined);
      let this_val = vm.this_value();
      if !this_val.is_object() {
        return Err(vm.throw_type_error("Promise.any called on non-object"));
      }
      let capability = new_promise_capability(vm, &this_val, &promise_any_function_proto)?;
      let promise_resolve = match vm.capture_completion(|vm| {
        get_method(vm, &this_val, &PKey::String("resolve".to_string().into()))
      }) {
        Ok(Some(resolve)) => resolve,
        Ok(None) => {
          let (error, thrown) =
            type_error_completion(vm, "Promise constructor resolve is not callable");
          return reject_capability(vm, &capability, abrupt_reason(&error, thrown));
        }
        Err((error, thrown)) => {
          return reject_capability(vm, &capability, abrupt_reason(&error, thrown));
        }
      };
      let iterator_method = match vm.capture_completion(|vm| {
        get_method(vm, iterable, &PKey::Symbol(SymbolId::well_known_iterator()))
      }) {
        Ok(Some(method)) => method,
        Ok(None) => {
          let (error, thrown) = type_error_completion(vm, "Promise.any argument is not iterable");
          return reject_capability(vm, &capability, abrupt_reason(&error, thrown));
        }
        Err((error, thrown)) => {
          return reject_capability(vm, &capability, abrupt_reason(&error, thrown));
        }
      };
      let (iterator, next) =
        match vm.capture_completion(|vm| get_iterator_record(vm, iterable, &iterator_method)) {
          Ok(record) => record,
          Err((error, thrown)) => {
            return reject_capability(vm, &capability, abrupt_reason(&error, thrown));
          }
        };
      let errors: Rc<RefCell<Vec<Option<Value>>>> = Rc::new(RefCell::new(Vec::new()));
      let remaining = Rc::new(RefCell::new(1usize));

      loop {
        let next_value = match vm.capture_completion(|vm| iterator_step(vm, &iterator, &next)) {
          Ok(Some(value)) => value,
          Ok(None) => {
            let is_last = {
              let mut remaining_ref = remaining.borrow_mut();
              *remaining_ref = remaining_ref.saturating_sub(1);
              *remaining_ref == 0
            };
            if is_last {
              let reason =
                match vm.capture_completion(|vm| aggregate_error(vm, errors.borrow().clone())) {
                  Ok(error) => error,
                  Err((error, thrown)) => abrupt_reason(&error, thrown),
                };
              return reject_capability(vm, &capability, reason);
            }
            return Ok(capability.promise);
          }
          Err((error, thrown)) => {
            return reject_capability(vm, &capability, abrupt_reason(&error, thrown));
          }
        };
        let index = errors.borrow().len();
        if index >= 9_007_199_254_740_991usize {
          let (error, thrown) = vm
            .capture_completion(|vm| {
              Err::<(), _>(vm.throw_range_error("Promise.any input contains too many elements"))
            })
            .expect_err("throw_range_error always returns an abrupt completion");
          return close_and_reject(vm, &capability, &iterator, abrupt_reason(&error, thrown));
        }
        errors.borrow_mut().push(None);
        *remaining.borrow_mut() += 1;
        let next_promise = match vm.capture_completion(|vm| {
          vm.call_value_with_this(&promise_resolve, &this_val, &[next_value])
        }) {
          Ok(promise) => promise,
          Err((error, thrown)) => {
            return close_and_reject(vm, &capability, &iterator, abrupt_reason(&error, thrown));
          }
        };
        let then = match vm.capture_completion(|vm| {
          vm.get_property_host(&next_promise, &PKey::String("then".to_string().into()))
        }) {
          Ok(then) => then,
          Err((error, thrown)) => {
            return close_and_reject(vm, &capability, &iterator, abrupt_reason(&error, thrown));
          }
        };
        let errors_for_rejected = Rc::clone(&errors);
        let remaining_for_rejected = Rc::clone(&remaining);
        let reject_for_rejected = capability.reject.clone();
        let already_called = Rc::new(RefCell::new(false));
        let on_rejected = builtin_object(
          Rc::new(move |vm, args| {
            if *already_called.borrow() {
              return Ok(Value::Undefined);
            }
            *already_called.borrow_mut() = true;
            let off = if args.len() > 1 { 1 } else { 0 };
            let reason = args.get(off).cloned().unwrap_or(Value::Undefined);
            errors_for_rejected.borrow_mut()[index] = Some(reason);
            let is_last = {
              let mut remaining_ref = remaining_for_rejected.borrow_mut();
              *remaining_ref = remaining_ref.saturating_sub(1);
              *remaining_ref == 0
            };
            if is_last {
              let aggregate = aggregate_error(vm, errors_for_rejected.borrow().clone())?;
              vm.call_value_with_this(&reject_for_rejected, &Value::Undefined, &[aggregate])?;
            }
            Ok(Value::Undefined)
          }),
          &promise_any_function_proto,
        );
        if let Err((error, thrown)) = vm.capture_completion(|vm| {
          vm.call_value_with_this(
            &then,
            &next_promise,
            &[capability.resolve.clone(), on_rejected],
          )
        }) {
          return close_and_reject(vm, &capability, &iterator, abrupt_reason(&error, thrown));
        }
      }
    }),
    function_proto_map,
  );
  set_builtin_length(&promise_any, 1.0);
  set_builtin_name(&promise_any, "any");
  promise_ctor_map.borrow_mut().properties.insert(
    PKey::String("any".to_string().into()),
    PropertyDescriptor::builtin_data(promise_any),
  );

  promise_ctor_map.borrow_mut().properties.insert(
    PKey::String("reject".to_string().into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new(|vm, args| {
        let off = static_builtin_arg_offset(vm, args, "Promise");
        let undefined = Value::Undefined;
        let reason = args.get(off).unwrap_or(&undefined);
        let this_val = vm.this_value();
        if !this_val.is_object() {
          return Err(vm.throw_type_error("Promise.reject called on non-object"));
        }

        let resolve_slot: Rc<RefCell<Option<Value>>> = Rc::new(RefCell::new(None));
        let reject_slot: Rc<RefCell<Option<Value>>> = Rc::new(RefCell::new(None));
        let resolve_slot_capture = resolve_slot;
        let reject_slot_capture = Rc::clone(&reject_slot);
        let executor = Value::Builtin(Rc::new(move |_vm, executor_args| {
          let off = if executor_args.len() > 2 { 1 } else { 0 };
          *resolve_slot_capture.borrow_mut() = executor_args.get(off).cloned();
          *reject_slot_capture.borrow_mut() = executor_args.get(off + 1).cloned();
          Ok(Value::Undefined)
        }));

        let promise = vm.construct_value(&this_val, &[executor])?;
        let reject_fn = reject_slot
          .borrow()
          .clone()
          .ok_or_else(|| vm.throw_type_error("Promise capability reject missing"))?;
        if let Err(err) =
          vm.call_value_with_this(&reject_fn, &Value::Undefined, std::slice::from_ref(reason))
        {
          let reject_reason = vm
            .exceptions
            .last_uncaught
            .take()
            .unwrap_or(Value::String(format!("{}", err).into()));
          if let Value::Promise(promise_state, _) = &promise {
            reject_pending_promise(vm, promise_state, reject_reason);
          }
        }
        Ok(promise)
      }),
      function_proto_map,
    )),
  );

  let with_resolvers_object_proto = Rc::clone(object_proto_map);
  let with_resolvers_function_proto = Rc::clone(function_proto_map);
  let with_resolvers = builtin_object(
    Rc::new(move |vm, _args| {
      let constructor = vm.this_value();
      let capability = new_promise_capability(vm, &constructor, &with_resolvers_function_proto)?;
      let mut result =
        ObjectData::with_prototype(Value::Object(Rc::clone(&with_resolvers_object_proto)));
      for (name, value) in [
        ("promise", capability.promise),
        ("resolve", capability.resolve),
        ("reject", capability.reject),
      ] {
        result.properties.insert(
          PKey::String(name.to_string().into()),
          PropertyDescriptor::Data {
            value,
            writable: true,
            enumerable: true,
            configurable: true,
          },
        );
      }
      Ok(Value::Object(Rc::new(RefCell::new(result))))
    }),
    function_proto_map,
  );
  set_builtin_length(&with_resolvers, 0.0);
  set_builtin_name(&with_resolvers, "withResolvers");
  promise_ctor_map.borrow_mut().properties.insert(
    PKey::String("withResolvers".to_string().into()),
    PropertyDescriptor::builtin_data(with_resolvers),
  );

  (promise_proto_map, promise_ctor_map, promise_ctor_val)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn resolving_functions_ignore_the_receiver_when_called_without_arguments() {
    let source = crate::js!(
      r#"
      var results = [];
      var unexpected = false;
      function check(promise, index) {
        var rejects = index === 2 || index === 3 || index === 6 || index === 7;
        return promise.then(
          value => { results.push(!rejects && value === undefined); },
          reason => { results.push(rejects && reason === undefined); }
        );
      }
      var promises = [
        new Promise(resolve => resolve.call(42)),
        new Promise(resolve => resolve.apply({ marker: 1 }, [])),
        new Promise((resolve, reject) => reject.call(42)),
        new Promise((resolve, reject) => reject.apply({ marker: 1 }, [])),
        Promise.resolve({ then(resolve) { resolve.call(42); } }),
        Promise.resolve({ then(resolve) { resolve.apply({ marker: 1 }, []); } }),
        Promise.resolve({ then(resolve, reject) { reject.call(42); } }),
        Promise.resolve({ then(resolve, reject) { reject.apply({ marker: 1 }, []); } })
      ];
      Promise.all(promises.map(check)).then(
        () => { result = results.join(','); },
        () => { unexpected = true; }
      );
      var result = '';
    "#
    );
    let session = crate::parser::Parser::parse_source(source).expect("parse");
    let code = crate::compiler::compile_session(&session).expect("compile");
    let mut vm = Vm::new(Rc::new(RefCell::new(ObjectData::new())));
    vm.run(code).expect("run");
    vm.flush_microtasks().expect("flush");
    assert_eq!(vm.get_global("unexpected"), Some(Value::Bool(false)));
    assert_eq!(
      vm.get_global("result"),
      Some(Value::String(
        "true,true,true,true,true,true,true,true".into()
      ))
    );
  }
}
