// SPDX-License-Identifier: MPL-2.0
use super::*;
use crate::ecmascript::semantics::byte_views::array_buffer_backing;

type RealmKernel = (
  Intrinsics,
  Rc<RefCell<crate::environment::EnvironmentRecord>>,
);

thread_local! {
  static REALM_KERNELS: RefCell<HashMap<u64, RealmKernel>> = RefCell::new(HashMap::new());
}

pub(crate) fn realm_kernel(globals: &Rc<RefCell<ObjectData>>) -> Option<RealmKernel> {
  let identity = globals.borrow().identity;
  REALM_KERNELS.with(|cache| cache.borrow().get(&identity).cloned())
}

/// Checks if a promise's constructor matches the given constructor value.
pub(crate) fn promise_constructor_matches(
  vm: &mut Vm,
  promise: &Value,
  constructor: &Value,
) -> Result<bool, RuntimeError> {
  let promise_constructor = vm.get_property_host(
    promise,
    &PropertyKey::String("constructor".to_string().into()),
  )?;
  Ok(strict_equal(&promise_constructor, constructor))
}

/// Get a method from an object, treating nullish values as absent.
pub(crate) fn get_method(
  vm: &mut Vm,
  object: &Value,
  key: &PropertyKey,
) -> Result<Option<Value>, RuntimeError> {
  let method = vm.get_property_host(object, key)?;
  if matches!(method, Value::Undefined | Value::Null) {
    return Ok(None);
  }
  if !Vm::is_callable_value(&method) {
    return Err(vm.throw_type_error("Iterator method is not callable"));
  }
  Ok(Some(method))
}

/// Get an iterator and its cached `next` method from an iterator method.
pub(crate) fn get_iterator_record(
  vm: &mut Vm,
  object: &Value,
  method: &Value,
) -> Result<(Value, Value), RuntimeError> {
  let iterator = vm.call_value_with_this(method, object, &[])?;
  if !iterator.is_object() {
    return Err(vm.throw_type_error("Iterator method did not return an object"));
  }
  let next = vm.get_property_host(&iterator, &PropertyKey::String("next".to_string().into()))?;
  Ok((iterator, next))
}

/// Advance an iterator, validating each iterator result object.
pub(crate) fn iterator_step(
  vm: &mut Vm,
  iterator: &Value,
  next: &Value,
) -> Result<Option<Value>, RuntimeError> {
  let result = vm.call_value_with_this(next, iterator, &[])?;
  if !result.is_object() {
    return Err(vm.throw_type_error("Iterator result is not an object"));
  }
  let done = vm.get_property_host(&result, &PropertyKey::String("done".to_string().into()))?;
  if vm.to_boolean(&done) {
    return Ok(None);
  }
  Ok(Some(vm.get_property_host(
    &result,
    &PropertyKey::String("value".to_string().into()),
  )?))
}

/// Perform IteratorClose, including the required return-result validation.
pub(crate) fn iterator_close(
  vm: &mut Vm,
  iterator: &Value,
) -> Result<(), RuntimeError> {
  vm.close_iterator_in_fork(iterator)
}

/// Collects values from an iterable into a vector.
pub(crate) fn collect_iterable_values(
  vm: &mut Vm,
  iterable: &Value,
) -> Result<Vec<Value>, RuntimeError> {
  if let Value::Array(array, _) = iterable {
    return Ok(
      array
        .borrow()
        .iter()
        .map(|value| value.clone().unwrap_or(Value::Undefined))
        .collect(),
    );
  }
  let iterator_method = get_method(
    vm,
    iterable,
    &PropertyKey::Symbol(SymbolId::well_known_iterator()),
  )?;
  if let Some(iterator_method) = iterator_method {
    let (iterator, next) = get_iterator_record(vm, iterable, &iterator_method)?;
    let mut items = Vec::new();
    while let Some(value) = iterator_step(vm, &iterator, &next)? {
      items.push(value);
    }
    Ok(items)
  } else {
    let len_val = vm.get_property_host(iterable, &PropertyKey::String("length".into()))?;
    let len = to_length(vm, &len_val)?;
    let mut items = Vec::new();
    items
      .try_reserve(len)
      .map_err(|_| vm.throw_range_error("Iterable is too large"))?;
    for idx in 0..len {
      let item = vm.get_property_host(
        iterable,
        &PropertyKey::String(usize_to_decimal_string(idx).into()),
      )?;
      items.push(item);
    }
    Ok(items)
  }
}

/// Resolves a promise with a value, ensuring the promise is not already resolved.
fn resolve_promise_with_value_inner(
  vm: &mut Vm,
  result_promise: &Rc<RefCell<PromiseState>>,
  value: Value,
  already_resolved: Rc<RefCell<bool>>,
) -> Result<(), RuntimeError> {
  if *already_resolved.borrow() {
    return Ok(());
  }

  if let Value::Promise(ref promise, _) = value
    && Rc::ptr_eq(promise, result_promise)
  {
    *already_resolved.borrow_mut() = true;
    let reason = vm.create_intrinsic_error("TypeError", "Chaining cycle detected")?;
    reject_pending_promise(vm, result_promise, reason);
    return Ok(());
  }

  if value.is_object() {
    let then_val = match vm.capture_completion(|vm| {
      vm.get_property_host(&value, &PropertyKey::String("then".to_string().into()))
    }) {
      Ok(value) => value,
      Err((error, thrown)) => {
        *already_resolved.borrow_mut() = true;
        let reason = thrown.unwrap_or_else(|| Value::String(error.to_string().into()));
        reject_pending_promise(vm, result_promise, reason);
        return Ok(());
      }
    };
    if Vm::is_callable_value(&then_val) {
      let resolve_called = Rc::clone(&already_resolved);
      let res_p = Rc::clone(result_promise);
      let thenable = value;
      let then_func = then_val;

      vm.queue_microtask(move |vm| {
        vm.with_function_realm(&then_func, |vm| {
          let resolve_called_inner = Rc::clone(&resolve_called);
          let res_p_inner = Rc::clone(&res_p);
          let resolve_cb = Value::Builtin(Rc::new(move |vm, args| {
            if *resolve_called_inner.borrow() {
              return Ok(Value::Undefined);
            }
            *resolve_called_inner.borrow_mut() = true;
            let off = usize::from(vm.builtin_this_arg_prepended());
            let v = args.get(off).cloned().unwrap_or(Value::Undefined);

            resolve_promise_with_value(vm, &res_p_inner, v)?;
            Ok(Value::Undefined)
          }));

          let reject_called_inner = Rc::clone(&resolve_called);
          let res_p2_inner = Rc::clone(&res_p);
          let reject_cb = Value::Builtin(Rc::new(move |vm, args| {
            if *reject_called_inner.borrow() {
              return Ok(Value::Undefined);
            }
            *reject_called_inner.borrow_mut() = true;
            let off = usize::from(vm.builtin_this_arg_prepended());
            let r = args.get(off).cloned().unwrap_or(Value::Undefined);
            reject_pending_promise(vm, &res_p2_inner, r);
            Ok(Value::Undefined)
          }));

          let function_proto = vm.realm().function_prototype();
          let Value::Object(function_proto) = function_proto else {
            unreachable!("intrinsic Function prototype")
          };
          let wrap = |callback| {
            let Value::Builtin(callback) = callback else {
              unreachable!("resolving callback")
            };
            let callback = builtin_object(callback, &function_proto);
            if let Value::BuiltinObject(_, properties) = &callback {
              properties
                .borrow_mut()
                .set_internal_slot("[[RealmGlobals]]", Value::Object(vm.globals_rc()));
            }
            set_builtin_length(&callback, 1.0);
            set_builtin_name(&callback, "");
            callback
          };
          let resolve_cb = wrap(resolve_cb);
          let reject_cb = wrap(reject_cb);

          match vm.call_value_with_this(&then_func, &thenable, &[resolve_cb, reject_cb]) {
            Ok(_) => Ok(()),
            Err(err) => {
              let reason = vm
                .exceptions
                .last_uncaught
                .take()
                .unwrap_or(Value::String(format!("{}", err).into()));
              if !*resolve_called.borrow() {
                *resolve_called.borrow_mut() = true;
                reject_pending_promise(vm, &res_p, reason);
              }
              Ok(())
            }
          }
        })
      });
      return Ok(());
    }
  }

  if !*already_resolved.borrow() {
    *already_resolved.borrow_mut() = true;
    fulfill_pending_promise(vm, result_promise, value);
  }
  Ok(())
}

/// Resolves a promise with a value, ensuring the promise is not already resolved.
pub(crate) fn resolve_promise_with_value(
  vm: &mut Vm,
  result_promise: &Rc<RefCell<PromiseState>>,
  value: Value,
) -> Result<(), RuntimeError> {
  resolve_promise_with_value_inner(vm, result_promise, value, Rc::new(RefCell::new(false)))
}

/// Create a builtin object wrapper for a native function with a prototype.
pub(crate) fn builtin_object(
  f: BuiltinFn,
  function_proto: &Rc<RefCell<ObjectData>>,
) -> Value {
  crate::ecmascript::semantics::builtin_object(f, function_proto)
}

/// Delegate to the ECMAScript semantics module for builtin wiring.
pub fn install_minimal_kernel_constructors(globals: &Rc<RefCell<ObjectData>>) -> Intrinsics {
  let identity = globals.borrow().identity;
  if let Some((intrinsics, _)) = realm_kernel(globals) {
    return intrinsics;
  }
  let intrinsics = crate::ecmascript::semantics::install_minimal_kernel_constructors(globals);
  let global_environment = Rc::new(RefCell::new(crate::environment::EnvironmentRecord::new(
    None,
  )));
  REALM_KERNELS.with(|cache| {
    cache
      .borrow_mut()
      .insert(identity, (intrinsics.clone(), global_environment));
  });
  intrinsics
}

/// Fulfills a pending promise with a given value, invoking all pending callbacks.
pub(crate) fn fulfill_pending_promise(
  vm: &mut Vm,
  promise: &Rc<RefCell<PromiseState>>,
  value: Value,
) {
  use crate::value::PromiseState;

  let mut state = promise.borrow_mut();

  if let PromiseState::Pending { on_fulfilled, .. } = &mut *state {
    let callbacks = std::mem::take(on_fulfilled);
    if crate::trace::enabled() {
      eprintln!(
        "[trace] fulfill_pending_promise promise={:p} callbacks={}",
        Rc::as_ptr(promise),
        callbacks.len()
      );
    }
    let stored_value = value.clone();
    *state = PromiseState::Fulfilled(value);
    drop(state);
    for callback in callbacks {
      let callback_value = stored_value.clone();
      vm.queue_microtask(move |vm| {
        let args = std::slice::from_ref(&callback_value);
        drop(vm.call_value_with_this(&callback, &Value::Undefined, args));
        Ok(())
      });
    }
  }
}

/// Rejects a pending promise with a reason, ensuring the promise is not already resolved.
pub(crate) fn reject_pending_promise(
  vm: &mut Vm,
  promise: &Rc<RefCell<PromiseState>>,
  reason: Value,
) {
  use crate::value::PromiseState;

  let mut state = promise.borrow_mut();
  if let PromiseState::Pending { on_rejected, .. } = &mut *state {
    let callbacks = std::mem::take(on_rejected);
    if crate::trace::enabled() {
      eprintln!(
        "[trace] reject_pending_promise promise={:p} callbacks={}",
        Rc::as_ptr(promise),
        callbacks.len()
      );
    }
    let stored_reason = reason.clone();
    let handled = !callbacks.is_empty();
    *state = PromiseState::Rejected { reason, handled };
    drop(state);
    if callbacks.is_empty() {
      let promise_ref = Rc::clone(promise);
      vm.queue_microtask(move |vm| {
        let still_unhandled = {
          let state = promise_ref.borrow();
          match &*state {
            PromiseState::Rejected {
              reason,
              handled: false,
            } => reason == &stored_reason,
            _ => false,
          }
        };
        if still_unhandled {
          vm.call_unhandled_rejection(
            vm.promise_value(Rc::clone(&promise_ref)),
            stored_reason.clone(),
          );
        }
        Ok(())
      });
    } else {
      for callback in callbacks {
        let callback_reason = stored_reason.clone();
        vm.queue_microtask(move |vm| {
          let args = std::slice::from_ref(&callback_reason);
          drop(vm.call_value_with_this(&callback, &Value::Undefined, args));
          Ok(())
        });
      }
    }
  }
}

/// Evaluates code in a ShadowRealm and returns the result.
pub(crate) fn shadowrealm_evaluate(
  vm: &mut Vm,
  args: &[Value],
) -> Result<Value, RuntimeError> {
  let undefined = Value::Undefined;
  let target = args.first().unwrap_or(&undefined);
  let record = require_shadow_realm_record(vm, target)?;
  let source = match args.get(1) {
    Some(Value::String(s)) => s.as_str_lossy(),
    _ => {
      return Err(vm.throw_type_error("ShadowRealm.prototype.evaluate requires a string argument"));
    }
  };
  let session = Parser::parse_source(source)
    .map_err(|e| syntax_error(vm, format!("ShadowRealm evaluate parse: {}", e)))?;
  let code = compiler::compile_session(&session)
    .map_err(|e| syntax_error(vm, format!("ShadowRealm evaluate compile: {}", e)))?;

  let mut transfer_set: Option<HashSet<usize>> = None;
  if let Some(Value::Array(arr_rc, _)) = args.get(2) {
    let mut set = HashSet::new();
    for v in arr_rc.borrow().iter().flatten().cloned() {
      match v {
        Value::Object(obj_rc) => {
          if let Some(backing_rc) = array_buffer_backing(&Value::Object(Rc::clone(&obj_rc))) {
            if !set.insert(Rc::as_ptr(&backing_rc) as usize) {
              return Err(vm.throw_type_error("Transfer list contains a duplicate ArrayBuffer"));
            }
            continue;
          }
          return Err(vm.throw_type_error("Transfer list entries must be ArrayBuffer objects"));
        }
        _ => {
          return Err(vm.throw_type_error("Transfer list entries must be ArrayBuffer objects"));
        }
      }
    }
    transfer_set = Some(set);
  }

  let inner_result = record
    .borrow_vm()
    .run(code)
    .map_err(|err| vm.throw_type_error(&format!("ShadowRealm evaluate error: {}", err)))?;

  clone_value_across_realms(vm, &inner_result, transfer_set.as_ref())
    .map_err(|s| vm.throw_type_error(&s))
}

/// Imports a value from a module in a ShadowRealm and returns it.
pub(crate) fn shadowrealm_import_value(
  vm: &mut Vm,
  args: &[Value],
) -> Result<Value, RuntimeError> {
  let undefined = Value::Undefined;
  let target = args.first().unwrap_or(&undefined);
  let record = require_shadow_realm_record(vm, target)?;
  let specifier = args.get(1).unwrap_or(&undefined);
  let specifier_str = vm.to_string(specifier)?;
  let export_name = match args.get(2) {
    Some(Value::String(s)) => s.clone(),
    _ => {
      return Err(
        vm.throw_type_error("ShadowRealm.prototype.importValue requires export name string"),
      );
    }
  };

  let promise = gc::track_promise(Rc::new(RefCell::new(PromiseState::Pending {
    on_fulfilled: Vec::new(),
    on_rejected: Vec::new(),
  })));
  let promise_value = vm.promise_value(Rc::clone(&promise));

  let mut transfer_set_arg: Option<Value> = None;
  if args.len() > 3 {
    transfer_set_arg = args.get(3).cloned();
  }

  let record_handle = Rc::clone(&record);
  let base_dir = record.base_dir();
  let specifier_clone = specifier_str;
  let export_clone = export_name;

  vm.queue_microtask(move |vm| {
    let prepared = (|| -> Result<_, String> {
      let entry = {
        let inner_vm = record_handle.borrow_vm();
        let mut loader = inner_vm.module_loader_mut();
        let entry = loader
          .construct(&base_dir, specifier_clone.as_str_lossy())
          .map_err(|error| format!("module construct failed: {error}"))?;
        loader
          .instantiate_all()
          .map_err(|error| format!("module instantiate failed: {error}"))?;
        entry
      };
      let entry_path = entry.borrow().path.clone();
      let evaluation = {
        let mut inner_vm = record_handle.borrow_vm();
        let loader = inner_vm.module_loader_handle();
        let result =
          crate::module::evaluate_entry_promise_shared(&loader, &entry_path, &mut inner_vm, false);
        vm.take_jobs_from(&mut inner_vm);
        result.map_err(|error| format!("module runtime failed: {error}"))?
      };
      Ok((entry, evaluation))
    })();
    let (entry, evaluation) = match prepared {
      Ok(prepared) => prepared,
      Err(message) => {
        let reason = vm
          .create_intrinsic_error("TypeError", &message)
          .unwrap_or_else(|_| Value::String(message.into()));
        reject_pending_promise(vm, &promise, reason);
        return Ok(());
      }
    };
    let rejected_promise = Rc::clone(&promise);
    let fulfilled_promise = Rc::clone(&promise);
    let export_clone = export_clone.clone();
    let transfer_set_arg = transfer_set_arg.clone();
    let on_fulfilled = Value::Builtin(Rc::new(move |vm, _| {
      let promise = &fulfilled_promise;
      let result = entry
        .borrow()
        .exports
        .get(export_clone.as_str_lossy())
        .cloned()
        .map(|slot| slot.value())
        .ok_or_else(|| format!("export {export_clone} not found"));
      match result {
        Ok(value) => {
          let mut transfer_set: Option<HashSet<usize>> = None;
          if let Some(Value::Array(arr_rc, _)) = transfer_set_arg.clone() {
            let mut set = HashSet::new();
            for v in arr_rc.borrow().iter().flatten().cloned() {
              match v {
                Value::Object(obj_rc) => {
                  if let Some(backing_rc) = array_buffer_backing(&Value::Object(Rc::clone(&obj_rc)))
                  {
                    if !set.insert(Rc::as_ptr(&backing_rc) as usize) {
                      let err_val = vm
                        .create_intrinsic_error(
                          "TypeError",
                          "Transfer list contains a duplicate ArrayBuffer",
                        )
                        .unwrap_or(Value::String(
                          "Transfer list contains a duplicate ArrayBuffer"
                            .to_string()
                            .into(),
                        ));
                      reject_pending_promise(vm, promise, err_val);
                      return Ok(Value::Undefined);
                    }
                    continue;
                  }
                  let err_val = vm
                    .create_intrinsic_error(
                      "TypeError",
                      "Transfer list entries must be ArrayBuffer objects",
                    )
                    .unwrap_or(Value::String(
                      "Transfer list entries must be ArrayBuffer objects"
                        .to_string()
                        .into(),
                    ));
                  reject_pending_promise(vm, promise, err_val);
                  return Ok(Value::Undefined);
                }
                _ => {
                  let err_val = vm
                    .create_intrinsic_error(
                      "TypeError",
                      "Transfer list entries must be ArrayBuffer objects",
                    )
                    .unwrap_or(Value::String(
                      "Transfer list entries must be ArrayBuffer objects"
                        .to_string()
                        .into(),
                    ));
                  reject_pending_promise(vm, promise, err_val);
                  return Ok(Value::Undefined);
                }
              }
            }
            transfer_set = Some(set);
          }

          match clone_value_across_realms(vm, &value, transfer_set.as_ref()) {
            Ok(cloned) => fulfill_pending_promise(vm, promise, cloned),
            Err(err) => {
              let err_val = vm
                .create_intrinsic_error("TypeError", &format!("ShadowRealm clone failed: {}", err))
                .unwrap_or(Value::String(err.into()));
              reject_pending_promise(vm, promise, err_val);
            }
          }
        }
        Err(msg) => {
          let err_val = vm
            .create_intrinsic_error("TypeError", &msg)
            .unwrap_or(Value::String(msg.clone().into()));
          reject_pending_promise(vm, promise, err_val);
        }
      }
      Ok(Value::Undefined)
    }));
    let on_rejected = Value::Builtin(Rc::new(move |vm, _| {
      let reason = vm
        .create_intrinsic_error("TypeError", "ShadowRealm module evaluation failed")
        .unwrap_or_else(|_| Value::String("ShadowRealm module evaluation failed".into()));
      reject_pending_promise(vm, &rejected_promise, reason);
      Ok(Value::Undefined)
    }));
    crate::module::observe_evaluation(vm, &evaluation, on_fulfilled, on_rejected);
    Ok(())
  });

  Ok(promise_value)
}

/// Dynamically imports a module and returns a promise resolving to the module namespace.
pub(crate) fn dynamic_import_value(
  vm: &mut Vm,
  args: &[Value],
) -> Result<Value, RuntimeError> {
  let undefined = Value::Undefined;
  let specifier = args.first().unwrap_or(&undefined);

  let promise = gc::track_promise(Rc::new(RefCell::new(PromiseState::Pending {
    on_fulfilled: Vec::new(),
    on_rejected: Vec::new(),
  })));
  let promise_value = vm.promise_value(Rc::clone(&promise));

  let specifier_str = match vm.to_string(specifier) {
    Ok(s) => s,
    Err(err) => {
      let reason = vm
        .exceptions
        .last_uncaught
        .take()
        .unwrap_or(Value::String(err.to_string().into()));
      reject_pending_promise(vm, &promise, reason);
      return Ok(promise_value);
    }
  };

  let current_module = vm.current_module_specifier().map(str::to_owned);
  let (base_dir, importer_specifier) = match current_module {
    Some(specifier) if std::path::Path::new(&specifier).is_absolute() => {
      let base = std::path::Path::new(&specifier)
        .parent()
        .map_or_else(
          || resolve_shadowrealm_base_dir(vm),
          std::path::Path::to_path_buf,
        );
      (base, None)
    }
    Some(specifier) => (resolve_shadowrealm_base_dir(vm), Some(specifier)),
    None => (resolve_shadowrealm_base_dir(vm), None),
  };
  let loader_handle = vm.module_loader_handle();
  vm.queue_dynamic_import_job(move |vm| {
    let result = (|| -> Result<(Value, Value), Value> {
      let entry = {
        let mut loader = loader_handle.borrow_mut();
        let entry = loader
          .construct_from_importer(
            &base_dir,
            importer_specifier.as_deref(),
            specifier_str.as_str_lossy(),
          )
          .map_err(|err| {
            host_error_object(
              vm,
              "SyntaxError",
              &format!("dynamic import construct failed: {}", err),
            )
          })?;
        loader.instantiate_all().map_err(|err| {
          host_error_object(
            vm,
            "SyntaxError",
            &format!("dynamic import instantiate failed: {}", err),
          )
        })?;
        entry
      };
      let entry_path = entry.borrow().path.clone();
      let evaluation =
        crate::module::evaluate_entry_promise_shared(&loader_handle, &entry_path, vm, false)
          .map_err(|err| {
            vm.exceptions
              .last_uncaught
              .take()
              .unwrap_or_else(|| {
                host_error_object(
                  vm,
                  "TypeError",
                  &format!("dynamic import evaluate failed: {}", err),
                )
              })
          })?;

      let module_index = entry.borrow().module_index.ok_or_else(|| {
        Value::String(
          "dynamic import failed: module missing index"
            .to_string()
            .into(),
        )
      })?;
      let exports = {
        let loader = loader_handle.borrow();
        loader
          .map
          .values()
          .filter_map(|record| {
            let record = record.borrow();
            record
              .module_index
              .map(|index| (index, record.exports.clone()))
          })
          .collect::<Vec<_>>()
      };
      for (index, exports) in exports {
        vm.ensure_module_exports(index, exports);
      }
      let namespace = vm
        .module_namespace_value(module_index)
        .ok_or_else(|| {
          Value::String(
            "dynamic import failed: module namespace missing"
              .to_string()
              .into(),
          )
        })?;
      Ok((evaluation, namespace))
    })();

    match result {
      Ok((evaluation, namespace)) => {
        let fulfilled = Rc::clone(&promise);
        let on_fulfilled = Value::Builtin(Rc::new(move |vm, _| {
          resolve_promise_with_value(vm, &fulfilled, namespace.clone())?;
          Ok(Value::Undefined)
        }));
        let rejected = Rc::clone(&promise);
        let on_rejected = Value::Builtin(Rc::new(move |vm, args| {
          let offset = usize::from(args.len() > 1);
          reject_pending_promise(
            vm,
            &rejected,
            args
              .get(offset)
              .cloned()
              .unwrap_or(Value::Undefined),
          );
          Ok(Value::Undefined)
        }));
        crate::module::observe_evaluation(vm, &evaluation, on_fulfilled, on_rejected);
      }
      Err(reason) => reject_pending_promise(vm, &promise, reason),
    }
    Ok(())
  });

  Ok(promise_value)
}

#[cfg(test)]
mod resolution_tests {
  use super::*;

  fn run(
    vm: &mut Vm,
    source: &str,
  ) {
    let session = crate::parser::Parser::parse_source(source).expect("parse");
    let chunk = crate::compiler::compile_session(&session).expect("compile");
    vm.run(chunk).expect("run");
  }

  #[test]
  fn thenable_resolving_functions_use_the_then_method_realm_and_ignore_this() {
    let mut foreign = Vm::new(Rc::new(RefCell::new(ObjectData::new())));
    run(
      &mut foreign,
      crate::js!(
        r#"
      globalThis.thenable = { then(resolve) {
        resolve(Object.getPrototypeOf(resolve) === Function.prototype);
      } };
    "#
      ),
    );
    let mut local = Vm::new(Rc::new(RefCell::new(ObjectData::new())));
    local.realm().set_global(
      "thenable",
      foreign
        .get_global("thenable")
        .expect("foreign thenable"),
    );
    run(
      &mut local,
      crate::js!(
        r#"
      var correctRealm = false, correctProxyRealm = false, correctValue = false, correctReason = false;
      Promise.resolve(thenable).then(value => { correctRealm = value; });
      Promise.resolve({ then: new Proxy(thenable.then, {}) }).then(value => { correctProxyRealm = value; });
      Promise.resolve({ then(resolve) { resolve.call(42); } })
        .then(value => { correctValue = value === undefined; });
      Promise.resolve({ then(resolve, reject) { reject.call(42); } })
        .catch(reason => { correctReason = reason === undefined; });
    "#
      ),
    );
    local
      .flush_microtasks()
      .expect("settle thenables");
    for name in [
      "correctRealm",
      "correctProxyRealm",
      "correctValue",
      "correctReason",
    ] {
      assert_eq!(local.get_global(name), Some(Value::Bool(true)), "{name}");
    }
  }
}
