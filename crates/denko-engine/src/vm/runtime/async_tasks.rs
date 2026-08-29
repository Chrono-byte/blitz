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
use crate::bytecode::Function;
use crate::runtime_utils::{reject_pending_promise, resolve_promise_with_value};
use crate::value::{ObjectData, PromiseState, Value};
use crate::vm::state::*;
use std::cell::RefCell;
use std::future::Future;
use std::rc::Rc;

struct AsyncExecution {
  vm: Vm,
  register_state: RegisterState,
}

struct AsyncRunner {
  execution: Option<AsyncExecution>,
  result_promise: Rc<RefCell<PromiseState>>,
}

enum AsyncResume {
  Start,
  Fulfill(Value),
  Reject(Value),
}

impl Vm {
  /// Keep a weakly referenced object alive until the current job completes.
  pub(crate) fn keep_alive(
    &mut self,
    value: Value,
  ) {
    self.memory.kept_alive.push(value);
  }

  /// Schedule cleanup for a FinalizationRegistry whose target became unreachable.
  pub(crate) fn queue_finalization_cleanup(
    &mut self,
    registry: Rc<RefCell<ObjectData>>,
  ) {
    let registry_value = Value::Object(registry);
    self.queue_microtask(move |vm| {
      let Value::Object(registry) = &registry_value else {
        return Ok(());
      };
      let (cleanup, holdings) = {
        let mut registry = registry.borrow_mut();
        registry.finalization_cleanup_scheduled = false;
        let cleanup = registry.finalization_cleanup_callback.clone();
        let Some(cells) = registry.finalization_registry_cells.as_mut() else {
          return Ok(());
        };
        let mut holdings = Vec::new();
        cells.retain(|cell| {
          if cell.active {
            true
          } else {
            holdings.push(cell.holdings.clone());
            false
          }
        });
        (cleanup, holdings)
      };
      let Some(cleanup) = cleanup else {
        return Ok(());
      };
      for holding in holdings {
        vm.call_value_with_this(&cleanup, &Value::Undefined, &[holding])?;
      }
      Ok(())
    });
  }

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
    self.promise_value(crate::gc::track_promise(Rc::new(RefCell::new(
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
    let Value::Promise(promise, _) = promise else {
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
      self.memory.kept_alive.clear();
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
      self.memory.kept_alive.clear();
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
    if let Value::Promise(promise, _) = &value
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
    proto: &Rc<Function>,
    env: &crate::closure_environment::ClosureEnvironment,
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

    let mut child = self.fork_execution();
    let register_arguments = call_args
      .iter()
      .take(crate::bytecode::abi::REGISTER_ARGUMENT_COUNT)
      .cloned()
      .collect::<Vec<_>>();
    if let Err(error) = child.push_call_frame(proto, env, call_args, this_arg, callee.clone()) {
      let reason = child
        .exceptions
        .last_uncaught
        .take()
        .unwrap_or_else(|| Value::String(error.to_string().into()));
      reject_pending_promise(self, &promise, reason);
      return Ok(self.promise_value(promise));
    }
    let register_state = RegisterState::new(
      child.execution.active.this_value.clone(),
      &register_arguments,
      child.execution.active.chunk.spill_slot_count as usize,
    );
    let runner = Rc::new(RefCell::new(AsyncRunner {
      execution: Some(AsyncExecution {
        vm: child,
        register_state,
      }),
      result_promise: Rc::clone(&promise),
    }));
    Self::resume_async_runner(self, &runner, AsyncResume::Start);
    Ok(self.promise_value(promise))
  }

  fn resume_async_runner(
    host: &mut Vm,
    runner: &Rc<RefCell<AsyncRunner>>,
    resume: AsyncResume,
  ) {
    let Some(mut execution) = runner.borrow_mut().execution.take() else {
      return;
    };

    let run_result = match resume {
      AsyncResume::Start => execution
        .vm
        .run_state(&mut execution.register_state, false),
      AsyncResume::Fulfill(value) => {
        if execution
          .register_state
          .pending_operation
          .is_some()
        {
          execution
            .register_state
            .settle_pending_operation(AwaitSettlement::Fulfilled(value));
        } else {
          execution.register_state.resume_with(value);
        }
        execution
          .vm
          .run_state(&mut execution.register_state, false)
      }
      AsyncResume::Reject(reason) => {
        if execution
          .register_state
          .pending_operation
          .is_some()
        {
          execution
            .register_state
            .settle_pending_operation(AwaitSettlement::Rejected(reason));
          execution
            .vm
            .run_state(&mut execution.register_state, false)
        } else {
          match execution.vm.throw_value(reason) {
            Ok(()) => execution
              .vm
              .run_state(&mut execution.register_state, false),
            Err(error) => Err(error),
          }
        }
      }
    };

    host
      .jobs
      .microtasks
      .append(&mut execution.vm.jobs.microtasks);
    host
      .jobs
      .dynamic_imports
      .append(&mut execution.vm.jobs.dynamic_imports);

    match run_result {
      Ok(RunOutcome::Await(value)) => {
        runner.borrow_mut().execution = Some(execution);
        if let Err(error) = Self::queue_async_resume(host, runner, value) {
          let result_promise = Rc::clone(&runner.borrow().result_promise);
          reject_pending_promise(
            host,
            &result_promise,
            Value::String(error.to_string().into()),
          );
          if let Some(mut execution) = runner.borrow_mut().execution.take() {
            execution
              .vm
              .discard_suspended_activation(&mut execution.register_state);
          }
        }
      }
      Ok(RunOutcome::Return(value)) => {
        execution
          .vm
          .discard_suspended_activation(&mut execution.register_state);
        let result_promise = Rc::clone(&runner.borrow().result_promise);
        if let Err(error) = resolve_promise_with_value(host, &result_promise, value) {
          reject_pending_promise(
            host,
            &result_promise,
            Value::String(error.to_string().into()),
          );
        }
      }
      Ok(RunOutcome::Initialized) => {
        Self::reject_invalid_async_outcome(host, runner, &mut execution, "generator boundary");
      }
      Ok(RunOutcome::Yield(_) | RunOutcome::YieldStar(_)) => {
        Self::reject_invalid_async_outcome(host, runner, &mut execution, "yield boundary");
      }
      Err(error) => {
        let reason = execution
          .vm
          .exceptions
          .last_uncaught
          .take()
          .unwrap_or_else(|| Value::String(error.to_string().into()));
        execution
          .vm
          .discard_suspended_activation(&mut execution.register_state);
        let result_promise = Rc::clone(&runner.borrow().result_promise);
        reject_pending_promise(host, &result_promise, reason);
      }
    }
  }

  fn reject_invalid_async_outcome(
    host: &mut Vm,
    runner: &Rc<RefCell<AsyncRunner>>,
    execution: &mut AsyncExecution,
    boundary: &str,
  ) {
    execution
      .vm
      .discard_suspended_activation(&mut execution.register_state);
    let result_promise = Rc::clone(&runner.borrow().result_promise);
    reject_pending_promise(
      host,
      &result_promise,
      Value::String(format!("invalid {boundary} in async function").into()),
    );
  }

  fn queue_async_resume(
    host: &mut Vm,
    runner: &Rc<RefCell<AsyncRunner>>,
    awaited: Value,
  ) -> Result<(), crate::errors::RuntimeError> {
    let promise = crate::gc::track_promise(Rc::new(RefCell::new(PromiseState::Pending {
      on_fulfilled: Vec::new(),
      on_rejected: Vec::new(),
    })));
    resolve_promise_with_value(host, &promise, awaited)?;

    let fulfilled_runner = Rc::clone(runner);
    let on_fulfilled = Value::Builtin(Rc::new(move |vm, args| {
      let offset = usize::from(args.len() > 1);
      let value = args
        .get(offset)
        .cloned()
        .unwrap_or(Value::Undefined);
      Self::resume_async_runner(vm, &fulfilled_runner, AsyncResume::Fulfill(value));
      Ok(Value::Undefined)
    }));
    let rejected_runner = Rc::clone(runner);
    let on_rejected = Value::Builtin(Rc::new(move |vm, args| {
      let offset = usize::from(args.len() > 1);
      let reason = args
        .get(offset)
        .cloned()
        .unwrap_or(Value::Undefined);
      Self::resume_async_runner(vm, &rejected_runner, AsyncResume::Reject(reason));
      Ok(Value::Undefined)
    }));

    enum PromiseReaction {
      Pending,
      Fulfilled(Value),
      Rejected(Value),
    }

    let reaction = {
      let mut state = promise.borrow_mut();
      match &mut *state {
        PromiseState::Pending {
          on_fulfilled: fulfilled,
          on_rejected: rejected,
        } => {
          fulfilled.push(on_fulfilled.clone());
          rejected.push(on_rejected.clone());
          PromiseReaction::Pending
        }
        PromiseState::Fulfilled(value) => PromiseReaction::Fulfilled(value.clone()),
        PromiseState::Rejected { reason, handled } => {
          *handled = true;
          PromiseReaction::Rejected(reason.clone())
        }
      }
    };

    match reaction {
      PromiseReaction::Pending => {}
      PromiseReaction::Fulfilled(value) => {
        host.queue_microtask(move |vm| {
          vm.call_value_with_this(
            &on_fulfilled,
            &Value::Undefined,
            std::slice::from_ref(&value),
          )?;
          Ok(())
        });
      }
      PromiseReaction::Rejected(reason) => {
        host.queue_microtask(move |vm| {
          vm.call_value_with_this(
            &on_rejected,
            &Value::Undefined,
            std::slice::from_ref(&reason),
          )?;
          Ok(())
        });
      }
    }
    Ok(())
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
    let rt = get_global_bg_runtime();
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
    Ok(get_global_bg_runtime().block_on(fut))
  }

  /// Spawn a fire-and-forget task on the background runtime.
  pub fn spawn_bg<Fut>(
    &self,
    fut: Fut,
  ) where
    Fut: Future<Output = ()> + Send + 'static,
  {
    drop(get_global_bg_runtime().spawn(fut));
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
      Value::Promise(state, _) if matches!(*state.borrow(), PromiseState::Pending { .. })
    ));

    vm.resolve_host_promise(&promise, Value::Number(42.0))
      .expect("resolve host promise");
    assert!(matches!(
      &promise,
      Value::Promise(state, _)
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
