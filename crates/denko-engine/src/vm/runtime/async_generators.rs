// SPDX-License-Identifier: MPL-2.0
//! Async generators serialize protocol requests while their activation awaits.
use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use crate::bytecode::Function;
use crate::errors::RuntimeError;
use crate::runtime_utils::{reject_pending_promise, resolve_promise_with_value};
use crate::value::{ObjectData, PropertyKey, Value};
use crate::vm::Vm;
use crate::vm::state::{AwaitSettlement, RegisterState, RunOutcome, YieldStarResume};

#[derive(Clone)]
enum RequestKind {
  Next(Value),
  Throw(Value),
  Return(Value),
}
struct Request {
  kind: RequestKind,
  promise: Value,
}
struct Execution {
  vm: Vm,
  registers: RegisterState,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Status {
  Start,
  Yield,
  Busy,
  Awaiting,
  Completed,
}
struct Runner {
  execution: Option<Execution>,
  status: Status,
  requests: VecDeque<Request>,
  pending_roots: Option<crate::gc::VmGcRegistration>,
}
#[derive(Clone, Copy)]
enum WaitKind {
  Body,
  Yield,
  Return,
  CompletedReturn,
}
enum Resume {
  Start,
  Next(Value),
  Throw(Value),
  Return(Value),
  Settlement(AwaitSettlement),
}

impl Vm {
  pub(in crate::vm::runtime) fn create_async_generator_object(
    &mut self,
    proto: &Rc<Function>,
    env: &crate::closure_environment::ClosureEnvironment,
    args: Vec<Value>,
    this_value: Value,
    callee: &Value,
  ) -> Result<Value, RuntimeError> {
    let mut child = self.fork_execution();
    let register_args = args
      .iter()
      .take(crate::bytecode::abi::REGISTER_ARGUMENT_COUNT)
      .cloned()
      .collect::<Vec<_>>();
    child.push_call_frame(proto, env, args, this_value, callee.clone())?;
    let mut registers = RegisterState::new(
      child.execution.active.this_value.clone(),
      &register_args,
      child.execution.active.chunk.spill_slot_count as usize,
    );
    let initialized = child.run_state(&mut registers, true);
    self.take_jobs_from(&mut child);
    match initialized {
      Ok(RunOutcome::Initialized) => {}
      Err(error) => {
        return Err(match child.exceptions.last_uncaught.take() {
          Some(reason) => self.throw_exception(reason),
          None => error,
        });
      }
      _ => {
        return Err(RuntimeError::from(
          "async generator reached body before initialization",
        ));
      }
    }
    let runner = Rc::new(RefCell::new(Runner {
      execution: Some(Execution {
        vm: child,
        registers,
      }),
      status: Status::Start,
      requests: VecDeque::new(),
      pending_roots: None,
    }));
    let prototype = self.get_property_host(callee, &PropertyKey::prototype())?;
    let prototype = if prototype.is_object() {
      prototype
    } else {
      self.realm_state.realm.async_generator_prototype()
    };
    let object =
      crate::gc::track_object(Rc::new(RefCell::new(ObjectData::with_prototype(prototype))));
    for (slot, kind) in [
      ("[[AsyncGeneratorNext]]", 0),
      ("[[AsyncGeneratorThrow]]", 1),
      ("[[AsyncGeneratorReturn]]", 2),
    ] {
      let runner = Rc::clone(&runner);
      object.borrow_mut().set_internal_slot(
        slot,
        Value::Builtin(Rc::new(move |vm, args| {
          let offset = usize::from(vm.builtin_this_arg_prepended());
          let argument = args
            .get(offset)
            .cloned()
            .unwrap_or(Value::Undefined);
          let kind = match kind {
            0 => RequestKind::Next(argument),
            1 => RequestKind::Throw(argument),
            _ => RequestKind::Return(argument),
          };
          let promise = vm.create_host_promise();
          if runner.borrow().pending_roots.is_none() {
            runner.borrow_mut().pending_roots = Some(crate::gc::VmGcRegistration::new());
          }
          runner.borrow_mut().requests.push_back(Request {
            kind,
            promise: promise.clone(),
          });
          Self::pump_async_generator(vm, &runner);
          Ok(promise)
        })),
      );
    }
    Ok(Value::Object(object))
  }

  fn pump_async_generator(
    host: &mut Vm,
    runner: &Rc<RefCell<Runner>>,
  ) {
    loop {
      let (status, kind) = {
        let mut runner = runner.borrow_mut();
        if matches!(runner.status, Status::Busy | Status::Awaiting) {
          return;
        }
        let Some(request) = runner.requests.front() else {
          return;
        };
        let kind = request.kind.clone();
        let status = runner.status;
        runner.status = Status::Busy;
        (status, kind)
      };
      match (status, kind) {
        (Status::Completed, RequestKind::Next(_)) => {
          Self::finish_async_generator_request(host, runner, Ok(Value::Undefined), true)
        }
        (Status::Completed, RequestKind::Throw(reason)) => {
          Self::finish_async_generator_request(host, runner, Err(reason), true)
        }
        (Status::Completed | Status::Start, RequestKind::Return(value)) => {
          Self::complete_async_generator(runner);
          Self::wait_async_generator(host, runner, value, WaitKind::CompletedReturn);
          return;
        }
        (Status::Start, RequestKind::Throw(reason)) => {
          Self::complete_async_generator(runner);
          Self::finish_async_generator_request(host, runner, Err(reason), true);
        }
        (Status::Start, RequestKind::Next(_)) => {
          Self::run_async_generator(host, runner, Resume::Start)
        }
        (Status::Yield, RequestKind::Next(value)) => {
          Self::run_async_generator(host, runner, Resume::Next(value))
        }
        (Status::Yield, RequestKind::Throw(reason)) => {
          Self::run_async_generator(host, runner, Resume::Throw(reason))
        }
        (Status::Yield, RequestKind::Return(value)) => {
          Self::wait_async_generator(host, runner, value, WaitKind::Return);
          return;
        }
        _ => unreachable!("only idle generator states are pumped"),
      }
    }
  }

  fn complete_async_generator(runner: &Rc<RefCell<Runner>>) {
    let execution = runner.borrow_mut().execution.take();
    if let Some(mut execution) = execution {
      execution
        .vm
        .discard_suspended_activation(&mut execution.registers);
    }
    runner.borrow_mut().status = Status::Completed;
  }

  fn finish_async_generator_request(
    host: &mut Vm,
    runner: &Rc<RefCell<Runner>>,
    result: Result<Value, Value>,
    done: bool,
  ) {
    let result = result.map(|value| host.make_iterator_result(value, done));
    Self::settle_async_generator_request(host, runner, result, done);
  }

  fn settle_async_generator_request(
    host: &mut Vm,
    runner: &Rc<RefCell<Runner>>,
    result: Result<Value, Value>,
    done: bool,
  ) {
    let request = {
      let mut runner = runner.borrow_mut();
      runner.status = if done {
        Status::Completed
      } else {
        Status::Busy
      };
      runner
        .requests
        .pop_front()
        .expect("active generator request")
    };
    let Value::Promise(promise, _) = request.promise else {
      unreachable!("intrinsic request promise")
    };
    match result {
      Ok(value) => {
        if let Err(error) = resolve_promise_with_value(host, &promise, value) {
          let reason = host
            .exceptions
            .last_uncaught
            .take()
            .unwrap_or_else(|| Value::String(error.to_string().into()));
          reject_pending_promise(host, &promise, reason);
        }
      }
      Err(reason) => reject_pending_promise(host, &promise, reason),
    }
    let mut runner = runner.borrow_mut();
    if !done {
      runner.status = Status::Yield;
    }
    if runner.requests.is_empty() {
      runner.pending_roots = None;
    }
  }

  fn run_async_generator(
    host: &mut Vm,
    runner: &Rc<RefCell<Runner>>,
    resume: Resume,
  ) {
    // JS may enqueue another request during any operation below.
    let mut execution = {
      let mut runner = runner.borrow_mut();
      runner.status = Status::Busy;
      runner
        .execution
        .take()
        .expect("live generator activation")
    };
    let injected = match resume {
      Resume::Start => Ok(()),
      Resume::Next(value) => {
        if let Some(delegate) = &mut execution.vm.execution.active.yield_star {
          delegate.resume = Some(YieldStarResume::Next(value));
        } else {
          execution.registers.resume_with(value);
        }
        Ok(())
      }
      Resume::Throw(reason) => {
        if let Some(delegate) = &mut execution.vm.execution.active.yield_star {
          delegate.resume = Some(YieldStarResume::Throw(reason));
          Ok(())
        } else {
          execution.vm.throw_value(reason)
        }
      }
      Resume::Return(value) => {
        if let Some(delegate) = &mut execution.vm.execution.active.yield_star {
          delegate.resume = Some(YieldStarResume::Return(value));
          Ok(())
        } else if execution.vm.begin_generator_return(value.clone()) {
          Ok(())
        } else {
          runner.borrow_mut().execution = Some(execution);
          Self::complete_async_generator(runner);
          Self::finish_async_generator_request(host, runner, Ok(value), true);
          return;
        }
      }
      Resume::Settlement(settlement) => {
        if execution.registers.pending_operation.is_some() {
          execution
            .registers
            .settle_pending_operation(settlement);
          Ok(())
        } else {
          match settlement {
            AwaitSettlement::Fulfilled(value) => {
              execution.registers.resume_with(value);
              Ok(())
            }
            AwaitSettlement::Rejected(reason) => execution.vm.throw_value(reason),
          }
        }
      }
    };
    let outcome = injected.and_then(|_| {
      execution
        .vm
        .run_state(&mut execution.registers, false)
    });
    host.take_jobs_from(&mut execution.vm);
    let failure = if let Err(error) = &outcome {
      Some(
        execution
          .vm
          .exceptions
          .last_uncaught
          .take()
          .unwrap_or_else(|| Value::String(error.to_string().into())),
      )
    } else {
      None
    };
    runner.borrow_mut().execution = Some(execution);
    match outcome {
      Ok(RunOutcome::Await(value)) => {
        Self::wait_async_generator(host, runner, value, WaitKind::Body)
      }
      Ok(RunOutcome::Yield(value)) => {
        Self::wait_async_generator(host, runner, value, WaitKind::Yield)
      }
      Ok(RunOutcome::Return(value)) => {
        Self::complete_async_generator(runner);
        Self::finish_async_generator_request(host, runner, Ok(value), true);
      }
      Err(_) => {
        Self::complete_async_generator(runner);
        Self::finish_async_generator_request(
          host,
          runner,
          Err(failure.expect("execution failure reason")),
          true,
        );
      }
      Ok(RunOutcome::YieldStar(result)) => {
        Self::settle_async_generator_request(host, runner, Ok(result), false)
      }
      Ok(RunOutcome::Initialized) => {
        Self::complete_async_generator(runner);
        Self::finish_async_generator_request(
          host,
          runner,
          Err(Value::String("invalid async generator suspension".into())),
          true,
        );
      }
    }
  }

  fn wait_async_generator(
    host: &mut Vm,
    runner: &Rc<RefCell<Runner>>,
    value: Value,
    kind: WaitKind,
  ) {
    runner.borrow_mut().status = Status::Awaiting;
    let fulfilled = Rc::clone(runner);
    let on_fulfilled = Value::Builtin(Rc::new(move |vm, args| {
      let offset = usize::from(vm.builtin_this_arg_prepended());
      let value = args
        .get(offset)
        .cloned()
        .unwrap_or(Value::Undefined);
      Self::settle_async_generator(vm, &fulfilled, kind, AwaitSettlement::Fulfilled(value));
      Ok(Value::Undefined)
    }));
    let rejected = Rc::clone(runner);
    let on_rejected = Value::Builtin(Rc::new(move |vm, args| {
      let offset = usize::from(vm.builtin_this_arg_prepended());
      let reason = args
        .get(offset)
        .cloned()
        .unwrap_or(Value::Undefined);
      Self::settle_async_generator(vm, &rejected, kind, AwaitSettlement::Rejected(reason));
      Ok(Value::Undefined)
    }));
    let observed = if matches!(kind, WaitKind::CompletedReturn) {
      host
        .observe_await(value, on_fulfilled, on_rejected)
        .map_err(|error| {
          host
            .exceptions
            .last_uncaught
            .take()
            .unwrap_or_else(|| Value::String(error.to_string().into()))
        })
    } else {
      let mut execution = runner
        .borrow_mut()
        .execution
        .take()
        .expect("awaiting generator activation");
      let observed = execution
        .vm
        .observe_await(value, on_fulfilled, on_rejected)
        .map_err(|error| {
          execution
            .vm
            .exceptions
            .last_uncaught
            .take()
            .unwrap_or_else(|| Value::String(error.to_string().into()))
        });
      host.take_jobs_from(&mut execution.vm);
      runner.borrow_mut().execution = Some(execution);
      observed
    };
    if let Err(reason) = observed {
      Self::settle_async_generator(host, runner, kind, AwaitSettlement::Rejected(reason));
    }
  }

  fn settle_async_generator(
    host: &mut Vm,
    runner: &Rc<RefCell<Runner>>,
    kind: WaitKind,
    settlement: AwaitSettlement,
  ) {
    match (kind, settlement) {
      (WaitKind::Body, settlement) => {
        Self::run_async_generator(host, runner, Resume::Settlement(settlement))
      }
      (WaitKind::Yield, AwaitSettlement::Fulfilled(value)) => {
        Self::finish_async_generator_request(host, runner, Ok(value), false)
      }
      (WaitKind::Yield | WaitKind::Return, AwaitSettlement::Rejected(reason)) => {
        Self::run_async_generator(host, runner, Resume::Throw(reason));
      }
      (WaitKind::Return, AwaitSettlement::Fulfilled(value)) => {
        Self::run_async_generator(host, runner, Resume::Return(value))
      }
      (WaitKind::CompletedReturn, AwaitSettlement::Fulfilled(value)) => {
        Self::finish_async_generator_request(host, runner, Ok(value), true)
      }
      (WaitKind::CompletedReturn, AwaitSettlement::Rejected(reason)) => {
        Self::finish_async_generator_request(host, runner, Err(reason), true)
      }
    }
    Self::pump_async_generator(host, runner);
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn evaluate(
    source: &str,
    expected: &str,
  ) -> Vm {
    let source = crate::js!(format(script), "{}", source);
    let session = crate::parser::Parser::parse_source(&source).expect("parse");
    let code = crate::compiler::compile_session(&session).expect("compile");
    let globals = Rc::new(RefCell::new(ObjectData::new()));
    globals.borrow_mut().properties.insert(
      PropertyKey::String("globalThis".into()),
      crate::value::PropertyDescriptor::ordinary_data(Value::Object(Rc::clone(&globals))),
    );
    crate::runtime_utils::install_minimal_kernel_constructors(&globals);
    let mut vm = Vm::new(globals);
    vm.run(code).expect("run");
    vm.flush_microtasks().expect("flush");
    let result = vm.get_global("result").expect("result global");
    assert!(
      matches!(result, Value::String(ref s) if s.as_str_lossy() == expected),
      "expected {expected}, got {result:?}"
    );
    vm
  }

  #[test]
  fn iterator_result_then_getters_enqueue_without_resuming_the_body_reentrantly() {
    evaluate(
      crate::js!(
        r#"
      var result = 'unfinished'; var log = [];
      var generator = (async function*() { yield 1; log.push('body'); yield 2; })();
      Object.defineProperty(Object.prototype, 'then', {
        configurable: true,
        get() {
          if (Object.hasOwn(this, 'done')) {
            delete Object.prototype.then;
            log.push('getter'); generator.next(); log.push('after');
          }
        }
      });
      generator.next().then(() => { result = log.join(','); });
    "#
      ),
      "getter,after,body",
    );
  }

  #[test]
  fn async_generator_requests_are_promises_and_fifo() {
    evaluate(
      crate::js!(
        r#"
      var result = 'unfinished'; var started = false; var ignored;
      async function* values() {
        started = true;
        ignored = yield Promise.resolve(1);
        yield await Promise.resolve(ignored);
        return 3;
      }
      var g = values(); var lazy = !started;
      var first = g.next(99); var immediate = started;
      var second = g.next(2); var third = g.next();
      (async function () {
        var a = await first; var b = await second; var c = await third;
        result = JSON.stringify([lazy, immediate, first instanceof Promise, a, b, c]);
      })();
    "#
      ),
      r#"[true,true,true,{"value":1,"done":false},{"value":2,"done":false},{"value":3,"done":true}]"#,
    );
  }

  #[test]
  fn async_generator_rejections_reenter_catch_and_finally() {
    evaluate(
      crate::js!(
        r#"
      var result = 'unfinished'; var sentinel = {};
      async function* values() {
        try { yield Promise.reject(sentinel); }
        catch (error) { yield error === sentinel; }
        try { return Promise.reject(sentinel); }
        catch (error) { yield error === sentinel; return 7; }
      }
      (async function () {
        var g = values(); var a = await g.next(); var b = await g.next(); var c = await g.next();
        result = JSON.stringify([a,b,c]);
      })();
    "#
      ),
      r#"[{"value":true,"done":false},{"value":true,"done":false},{"value":7,"done":true}]"#,
    );
  }

  #[test]
  fn async_generator_return_awaits_and_runs_finally() {
    evaluate(
      crate::js!(
        r#"
      var result = 'unfinished';
      async function* values() { try { yield 1; } finally { await 0; yield 2; } }
      (async function () {
        var g = values(); var a = await g.next(); var b = await g.return(Promise.resolve(9)); var c = await g.next();
        result = JSON.stringify([a,b,c]);
      })();
    "#
      ),
      r#"[{"value":1,"done":false},{"value":2,"done":false},{"value":9,"done":true}]"#,
    );
  }

  #[test]
  fn async_generator_completed_requests_and_reentrant_next() {
    evaluate(
      crate::js!(
        r#"
      var result = 'unfinished'; var queued; var g;
      async function* values() { queued = g.next(); yield 1; return 2; }
      g = values(); var first = g.next();
      (async function () {
        var a = await first; var b = await queued; var c = await g.return(Promise.resolve(3));
        var sentinel = {}; var same = false;
        try { await g.throw(sentinel); } catch (error) { same = error === sentinel; }
        var d = await g.next();
        result = JSON.stringify([a,b,c,same,d]);
      })();
    "#
      ),
      r#"[{"value":1,"done":false},{"value":2,"done":true},{"value":3,"done":true},true,{"done":true}]"#,
    );
  }

  #[test]
  fn async_generator_return_before_start_does_not_execute_body() {
    evaluate(
      crate::js!(
        r#"
      var result = 'unfinished'; var started = false;
      async function* values() { started = true; yield 1; }
      (async function () {
        var g = values(); var a = await g.return(Promise.resolve(8)); var b = await g.next();
        result = JSON.stringify([started,a,b]);
      })();
    "#
      ),
      r#"[false,{"value":8,"done":true},{"done":true}]"#,
    );
  }
  #[test]
  fn async_generator_instance_uses_prototype_after_parameter_initialization() {
    evaluate(
      crate::js!(
        r#"
      var result = 'unfinished';
      var g = async function* (value = (g.prototype = null)) {};
      var old = g.prototype;
      var descriptor = Object.getOwnPropertyDescriptor(g, 'prototype');
      var iterator = g();
      result = JSON.stringify([descriptor.writable, !descriptor.enumerable,
        !descriptor.configurable, Object.getPrototypeOf(iterator) !== old]);
    "#
      ),
      "[true,true,true,true]",
    );
  }

  #[test]
  fn async_generator_then_callbacks_observe_values() {
    let mut vm = evaluate(
      crate::js!(
        r#"
      var result = 'unfinished'; var results = [];
      async function* values() { const sent = yield Promise.resolve(1); yield await Promise.resolve(sent + 1); return 9; }
      const iterator = values();
      const first = iterator.next(); const second = iterator.next(4); const third = iterator.next();
      first.then(item => results.push(item.value + ':' + item.done));
      second.then(item => results.push(item.value + ':' + item.done));
      third.then(item => results.push(item.value + ':' + item.done));
      third.then(() => { result = JSON.stringify(globalThis.savedResults); });
      globalThis.savedResults = results;
    "#
      ),
      r#"["1:false","5:false","9:true"]"#,
    );
    let values = vm
      .get_global("savedResults")
      .expect("saved results");
    assert_eq!(
      vm.get_property_host(&values, &PropertyKey::String("length".into()))
        .expect("length"),
      Value::Number(3.0)
    );
    for (index, expected) in ["1:false", "5:false", "9:true"]
      .iter()
      .enumerate()
    {
      assert_eq!(
        vm.get_property_host(&values, &PropertyKey::String(index.to_string().into()))
          .expect("item"),
        Value::String((*expected).into())
      );
    }
  }

  #[test]
  fn async_generator_bare_return_does_not_add_an_await_turn() {
    evaluate(
      crate::js!(
        r#"
      var result = 'unfinished'; var order = '';
      (async function* () { return; })().next().then(() => { order += 'bare '; });
      (async function* () { return undefined; })().next().then(() => { order += 'value '; });
      Promise.resolve().then(() => { order += 'tick '; });
      Promise.resolve().then(() => {}).then(() => { result = order; });
    "#
      ),
      "bare tick value ",
    );
  }

  #[test]
  fn borrowed_next_awaits_in_the_generator_realm() {
    fn run(
      vm: &mut Vm,
      source: &str,
    ) {
      let session = crate::parser::Parser::parse_source(source).expect("parse");
      let chunk = crate::compiler::compile_session(&session).expect("compile");
      vm.run(chunk).expect("run");
    }
    let mut foreign = Vm::new(Rc::new(RefCell::new(ObjectData::new())));
    run(
      &mut foreign,
      crate::js!(
        r#"
      globalThis.generator = (async function* () {
        const promise = Promise.resolve(7);
        promise.then = function () { throw 'wrong then'; };
        yield await promise;
      })();
    "#
      ),
    );
    let mut local = Vm::new(Rc::new(RefCell::new(ObjectData::new())));
    local.realm().set_global(
      "generator",
      foreign
        .get_global("generator")
        .expect("foreign generator"),
    );
    run(
      &mut local,
      crate::js!(
        r#"
      var passed = false;
      const next = Object.getPrototypeOf(Object.getPrototypeOf((async function* () {})())).next;
      next.call(generator).then(result => { passed = result.value === 7; });
    "#
      ),
    );
    local
      .flush_microtasks()
      .expect("settle borrowed next");
    assert_eq!(local.get_global("passed"), Some(Value::Bool(true)));
  }

  #[test]
  fn completed_async_generators_release_execution_but_root_pending_requests() {
    fn run(
      vm: &mut Vm,
      source: &str,
    ) {
      let source = crate::js!(format(script), "{}", source);
      let session = crate::parser::Parser::parse_source(&source).expect("parse");
      let code = crate::compiler::compile_session(&session).expect("compile");
      vm.run(code).expect("run");
    }
    let mut vm = Vm::new(Rc::new(RefCell::new(ObjectData::new())));
    let baseline = crate::gc::live_vm_count();
    run(
      &mut vm,
      crate::js!("var g = (async function* () {})(); g.next();"),
    );
    vm.flush_microtasks().expect("finish generator");
    assert_eq!(crate::gc::live_vm_count(), baseline);
    run(
      &mut vm,
      crate::js!(
        r#"
      var release; var result = 'unfinished';
      g.return(new Promise(function (resolve) { release = resolve; }));
      var queuedValue = { answer: 42 };
      g.return(queuedValue).then(function (item) { result = item.value.answer; });
      queuedValue = null;
    "#
      ),
    );
    assert_eq!(crate::gc::live_vm_count(), baseline + 1);
    vm.collect_garbage_prototype();
    let release = vm.get_global("release").expect("release");
    vm.call_value_with_this(&release, &Value::Undefined, &[Value::Undefined])
      .expect("release wait");
    vm.flush_microtasks()
      .expect("finish queued requests");
    assert_eq!(vm.get_global("result"), Some(Value::Number(42.0)));
    assert_eq!(crate::gc::live_vm_count(), baseline);
  }
}
