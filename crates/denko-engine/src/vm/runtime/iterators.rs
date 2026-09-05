// SPDX-License-Identifier: MPL-2.0

use crate::errors::RuntimeError;
use crate::value::{ObjectData, PropertyDescriptor, PropertyKey, SymbolId, Value};
use crate::vm::Vm;
use crate::vm::runtime::execution::{YieldStarStep, ensure_slot};
use crate::vm::state::{
  AwaitSettlement, PendingOperation, PendingOperationStep, YieldStarPhase, YieldStarResume,
  YieldStarState,
};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

const ASYNC_ITERATOR_RECORD_SLOT: &str = "[[BlitzAsyncIteratorRecord]]";
const ASYNC_ITERATOR_SLOT: &str = "[[BlitzAsyncIterator]]";
const ASYNC_ITERATOR_NEXT_SLOT: &str = "[[BlitzAsyncIteratorNext]]";
const ASYNC_ITERATOR_SYNC_SLOT: &str = "[[BlitzAsyncIteratorSync]]";
const ASYNC_ITERATOR_DONE_SLOT: &str = "[[BlitzAsyncIteratorDone]]";
const SYNC_ITERATOR_RECORD_SLOT: &str = "[[BlitzSyncIteratorRecord]]";
const SYNC_ITERATOR_SLOT: &str = "[[BlitzSyncIterator]]";
const SYNC_ITERATOR_NEXT_SLOT: &str = "[[BlitzSyncIteratorNext]]";
const SYNC_ITERATOR_DONE_SLOT: &str = "[[BlitzSyncIteratorDone]]";

#[derive(Clone)]
struct SyncIteratorRecord {
  wrapper: Rc<RefCell<ObjectData>>,
  iterator: Value,
  next: Value,
  done: bool,
}

fn sync_iterator_record(value: &Value) -> Option<SyncIteratorRecord> {
  let Value::Object(wrapper) = value else {
    return None;
  };
  let wrapper_ref = wrapper.borrow();
  if !matches!(
    wrapper_ref.internal_slot(SYNC_ITERATOR_RECORD_SLOT),
    Some(Value::Bool(true))
  ) {
    return None;
  }
  Some(SyncIteratorRecord {
    wrapper: Rc::clone(wrapper),
    iterator: wrapper_ref
      .internal_slot(SYNC_ITERATOR_SLOT)?
      .clone(),
    next: wrapper_ref
      .internal_slot(SYNC_ITERATOR_NEXT_SLOT)?
      .clone(),
    done: matches!(
      wrapper_ref.internal_slot(SYNC_ITERATOR_DONE_SLOT),
      Some(Value::Bool(true))
    ),
  })
}

fn mark_sync_iterator_done(
  wrapper: &Rc<RefCell<ObjectData>>,
  done: bool,
) {
  wrapper
    .borrow_mut()
    .set_internal_slot(SYNC_ITERATOR_DONE_SLOT, Value::Bool(done));
}

#[derive(Clone)]
struct AsyncIteratorRecord {
  wrapper: Rc<RefCell<ObjectData>>,
  iterator: Value,
  next: Value,
  sync: bool,
  done: bool,
}

fn async_iterator_record(value: &Value) -> Option<AsyncIteratorRecord> {
  let Value::Object(wrapper) = value else {
    return None;
  };
  let wrapper_ref = wrapper.borrow();
  if !matches!(
    wrapper_ref.internal_slot(ASYNC_ITERATOR_RECORD_SLOT),
    Some(Value::Bool(true))
  ) {
    return None;
  }
  Some(AsyncIteratorRecord {
    wrapper: Rc::clone(wrapper),
    iterator: wrapper_ref
      .internal_slot(ASYNC_ITERATOR_SLOT)?
      .clone(),
    next: wrapper_ref
      .internal_slot(ASYNC_ITERATOR_NEXT_SLOT)?
      .clone(),
    sync: matches!(
      wrapper_ref.internal_slot(ASYNC_ITERATOR_SYNC_SLOT),
      Some(Value::Bool(true))
    ),
    done: matches!(
      wrapper_ref.internal_slot(ASYNC_ITERATOR_DONE_SLOT),
      Some(Value::Bool(true))
    ),
  })
}

fn mark_async_iterator_done(wrapper: &Rc<RefCell<ObjectData>>) {
  wrapper
    .borrow_mut()
    .set_internal_slot(ASYNC_ITERATOR_DONE_SLOT, Value::Bool(true));
}

impl Vm {
  pub(super) fn begin_async_iterator_next(
    &mut self,
    iterator: &Value,
    destination: usize,
  ) -> Result<Option<PendingOperationStep>, RuntimeError> {
    let Some(record) = async_iterator_record(iterator) else {
      return Ok(None);
    };
    let result = if record.sync {
      let result = self
        .capture_completion(|vm| vm.call_value_with_this(&record.next, &record.iterator, &[]))
        .map_err(|(error, thrown)| {
          thrown.unwrap_or_else(|| Value::String(error.to_string().into()))
        });
      self.async_from_sync_continuation(&record, result, true)
    } else {
      self.call_value_with_this(&record.next, &record.iterator, &[])?
    };
    Ok(Some(PendingOperationStep::Await {
      value: result,
      operation: PendingOperation::NextNative {
        destination,
        record: iterator.clone(),
      },
    }))
  }

  /// AsyncFromSyncIteratorContinuation has its own promise reaction before the
  /// consuming for-await loop awaits the resulting iterator-result promise.
  fn async_from_sync_continuation(
    &mut self,
    record: &AsyncIteratorRecord,
    result: Result<Value, Value>,
    close_on_rejection: bool,
  ) -> Value {
    let promise_value = self.create_host_promise();
    let Value::Promise(promise, _) = &promise_value else {
      unreachable!("intrinsic promise")
    };
    let result = result.and_then(|result| {
      self
        .capture_completion(|vm| {
          if !result.is_object() {
            return Err(vm.throw_type_error("Iterator result must be an object"));
          }
          let done = vm.get_property_host(&result, &PropertyKey::String("done".into()))?;
          let done = vm.to_boolean(&done);
          let value = vm.get_property_host(&result, &PropertyKey::String("value".into()))?;
          Ok((done, value))
        })
        .map_err(|(error, thrown)| {
          thrown.unwrap_or_else(|| Value::String(error.to_string().into()))
        })
    });
    let (done, value) = match result {
      Ok(result) => result,
      Err(reason) => {
        crate::runtime_utils::reject_pending_promise(self, promise, reason);
        return promise_value;
      }
    };
    let fulfilled = Rc::clone(promise);
    let on_fulfilled = Value::Builtin(Rc::new(move |vm, args| {
      let offset = usize::from(vm.builtin_this_arg_prepended());
      let value = args
        .get(offset)
        .cloned()
        .unwrap_or(Value::Undefined);
      let result = vm.make_iterator_result(value, done);
      crate::runtime_utils::resolve_promise_with_value(vm, &fulfilled, result)?;
      Ok(Value::Undefined)
    }));
    let rejected = Rc::clone(promise);
    let iterator = record.iterator.clone();
    let wrapper = Rc::clone(&record.wrapper);
    let on_rejected = Value::Builtin(Rc::new(move |vm, args| {
      let offset = usize::from(vm.builtin_this_arg_prepended());
      let reason = args
        .get(offset)
        .cloned()
        .unwrap_or(Value::Undefined);
      if close_on_rejection && !done {
        mark_async_iterator_done(&wrapper);
        drop(vm.close_iterator_in_fork(&iterator));
      }
      crate::runtime_utils::reject_pending_promise(vm, &rejected, reason);
      Ok(Value::Undefined)
    }));
    if let Err((error, thrown)) =
      self.capture_completion(|vm| vm.observe_await(value, on_fulfilled, on_rejected))
    {
      let reason = thrown.unwrap_or_else(|| Value::String(error.to_string().into()));
      if close_on_rejection && !done {
        mark_async_iterator_done(&record.wrapper);
        drop(self.close_iterator_in_fork(&record.iterator));
      }
      crate::runtime_utils::reject_pending_promise(self, promise, reason);
    }
    promise_value
  }

  pub(super) fn begin_async_iterator_close(
    &mut self,
    iterator: &Value,
    destination: usize,
  ) -> Result<Option<PendingOperationStep>, RuntimeError> {
    let Some(record) = async_iterator_record(iterator) else {
      return Ok(None);
    };
    if record.done {
      return Ok(Some(PendingOperationStep::Complete {
        destination,
        value: iterator.clone(),
      }));
    }
    mark_async_iterator_done(&record.wrapper);
    if record.sync {
      let result = self
        .capture_completion(|vm| {
          let return_fn =
            vm.get_property_host(&record.iterator, &PropertyKey::String("return".into()))?;
          if matches!(return_fn, Value::Undefined | Value::Null) {
            return Ok(None);
          }
          if !Self::is_callable_value(&return_fn) {
            return Err(vm.throw_type_error("Iterator return is not callable"));
          }
          vm.call_value_with_this(&return_fn, &record.iterator, &[])
            .map(Some)
        })
        .map_err(|(error, thrown)| {
          thrown.unwrap_or_else(|| Value::String(error.to_string().into()))
        });
      let value = match result {
        Ok(None) => {
          // An absent sync return method resolves its adapter promise directly;
          // only a supplied return result passes through the value continuation.
          let promise = self.create_host_promise();
          let result = self.make_iterator_result(Value::Undefined, true);
          self.resolve_host_promise(&promise, result)?;
          promise
        }
        result => self.async_from_sync_continuation(
          &record,
          result.map(|value| value.expect("present return result")),
          false,
        ),
      };
      return Ok(Some(PendingOperationStep::Await {
        value,
        operation: PendingOperation::CloseNative {
          destination,
          record: iterator.clone(),
        },
      }));
    }
    let return_fn = self.get_property_host(
      &record.iterator,
      &PropertyKey::String("return".to_string().into()),
    )?;
    if matches!(return_fn, Value::Undefined | Value::Null) {
      return Ok(Some(PendingOperationStep::Complete {
        destination,
        value: iterator.clone(),
      }));
    }
    if !Self::is_callable_value(&return_fn) {
      return Err(self.throw_type_error("Iterator return is not callable"));
    }
    let result = self.call_value_with_this(&return_fn, &record.iterator, &[])?;
    Ok(Some(PendingOperationStep::Await {
      value: result,
      operation: PendingOperation::CloseNative {
        destination,
        record: iterator.clone(),
      },
    }))
  }

  pub(super) fn resume_pending_iterator_operation(
    &mut self,
    operation: PendingOperation,
    settlement: AwaitSettlement,
  ) -> Result<PendingOperationStep, RuntimeError> {
    if let PendingOperation::YieldStar { destination } = operation {
      return match settlement {
        AwaitSettlement::Fulfilled(value) => {
          let state = self
            .execution
            .active
            .yield_star
            .as_mut()
            .ok_or_else(|| RuntimeError::from("Missing async yield-star state"))?;
          state.settled = Some(value);
          Ok(PendingOperationStep::Complete {
            destination,
            value: Value::Undefined,
          })
        }
        AwaitSettlement::Rejected(reason) => {
          if let Some(state) = self.execution.active.yield_star.take()
            && matches!(
              state.phase,
              YieldStarPhase::SyncValue {
                done: false,
                is_return: false
              }
            )
          {
            drop(self.close_iterator_in_fork(&state.iterator));
          }
          Ok(PendingOperationStep::Throw(reason))
        }
      };
    }
    match (operation, settlement) {
      (PendingOperation::YieldStar { .. }, _) => unreachable!("handled above"),
      (_, AwaitSettlement::Rejected(reason)) => Ok(PendingOperationStep::Throw(reason)),
      (
        PendingOperation::NextNative {
          destination,
          record,
        },
        AwaitSettlement::Fulfilled(result),
      ) => {
        if !result.is_object() {
          return Err(self.throw_type_error("Async iterator result must be an object"));
        }
        let done =
          self.get_property_host(&result, &PropertyKey::String("done".to_string().into()))?;
        let done = self.to_boolean(&done);
        let value = if done {
          Value::Undefined
        } else {
          self.get_property_host(&result, &PropertyKey::String("value".to_string().into()))?
        };
        if done && let Some(record) = async_iterator_record(&record) {
          mark_async_iterator_done(&record.wrapper);
        }
        Ok(PendingOperationStep::Complete {
          destination,
          value: self.make_iterator_result(value, done),
        })
      }
      (
        PendingOperation::CloseNative {
          destination,
          record,
        },
        AwaitSettlement::Fulfilled(result),
      ) => {
        if !result.is_object() {
          return Err(self.throw_type_error("Async iterator return result is not an object"));
        }
        Ok(PendingOperationStep::Complete {
          destination,
          value: record,
        })
      }
    }
  }

  pub(crate) fn get_sync_iterator(
    &mut self,
    iterable: Value,
  ) -> Result<Value, RuntimeError> {
    let method = crate::runtime_utils::get_method(
      self,
      &iterable,
      &PropertyKey::Symbol(SymbolId::well_known_iterator()),
    )?
    .ok_or_else(|| self.throw_type_error("Value is not iterable"))?;
    let (iterator, next) = crate::runtime_utils::get_iterator_record(self, &iterable, &method)?;
    let mut wrapper = ObjectData::new();
    wrapper.set_internal_slot(SYNC_ITERATOR_RECORD_SLOT, Value::Bool(true));
    wrapper.set_internal_slot(SYNC_ITERATOR_SLOT, iterator);
    wrapper.set_internal_slot(SYNC_ITERATOR_NEXT_SLOT, next);
    wrapper.set_internal_slot(SYNC_ITERATOR_DONE_SLOT, Value::Bool(false));
    Ok(Value::Object(Rc::new(RefCell::new(wrapper))))
  }

  pub(super) fn get_async_iterator(
    &mut self,
    iterable: Value,
  ) -> Result<Value, RuntimeError> {
    let async_method = crate::runtime_utils::get_method(
      self,
      &iterable,
      &PropertyKey::Symbol(SymbolId::well_known_async_iterator()),
    )?;
    let (iterator, next, sync) = if let Some(method) = async_method {
      let (iterator, next) = crate::runtime_utils::get_iterator_record(self, &iterable, &method)?;
      (iterator, next, false)
    } else {
      let sync_method = crate::runtime_utils::get_method(
        self,
        &iterable,
        &PropertyKey::Symbol(SymbolId::well_known_iterator()),
      )?
      .ok_or_else(|| self.throw_type_error("Value is not async iterable"))?;
      let (iterator, next) =
        crate::runtime_utils::get_iterator_record(self, &iterable, &sync_method)?;
      (iterator, next, true)
    };

    let mut wrapper = ObjectData::new();
    wrapper.set_internal_slot(ASYNC_ITERATOR_RECORD_SLOT, Value::Bool(true));
    wrapper.set_internal_slot(ASYNC_ITERATOR_SLOT, iterator);
    wrapper.set_internal_slot(ASYNC_ITERATOR_NEXT_SLOT, next);
    wrapper.set_internal_slot(ASYNC_ITERATOR_SYNC_SLOT, Value::Bool(sync));
    wrapper.set_internal_slot(ASYNC_ITERATOR_DONE_SLOT, Value::Bool(false));
    Ok(Value::Object(Rc::new(RefCell::new(wrapper))))
  }

  pub(crate) fn iterator_next(
    &mut self,
    iterator: Value,
  ) -> Result<Value, RuntimeError> {
    if let Some(record) = sync_iterator_record(&iterator) {
      mark_sync_iterator_done(&record.wrapper, true);
      let result = self.call_value_with_this(&record.next, &record.iterator, &[])?;
      if !result.is_object() {
        return Err(self.throw_type_error("Iterator result must be an object"));
      }
      mark_sync_iterator_done(&record.wrapper, false);
      return Ok(result);
    }
    if let Some(record) = async_iterator_record(&iterator) {
      let result = self.call_value_with_this(&record.next, &record.iterator, &[])?;
      if record.sync {
        if !result.is_object() {
          return Err(self.throw_type_error("Iterator result must be an object"));
        }
        let done =
          self.get_property_host(&result, &PropertyKey::String("done".to_string().into()))?;
        let done = self.to_boolean(&done);
        let value =
          self.get_property_host(&result, &PropertyKey::String("value".to_string().into()))?;
        let value = match self.await_value_or_throw(value) {
          Ok(value) => value,
          Err(error) => {
            if !done {
              mark_async_iterator_done(&record.wrapper);
              drop(self.close_iterator_in_fork(&record.iterator));
            }
            return Err(error);
          }
        };
        if done {
          mark_async_iterator_done(&record.wrapper);
        }
        return Ok(self.make_iterator_result(value, done));
      }

      let result = self.await_value_or_throw(result)?;
      if !result.is_object() {
        return Err(self.throw_type_error("Async iterator result must be an object"));
      }
      let done =
        self.get_property_host(&result, &PropertyKey::String("done".to_string().into()))?;
      if self.to_boolean(&done) {
        mark_async_iterator_done(&record.wrapper);
      }
      return Ok(result);
    }

    let next_fn =
      self.get_property_host(&iterator, &PropertyKey::String("next".to_string().into()))?;
    let result = self.call_value_with_this(&next_fn, &iterator, &[])?;
    if !result.is_object() {
      return Err(self.throw_type_error("Iterator result must be an object"));
    }
    let done = self.get_property_host(&result, &PropertyKey::String("done".to_string().into()))?;
    if self.to_boolean(&done) {
      let done_key = PropertyKey::String("__tsblitz_iter_done".to_string().into());
      match &iterator {
        Value::Object(obj) => {
          obj.borrow_mut().properties.insert(
            done_key,
            PropertyDescriptor::ordinary_data(Value::Bool(true)),
          );
        }
        Value::Array(_, props) | Value::Closure { props, .. } | Value::BuiltinObject(_, props) => {
          props.borrow_mut().properties.insert(
            done_key,
            PropertyDescriptor::ordinary_data(Value::Bool(true)),
          );
        }
        _ => {}
      }
    }
    Ok(result)
  }

  pub(super) fn create_yield_star_state(
    &mut self,
    iterable: Value,
    is_async: bool,
  ) -> Result<YieldStarState, RuntimeError> {
    let mut sync = !is_async;
    let mut iterator_method = if is_async {
      self.get_property_host(
        &iterable,
        &PropertyKey::Symbol(SymbolId::well_known_async_iterator()),
      )?
    } else {
      Value::Undefined
    };
    if matches!(iterator_method, Value::Undefined | Value::Null) {
      sync = true;
      iterator_method = self.get_property_host(
        &iterable,
        &PropertyKey::Symbol(SymbolId::well_known_iterator()),
      )?;
    }
    if !Self::is_callable_value(&iterator_method) {
      return Err(self.throw_type_error("Yield-star value is not iterable"));
    }
    let iterator = self.call_value_with_this(&iterator_method, &iterable, &[])?;
    if !iterator.is_object() {
      return Err(self.throw_type_error("Iterator method must return an object"));
    }
    let next_method = self.get_property_host(&iterator, &PropertyKey::String("next".into()))?;
    Ok(YieldStarState {
      iterator,
      next_method,
      resume: Some(YieldStarResume::Next(Value::Undefined)),
      is_async,
      sync,
      phase: YieldStarPhase::Resume,
      settled: None,
    })
  }

  pub(crate) fn begin_generator_return(
    &mut self,
    value: Value,
  ) -> bool {
    let Some(handler_index) = self
      .exceptions
      .handlers
      .iter()
      .rposition(|handler| {
        handler.frame_count == self.execution.call_stack.len()
          && Arc::ptr_eq(&handler.chunk, &self.execution.active.chunk)
          && handler.finally.is_some()
      })
    else {
      return false;
    };
    let handler = self.exceptions.handlers.remove(handler_index);
    self.exceptions.handlers.truncate(handler_index);
    let finally = handler.finally.expect("checked above");
    ensure_slot(&mut self.execution.active.locals, finally.return_slot);
    ensure_slot(&mut self.execution.active.locals, finally.did_return_slot);
    self.execution.active.locals[finally.return_slot].set(value);
    self.execution.active.locals[finally.did_return_slot].set(Value::Bool(true));
    self.execution.active.lexical_env = handler.saved_lexical_env;
    self.execution.active.pc = finally.finalizer_pc;
    self.execution.active.yield_star = None;
    self
      .execution
      .generator_return_frame_count
      .get_or_insert(self.execution.call_stack.len());
    true
  }

  pub(super) fn resume_yield_star(&mut self) -> Result<YieldStarStep, RuntimeError> {
    let adapter = self
      .execution
      .active
      .yield_star
      .as_ref()
      .filter(|state| state.is_async && state.sync && matches!(state.phase, YieldStarPhase::Resume))
      .cloned();
    if let Some(mut state) = adapter {
      return match self.capture_completion(Self::resume_yield_star_inner) {
        Ok(step) => Ok(step),
        Err((error, None)) => Err(error),
        Err((_, Some(reason))) => {
          let promise = self.create_host_promise();
          let Value::Promise(ref pending, _) = promise else {
            unreachable!("intrinsic Promise")
          };
          crate::runtime_utils::reject_pending_promise(self, pending, reason);
          state.phase = YieldStarPhase::Result {
            is_return: false,
            missing_throw: false,
          };
          self.execution.active.yield_star = Some(state);
          Ok(YieldStarStep::Await(promise))
        }
      };
    }
    self.resume_yield_star_inner()
  }

  fn resume_yield_star_inner(&mut self) -> Result<YieldStarStep, RuntimeError> {
    let mut state = self
      .execution
      .active
      .yield_star
      .take()
      .ok_or_else(|| RuntimeError::from("Missing yield-star state"))?;
    if let Some(result) = state.settled.take() {
      return match state.phase {
        YieldStarPhase::Result {
          is_return,
          missing_throw,
        } => {
          if missing_throw {
            if !result.is_object() {
              return Err(self.throw_type_error("Iterator return method must return an object"));
            }
            Err(self.throw_type_error("Delegated iterator has no throw method"))
          } else {
            self.finish_yield_star_result(state, result, is_return, false)
          }
        }
        YieldStarPhase::SyncValue { done, is_return } => {
          let result = self.make_iterator_result(result, done);
          state.phase = YieldStarPhase::Result {
            is_return,
            missing_throw: false,
          };
          self.execution.active.yield_star = Some(state);
          Ok(YieldStarStep::Await(result))
        }
        YieldStarPhase::ReturnValue => Ok(YieldStarStep::Return(result)),
        YieldStarPhase::Resume => Err(RuntimeError::from("Unexpected yield-star settlement")),
      };
    }
    let resume = state
      .resume
      .take()
      .ok_or_else(|| RuntimeError::from("Missing yield-star resume operation"))?;
    let is_return = matches!(resume, YieldStarResume::Return(_));

    let result = match resume {
      YieldStarResume::Next(value) => {
        self.call_value_with_this(&state.next_method, &state.iterator, &[value])?
      }
      YieldStarResume::Throw(value) => {
        let throw_method = self.get_property_host(
          &state.iterator,
          &PropertyKey::String("throw".to_string().into()),
        )?;
        if matches!(throw_method, Value::Undefined | Value::Null) {
          let return_method = self.get_property_host(
            &state.iterator,
            &PropertyKey::String("return".to_string().into()),
          )?;
          if !matches!(return_method, Value::Undefined | Value::Null) {
            if !Self::is_callable_value(&return_method) {
              return Err(self.throw_type_error("Iterator return method is not callable"));
            }
            let close_result = self.call_value_with_this(&return_method, &state.iterator, &[])?;
            if state.is_async && !state.sync {
              state.phase = YieldStarPhase::Result {
                is_return: false,
                missing_throw: true,
              };
              self.execution.active.yield_star = Some(state);
              return Ok(YieldStarStep::Await(close_result));
            }
            if !close_result.is_object() {
              return Err(self.throw_type_error("Iterator return method must return an object"));
            }
          }
          return Err(self.throw_type_error("Delegated iterator has no throw method"));
        }
        if !Self::is_callable_value(&throw_method) {
          return Err(self.throw_type_error("Iterator throw method is not callable"));
        }
        self.call_value_with_this(&throw_method, &state.iterator, &[value])?
      }
      YieldStarResume::Return(value) => {
        let return_method = self.get_property_host(
          &state.iterator,
          &PropertyKey::String("return".to_string().into()),
        )?;
        if matches!(return_method, Value::Undefined | Value::Null) {
          if state.is_async {
            let awaited = if state.sync {
              state.phase = YieldStarPhase::Result {
                is_return: true,
                missing_throw: false,
              };
              self.make_iterator_result(value, true)
            } else {
              state.phase = YieldStarPhase::ReturnValue;
              value
            };
            self.execution.active.yield_star = Some(state);
            return Ok(YieldStarStep::Await(awaited));
          }
          return Ok(YieldStarStep::Return(value));
        }
        if !Self::is_callable_value(&return_method) {
          return Err(self.throw_type_error("Iterator return method is not callable"));
        }
        self.call_value_with_this(&return_method, &state.iterator, &[value])?
      }
    };

    if state.is_async && !state.sync {
      state.phase = YieldStarPhase::Result {
        is_return,
        missing_throw: false,
      };
      self.execution.active.yield_star = Some(state);
      return Ok(YieldStarStep::Await(result));
    }
    self.finish_yield_star_result(state, result, is_return, true)
  }

  fn finish_yield_star_result(
    &mut self,
    mut state: YieldStarState,
    result: Value,
    is_return: bool,
    unwrap_sync: bool,
  ) -> Result<YieldStarStep, RuntimeError> {
    if !result.is_object() {
      return Err(self.throw_type_error("Iterator result must be an object"));
    }
    let done = self.get_property_host(&result, &PropertyKey::String("done".into()))?;
    let done = self.to_boolean(&done);
    if !state.is_async && !done {
      self.execution.active.yield_star = Some(state);
      return Ok(YieldStarStep::Yield(result));
    }
    let value = self.get_property_host(&result, &PropertyKey::String("value".into()))?;
    if state.is_async && state.sync && unwrap_sync {
      state.phase = YieldStarPhase::SyncValue { done, is_return };
      self.execution.active.yield_star = Some(state);
      return Ok(YieldStarStep::Await(value));
    }
    self.finish_yield_star_value(state, value, done, is_return)
  }

  fn finish_yield_star_value(
    &mut self,
    mut state: YieldStarState,
    value: Value,
    done: bool,
    is_return: bool,
  ) -> Result<YieldStarStep, RuntimeError> {
    if done {
      if is_return {
        Ok(YieldStarStep::Return(value))
      } else {
        Ok(YieldStarStep::Complete(value))
      }
    } else {
      state.phase = YieldStarPhase::Resume;
      self.execution.active.yield_star = Some(state);
      Ok(YieldStarStep::Yield(
        self.make_iterator_result(value, false),
      ))
    }
  }

  pub(super) fn iterator_close(
    &mut self,
    iterator: Value,
  ) -> Result<Value, RuntimeError> {
    if let Some(record) = sync_iterator_record(&iterator) {
      if record.done {
        return Ok(iterator);
      }
      mark_sync_iterator_done(&record.wrapper, true);
      let return_fn = self.get_property_host(
        &record.iterator,
        &PropertyKey::String("return".to_string().into()),
      )?;
      if matches!(return_fn, Value::Undefined | Value::Null) {
        return Ok(iterator);
      }
      if !Self::is_callable_value(&return_fn) {
        return Err(self.throw_type_error("Iterator return is not callable"));
      }
      let result = self.call_value_with_this(&return_fn, &record.iterator, &[])?;
      if !result.is_object() {
        return Err(self.throw_type_error("Iterator return result is not an object"));
      }
      return Ok(iterator);
    }
    if let Some(record) = async_iterator_record(&iterator) {
      if record.done {
        return Ok(iterator);
      }
      mark_async_iterator_done(&record.wrapper);
      let return_fn = self.get_property_host(
        &record.iterator,
        &PropertyKey::String("return".to_string().into()),
      )?;
      if matches!(return_fn, Value::Undefined | Value::Null) {
        return Ok(iterator);
      }
      if !Self::is_callable_value(&return_fn) {
        return Err(self.throw_type_error("Iterator return is not callable"));
      }
      let result = self.call_value_with_this(&return_fn, &record.iterator, &[])?;
      if record.sync {
        if !result.is_object() {
          return Err(self.throw_type_error("Iterator return result is not an object"));
        }
        let done =
          self.get_property_host(&result, &PropertyKey::String("done".to_string().into()))?;
        let value =
          self.get_property_host(&result, &PropertyKey::String("value".to_string().into()))?;
        drop(self.await_value_or_throw(value)?);
        let _ = self.to_boolean(&done);
        return Ok(iterator);
      }
      let result = self.await_value_or_throw(result)?;
      if !result.is_object() {
        return Err(self.throw_type_error("Async iterator return result is not an object"));
      }
      return Ok(iterator);
    }

    if matches!(iterator, Value::Undefined | Value::Null) {
      return Ok(iterator);
    }
    let done_key = PropertyKey::String("__tsblitz_iter_done".to_string().into());
    let already_done = match &iterator {
      Value::Object(obj) => obj
        .borrow()
        .properties
        .get(&done_key)
        .is_some_and(|desc| {
          matches!(
            desc,
            PropertyDescriptor::Data {
              value: Value::Bool(true),
              ..
            }
          )
        }),
      Value::Array(_, props) | Value::Closure { props, .. } | Value::BuiltinObject(_, props) => {
        props
          .borrow()
          .properties
          .get(&done_key)
          .is_some_and(|desc| {
            matches!(
              desc,
              PropertyDescriptor::Data {
                value: Value::Bool(true),
                ..
              }
            )
          })
      }
      _ => false,
    };
    if already_done {
      return Ok(iterator);
    }
    let return_fn =
      self.get_property_host(&iterator, &PropertyKey::String("return".to_string().into()))?;
    if matches!(return_fn, Value::Undefined | Value::Null) {
      return Ok(iterator);
    }
    if !Self::is_callable_value(&return_fn) {
      return Err(self.throw_type_error("Iterator return is not callable"));
    }
    let ret = self.call_value_with_this(&return_fn, &iterator, &[])?;
    if !ret.is_object() {
      return Err(self.throw_type_error("Iterator return result is not an object"));
    }
    Ok(iterator)
  }
}

#[cfg(test)]
mod async_delegation_tests;
