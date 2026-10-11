// SPDX-License-Identifier: MPL-2.0
use super::*;
use crate::errors::RuntimeError;
use crate::runtime_utils::{fulfill_pending_promise, reject_pending_promise};
use crate::value::PromiseState;
use std::rc::Weak;

type Record = Rc<RefCell<ModuleRecord>>;

#[derive(Default)]
pub(super) struct Evaluation {
  pub(super) dependencies: Vec<Weak<RefCell<ModuleRecord>>>,
  pub(super) has_tla: bool,
  ancestor: usize,
  cycle_root: Weak<RefCell<ModuleRecord>>,
  promise: Option<Value>,
  error: Option<Value>,
  async_order: Option<usize>,
  pending_dependencies: usize,
  parents: Vec<Weak<RefCell<ModuleRecord>>>,
}

static ASYNC_ORDER: AtomicUsize = AtomicUsize::new(0);

fn cycle_root(record: &Record) -> Record {
  record
    .borrow()
    .evaluation
    .cycle_root
    .upgrade()
    .unwrap_or_else(|| Rc::clone(record))
}

pub(super) fn evaluate(
  vm: &mut Vm,
  entry: &Record,
) -> Result<Value, RuntimeError> {
  let entry = cycle_root(entry);
  if let Some(promise) = entry.borrow().evaluation.promise.clone() {
    return Ok(promise);
  }
  let promise = vm.create_host_promise();
  entry.borrow_mut().evaluation.promise = Some(promise.clone());
  let mut stack = Vec::new();
  let mut index = 0;
  if let Err(reason) = inner_evaluate(vm, &entry, &mut stack, &mut index) {
    for record in stack {
      let mut record = record.borrow_mut();
      record.state = ModuleState::Evaluated;
      record.evaluation.error = Some(reason.clone());
    }
    reject_record(vm, &entry, reason);
  } else if entry.borrow().state == ModuleState::Evaluated {
    settle_success(vm, &entry);
  }
  Ok(promise)
}

fn inner_evaluate(
  vm: &mut Vm,
  record: &Record,
  stack: &mut Vec<Record>,
  index: &mut usize,
) -> Result<(), Value> {
  {
    let record = record.borrow();
    if matches!(
      record.state,
      ModuleState::Evaluated | ModuleState::EvaluatingAsync
    ) {
      return record
        .evaluation
        .error
        .clone()
        .map_or(Ok(()), Err);
    }
    if record.state == ModuleState::Evaluating {
      return Ok(());
    }
  }
  let module_index = *index;
  *index += 1;
  let dependencies = {
    let mut record = record.borrow_mut();
    record.state = ModuleState::Evaluating;
    record.evaluation.ancestor = module_index;
    record
      .evaluation
      .dependencies
      .iter()
      .filter_map(Weak::upgrade)
      .collect::<Vec<_>>()
  };
  stack.push(Rc::clone(record));
  for dependency in dependencies {
    inner_evaluate(vm, &dependency, stack, index)?;
    let dependency = if dependency.borrow().state == ModuleState::Evaluating {
      let ancestor = dependency.borrow().evaluation.ancestor;
      let mut record = record.borrow_mut();
      record.evaluation.ancestor = record.evaluation.ancestor.min(ancestor);
      drop(record);
      dependency
    } else {
      cycle_root(&dependency)
    };
    if let Some(reason) = dependency.borrow().evaluation.error.clone() {
      return Err(reason);
    }
    if dependency
      .borrow()
      .evaluation
      .async_order
      .is_some()
    {
      record
        .borrow_mut()
        .evaluation
        .pending_dependencies += 1;
      dependency
        .borrow_mut()
        .evaluation
        .parents
        .push(Rc::downgrade(record));
    }
  }
  let asynchronous = {
    let record = record.borrow();
    record.evaluation.has_tla || record.evaluation.pending_dependencies > 0
  };
  if asynchronous {
    record.borrow_mut().evaluation.async_order = Some(ASYNC_ORDER.fetch_add(1, Ordering::Relaxed));
    if record.borrow().evaluation.pending_dependencies == 0 {
      execute_async(vm, record)?;
    }
  } else {
    execute_sync(vm, record)?;
  }
  if record.borrow().evaluation.ancestor == module_index {
    loop {
      let member = stack.pop().expect("module occurs in DFS stack");
      let is_root = Rc::ptr_eq(&member, record);
      {
        let mut member = member.borrow_mut();
        member.state = if member.evaluation.async_order.is_some() {
          ModuleState::EvaluatingAsync
        } else {
          ModuleState::Evaluated
        };
        member.evaluation.cycle_root = Rc::downgrade(record);
      }
      if is_root {
        break;
      }
    }
  }
  Ok(())
}

fn start_body(
  vm: &mut Vm,
  record: &Record,
) -> Result<Value, Value> {
  let (code, specifier, host_initialized) = {
    let record = record.borrow();
    (
      record.bytecode.clone(),
      record.specifier.clone(),
      record.host_initialized,
    )
  };
  if host_initialized {
    let promise = vm.create_host_promise();
    vm.resolve_host_promise(&promise, Value::Undefined)
      .map_err(|error| Value::String(error.to_string().into()))?;
    return Ok(promise);
  }
  let code = code.ok_or_else(|| Value::String("module is missing compiled bytecode".into()))?;
  let previous = vm.current_module_specifier().map(str::to_owned);
  vm.set_current_module_specifier(Some(specifier));
  let result = vm.start_async_module(Arc::new(code));
  vm.set_current_module_specifier(previous);
  result.map_err(|error| {
    vm.uncaught_exception()
      .cloned()
      .unwrap_or_else(|| Value::String(error.to_string().into()))
  })
}

fn execute_sync(
  vm: &mut Vm,
  record: &Record,
) -> Result<(), Value> {
  let promise = start_body(vm, record)?;
  let Value::Promise(state, _) = promise else {
    unreachable!("module execution returns a promise")
  };
  let mut state = state.borrow_mut();
  match &mut *state {
    PromiseState::Fulfilled(_) => Ok(()),
    PromiseState::Rejected { reason, handled } => {
      *handled = true;
      Err(reason.clone())
    }
    PromiseState::Pending { .. } => Err(Value::String(
      "synchronous module unexpectedly suspended".into(),
    )),
  }
}

fn execute_async(
  vm: &mut Vm,
  record: &Record,
) -> Result<(), Value> {
  let promise = start_body(vm, record)?;
  let fulfilled = Rc::clone(record);
  let on_fulfilled = Value::Builtin(Rc::new(move |vm, _| {
    execution_fulfilled(vm, &fulfilled);
    Ok(Value::Undefined)
  }));
  let rejected = Rc::clone(record);
  let on_rejected = Value::Builtin(Rc::new(move |vm, args| {
    let offset = usize::from(vm.builtin_this_arg_prepended());
    let reason = args
      .get(offset)
      .cloned()
      .unwrap_or(Value::Undefined);
    reject_record(vm, &rejected, reason);
    Ok(Value::Undefined)
  }));
  observe(vm, &promise, on_fulfilled, on_rejected);
  Ok(())
}

fn settle_success(
  vm: &mut Vm,
  record: &Record,
) {
  let promise = {
    let mut record = record.borrow_mut();
    record.state = ModuleState::Evaluated;
    record.evaluation.async_order = None;
    record.evaluation.promise.clone()
  };
  if let Some(Value::Promise(promise, _)) = promise {
    fulfill_pending_promise(vm, &promise, Value::Undefined);
  }
}

fn gather_available(
  record: &Record,
  available: &mut Vec<Record>,
) {
  let parents = record.borrow().evaluation.parents.clone();
  for parent in parents
    .into_iter()
    .filter_map(|parent| parent.upgrade())
  {
    if available
      .iter()
      .any(|item| Rc::ptr_eq(item, &parent))
      || cycle_root(&parent)
        .borrow()
        .evaluation
        .error
        .is_some()
    {
      continue;
    }
    let ready = {
      let mut parent = parent.borrow_mut();
      if parent.state == ModuleState::Evaluated {
        continue;
      }
      parent.evaluation.pending_dependencies -= 1;
      parent.evaluation.pending_dependencies == 0
    };
    if ready {
      available.push(Rc::clone(&parent));
      if !parent.borrow().evaluation.has_tla {
        gather_available(&parent, available);
      }
    }
  }
}

fn execution_fulfilled(
  vm: &mut Vm,
  record: &Record,
) {
  if record.borrow().state == ModuleState::Evaluated {
    return;
  }
  settle_success(vm, record);
  let mut available = Vec::new();
  gather_available(record, &mut available);
  available.sort_by_key(|record| record.borrow().evaluation.async_order);
  for ancestor in available {
    if ancestor.borrow().state == ModuleState::Evaluated {
      continue;
    }
    if ancestor.borrow().evaluation.has_tla {
      if let Err(reason) = execute_async(vm, &ancestor) {
        reject_record(vm, &ancestor, reason);
      }
    } else {
      match execute_sync(vm, &ancestor) {
        Ok(()) => settle_success(vm, &ancestor),
        Err(reason) => reject_record(vm, &ancestor, reason),
      }
    }
  }
}

fn reject_record(
  vm: &mut Vm,
  record: &Record,
  reason: Value,
) {
  let (promise, parents) = {
    let mut record = record.borrow_mut();
    if record.state == ModuleState::Evaluated && record.evaluation.error.is_none() {
      return;
    }
    record.state = ModuleState::Evaluated;
    record.evaluation.async_order = None;
    record.evaluation.error = Some(reason.clone());
    (
      record.evaluation.promise.clone(),
      std::mem::take(&mut record.evaluation.parents),
    )
  };
  if let Some(Value::Promise(promise, _)) = promise {
    reject_pending_promise(vm, &promise, reason.clone());
  }
  for parent in parents
    .into_iter()
    .filter_map(|parent| parent.upgrade())
  {
    reject_record(vm, &parent, reason.clone());
  }
}

pub(crate) fn observe(
  vm: &mut Vm,
  promise: &Value,
  on_fulfilled: Value,
  on_rejected: Value,
) {
  let Value::Promise(state, _) = promise else {
    unreachable!("module evaluation returns a promise")
  };
  let settled = {
    let mut state = state.borrow_mut();
    match &mut *state {
      PromiseState::Pending {
        on_fulfilled: fulfilled,
        on_rejected: rejected,
      } => {
        fulfilled.push(on_fulfilled);
        rejected.push(on_rejected);
        None
      }
      PromiseState::Fulfilled(value) => Some((on_fulfilled, value.clone())),
      PromiseState::Rejected { reason, handled } => {
        *handled = true;
        Some((on_rejected, reason.clone()))
      }
    }
  };
  if let Some((callback, value)) = settled {
    vm.queue_microtask(move |vm| {
      vm.call_value_with_this(&callback, &Value::Undefined, std::slice::from_ref(&value))?;
      Ok(())
    });
  }
}

pub(super) fn finish_host_evaluation(
  vm: &mut Vm,
  promise: &Value,
) -> Result<(), RuntimeError> {
  vm.flush_microtasks()?;
  if let Value::Promise(state, _) = promise {
    let reason = {
      let mut state = state.borrow_mut();
      match &mut *state {
        PromiseState::Rejected { reason, handled } => {
          *handled = true;
          Some(reason.clone())
        }
        _ => None,
      }
    };
    if let Some(reason) = reason {
      return Err(vm.throw_exception(reason));
    }
  }
  Ok(())
}

impl ModuleLoader {
  pub(crate) fn append_evaluation_roots(
    &self,
    roots: &mut Vec<Value>,
  ) {
    for record in self.map.values() {
      let record = record.borrow();
      roots.extend(record.evaluation.promise.iter().cloned());
      roots.extend(record.evaluation.error.iter().cloned());
    }
  }
}
