// SPDX-License-Identifier: MPL-2.0
use crate::bytecode::FunctionProto;
use crate::ecmascript::semantics::properties::own_property_descriptor;
use crate::environment::EnvironmentRecord;
use crate::errors::RuntimeError;
use crate::gc;
use crate::runtime_utils::{to_length, usize_to_decimal_string, usize_to_f64};
use crate::value::{BuiltinFn, ObjectData, PropertyDescriptor, PropertyKey, SharedValue, Value};
use crate::vm::Vm;
use crate::vm::state::{
  ArgumentsState, ExecutionContext, Frame, LOCALS_INITIAL_CAPACITY, SlotMetadata,
};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

mod arguments;
mod generators;

impl Vm {
  fn invoke_builtin(
    &mut self,
    function: &BuiltinFn,
    args: &[Value],
  ) -> Result<Value, RuntimeError> {
    // Native integrations can still hold object backing stores for legacy
    // construction APIs. Treat every native callback as a cache coherency
    // boundary until all external object mutation is opaque and VM-owned.
    self.caches.property_get.clear();
    let result = (function.as_ref())(self, args);
    self.caches.property_get.clear();
    result
  }

  pub(super) fn throw_class_constructor_call_error(
    &mut self,
    proto: &FunctionProto,
  ) -> RuntimeError {
    self.throw_type_error(&format!(
      "Class constructor {} cannot be invoked without 'new'",
      proto.name
    ))
  }

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
    let previous_new_target = self.construction.new_target.replace(new_target);
    let result = self.construct_value(callee, args);
    self.construction.new_target = previous_new_target;
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
    let temp_root_base = self.roots.temporary.len();
    self.roots.temporary.push(callee.clone());
    self.roots.temporary.extend(args.iter().cloned());

    if !callee.is_constructable() {
      if crate::trace::enabled() {
        eprintln!(
          "[trace] construct_value: callee not constructable ip={} actual={}",
          self.execution.active.ip.saturating_sub(1),
          callee.type_name()
        );
        crate::trace::value_string("[trace]   callee", callee);
        // Dump a small window of bytecode around the current ip to help
        // correlate the failing instruction with constants and nearby ops.
        let cur_ip = self.execution.active.ip.saturating_sub(1);
        let start = cur_ip.saturating_sub(16);
        let end = (cur_ip + 16).min(self.execution.active.chunk.code.len());
        eprintln!("[trace]   bytecode window ({}..{}):", start, end);
        for i in start..end {
          let instr = self.execution.active.chunk.code[i];
          let b = (instr & 0xFF) as u8;
          eprintln!("[trace]     {:04} 0x{:02x}", i, b);
        }
        // Print a short constant pool summary
        eprintln!(
          "[trace]   constants (len={}):",
          self.execution.active.chunk.constants.len()
        );
        for (i, constant) in self
          .execution
          .active
          .chunk
          .constants
          .iter()
          .enumerate()
          .take(32)
        {
          let value = constant.to_value();
          crate::trace::value_string(&format!("[trace]     const[{}]", i), &value);
        }
      }
      self.roots.temporary.truncate(temp_root_base);
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
          let trap = self.get_property_value(
            &handler,
            &PropertyKey::String("construct".to_string().into()),
          )?;
          let new_target = self
            .construction
            .new_target
            .clone()
            .unwrap_or_else(|| callee.clone());
          if matches!(trap, Value::Undefined | Value::Null) {
            let previous_new_target = self.construction.new_target.replace(new_target);
            let result = self.construct_value(&target, args);
            self.construction.new_target = previous_new_target;
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
        self.roots.temporary.truncate(temp_root_base);
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
            .construction
            .new_target
            .clone()
            .unwrap_or_else(|| callee.clone());
          let proto = match self.get_property_value(
            &proto_source,
            &PropertyKey::String("prototype".to_string().into()),
          ) {
            Ok(proto) => proto,
            Err(error) => {
              self.roots.temporary.truncate(temp_root_base);
              return Err(error);
            }
          };
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
          Value::Object(instance_obj)
        };
        let prev_new_target = self.construction.new_target.clone();
        let prev_recent_return = self.construction.recent_return_value.take();
        let active_new_target = prev_new_target
          .clone()
          .unwrap_or_else(|| callee.clone());
        self.construction.new_target = Some(active_new_target);
        if is_derived_constructor {
          self.construction.derived_this.push(None);
        }
        let result = self.call_with_this(callee, &instance, args);
        self.construction.new_target = prev_new_target;
        let initialized_this = if is_derived_constructor {
          self
            .construction
            .derived_this
            .pop()
            .expect("derived constructor state must remain balanced")
        } else {
          None
        };
        self.construction.recent_return_value = prev_recent_return;
        let result = result?;
        let result = match result {
          Value::Object(_)
          | Value::Array(..)
          | Value::Closure { .. }
          | Value::BuiltinObject(_, _)
          | Value::Proxy(_)
          | Value::Promise(..) => result,
          _ if is_derived_constructor => {
            let Some(this_obj) = initialized_this else {
              self.roots.temporary.truncate(temp_root_base);
              return Err(self.throw_reference_error(
                "Must call super constructor before returning from derived constructor",
              ));
            };
            this_obj
          }
          _ => instance,
        };
        if matches!(
          result,
          Value::Object(_)
            | Value::Array(..)
            | Value::Closure { .. }
            | Value::BuiltinObject(_, _)
            | Value::Proxy(_)
            | Value::Promise(..)
        ) {
          self.construction.recent_return_value = Some(result.clone());
        } else {
          self.construction.recent_return_value = None;
        }
        self.roots.temporary.truncate(temp_root_base);
        Ok(result)
      }
      Value::BuiltinObject(_, props) => {
        let bound_target = props
          .borrow()
          .internal_slot("[[BoundTargetFunction]]")
          .cloned();
        if let Some(target) = bound_target {
          let bound_args = props
            .borrow()
            .internal_slot("[[BoundArguments]]")
            .and_then(|value| match value {
              Value::Array(arr, _) => Some(
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
          let prev_new_target = self.construction.new_target.clone();
          let active_new_target = prev_new_target
            .clone()
            .unwrap_or_else(|| target.clone());
          self.construction.new_target = Some(active_new_target);
          let result = self.construct_value(&target, &combined);
          self.construction.new_target = prev_new_target;
          self.roots.temporary.truncate(temp_root_base);
          return result;
        }

        let proto_source = self
          .construction
          .new_target
          .clone()
          .unwrap_or_else(|| callee.clone());
        let proto = match self.get_property_value(
          &proto_source,
          &PropertyKey::String("prototype".to_string().into()),
        ) {
          Ok(proto) => proto,
          Err(error) => {
            self.roots.temporary.truncate(temp_root_base);
            return Err(error);
          }
        };
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
        let instance = Value::Object(instance_obj);
        // Builtin constructors receive:
        // args[0] = new instance (via call_with_this)
        // args[1..] = user-provided arguments
        // Keep [[NewTarget]] in VM state so builtins can inspect it
        // without perturbing user-visible argument positions.
        let prev_new_target = self.construction.new_target.clone();
        let active_new_target = prev_new_target
          .clone()
          .unwrap_or_else(|| callee.clone());
        self.construction.new_target = Some(active_new_target);
        let result = self.call_with_this(callee, &instance, args);
        self.construction.new_target = prev_new_target;
        let result = result?;
        let result = match result {
          Value::Object(_)
          | Value::Array(..)
          | Value::Closure { .. }
          | Value::BuiltinObject(_, _)
          | Value::Proxy(_)
          | Value::Promise(..) => result,
          _ => instance,
        };
        if matches!(
          result,
          Value::Object(_)
            | Value::Array(..)
            | Value::Closure { .. }
            | Value::BuiltinObject(_, _)
            | Value::Proxy(_)
            | Value::Promise(..)
        ) {
          self.construction.recent_return_value = Some(result.clone());
        } else {
          self.construction.recent_return_value = None;
        }
        self.roots.temporary.truncate(temp_root_base);
        Ok(result)
      }
      Value::Builtin(f) => {
        let prev_builtin_this =
          std::mem::replace(&mut self.execution.builtin_this_arg_prepended, false);
        let result = self.invoke_builtin(f, args);
        self.execution.builtin_this_arg_prepended = prev_builtin_this;
        if let Ok(value) = &result {
          if matches!(
            value,
            Value::Object(_)
              | Value::Array(..)
              | Value::Closure { .. }
              | Value::BuiltinObject(_, _)
              | Value::Promise(..)
          ) {
            self.construction.recent_return_value = Some(value.clone());
          } else {
            self.construction.recent_return_value = None;
          }
        }
        self.roots.temporary.truncate(temp_root_base);
        result
      }
      _ => {
        self.roots.temporary.truncate(temp_root_base);
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
        own_property_descriptor(arguments, &PropertyKey::String("callee".to_string().into())),
        Some(PropertyDescriptor::Accessor { .. })
      )
      && let Ok(length) =
        self.get_property_host(arguments, &PropertyKey::String("length".to_string().into()))
      && let Ok(length) = to_length(self, &length)
    {
      let mut flattened = Vec::with_capacity(length);
      for index in 0..length {
        flattened
          .push(self.get_property_host(arguments, &PropertyKey::String(index.to_string().into()))?);
      }
      args = flattened;
    }

    let frame = self
      .execution
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
        &PropertyKey::String("[[SuperConstructor]]".to_string().into()),
      )
      .unwrap_or_else(|_| self.get_prototype(&callee));
    if matches!(super_constructor, Value::Null | Value::Undefined) {
      return Err(self.throw_type_error("Class extends value does not have a valid constructor"));
    }
    if !super_constructor.is_constructable() {
      return Err(self.throw_type_error("Super constructor is not a constructor"));
    }

    let new_target = self.current_new_target().unwrap_or(callee);
    let previous_new_target = self.construction.new_target.replace(new_target);
    let result = self.construct_value(&super_constructor, &args);
    self.construction.new_target = previous_new_target;
    let result = result?;
    self.execution.active.this_value = result.clone();
    if let Some(initialized_this) = self.construction.derived_this.last_mut() {
      *initialized_this = Some(result.clone());
    }
    self.construction.recent_return_value = Some(result.clone());
    Ok(result)
  }

  /// Calls a function without an explicit receiver.
  ///
  /// Native builtins use the same argument convention as RISC v2 bytecode
  /// `Call` opcode: user arguments begin at index zero. Closures still pass
  /// through ordinary ECMAScript `this` preparation.
  pub(super) fn call_value(
    &mut self,
    callee: &Value,
    args: &[Value],
  ) -> Result<Value, RuntimeError> {
    match callee {
      Value::Builtin(f) | Value::BuiltinObject(f, _) => {
        let temp_root_base = self.roots.temporary.len();
        self.roots.temporary.push(callee.clone());
        self.roots.temporary.extend(args.iter().cloned());

        let prev_this = std::mem::replace(&mut self.execution.active.this_value, Value::Undefined);
        let prev_builtin_this =
          std::mem::replace(&mut self.execution.builtin_this_arg_prepended, false);
        let prev_closure_env = self.execution.active.closure_env.clone();
        let prev_slots = self.execution.active.slots.clone();
        let prev_lexical_env = Rc::clone(&self.execution.active.lexical_env);
        let result = self.invoke_builtin(f, args);
        self.execution.builtin_this_arg_prepended = prev_builtin_this;
        self.execution.active.this_value = prev_this;
        self.execution.active.closure_env = prev_closure_env;
        self.execution.active.slots = prev_slots;
        self.execution.active.lexical_env = prev_lexical_env;

        let result = (|| {
          let result = result?;
          self.roots.temporary.push(result.clone());
          self.run_microtasks_if_needed()?;
          self.construction.recent_return_value = matches!(
            result,
            Value::Object(_)
              | Value::Array(..)
              | Value::Closure { .. }
              | Value::BuiltinObject(_, _)
              | Value::Proxy(_)
              | Value::Promise(..)
          )
          .then(|| result.clone());
          Ok(result)
        })();
        self.roots.temporary.truncate(temp_root_base);
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
    let temp_root_base = self.roots.temporary.len();
    self.roots.temporary.push(callee.clone());
    let this_arg_owned = this_arg.clone();
    self.roots.temporary.push(this_arg_owned.clone());
    self.roots.temporary.extend(args.iter().cloned());
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
            self.get_property_value(&handler, &PropertyKey::String("apply".to_string().into()))?;
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
          let prev_this = std::mem::replace(&mut self.execution.active.this_value, this_arg_owned);
          let prev_builtin_this =
            std::mem::replace(&mut self.execution.builtin_this_arg_prepended, true);
          let prev_closure_env = self.execution.active.closure_env.clone();
          let prev_slots = self.execution.active.slots.clone();
          let prev_lexical_env = Rc::clone(&self.execution.active.lexical_env);
          let result = self.invoke_builtin(f, &call_args);
          self.execution.builtin_this_arg_prepended = prev_builtin_this;
          self.execution.active.this_value = prev_this;
          self.execution.active.closure_env = prev_closure_env;
          self.execution.active.slots = prev_slots;
          self.execution.active.lexical_env = prev_lexical_env;
          let result = result?;
          let result_root_base = self.roots.temporary.len();
          self.roots.temporary.push(result.clone());
          let post_call = (|| {
            self.run_microtasks_if_needed()?;
            Ok::<(), RuntimeError>(())
          })();
          self.roots.temporary.truncate(result_root_base);
          post_call?;
          if matches!(
            result,
            Value::Object(_)
              | Value::Array(..)
              | Value::Closure { .. }
              | Value::BuiltinObject(_, _)
              | Value::Promise(..)
          ) {
            self.construction.recent_return_value = Some(result.clone());
          } else {
            self.construction.recent_return_value = None;
          }
          Ok(result)
        }
        Value::BuiltinObject(f, _props) => {
          let mut call_args: Vec<Value> = Vec::with_capacity(args.len() + 1);
          call_args.push(this_arg_owned.clone());
          call_args.extend_from_slice(args);
          let prev_this = std::mem::replace(&mut self.execution.active.this_value, this_arg_owned);
          let prev_builtin_this =
            std::mem::replace(&mut self.execution.builtin_this_arg_prepended, true);
          let prev_closure_env = self.execution.active.closure_env.clone();
          let prev_slots = self.execution.active.slots.clone();
          let prev_lexical_env = Rc::clone(&self.execution.active.lexical_env);
          let result = self.invoke_builtin(f, &call_args);
          self.execution.builtin_this_arg_prepended = prev_builtin_this;
          self.execution.active.this_value = prev_this;
          self.execution.active.closure_env = prev_closure_env;
          self.execution.active.slots = prev_slots;
          self.execution.active.lexical_env = prev_lexical_env;
          let result = result?;
          let result_root_base = self.roots.temporary.len();
          self.roots.temporary.push(result.clone());
          let post_call = (|| {
            self.run_microtasks_if_needed()?;
            Ok::<(), RuntimeError>(())
          })();
          self.roots.temporary.truncate(result_root_base);
          post_call?;
          if matches!(
            result,
            Value::Object(_)
              | Value::Array(..)
              | Value::Closure { .. }
              | Value::BuiltinObject(_, _)
              | Value::Promise(..)
          ) {
            self.construction.recent_return_value = Some(result.clone());
          } else {
            self.construction.recent_return_value = None;
          }
          Ok(result)
        }
        Value::Closure { proto, .. } => {
          if proto.is_class_constructor && self.construction.new_target.is_none() {
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
            let caller_context = self.execution.active.clone();
            let caller_frame_len = self.execution.call_stack.len();
            let caller_handler_len = self.exceptions.handlers.len();
            self.push_call_frame(proto, &closure_env, call_args, this_arg, callee.clone())?;
            let result = self
              .run_risc_loop(false)
              .and_then(|value| self.execute_return(value));
            if result.is_ok() || self.exceptions.pending.is_none() {
              // Restore the suspended caller state after the callee returns.
              self.execution.active = caller_context;
              self
                .execution
                .call_stack
                .truncate(caller_frame_len);
              self
                .exceptions
                .handlers
                .truncate(caller_handler_len);
            }
            result
          }
        }
        _ => {
          if crate::trace::enabled() {
            eprintln!("[trace] call_with_this: callee not callable (final fallback)");
            crate::trace::value_string("[trace]   callee", callee);
            crate::trace::value_string("[trace]   this_arg", this_arg);
          }
          self.dump_diag("CallWithThis failed: callee not callable", callee);
          Err(self.throw_type_error(&format!("{} is not a function", callee.type_name())))
        }
      }
    })();
    self.roots.temporary.truncate(temp_root_base);
    result
  }

  /// Prepares the arguments for a function call, handling rest parameters.
  /// Configures the call frame and local variables for a function call.
  #[allow(clippy::too_many_lines)]
  pub(super) fn configure_call_frame(
    &mut self,
    proto: &Rc<FunctionProto>,
    closure_env: &Rc<RefCell<Vec<SharedValue>>>,
    call_args: Vec<Value>,
    mut this_value: Value,
    callee: Value,
  ) -> Result<(), RuntimeError> {
    self.execution.pending_closure_env = Some(Rc::clone(closure_env));
    if !proto.is_strict {
      if matches!(this_value, Value::Undefined | Value::Null) {
        this_value = Value::Object(Rc::clone(&self.realm_state.globals));
      } else {
        this_value = match self.to_object_value(this_value) {
          Ok(value) => value,
          Err(err) => {
            self.execution.pending_closure_env = None;
            return Err(err);
          }
        };
      }
    }

    self.execution.active.chunk = Arc::clone(&proto.chunk);
    self.execution.active.ip = 0;
    self.execution.active.closure_env = Some(Rc::clone(closure_env));
    self.execution.active.slots = Some(SlotMetadata {
      types: proto.slot_types.clone(),
      names: proto.slot_names.clone(),
      immutable: proto.slot_immutable.clone(),
      sloppy_immutable_silent: proto.slot_sloppy_immutable_silent.clone(),
    });
    self.execution.active.yield_star = None;
    self.execution.active.this_value = if proto.is_derived_constructor {
      Value::Uninitialized
    } else {
      this_value
    };

    self.execution.active.locals =
      std::iter::repeat_with(|| Rc::new(RefCell::new(Value::Undefined)))
        .take(proto.slot_types.len().max(proto.arity))
        .collect();
    for (idx, is_tdz) in proto.lexical_tdz.iter().copied().enumerate() {
      if is_tdz && idx < self.execution.active.locals.len() {
        *self.execution.active.locals[idx].borrow_mut() = Value::Uninitialized;
      }
    }
    let captured_lexical_env = match &callee {
      Value::Closure { props, .. } => props
        .borrow()
        .captured_lexical_env()
        .unwrap_or_else(|| Rc::clone(&self.execution.active.lexical_env)),
      _ => Rc::clone(&self.execution.active.lexical_env),
    };
    let fn_env = Rc::new(RefCell::new(EnvironmentRecord::new(Some(
      captured_lexical_env,
    ))));
    for (idx, maybe_name) in proto.slot_names.iter().enumerate() {
      if let Some(name) = maybe_name
        && idx < self.execution.active.locals.len()
      {
        let initialized = !proto
          .lexical_tdz
          .get(idx)
          .copied()
          .unwrap_or(false);
        fn_env.borrow_mut().create_mutable_binding(
          name.clone(),
          Rc::clone(&self.execution.active.locals[idx]),
          initialized,
        );
      }
    }
    self.execution.active.lexical_env = fn_env;
    for (i, value) in call_args
      .iter()
      .cloned()
      .enumerate()
      .take(proto.arity)
    {
      *self.execution.active.locals[i].borrow_mut() = value;
    }
    if crate::trace::enabled() && proto.is_generator && !proto.has_simple_parameter_list {
      for (idx, slot) in self.execution.active.locals.iter().enumerate() {
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
      && slot < self.execution.active.locals.len()
    {
      *self.execution.active.locals[slot].borrow_mut() = callee.clone();
    }
    if let Some(slot) = proto.arguments_slot
      && slot < self.execution.active.locals.len()
    {
      let args_for_arguments: Vec<Option<Value>> = call_args
        .iter()
        .map(|v| Some(v.clone()))
        .collect();
      if proto.has_simple_parameter_list && !proto.is_strict {
        let mapped_count = proto.arity.min(args_for_arguments.len());
        let args_obj = self.build_mapped_arguments_object(proto, callee, &args_for_arguments);
        self.execution.active.arguments.object = Some(args_obj.clone());
        self.execution.active.arguments.mapped_count = mapped_count;
        self.execution.active.locals[slot] = Rc::new(RefCell::new(args_obj));
      } else {
        let props: Rc<RefCell<ObjectData>> =
          gc::track_object(Rc::new(RefCell::new(ObjectData::new())));
        props
          .borrow_mut()
          .set_internal_slot("[[ArgumentsData]]", Value::Bool(true));
        for (idx, maybe_value) in args_for_arguments.iter().enumerate() {
          props.borrow_mut().properties.insert(
            PropertyKey::String(usize_to_decimal_string(idx).into()),
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
            PropertyKey::String("callee".to_string().into()),
            PropertyDescriptor::Accessor {
              get: Some(poison.clone()),
              set: Some(poison.clone()),
              enumerable: false,
              configurable: false,
            },
          );
          props.borrow_mut().properties.insert(
            PropertyKey::String("caller".to_string().into()),
            PropertyDescriptor::Accessor {
              get: Some(poison.clone()),
              set: Some(poison),
              enumerable: false,
              configurable: false,
            },
          );
        }
        props.borrow_mut().properties.insert(
          PropertyKey::String("length".to_string().into()),
          PropertyDescriptor::Data {
            value: Value::Number(usize_to_f64(args_for_arguments.len())),
            writable: true,
            enumerable: false,
            configurable: true,
          },
        );
        let object_proto = self.realm_state.realm.object_prototype();
        if !matches!(object_proto, Value::Null) {
          props
            .borrow_mut()
            .set_prototype_value(object_proto);
        }
        self.install_arguments_iterator_property(&props);
        self.execution.active.arguments.object = None;
        self.execution.active.arguments.mapped_count = 0;
        self.execution.active.locals[slot] = Rc::new(RefCell::new(Value::Object(props)));
      }
    }
    self.execution.pending_closure_env = None;

    Ok(())
  }

  /// Save the caller's state and set up the callee.
  pub(super) fn push_call_frame(
    &mut self,
    proto: &Rc<FunctionProto>,
    closure_env: &Rc<RefCell<Vec<SharedValue>>>,
    call_args: Vec<Value>,
    this_value: Value,
    callee: Value,
  ) -> Result<(), RuntimeError> {
    let new_target = self.construction.new_target.take();
    let lexical_env = Rc::clone(&self.execution.active.lexical_env);
    let caller_context = std::mem::replace(
      &mut self.execution.active,
      ExecutionContext {
        chunk: Arc::clone(&proto.chunk),
        ip: 0,
        locals: Vec::with_capacity(LOCALS_INITIAL_CAPACITY),
        this_value: this_value.clone(),
        closure_env: None,
        slots: None,
        lexical_env,
        yield_star: None,
        arguments: ArgumentsState {
          object: None,
          mapped_count: 0,
        },
        strict: proto.is_strict,
      },
    );
    self.execution.call_stack.push(Frame {
      context: caller_context,
      risc_state: None,
      callee: callee.clone(),
      // Arrow functions capture the surrounding [[NewTarget]], while an
      // ordinary nested call starts with no [[NewTarget]].
      new_target,
      upvalues: proto.upvalues.clone(),
    });

    self.configure_call_frame(proto, closure_env, call_args, this_value, callee)?;

    Ok(())
  }
}
