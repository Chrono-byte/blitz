// SPDX-License-Identifier: MPL-2.0
//! Buffered terminal input and prompt-safe output for interactive applications.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::thread;

use blitz::errors::RuntimeError;
use blitz::gc;
use blitz::value::{BuiltinFn, ObjectData, PropertyDescriptor, PropertyKey, Value};
use blitz::{HostRoot, Vm};
use rustyline::{DefaultEditor, ExternalPrinter};
use tokio::sync::mpsc;

/// Owned input emitted by the dedicated line-editor thread.
pub(super) enum TerminalEvent {
  Line(String),
  Eof,
  Error(String),
}

struct TerminalInner {
  line_callback: HostRoot,
  close_callback: HostRoot,
  started: bool,
  active: bool,
  prompt: Arc<Mutex<String>>,
  printer: Arc<Mutex<Option<Box<dyn ExternalPrinter + Send>>>>,
  events: mpsc::Sender<TerminalEvent>,
}

/// VM-thread terminal callback and line-editor state.
#[derive(Clone)]
pub(super) struct TerminalState(Rc<RefCell<TerminalInner>>);

impl TerminalState {
  pub(super) fn is_active(&self) -> bool {
    self.0.borrow().active
  }

  pub(super) fn subscribe_lines(
    &self,
    vm: &mut Vm,
    callback: Value,
  ) -> Result<(), RuntimeError> {
    if !callback.is_callable() {
      return Err(RuntimeError::from(
        "terminal line callback must be callable",
      ));
    }
    vm.set_host_root(self.0.borrow().line_callback, callback)?;
    start_input(self);
    Ok(())
  }

  pub(super) fn subscribe_close(
    &self,
    vm: &mut Vm,
    callback: Value,
  ) -> Result<(), RuntimeError> {
    if !callback.is_callable() {
      return Err(RuntimeError::from(
        "terminal close callback must be callable",
      ));
    }
    vm.set_host_root(self.0.borrow().close_callback, callback)?;
    Ok(())
  }

  pub(super) fn clear_line_subscription(
    &self,
    vm: &mut Vm,
  ) -> Result<(), RuntimeError> {
    let inner = self.0.borrow();
    vm.set_host_root(inner.line_callback, Value::Undefined)?;
    vm.set_host_root(inner.close_callback, Value::Undefined)?;
    Ok(())
  }

  pub(super) fn set_prompt(
    &self,
    prompt: String,
  ) {
    if let Ok(mut current) = self.0.borrow().prompt.lock() {
      *current = prompt;
    }
  }

  pub(super) fn close_and_notify(
    &self,
    vm: &mut Vm,
  ) -> Result<(), RuntimeError> {
    let callback = vm.host_root({
      let mut inner = self.0.borrow_mut();
      inner.active = false;
      inner.close_callback
    });
    if let Some(callback) = callback.filter(Value::is_callable) {
      vm.call_value_with_this(&callback, &Value::Undefined, &[])?;
    }
    Ok(())
  }

  pub(super) fn write(
    &self,
    message: String,
  ) {
    print_external(self, message);
  }

  pub(super) fn dispatch(
    &self,
    vm: &mut Vm,
    event: TerminalEvent,
  ) -> Result<(), RuntimeError> {
    match event {
      TerminalEvent::Line(line) => {
        let callback = vm.host_root(self.0.borrow().line_callback);
        if let Some(callback) = callback.filter(Value::is_callable)
          && let Err(error) =
            vm.call_value_with_this(&callback, &Value::Undefined, &[Value::String(line)])
        {
          print_external(self, format!("Input handler error: {error}"));
        }
      }
      TerminalEvent::Eof => {
        let callback = vm.host_root({
          let mut inner = self.0.borrow_mut();
          inner.active = false;
          inner.close_callback
        });
        if let Some(callback) = callback.filter(Value::is_callable)
          && let Err(error) = vm.call_value_with_this(&callback, &Value::Undefined, &[])
        {
          print_external(self, format!("Input close handler error: {error}"));
        }
      }
      TerminalEvent::Error(message) => {
        self.0.borrow_mut().active = false;
        return Err(RuntimeError::from(format!(
          "terminal input failed: {message}"
        )));
      }
    }
    Ok(())
  }
}

pub(super) fn install(
  vm: &mut Vm,
  events: mpsc::Sender<TerminalEvent>,
) -> TerminalState {
  let line_callback = vm.create_host_root(Value::Undefined);
  let close_callback = vm.create_host_root(Value::Undefined);
  let state = TerminalState(Rc::new(RefCell::new(TerminalInner {
    line_callback,
    close_callback,
    started: false,
    active: false,
    prompt: Arc::new(Mutex::new("> ".to_string())),
    printer: Arc::new(Mutex::new(None)),
    events,
  })));
  let stdin = object(vm.realm().object_prototype());
  let stdout = object(vm.realm().object_prototype());

  let line_state = state.clone();
  let on_line: BuiltinFn = Rc::new(move |vm, args| {
    let callback = args
      .get(1)
      .cloned()
      .ok_or_else(|| RuntimeError::from("Blitz.stdin.onLine requires a callback"))?;
    if !callback.is_callable() {
      return Err(RuntimeError::from(
        "Blitz.stdin.onLine callback must be callable",
      ));
    }
    vm.set_host_root(line_state.0.borrow().line_callback, callback)?;
    start_input(&line_state);
    Ok(Value::Undefined)
  });
  set(&stdin, "onLine", Value::Builtin(on_line));

  let close_state = state.clone();
  let on_close: BuiltinFn = Rc::new(move |vm, args| {
    let callback = args
      .get(1)
      .cloned()
      .ok_or_else(|| RuntimeError::from("Blitz.stdin.onClose requires a callback"))?;
    if !callback.is_callable() {
      return Err(RuntimeError::from(
        "Blitz.stdin.onClose callback must be callable",
      ));
    }
    vm.set_host_root(close_state.0.borrow().close_callback, callback)?;
    Ok(Value::Undefined)
  });
  set(&stdin, "onClose", Value::Builtin(on_close));

  let prompt_state = state.clone();
  let set_prompt: BuiltinFn = Rc::new(move |vm, args| {
    let prompt = args
      .get(1)
      .map_or_else(|| Ok(String::new()), |value| vm.to_string(value))?;
    if let Ok(mut current) = prompt_state.0.borrow().prompt.lock() {
      *current = prompt;
    }
    Ok(Value::Undefined)
  });
  set(&stdin, "setPrompt", Value::Builtin(set_prompt));

  let stop_state = state.clone();
  let close: BuiltinFn = Rc::new(move |_vm, _args| {
    stop_state.0.borrow_mut().active = false;
    Ok(Value::Undefined)
  });
  set(&stdin, "close", Value::Builtin(close));

  let output_state = state.clone();
  let write_line: BuiltinFn = Rc::new(move |vm, args| {
    let message = args
      .get(1)
      .map_or_else(|| Ok(String::new()), |value| vm.to_string(value))?;
    print_external(&output_state, message);
    Ok(Value::Undefined)
  });
  set(&stdout, "write", Value::Builtin(Rc::clone(&write_line)));
  set(&stdout, "writeLine", Value::Builtin(write_line));

  if let Some(blitz) = vm.realm().get_global("Blitz") {
    set(&blitz, "stdin", stdin);
    set(&blitz, "stdout", stdout);
  }
  state
}

fn start_input(state: &TerminalState) {
  let (events, prompt, printer) = {
    let mut inner = state.0.borrow_mut();
    if inner.started {
      return;
    }
    inner.started = true;
    inner.active = true;
    (
      inner.events.clone(),
      Arc::clone(&inner.prompt),
      Arc::clone(&inner.printer),
    )
  };
  drop(thread::spawn(move || {
    let mut editor = match DefaultEditor::new() {
      Ok(editor) => editor,
      Err(error) => {
        let _result = events.blocking_send(TerminalEvent::Error(error.to_string()));
        return;
      }
    };
    if let Ok(external_printer) = editor.create_external_printer()
      && let Ok(mut slot) = printer.lock()
    {
      *slot = Some(Box::new(external_printer));
    }
    loop {
      let current_prompt = prompt
        .lock()
        .map_or_else(|_| "> ".to_string(), |value| value.clone());
      match editor.readline(&current_prompt) {
        Ok(line) => {
          if !line.is_empty() {
            let _result = editor.add_history_entry(line.as_str());
          }
          if events
            .blocking_send(TerminalEvent::Line(line))
            .is_err()
          {
            break;
          }
        }
        Err(rustyline::error::ReadlineError::Interrupted) => continue,
        Err(rustyline::error::ReadlineError::Eof) => {
          let _result = events.blocking_send(TerminalEvent::Eof);
          break;
        }
        Err(error) => {
          let _result = events.blocking_send(TerminalEvent::Error(error.to_string()));
          break;
        }
      }
    }
  }));
}

fn print_external(
  state: &TerminalState,
  message: String,
) {
  let printer = Arc::clone(&state.0.borrow().printer);
  if let Ok(mut printer) = printer.lock()
    && let Some(printer) = printer.as_mut()
    && printer.print(message.clone()).is_ok()
  {
    return;
  }
  println!("{message}");
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
    PropertyKey::String(name.to_string()),
    PropertyDescriptor::ordinary_data(value),
  );
}

#[cfg(test)]
mod tests {
  use super::*;
  use blitz::compiler;
  use blitz::parser::Parser as JsParser;

  #[test]
  fn installs_buffered_terminal_api() {
    let globals = Rc::new(RefCell::new(ObjectData::new()));
    let mut vm = Vm::new(Rc::clone(&globals));
    let blitz = object(vm.realm().object_prototype());
    vm.realm().set_global("Blitz", blitz);
    let (events, _receiver) = mpsc::channel(1);
    let state = install(&mut vm, events);
    assert!(!state.is_active());
    let blitz = vm.get_global("Blitz").expect("Blitz global");
    for (container, methods) in [
      ("stdin", &["onLine", "onClose", "setPrompt", "close"][..]),
      ("stdout", &["write", "writeLine"][..]),
    ] {
      let object = vm
        .get_property_host(&blitz, &PropertyKey::String(container.to_string()))
        .unwrap_or_else(|_| panic!("read Blitz.{container}"));
      for method in methods {
        let value = vm
          .get_property_host(&object, &PropertyKey::String((*method).to_string()))
          .unwrap_or_else(|_| panic!("read Blitz.{container}.{method}"));
        assert!(value.is_callable());
      }
    }
  }

  #[test]
  fn dispatches_line_to_persistent_handler_with_captured_bindings() {
    let globals = Rc::new(RefCell::new(ObjectData::new()));
    globals.borrow_mut().properties.insert(
      PropertyKey::String("globalThis".to_string()),
      PropertyDescriptor::ordinary_data(Value::Object(Rc::clone(&globals))),
    );
    let mut vm = Vm::new(globals);
    let blitz = object(vm.realm().object_prototype());
    vm.realm().set_global("Blitz", blitz);
    let (events, _receiver) = mpsc::channel(1);
    let state = install(&mut vm, events);
    let source = blitz::js!(
      r#"
      globalThis.sentLine = "";
      const prefix = "MSG ";
      const chatSocket = {
        OPEN: 1,
        readyState: 1,
        send: function(value) { globalThis.sentLine = value; }
      };
      globalThis.handleInput = (line) => {
        chatSocket.send(prefix + line.trim());
      };
    "#
    );
    let session = JsParser::parse_source(source).expect("parse terminal callback");
    let chunk =
      compiler::compile_oxc(session.program(), source).expect("compile terminal callback");
    vm.run(chunk).expect("define terminal callback");
    let callback = vm
      .get_global("handleInput")
      .expect("terminal callback");
    vm.set_host_root(state.0.borrow().line_callback, callback)
      .expect("retain terminal callback");
    state
      .dispatch(&mut vm, TerminalEvent::Line("  meow  ".to_string()))
      .expect("dispatch terminal line");
    assert_eq!(
      vm.get_global("sentLine"),
      Some(Value::String("MSG meow".to_string()))
    );
  }

  #[test]
  fn dispatches_irc_line_handler_after_sibling_socket_callbacks() {
    let globals = Rc::new(RefCell::new(ObjectData::new()));
    globals.borrow_mut().properties.insert(
      PropertyKey::String("globalThis".to_string()),
      PropertyDescriptor::ordinary_data(Value::Object(Rc::clone(&globals))),
    );
    let mut vm = Vm::new(Rc::clone(&globals));
    let blitz = object(vm.realm().object_prototype());
    vm.realm().set_global("Blitz", blitz);
    let (events, _receiver) = mpsc::channel(1);
    let state = install(&mut vm, events);
    let source = blitz::js!(
      r#"
      globalThis.WebSocket = { OPEN: 1 };
      globalThis.sentLine = "";
      const socket = {
        readyState: 1,
        send: function(value) { globalThis.sentLine = value; }
      };

      socket.onopen = () => socket.send("JOIN Bob");
      socket.onmessage = (event) => String(event.data);
      socket.onerror = (event) => String(event.message);
      socket.onclose = () => undefined;

      globalThis.handleInput = (line) => {
        const message = line.trim();
        if (message === "/quit") {
          if (socket.readyState === WebSocket.OPEN) {
            socket.send("QUIT");
          }
          return;
        }
        if (message.length === 0) {
          return;
        }
        if (socket.readyState !== WebSocket.OPEN) {
          return;
        }
        socket.send(`MSG ${message}`);
      };

      globalThis.handleClose = () => {
        if (socket.readyState === WebSocket.OPEN) {
          socket.close(1000, "stdin closed");
        }
      };
    "#
    );
    let session = JsParser::parse_source(source).expect("parse IRC terminal callback");
    let chunk =
      compiler::compile_oxc(session.program(), source).expect("compile IRC terminal callback");
    vm.run(chunk)
      .expect("define IRC terminal callback");
    let callback = vm
      .get_global("handleInput")
      .expect("IRC terminal callback");
    vm.set_host_root(state.0.borrow().line_callback, callback)
      .expect("retain IRC terminal callback");
    globals
      .borrow_mut()
      .properties
      .shift_remove(&PropertyKey::String("handleInput".to_string()));
    vm.collect_garbage_prototype();

    state
      .dispatch(&mut vm, TerminalEvent::Line("hello".to_string()))
      .expect("dispatch IRC terminal line");

    assert_eq!(
      vm.get_global("sentLine"),
      Some(Value::String("MSG hello".to_string()))
    );
  }
}
