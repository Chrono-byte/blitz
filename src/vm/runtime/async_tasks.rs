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
use crate::value::{PromiseState, Value};
use crate::vm::state::*;
use std::cell::RefCell;
use std::future::Future;
use std::rc::Rc;
use std::sync::Arc;

impl Vm {
    /// Queue a microtask to be executed after the current call stack completes.
    ///
    /// # Errors
    ///
    /// If the microtask queue is full, an error is returned.
    pub(crate) fn queue_microtask<F>(&mut self, job: F)
    where
        F: FnMut(&mut Vm) -> Result<(), crate::errors::RuntimeError> + 'static,
    {
        self.microtasks.push_back(Box::new(job));
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
    pub(crate) fn drain_microtasks(&mut self) -> Result<(), crate::errors::RuntimeError> {
        while let Some(mut job) = self.microtasks.pop_front() {
            if let Err(err) = job(self) {
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

    pub(super) fn run_microtasks_if_needed(&mut self) -> Result<(), crate::errors::RuntimeError> {
        if self.microtasks.is_empty() {
            return Ok(());
        }
        self.drain_microtasks()
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
        env: &Rc<RefCell<Vec<Rc<RefCell<Value>>>>>,
        call_args: Vec<Value>,
        this_arg: Value,
    ) -> Result<Value, crate::errors::RuntimeError> {
        let promise = Rc::new(RefCell::new(PromiseState::Pending {
            on_fulfilled: Vec::new(),
            on_rejected: Vec::new(),
        }));

        let proto = Rc::clone(proto);
        let env = Rc::clone(env);
        let promise_for_job = Rc::clone(&promise);
        let stack_base = self.stack.len();
        let saved_stack = self.stack[..stack_base].to_vec();
        let caller_chunk = Rc::clone(&self.chunk);
        let caller_ip = self.ip;
        let caller_locals = self.locals.clone();
        let caller_this = self.this_value.clone();
        let caller_env = self.current_closure_env.clone();
        let caller_lexical_env = self.lexical_env.clone();
        let caller_slot_types = self.current_slot_types.clone();
        let caller_frame_len = self.frames.len();
        let caller_handler_len = self.handlers.len();
        let thrown_value = self.stack.last().cloned();

        self.last_uncaught_exception = None;
        let exec_result = (|| {
            self.push_call_frame(&proto, &env, call_args, this_arg)?;
            self.run_until_return(stack_base, false)
        })();

        self.chunk = caller_chunk;
        self.ip = caller_ip;
        self.locals = caller_locals;
        self.this_value = caller_this;
        self.current_closure_env = caller_env;
        self.lexical_env = caller_lexical_env;
        self.current_slot_types = caller_slot_types;
        self.stack = saved_stack;
        self.frames.truncate(caller_frame_len);
        self.handlers.truncate(caller_handler_len);

        match exec_result {
            Ok(result) => {
                let _ = resolve_promise_with_value(self, &promise_for_job, result);
            }
            Err(err) => {
                let reason = thrown_value
                    .or_else(|| self.last_uncaught_exception.take())
                    .unwrap_or_else(|| Value::String(format!("{}", err)));
                reject_pending_promise(self, &promise_for_job, reason);
            }
        }

        self.run_microtasks_if_needed()?;

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
