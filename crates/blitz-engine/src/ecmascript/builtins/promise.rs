// SPDX-License-Identifier: MPL-2.0
//! Promise builtin functions and prototype methods

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
  // IteratorClose observes `return`, but an existing throw completion wins
  // over any abrupt completion produced while closing.
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

/// 27.2 Promise Objects
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
          let value = if let Some(value) = args.get(1) {
            value.clone()
          } else if let Some(value) = args.first() {
            value.clone()
          } else {
            Value::Undefined
          };
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
          let reason = if let Some(reason) = args.get(1) {
            reason.clone()
          } else if let Some(reason) = args.first() {
            reason.clone()
          } else {
            Value::Undefined
          };
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

      Ok(Value::Promise(promise))
    })
  };

  promise_ctor_map.borrow_mut().properties.insert(
    PKey::String("prototype".to_string().into()),
    PropertyDescriptor::builtin_prototype(Value::Object(Rc::clone(&promise_proto_map))),
  );
  let promise_ctor_val = Value::BuiltinObject(promise_ctor_fn, Rc::clone(&promise_ctor_map));
  set_builtin_name(&promise_ctor_val, "Promise");
  set_builtin_length(&promise_ctor_val, 1.0);

  promise_proto_map.borrow_mut().properties.insert(
    PKey::String("constructor".to_string().into()),
    PropertyDescriptor::builtin_data(promise_ctor_val.clone()),
  );
  promise_proto_map.borrow_mut().properties.insert(
    PKey::String("then".to_string().into()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new(|vm, args| {
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

        if let Value::Promise(promise_rc) = this {
          let result_promise =
            crate::gc::track_promise(Rc::new(RefCell::new(PromiseState::Pending {
              on_fulfilled: Vec::new(),
              on_rejected: Vec::new(),
            })));
          let result_promise_value = Value::Promise(Rc::clone(&result_promise));

          let mut state = promise_rc.borrow_mut();
          match &mut *state {
            PromiseState::Pending {
              on_fulfilled: existing_fulfilled,
              on_rejected: existing_rejected,
            } => {
              let user_on_fulfilled = on_fulfilled;
              let res_p_f = Rc::clone(&result_promise);
              let on_fulfilled_fn = Value::Builtin(Rc::new(move |vm, args| {
                let off = if args.len() > 1 { 1 } else { 0 };
                let value = args.get(off).cloned().unwrap_or(Value::Undefined);
                if matches!(user_on_fulfilled, Value::Undefined) {
                  resolve_promise_with_value(vm, &res_p_f, value)?;
                  return Ok(Value::Undefined);
                }
                vm.exceptions.last_uncaught = None;
                match vm.call_value_with_this(&user_on_fulfilled, &Value::Undefined, &[value]) {
                  Ok(out) => resolve_promise_with_value(vm, &res_p_f, out)?,
                  Err(err) => {
                    let reason = vm
                      .exceptions
                      .last_uncaught
                      .take()
                      .unwrap_or(Value::String(format!("{}", err).into()));
                    reject_pending_promise(vm, &res_p_f, reason);
                  }
                }
                Ok(Value::Undefined)
              }));

              let user_on_rejected = on_rejected;
              let res_p_r = result_promise;
              let on_rejected_fn = Value::Builtin(Rc::new(move |vm, args| {
                let off = if args.len() > 1 { 1 } else { 0 };
                let reason = args.get(off).cloned().unwrap_or(Value::Undefined);
                if matches!(user_on_rejected, Value::Undefined) {
                  reject_pending_promise(vm, &res_p_r, reason);
                  return Ok(Value::Undefined);
                }
                vm.exceptions.last_uncaught = None;
                match vm.call_value_with_this(&user_on_rejected, &Value::Undefined, &[reason]) {
                  Ok(out) => resolve_promise_with_value(vm, &res_p_r, out)?,
                  Err(err) => {
                    let reject_reason = vm
                      .exceptions
                      .last_uncaught
                      .take()
                      .unwrap_or(Value::String(format!("{}", err).into()));
                    reject_pending_promise(vm, &res_p_r, reject_reason);
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
              let res_p_f = result_promise;
              vm.queue_microtask(move |vm| {
                if matches!(user_on_fulfilled, Value::Undefined) {
                  resolve_promise_with_value(vm, &res_p_f, value.clone())?;
                } else {
                  vm.exceptions.last_uncaught = None;
                  match vm.call_value_with_this(
                    &user_on_fulfilled,
                    &Value::Undefined,
                    std::slice::from_ref(&value),
                  ) {
                    Ok(out) => resolve_promise_with_value(vm, &res_p_f, out)?,
                    Err(err) => {
                      let reason = vm
                        .exceptions
                        .last_uncaught
                        .take()
                        .unwrap_or(Value::String(format!("{}", err).into()));
                      reject_pending_promise(vm, &res_p_f, reason);
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
              let res_p_r = result_promise;
              vm.queue_microtask(move |vm| {
                if matches!(user_on_rejected, Value::Undefined) {
                  reject_pending_promise(vm, &res_p_r, reason.clone());
                } else {
                  vm.exceptions.last_uncaught = None;
                  match vm.call_value_with_this(
                    &user_on_rejected,
                    &Value::Undefined,
                    std::slice::from_ref(&reason),
                  ) {
                    Ok(out) => resolve_promise_with_value(vm, &res_p_r, out)?,
                    Err(err) => {
                      let reject_reason = vm
                        .exceptions
                        .last_uncaught
                        .take()
                        .unwrap_or(Value::String(format!("{}", err).into()));
                      reject_pending_promise(vm, &res_p_r, reject_reason);
                    }
                  }
                }
                Ok(())
              });
            }
          }

          Ok(result_promise_value)
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
        if let Value::Promise(_) = this {
          let then_method =
            vm.get_property_host(this, &PropertyKey::String("then".to_string().into()))?;
          vm.call_value_with_this(&then_method, this, &[Value::Undefined, on_rejected])
        } else {
          Err(vm.throw_type_error("Promise.prototype.catch called on non-promise"))
        }
      }),
      function_proto_map,
    )),
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
        if let Value::Promise(_) = value
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
          if let Value::Promise(promise_state) = &promise {
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
      // The sentinel prevents a synchronously-called resolve element from
      // resolving the result before iterator exhaustion is observed.
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
            // IteratorStepValue marks the iterator record done before
            // forwarding errors from next/done/value, so it is not closed.
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
          if let Value::Promise(promise_state) = &promise {
            reject_pending_promise(vm, promise_state, reject_reason);
          }
        }
        Ok(promise)
      }),
      function_proto_map,
    )),
  );

  (promise_proto_map, promise_ctor_map, promise_ctor_val)
}
