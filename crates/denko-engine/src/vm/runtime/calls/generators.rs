// SPDX-License-Identifier: MPL-2.0

use crate::bytecode::FunctionProto;
use crate::errors::RuntimeError;
use crate::gc;
use crate::value::{ObjectData, PropertyDescriptor, PropertyKey, SharedValue, SymbolId, Value};
use crate::vm::Vm;
use crate::vm::state::{GeneratorRunner, RiscExecutionState, RiscRunOutcome};
use std::cell::RefCell;
use std::rc::Rc;

impl Vm {
  fn propagate_generator_error(
    &mut self,
    runner: &mut GeneratorRunner,
    fallback: RuntimeError,
  ) -> RuntimeError {
    let Some(thrown) = runner.vm.exceptions.last_uncaught.take() else {
      return fallback;
    };
    self
      .throw_value(thrown)
      .err()
      .unwrap_or_else(|| RuntimeError::from("JS exception"))
  }

  pub(in crate::vm::runtime) fn create_generator_object(
    &mut self,
    proto: &Rc<FunctionProto>,
    closure_env: &Rc<RefCell<Vec<SharedValue>>>,
    call_args: Vec<Value>,
    this_value: Value,
    callee: &Value,
  ) -> Result<Value, RuntimeError> {
    let mut child = self.fork_execution();
    child.push_call_frame(proto, closure_env, call_args, this_value, callee.clone())?;
    let mut risc_state = RiscExecutionState::new(
      child.execution.active.this_value.clone(),
      child.execution.active.chunk.risc_spill_count as usize,
    );
    let initialization = match child.run_risc_state(&mut risc_state, true) {
      Ok(outcome) => outcome,
      Err(error) => {
        let Some(thrown) = child.exceptions.last_uncaught.take() else {
          return Err(error);
        };
        return match self.throw_value(thrown) {
          Err(error) => Err(error),
          Ok(()) => Err(RuntimeError::from(
            "generator initialization exception did not remain abrupt",
          )),
        };
      }
    };
    match initialization {
      RiscRunOutcome::Initialized => {}
      RiscRunOutcome::Return(_) | RiscRunOutcome::Yield(_) | RiscRunOutcome::YieldStar(_) => {
        return Err(RuntimeError::from(
          "generator body reached a suspension before initialization",
        ));
      }
    }
    let runner = Rc::new(RefCell::new(GeneratorRunner {
      vm: child,
      risc_state,
      started: false,
      completed: false,
    }));
    let next_runner = Rc::clone(&runner);
    let throw_runner = Rc::clone(&runner);
    let return_runner = runner;
    let gen_obj = gc::track_object(Rc::new(RefCell::new(ObjectData::new())));
    gen_obj
      .borrow_mut()
      .set_prototype_value(self.realm_state.realm.generator_prototype());
    gen_obj.borrow_mut().properties.insert(
      PropertyKey::String("next".to_string().into()),
      PropertyDescriptor::ordinary_data(Value::Builtin(Rc::new(move |vm, args| {
        let mut runner = next_runner
          .try_borrow_mut()
          .map_err(|_| RuntimeError::from("TypeError: generator is already executing"))?;
        let user_args = if vm.builtin_this_arg_prepended() {
          args.get(1..).unwrap_or_default()
        } else {
          args
        };
        let first_arg = user_args
          .first()
          .cloned()
          .unwrap_or(Value::Undefined);

        if runner.completed {
          Ok(Self::make_iterator_result(Value::Undefined, true))
        } else {
          if !runner.started {
            runner.started = true;
          } else {
            runner.resume_next(first_arg);
          }

          let outcome = {
            let GeneratorRunner { vm, risc_state, .. } = &mut *runner;
            vm.run_risc_state(risc_state, false)
          };
          match outcome {
            Ok(RiscRunOutcome::Yield(value)) => Ok(Self::make_iterator_result(value, false)),
            Ok(RiscRunOutcome::YieldStar(result)) => Ok(result),
            Ok(RiscRunOutcome::Return(value)) => {
              runner.completed = true;
              Ok(Self::make_iterator_result(value, true))
            }
            Ok(RiscRunOutcome::Initialized) => Err(RuntimeError::from(
              "generator initialization marker executed twice",
            )),
            Err(error) => {
              runner.completed = true;
              Err(vm.propagate_generator_error(&mut runner, error))
            }
          }
        }
      }))),
    );
    gen_obj.borrow_mut().properties.insert(
      PropertyKey::String("throw".to_string().into()),
      PropertyDescriptor::ordinary_data(Value::Builtin(Rc::new(move |vm, args| {
        let mut runner = throw_runner
          .try_borrow_mut()
          .map_err(|_| RuntimeError::from("TypeError: generator is already executing"))?;
        let user_args = if vm.builtin_this_arg_prepended() {
          args.get(1..).unwrap_or_default()
        } else {
          args
        };
        let thrown = user_args
          .first()
          .cloned()
          .unwrap_or(Value::Undefined);

        if runner.completed {
          Err(RuntimeError::from(format!(
            "Uncaught: {}",
            runner.vm.format_uncaught_thrown_value(&thrown)
          )))
        } else {
          let delegated = runner.resume_throw(thrown.clone());
          if !delegated && let Err(err) = runner.vm.throw_value(thrown) {
            runner.completed = true;
            return Err(vm.propagate_generator_error(&mut runner, err));
          }

          let outcome = {
            let GeneratorRunner {
              vm: generator_vm,
              risc_state,
              ..
            } = &mut *runner;
            generator_vm.run_risc_state(risc_state, false)
          };
          match outcome {
            Ok(RiscRunOutcome::Yield(value)) => Ok(Self::make_iterator_result(value, false)),
            Ok(RiscRunOutcome::YieldStar(result)) => Ok(result),
            Ok(RiscRunOutcome::Return(value)) => {
              runner.completed = true;
              Ok(Self::make_iterator_result(value, true))
            }
            Ok(RiscRunOutcome::Initialized) => Err(RuntimeError::from(
              "generator initialization marker executed twice",
            )),
            Err(error) => {
              runner.completed = true;
              Err(vm.propagate_generator_error(&mut runner, error))
            }
          }
        }
      }))),
    );
    gen_obj.borrow_mut().properties.insert(
      PropertyKey::String("return".to_string().into()),
      PropertyDescriptor::ordinary_data(Value::Builtin(Rc::new(move |vm, args| {
        let mut runner = return_runner
          .try_borrow_mut()
          .map_err(|_| RuntimeError::from("TypeError: generator is already executing"))?;
        let user_args = if vm.builtin_this_arg_prepended() {
          args.get(1..).unwrap_or_default()
        } else {
          args
        };
        let result_value = user_args
          .first()
          .cloned()
          .unwrap_or(Value::Undefined);
        if !runner.resume_return(result_value.clone()) {
          runner.completed = true;
          return Ok(Self::make_iterator_result(result_value, true));
        }

        let outcome = {
          let GeneratorRunner { vm, risc_state, .. } = &mut *runner;
          vm.run_risc_state(risc_state, false)
        };
        match outcome {
          Ok(RiscRunOutcome::Yield(value)) => Ok(Self::make_iterator_result(value, false)),
          Ok(RiscRunOutcome::YieldStar(result)) => Ok(result),
          Ok(RiscRunOutcome::Return(value)) => {
            runner.completed = true;
            Ok(Self::make_iterator_result(value, true))
          }
          Ok(RiscRunOutcome::Initialized) => Err(RuntimeError::from(
            "generator initialization marker executed twice",
          )),
          Err(error) => {
            runner.completed = true;
            Err(vm.propagate_generator_error(&mut runner, error))
          }
        }
      }))),
    );
    gen_obj.borrow_mut().properties.insert(
      PropertyKey::Symbol(SymbolId::well_known_iterator()),
      PropertyDescriptor::ordinary_data(Value::Builtin(Rc::new(|_vm, args| {
        Ok(args.first().cloned().unwrap_or(Value::Undefined))
      }))),
    );
    Ok(Value::Object(gen_obj))
  }
}
