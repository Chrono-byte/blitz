// SPDX-License-Identifier: MPL-2.0
//! Async/microtask helpers and background runtime utilities.
//!
//! This module provides utilities the VM uses to schedule and run
//! microtasks, interact with the background Tokio runtime, and bridge
//! synchronous VM execution with asynchronous operations (promises,
//! async functions, etc.).
//!
//! Microtask / job queue semantics
//! - Microtasks are stored as closures and drained in FIFO order. Job errors
//!   are recorded on the VM, reported through the unhandled-rejection hook when
//!   installed, and do not stop the remaining queue from being drained.
//! - Promise reactions and thenable-resolution work are queued as microtasks,
//!   preserving their observable job ordering.
use crate::bytecode::FunctionProto;
use crate::runtime_utils::{reject_pending_promise, resolve_promise_with_value};
use crate::value::{PromiseState, SharedValue, Value};
use crate::vm::state::*;
use std::cell::RefCell;
use std::future::Future;
use std::rc::Rc;
use std::sync::Arc;

impl Vm {
  /// Queue a microtask to be executed after the current call stack completes.
  pub(crate) fn queue_microtask<F>(
    &mut self,
    job: F,
  ) where
    F: FnMut(&mut Vm) -> Result<(), crate::errors::RuntimeError> + 'static,
  {
    self.jobs.microtasks.push_back(Box::new(job));
  }

  /// Queue a dynamic import job to be executed by the host job loop.
  pub(crate) fn queue_dynamic_import_job<F>(
    &mut self,
    job: F,
  ) where
    F: FnMut(&mut Vm) -> Result<(), crate::errors::RuntimeError> + 'static,
  {
    self.jobs.dynamic_imports.push_back(Box::new(job));
  }

  /// Install a host hook which will be invoked for unhandled promise
  /// rejections and microtask exceptions. The hook receives (vm, promise, reason)
  /// where `promise` is the rejected promise object or `Undefined` for
  /// microtask exceptions that aren't tied to a specific promise.
  pub fn set_unhandled_rejection_hook<F>(
    &mut self,
    hook: F,
  ) where
    F: FnMut(&mut Vm, Value, Value) + 'static,
  {
    self.jobs.unhandled_rejection_hook = Some(Rc::new(RefCell::new(hook)));
  }

  /// Create a pending promise owned by a host integration.
  ///
  /// Hosts should settle the returned value with [`Vm::resolve_host_promise`]
  /// on the VM thread. The promise uses the same reaction and microtask
  /// machinery as promises created by JavaScript.
  #[must_use]
  pub fn create_host_promise(&self) -> Value {
    Value::Promise(crate::gc::track_promise(Rc::new(RefCell::new(
      PromiseState::Pending {
        on_fulfilled: Vec::new(),
        on_rejected: Vec::new(),
      },
    ))))
  }

  /// Resolve a pending promise previously created for a host integration.
  ///
  /// # Errors
  ///
  /// Returns an error when `promise` is not a Promise or when resolving it
  /// invokes failing promise machinery.
  pub fn resolve_host_promise(
    &mut self,
    promise: &Value,
    value: Value,
  ) -> Result<(), crate::errors::RuntimeError> {
    let Value::Promise(promise) = promise else {
      return Err(crate::errors::RuntimeError::from(
        "host promise settlement requires a Promise",
      ));
    };
    resolve_promise_with_value(self, promise, value)
  }

  /// Invoke the installed unhandled rejection hook, if present.
  pub(crate) fn call_unhandled_rejection(
    &mut self,
    promise: Value,
    reason: Value,
  ) {
    if let Some(hook_rc) = self
      .jobs
      .unhandled_rejection_hook
      .as_ref()
      .map(Rc::clone)
    {
      let mut hook = hook_rc.borrow_mut();
      (hook)(self, promise, reason);
    }
  }

  /// Drain all pending dynamic import jobs in FIFO order.
  ///
  /// Job failures are recorded on the VM and reported through the host hook;
  /// they do not stop the remaining jobs from being drained.
  pub(crate) fn drain_dynamic_import_jobs(&mut self) -> Result<(), crate::errors::RuntimeError> {
    while let Some(mut job) = self.jobs.dynamic_imports.pop_front() {
      let caller_handlers = std::mem::take(&mut self.exceptions.handlers);
      let result = job(self);
      self.exceptions.handlers = caller_handlers;
      self.maybe_collect_garbage_prototype();
      if let Err(err) = result {
        let exc_val = Value::String(format!("{}", err).into());
        self.exceptions.last_uncaught = Some(exc_val.clone());
        if let Some(hook_rc) = self
          .jobs
          .unhandled_rejection_hook
          .as_ref()
          .map(Rc::clone)
        {
          let mut hook = hook_rc.borrow_mut();
          (hook)(self, Value::Undefined, exc_val.clone());
        }
        continue;
      }
    }
    Ok(())
  }

  /// Drain all pending microtasks, executing them in FIFO order and continuing on errors.
  ///
  /// Microtask callbacks are executed to completion. If a microtask returns
  /// an error, the VM records it on `last_uncaught_exception` and continues
  /// draining the remaining microtasks. This mirrors the common host policy
  /// of running microtasks to completion while surfacing exceptions via a
  /// host hook instead of aborting the queue on first error.
  pub(crate) fn drain_microtasks(&mut self) -> Result<(), crate::errors::RuntimeError> {
    while let Some(mut job) = self.jobs.microtasks.pop_front() {
      let caller_handlers = std::mem::take(&mut self.exceptions.handlers);
      let result = job(self);
      self.exceptions.handlers = caller_handlers;
      self.maybe_collect_garbage_prototype();
      if let Err(err) = result {
        // Record the last uncaught exception so callers or host hooks
        // can inspect it. Convert the runtime error into a JS value
        // string for now; hosts may implement richer reporting.
        let exc_val = Value::String(format!("{}", err).into());
        self.exceptions.last_uncaught = Some(exc_val.clone());
        // If a host hook is installed, call it with Undefined promise
        // to indicate this was a microtask exception.
        if let Some(hook_rc) = self
          .jobs
          .unhandled_rejection_hook
          .as_ref()
          .map(Rc::clone)
        {
          let mut hook = hook_rc.borrow_mut();
          (hook)(self, Value::Undefined, exc_val.clone());
        }
        // Continue draining remaining microtasks rather than returning.
        continue;
      }
    }
    Ok(())
  }

  /// Run all pending microtasks and dynamic import jobs until the queues are empty.
  ///
  /// Prioritizes microtasks over dynamic import jobs, processing them in rounds.
  ///
  /// Job failures are recorded on the VM and reported through the host hook;
  /// they do not stop the remaining jobs from being drained.
  pub(crate) fn run_microtasks_if_needed(&mut self) -> Result<(), crate::errors::RuntimeError> {
    while !self.jobs.microtasks.is_empty() || !self.jobs.dynamic_imports.is_empty() {
      if !self.jobs.microtasks.is_empty() {
        self.drain_microtasks()?;
        continue;
      }
      self.drain_dynamic_import_jobs()?;
    }
    Ok(())
  }

  /// Resolve a value through the runtime's promise machinery and wait for it
  /// to settle using the existing microtask queue.
  pub fn await_value(
    &mut self,
    value: Value,
  ) -> Result<Value, crate::errors::RuntimeError> {
    if let Value::Promise(promise) = &value
      && let PromiseState::Rejected { handled, .. } = &mut *promise.borrow_mut()
    {
      *handled = true;
    }
    let promise = crate::gc::track_promise(Rc::new(RefCell::new(PromiseState::Pending {
      on_fulfilled: Vec::new(),
      on_rejected: Vec::new(),
    })));
    resolve_promise_with_value(self, &promise, value)?;
    loop {
      let settled_state = {
        let state_ref = promise.borrow();
        match &*state_ref {
          PromiseState::Fulfilled(v) => Some(Ok(v.clone())),
          PromiseState::Rejected { reason, .. } => Some(Err(reason.clone())),
          PromiseState::Pending { .. } => None,
        }
      };

      match settled_state {
        Some(Ok(value)) => return Ok(value),
        Some(Err(reason)) => {
          self.exceptions.last_uncaught = Some(reason);
          return Err(crate::errors::RuntimeError::from("Promise rejected"));
        }
        None => {
          if !self.jobs.microtasks.is_empty() || !self.jobs.dynamic_imports.is_empty() {
            self.run_microtasks_if_needed()?;
            self.maybe_collect_garbage_prototype();
            continue;
          }
          return Err(crate::errors::RuntimeError::from(
            "Promise did not settle during synchronous await",
          ));
        }
      }
    }
  }

  /// Await a value from bytecode semantics and turn promise rejection into a
  /// language-level throw that can be handled by the active exception region.
  pub(super) fn await_value_or_throw(
    &mut self,
    value: Value,
  ) -> Result<Value, crate::errors::RuntimeError> {
    match self.await_value(value) {
      Ok(value) => Ok(value),
      Err(error) => {
        let Some(reason) = self.exceptions.last_uncaught.take() else {
          return Err(error);
        };
        self.throw_value(reason)?;
        Err(crate::errors::RuntimeError::from("JS exception"))
      }
    }
  }

  /// Drain any pending microtasks, ensuring async bootstrap work completes.
  ///
  /// Microtask failures are recorded on the VM and reported through the host
  /// hook while the remaining microtasks continue to execute.
  pub fn flush_microtasks(&mut self) -> Result<(), crate::errors::RuntimeError> {
    self.run_microtasks_if_needed()
  }

  /// Invoke an async closure, queuing its microtasks for execution after the current call stack completes.
  ///
  /// Execution failures reject the returned promise. Failures from queued
  /// microtasks are recorded on the VM and reported through the host hook.
  pub(super) fn invoke_async_closure(
    &mut self,
    proto: &Rc<FunctionProto>,
    env: &Rc<RefCell<Vec<SharedValue>>>,
    call_args: Vec<Value>,
    this_arg: Value,
    callee: &Value,
  ) -> Result<Value, crate::errors::RuntimeError> {
    let promise = crate::gc::track_promise(Rc::new(RefCell::new(PromiseState::Pending {
      on_fulfilled: Vec::new(),
      on_rejected: Vec::new(),
    })));
    if crate::trace::enabled() {
      eprintln!(
        "[trace] invoke_async_closure start promise={:p} fn='{}'",
        Rc::as_ptr(&promise),
        proto.name
      );
    }

    let proto = Rc::clone(proto);
    let env = Rc::clone(env);
    let promise_for_job = Rc::clone(&promise);
    let caller_chunk = Arc::clone(&self.execution.active.chunk);
    let caller_ip = self.execution.active.ip;
    let caller_locals = self.execution.active.locals.clone();
    let caller_this = self.execution.active.this_value.clone();
    let caller_env = self.execution.active.closure_env.clone();
    let caller_lexical_env = Rc::clone(&self.execution.active.lexical_env);
    let caller_slots = self.execution.active.slots.clone();
    let caller_frame_len = self.execution.call_stack.len();
    let caller_handlers = std::mem::take(&mut self.exceptions.handlers);

    self.exceptions.last_uncaught = None;
    let exec_result = (|| {
      self.push_call_frame(&proto, &env, call_args, this_arg, callee.clone())?;
      self
        .run_risc_loop(false)
        .and_then(|value| self.execute_return(value))
    })();
    self.maybe_collect_garbage_prototype();

    self.execution.active.chunk = caller_chunk;
    self.execution.active.ip = caller_ip;
    self.execution.active.locals = caller_locals;
    self.execution.active.this_value = caller_this;
    self.execution.active.closure_env = caller_env;
    self.execution.active.lexical_env = caller_lexical_env;
    self.execution.active.slots = caller_slots;
    self
      .execution
      .call_stack
      .truncate(caller_frame_len);
    self.exceptions.handlers = caller_handlers;

    self.run_microtasks_if_needed()?;
    self.maybe_collect_garbage_prototype();
    match exec_result {
      Ok(result) => {
        if crate::trace::enabled() {
          eprintln!(
            "[trace] invoke_async_closure queue_resolve promise={:p}",
            Rc::as_ptr(&promise_for_job)
          );
        }
        let mut result_cell = Some(result);
        self.queue_microtask(move |vm| {
          if crate::trace::enabled() {
            eprintln!(
              "[trace] invoke_async_closure run_resolve promise={:p}",
              Rc::as_ptr(&promise_for_job)
            );
          }
          let value = result_cell
            .take()
            .expect("result_cell must be Some");
          if crate::trace::enabled() {
            crate::trace::value_string("[trace] invoke_async_closure resolve_value", &value);
          }
          if let Err(err) = resolve_promise_with_value(vm, &promise_for_job, value) {
            if crate::trace::enabled() {
              eprintln!("[trace] invoke_async_closure resolve_error: {}", err);
            }
            return Err(err);
          }
          Ok(())
        });
      }
      Err(err) => {
        if crate::trace::enabled() {
          eprintln!(
            "[trace] invoke_async_closure queue_reject promise={:p}",
            Rc::as_ptr(&promise_for_job)
          );
        }
        let reason = self
          .exceptions
          .last_uncaught
          .take()
          .unwrap_or_else(|| Value::String(format!("{}", err).into()));
        let mut reason_cell = Some(reason);
        self.queue_microtask(move |vm| {
          if crate::trace::enabled() {
            eprintln!(
              "[trace] invoke_async_closure run_reject promise={:p}",
              Rc::as_ptr(&promise_for_job)
            );
          }
          let reason = reason_cell
            .take()
            .expect("reason_cell must be Some");
          reject_pending_promise(vm, &promise_for_job, reason);
          Ok(())
        });
      }
    }

    Ok(Value::Promise(promise))
  }

  /// Run a blocking closure on the background runtime and wait for the result synchronously.
  ///
  /// # Errors
  ///
  /// Returns a [`crate::errors::RuntimeError`] if the background task fails.
  pub fn run_blocking<F, R>(
    &self,
    f: F,
  ) -> Result<R, crate::errors::RuntimeError>
  where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
  {
    let rt = Arc::clone(&self.jobs.background_runtime);
    let handle = rt.spawn_blocking(f);
    rt.block_on(handle)
      .map_err(|e| crate::errors::RuntimeError::from(format!("Background task failed: {}", e)))
  }

  /// Synchronously execute an async future on the background runtime.
  ///
  /// Returns the future's output after running it on the background runtime.
  pub fn block_on<Fut>(
    &self,
    fut: Fut,
  ) -> Result<Fut::Output, crate::errors::RuntimeError>
  where
    Fut: Future + Send + 'static,
    Fut::Output: Send + 'static,
  {
    Ok(Arc::clone(&self.jobs.background_runtime).block_on(fut))
  }

  /// Spawn a fire-and-forget task on the background runtime.
  pub fn spawn_bg<Fut>(
    &self,
    fut: Fut,
  ) where
    Fut: Future<Output = ()> + Send + 'static,
  {
    drop(self.jobs.background_runtime.spawn(fut));
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::errors::RuntimeError;
  use crate::value::{ObjectData, Value};
  use std::cell::RefCell;
  use std::rc::Rc;

  fn test_vm() -> Vm {
    Vm::new(Rc::new(RefCell::new(ObjectData::new())))
  }

  #[test]
  fn microtasks_continue_after_error_and_notify_hook() {
    let mut vm = test_vm();
    let events = Rc::new(RefCell::new(Vec::new()));
    let hook_calls: Rc<RefCell<Vec<(String, String)>>> = Rc::new(RefCell::new(Vec::new()));

    vm.set_unhandled_rejection_hook({
      let hook_calls = Rc::clone(&hook_calls);
      move |_vm, promise, reason| {
        hook_calls
          .borrow_mut()
          .push((promise.type_name().to_string(), format!("{}", reason)));
      }
    });

    vm.queue_microtask({
      let events = Rc::clone(&events);
      move |_vm| {
        events.borrow_mut().push("first".to_string());
        Ok(())
      }
    });
    vm.queue_microtask({
      let events = Rc::clone(&events);
      move |_vm| {
        events.borrow_mut().push("second".to_string());
        Err(RuntimeError::from("microtask failed"))
      }
    });
    vm.queue_microtask({
      let events = Rc::clone(&events);
      move |_vm| {
        events.borrow_mut().push("third".to_string());
        Ok(())
      }
    });

    vm.flush_microtasks().expect("flush");

    assert_eq!(
      events.borrow().as_slice(),
      [
        "first".to_string(),
        "second".to_string(),
        "third".to_string()
      ]
    );
    assert!(matches!(
        vm.exceptions.last_uncaught.as_ref(),
        Some(Value::String(s)) if s.as_str_lossy().contains("microtask failed")
    ));
    assert_eq!(
      hook_calls.borrow().as_slice(),
      [(
        "undefined".to_string(),
        "Runtime error: microtask failed".to_string()
      )]
    );
  }

  #[test]
  fn host_promises_use_the_runtime_promise_state_machine() {
    let mut vm = test_vm();
    let promise = vm.create_host_promise();
    assert!(matches!(
      &promise,
      Value::Promise(state) if matches!(*state.borrow(), PromiseState::Pending { .. })
    ));

    vm.resolve_host_promise(&promise, Value::Number(42.0))
      .expect("resolve host promise");
    assert!(matches!(
      &promise,
      Value::Promise(state)
        if matches!(&*state.borrow(), PromiseState::Fulfilled(Value::Number(42.0)))
    ));
  }

  #[test]
  fn run_blocking_executes_closure() {
    let vm = test_vm();
    let value = vm
      .run_blocking(|| 4usize + 5)
      .expect("run blocking");

    assert_eq!(value, 9);
  }
}
