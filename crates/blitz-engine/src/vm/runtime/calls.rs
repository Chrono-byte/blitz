use crate::bytecode::FunctionProto;
use crate::ecmascript::semantics::properties::own_property_descriptor;
use crate::environment::EnvironmentRecord;
use crate::errors::RuntimeError;
use crate::gc;
use crate::mutex_compat::MutexBorrowExt;
use crate::runtime_utils::{to_length, usize_to_decimal_string, usize_to_f64};
use crate::value::{
  ArrayData, ObjectData, PropertyDescriptor, PropertyKey, SharedValue, SymbolId, Value,
};
use crate::vm::Vm;
use crate::vm::runtime::execution::get_property;
use crate::vm::state::{
  Frame, GeneratorRunner, LOCALS_INITIAL_CAPACITY, RiscExecutionState, RiscRunOutcome,
};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Mutex;

impl Vm {
  fn propagate_generator_error(
    &mut self,
    runner: &mut GeneratorRunner,
    fallback: RuntimeError,
  ) -> RuntimeError {
    let Some(thrown) = runner.vm.last_uncaught_exception.take() else {
      return fallback;
    };
    self
      .throw_value(thrown)
      .err()
      .unwrap_or_else(|| RuntimeError::from("JS exception"))
  }

  fn throw_class_constructor_call_error(
    &mut self,
    proto: &FunctionProto,
  ) -> RuntimeError {
    self.throw_type_error(&format!(
      "Class constructor {} cannot be invoked without 'new'",
      proto.name
    ))
  }

  pub(super) fn flatten_spread_values(
    &mut self,
    values: Value,
    mask: Value,
  ) -> Result<Vec<Value>, RuntimeError> {
    let values_vec = match values {
      Value::Array(arr, _) => arr.borrow().clone_elements(),
      _ => Vec::new(),
    };
    let mask_vec = match mask {
      Value::Array(arr, _) => arr.borrow().clone_elements(),
      _ => Vec::new(),
    };

    let mut flat: Vec<Value> = Vec::new();
    for (i, value) in values_vec.into_iter().enumerate() {
      let spread = mask_vec
        .get(i)
        .and_then(|v| v.as_ref())
        .is_some_and(|v| self.to_boolean(v));
      if !spread {
        if let Some(v) = value {
          flat.push(v);
        }
        continue;
      }

      if let Some(iterable) = value {
        let iterator_method = self.get_property_host(
          &iterable,
          &PropertyKey::Symbol(SymbolId::well_known_iterator()),
        )?;

        if !Self::is_callable_value(&iterator_method) {
          return Err(self.throw_type_error("Spread value is not iterable"));
        }

        let iter_obj = self.call_value_with_this(&iterator_method, &iterable, &[])?;

        loop {
          let next_fn =
            self.get_property_host(&iter_obj, &PropertyKey::String("next".to_string()))?;
          let result = self.call_value_with_this(&next_fn, &iter_obj, &[])?;
          let done_val =
            self.get_property_host(&result, &PropertyKey::String("done".to_string()))?;
          if self.to_boolean(&done_val) {
            break;
          }
          let val = self.get_property_host(&result, &PropertyKey::String("value".to_string()))?;
          flat.push(val);
        }
      }
    }

    Ok(flat)
  }

  pub(super) fn create_generator_object(
    &mut self,
    proto: &Rc<FunctionProto>,
    closure_env: &Rc<Mutex<Vec<SharedValue>>>,
    call_args: Vec<Value>,
    this_value: Value,
    callee: &Value,
  ) -> Result<Value, RuntimeError> {
    let mut child = self.fork_execution();
    child.push_call_frame(proto, closure_env, call_args, this_value, callee.clone())?;
    let mut risc_state = RiscExecutionState::new(
      child.this_value.clone(),
      child.chunk.risc_spill_count as usize,
    );
    match child.run_risc_state(&mut risc_state, true)? {
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
      .set_prototype_value(self.realm.generator_prototype());
    gen_obj.borrow_mut().properties.insert(
      PropertyKey::String("next".to_string()),
      PropertyDescriptor::data(Value::Builtin(Rc::new(move |vm, args| {
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
      PropertyKey::String("throw".to_string()),
      PropertyDescriptor::data(Value::Builtin(Rc::new(move |vm, args| {
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
      PropertyKey::String("return".to_string()),
      PropertyDescriptor::data(Value::Builtin(Rc::new(move |vm, args| {
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
      PropertyDescriptor::data(Value::Builtin(Rc::new(|_vm, args| {
        Ok(args.first().cloned().unwrap_or(Value::Undefined))
      }))),
    );
    Ok(Value::Object(gen_obj))
  }

  /// Host hook: invoke a callable with an explicit receiver.
  ///
  /// # Errors
  ///
  /// Returns a [`crate::errors::RuntimeError`] if the call fails.
  pub fn call_value_with_this(
    &mut self,
    callee: &Value,
    this_arg: &Value,
    args: &[Value],
  ) -> Result<Value, RuntimeError> {
    self.call_with_this(callee, this_arg, args)
  }

  /// Host hook: construct a callable with `new` semantics.
  ///
  /// # Errors
  ///
  /// Returns a [`crate::errors::RuntimeError`] if the construction fails.
  pub(crate) fn construct_value_with_new_target(
    &mut self,
    callee: &Value,
    args: &[Value],
    new_target: Value,
  ) -> Result<Value, RuntimeError> {
    let previous_new_target = self.construct_new_target.replace(new_target);
    let result = self.construct_value(callee, args);
    self.construct_new_target = previous_new_target;
    result
  }

  /// Host hook: construct a callable with `new` semantics.
  ///
  /// # Errors
  ///
  /// Returns a [`crate::errors::RuntimeError`] if the construction fails.
  #[allow(clippy::too_many_lines)]
  pub fn construct_value(
    &mut self,
    callee: &Value,
    args: &[Value],
  ) -> Result<Value, RuntimeError> {
    let set_constructor_property = |value: &Value, ctor: &Value| -> Result<(), RuntimeError> {
      match value {
        Value::Object(obj) => {
          obj.borrow_mut().properties.insert(
            PropertyKey::String("constructor".to_string()),
            crate::runtime_utils::non_enum_data(ctor.clone()),
          );
        }
        Value::Array(_, props) => {
          props.borrow_mut().properties.insert(
            PropertyKey::String("constructor".to_string()),
            crate::runtime_utils::non_enum_data(ctor.clone()),
          );
        }
        Value::Closure { props, .. } => {
          props.borrow_mut().properties.insert(
            PropertyKey::String("constructor".to_string()),
            crate::runtime_utils::non_enum_data(ctor.clone()),
          );
        }
        Value::BuiltinObject(_, props) => {
          props.borrow_mut().properties.insert(
            PropertyKey::String("constructor".to_string()),
            crate::runtime_utils::non_enum_data(ctor.clone()),
          );
        }
        Value::Proxy(_) => {}
        _ => {}
      }
      Ok(())
    };
    let temp_root_base = self.gc_temp_roots.len();
    self.gc_temp_roots.push(callee.clone());
    self.gc_temp_roots.extend(args.iter().cloned());

    if !callee.is_constructable() {
      if crate::trace::enabled() {
        eprintln!(
          "[trace] construct_value: callee not constructable ip={} actual={}",
          self.ip.saturating_sub(1),
          callee.type_name()
        );
        crate::trace::value_string("[trace]   callee", callee);
        for (i, v) in self.stack.iter_rev().take(6).enumerate() {
          crate::trace::value_string(&format!("[trace]   stack[{}]", i), v);
        }
        // Dump a small window of bytecode around the current ip to help
        // correlate the failing instruction with constants and nearby ops.
        let cur_ip = self.ip.saturating_sub(1);
        let start = cur_ip.saturating_sub(16);
        let end = (cur_ip + 16).min(self.chunk.code.len());
        eprintln!("[trace]   bytecode window ({}..{}):", start, end);
        for i in start..end {
          let instr = self.chunk.code[i];
          let b = (instr & 0xFF) as u8;
          eprintln!("[trace]     {:04} 0x{:02x}", i, b);
        }
        // Print a short constant pool summary
        eprintln!("[trace]   constants (len={}):", self.chunk.constants.len());
        for (i, c) in self.chunk.constants.iter().enumerate().take(32) {
          crate::trace::value_string(&format!("[trace]     const[{}]", i), c);
        }
      }
      self.gc_temp_roots.truncate(temp_root_base);
      return Err(self.throw_type_error(&format!(
        "Construct: not a constructor (actual={})",
        callee.type_name()
      )));
    }
    match callee {
      Value::Proxy(proxy) => {
        let result = (|| -> Result<Value, RuntimeError> {
          let (target, handler) = {
            let proxy = proxy.borrow();
            let Some(handler) = proxy.handler.clone() else {
              return Err(self.throw_type_error("Cannot construct a revoked proxy"));
            };
            (proxy.target.clone(), handler)
          };
          let trap =
            self.get_property_value(&handler, &PropertyKey::String("construct".to_string()))?;
          let new_target = self
            .construct_new_target
            .clone()
            .unwrap_or_else(|| callee.clone());
          if matches!(trap, Value::Undefined | Value::Null) {
            let previous_new_target = self.construct_new_target.replace(new_target);
            let result = self.construct_value(&target, args);
            self.construct_new_target = previous_new_target;
            return result;
          }
          if !Self::is_callable_value(&trap) {
            return Err(self.throw_type_error("Proxy construct trap is not callable"));
          }

          let arguments = self.new_array(args.iter().cloned().map(Some).collect());
          let trap_result =
            self.call_with_this(&trap, &handler, &[target, arguments, new_target])?;
          if !trap_result.is_object() {
            return Err(self.throw_type_error("Proxy construct trap must return an object"));
          }
          Ok(trap_result)
        })();
        self.gc_temp_roots.truncate(temp_root_base);
        result
      }
      Value::Closure { .. } => {
        let is_derived_constructor = match callee {
          Value::Closure { proto, .. } => proto.is_derived_constructor,
          _ => false,
        };

        let instance = if is_derived_constructor {
          Value::Undefined
        } else {
          let proto_source = self
            .construct_new_target
            .clone()
            .unwrap_or_else(|| callee.clone());
          let proto = self
            .get_property_value(&proto_source, &PropertyKey::String("prototype".to_string()))
            .unwrap_or(Value::Null);
          let instance_proto = match proto {
            Value::Object(_)
            | Value::Array(..)
            | Value::Closure { .. }
            | Value::BuiltinObject(_, _) => proto,
            _ => Value::Null,
          };
          let instance_obj = gc::track_object(Rc::new(RefCell::new(ObjectData::with_prototype(
            instance_proto,
          ))));
          instance_obj.borrow_mut().properties.insert(
            PropertyKey::String("constructor".to_string()),
            PropertyDescriptor::data(callee.clone()),
          );
          Value::Object(instance_obj)
        };
        let prev_new_target = self.construct_new_target.clone();
        let prev_recent_return = self.recent_return_value.take();
        let active_new_target = prev_new_target
          .clone()
          .unwrap_or_else(|| callee.clone());
        self.construct_new_target = Some(active_new_target);
        let result = self.call_with_this(callee, &instance, args);
        self.construct_new_target = prev_new_target;
        let result = result?;
        let initialized_this = self.recent_return_value.take();
        self.recent_return_value = prev_recent_return;
        let result = match result {
          Value::Object(_)
          | Value::Array(..)
          | Value::Closure { .. }
          | Value::BuiltinObject(_, _)
          | Value::Proxy(_)
          | Value::Promise(_) => {
            drop(set_constructor_property(&result, callee));
            result
          }
          _ if is_derived_constructor => {
            let Some(this_obj) = initialized_this else {
              self.gc_temp_roots.truncate(temp_root_base);
              return Err(self.throw_reference_error(
                "Must call super constructor before returning from derived constructor",
              ));
            };
            this_obj
          }
          _ => instance,
        };
        drop(set_constructor_property(&result, callee));
        if matches!(
          result,
          Value::Object(_)
            | Value::Array(..)
            | Value::Closure { .. }
            | Value::BuiltinObject(_, _)
            | Value::Proxy(_)
            | Value::Promise(_)
        ) {
          self.recent_return_value = Some(result.clone());
        } else {
          self.recent_return_value = None;
        }
        self.gc_temp_roots.truncate(temp_root_base);
        Ok(result)
      }
      Value::BuiltinObject(_, props) => {
        let bound_target = props
          .borrow()
          .properties
          .get(&PropertyKey::String("[[BoundTargetFunction]]".to_string()))
          .and_then(|desc| match desc {
            PropertyDescriptor::Data { value, .. } => Some(value.clone()),
            _ => None,
          });
        if let Some(target) = bound_target {
          let bound_args = props
            .borrow()
            .properties
            .get(&PropertyKey::String("[[BoundArguments]]".to_string()))
            .and_then(|desc| match desc {
              PropertyDescriptor::Data {
                value: Value::Array(arr, _),
                ..
              } => Some(
                arr
                  .borrow()
                  .iter()
                  .filter_map(Clone::clone)
                  .collect::<Vec<Value>>(),
              ),
              _ => None,
            })
            .unwrap_or_default();
          let mut combined = bound_args;
          combined.extend_from_slice(args);
          let prev_new_target = self.construct_new_target.clone();
          let active_new_target = prev_new_target
            .clone()
            .unwrap_or_else(|| target.clone());
          self.construct_new_target = Some(active_new_target);
          let result = self.construct_value(&target, &combined);
          self.construct_new_target = prev_new_target;
          self.gc_temp_roots.truncate(temp_root_base);
          return result;
        }

        let proto_source = self
          .construct_new_target
          .clone()
          .unwrap_or_else(|| callee.clone());
        let proto = self
          .get_property_value(&proto_source, &PropertyKey::String("prototype".to_string()))
          .unwrap_or(Value::Null);
        let instance_proto = match proto {
          Value::Object(_)
          | Value::Array(..)
          | Value::Closure { .. }
          | Value::BuiltinObject(_, _) => proto,
          _ => Value::Null,
        };
        let instance_obj = gc::track_object(Rc::new(RefCell::new(ObjectData::with_prototype(
          instance_proto,
        ))));
        instance_obj.borrow_mut().properties.insert(
          PropertyKey::String("constructor".to_string()),
          PropertyDescriptor::data(callee.clone()),
        );
        let instance = Value::Object(instance_obj);
        // Builtin constructors receive:
        // args[0] = new instance (via call_with_this)
        // args[1..] = user-provided arguments
        // Keep [[NewTarget]] in VM state so builtins can inspect it
        // without perturbing user-visible argument positions.
        let prev_new_target = self.construct_new_target.clone();
        let active_new_target = prev_new_target
          .clone()
          .unwrap_or_else(|| callee.clone());
        self.construct_new_target = Some(active_new_target);
        let result = self.call_with_this(callee, &instance, args);
        self.construct_new_target = prev_new_target;
        let result = result?;
        let result = match result {
          Value::Object(_)
          | Value::Array(..)
          | Value::Closure { .. }
          | Value::BuiltinObject(_, _)
          | Value::Proxy(_)
          | Value::Promise(_) => {
            drop(set_constructor_property(&result, callee));
            result
          }
          _ => instance,
        };
        drop(set_constructor_property(&result, callee));
        if matches!(
          result,
          Value::Object(_)
            | Value::Array(..)
            | Value::Closure { .. }
            | Value::BuiltinObject(_, _)
            | Value::Proxy(_)
            | Value::Promise(_)
        ) {
          self.recent_return_value = Some(result.clone());
        } else {
          self.recent_return_value = None;
        }
        self.gc_temp_roots.truncate(temp_root_base);
        Ok(result)
      }
      Value::Builtin(f) => {
        let prev_builtin_this = std::mem::replace(&mut self.builtin_this_arg_prepended, false);
        let result = (f.as_ref())(self, args);
        self.builtin_this_arg_prepended = prev_builtin_this;
        if let Ok(value) = &result {
          if matches!(
            value,
            Value::Object(_)
              | Value::Array(..)
              | Value::Closure { .. }
              | Value::BuiltinObject(_, _)
              | Value::Promise(_)
          ) {
            self.recent_return_value = Some(value.clone());
          } else {
            self.recent_return_value = None;
          }
        }
        self.gc_temp_roots.truncate(temp_root_base);
        result
      }
      _ => {
        self.gc_temp_roots.truncate(temp_root_base);
        Err(RuntimeError::from(format!(
          "Construct: not a constructor (actual={})",
          callee.type_name()
        )))
      }
    }
  }

  pub(super) fn construct_super_value(
    &mut self,
    mut args: Vec<Value>,
  ) -> Result<Value, RuntimeError> {
    if args.len() == 1
      && let Some(arguments) = args.first()
      && matches!(
        own_property_descriptor(arguments, &PropertyKey::String("callee".to_string())),
        Some(PropertyDescriptor::Accessor { .. })
      )
      && let Ok(length) =
        self.get_property_host(arguments, &PropertyKey::String("length".to_string()))
      && let Ok(length) = to_length(self, &length)
    {
      let mut flattened = Vec::with_capacity(length);
      for index in 0..length {
        flattened.push(self.get_property_host(arguments, &PropertyKey::String(index.to_string()))?);
      }
      args = flattened;
    }

    let frame = self
      .call_stack
      .last()
      .ok_or_else(|| RuntimeError::from("super() outside function frame"))?;
    let (callee, is_derived_constructor) = match &frame.callee {
      Value::Closure { proto, .. } => (frame.callee.clone(), proto.is_derived_constructor),
      _ => (frame.callee.clone(), false),
    };
    if !is_derived_constructor {
      return Err(self.throw_type_error("super() is only valid in derived class constructors"));
    }

    let super_constructor = self
      .get_property_value(
        &callee,
        &PropertyKey::String("[[SuperConstructor]]".to_string()),
      )
      .unwrap_or_else(|_| self.get_prototype(&callee));
    if matches!(super_constructor, Value::Null | Value::Undefined) {
      return Err(self.throw_type_error("Class extends value does not have a valid constructor"));
    }
    if !super_constructor.is_constructable() {
      return Err(self.throw_type_error("Super constructor is not a constructor"));
    }

    let new_target = self.current_new_target().unwrap_or(callee);
    let previous_new_target = self.construct_new_target.replace(new_target);
    let result = self.construct_value(&super_constructor, &args);
    self.construct_new_target = previous_new_target;
    let result = result?;
    self.this_value = result.clone();
    self.recent_return_value = Some(result.clone());
    Ok(result)
  }

  /// Calls a function without an explicit receiver.
  ///
  /// Native builtins use the same argument convention as the compiler IR
  /// `Call` opcode: user arguments begin at index zero. Closures still pass
  /// through ordinary ECMAScript `this` preparation.
  pub(super) fn call_value(
    &mut self,
    callee: &Value,
    args: &[Value],
  ) -> Result<Value, RuntimeError> {
    match callee {
      Value::Builtin(f) | Value::BuiltinObject(f, _) => {
        let temp_root_base = self.gc_temp_roots.len();
        self.gc_temp_roots.push(callee.clone());
        self.gc_temp_roots.extend(args.iter().cloned());

        let prev_this = std::mem::replace(&mut self.this_value, Value::Undefined);
        let prev_builtin_this = std::mem::replace(&mut self.builtin_this_arg_prepended, false);
        let prev_closure_env = self.current_closure_env.clone();
        let prev_slot_types = self.current_slot_types.clone();
        let prev_lexical_env = Rc::clone(&self.lexical_env);
        let result = (f.as_ref())(self, args);
        self.builtin_this_arg_prepended = prev_builtin_this;
        self.this_value = prev_this;
        self.current_closure_env = prev_closure_env;
        self.current_slot_types = prev_slot_types;
        self.lexical_env = prev_lexical_env;

        let result = (|| {
          let result = result?;
          self.gc_temp_roots.push(result.clone());
          self.run_microtasks_if_needed()?;
          self.recent_return_value = matches!(
            result,
            Value::Object(_)
              | Value::Array(..)
              | Value::Closure { .. }
              | Value::BuiltinObject(_, _)
              | Value::Proxy(_)
              | Value::Promise(_)
          )
          .then(|| result.clone());
          Ok(result)
        })();
        self.gc_temp_roots.truncate(temp_root_base);
        result
      }
      _ => self.call_with_this(callee, &Value::Undefined, args),
    }
  }

  /// Calls a function with an explicit `this` value and arguments.
  #[allow(clippy::too_many_lines)]
  pub(super) fn call_with_this(
    &mut self,
    callee: &Value,
    this_arg: &Value,
    args: &[Value],
  ) -> Result<Value, RuntimeError> {
    let temp_root_base = self.gc_temp_roots.len();
    self.gc_temp_roots.push(callee.clone());
    let this_arg_owned = this_arg.clone();
    self.gc_temp_roots.push(this_arg_owned.clone());
    self.gc_temp_roots.extend(args.iter().cloned());
    let result = (|| -> Result<Value, RuntimeError> {
      match callee {
        Value::Proxy(proxy) => {
          let (target, handler) = {
            let proxy = proxy.borrow();
            let Some(handler) = proxy.handler.clone() else {
              return Err(self.throw_type_error("Cannot call a revoked proxy"));
            };
            (proxy.target.clone(), handler)
          };
          if !target.is_callable() {
            return Err(self.throw_type_error("Proxy target is not callable"));
          }
          let trap =
            self.get_property_value(&handler, &PropertyKey::String("apply".to_string()))?;
          if matches!(trap, Value::Undefined | Value::Null) {
            return self.call_with_this(&target, &this_arg_owned, args);
          }
          if !Self::is_callable_value(&trap) {
            return Err(self.throw_type_error("Proxy apply trap is not callable"));
          }

          let arguments = self.new_array(args.iter().cloned().map(Some).collect());
          self.call_with_this(
            &trap,
            &handler,
            &[target, this_arg_owned.clone(), arguments],
          )
        }
        Value::Builtin(f) => {
          // For builtins invoked with an explicit `this`, pass the receiver
          // as the first argument so native functions that expect the
          // receiver can access it (used by accessor getters/setters).
          let mut call_args: Vec<Value> = Vec::with_capacity(args.len() + 1);
          call_args.push(this_arg_owned.clone());
          call_args.extend_from_slice(args);
          let prev_this = std::mem::replace(&mut self.this_value, this_arg_owned);
          let prev_builtin_this = std::mem::replace(&mut self.builtin_this_arg_prepended, true);
          let prev_closure_env = self.current_closure_env.clone();
          let prev_slot_types = self.current_slot_types.clone();
          let prev_lexical_env = Rc::clone(&self.lexical_env);
          let result = (f.as_ref())(self, &call_args);
          self.builtin_this_arg_prepended = prev_builtin_this;
          self.this_value = prev_this;
          self.current_closure_env = prev_closure_env;
          self.current_slot_types = prev_slot_types;
          self.lexical_env = prev_lexical_env;
          let result = result?;
          let result_root_base = self.gc_temp_roots.len();
          self.gc_temp_roots.push(result.clone());
          let post_call = (|| {
            self.run_microtasks_if_needed()?;
            Ok::<(), RuntimeError>(())
          })();
          self.gc_temp_roots.truncate(result_root_base);
          post_call?;
          if matches!(
            result,
            Value::Object(_)
              | Value::Array(..)
              | Value::Closure { .. }
              | Value::BuiltinObject(_, _)
              | Value::Promise(_)
          ) {
            self.recent_return_value = Some(result.clone());
          } else {
            self.recent_return_value = None;
          }
          Ok(result)
        }
        Value::BuiltinObject(f, _props) => {
          let mut call_args: Vec<Value> = Vec::with_capacity(args.len() + 1);
          call_args.push(this_arg_owned.clone());
          call_args.extend_from_slice(args);
          let prev_this = std::mem::replace(&mut self.this_value, this_arg_owned);
          let prev_builtin_this = std::mem::replace(&mut self.builtin_this_arg_prepended, true);
          let prev_closure_env = self.current_closure_env.clone();
          let prev_slot_types = self.current_slot_types.clone();
          let prev_lexical_env = Rc::clone(&self.lexical_env);
          let result = (f.as_ref())(self, &call_args);
          self.builtin_this_arg_prepended = prev_builtin_this;
          self.this_value = prev_this;
          self.current_closure_env = prev_closure_env;
          self.current_slot_types = prev_slot_types;
          self.lexical_env = prev_lexical_env;
          let result = result?;
          let result_root_base = self.gc_temp_roots.len();
          self.gc_temp_roots.push(result.clone());
          let post_call = (|| {
            self.run_microtasks_if_needed()?;
            Ok::<(), RuntimeError>(())
          })();
          self.gc_temp_roots.truncate(result_root_base);
          post_call?;
          if matches!(
            result,
            Value::Object(_)
              | Value::Array(..)
              | Value::Closure { .. }
              | Value::BuiltinObject(_, _)
              | Value::Promise(_)
          ) {
            self.recent_return_value = Some(result.clone());
          } else {
            self.recent_return_value = None;
          }
          Ok(result)
        }
        Value::Closure { proto, .. } => {
          if proto.is_class_constructor && self.construct_new_target.is_none() {
            return Err(self.throw_class_constructor_call_error(proto));
          }
          let mut call_args: Vec<Value> = Vec::with_capacity(args.len());
          call_args.extend_from_slice(args);
          let call_args = self.prepare_call_args(&call_args, proto)?;
          let closure_env = match callee {
            Value::Closure { env, .. } => Rc::clone(env),
            _ => {
              return Err(self.throw_type_error("callee is not a closure"));
            }
          };
          let this_arg = Self::closure_lexical_this(callee).unwrap_or_else(|| this_arg.clone());
          let this_arg = if proto.is_derived_constructor {
            Value::Uninitialized
          } else {
            this_arg
          };
          if proto.is_generator {
            Ok(self.create_generator_object(proto, &closure_env, call_args, this_arg, callee)?)
          } else if proto.is_async {
            self.invoke_async_closure(proto, &closure_env, call_args, this_arg, callee)
          } else {
            let stack_base = self.stack.len();
            let saved_stack = self.stack.snapshot();
            if crate::trace::enabled() {
              eprintln!(
                "[trace] call_with_this closure stack_base={} stack_len={} proto='{}'",
                stack_base,
                self.stack.len(),
                proto.name
              );
            }
            let caller_chunk = Rc::clone(&self.chunk);
            let caller_ip = self.ip;
            let caller_locals = self.locals.clone();
            let caller_this = self.this_value.clone();
            let caller_env = self.current_closure_env.clone();
            let caller_slot_types = self.current_slot_types.clone();
            let caller_strict_mode = self.current_strict_mode;
            let caller_frame_len = self.call_stack.len();
            let caller_handler_len = self.handlers.len();
            self.push_call_frame(proto, &closure_env, call_args, this_arg, callee.clone())?;
            let result = self.run_risc_loop(false).and_then(|value| {
              self.stack.push(value);
              self.execute_return()?;
              self.pop()
            });
            if result.is_ok() || !self.pending_exception {
              // Restore the suspended caller state after the callee returns.
              self.chunk = caller_chunk;
              self.ip = caller_ip;
              self.locals = caller_locals;
              self.this_value = caller_this;
              self.current_closure_env = caller_env;
              self.current_slot_types = caller_slot_types;
              self.stack.restore_from(saved_stack);
              self.call_stack.truncate(caller_frame_len);
              self.current_strict_mode = caller_strict_mode;
              self.handlers.truncate(caller_handler_len);
            }
            result
          }
        }
        _ => {
          if crate::trace::enabled() {
            eprintln!("[trace] call_with_this: callee not callable (final fallback)");
            crate::trace::value_string("[trace]   callee", callee);
            crate::trace::value_string("[trace]   this_arg", this_arg);
            for (i, v) in self.stack.iter_rev().take(8).enumerate() {
              crate::trace::value_string(&format!("[trace]   stack[{}]", i), v);
            }
          }
          self.dump_diag("CallWithThis failed: callee not callable", callee);
          Err(self.throw_type_error(&format!("{} is not a function", callee.type_name())))
        }
      }
    })();
    self.gc_temp_roots.truncate(temp_root_base);
    result
  }

  /// Prepares the arguments for a function call, handling rest parameters.
  pub(super) fn prepare_call_args(
    &self,
    args: &[Value],
    proto: &FunctionProto,
  ) -> Result<Vec<Value>, RuntimeError> {
    if !proto.has_rest {
      return Ok(args.to_vec());
    }

    let ar = proto.arity;
    let fixed_count = ar.saturating_sub(1);
    let mut fixed = Vec::with_capacity(ar);

    for i in 0..fixed_count {
      fixed.push(args.get(i).cloned().unwrap_or(Value::Undefined));
    }

    let rest_slice: Vec<Option<Value>> = if args.len() > fixed_count {
      args[fixed_count..]
        .iter()
        .map(|v| Some(v.clone()))
        .collect()
    } else {
      Vec::new()
    };

    let arr_rc = Rc::new(RefCell::new(ArrayData::from(rest_slice)));
    let props: Rc<RefCell<ObjectData>> = gc::track_object(Rc::new(RefCell::new(ObjectData::new())));
    props.borrow_mut().properties.insert(
      PropertyKey::String("length".to_string()),
      PropertyDescriptor::data(Value::Number(usize_to_f64(arr_rc.borrow().len()))),
    );

    if let Some(array_val) = self
      .globals
      .borrow()
      .properties
      .get(&PropertyKey::String("Array".to_string()))
      .and_then(|d| match d {
        PropertyDescriptor::Data { value, .. } => Some(value.clone()),
        _ => None,
      })
      && let Ok(proto_val) =
        get_property(&array_val, &PropertyKey::String("prototype".to_string()), 0)
      && !matches!(proto_val, Value::Null)
    {
      props.borrow_mut().set_prototype_value(proto_val);
      let proto_for_lookup = props.borrow().get_prototype();
      if let Ok(for_each_val) = get_property(
        &proto_for_lookup,
        &PropertyKey::String("forEach".to_string()),
        0,
      ) && !matches!(for_each_val, Value::Undefined)
      {
        props.borrow_mut().properties.insert(
          PropertyKey::String("forEach".to_string()),
          PropertyDescriptor::data(for_each_val),
        );
      }
    }

    fixed.push(Value::Array(arr_rc, props));
    if crate::trace::enabled() {
      // Log allocation site for rest-array used as arguments so we can
      // correlate a later callee pointing at the same props pointer.
      if let Value::Array(_arr, props_rc) = fixed
        .last()
        .cloned()
        .expect("fixed must not be empty after push")
      {
        eprintln!(
          "[trace] prepare_call_args: created rest-Array props_ptr={:p} proto='{}'",
          Rc::as_ptr(&props_rc),
          proto.name
        );
      }
    }
    Ok(fixed)
  }

  /// Configures the call frame for a function call, setting up the stack and local variables.
  #[allow(clippy::too_many_lines)]
  pub(super) fn configure_call_frame(
    &mut self,
    proto: &Rc<FunctionProto>,
    closure_env: &Rc<Mutex<Vec<SharedValue>>>,
    call_args: Vec<Value>,
    mut this_value: Value,
    callee: Value,
  ) -> Result<(), RuntimeError> {
    self.pending_closure_env = Some(Rc::clone(closure_env));
    if !proto.is_strict {
      if matches!(this_value, Value::Undefined | Value::Null) {
        this_value = Value::Object(Rc::clone(&self.globals));
      } else {
        this_value = match self.to_object_value(this_value) {
          Ok(value) => value,
          Err(err) => {
            self.pending_closure_env = None;
            return Err(err);
          }
        };
      }
    }

    self.chunk = Rc::clone(&proto.chunk);
    self.ip = 0;
    self.stack.clear();
    self.current_closure_env = Some(Rc::clone(closure_env));
    self.current_slot_types = Some(proto.slot_types.clone());
    self.current_slot_names = Some(proto.slot_names.clone());
    self.current_slot_immutable = Some(proto.slot_immutable.clone());
    self.current_slot_sloppy_immutable_silent = Some(proto.slot_sloppy_immutable_silent.clone());
    self.yield_star_state = None;
    self.this_value = if proto.is_derived_constructor {
      Value::Uninitialized
    } else {
      this_value
    };

    self.locals = std::iter::repeat_with(|| Rc::new(Mutex::new(Value::Undefined)))
      .take(proto.slot_types.len().max(proto.arity))
      .collect();
    for (idx, is_tdz) in proto.lexical_tdz.iter().copied().enumerate() {
      if is_tdz && idx < self.locals.len() {
        *self.locals[idx].borrow_mut() = Value::Uninitialized;
      }
    }
    let captured_lexical_env = match &callee {
      Value::Closure { props, .. } => props
        .borrow()
        .captured_lexical_env()
        .unwrap_or_else(|| Rc::clone(&self.lexical_env)),
      _ => Rc::clone(&self.lexical_env),
    };
    let fn_env = Rc::new(RefCell::new(EnvironmentRecord::new(Some(
      captured_lexical_env,
    ))));
    for (idx, maybe_name) in proto.slot_names.iter().enumerate() {
      if let Some(name) = maybe_name
        && idx < self.locals.len()
      {
        let initialized = !proto
          .lexical_tdz
          .get(idx)
          .copied()
          .unwrap_or(false);
        fn_env.borrow_mut().create_mutable_binding(
          name.clone(),
          Rc::clone(&self.locals[idx]),
          initialized,
        );
      }
    }
    self.lexical_env = fn_env;
    for (i, value) in call_args
      .iter()
      .cloned()
      .enumerate()
      .take(proto.arity)
    {
      *self.locals[i].borrow_mut() = value;
    }
    if crate::trace::enabled() && proto.is_generator && !proto.has_simple_parameter_list {
      for (idx, slot) in self.locals.iter().enumerate() {
        let name = proto
          .slot_names
          .get(idx)
          .and_then(Clone::clone)
          .unwrap_or_else(|| "<none>".to_string());
        eprintln!(
          "[trace] configure_call_frame gen slot={} name='{}' value={}",
          idx,
          name,
          slot.borrow().type_name()
        );
      }
    }
    if let Some(slot) = proto.self_binding_slot
      && slot < self.locals.len()
    {
      *self.locals[slot].borrow_mut() = callee.clone();
    }
    if let Some(slot) = proto.arguments_slot
      && slot < self.locals.len()
    {
      let args_for_arguments: Vec<Option<Value>> = call_args
        .iter()
        .map(|v| Some(v.clone()))
        .collect();
      if proto.has_simple_parameter_list && !proto.is_strict {
        let mapped_count = proto.arity.min(args_for_arguments.len());
        let args_obj = self.build_mapped_arguments_object(proto, callee, &args_for_arguments);
        self.current_arguments_object = Some(args_obj.clone());
        self.current_arguments_mapped_count = mapped_count;
        self.locals[slot] = Rc::new(Mutex::new(args_obj));
      } else {
        let props: Rc<RefCell<ObjectData>> =
          gc::track_object(Rc::new(RefCell::new(ObjectData::new())));
        for (idx, maybe_value) in args_for_arguments.iter().enumerate() {
          props.borrow_mut().properties.insert(
            PropertyKey::String(usize_to_decimal_string(idx)),
            PropertyDescriptor::Data {
              value: maybe_value.clone().unwrap_or(Value::Undefined),
              writable: true,
              enumerable: true,
              configurable: true,
            },
          );
        }
        if proto.is_strict {
          let poison = Value::Builtin(Rc::new(|vm, _args| {
            Err(vm.throw_type_error("Access to strict arguments callee/caller is forbidden"))
          }));
          props.borrow_mut().properties.insert(
            PropertyKey::String("callee".to_string()),
            PropertyDescriptor::Accessor {
              get: Some(poison.clone()),
              set: Some(poison.clone()),
              enumerable: false,
              configurable: false,
            },
          );
          props.borrow_mut().properties.insert(
            PropertyKey::String("caller".to_string()),
            PropertyDescriptor::Accessor {
              get: Some(poison.clone()),
              set: Some(poison),
              enumerable: false,
              configurable: false,
            },
          );
        }
        props.borrow_mut().properties.insert(
          PropertyKey::String("length".to_string()),
          PropertyDescriptor::Data {
            value: Value::Number(usize_to_f64(args_for_arguments.len())),
            writable: true,
            enumerable: false,
            configurable: true,
          },
        );
        let object_proto = self.realm.object_prototype();
        if !matches!(object_proto, Value::Null) {
          props
            .borrow_mut()
            .set_prototype_value(object_proto);
        }
        self.install_arguments_iterator_property(&props);
        self.current_arguments_object = None;
        self.current_arguments_mapped_count = 0;
        self.locals[slot] = Rc::new(Mutex::new(Value::Object(props)));
      }
    }
    self.pending_closure_env = None;

    Ok(())
  }

  /// Save the caller's state and set up the callee.
  pub(super) fn push_call_frame(
    &mut self,
    proto: &Rc<FunctionProto>,
    closure_env: &Rc<Mutex<Vec<SharedValue>>>,
    call_args: Vec<Value>,
    this_value: Value,
    callee: Value,
  ) -> Result<(), RuntimeError> {
    let caller_stack = self.stack.take_all();
    let stack_base = caller_stack.len();
    let caller_strict_mode = self.current_strict_mode;

    if crate::trace::enabled() {
      eprintln!(
        "[trace] push_call_frame stack_base={} proto='{}' arity={} stack_len={}",
        stack_base,
        proto.name,
        proto.arity,
        self.stack.len()
      );
    }

    self.call_stack.push(Frame {
      chunk: Rc::clone(&self.chunk),
      ip: self.ip,
      stack: caller_stack,
      locals: std::mem::replace(
        &mut self.locals,
        Vec::with_capacity(LOCALS_INITIAL_CAPACITY),
      ),
      this_value: std::mem::replace(&mut self.this_value, this_value.clone()),
      callee: callee.clone(),
      // `construct_new_target` is a pending entry value. Moving it into the
      // suspended frame makes it active for this callee while ensuring an
      // ordinary nested call starts with no [[NewTarget]].
      new_target: self.construct_new_target.take(),
      closure_env: self.current_closure_env.take(),
      upvalues: proto.upvalues.clone(),
      slot_types: self.current_slot_types.take(),
      slot_names: self.current_slot_names.take(),
      slot_immutable: self.current_slot_immutable.take(),
      slot_sloppy_immutable_silent: self.current_slot_sloppy_immutable_silent.take(),
      stack_base,
      is_strict: caller_strict_mode,
      lexical_env: Rc::clone(&self.lexical_env),
      yield_star_state: self.yield_star_state.take(),
      arguments_object: self.current_arguments_object.take(),
      arguments_mapped_count: self.current_arguments_mapped_count,
    });
    self.current_strict_mode = proto.is_strict;

    self.configure_call_frame(proto, closure_env, call_args, this_value, callee)?;

    Ok(())
  }

  /// Builds an arguments object for a function call, mapping arguments to local slots.
  fn build_mapped_arguments_object(
    &mut self,
    proto: &FunctionProto,
    callee: Value,
    args_for_arguments: &[Option<Value>],
  ) -> Value {
    let obj_rc: Rc<RefCell<ObjectData>> =
      gc::track_object(Rc::new(RefCell::new(ObjectData::new())));
    let arg_count = args_for_arguments.len();
    let mapped_count = proto.arity.min(self.locals.len());
    for idx in 0..arg_count {
      let key = PropertyKey::String(usize_to_decimal_string(idx));
      if idx < mapped_count {
        obj_rc.borrow_mut().properties.insert(
          key,
          PropertyDescriptor::Data {
            value: args_for_arguments
              .get(idx)
              .cloned()
              .flatten()
              .unwrap_or(Value::Undefined),
            writable: true,
            enumerable: true,
            configurable: true,
          },
        );
      } else {
        obj_rc.borrow_mut().properties.insert(
          key,
          PropertyDescriptor::data(
            args_for_arguments
              .get(idx)
              .cloned()
              .flatten()
              .unwrap_or(Value::Undefined),
          ),
        );
      }
    }
    obj_rc.borrow_mut().properties.insert(
      PropertyKey::String("callee".to_string()),
      PropertyDescriptor::Data {
        value: callee,
        writable: true,
        enumerable: false,
        configurable: true,
      },
    );
    obj_rc.borrow_mut().properties.insert(
      PropertyKey::String("length".to_string()),
      PropertyDescriptor::Data {
        value: Value::Number(usize_to_f64(arg_count)),
        writable: true,
        enumerable: false,
        configurable: true,
      },
    );
    let object_proto = self.realm.object_prototype();
    if !matches!(object_proto, Value::Null) {
      obj_rc
        .borrow_mut()
        .set_prototype_value(object_proto);
    }
    self.install_arguments_iterator_property(&obj_rc);
    Value::Object(obj_rc)
  }

  fn install_arguments_iterator_property(
    &mut self,
    obj_rc: &Rc<RefCell<ObjectData>>,
  ) {
    let array_proto = self.realm.array_prototype();
    let iterator_key = PropertyKey::Symbol(SymbolId::well_known_iterator());
    let Ok(iterator) = self.get_property_host(&array_proto, &iterator_key) else {
      return;
    };
    obj_rc.borrow_mut().properties.insert(
      iterator_key,
      PropertyDescriptor::Data {
        value: iterator,
        writable: true,
        enumerable: false,
        configurable: true,
      },
    );
  }
}
