// SPDX-License-Identifier: MPL-2.0
//! Promise builtin functions and prototype methods

use std::cell::RefCell;
use std::rc::Rc;

use crate::runtime_utils::{
  builtin_object, collect_iterable_values, promise_constructor_matches, reject_pending_promise,
  resolve_promise_with_value, set_builtin_length, set_builtin_name, static_builtin_arg_offset,
};
use crate::value::{BuiltinFn, ObjectData, PromiseState, PropertyDescriptor, PropertyKey, Value};
use crate::vm::Vm;

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
      let resolve_fn = Value::Builtin(Rc::new(move |vm, args| {
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
            .last_uncaught_exception
            .take()
            .unwrap_or(Value::String(format!("{}", err)));
          reject_pending_promise(vm, &promise_for_resolve, reason);
        }
        Ok(Value::Undefined)
      }));

      let promise_for_reject = Rc::clone(&promise);
      let reject_called = resolve_called;
      let reject_fn = Value::Builtin(Rc::new(move |vm, args| {
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
      }));

      vm.last_uncaught_exception = None;
      if let Err(err) =
        vm.call_value_with_this(executor, &Value::Undefined, &[resolve_fn, reject_fn])
      {
        let reason = vm
          .last_uncaught_exception
          .take()
          .unwrap_or(Value::String(format!("{}", err)));
        reject_pending_promise(vm, &promise, reason);
      }

      Ok(Value::Promise(promise))
    })
  };

  promise_ctor_map.borrow_mut().properties.insert(
    PKey::String("prototype".to_string()),
    PropertyDescriptor::builtin_prototype(Value::Object(Rc::clone(&promise_proto_map))),
  );
  let promise_ctor_val = Value::BuiltinObject(promise_ctor_fn, Rc::clone(&promise_ctor_map));
  set_builtin_name(&promise_ctor_val, "Promise");
  set_builtin_length(&promise_ctor_val, 1.0);

  promise_proto_map.borrow_mut().properties.insert(
    PKey::String("constructor".to_string()),
    PropertyDescriptor::builtin_data(promise_ctor_val.clone()),
  );
  promise_proto_map.borrow_mut().properties.insert(
    PKey::String("then".to_string()),
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
                vm.last_uncaught_exception = None;
                match vm.call_value_with_this(&user_on_fulfilled, &Value::Undefined, &[value]) {
                  Ok(out) => resolve_promise_with_value(vm, &res_p_f, out)?,
                  Err(err) => {
                    let reason = vm
                      .last_uncaught_exception
                      .take()
                      .unwrap_or(Value::String(format!("{}", err)));
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
                vm.last_uncaught_exception = None;
                match vm.call_value_with_this(&user_on_rejected, &Value::Undefined, &[reason]) {
                  Ok(out) => resolve_promise_with_value(vm, &res_p_r, out)?,
                  Err(err) => {
                    let reject_reason = vm
                      .last_uncaught_exception
                      .take()
                      .unwrap_or(Value::String(format!("{}", err)));
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
                  vm.last_uncaught_exception = None;
                  match vm.call_value_with_this(
                    &user_on_fulfilled,
                    &Value::Undefined,
                    std::slice::from_ref(&value),
                  ) {
                    Ok(out) => resolve_promise_with_value(vm, &res_p_f, out)?,
                    Err(err) => {
                      let reason = vm
                        .last_uncaught_exception
                        .take()
                        .unwrap_or(Value::String(format!("{}", err)));
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
                  vm.last_uncaught_exception = None;
                  match vm.call_value_with_this(
                    &user_on_rejected,
                    &Value::Undefined,
                    std::slice::from_ref(&reason),
                  ) {
                    Ok(out) => resolve_promise_with_value(vm, &res_p_r, out)?,
                    Err(err) => {
                      let reject_reason = vm
                        .last_uncaught_exception
                        .take()
                        .unwrap_or(Value::String(format!("{}", err)));
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
    PKey::String("catch".to_string()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new(|vm, args| {
        let undefined = Value::Undefined;
        let this = args.first().unwrap_or(&undefined);
        let on_rejected = args.get(1).cloned().unwrap_or(Value::Undefined);
        if let Value::Promise(_) = this {
          let then_method = vm.get_property_host(this, &PropertyKey::String("then".to_string()))?;
          vm.call_value_with_this(&then_method, this, &[Value::Undefined, on_rejected])
        } else {
          Err(vm.throw_type_error("Promise.prototype.catch called on non-promise"))
        }
      }),
      function_proto_map,
    )),
  );

  promise_ctor_map.borrow_mut().properties.insert(
    PKey::String("resolve".to_string()),
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
            .last_uncaught_exception
            .take()
            .unwrap_or(Value::String(format!("{}", err)));
          if let Value::Promise(promise_state) = &promise {
            reject_pending_promise(vm, promise_state, reason);
          }
        }
        Ok(promise)
      }),
      function_proto_map,
    )),
  );

  promise_ctor_map.borrow_mut().properties.insert(
    PKey::String("all".to_string()),
    PropertyDescriptor::builtin_data(builtin_object(
      Rc::new(|vm, args| {
        let off = static_builtin_arg_offset(vm, args, "Promise");
        let undefined = Value::Undefined;
        let iterable = args.get(off).unwrap_or(&undefined);
        let this_val = vm.this_value();
        if !this_val.is_object() {
          return Err(vm.throw_type_error("Promise.all called on non-object"));
        }

        let resolve_slot: Rc<RefCell<Option<Value>>> = Rc::new(RefCell::new(None));
        let reject_slot: Rc<RefCell<Option<Value>>> = Rc::new(RefCell::new(None));
        let resolve_slot_capture = Rc::clone(&resolve_slot);
        let reject_slot_capture = Rc::clone(&reject_slot);
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
        let reject_fn = reject_slot
          .borrow()
          .clone()
          .ok_or_else(|| vm.throw_type_error("Promise capability reject missing"))?;

        let items = match collect_iterable_values(vm, iterable) {
          Ok(items) => items,
          Err(err) => {
            let reason = vm
              .create_intrinsic_error("TypeError", &format!("Promise.all iterable error: {}", err))
              .unwrap_or(Value::String(format!("{}", err)));
            drop(vm.call_value_with_this(&reject_fn, &Value::Undefined, &[reason]));
            return Ok(promise);
          }
        };

        if items.is_empty() {
          let array = vm.new_array(Vec::new());
          drop(vm.call_value_with_this(&resolve_fn, &Value::Undefined, &[array]));
          return Ok(promise);
        }

        let results: Rc<RefCell<Vec<Option<Value>>>> =
          Rc::new(RefCell::new(vec![None; items.len()]));
        let remaining = Rc::new(RefCell::new(items.len()));
        let settled = Rc::new(RefCell::new(false));

        for (idx, item) in items.into_iter().enumerate() {
          let then_val = vm.get_property_host(&item, &PropertyKey::String("then".to_string()))?;
          if Vm::is_callable_value(&then_val) {
            let results_rc = Rc::clone(&results);
            let remaining_rc = Rc::clone(&remaining);
            let settled_rc = Rc::clone(&settled);
            let resolve_fn_rc = resolve_fn.clone();
            let reject_fn_rc = reject_fn.clone();
            let resolve_slot_resolved = Rc::new(RefCell::new(false));
            let reject_slot_resolved = Rc::new(RefCell::new(false));

            let resolve_slot_resolved_cb = Rc::clone(&resolve_slot_resolved);
            let resolve_cb = Value::Builtin(Rc::new(move |vm, args| {
              if *settled_rc.borrow() || *resolve_slot_resolved_cb.borrow() {
                return Ok(Value::Undefined);
              }
              *resolve_slot_resolved_cb.borrow_mut() = true;
              let off = if args.len() > 1 { 1 } else { 0 };
              let value = args.get(off).cloned().unwrap_or(Value::Undefined);
              results_rc.borrow_mut()[idx] = Some(value);
              let mut remaining_ref = remaining_rc.borrow_mut();
              if *remaining_ref > 0 {
                *remaining_ref -= 1;
              }
              if *remaining_ref == 0 && !*settled_rc.borrow() {
                *settled_rc.borrow_mut() = true;
                let array = vm.new_array(results_rc.borrow().clone());
                drop(vm.call_value_with_this(&resolve_fn_rc, &Value::Undefined, &[array]));
              }
              Ok(Value::Undefined)
            }));

            let reject_slot_resolved_cb = Rc::clone(&reject_slot_resolved);
            let settled_rc = Rc::clone(&settled);
            let reject_cb = Value::Builtin(Rc::new(move |vm, args| {
              if *settled_rc.borrow() || *reject_slot_resolved_cb.borrow() {
                return Ok(Value::Undefined);
              }
              *reject_slot_resolved_cb.borrow_mut() = true;
              *settled_rc.borrow_mut() = true;
              let off = if args.len() > 1 { 1 } else { 0 };
              let reason = args.get(off).cloned().unwrap_or(Value::Undefined);
              drop(vm.call_value_with_this(&reject_fn_rc, &Value::Undefined, &[reason]));
              Ok(Value::Undefined)
            }));

            if let Err(err) = vm.call_value_with_this(&then_val, &item, &[resolve_cb, reject_cb]) {
              *settled.borrow_mut() = true;
              let reason = vm
                .last_uncaught_exception
                .take()
                .unwrap_or(Value::String(format!("{}", err)));
              drop(vm.call_value_with_this(&reject_fn, &Value::Undefined, &[reason]));
              return Ok(promise);
            }
          } else {
            results.borrow_mut()[idx] = Some(item);
            let mut remaining_ref = remaining.borrow_mut();
            if *remaining_ref > 0 {
              *remaining_ref -= 1;
            }
            if *remaining_ref == 0 && !*settled.borrow() {
              *settled.borrow_mut() = true;
              let array = vm.new_array(results.borrow().clone());
              drop(vm.call_value_with_this(&resolve_fn, &Value::Undefined, &[array]));
            }
          }
        }

        Ok(promise)
      }),
      function_proto_map,
    )),
  );

  promise_ctor_map.borrow_mut().properties.insert(
    PKey::String("reject".to_string()),
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
            .last_uncaught_exception
            .take()
            .unwrap_or(Value::String(format!("{}", err)));
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
