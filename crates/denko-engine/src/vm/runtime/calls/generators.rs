// SPDX-License-Identifier: MPL-2.0

use crate::bytecode::Function;
use crate::errors::RuntimeError;
use crate::gc;
use crate::value::{ObjectData, Value};
use crate::vm::Vm;
use crate::vm::state::{GeneratorRunner, GeneratorStatus, RegisterState, RunOutcome};
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
    proto: &Rc<Function>,
    closure_env: &crate::closure_environment::ClosureEnvironment,
    call_args: Vec<Value>,
    this_value: Value,
    callee: &Value,
  ) -> Result<Value, RuntimeError> {
    let mut child = self.fork_execution();
    let register_arguments = call_args
      .iter()
      .take(crate::bytecode::abi::REGISTER_ARGUMENT_COUNT)
      .cloned()
      .collect::<Vec<_>>();
    child.push_call_frame(proto, closure_env, call_args, this_value, callee.clone())?;
    let mut register_state = RegisterState::new(
      child.execution.active.this_value.clone(),
      &register_arguments,
      child.execution.active.code.spill_slot_count as usize,
    );
    let initialization = match child.run_state(&mut register_state, true) {
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
      RunOutcome::Initialized => {}
      RunOutcome::Return(_)
      | RunOutcome::Yield(_)
      | RunOutcome::YieldStar(_)
      | RunOutcome::Await(_) => {
        return Err(RuntimeError::from(
          "generator body reached a suspension before initialization",
        ));
      }
    }
    let runner = Rc::new(RefCell::new(GeneratorRunner {
      vm: child,
      register_state,
      status: GeneratorStatus::SuspendedStart,
    }));
    let next_runner = Rc::clone(&runner);
    let throw_runner = Rc::clone(&runner);
    let return_runner = runner;
    let gen_obj = gc::track_object(Rc::new(RefCell::new(ObjectData::new())));
    gen_obj
      .borrow_mut()
      .set_internal_slot("[[GeneratorState]]", Value::Bool(true));
    let instance_prototype = self
      .get_property_host(
        callee,
        &crate::value::PropertyKey::String("prototype".into()),
      )
      .ok()
      .filter(Value::is_object)
      .unwrap_or_else(|| self.realm_state.realm.generator_prototype());
    gen_obj
      .borrow_mut()
      .set_prototype_value(instance_prototype);
    gen_obj.borrow_mut().set_internal_slot(
      "[[GeneratorNext]]",
      Value::Builtin(Rc::new(move |vm, args| {
        let mut runner = next_runner
          .try_borrow_mut()
          .map_err(|_| vm.throw_type_error("generator is already executing"))?;
        let user_args = if vm.builtin_this_arg_prepended() {
          args.get(1..).unwrap_or_default()
        } else {
          args
        };
        let first_arg = user_args
          .first()
          .cloned()
          .unwrap_or(Value::Undefined);

        if runner.status == GeneratorStatus::Completed {
          Ok(vm.make_iterator_result(Value::Undefined, true))
        } else {
          if runner.status == GeneratorStatus::SuspendedStart {
            runner.status = GeneratorStatus::SuspendedYield;
          } else {
            runner.resume_next(first_arg);
          }

          let outcome = {
            let GeneratorRunner {
              vm, register_state, ..
            } = &mut *runner;
            vm.run_state(register_state, false)
          };
          match outcome {
            Ok(RunOutcome::Yield(value)) => Ok(vm.make_iterator_result(value, false)),
            Ok(RunOutcome::YieldStar(result)) => Ok(result),
            Ok(RunOutcome::Return(value)) => {
              runner.complete();
              Ok(vm.make_iterator_result(value, true))
            }
            Ok(RunOutcome::Initialized) => Err(RuntimeError::from(
              "generator initialization marker executed twice",
            )),
            Ok(RunOutcome::Await(_)) => {
              Err(RuntimeError::from("await boundary reached in generator"))
            }
            Err(error) => {
              let error = vm.propagate_generator_error(&mut runner, error);
              runner.complete();
              Err(error)
            }
          }
        }
      })),
    );
    gen_obj.borrow_mut().set_internal_slot(
      "[[GeneratorThrow]]",
      Value::Builtin(Rc::new(move |vm, args| {
        let mut runner = throw_runner
          .try_borrow_mut()
          .map_err(|_| vm.throw_type_error("generator is already executing"))?;
        let user_args = if vm.builtin_this_arg_prepended() {
          args.get(1..).unwrap_or_default()
        } else {
          args
        };
        let thrown = user_args
          .first()
          .cloned()
          .unwrap_or(Value::Undefined);

        if runner.status == GeneratorStatus::Completed {
          Err(vm.throw_exception(thrown))
        } else {
          let delegated = runner.resume_throw(thrown.clone());
          if !delegated && let Err(err) = runner.vm.throw_value(thrown) {
            let error = vm.propagate_generator_error(&mut runner, err);
            runner.complete();
            return Err(error);
          }

          let outcome = {
            let GeneratorRunner {
              vm: generator_vm,
              register_state,
              ..
            } = &mut *runner;
            generator_vm.run_state(register_state, false)
          };
          match outcome {
            Ok(RunOutcome::Yield(value)) => Ok(vm.make_iterator_result(value, false)),
            Ok(RunOutcome::YieldStar(result)) => Ok(result),
            Ok(RunOutcome::Return(value)) => {
              runner.complete();
              Ok(vm.make_iterator_result(value, true))
            }
            Ok(RunOutcome::Initialized) => Err(RuntimeError::from(
              "generator initialization marker executed twice",
            )),
            Ok(RunOutcome::Await(_)) => {
              Err(RuntimeError::from("await boundary reached in generator"))
            }
            Err(error) => {
              let error = vm.propagate_generator_error(&mut runner, error);
              runner.complete();
              Err(error)
            }
          }
        }
      })),
    );
    gen_obj.borrow_mut().set_internal_slot(
      "[[GeneratorReturn]]",
      Value::Builtin(Rc::new(move |vm, args| {
        let mut runner = return_runner
          .try_borrow_mut()
          .map_err(|_| vm.throw_type_error("generator is already executing"))?;
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
          runner.complete();
          return Ok(vm.make_iterator_result(result_value, true));
        }

        let outcome = {
          let GeneratorRunner {
            vm, register_state, ..
          } = &mut *runner;
          vm.run_state(register_state, false)
        };
        match outcome {
          Ok(RunOutcome::Yield(value)) => Ok(vm.make_iterator_result(value, false)),
          Ok(RunOutcome::YieldStar(result)) => Ok(result),
          Ok(RunOutcome::Return(value)) => {
            runner.complete();
            Ok(vm.make_iterator_result(value, true))
          }
          Ok(RunOutcome::Initialized) => Err(RuntimeError::from(
            "generator initialization marker executed twice",
          )),
          Ok(RunOutcome::Await(_)) => {
            Err(RuntimeError::from("await boundary reached in generator"))
          }
          Err(error) => {
            let error = vm.propagate_generator_error(&mut runner, error);
            runner.complete();
            Err(error)
          }
        }
      })),
    );
    Ok(Value::Object(gen_obj))
  }
}
