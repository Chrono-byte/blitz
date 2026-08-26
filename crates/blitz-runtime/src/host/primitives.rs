// SPDX-License-Identifier: MPL-2.0

use super::root::RootId;
use super::*;

pub(super) fn runtime_primitive(
  vm: &Vm,
  root: RootSupervisor,
) -> Value {
  let primitive = object(vm.realm().object_prototype());

  let reserve_root = root.clone();
  let reserve: BuiltinFn = Rc::new(move |vm, _args| {
    let result = object(vm.realm().object_prototype());
    match reserve_root.reserve() {
      Ok(id) => {
        set(&result, "ok", Value::Bool(true));
        set(&result, "value", root_id_value(vm, id));
      }
      Err(error) => {
        set(&result, "ok", Value::Bool(false));
        set(&result, "message", Value::String(error.to_string().into()));
      }
    }
    Ok(result)
  });
  set(&primitive, "reserveRoot", Value::Builtin(reserve));

  let attach_root = root.clone();
  let attach: BuiltinFn = Rc::new(move |vm, args| {
    let id = root_id_from_value(vm, args.get(1))?;
    let interrupt = args.get(2).cloned().unwrap_or(Value::Undefined);
    attach_root.attach(vm, id, interrupt)?;
    Ok(Value::Undefined)
  });
  set(&primitive, "attachRoot", Value::Builtin(attach));

  let interrupt_root = root.clone();
  let interrupt: BuiltinFn = Rc::new(move |vm, args| {
    let status = integer_status(vm, args.get(1), 1)?;
    let interrupted = interrupt_root.request_interrupt(vm, status)?;
    Ok(Value::Bool(interrupted))
  });
  set(&primitive, "interruptRoot", Value::Builtin(interrupt));

  let complete_root = root.clone();
  let complete: BuiltinFn = Rc::new(move |vm, args| {
    let id = root_id_from_value(vm, args.get(1))?;
    let status = integer_status(vm, args.get(2), 1)?;
    let completed = complete_root.complete(vm, id, status)?;
    Ok(Value::Bool(completed))
  });
  set(&primitive, "completeRoot", Value::Builtin(complete));

  let fail: BuiltinFn = Rc::new(move |vm, args| {
    let id = root_id_from_value(vm, args.get(1))?;
    root.fail_reservation(vm, id)?;
    Ok(Value::Undefined)
  });
  set(&primitive, "failRoot", Value::Builtin(fail));

  if let Value::Object(object) = &primitive {
    object.borrow_mut().extensible = false;
  }
  primitive
}

fn root_id_value(
  vm: &Vm,
  id: RootId,
) -> Value {
  let value = object(vm.realm().object_prototype());
  set(&value, "generation", Value::Number(id.generation() as f64));
  if let Value::Object(object) = &value {
    object.borrow_mut().extensible = false;
  }
  value
}

fn root_id_from_value(
  vm: &mut Vm,
  value: Option<&Value>,
) -> Result<RootId, RuntimeError> {
  let value = value.ok_or_else(|| RuntimeError::from("runtime root identifier is required"))?;
  let generation =
    vm.get_property_host(value, &PropertyKey::String("generation".to_string().into()))?;
  let generation = vm.to_number_value(&generation)?;
  if !generation.is_finite() || generation < 0.0 || generation.fract() != 0.0 {
    return Err(RuntimeError::from("runtime root identifier is invalid"));
  }
  let generation = generation
    .to_string()
    .parse::<u64>()
    .map_err(|_| RuntimeError::from("runtime root identifier is invalid"))?;
  Ok(RootId::from_generation(generation))
}

fn integer_status(
  vm: &mut Vm,
  value: Option<&Value>,
  default: i32,
) -> Result<i32, RuntimeError> {
  let Some(value) = value else {
    return Ok(default);
  };
  let status = vm.to_number_value(value)?;
  if !status.is_finite() || status.fract() != 0.0 {
    return Err(RuntimeError::from("runtime exit status must be an integer"));
  }
  status
    .to_string()
    .parse::<i32>()
    .map_err(|_| RuntimeError::from("runtime exit status is outside the supported range"))
}

pub(super) fn http_primitive(
  vm: &Vm,
  server: Rc<RefCell<Option<server::ServerState>>>,
) -> Value {
  let primitive = object(vm.realm().object_prototype());
  let open_server = Rc::clone(&server);
  let open: BuiltinFn = Rc::new(move |vm, args| {
    let Some(options) = args.get(1).cloned() else {
      let error = http_error(
        vm,
        "InvalidOptions",
        "listen",
        "openServer requires options",
      );
      return effect_fail(vm, error);
    };
    let handler =
      match vm.get_property_host(&options, &PropertyKey::String("handler".to_string().into())) {
        Ok(handler) if handler.is_callable() => handler,
        _ => {
          let error = http_error(
            vm,
            "InvalidOptions",
            "listen",
            "HTTP handler must be callable",
          );
          return effect_fail(vm, error);
        }
      };
    let Some(state) = open_server.borrow().clone() else {
      let error = http_error(vm, "Transport", "listen", "HTTP host is not initialized");
      return effect_fail(vm, error);
    };
    match server::serve_method(state)(vm, &[Value::Undefined, options, handler]) {
      Ok(handle) => {
        let local_address = match (
          vm.get_property_host(&handle, &PropertyKey::String("hostname".to_string().into())),
          vm.get_property_host(&handle, &PropertyKey::String("port".to_string().into())),
        ) {
          (Ok(Value::String(hostname)), Ok(Value::Number(port))) => {
            Value::String(format!("{hostname}:{}", port.trunc()).into())
          }
          _ => Value::String(String::new().into()),
        };
        if let Value::Object(object) = &handle {
          object.borrow_mut().properties.insert(
            PropertyKey::String("localAddress".to_string().into()),
            PropertyDescriptor::ordinary_data(local_address),
          );
        }
        effect_succeed(vm, handle)
      }
      Err(error) => {
        let message = error.to_string();
        let reason = if message.contains("bind") {
          "Bind"
        } else {
          "InvalidOptions"
        };
        let error = http_error(vm, reason, "listen", &message);
        effect_fail(vm, error)
      }
    }
  });
  set(&primitive, "openServer", Value::Builtin(open));

  let close: BuiltinFn = Rc::new(|vm, args| {
    let Some(handle) = args.get(1) else {
      return effect_succeed(vm, Value::Undefined);
    };
    let shutdown =
      vm.get_property_host(handle, &PropertyKey::String("shutdown".to_string().into()))?;
    if shutdown.is_callable() {
      let result = vm.call_value_with_this(&shutdown, handle, &[])?;
      let _value = vm.await_value(result)?;
    }
    effect_succeed(vm, Value::Undefined)
  });
  set(&primitive, "closeServer", Value::Builtin(close));

  let await_shutdown: BuiltinFn = Rc::new(|vm, args| {
    if args.get(1).is_none() {
      let error = http_error(vm, "Transport", "serve", "awaitShutdown requires a server");
      return effect_fail(vm, error);
    }
    effect_function(vm, "never")
  });
  set(&primitive, "awaitShutdown", Value::Builtin(await_shutdown));
  if let Value::Object(object) = &primitive {
    object.borrow_mut().extensible = false;
  }
  primitive
}

fn http_error(
  vm: &Vm,
  reason: &str,
  operation: &str,
  message: &str,
) -> Value {
  let error = object(vm.realm().object_prototype());
  set(
    &error,
    "_tag",
    Value::String("HttpServerError".to_string().into()),
  );
  set(&error, "reason", Value::String(reason.to_string().into()));
  set(
    &error,
    "operation",
    Value::String(operation.to_string().into()),
  );
  set(&error, "message", Value::String(message.to_string().into()));
  error
}

pub(super) fn websocket_primitive(
  vm: &Vm,
  server: Rc<RefCell<Option<server::ServerState>>>,
) -> Value {
  let primitive = object(vm.realm().object_prototype());

  let connect: BuiltinFn = Rc::new(|vm, args| {
    let options = args.get(1).cloned().unwrap_or(Value::Undefined);
    let complete = args.get(2).cloned().unwrap_or(Value::Undefined);
    let url = vm.get_property_host(&options, &PropertyKey::String("url".to_string().into()))?;
    let constructor = vm
      .get_global("WebSocket")
      .ok_or_else(|| RuntimeError::from("WebSocket is not installed"))?;
    let socket = vm.construct_value(&constructor, &[url])?;
    let callback_root = vm.create_host_root(complete);
    let callback_socket = socket.clone();
    let on_open: BuiltinFn = Rc::new(move |vm, _args| {
      if let Some(callback) = vm
        .host_root(callback_root)
        .filter(Value::is_callable)
      {
        vm.release_host_root(callback_root)?;
        complete_success(vm, &callback, callback_socket.clone())?;
      }
      Ok(Value::Undefined)
    });
    let error_root = callback_root;
    let on_error: BuiltinFn = Rc::new(move |vm, _args| {
      if let Some(callback) = vm
        .host_root(error_root)
        .filter(Value::is_callable)
      {
        vm.release_host_root(error_root)?;
        complete_failure(vm, &callback, "connect", "WebSocket connection failed")?;
      }
      Ok(Value::Undefined)
    });
    let install_result = (|| {
      set_socket_handler(vm, &socket, "onopen", Value::Builtin(on_open))?;
      set_socket_handler(vm, &socket, "onerror", Value::Builtin(on_error))
    })();
    if let Err(error) = install_result {
      if vm.host_root(callback_root).is_some() {
        vm.release_host_root(callback_root)?;
      }
      return Err(error);
    }
    Ok(socket)
  });
  set(&primitive, "connect", Value::Builtin(connect));

  let upgrade_server = Rc::clone(&server);
  let prepare_upgrade: BuiltinFn = Rc::new(move |vm, args| {
    let request = args.get(1).cloned().unwrap_or(Value::Undefined);
    let complete = args.get(3).cloned().unwrap_or(Value::Undefined);
    let Some(state) = upgrade_server.borrow().clone() else {
      complete_failure(
        vm,
        &complete,
        "prepareUpgrade",
        "WebSocket host is not initialized",
      )?;
      return Ok(Value::Undefined);
    };
    let prototype = state
      .websocket_prototype()
      .unwrap_or_else(|| vm.realm().object_prototype());
    match server::upgrade_websocket_method(state, prototype)(vm, &[Value::Undefined, request]) {
      Ok(upgrade) => {
        if complete.is_callable() {
          complete_success(vm, &complete, upgrade.clone())?;
        }
        Ok(upgrade)
      }
      Err(error) if complete.is_callable() => {
        complete_failure(vm, &complete, "prepareUpgrade", &error.to_string())?;
        Ok(Value::Undefined)
      }
      Err(error) => Err(error),
    }
  });
  set(
    &primitive,
    "prepareUpgrade",
    Value::Builtin(prepare_upgrade),
  );

  let accept: BuiltinFn = Rc::new(|vm, args| {
    let upgrade = args.get(1).cloned().unwrap_or(Value::Undefined);
    let complete = args.get(2).cloned().unwrap_or(Value::Undefined);
    let socket =
      vm.get_property_host(&upgrade, &PropertyKey::String("socket".to_string().into()))?;
    if complete.is_callable() {
      complete_success(vm, &complete, socket.clone())?;
    }
    Ok(socket)
  });
  set(&primitive, "accept", Value::Builtin(accept));

  let send: BuiltinFn = Rc::new(|vm, args| {
    let socket = args.get(1).cloned().unwrap_or(Value::Undefined);
    let message = args.get(2).cloned().unwrap_or(Value::Undefined);
    let complete = args.get(3).cloned().unwrap_or(Value::Undefined);
    let method = vm.get_property_host(&socket, &PropertyKey::String("send".to_string().into()))?;
    match vm.call_value_with_this(&method, &socket, &[message]) {
      Ok(_) if complete.is_callable() => complete_success(vm, &complete, Value::Undefined)?,
      Ok(_) => {}
      Err(error) if complete.is_callable() => {
        complete_failure(vm, &complete, "send", &error.to_string())?;
      }
      Err(error) => return Err(error),
    }
    Ok(Value::Undefined)
  });
  set(&primitive, "send", Value::Builtin(send));

  let close: BuiltinFn = Rc::new(|vm, args| {
    let socket = args.get(1).cloned().unwrap_or(Value::Undefined);
    let options = args.get(2).cloned().unwrap_or(Value::Undefined);
    let complete = args.get(3).cloned().unwrap_or(Value::Undefined);
    let code = vm.get_property_host(&options, &PropertyKey::String("code".to_string().into()))?;
    let reason =
      vm.get_property_host(&options, &PropertyKey::String("reason".to_string().into()))?;
    let method = vm.get_property_host(&socket, &PropertyKey::String("close".to_string().into()))?;
    let arguments = if matches!(code, Value::Undefined) {
      Vec::new()
    } else if matches!(reason, Value::Undefined) {
      vec![code]
    } else {
      vec![code, reason]
    };
    match vm.call_value_with_this(&method, &socket, &arguments) {
      Ok(_) if complete.is_callable() => complete_success(vm, &complete, Value::Undefined)?,
      Ok(_) => {}
      Err(error) if complete.is_callable() => {
        complete_failure(vm, &complete, "close", &error.to_string())?;
      }
      Err(error) => return Err(error),
    }
    Ok(Value::Undefined)
  });
  set(&primitive, "close", Value::Builtin(close));

  let subscribe: BuiltinFn = Rc::new(|vm, args| {
    let socket = args.get(1).cloned().unwrap_or(Value::Undefined);
    let on_event = args.get(2).cloned().unwrap_or(Value::Undefined);
    let callback_root = vm.create_host_root(on_event);
    for (property, event_type) in [
      ("onmessage", "message"),
      ("onclose", "close"),
      ("onerror", "error"),
    ] {
      let event_type = event_type.to_string();
      let listener: BuiltinFn = Rc::new(move |vm, args| {
        let source = args.get(1).cloned().unwrap_or(Value::Undefined);
        let event = object(vm.realm().object_prototype());
        if event_type == "message" {
          let data =
            vm.get_property_host(&source, &PropertyKey::String("data".to_string().into()))?;
          let kind = if matches!(data, Value::String(_)) {
            "text"
          } else {
            "binary"
          };
          set(&event, "type", Value::String(kind.to_string().into()));
          set(&event, "data", data);
        } else {
          set(&event, "type", Value::String(event_type.clone().into()));
          if event_type == "error" {
            set(&event, "error", source);
          }
        }
        if let Some(callback) = vm.host_root(callback_root) {
          vm.call_value_with_this(&callback, &Value::Undefined, &[event])?;
        }
        Ok(Value::Undefined)
      });
      set_socket_handler(vm, &socket, property, Value::Builtin(listener))?;
    }
    let subscription = object(vm.realm().object_prototype());
    let cancel_socket = socket.clone();
    let cancel: BuiltinFn = Rc::new(move |vm, _args| {
      for property in ["onmessage", "onclose", "onerror"] {
        set_socket_handler(vm, &cancel_socket, property, Value::Undefined)?;
      }
      if vm.host_root(callback_root).is_some() {
        vm.release_host_root(callback_root)?;
      }
      Ok(Value::Undefined)
    });
    set(&subscription, "__cancel", Value::Builtin(cancel));
    Ok(subscription)
  });
  set(&primitive, "subscribe", Value::Builtin(subscribe));

  let cancel: BuiltinFn = Rc::new(|vm, args| {
    let operation = args.get(1).cloned().unwrap_or(Value::Undefined);
    let cancel = vm.get_property_host(
      &operation,
      &PropertyKey::String("__cancel".to_string().into()),
    )?;
    if cancel.is_callable() {
      vm.call_value_with_this(&cancel, &operation, &[])?;
    }
    Ok(Value::Undefined)
  });
  set(&primitive, "cancel", Value::Builtin(cancel));
  if let Value::Object(object) = &primitive {
    object.borrow_mut().extensible = false;
  }
  primitive
}

fn set_socket_handler(
  vm: &mut Vm,
  socket: &Value,
  property: &str,
  value: Value,
) -> Result<(), RuntimeError> {
  vm.set_property_host(socket, &Value::String(property.to_string().into()), value)?;
  Ok(())
}

fn complete_success(
  vm: &mut Vm,
  callback: &Value,
  value: Value,
) -> Result<(), RuntimeError> {
  let result = object(vm.realm().object_prototype());
  set(&result, "ok", Value::Bool(true));
  set(&result, "value", value);
  vm.call_value_with_this(callback, &Value::Undefined, &[result])?;
  Ok(())
}

fn complete_failure(
  vm: &mut Vm,
  callback: &Value,
  operation: &str,
  message: &str,
) -> Result<(), RuntimeError> {
  let error = object(vm.realm().object_prototype());
  set(
    &error,
    "_tag",
    Value::String("WebSocketError".to_string().into()),
  );
  set(&error, "reason", Value::String("Host".to_string().into()));
  set(
    &error,
    "operation",
    Value::String(operation.to_string().into()),
  );
  set(&error, "message", Value::String(message.to_string().into()));
  let result = object(vm.realm().object_prototype());
  set(&result, "ok", Value::Bool(false));
  set(&result, "error", error);
  vm.call_value_with_this(callback, &Value::Undefined, &[result])?;
  Ok(())
}

pub(super) fn terminal_primitive(
  vm: &Vm,
  terminal: Rc<RefCell<Option<terminal::TerminalState>>>,
) -> Value {
  let primitive = object(vm.realm().object_prototype());

  for name in ["write", "writeLine"] {
    let state = Rc::clone(&terminal);
    let method: BuiltinFn = Rc::new(move |vm, args| {
      let text = args.get(1).map_or_else(
        || Ok(String::new()),
        |value| {
          vm.to_string(value)
            .map(|value| value.to_string_lossy())
        },
      )?;
      let complete = args.get(2).cloned().unwrap_or(Value::Undefined);
      if let Some(terminal) = state.borrow().clone() {
        terminal.write(text);
        complete_success(vm, &complete, Value::Undefined)?;
      } else {
        complete_failure(vm, &complete, "write", "terminal host is not initialized")?;
      }
      Ok(Value::Undefined)
    });
    set(&primitive, name, Value::Builtin(method));
  }

  let prompt_state = Rc::clone(&terminal);
  let set_prompt: BuiltinFn = Rc::new(move |vm, args| {
    let prompt = args.get(1).map_or_else(
      || Ok(String::new()),
      |value| {
        vm.to_string(value)
          .map(|value| value.to_string_lossy())
      },
    )?;
    let complete = args.get(2).cloned().unwrap_or(Value::Undefined);
    if let Some(terminal) = prompt_state.borrow().clone() {
      terminal.set_prompt(prompt);
      complete_success(vm, &complete, Value::Undefined)?;
    } else {
      complete_failure(
        vm,
        &complete,
        "setPrompt",
        "terminal host is not initialized",
      )?;
    }
    Ok(Value::Undefined)
  });
  set(&primitive, "setPrompt", Value::Builtin(set_prompt));

  let subscribe_state = Rc::clone(&terminal);
  let subscribe: BuiltinFn = Rc::new(move |vm, args| {
    let on_event = args.get(1).cloned().unwrap_or(Value::Undefined);
    let Some(terminal) = subscribe_state.borrow().clone() else {
      return Err(RuntimeError::from("terminal host is not initialized"));
    };
    let callback_root = vm.create_host_root(on_event);
    let line: BuiltinFn = Rc::new(move |vm, args| {
      let event = object(vm.realm().object_prototype());
      set(&event, "type", Value::String("line".to_string().into()));
      set(
        &event,
        "line",
        args
          .get(1)
          .cloned()
          .unwrap_or(Value::String(String::new().into())),
      );
      if let Some(callback) = vm.host_root(callback_root) {
        vm.call_value_with_this(&callback, &Value::Undefined, &[event])?;
      }
      Ok(Value::Undefined)
    });
    let close_root = callback_root;
    let close: BuiltinFn = Rc::new(move |vm, _args| {
      let event = object(vm.realm().object_prototype());
      set(&event, "type", Value::String("close".to_string().into()));
      if let Some(callback) = vm.host_root(close_root) {
        vm.call_value_with_this(&callback, &Value::Undefined, &[event])?;
      }
      Ok(Value::Undefined)
    });
    terminal.subscribe_lines(vm, Value::Builtin(line))?;
    terminal.subscribe_close(vm, Value::Builtin(close))?;
    let subscription = object(vm.realm().object_prototype());
    let cancel_terminal = terminal.clone();
    let cancel: BuiltinFn = Rc::new(move |vm, _args| {
      cancel_terminal.clear_line_subscription(vm)?;
      if vm.host_root(callback_root).is_some() {
        vm.release_host_root(callback_root)?;
      }
      Ok(Value::Undefined)
    });
    set(&subscription, "__cancel", Value::Builtin(cancel));
    Ok(subscription)
  });
  set(&primitive, "subscribeLines", Value::Builtin(subscribe));

  let close_state = Rc::clone(&terminal);
  let close: BuiltinFn = Rc::new(move |vm, args| {
    let complete = args.get(1).cloned().unwrap_or(Value::Undefined);
    if let Some(terminal) = close_state.borrow().clone() {
      terminal.close_and_notify(vm)?;
      complete_success(vm, &complete, Value::Undefined)?;
    } else {
      complete_failure(vm, &complete, "close", "terminal host is not initialized")?;
    }
    Ok(Value::Undefined)
  });
  set(&primitive, "close", Value::Builtin(close));

  let cancel: BuiltinFn = Rc::new(|vm, args| {
    let operation = args.get(1).cloned().unwrap_or(Value::Undefined);
    let cancel = vm.get_property_host(
      &operation,
      &PropertyKey::String("__cancel".to_string().into()),
    )?;
    if cancel.is_callable() {
      vm.call_value_with_this(&cancel, &operation, &[])?;
    }
    Ok(Value::Undefined)
  });
  set(&primitive, "cancel", Value::Builtin(cancel));
  if let Value::Object(object) = &primitive {
    object.borrow_mut().extensible = false;
  }
  primitive
}
