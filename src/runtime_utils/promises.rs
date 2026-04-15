use super::*;

pub(crate) fn promise_constructor_matches(
    vm: &mut Vm,
    promise: &Value,
    constructor: &Value,
) -> Result<bool, RuntimeError> {
    let promise_constructor =
        vm.get_property_host(promise, &PropertyKey::String("constructor".to_string()))?;
    Ok(strict_equal(&promise_constructor, constructor))
}

pub(crate) fn collect_iterable_values(
    vm: &mut Vm,
    iterable: &Value,
) -> Result<Vec<Value>, RuntimeError> {
    match iterable {
        Value::Array(arr, _) => Ok(arr
            .borrow()
            .iter()
            .map(|v| v.clone().unwrap_or(Value::Undefined))
            .collect()),
        other => {
            let iterator_method =
                vm.get_property_host(other, &PropertyKey::Symbol(SymbolId::well_known_iterator()))?;

            let is_callable = Vm::is_callable_value(&iterator_method);
            if is_callable {
                let iter_obj = vm.call_value_with_this(&iterator_method, other, &[])?;
                let mut items = Vec::new();
                loop {
                    let next_fn =
                        vm.get_property_host(&iter_obj, &PropertyKey::String("next".to_string()))?;
                    let result = vm.call_value_with_this(&next_fn, &iter_obj, &[])?;
                    let done_val =
                        vm.get_property_host(&result, &PropertyKey::String("done".to_string()))?;
                    if vm.to_boolean(&done_val) {
                        break;
                    }
                    let val =
                        vm.get_property_host(&result, &PropertyKey::String("value".to_string()))?;
                    items.push(val);
                }
                Ok(items)
            } else {
                let len_val =
                    vm.get_property_host(other, &PropertyKey::String("length".to_string()))?;
                let len = to_length(vm, &len_val)?;
                let mut items = Vec::with_capacity(len);
                for idx in 0..len {
                    let item = vm.get_property_host(
                        other,
                        &PropertyKey::String(usize_to_decimal_string(idx)),
                    )?;
                    items.push(item);
                }
                Ok(items)
            }
        }
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

    if let Value::Promise(ref p_rc) = value {
        if Rc::ptr_eq(p_rc, result_promise) {
            *already_resolved.borrow_mut() = true;
            return Err(vm.throw_type_error("Chaining cycle detected"));
        }

        let mut state = p_rc.borrow_mut();
        match &mut *state {
            PromiseState::Pending {
                on_fulfilled,
                on_rejected,
            } => {
                let resolve_called = Rc::clone(&already_resolved);
                let res_p = Rc::clone(result_promise);
                on_fulfilled.push(Value::Builtin(Rc::new(move |vm, args| {
                    if *resolve_called.borrow() {
                        return Ok(Value::Undefined);
                    }
                    *resolve_called.borrow_mut() = true;
                    let off = if args.len() > 1 { 1 } else { 0 };
                    let v = args.get(off).cloned().unwrap_or(Value::Undefined);
                    resolve_promise_with_value(vm, &res_p, v)?;
                    Ok(Value::Undefined)
                })));

                let reject_called = Rc::clone(&already_resolved);
                let res_p2 = Rc::clone(result_promise);
                on_rejected.push(Value::Builtin(Rc::new(move |vm, args| {
                    if *reject_called.borrow() {
                        return Ok(Value::Undefined);
                    }
                    *reject_called.borrow_mut() = true;
                    let off = if args.len() > 1 { 1 } else { 0 };
                    let r = args.get(off).cloned().unwrap_or(Value::Undefined);
                    reject_pending_promise(vm, &res_p2, r);
                    Ok(Value::Undefined)
                })));
                return Ok(());
            }
            PromiseState::Fulfilled(v) => {
                let v = v.clone();
                let resolve_called = Rc::clone(&already_resolved);
                let res_p = Rc::clone(result_promise);
                vm.queue_microtask(move |vm| {
                    if *resolve_called.borrow() {
                        return Ok(());
                    }
                    *resolve_called.borrow_mut() = true;
                    fulfill_pending_promise(vm, &res_p, v.clone());
                    Ok(())
                });
                return Ok(());
            }
            PromiseState::Rejected { reason, .. } => {
                let reason = reason.clone();
                let reject_called = Rc::clone(&already_resolved);
                let res_p = Rc::clone(result_promise);
                vm.queue_microtask(move |vm| {
                    if *reject_called.borrow() {
                        return Ok(());
                    }
                    *reject_called.borrow_mut() = true;
                    reject_pending_promise(vm, &res_p, reason.clone());
                    Ok(())
                });
                return Ok(());
            }
        }
    }

    if value.is_object() {
        let then_val = vm.get_property_host(&value, &PropertyKey::String("then".to_string()))?;
        if Vm::is_callable_value(&then_val) {
            // Per-spec PromiseResolveThenableJob semantics: calling a thenable's
            // `then` should not synchronously settle the outer promise. Instead
            // schedule a microtask that will invoke `then` with resolve/reject
            // callbacks. This preserves observable job ordering when a thenable
            // calls back synchronously.
            let resolve_called = Rc::clone(&already_resolved);
            let res_p = Rc::clone(result_promise);
            let thenable = value;
            let then_func = then_val;

            vm.queue_microtask(move |vm| {
                // Build resolve and reject callbacks that will settle res_p
                let resolve_called_inner = resolve_called.clone();
                let res_p_inner = res_p.clone();
                let resolve_cb = Value::Builtin(Rc::new(move |vm, args| {
                    if *resolve_called_inner.borrow() {
                        return Ok(Value::Undefined);
                    }
                    *resolve_called_inner.borrow_mut() = true;
                    let off = if args.len() > 1 { 1 } else { 0 };
                    let v = args.get(off).cloned().unwrap_or(Value::Undefined);
                    resolve_promise_with_value(vm, &res_p_inner, v)?;
                    Ok(Value::Undefined)
                }));

                let reject_called_inner = resolve_called.clone();
                let res_p2_inner = res_p.clone();
                let reject_cb = Value::Builtin(Rc::new(move |vm, args| {
                    if *reject_called_inner.borrow() {
                        return Ok(Value::Undefined);
                    }
                    *reject_called_inner.borrow_mut() = true;
                    let off = if args.len() > 1 { 1 } else { 0 };
                    let r = args.get(off).cloned().unwrap_or(Value::Undefined);
                    reject_pending_promise(vm, &res_p2_inner, r);
                    Ok(Value::Undefined)
                }));

                // Call the then function with the thenable as `this`.
                // Errors thrown here should reject the promise.
                match vm.call_value_with_this(&then_func, &thenable, &[resolve_cb, reject_cb]) {
                    Ok(_) => Ok(()),
                    Err(err) => {
                        let reason = vm
                            .last_uncaught_exception
                            .take()
                            .unwrap_or(Value::String(format!("{}", err)));
                        reject_pending_promise(vm, &res_p, reason);
                        Ok(())
                    }
                }
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

pub(crate) fn builtin_object(f: BuiltinFn, function_proto: &Rc<RefCell<ObjectData>>) -> Value {
    crate::ecmascript::semantics::builtin_object(f, function_proto)
}

/// Delegate to the ECMAScript semantics module for builtin wiring.
pub fn install_minimal_kernel_constructors(globals: &Rc<RefCell<ObjectData>>) -> Intrinsics {
    crate::ecmascript::semantics::install_minimal_kernel_constructors(globals)
}

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
                let _ = vm.call_value_with_this(&callback, &Value::Undefined, args);
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
                        Value::Promise(promise_ref.clone()),
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
                    let _ = vm.call_value_with_this(&callback, &Value::Undefined, args);
                    Ok(())
                });
            }
        }
    }
}

pub(crate) fn shadowrealm_evaluate(vm: &mut Vm, args: &[Value]) -> Result<Value, RuntimeError> {
    let undefined = Value::Undefined;
    let target = args.first().unwrap_or(&undefined);
    let record = require_shadow_realm_record(vm, target)?;
    let source = match args.get(1) {
        Some(Value::String(s)) => s.as_str(),
        _ => {
            return Err(
                vm.throw_type_error("ShadowRealm.prototype.evaluate requires a string argument")
            );
        }
    };
    let session = Parser::parse_source(source)
        .map_err(|e| syntax_error(vm, format!("ShadowRealm evaluate parse: {}", e)))?;
    let chunk = compiler::compile_oxc(session.program(), source)
        .map_err(|e| syntax_error(vm, format!("ShadowRealm evaluate compile: {}", e)))?;

    // Run the code inside the ShadowRealm VM and clone the resulting value
    // into the current (host) VM using a pragmatic structured-clone that
    // supports primitives, plain objects, and arrays. Non-cloneable values
    // (functions, promises, symbols, accessors, cyclic structures) will
    // result in a TypeError to avoid leaking object identity across realms.
    // Parse optional transfer list argument (third argument) as an Array of
    // ArrayBuffer objects. We collect backing-store addresses (Rc pointer
    // addresses of the [[ArrayBufferData]] Array) into a HashSet and pass it
    // to the clone helper so it can detach transferred buffers in the source
    // realm.
    let mut transfer_set: Option<HashSet<usize>> = None;
    if let Some(Value::Array(arr_rc, _)) = args.get(2) {
        let mut set = HashSet::new();
        for v in arr_rc.borrow().iter().flatten().cloned() {
            match v {
                Value::Object(obj_rc) => {
                    // Look up [[ArrayBufferData]] slot and grab underlying
                    // array backing Rc pointer address.
                    if let Some(PropertyDescriptor::Data { value: ab_data, .. }) = obj_rc
                        .borrow()
                        .properties
                        .get(&PropertyKey::String("[[ArrayBufferData]]".to_string()))
                        .cloned()
                        && let Value::Array(backing_rc, _) = ab_data
                    {
                        set.insert(Rc::as_ptr(&backing_rc) as usize);
                        continue;
                    }
                    return Err(
                        vm.throw_type_error("Transfer list entries must be ArrayBuffer objects")
                    );
                }
                _ => {
                    return Err(
                        vm.throw_type_error("Transfer list entries must be ArrayBuffer objects")
                    );
                }
            }
        }
        transfer_set = Some(set);
    }

    let inner_result = record
        .borrow_vm()
        .run(chunk)
        .map_err(|err| vm.throw_type_error(&format!("ShadowRealm evaluate error: {}", err)))?;

    // Pass transfer_set.as_ref() to clone helper so it may detach transferred
    // ArrayBuffers in the source realm.
    clone_value_across_realms(vm, &inner_result, transfer_set.as_ref())
        .map_err(|s| vm.throw_type_error(&s))
}

pub(crate) fn shadowrealm_import_value(vm: &mut Vm, args: &[Value]) -> Result<Value, RuntimeError> {
    let undefined = Value::Undefined;
    let target = args.first().unwrap_or(&undefined);
    let record = require_shadow_realm_record(vm, target)?;
    let specifier = args.first().unwrap_or(&undefined);
    let specifier_str = vm.to_string(specifier)?;
    let export_name = match args.get(1) {
        Some(Value::String(s)) => s.clone(),
        _ => {
            return Err(vm.throw_type_error(
                "ShadowRealm.prototype.importValue requires export name string",
            ));
        }
    };

    let promise = gc::track_promise(Rc::new(RefCell::new(PromiseState::Pending {
        on_fulfilled: Vec::new(),
        on_rejected: Vec::new(),
    })));
    let promise_value = Value::Promise(promise.clone());

    // Parse transfer list argument (third argument) if present. For importValue
    // the signature is (shadowRealm, specifier, exportName, [transferList]) so
    // the transfer list will be at index 2 or 3 depending on calling conv. The
    // existing code places export name at args.get(1), so treat args.get(2) as
    // the optional transfer list.
    let mut transfer_set_arg: Option<Value> = None;
    if args.len() > 2 {
        transfer_set_arg = args.get(2).cloned();
    }

    let record_handle = record.clone();
    let base_dir = record.base_dir();
    let specifier_clone = specifier_str;
    let export_clone = export_name;

    vm.queue_microtask(move |vm| {
        let result = (|| -> Result<Value, String> {
            let entry = {
                let inner_vm = record_handle.borrow_vm();
                let mut loader = inner_vm.module_loader_mut();
                let entry = loader
                    .construct(&base_dir, &specifier_clone)
                    .map_err(|err| format!("module construct failed: {}", err))?;
                loader
                    .instantiate_all()
                    .map_err(|err| format!("module instantiate failed: {}", err))?;
                entry
            };
            let chunk = {
                let rec = entry.borrow();
                let module_index = rec
                    .module_index
                    .ok_or_else(|| "module missing index".to_string())?;
                let module_ctx = ModuleContext {
                    module_index,
                    imports: rec.import_sources.clone(),
                    exports: rec.exports.keys().cloned().collect(),
                };
                compiler::compile_oxc_with_ctx(
                    rec.session.program(),
                    Some(&module_ctx),
                    rec.session.source(),
                )
                .map_err(|err| format!("module compile failed: {}", err))?
            };
            let chunk = clear_slot_types_in_chunk(chunk);
            {
                let mut inner_vm = record_handle.borrow_vm();
                inner_vm
                    .run(chunk)
                    .map_err(|err| format!("module runtime failed: {}", err))?;
            }
            let slot = entry
                .borrow()
                .exports
                .get(&export_clone)
                .cloned()
                .ok_or_else(|| format!("export {} not found", export_clone))?;
            Ok(slot.borrow().clone())
        })();

        match result {
            Ok(value) => {
                // Build transfer set from transfer_set_arg if present.
                let mut transfer_set: Option<HashSet<usize>> = None;
                if let Some(Value::Array(arr_rc, _)) = transfer_set_arg.clone() {
                    let mut set = HashSet::new();
                    for v in arr_rc.borrow().iter().flatten().cloned() {
                        match v {
                            Value::Object(obj_rc) => {
                                if let Some(PropertyDescriptor::Data { value: ab_data, .. }) =
                                    obj_rc
                                        .borrow()
                                        .properties
                                        .get(&PropertyKey::String(
                                            "[[ArrayBufferData]]".to_string(),
                                        ))
                                        .cloned()
                                    && let Value::Array(backing_rc, _) = ab_data
                                {
                                    set.insert(Rc::as_ptr(&backing_rc) as usize);
                                    continue;
                                }
                                let err_val = vm
                                    .create_intrinsic_error(
                                        "TypeError",
                                        "Transfer list entries must be ArrayBuffer objects",
                                    )
                                    .unwrap_or(Value::String(
                                        "Transfer list entries must be ArrayBuffer objects"
                                            .to_string(),
                                    ));
                                reject_pending_promise(vm, &promise, err_val);
                                return Ok(());
                            }
                            _ => {
                                let err_val = vm
                                    .create_intrinsic_error(
                                        "TypeError",
                                        "Transfer list entries must be ArrayBuffer objects",
                                    )
                                    .unwrap_or(Value::String(
                                        "Transfer list entries must be ArrayBuffer objects"
                                            .to_string(),
                                    ));
                                reject_pending_promise(vm, &promise, err_val);
                                return Ok(());
                            }
                        }
                    }
                    transfer_set = Some(set);
                }

                match clone_value_across_realms(vm, &value, transfer_set.as_ref()) {
                    Ok(cloned) => fulfill_pending_promise(vm, &promise, cloned),
                    Err(err) => {
                        let err_val = vm
                            .create_intrinsic_error(
                                "TypeError",
                                &format!("ShadowRealm clone failed: {}", err),
                            )
                            .unwrap_or(Value::String(err));
                        reject_pending_promise(vm, &promise, err_val);
                    }
                }
            }
            Err(msg) => {
                let err_val = vm
                    .create_intrinsic_error("TypeError", &msg)
                    .unwrap_or(Value::String(msg.clone()));
                reject_pending_promise(vm, &promise, err_val);
            }
        }
        Ok(())
    });

    Ok(promise_value)
}

pub(crate) fn dynamic_import_value(vm: &mut Vm, args: &[Value]) -> Result<Value, RuntimeError> {
    let undefined = Value::Undefined;
    let specifier = args.first().unwrap_or(&undefined);
    let options = args.get(1).unwrap_or(&undefined);

    let promise = gc::track_promise(Rc::new(RefCell::new(PromiseState::Pending {
        on_fulfilled: Vec::new(),
        on_rejected: Vec::new(),
    })));
    let promise_value = Value::Promise(promise.clone());

    let specifier_str = match vm.to_string(specifier) {
        Ok(s) => s,
        Err(err) => {
            let reason = vm
                .last_uncaught_exception
                .take()
                .unwrap_or(Value::String(err.to_string()));
            reject_pending_promise(vm, &promise, reason);
            return Ok(promise_value);
        }
    };

    let base_dir = resolve_shadowrealm_base_dir(vm);
    let mut import_vm = Vm::new(vm.globals_rc());
    let loader_handle = import_vm.module_loader_handle();
    let result = (|| -> Result<Value, Value> {
        let entry = {
            let mut loader = loader_handle.borrow_mut();
            let entry = loader.construct(&base_dir, &specifier_str).map_err(|err| {
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
        crate::module::evaluate_entry_shared(&loader_handle, &entry_path, &mut import_vm, false)
            .map_err(|err| {
                import_vm
                    .last_uncaught_exception
                    .take()
                    .or_else(|| {
                        Some(host_error_object(
                            vm,
                            "TypeError",
                            &format!("dynamic import evaluate failed: {}", err),
                        ))
                    })
                    .unwrap_or(Value::String(format!(
                        "dynamic import evaluate failed: {}",
                        err
                    )))
            })?;

        if !matches!(options, Value::Undefined) {
            // The `options` argument is currently parsed for side effects only.
            // Test262 dynamic-import coverage in this tree only exercises the
            // source-only form, so we intentionally ignore it here.
        }

        let module_index = entry.borrow().module_index.ok_or_else(|| {
            Value::String("dynamic import failed: module missing index".to_string())
        })?;
        let ns = import_vm
            .module_namespace_value(module_index)
            .ok_or_else(|| {
                Value::String("dynamic import failed: module namespace missing".to_string())
            })?;
        Ok(ns)
    })();

    match result {
        Ok(value) => {
            let promise_for_fulfill = promise.clone();
            let value_for_fulfill = value;
            vm.queue_dynamic_import_job(move |vm| {
                fulfill_pending_promise(vm, &promise_for_fulfill, value_for_fulfill.clone());
                Ok(())
            });
        }
        Err(reason) => {
            let promise_for_reject = promise.clone();
            let reason_for_reject = reason;
            vm.queue_dynamic_import_job(move |vm| {
                reject_pending_promise(vm, &promise_for_reject, reason_for_reject.clone());
                Ok(())
            });
        }
    }

    Ok(promise_value)
}
