// SPDX-License-Identifier: MPL-2.0

use crate::errors::RuntimeError;
use crate::value::{ObjectData, PropertyDescriptor, PropertyKey, SymbolId, Value};
use crate::vm::Vm;
use crate::vm::runtime::execution::{YieldStarStep, ensure_slot};
use crate::vm::state::{YieldStarResume, YieldStarState};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

const ASYNC_ITERATOR_RECORD_SLOT: &str = "[[BlitzAsyncIteratorRecord]]";
const ASYNC_ITERATOR_SLOT: &str = "[[BlitzAsyncIterator]]";
const ASYNC_ITERATOR_NEXT_SLOT: &str = "[[BlitzAsyncIteratorNext]]";
const ASYNC_ITERATOR_SYNC_SLOT: &str = "[[BlitzAsyncIteratorSync]]";
const ASYNC_ITERATOR_DONE_SLOT: &str = "[[BlitzAsyncIteratorDone]]";

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

    // This object is a VM-private iterator record. Its state lives in internal
    // slots so acquisition details cannot be observed or forged by JavaScript.
    let mut wrapper = ObjectData::new();
    wrapper.set_internal_slot(ASYNC_ITERATOR_RECORD_SLOT, Value::Bool(true));
    wrapper.set_internal_slot(ASYNC_ITERATOR_SLOT, iterator);
    wrapper.set_internal_slot(ASYNC_ITERATOR_NEXT_SLOT, next);
    wrapper.set_internal_slot(ASYNC_ITERATOR_SYNC_SLOT, Value::Bool(sync));
    wrapper.set_internal_slot(ASYNC_ITERATOR_DONE_SLOT, Value::Bool(false));
    Ok(Value::Object(Rc::new(RefCell::new(wrapper))))
  }

  pub(super) fn iterator_next(
    &mut self,
    iterator: Value,
  ) -> Result<Value, RuntimeError> {
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
              // AsyncFromSyncIteratorContinuation closes the synchronous
              // iterator when a yielded value rejects. The original rejection
              // wins over any close error.
              drop(self.close_iterator_in_fork(&record.iterator));
            }
            return Err(error);
          }
        };
        if done {
          mark_async_iterator_done(&record.wrapper);
        }
        return Ok(Self::make_iterator_result(value, done));
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
  ) -> Result<YieldStarState, RuntimeError> {
    let iterator_method = self.get_property_host(
      &iterable,
      &PropertyKey::Symbol(SymbolId::well_known_iterator()),
    )?;
    if !Self::is_callable_value(&iterator_method) {
      return Err(self.throw_type_error("Yield-star value is not iterable"));
    }

    let iterator = self.call_value_with_this(&iterator_method, &iterable, &[])?;
    if !iterator.is_object() {
      return Err(self.throw_type_error("Iterator method must return an object"));
    }

    let next_method =
      self.get_property_host(&iterator, &PropertyKey::String("next".to_string().into()))?;
    if !Self::is_callable_value(&next_method) {
      return Err(self.throw_type_error("Iterator next method is not callable"));
    }

    Ok(YieldStarState {
      iterator,
      next_method,
      resume: Some(YieldStarResume::Next(Value::Undefined)),
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
    *self.execution.active.locals[finally.return_slot].borrow_mut() = value;
    *self.execution.active.locals[finally.did_return_slot].borrow_mut() = Value::Bool(true);
    self.execution.active.lexical_env = handler.saved_lexical_env;
    self.execution.active.ip = finally.finalizer_ip;
    self.execution.active.yield_star = None;
    self
      .execution
      .generator_return_frame_count
      .get_or_insert(self.execution.call_stack.len());
    true
  }

  pub(super) fn resume_yield_star(&mut self) -> Result<YieldStarStep, RuntimeError> {
    let mut state = self
      .execution
      .active
      .yield_star
      .take()
      .ok_or_else(|| RuntimeError::from("Missing yield-star state"))?;
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
          return Ok(YieldStarStep::Return(value));
        }
        if !Self::is_callable_value(&return_method) {
          return Err(self.throw_type_error("Iterator return method is not callable"));
        }
        self.call_value_with_this(&return_method, &state.iterator, &[value])?
      }
    };

    if !result.is_object() {
      return Err(self.throw_type_error("Iterator result must be an object"));
    }
    let done = self.get_property_host(&result, &PropertyKey::String("done".to_string().into()))?;
    if self.to_boolean(&done) {
      let value =
        self.get_property_host(&result, &PropertyKey::String("value".to_string().into()))?;
      if is_return {
        Ok(YieldStarStep::Return(value))
      } else {
        Ok(YieldStarStep::Complete(value))
      }
    } else {
      self.execution.active.yield_star = Some(state);
      Ok(YieldStarStep::Yield(result))
    }
  }

  pub(super) fn iterator_close(
    &mut self,
    iterator: Value,
  ) -> Result<Value, RuntimeError> {
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
        // AsyncFromSyncIteratorContinuation observes both fields and awaits
        // the returned value, even though AsyncIteratorClose discards them.
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
