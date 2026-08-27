// SPDX-License-Identifier: MPL-2.0

use super::*;

/// Install the ABI object consumed by the prelinked private adapter.
///
/// Acquisition is lazy, expected validation/bind failures stay in the Effect
/// error channel, and scoped release closes the listener and active
/// connections idempotently. The transitional limitation remains that requests
/// are forwarded to the ordinary runtime event channel rather than invoking
/// the `blitz:app` router/layers; host capability validation does not hide it.
pub(super) struct HostBridgeInputs {
  pub(super) requests: mpsc::Sender<HttpRequestEvent>,
  pub(super) websocket_events: mpsc::Sender<WebSocketEvent>,
  pub(super) resources: ResourceRegistry,
  pub(super) server: Rc<RefCell<Option<server::ServerState>>>,
  pub(super) terminal: Rc<RefCell<Option<terminal::TerminalState>>>,
  pub(super) root: RootSupervisor,
  pub(super) node_file_system_allowed: bool,
  pub(super) crypto_random_allowed: bool,
}

pub(super) fn create_host_bridge(
  vm: &mut Vm,
  inputs: HostBridgeInputs,
) -> (Value, HostBridgeCapabilities) {
  let HostBridgeInputs {
    requests,
    websocket_events,
    resources,
    server,
    terminal,
    root,
    node_file_system_allowed,
    crypto_random_allowed,
  } = inputs;
  let app_transport = object(vm.realm().object_prototype());
  set(
    &app_transport,
    "openAppServer",
    Value::Builtin(open_app_server_method(
      requests,
      websocket_events,
      resources,
    )),
  );
  set(
    &app_transport,
    "closeAppServer",
    Value::Builtin(close_app_server_method()),
  );
  set(
    &app_transport,
    "awaitShutdown",
    Value::Builtin(await_shutdown_method()),
  );
  if let Value::Object(object) = &app_transport {
    object.borrow_mut().extensible = false;
  }
  let bridge = object(vm.realm().object_prototype());
  set(
    &bridge,
    "abiVersion",
    Value::Number(f64::from(HOST_BRIDGE_ABI_VERSION)),
  );
  set(&bridge, "appTransport", app_transport);
  set(&bridge, "fileSystem", fs::install(vm));
  set(
    &bridge,
    "nodeFileSystem",
    node_fs::install(vm, node_file_system_allowed),
  );
  set(
    &bridge,
    "nodeCryptoRandom",
    node_crypto::install(vm, crypto_random_allowed),
  );
  set(&bridge, "http", http_primitive(vm, Rc::clone(&server)));
  set(&bridge, "runtime", runtime_primitive(vm, root));
  set(&bridge, "webSocket", websocket_primitive(vm, server));
  set(&bridge, "terminal", terminal_primitive(vm, terminal));
  if let Value::Object(object) = &bridge {
    object.borrow_mut().extensible = false;
  }
  let capabilities = HostBridgeCapabilities::new(HOST_BRIDGE_ABI_VERSION)
    .with_primitive(
      APP_TRANSPORT_PRIMITIVE,
      HOST_BRIDGE_ABI_VERSION,
      APP_TRANSPORT_METHODS.iter().copied(),
    )
    .with_primitive(
      FILE_SYSTEM_PRIMITIVE,
      HOST_BRIDGE_ABI_VERSION,
      FILE_SYSTEM_METHODS.iter().copied(),
    )
    .with_primitive(
      NODE_FILE_SYSTEM_PRIMITIVE,
      HOST_BRIDGE_ABI_VERSION,
      NODE_FILE_SYSTEM_METHODS.iter().copied(),
    )
    .with_primitive(
      NODE_CRYPTO_RANDOM_PRIMITIVE,
      NODE_CRYPTO_RANDOM_ABI_VERSION,
      NODE_CRYPTO_RANDOM_METHODS.iter().copied(),
    )
    .with_primitive(
      HTTP_PRIMITIVE,
      HOST_BRIDGE_ABI_VERSION,
      HTTP_METHODS.iter().copied(),
    )
    .with_primitive(
      TERMINAL_PRIMITIVE,
      HOST_BRIDGE_ABI_VERSION,
      TERMINAL_METHODS.iter().copied(),
    )
    .with_primitive(
      WEB_SOCKET_PRIMITIVE,
      HOST_BRIDGE_ABI_VERSION,
      WEB_SOCKET_METHODS.iter().copied(),
    )
    .with_primitive(
      RUNTIME_CONTROL_PRIMITIVE,
      HOST_BRIDGE_ABI_VERSION,
      RUNTIME_CONTROL_METHODS.iter().copied(),
    );
  (bridge, capabilities)
}

pub(super) fn object(prototype: Value) -> Value {
  Value::Object(gc::track_object(Rc::new(RefCell::new(
    ObjectData::with_prototype(prototype),
  ))))
}

/// Build a public host callable with ordinary function metadata.
///
/// Bare `Value::Builtin` values are reserved for VM-internal callbacks whose
/// properties are not observable by application code.
pub(crate) fn host_function(
  vm: &Vm,
  name: &str,
  length: u32,
  function: BuiltinFn,
) -> Value {
  let properties = gc::track_object(Rc::new(RefCell::new(ObjectData::with_prototype(
    vm.realm().function_prototype(),
  ))));
  {
    let mut properties = properties.borrow_mut();
    properties.properties.insert(
      PropertyKey::String("name".to_string().into()),
      PropertyDescriptor::builtin_metadata(Value::String(name.to_string().into())),
    );
    properties.properties.insert(
      PropertyKey::String("length".to_string().into()),
      PropertyDescriptor::builtin_metadata(Value::Number(f64::from(length))),
    );
  }
  Value::BuiltinObject(function, properties)
}

pub(crate) fn host_constructor(
  vm: &Vm,
  name: &str,
  length: u32,
  prototype: Value,
  function: BuiltinFn,
) -> Value {
  let value = host_function(vm, name, length, function);
  if let Value::BuiltinObject(_, properties) = &value {
    properties.borrow_mut().properties.insert(
      PropertyKey::String("prototype".to_string().into()),
      PropertyDescriptor::ordinary_data(prototype),
    );
  }
  value
}

pub(super) fn set(
  target: &Value,
  name: &str,
  value: Value,
) {
  if let Value::Object(object) = target {
    object.borrow_mut().properties.insert(
      PropertyKey::String(name.to_string().into()),
      PropertyDescriptor::Data {
        value,
        writable: false,
        enumerable: false,
        configurable: false,
      },
    );
  }
}

pub(super) fn effect_function(
  vm: &mut Vm,
  name: &str,
) -> Result<Value, RuntimeError> {
  let record = {
    let loader = vm.module_loader_handle();
    let loader = loader.borrow();
    loader
      .map
      .values()
      .find(|record| record.borrow().specifier == "effect/Effect")
      .cloned()
  };
  let record = record.ok_or_else(|| RuntimeError::from("effect/Effect is not instantiated"))?;
  let slot = record
    .borrow()
    .exports
    .get(name)
    .cloned()
    .ok_or_else(|| RuntimeError::from(format!("effect/Effect has no {name} export")))?;
  Ok(slot.value())
}

fn effect_sync(
  vm: &mut Vm,
  thunk: BuiltinFn,
) -> Result<Value, RuntimeError> {
  let sync = effect_function(vm, "sync")?;
  vm.call_value_with_this(&sync, &Value::Undefined, &[Value::Builtin(thunk)])
}

fn transport_options(
  vm: &mut Vm,
  value: &Value,
) -> Result<AppTransportOptions, RuntimeError> {
  let hostname_value =
    vm.get_property_host(value, &PropertyKey::String("hostname".to_string().into()))?;
  let hostname = vm.to_string(&hostname_value)?;
  let port_value = vm.get_property_host(value, &PropertyKey::String("port".to_string().into()))?;
  let port_number = vm.to_number_value(&port_value)?.trunc();
  if !port_number.is_finite() || !(0.0_f64..=f64::from(u16::MAX)).contains(&port_number) {
    return Err(RuntimeError::from("application transport port is invalid"));
  }
  let port = port_number
    .to_string()
    .parse::<u16>()
    .map_err(|_| RuntimeError::from("application transport port is invalid"))?;
  let rpc_path_value =
    vm.get_property_host(value, &PropertyKey::String("rpcPath".to_string().into()))?;
  let rpc_path = vm.to_string(&rpc_path_value)?.to_string_lossy();
  Ok(AppTransportOptions {
    hostname: hostname.to_string_lossy(),
    port,
    rpc_path,
  })
}

fn serve_error_value(
  vm: &Vm,
  reason: &str,
  message: String,
) -> Value {
  let error = object(vm.realm().object_prototype());
  set(
    &error,
    "_tag",
    Value::String("ServeError".to_string().into()),
  );
  set(&error, "reason", Value::String(reason.to_string().into()));
  set(&error, "message", Value::String(message.into()));
  error
}

pub(super) fn effect_fail(
  vm: &mut Vm,
  error: Value,
) -> Result<Value, RuntimeError> {
  let fail = effect_function(vm, "fail")?;
  vm.call_value_with_this(&fail, &Value::Undefined, &[error])
}

pub(super) fn effect_succeed(
  vm: &mut Vm,
  value: Value,
) -> Result<Value, RuntimeError> {
  let succeed = effect_function(vm, "succeed")?;
  vm.call_value_with_this(&succeed, &Value::Undefined, &[value])
}

pub(super) fn effect_suspend(
  vm: &mut Vm,
  thunk: BuiltinFn,
) -> Result<Value, RuntimeError> {
  let suspend = effect_function(vm, "suspend")?;
  vm.call_value_with_this(&suspend, &Value::Undefined, &[Value::Builtin(thunk)])
}

fn open_app_server_method(
  requests: mpsc::Sender<HttpRequestEvent>,
  websocket_events: mpsc::Sender<WebSocketEvent>,
  resources: ResourceRegistry,
) -> BuiltinFn {
  Rc::new(move |vm, args| {
    let options = match args.get(1) {
      Some(value) => match transport_options(vm, value) {
        Ok(options) => options,
        Err(error) => {
          let error = serve_error_value(vm, "InvalidOptions", error.to_string());
          return effect_fail(vm, error);
        }
      },
      None => {
        let error = serve_error_value(
          vm,
          "InvalidOptions",
          "openAppServer requires options".to_string(),
        );
        return effect_fail(vm, error);
      }
    };
    let thunk: BuiltinFn = {
      let requests = requests.clone();
      let websocket_events = websocket_events.clone();
      let resources = resources.clone();
      Rc::new(move |vm, _args| {
        match vm.block_on(open_app_server(
          options.clone(),
          requests.clone(),
          websocket_events.clone(),
        ))? {
          Ok(resource) => {
            let resource_handle =
              resources.register(format!("app-server:{}", resource.local_addr()), true);
            let resource = resource_value(vm, Arc::new(resource), resource_handle);
            effect_succeed(vm, resource)
          }
          Err(error) => {
            let reason = match error {
              AppTransportError::InvalidOptions(_) => "InvalidOptions",
              AppTransportError::Bind(_) => "Bind",
            };
            let error = serve_error_value(vm, reason, error.to_string());
            effect_fail(vm, error)
          }
        }
      })
    };
    effect_suspend(vm, thunk)
  })
}

fn resource_value(
  vm: &mut Vm,
  resource: Arc<AppServerResource>,
  resource_handle: ResourceHandle,
) -> Value {
  let server = object(vm.realm().object_prototype());
  set(
    &server,
    "localAddr",
    Value::String(resource.local_addr().to_string().into()),
  );
  let close_resource = Arc::clone(&resource);
  let close: BuiltinFn = Rc::new(move |vm, _args| {
    vm.block_on(Arc::clone(&close_resource).close_owned())?;
    resource_handle.close();
    Ok(Value::Undefined)
  });
  set(&server, "__blitz_close", Value::Builtin(close));
  server
}

fn close_app_server_method() -> BuiltinFn {
  Rc::new(|vm, args| {
    let server = args
      .get(1)
      .ok_or_else(|| RuntimeError::from("closeAppServer requires a server"))?;
    let close = vm.get_property_host(
      server,
      &PropertyKey::String("__blitz_close".to_string().into()),
    )?;
    let thunk = match close {
      Value::Builtin(close) => close,
      _ => return Err(RuntimeError::from("invalid application server resource")),
    };
    effect_sync(vm, thunk)
  })
}

fn await_shutdown_method() -> BuiltinFn {
  Rc::new(|vm, args| {
    let server = args
      .get(1)
      .ok_or_else(|| RuntimeError::from("awaitShutdown requires a server"))?;
    let close = vm.get_property_host(
      server,
      &PropertyKey::String("__blitz_close".to_string().into()),
    )?;
    if !matches!(close, Value::Builtin(_)) {
      return Err(RuntimeError::from("invalid application server resource"));
    }
    effect_function(vm, "never")
  })
}
