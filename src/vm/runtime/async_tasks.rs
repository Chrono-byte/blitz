//! Async/microtask helpers and background runtime utilities.
//!
//! This module provides utilities the VM uses to schedule and run
//! microtasks, interact with the background Tokio runtime, and bridge
//! synchronous VM execution with asynchronous operations (promises,
//! async functions, etc.).
//!
//! Microtask / job queue semantics
//! - The current microtask queue implementation stores closures and runs
//!   them in FIFO order. If a microtask throws an error while being drained,
//!   the draining routine returns early and remaining microtasks are not
//!   executed. This is a host-specific choice and differs from the behaviour
//!   of mainstream engines which typically process microtasks to completion
//!   and surface exceptions to the host's unhandled rejection/exception
//!   mechanism.
//! - Promises and thenable resolution in this runtime may synchronously
//!   invoke reactions depending on call sites; the spec requires careful
//!   job scheduling for promise resolution (PromiseJobs) to guarantee
//!   observable ordering.
//!
//! Recommendations:
//! - When targeting ECMA-262 conformance, schedule promise reactions and
//!   thenable resolution work via microtasks/jobs and adjust `drain_*
//!   ` helpers to run jobs to completion while exposing unhandled
//!   exceptions through a host hook.
use crate::bytecode::FunctionProto;
use crate::runtime_utils::{reject_pending_promise, resolve_promise_with_value};
use crate::value::{PromiseState, Value, ValueSlot};
use crate::vm::state::*;
use std::cell::RefCell;
use std::future::Future;
use std::rc::Rc;
use std::sync::Arc;

impl Vm {
    /// Queue a microtask to be executed after the current call stack completes.
    pub(crate) fn queue_microtask<F>(&mut self, job: F)
    where
        F: FnMut(&mut Vm) -> Result<(), crate::errors::RuntimeError> + 'static,
    {
        self.microtasks.push_back(Box::new(job));
    }

    /// Queue a dynamic import job to be executed by the host job loop.
    pub(crate) fn queue_dynamic_import_job<F>(&mut self, job: F)
    where
        F: FnMut(&mut Vm) -> Result<(), crate::errors::RuntimeError> + 'static,
    {
        self.dynamic_import_jobs.push_back(Box::new(job));
    }

    /// Install a host hook which will be invoked for unhandled promise
    /// rejections and microtask exceptions. The hook receives (vm, promise, reason)
    /// where `promise` is the rejected promise object or `Undefined` for
    /// microtask exceptions that aren't tied to a specific promise.
    pub fn set_unhandled_rejection_hook<F>(&mut self, hook: F)
    where
        F: FnMut(&mut Vm, Value, Value) + 'static,
    {
        self.unhandled_rejection_hook = Some(Rc::new(RefCell::new(hook)));
    }

    /// Invoke the installed unhandled rejection hook, if present.
    pub(crate) fn call_unhandled_rejection(&mut self, promise: Value, reason: Value) {
        if let Some(hook_rc) = self.unhandled_rejection_hook.as_ref().map(Rc::clone) {
            let mut hook = hook_rc.borrow_mut();
            (hook)(self, promise, reason);
        }
    }

    /// Drain all pending microtasks, executing them in FIFO order.
    ///
    /// # Errors
    ///
    /// Microtask callbacks are executed to completion. If a microtask returns
    /// an error the VM records it on `last_uncaught_exception` and continues
    /// draining the remaining microtasks. This mirrors the common host policy
    /// of running microtasks to completion while surfaceing exceptions via a
    /// host hook instead of aborting the queue on first error.
    pub(crate) fn drain_dynamic_import_jobs(&mut self) -> Result<(), crate::errors::RuntimeError> {
        while let Some(mut job) = self.dynamic_import_jobs.pop_front() {
            let caller_handlers = std::mem::take(&mut self.handlers);
            let result = job(self);
            self.handlers = caller_handlers;
            self.maybe_collect_garbage_prototype();
            if let Err(err) = result {
                let exc_val = Value::String(format!("{}", err));
                self.last_uncaught_exception = Some(exc_val.clone());
                if let Some(hook_rc) = self.unhandled_rejection_hook.as_ref().map(Rc::clone) {
                    let mut hook = hook_rc.borrow_mut();
                    (hook)(self, Value::Undefined, exc_val.clone());
                }
                continue;
            }
        }
        Ok(())
    }

    pub(crate) fn drain_microtasks(&mut self) -> Result<(), crate::errors::RuntimeError> {
        while let Some(mut job) = self.microtasks.pop_front() {
            let caller_handlers = std::mem::take(&mut self.handlers);
            let result = job(self);
            self.handlers = caller_handlers;
            self.maybe_collect_garbage_prototype();
            if let Err(err) = result {
                // Record the last uncaught exception so callers or host hooks
                // can inspect it. Convert the runtime error into a JS value
                // string for now; hosts may implement richer reporting.
                let exc_val = Value::String(format!("{}", err));
                self.last_uncaught_exception = Some(exc_val.clone());
                // If a host hook is installed, call it with Undefined promise
                // to indicate this was a microtask exception.
                if let Some(hook_rc) = self.unhandled_rejection_hook.as_ref().map(Rc::clone) {
                    let mut hook = hook_rc.borrow_mut();
                    (hook)(self, Value::Undefined, exc_val.clone());
                }
                // Continue draining remaining microtasks rather than returning.
                continue;
            }
        }
        Ok(())
    }

    pub(crate) fn run_microtasks_if_needed(&mut self) -> Result<(), crate::errors::RuntimeError> {
        while !self.microtasks.is_empty() || !self.dynamic_import_jobs.is_empty() {
            if !self.microtasks.is_empty() {
                self.drain_microtasks()?;
                continue;
            }
            self.drain_dynamic_import_jobs()?;
        }
        Ok(())
    }

    /// Resolve a value through the runtime's promise machinery and wait for it
    /// to settle using the existing microtask queue.
    pub(crate) fn await_value(
        &mut self,
        value: Value,
    ) -> Result<Value, crate::errors::RuntimeError> {
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
                    self.last_uncaught_exception = Some(reason);
                    return Err(crate::errors::RuntimeError::from("Promise rejected"));
                }
                None => {
                    if !self.microtasks.is_empty() || !self.dynamic_import_jobs.is_empty() {
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

    /// Drain any pending microtasks, ensuring async bootstrap work completes.
    ///
    /// # Errors
    ///
    /// If a microtask throws an error, it is propagated and the remaining microtasks are not executed.
    pub fn flush_microtasks(&mut self) -> Result<(), crate::errors::RuntimeError> {
        self.run_microtasks_if_needed()
    }

    /// Invoke an async closure, queuing its microtasks for execution after the current call stack completes.
    ///
    /// # Errors
    ///
    /// If a microtask throws an error, it is propagated and the remaining microtasks are not executed.
    pub(super) fn invoke_async_closure(
        &mut self,
        proto: &Rc<FunctionProto>,
        env: &Rc<std::sync::Mutex<Vec<ValueSlot>>>,
        call_args: Vec<Value>,
        this_arg: Value,
        callee: &Value,
    ) -> Result<Value, crate::errors::RuntimeError> {
        let promise = crate::gc::track_promise(Rc::new(RefCell::new(PromiseState::Pending {
            on_fulfilled: Vec::new(),
            on_rejected: Vec::new(),
        })));

        let proto = Rc::clone(proto);
        let env = Rc::clone(env);
        let promise_for_job = Rc::clone(&promise);
        let stack_base = self.stack.len();
        let saved_stack = self.stack.snapshot();
        let caller_chunk = Rc::clone(&self.chunk);
        let caller_ip = self.ip;
        let caller_locals = self.locals.clone();
        let caller_this = self.this_value.clone();
        let caller_env = self.current_closure_env.clone();
        let caller_lexical_env = self.lexical_env.clone();
        let caller_slot_types = self.current_slot_types.clone();
        let caller_frame_len = self.call_stack.len();
        let caller_handlers = std::mem::take(&mut self.handlers);
        let thrown_value = self.stack.last().cloned();

        self.last_uncaught_exception = None;
        let exec_result = (|| {
            self.push_call_frame(&proto, &env, call_args, this_arg, callee.clone())?;
            self.run_until_return(stack_base, false)
        })();
        self.maybe_collect_garbage_prototype();

        self.chunk = caller_chunk;
        self.ip = caller_ip;
        self.locals = caller_locals;
        self.this_value = caller_this;
        self.current_closure_env = caller_env;
        self.lexical_env = caller_lexical_env;
        self.current_slot_types = caller_slot_types;
        self.stack.restore_from(saved_stack);
        self.call_stack.truncate(caller_frame_len);
        self.handlers = caller_handlers;

        self.run_microtasks_if_needed()?;
        self.maybe_collect_garbage_prototype();
        match exec_result {
            Ok(result) => {
                let promise_job = Rc::clone(&promise_for_job);
                let mut result_cell = Some(result);
                self.queue_microtask(move |vm| {
                    let value = result_cell.take().unwrap();
                    resolve_promise_with_value(vm, &promise_job, value)?;
                    Ok(())
                });
            }
            Err(err) => {
                let reason = thrown_value
                    .or_else(|| self.last_uncaught_exception.take())
                    .unwrap_or_else(|| Value::String(format!("{}", err)));
                let promise_job = Rc::clone(&promise_for_job);
                let mut reason_cell = Some(reason);
                self.queue_microtask(move |vm| {
                    let reason = reason_cell.take().unwrap();
                    reject_pending_promise(vm, &promise_job, reason);
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
    pub fn run_blocking<F, R>(&self, f: F) -> Result<R, crate::errors::RuntimeError>
    where
        F: FnOnce() -> R + Send + 'static,
        R: Send + 'static,
    {
        let rt = Arc::clone(&self.bg_runtime);
        let handle = rt.spawn_blocking(f);
        rt.block_on(handle).map_err(|e| {
            crate::errors::RuntimeError::from(format!("Background task failed: {}", e))
        })
    }

    /// Synchronously execute an async future on the background runtime.
    ///
    /// # Errors
    ///
    /// Returns a [`crate::errors::RuntimeError`] if the background task fails.
    pub fn block_on<Fut>(&self, fut: Fut) -> Result<Fut::Output, crate::errors::RuntimeError>
    where
        Fut: Future + Send + 'static,
        Fut::Output: Send + 'static,
    {
        Ok(Arc::clone(&self.bg_runtime).block_on(fut))
    }

    /// Spawn a fire-and-forget task on the background runtime.
    pub fn spawn_bg<Fut>(&self, fut: Fut)
    where
        Fut: Future<Output = ()> + Send + 'static,
    {
        let _ = self.bg_runtime.spawn(fut);
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
            vm.last_uncaught_exception.as_ref(),
            Some(Value::String(s)) if s.contains("microtask failed")
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
    fn run_blocking_executes_closure() {
        let vm = test_vm();
        let value = vm.run_blocking(|| 4usize + 5).expect("run blocking");

        assert_eq!(value, 9);
    }
}
