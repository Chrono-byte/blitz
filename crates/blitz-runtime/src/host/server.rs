// SPDX-License-Identifier: MPL-2.0
//! `Blitz.serve` HTTP routing and VM callback bridge.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::rc::Rc;

use denko::errors::RuntimeError;
use denko::gc;
use denko::value::{
  ArrayData, BuiltinFn, ObjectData, PromiseState, PropertyDescriptor, PropertyKey, Value,
};
use denko::{HostRoot, Vm};
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::watch;

use crate::host::event_loop::{OperationId, OperationRegistry, ResourceHandle, ResourceRegistry};
use crate::host::{host_constructor, host_function};
use crate::net::http::{HttpRequestEvent, HttpResponse, HttpServer};
use crate::net::websocket::{
  WebSocketCommand, WebSocketEvent, WebSocketServerOptions, connect_client,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
enum WebSocketReadyState {
  Connecting = 0,
  Open = 1,
  Closing = 2,
  Closed = 3,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct WebSocketState {
  ready_state: WebSocketReadyState,
  buffered_amount: usize,
}

impl Default for WebSocketState {
  fn default() -> Self {
    Self {
      ready_state: WebSocketReadyState::Connecting,
      buffered_amount: 0,
    }
  }
}

impl WebSocketState {
  const fn ready_state(&self) -> WebSocketReadyState {
    self.ready_state
  }

  const fn buffered_amount(&self) -> usize {
    self.buffered_amount
  }

  fn open(&mut self) -> bool {
    if self.ready_state != WebSocketReadyState::Connecting {
      return false;
    }
    self.ready_state = WebSocketReadyState::Open;
    true
  }

  fn begin_close(&mut self) {
    if self.ready_state != WebSocketReadyState::Closed {
      self.ready_state = WebSocketReadyState::Closing;
    }
  }

  fn finish_close(&mut self) {
    self.ready_state = WebSocketReadyState::Closed;
    self.buffered_amount = 0;
  }

  fn queue_bytes(
    &mut self,
    amount: usize,
  ) -> bool {
    if self.ready_state != WebSocketReadyState::Open {
      return false;
    }
    let Some(total) = self.buffered_amount.checked_add(amount) else {
      return false;
    };
    self.buffered_amount = total;
    true
  }

  fn drain_bytes(
    &mut self,
    amount: usize,
  ) {
    self.buffered_amount = self.buffered_amount.saturating_sub(amount);
  }
}

struct WebSocketResource {
  object: Value,
  object_root: HostRoot,
  state: WebSocketState,
  commands: Option<mpsc::Sender<WebSocketCommand>>,
  shutdown: Option<watch::Sender<bool>>,
  listeners: BTreeMap<String, Vec<WebSocketListener>>,
}

struct WebSocketListener {
  callback: Value,
  root: HostRoot,
}

struct PendingHttpOperation {
  event: HttpRequestEvent,
  websocket_id: Option<u64>,
  promise_root: HostRoot,
  cancel_root: Option<HostRoot>,
}

struct ServerInner {
  listen_addr: SocketAddr,
  local_addr: Option<SocketAddr>,
  handler: Option<HostRoot>,
  requests: mpsc::Sender<HttpRequestEvent>,
  websocket_events: mpsc::Sender<WebSocketEvent>,
  resources: ResourceRegistry,
  server: Option<HttpServer>,
  server_resource: Option<ResourceHandle>,
  finished: Option<HostRoot>,
  generation: u64,
  current_websocket_id: Option<u64>,
  pending_websocket_ids: BTreeSet<u64>,
  pending_http: OperationRegistry<PendingHttpOperation>,
  websockets: BTreeMap<u64, WebSocketResource>,
  next_client_id: u64,
  websocket_prototype: Option<Value>,
}

/// VM-thread state for registered routes and the server resource.
#[derive(Clone)]
pub(super) struct ServerState(Rc<RefCell<ServerInner>>);

impl ServerState {
  pub(super) fn has_server(&self) -> bool {
    self.0.borrow().server.is_some()
  }

  pub(super) fn has_resources(&self) -> bool {
    let inner = self.0.borrow();
    inner
      .server_resource
      .as_ref()
      .is_some_and(ResourceHandle::is_referenced)
      || !inner.websockets.is_empty()
  }

  pub(super) fn shutdown_websockets(&self) {
    let inner = self.0.borrow();
    for resource in inner.websockets.values() {
      if let Some(shutdown) = &resource.shutdown {
        let _result = shutdown.send(true);
      }
    }
  }

  pub(super) fn local_addr(&self) -> Option<SocketAddr> {
    self.0.borrow().local_addr
  }

  pub(super) fn websocket_prototype(&self) -> Option<Value> {
    self.0.borrow().websocket_prototype.clone()
  }

  pub(super) fn close_current(
    &self,
    vm: &mut Vm,
  ) -> Result<Value, RuntimeError> {
    let generation = self.0.borrow().generation;
    let finished = self.finished_value(vm);
    self.close(vm, generation, &finished)?;
    Ok(finished)
  }

  fn finished_value(
    &self,
    vm: &Vm,
  ) -> Value {
    self
      .0
      .borrow()
      .finished
      .and_then(|root| vm.host_root(root))
      .unwrap_or_else(|| resolved_promise(Value::Undefined))
  }

  fn close(
    &self,
    vm: &mut Vm,
    generation: u64,
    finished: &Value,
  ) -> Result<(), RuntimeError> {
    let (server, resource, handler, pending_http) = {
      let mut inner = self.0.borrow_mut();
      if inner.generation != generation {
        return Ok(());
      }
      inner.local_addr = None;
      (
        inner.server.take(),
        inner.server_resource.take(),
        inner.handler.take(),
        inner.pending_http.cancel_all(),
      )
    };
    for pending in pending_http {
      if let Some(cancel_root) = pending.cancel_root {
        if let Some(cancel) = vm.host_root(cancel_root) {
          vm.call_value_with_this(&cancel, &Value::Undefined, &[])?;
        }
        vm.release_host_root(cancel_root)?;
      }
      vm.release_host_root(pending.promise_root)?;
      self.finish_request(pending.websocket_id);
      pending
        .event
        .respond(HttpResponse::text(503, "server shutting down"));
    }
    if let Some(handler) = handler {
      vm.release_host_root(handler)?;
    }
    self.shutdown_websockets();
    if let Some(server) = server {
      vm.block_on(server.close())?;
    }
    if let Some(resource) = resource {
      resource.close();
    }
    if matches!(finished, Value::Promise(..)) {
      vm.resolve_host_promise(finished, Value::Undefined)?;
    }
    Ok(())
  }

  pub(super) fn dispatch(
    &self,
    vm: &mut Vm,
    event: HttpRequestEvent,
  ) -> Result<(), RuntimeError> {
    let callback = self
      .0
      .borrow()
      .handler
      .and_then(|root| vm.host_root(root));
    let Some(callback) = callback else {
      event.respond(HttpResponse::text(404, "Not Found"));
      return Ok(());
    };

    let websocket_id = event.websocket_id();
    {
      let mut inner = self.0.borrow_mut();
      inner.current_websocket_id = websocket_id;
      if let Some(id) = websocket_id {
        inner.pending_websocket_ids.insert(id);
      }
    }
    let request = request_object(vm, &event);
    let info = handler_info_object(vm, event.remote_addr);
    let result = match vm.call_value_with_this(&callback, &Value::Undefined, &[request, info]) {
      Ok(result) => result,
      Err(error) => {
        eprintln!("HTTP handler error: {error}");
        self.finish_request(websocket_id);
        event.respond(HttpResponse::text(500, "Internal Server Error"));
        return Ok(());
      }
    };
    if matches!(result, Value::Promise(..)) {
      let promise_root = vm.create_host_root(result.clone());
      let cancel_root = vm
        .get_property_host(
          &result,
          &PropertyKey::String("__blitz_cancel".to_string().into()),
        )
        .ok()
        .filter(Value::is_callable)
        .map(|cancel| vm.create_host_root(cancel));
      let operation = {
        let mut inner = self.0.borrow_mut();
        let operation = inner.pending_http.register(
          "http-handler",
          PendingHttpOperation {
            event,
            websocket_id,
            promise_root,
            cancel_root,
          },
        );
        let _started = inner.pending_http.start(operation);
        operation
      };
      let success_state = self.clone();
      let on_success: BuiltinFn = Rc::new(move |vm, args| {
        let value = args
          .get(1)
          .or_else(|| args.first())
          .cloned()
          .unwrap_or(Value::Undefined);
        success_state.complete_pending_request(vm, operation, &value)?;
        Ok(Value::Undefined)
      });
      let failure_state = self.clone();
      let on_failure: BuiltinFn = Rc::new(move |vm, args| {
        let reason = args
          .get(1)
          .or_else(|| args.first())
          .cloned()
          .unwrap_or(Value::Undefined);
        failure_state.fail_pending_request(vm, operation, &reason)?;
        Ok(Value::Undefined)
      });
      let attach_result = (|| {
        let then =
          vm.get_property_host(&result, &PropertyKey::String("then".to_string().into()))?;
        vm.call_value_with_this(
          &then,
          &result,
          &[Value::Builtin(on_success), Value::Builtin(on_failure)],
        )
      })();
      if let Err(error) = attach_result {
        self.abort_pending_request(vm, operation)?;
        return Err(error);
      }
      self.0.borrow_mut().current_websocket_id = None;
      return Ok(());
    }
    self.complete_request(vm, event, websocket_id, &result);
    Ok(())
  }

  fn take_pending_request(
    &self,
    operation: OperationId,
  ) -> Option<PendingHttpOperation> {
    self
      .0
      .borrow_mut()
      .pending_http
      .complete(operation)
  }

  fn complete_pending_request(
    &self,
    vm: &mut Vm,
    operation: OperationId,
    result: &Value,
  ) -> Result<(), RuntimeError> {
    let Some(pending) = self.take_pending_request(operation) else {
      return Ok(());
    };
    release_pending_roots(vm, &pending)?;
    self.complete_request(vm, pending.event, pending.websocket_id, result);
    Ok(())
  }

  fn fail_pending_request(
    &self,
    vm: &mut Vm,
    operation: OperationId,
    reason: &Value,
  ) -> Result<(), RuntimeError> {
    let Some(pending) = self.take_pending_request(operation) else {
      return Ok(());
    };
    eprintln!("HTTP handler promise rejected: {reason}");
    release_pending_roots(vm, &pending)?;
    self.finish_request(pending.websocket_id);
    pending
      .event
      .respond(HttpResponse::text(500, "Internal Server Error"));
    Ok(())
  }

  fn abort_pending_request(
    &self,
    vm: &mut Vm,
    operation: OperationId,
  ) -> Result<(), RuntimeError> {
    let Some(pending) = self.take_pending_request(operation) else {
      return Ok(());
    };
    if let Some(cancel) = pending.cancel_root
      && let Some(cancel) = vm.host_root(cancel)
    {
      vm.call_value_with_this(&cancel, &Value::Undefined, &[])?;
    }
    release_pending_roots(vm, &pending)?;
    self.finish_request(pending.websocket_id);
    pending
      .event
      .respond(HttpResponse::text(500, "Internal Server Error"));
    Ok(())
  }

  fn finish_request(
    &self,
    websocket_id: Option<u64>,
  ) {
    let mut inner = self.0.borrow_mut();
    if let Some(id) = websocket_id {
      inner.pending_websocket_ids.remove(&id);
    }
    if inner.current_websocket_id == websocket_id {
      inner.current_websocket_id = None;
    }
  }

  fn complete_request(
    &self,
    vm: &mut Vm,
    event: HttpRequestEvent,
    websocket_id: Option<u64>,
    result: &Value,
  ) {
    self.finish_request(websocket_id);
    match response_from_value(vm, result) {
      Ok(HandlerResponse::Http(response)) => event.respond(response),
      Ok(HandlerResponse::WebSocket) => event.respond_websocket(),
      Err(_) => event.respond(HttpResponse::text(500, "handler must return a Response")),
    }
  }

  pub(super) fn dispatch_websocket(
    &self,
    vm: &mut Vm,
    event: WebSocketEvent,
  ) -> Result<(), RuntimeError> {
    match event {
      WebSocketEvent::Open {
        connection_id,
        commands,
      } => {
        let socket = {
          let mut inner = self.0.borrow_mut();
          let Some(resource) = inner.websockets.get_mut(&connection_id) else {
            return Ok(());
          };
          resource.commands = Some(commands);
          resource.state.open();
          set_socket_state(resource);
          resource.object.clone()
        };
        dispatch_socket_event(
          vm,
          &self.0,
          connection_id,
          &socket,
          "open",
          event_object(vm, "open"),
        )?;
      }
      WebSocketEvent::Text {
        connection_id,
        text,
      } => {
        let Some(socket) = self.socket_object(connection_id) else {
          return Ok(());
        };
        let message = event_object(vm, "message");
        set(&message, "data", Value::String(text.into()));
        dispatch_socket_event(vm, &self.0, connection_id, &socket, "message", message)?;
      }
      WebSocketEvent::Binary {
        connection_id,
        data,
      } => {
        let Some(socket) = self.socket_object(connection_id) else {
          return Ok(());
        };
        let message = event_object(vm, "message");
        set(&message, "data", byte_array(vm, data));
        dispatch_socket_event(vm, &self.0, connection_id, &socket, "message", message)?;
      }
      WebSocketEvent::Drain {
        connection_id,
        bytes,
      } => {
        if let Some(resource) = self
          .0
          .borrow_mut()
          .websockets
          .get_mut(&connection_id)
        {
          resource.state.drain_bytes(bytes);
          set_socket_state(resource);
        }
      }
      WebSocketEvent::Close {
        connection_id,
        code,
        reason,
      } => {
        let socket = {
          let mut inner = self.0.borrow_mut();
          let Some(resource) = inner.websockets.get_mut(&connection_id) else {
            return Ok(());
          };
          resource.state.finish_close();
          resource.commands = None;
          set_socket_state(resource);
          resource.object.clone()
        };
        let close = event_object(vm, "close");
        set(
          &close,
          "code",
          Value::Number(f64::from(code.unwrap_or(1005))),
        );
        set(&close, "reason", Value::String(reason.into()));
        set(&close, "wasClean", Value::Bool(code.is_some()));
        dispatch_socket_event(vm, &self.0, connection_id, &socket, "close", close)?;
        if let Some(resource) = self
          .0
          .borrow_mut()
          .websockets
          .remove(&connection_id)
        {
          release_websocket_roots(vm, resource)?;
        }
      }
      WebSocketEvent::Error {
        connection_id,
        message,
      } => {
        let Some(socket) = self.socket_object(connection_id) else {
          return Ok(());
        };
        let error = event_object(vm, "error");
        set(&error, "message", Value::String(message.into()));
        dispatch_socket_event(vm, &self.0, connection_id, &socket, "error", error)?;
      }
    }
    Ok(())
  }

  fn socket_object(
    &self,
    connection_id: u64,
  ) -> Option<Value> {
    self
      .0
      .borrow()
      .websockets
      .get(&connection_id)
      .map(|resource| resource.object.clone())
  }
}

fn release_pending_roots(
  vm: &mut Vm,
  pending: &PendingHttpOperation,
) -> Result<(), RuntimeError> {
  vm.release_host_root(pending.promise_root)?;
  if let Some(cancel) = pending.cancel_root {
    vm.release_host_root(cancel)?;
  }
  Ok(())
}

pub(super) fn install(
  vm: &mut Vm,
  listen_addr: SocketAddr,
  requests: mpsc::Sender<HttpRequestEvent>,
  websocket_events: mpsc::Sender<WebSocketEvent>,
  resources: ResourceRegistry,
  install_compatibility_globals: bool,
) -> ServerState {
  let state = ServerState(Rc::new(RefCell::new(ServerInner {
    listen_addr,
    local_addr: None,
    handler: None,
    requests,
    websocket_events,
    resources,
    server: None,
    server_resource: None,
    finished: None,
    generation: 0,
    current_websocket_id: None,
    pending_websocket_ids: BTreeSet::new(),
    pending_http: OperationRegistry::new(),
    websockets: BTreeMap::new(),
    next_client_id: 1_u64 << 53,
    websocket_prototype: None,
  })));
  let websocket_prototype = object(vm.realm().object_prototype());
  state.0.borrow_mut().websocket_prototype = Some(websocket_prototype.clone());
  if install_compatibility_globals {
    let blitz = object(vm.realm().object_prototype());
    set(
      &blitz,
      "serve",
      host_function(vm, "serve", 1, serve_method(state.clone())),
    );
    set(
      &blitz,
      "upgradeWebSocket",
      host_function(
        vm,
        "upgradeWebSocket",
        1,
        upgrade_websocket_method(state.clone(), websocket_prototype.clone()),
      ),
    );
    vm.realm().set_global("Blitz", blitz);
  }
  install_response_constructor(vm);
  install_headers_constructor(vm);
  install_websocket_constructor(vm, state.clone(), websocket_prototype);
  state
}

pub(super) fn serve_method(state: ServerState) -> BuiltinFn {
  Rc::new(move |vm, args| {
    let actual = args.get(1..).unwrap_or_default();
    let default_addr = state.0.borrow().listen_addr;
    let (address, callback) = serve_arguments(vm, actual, default_addr)?;

    let needs_server = !state.has_server();
    if needs_server {
      let requests = state.0.borrow().requests.clone();
      let websocket_events = state.0.borrow().websocket_events.clone();
      let server = vm
        .block_on(HttpServer::bind(
          address,
          requests,
          websocket_events,
          WebSocketServerOptions::default(),
        ))?
        .map_err(|error| RuntimeError::from(format!("failed to bind HTTP server: {error}")))?;
      let local_addr = server.local_addr();
      let finished = vm.create_host_promise();
      let finished_root = vm.create_host_root(finished);
      let mut inner = state.0.borrow_mut();
      if let Some(previous) = inner.finished.replace(finished_root) {
        vm.release_host_root(previous)?;
      }
      inner.generation = inner.generation.wrapping_add(1);
      inner.local_addr = Some(local_addr);
      inner.server_resource = Some(inner.resources.register("http-server", true));
      inner.server = Some(server);
    }
    let callback = vm.create_host_root(callback);
    if let Some(previous) = state.0.borrow_mut().handler.replace(callback) {
      vm.release_host_root(previous)?;
    }

    let (local_addr, generation, finished) = {
      let inner = state.0.borrow();
      let local_addr = inner
        .local_addr
        .ok_or_else(|| RuntimeError::from("HTTP server did not expose a bound address"))?;
      let finished = inner
        .finished
        .and_then(|root| vm.host_root(root))
        .ok_or_else(|| RuntimeError::from("HTTP server did not expose completion state"))?;
      (local_addr, inner.generation, finished)
    };
    Ok(server_handle(
      vm,
      state.clone(),
      local_addr,
      generation,
      finished,
    ))
  })
}

fn server_handle(
  vm: &Vm,
  state: ServerState,
  local_addr: SocketAddr,
  generation: u64,
  finished: Value,
) -> Value {
  let handle = object(vm.realm().object_prototype());
  let hostname = local_addr.ip().to_string();
  let port = f64::from(local_addr.port());
  let address = object(vm.realm().object_prototype());
  set(
    &address,
    "transport",
    Value::String("tcp".to_string().into()),
  );
  set(&address, "hostname", Value::String(hostname.clone().into()));
  set(&address, "port", Value::Number(port));
  set(&handle, "addr", address);
  set(&handle, "finished", finished.clone());
  set(&handle, "hostname", Value::String(hostname.into()));
  set(&handle, "port", Value::Number(port));

  let shutdown_state = state.clone();
  let shutdown_finished = finished;
  let shutdown: BuiltinFn = Rc::new(move |vm, _args| {
    shutdown_state.close(vm, generation, &shutdown_finished)?;
    Ok(shutdown_finished.clone())
  });
  set(
    &handle,
    "shutdown",
    host_function(vm, "shutdown", 0, shutdown),
  );

  let reference_state = state.clone();
  let reference: BuiltinFn = Rc::new(move |_vm, _args| {
    let inner = reference_state.0.borrow();
    if inner.generation == generation
      && let Some(resource) = &inner.server_resource
    {
      resource.reference();
    }
    Ok(Value::Undefined)
  });
  set(&handle, "ref", host_function(vm, "ref", 0, reference));

  let unreference: BuiltinFn = Rc::new(move |_vm, _args| {
    let inner = state.0.borrow();
    if inner.generation == generation
      && let Some(resource) = &inner.server_resource
    {
      resource.unreference();
    }
    Ok(Value::Undefined)
  });
  set(&handle, "unref", host_function(vm, "unref", 0, unreference));
  handle
}

pub(super) fn upgrade_websocket_method(
  state: ServerState,
  prototype: Value,
) -> BuiltinFn {
  Rc::new(move |vm, args| {
    let request = args
      .get(1)
      .ok_or_else(|| RuntimeError::from("Blitz.upgradeWebSocket requires a Request"))?;
    let id = vm.get_property_host(
      request,
      &PropertyKey::String("__blitz_websocket_id".to_string().into()),
    )?;
    let Value::String(id) = id else {
      return Err(RuntimeError::from("request cannot be upgraded"));
    };
    let id = id
      .parse::<u64>()
      .map_err(|_| RuntimeError::from("invalid WebSocket request"))?;
    {
      let inner = state.0.borrow();
      if (inner.current_websocket_id != Some(id) && !inner.pending_websocket_ids.contains(&id))
        || inner.websockets.contains_key(&id)
      {
        return Err(RuntimeError::from("request cannot be upgraded"));
      }
    }

    let url = vm.get_property_host(request, &PropertyKey::String("url".to_string().into()))?;
    let url = vm
      .to_string(&url)?
      .to_string_lossy()
      .replacen("http://", "ws://", 1);
    let socket = websocket_object(vm, state.clone(), id, &url, prototype.clone());
    let object_root = vm.create_host_root(socket.clone());
    state.0.borrow_mut().websockets.insert(
      id,
      WebSocketResource {
        object: socket.clone(),
        object_root,
        state: WebSocketState::default(),
        commands: None,
        shutdown: None,
        listeners: BTreeMap::new(),
      },
    );
    let response = object(vm.realm().object_prototype());
    set(&response, "__blitz_response", Value::Bool(true));
    set(&response, "__blitz_upgrade_response", Value::Bool(true));
    set(&response, "status", Value::Number(101.0));
    set(&response, "body", Value::String(String::new().into()));
    let result = object(vm.realm().object_prototype());
    set(&result, "socket", socket);
    set(&result, "response", response);
    Ok(result)
  })
}

fn serve_arguments(
  vm: &mut Vm,
  actual: &[Value],
  default_addr: SocketAddr,
) -> Result<(SocketAddr, Value), RuntimeError> {
  let first = actual
    .first()
    .ok_or_else(|| RuntimeError::from("Blitz.serve requires a handler"))?;
  if first.is_callable() {
    return Ok((default_addr, first.clone()));
  }

  let callback = actual
    .get(1)
    .cloned()
    .ok_or_else(|| RuntimeError::from("Blitz.serve requires a handler"))?;
  if !callback.is_callable() {
    return Err(RuntimeError::from("Blitz.serve handler must be callable"));
  }
  let Value::Object(_) = first else {
    return Err(RuntimeError::from(
      "Blitz.serve expects a handler or an options object and handler",
    ));
  };
  let hostname =
    vm.get_property_host(first, &PropertyKey::String("hostname".to_string().into()))?;
  let hostname = match hostname {
    Value::Undefined => default_addr.ip().to_string(),
    value => vm.to_string(&value)?.to_string_lossy(),
  };
  let port = vm.get_property_host(first, &PropertyKey::String("port".to_string().into()))?;
  let port = match port {
    Value::Undefined => default_addr.port(),
    Value::Number(port) if port.is_finite() && port.fract() == 0.0 => port
      .to_string()
      .parse::<u16>()
      .map_err(|_| RuntimeError::from("Blitz.serve port must be between 0 and 65535"))?,
    _ => return Err(RuntimeError::from("Blitz.serve port must be an integer")),
  };
  let address = format!("{hostname}:{port}")
    .parse::<SocketAddr>()
    .map_err(|_| RuntimeError::from("Blitz.serve hostname must be an IP address"))?;
  Ok((address, callback))
}

fn request_object(
  vm: &Vm,
  event: &HttpRequestEvent,
) -> Value {
  let request = object(vm.realm().object_prototype());
  set(
    &request,
    "method",
    Value::String(event.method.clone().into()),
  );
  set(&request, "path", Value::String(event.path.clone().into()));
  set(&request, "url", Value::String(event.url.clone().into()));
  if let Some(id) = event.websocket_id() {
    set(
      &request,
      "__blitz_websocket_id",
      Value::String(id.to_string().into()),
    );
  }
  set(&request, "headers", headers_object(vm, &event.headers));
  set(
    &request,
    "query",
    event
      .query
      .as_ref()
      .map_or(Value::Null, |query| Value::String(query.clone().into())),
  );
  set(
    &request,
    "body",
    Value::String(
      String::from_utf8_lossy(&event.body)
        .into_owned()
        .into(),
    ),
  );
  let body = String::from_utf8_lossy(&event.body).into_owned();
  let text: BuiltinFn =
    Rc::new(move |_vm, _args| Ok(resolved_promise(Value::String(body.clone().into()))));
  set(&request, "text", host_function(vm, "text", 0, text));
  let body = String::from_utf8_lossy(&event.body).into_owned();
  let json: BuiltinFn = Rc::new(move |vm, _args| Ok(json_promise(vm, &body)));
  set(&request, "json", host_function(vm, "json", 0, json));
  request
}

fn websocket_object(
  vm: &Vm,
  state: ServerState,
  connection_id: u64,
  url: &str,
  prototype: Value,
) -> Value {
  let socket = object(prototype);
  set(&socket, "readyState", Value::Number(0.0));
  set(&socket, "bufferedAmount", Value::Number(0.0));
  set(
    &socket,
    "binaryType",
    Value::String("arraybuffer".to_string().into()),
  );
  set(&socket, "protocol", Value::String(String::new().into()));
  set(&socket, "extensions", Value::String(String::new().into()));
  set(&socket, "url", Value::String(url.to_string().into()));
  set(&socket, "CONNECTING", Value::Number(0.0));
  set(&socket, "OPEN", Value::Number(1.0));
  set(&socket, "CLOSING", Value::Number(2.0));
  set(&socket, "CLOSED", Value::Number(3.0));

  let send_state = state.clone();
  let send: BuiltinFn = Rc::new(move |vm, args| {
    let data = args
      .get(1)
      .ok_or_else(|| RuntimeError::from("WebSocket.send requires data"))?;
    let command = match data {
      Value::Array(elements, _) => {
        let mut bytes = Vec::new();
        for element in elements.borrow().clone_elements() {
          let Some(Value::Number(value)) = element else {
            return Err(RuntimeError::from(
              "binary WebSocket data must contain bytes",
            ));
          };
          let byte = value
            .to_string()
            .parse::<u8>()
            .map_err(|_| RuntimeError::from("binary WebSocket data must contain bytes"))?;
          bytes.push(byte);
        }
        WebSocketCommand::Binary(bytes)
      }
      value => WebSocketCommand::Text(vm.to_string(value)?.to_string_lossy()),
    };
    send_websocket_command(&send_state, connection_id, command)?;
    Ok(Value::Undefined)
  });
  set(&socket, "send", host_function(vm, "send", 1, send));

  let close_state = state.clone();
  let close: BuiltinFn = Rc::new(move |vm, args| {
    let code = match args.get(1) {
      None | Some(Value::Undefined) => 1000,
      Some(Value::Number(code)) if code.fract() == 0.0 => code
        .to_string()
        .parse::<u16>()
        .map_err(|_| RuntimeError::from("invalid WebSocket close code"))?,
      _ => return Err(RuntimeError::from("invalid WebSocket close code")),
    };
    if !is_valid_websocket_close_code(code) {
      return Err(RuntimeError::from("invalid WebSocket close code"));
    }
    let reason = args.get(2).map_or_else(
      || Ok(String::new()),
      |value| {
        vm.to_string(value)
          .map(|value| value.to_string_lossy())
      },
    )?;
    if reason.len() > 123 {
      return Err(RuntimeError::from(
        "WebSocket close reason exceeds 123 bytes",
      ));
    }
    send_websocket_command(
      &close_state,
      connection_id,
      WebSocketCommand::Close { code, reason },
    )?;
    if let Some(resource) = close_state
      .0
      .borrow_mut()
      .websockets
      .get_mut(&connection_id)
    {
      resource.state.begin_close();
      set_socket_state(resource);
    }
    Ok(Value::Undefined)
  });
  set(&socket, "close", host_function(vm, "close", 0, close));

  let listener_state = state.clone();
  let add_listener: BuiltinFn = Rc::new(move |vm, args| {
    let event_type = args
      .get(1)
      .ok_or_else(|| RuntimeError::from("addEventListener requires an event type"))?;
    let callback = args
      .get(2)
      .cloned()
      .ok_or_else(|| RuntimeError::from("addEventListener requires a callback"))?;
    if !callback.is_callable() {
      return Ok(Value::Undefined);
    }
    let event_type = vm.to_string(event_type)?.to_string_lossy();
    if let Some(resource) = listener_state
      .0
      .borrow_mut()
      .websockets
      .get_mut(&connection_id)
    {
      let root = vm.create_host_root(callback.clone());
      resource
        .listeners
        .entry(event_type)
        .or_default()
        .push(WebSocketListener { callback, root });
    }
    Ok(Value::Undefined)
  });
  set(
    &socket,
    "addEventListener",
    host_function(vm, "addEventListener", 2, add_listener),
  );

  let listener_state = state;
  let remove_listener: BuiltinFn = Rc::new(move |vm, args| {
    let Some(event_type) = args.get(1) else {
      return Ok(Value::Undefined);
    };
    let Some(callback) = args.get(2) else {
      return Ok(Value::Undefined);
    };
    let event_type = vm.to_string(event_type)?.to_string_lossy();
    let removed = {
      let mut inner = listener_state.0.borrow_mut();
      let Some(listeners) = inner
        .websockets
        .get_mut(&connection_id)
        .and_then(|resource| resource.listeners.get_mut(&event_type))
      else {
        return Ok(Value::Undefined);
      };
      let mut removed = Vec::new();
      listeners.retain(|listener| {
        if listener.callback == *callback {
          removed.push(listener.root);
          false
        } else {
          true
        }
      });
      removed
    };
    for root in removed {
      vm.release_host_root(root)?;
    }
    Ok(Value::Undefined)
  });
  set(
    &socket,
    "removeEventListener",
    host_function(vm, "removeEventListener", 2, remove_listener),
  );
  socket
}

fn send_websocket_command(
  state: &ServerState,
  connection_id: u64,
  command: WebSocketCommand,
) -> Result<(), RuntimeError> {
  let amount = match &command {
    WebSocketCommand::Text(text) => text.len(),
    WebSocketCommand::Binary(data) => data.len(),
    WebSocketCommand::Close { .. } => 0,
  };
  let commands = {
    let mut inner = state.0.borrow_mut();
    let resource = inner
      .websockets
      .get_mut(&connection_id)
      .ok_or_else(|| RuntimeError::from("WebSocket is closed"))?;
    if resource.state.ready_state() != WebSocketReadyState::Open {
      return Err(RuntimeError::from("WebSocket is not open"));
    }
    if amount > 0 && !resource.state.queue_bytes(amount) {
      return Err(RuntimeError::from("WebSocket buffered amount overflow"));
    }
    set_socket_state(resource);
    resource
      .commands
      .clone()
      .ok_or_else(|| RuntimeError::from("WebSocket is not open"))?
  };
  commands.try_send(command).map_err(|error| {
    if let Some(resource) = state
      .0
      .borrow_mut()
      .websockets
      .get_mut(&connection_id)
    {
      resource.state.drain_bytes(amount);
      set_socket_state(resource);
    }
    match error {
      TrySendError::Full(_) => RuntimeError::from("WebSocket send queue is full"),
      TrySendError::Closed(_) => RuntimeError::from("WebSocket is closed"),
    }
  })
}

fn set_socket_state(resource: &WebSocketResource) {
  set(
    &resource.object,
    "readyState",
    Value::Number(f64::from(resource.state.ready_state() as u8)),
  );
  set(
    &resource.object,
    "bufferedAmount",
    Value::Number(
      resource
        .state
        .buffered_amount()
        .to_string()
        .parse::<f64>()
        .unwrap_or(f64::MAX),
    ),
  );
}

fn release_websocket_roots(
  vm: &mut Vm,
  resource: WebSocketResource,
) -> Result<(), RuntimeError> {
  vm.release_host_root(resource.object_root)?;
  for listener in resource.listeners.into_values().flatten() {
    vm.release_host_root(listener.root)?;
  }
  Ok(())
}

const fn is_valid_websocket_close_code(code: u16) -> bool {
  matches!(code, 1000..=1003 | 1007..=1014 | 3000..=4999)
}

fn dispatch_socket_event(
  vm: &mut Vm,
  state: &Rc<RefCell<ServerInner>>,
  connection_id: u64,
  socket: &Value,
  event_type: &str,
  event: Value,
) -> Result<(), RuntimeError> {
  let property = format!("on{event_type}");
  if let Ok(callback) = vm.get_property_host(socket, &PropertyKey::String(property.into()))
    && callback.is_callable()
  {
    vm.call_value_with_this(&callback, socket, std::slice::from_ref(&event))?;
  }
  let listeners = state
    .borrow()
    .websockets
    .get(&connection_id)
    .and_then(|resource| resource.listeners.get(event_type))
    .map(|listeners| {
      listeners
        .iter()
        .map(|listener| listener.callback.clone())
        .collect::<Vec<_>>()
    })
    .unwrap_or_default();
  for listener in listeners {
    vm.call_value_with_this(&listener, socket, std::slice::from_ref(&event))?;
  }
  Ok(())
}

fn event_object(
  vm: &Vm,
  event_type: &str,
) -> Value {
  let event = object(vm.realm().object_prototype());
  set(&event, "type", Value::String(event_type.to_string().into()));
  event
}

fn byte_array(
  vm: &Vm,
  data: Vec<u8>,
) -> Value {
  let elements = data
    .into_iter()
    .map(|byte| Some(Value::Number(f64::from(byte))))
    .collect();
  Value::Array(
    gc::track_array(Rc::new(RefCell::new(ArrayData::new(elements)))),
    gc::track_object(Rc::new(RefCell::new(ObjectData::with_prototype(
      vm.realm().array_prototype(),
    )))),
  )
}

fn headers_object(
  vm: &Vm,
  entries: &[(String, String)],
) -> Value {
  let values = Rc::new(RefCell::new(
    entries
      .iter()
      .cloned()
      .collect::<BTreeMap<_, _>>(),
  ));
  let headers = object(vm.realm().object_prototype());
  for (name, value) in entries {
    set(&headers, name, Value::String(value.clone().into()));
  }

  let get_values = Rc::clone(&values);
  let get: BuiltinFn = Rc::new(move |vm, args| {
    let Some(name) = args.get(1) else {
      return Ok(Value::Null);
    };
    let name = vm
      .to_string(name)?
      .as_str_lossy()
      .to_ascii_lowercase();
    Ok(
      get_values
        .borrow()
        .get(&name)
        .map_or(Value::Null, |value| Value::String(value.clone().into())),
    )
  });
  set(&headers, "get", host_function(vm, "get", 1, get));

  let has_values = Rc::clone(&values);
  let has: BuiltinFn = Rc::new(move |vm, args| {
    let Some(name) = args.get(1) else {
      return Ok(Value::Bool(false));
    };
    let name = vm
      .to_string(name)?
      .as_str_lossy()
      .to_ascii_lowercase();
    Ok(Value::Bool(has_values.borrow().contains_key(&name)))
  });
  set(&headers, "has", host_function(vm, "has", 1, has));

  let set_values = Rc::clone(&values);
  let set_header: BuiltinFn = Rc::new(move |vm, args| {
    let name = args
      .get(1)
      .ok_or_else(|| RuntimeError::from("Headers.set requires a name"))?;
    let value = args
      .get(2)
      .ok_or_else(|| RuntimeError::from("Headers.set requires a value"))?;
    let name = vm
      .to_string(name)?
      .as_str_lossy()
      .to_ascii_lowercase();
    let value = vm.to_string(value)?.to_string_lossy();
    set_values
      .borrow_mut()
      .insert(name.clone(), value.clone());
    if let Some(receiver) = args.first() {
      set(receiver, &name, Value::String(value.into()));
    }
    Ok(Value::Undefined)
  });
  set(&headers, "set", host_function(vm, "set", 2, set_header));

  let append_values = Rc::clone(&values);
  let append: BuiltinFn = Rc::new(move |vm, args| {
    let name = args
      .get(1)
      .ok_or_else(|| RuntimeError::from("Headers.append requires a name"))?;
    let value = args
      .get(2)
      .ok_or_else(|| RuntimeError::from("Headers.append requires a value"))?;
    let name = vm
      .to_string(name)?
      .as_str_lossy()
      .to_ascii_lowercase();
    let value = vm.to_string(value)?.to_string_lossy();
    let combined = {
      let mut values = append_values.borrow_mut();
      let entry = values.entry(name.clone()).or_default();
      if !entry.is_empty() {
        entry.push_str(", ");
      }
      entry.push_str(&value);
      entry.clone()
    };
    if let Some(receiver) = args.first() {
      set(receiver, &name, Value::String(combined.into()));
    }
    Ok(Value::Undefined)
  });
  set(&headers, "append", host_function(vm, "append", 2, append));

  let delete_values = Rc::clone(&values);
  let delete: BuiltinFn = Rc::new(move |vm, args| {
    let Some(name) = args.get(1) else {
      return Ok(Value::Undefined);
    };
    let name = vm
      .to_string(name)?
      .as_str_lossy()
      .to_ascii_lowercase();
    delete_values.borrow_mut().remove(&name);
    if let Some(Value::Object(receiver)) = args.first() {
      receiver
        .borrow_mut()
        .properties
        .shift_remove(&PropertyKey::String(name.into()));
    }
    Ok(Value::Undefined)
  });
  set(&headers, "delete", host_function(vm, "delete", 1, delete));

  let for_each_values = values;
  let for_each: BuiltinFn = Rc::new(move |vm, args| {
    let callback = args
      .get(1)
      .ok_or_else(|| RuntimeError::from("Headers.forEach requires a callback"))?;
    if !callback.is_callable() {
      return Err(RuntimeError::from(
        "Headers.forEach callback must be callable",
      ));
    }
    let receiver = args.first().cloned().unwrap_or(Value::Undefined);
    for (name, value) in for_each_values.borrow().clone() {
      vm.call_value_with_this(
        callback,
        &Value::Undefined,
        &[
          Value::String(value.into()),
          Value::String(name.into()),
          receiver.clone(),
        ],
      )?;
    }
    Ok(Value::Undefined)
  });
  set(
    &headers,
    "forEach",
    host_function(vm, "forEach", 1, for_each),
  );
  headers
}

fn handler_info_object(
  vm: &Vm,
  remote_addr: SocketAddr,
) -> Value {
  let info = object(vm.realm().object_prototype());
  let address = object(vm.realm().object_prototype());
  set(
    &address,
    "hostname",
    Value::String(remote_addr.ip().to_string().into()),
  );
  set(
    &address,
    "port",
    Value::Number(f64::from(remote_addr.port())),
  );
  set(
    &address,
    "transport",
    Value::String("tcp".to_string().into()),
  );
  set(&info, "requestId", Value::String(String::new().into()));
  set(&info, "remoteAddr", address.clone());
  set(&info, "remoteAddress", address);
  info
}

enum HandlerResponse {
  Http(HttpResponse),
  WebSocket,
}

fn response_from_value(
  vm: &mut Vm,
  value: &Value,
) -> Result<HandlerResponse, RuntimeError> {
  match value {
    Value::Object(_) => {
      let marker = vm.get_property_host(
        value,
        &PropertyKey::String("__blitz_response".to_string().into()),
      )?;
      if !matches!(marker, Value::Bool(true)) {
        return Err(RuntimeError::from("handler must return a Response"));
      }
      let upgrade = vm.get_property_host(
        value,
        &PropertyKey::String("__blitz_upgrade_response".to_string().into()),
      )?;
      if matches!(upgrade, Value::Bool(true)) {
        return Ok(HandlerResponse::WebSocket);
      }
      let status =
        vm.get_property_host(value, &PropertyKey::String("status".to_string().into()))?;
      let status = match status {
        Value::Number(status)
          if status.is_finite() && status.fract() == 0.0 && (100.0..=599.0).contains(&status) =>
        {
          status.to_string().parse::<u16>().unwrap_or(200)
        }
        _ => 200,
      };
      let body = vm.get_property_host(value, &PropertyKey::String("body".to_string().into()))?;
      let body = if matches!(body, Value::Undefined) {
        String::new()
      } else {
        vm.to_string(&body)?.to_string_lossy()
      };
      let headers =
        vm.get_property_host(value, &PropertyKey::String("headers".to_string().into()))?;
      let mut response = HttpResponse::text(status, body);
      let supplied_headers = extract_header_entries(vm, &headers)?;
      if !supplied_headers.is_empty() {
        response.headers = supplied_headers;
      }
      Ok(HandlerResponse::Http(response))
    }
    _ => Err(RuntimeError::from("handler must return a Response")),
  }
}

fn install_websocket_constructor(
  vm: &mut Vm,
  state: ServerState,
  prototype: Value,
) {
  let instance_prototype = prototype.clone();
  let constructor_state = state;
  let constructor: BuiltinFn = Rc::new(move |vm, args| {
    let url = args
      .get(1)
      .ok_or_else(|| RuntimeError::from("WebSocket requires a URL"))?;
    let url = vm.to_string(url)?.to_string_lossy();
    if !url.starts_with("ws://") {
      return Err(RuntimeError::from(
        "WebSocket currently supports ws:// URLs only",
      ));
    }
    let (connection_id, events) = {
      let mut inner = constructor_state.0.borrow_mut();
      let connection_id = inner.next_client_id;
      inner.next_client_id = inner
        .next_client_id
        .checked_add(1)
        .ok_or_else(|| RuntimeError::from("WebSocket connection ID overflow"))?;
      (connection_id, inner.websocket_events.clone())
    };
    let socket = websocket_object(
      vm,
      constructor_state.clone(),
      connection_id,
      &url,
      instance_prototype.clone(),
    );
    let object_root = vm.create_host_root(socket.clone());
    let (shutdown, shutdown_rx) = watch::channel(false);
    constructor_state
      .0
      .borrow_mut()
      .websockets
      .insert(
        connection_id,
        WebSocketResource {
          object: socket.clone(),
          object_root,
          state: WebSocketState::default(),
          commands: None,
          shutdown: Some(shutdown),
          listeners: BTreeMap::new(),
        },
      );
    vm.spawn_bg(connect_client(
      connection_id,
      url,
      events,
      WebSocketServerOptions::default(),
      shutdown_rx,
    ));
    Ok(socket)
  });
  let value = host_constructor(vm, "WebSocket", 1, prototype, constructor);
  let Value::BuiltinObject(_, props) = &value else {
    unreachable!("host constructors are builtin objects");
  };
  for (name, value) in [
    ("CONNECTING", 0.0),
    ("OPEN", 1.0),
    ("CLOSING", 2.0),
    ("CLOSED", 3.0),
  ] {
    props.borrow_mut().properties.insert(
      PropertyKey::String(name.to_string().into()),
      PropertyDescriptor::ordinary_data(Value::Number(value)),
    );
  }
  vm.realm().set_global("WebSocket", value);
}

fn install_response_constructor(vm: &mut Vm) {
  let object_prototype = vm.realm().object_prototype();
  let prototype = object(object_prototype);
  let instance_prototype = prototype.clone();
  let constructor: BuiltinFn = Rc::new(move |vm, args| {
    let actual = args.get(1..).unwrap_or_default();
    let response = object(instance_prototype.clone());
    let body = match actual.first() {
      None | Some(Value::Null | Value::Undefined) => String::new(),
      Some(value) => vm.to_string(value)?.to_string_lossy(),
    };
    let status = if let Some(init @ Value::Object(_)) = actual.get(1) {
      match vm.get_property_host(init, &PropertyKey::String("status".to_string().into()))? {
        Value::Number(status) => status,
        _ => 200.0,
      }
    } else {
      200.0
    };
    if !status.is_finite() || status.fract() != 0.0 || !(200.0..=599.0).contains(&status) {
      return Err(RuntimeError::from(
        "Response status must be between 200 and 599",
      ));
    }
    let (status_text, header_entries) = if let Some(init @ Value::Object(_)) = actual.get(1) {
      let status_text =
        vm.get_property_host(init, &PropertyKey::String("statusText".to_string().into()))?;
      let status_text = if matches!(status_text, Value::Undefined) {
        String::new()
      } else {
        vm.to_string(&status_text)?.to_string_lossy()
      };
      let headers =
        vm.get_property_host(init, &PropertyKey::String("headers".to_string().into()))?;
      (status_text, extract_header_entries(vm, &headers)?)
    } else {
      (String::new(), Vec::new())
    };
    let headers = headers_object(vm, &header_entries);
    set(&response, "__blitz_response", Value::Bool(true));
    set(&response, "body", Value::String(body.clone().into()));
    set(&response, "status", Value::Number(status));
    set(&response, "statusText", Value::String(status_text.into()));
    set(&response, "headers", headers);
    set(
      &response,
      "ok",
      Value::Bool((200.0..=299.0).contains(&status)),
    );
    set(&response, "redirected", Value::Bool(false));
    set(
      &response,
      "type",
      Value::String("default".to_string().into()),
    );
    set(&response, "url", Value::String(String::new().into()));
    set(&response, "bodyUsed", Value::Bool(false));
    let text: BuiltinFn =
      Rc::new(move |_vm, _args| Ok(resolved_promise(Value::String(body.clone().into()))));
    set(&response, "text", host_function(vm, "text", 0, text));
    let json_body =
      match vm.get_property_host(&response, &PropertyKey::String("body".to_string().into()))? {
        Value::String(body) => body,
        _ => String::new().into(),
      };
    let json: BuiltinFn = Rc::new(move |vm, _args| Ok(json_promise(vm, json_body.as_str_lossy())));
    set(&response, "json", host_function(vm, "json", 0, json));
    Ok(response)
  });
  let constructor = host_constructor(vm, "Response", 0, prototype, constructor);
  vm.realm().set_global("Response", constructor);
}

fn install_headers_constructor(vm: &mut Vm) {
  let prototype = object(vm.realm().object_prototype());
  let constructor: BuiltinFn = Rc::new(move |vm, args| {
    let entries = match args.get(1) {
      None | Some(Value::Undefined | Value::Null) => Vec::new(),
      Some(init) => extract_header_entries(vm, init)?,
    };
    Ok(headers_object(vm, &entries))
  });
  let constructor = host_constructor(vm, "Headers", 0, prototype, constructor);
  vm.realm().set_global("Headers", constructor);
}

fn extract_header_entries(
  vm: &mut Vm,
  value: &Value,
) -> Result<Vec<(String, String)>, RuntimeError> {
  let Value::Object(properties) = value else {
    if matches!(value, Value::Undefined | Value::Null) {
      return Ok(Vec::new());
    }
    return Err(RuntimeError::from("headers must be an object"));
  };
  #[allow(clippy::needless_collect)]
  let entries = properties
    .borrow()
    .properties
    .iter()
    .filter_map(|(key, descriptor)| {
      let PropertyKey::String(name) = key else {
        return None;
      };
      let PropertyDescriptor::Data { value, .. } = descriptor else {
        return None;
      };
      if value.is_callable() || name.as_str_lossy().starts_with("__blitz_") {
        return None;
      }
      Some((name.as_str_lossy().to_ascii_lowercase(), value.clone()))
    })
    .collect::<Vec<_>>();
  entries
    .into_iter()
    .map(|(name, value)| {
      vm.to_string(&value)
        .map(|value| (name, value.to_string_lossy()))
    })
    .collect()
}

fn resolved_promise(value: Value) -> Value {
  Value::Promise(
    gc::track_promise(Rc::new(RefCell::new(PromiseState::Fulfilled(value)))),
    gc::track_object(Rc::new(RefCell::new(ObjectData::new()))),
  )
}

fn json_promise(
  vm: &mut Vm,
  body: &str,
) -> Value {
  match serde_json::from_str::<serde_json::Value>(body) {
    Ok(value) => resolved_promise(json_to_value(vm, value)),
    Err(error) => {
      let reason = Value::String(error.to_string().into());
      Value::Promise(
        gc::track_promise(Rc::new(RefCell::new(PromiseState::Rejected {
          reason,
          handled: true,
        }))),
        gc::track_object(Rc::new(RefCell::new(ObjectData::new()))),
      )
    }
  }
}

fn json_to_value(
  vm: &Vm,
  value: serde_json::Value,
) -> Value {
  match value {
    serde_json::Value::Null => Value::Null,
    serde_json::Value::Bool(value) => Value::Bool(value),
    serde_json::Value::Number(value) => Value::Number(value.as_f64().unwrap_or(f64::NAN)),
    serde_json::Value::String(value) => Value::String(value.into()),
    serde_json::Value::Array(values) => {
      let elements = values
        .into_iter()
        .map(|value| Some(json_to_value(vm, value)))
        .collect();
      Value::Array(
        gc::track_array(Rc::new(RefCell::new(ArrayData::new(elements)))),
        gc::track_object(Rc::new(RefCell::new(ObjectData::with_prototype(
          vm.realm().array_prototype(),
        )))),
      )
    }
    serde_json::Value::Object(values) => {
      let object = object(vm.realm().object_prototype());
      for (name, value) in values {
        set(&object, &name, json_to_value(vm, value));
      }
      object
    }
  }
}

fn object(prototype: Value) -> Value {
  Value::Object(gc::track_object(Rc::new(RefCell::new(
    ObjectData::with_prototype(prototype),
  ))))
}

fn set(
  target: &Value,
  name: &str,
  value: Value,
) {
  let Value::Object(properties) = target else {
    return;
  };
  properties.borrow_mut().properties.insert(
    PropertyKey::String(name.to_string().into()),
    PropertyDescriptor::ordinary_data(value),
  );
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn websocket_state_tracks_lifecycle_and_buffered_bytes() {
    let mut state = WebSocketState::default();
    assert_eq!(state.ready_state(), WebSocketReadyState::Connecting);
    assert!(!state.queue_bytes(4));
    assert!(state.open());
    assert!(state.queue_bytes(7));
    state.drain_bytes(2);
    assert_eq!(state.buffered_amount(), 5);
    state.begin_close();
    assert_eq!(state.ready_state(), WebSocketReadyState::Closing);
    assert!(!state.queue_bytes(1));
    state.finish_close();
    assert_eq!(state.ready_state(), WebSocketReadyState::Closed);
    assert_eq!(state.buffered_amount(), 0);
  }

  #[test]
  fn websocket_close_codes_exclude_reserved_values() {
    for code in [1000, 1001, 1002, 1003, 1007, 1009, 1011, 3000, 4999] {
      assert!(is_valid_websocket_close_code(code), "{code}");
    }
    for code in [999, 1004, 1005, 1006, 1015, 2999, 5000] {
      assert!(!is_valid_websocket_close_code(code), "{code}");
    }
  }
}
