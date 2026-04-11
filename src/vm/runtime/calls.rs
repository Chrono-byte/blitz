use crate::errors::RuntimeError;
use crate::runtime_utils::array_from_values;
use crate::value::{PropertyKey, SymbolId, Value};
use crate::vm::Vm;
use std::rc::Rc;

impl Vm {
    fn prune_handlers_for_tail_call(&mut self) {
        let active_frames = self.call_stack.len();
        while self
            .handlers
            .last()
            .is_some_and(|h| h.frame_count >= active_frames)
        {
            self.handlers.pop();
        }
    }

    pub(super) fn execute_tail_call_with_nargs(
        &mut self,
        nargs_override: Option<usize>,
    ) -> Result<(), RuntimeError> {
        let nargs = nargs_override.unwrap_or(self.read_byte()? as usize);
        let mut args: Vec<Value> = Vec::with_capacity(nargs);
        for _ in 0..nargs {
            args.push(self.pop()?);
        }
        args.reverse();
        let callee = self.pop()?;
        self.tail_call_value(callee, args, Value::Undefined, false)
    }

    fn tail_call_value(
        &mut self,
        callee: Value,
        args: Vec<Value>,
        this_value: Value,
        prepend_this_for_builtins: bool,
    ) -> Result<(), RuntimeError> {
        self.prune_handlers_for_tail_call();
        match callee {
            Value::Builtin(f) => {
                let mut call_args: Vec<Value> =
                    Vec::with_capacity(args.len() + usize::from(prepend_this_for_builtins));
                if prepend_this_for_builtins {
                    call_args.push(this_value.clone());
                }
                call_args.extend_from_slice(&args);
                let prev_this = std::mem::replace(&mut self.this_value, this_value);
                let prev_builtin_this = std::mem::replace(
                    &mut self.builtin_this_arg_prepended,
                    prepend_this_for_builtins,
                );
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
                self.maybe_collect_garbage_prototype();
                self.stack.push(result);
                self.execute_return()
            }
            Value::BuiltinObject(f, _props) => {
                let mut call_args: Vec<Value> =
                    Vec::with_capacity(args.len() + usize::from(prepend_this_for_builtins));
                if prepend_this_for_builtins {
                    call_args.push(this_value.clone());
                }
                call_args.extend_from_slice(&args);
                let prev_this = std::mem::replace(&mut self.this_value, this_value);
                let prev_builtin_this = std::mem::replace(
                    &mut self.builtin_this_arg_prepended,
                    prepend_this_for_builtins,
                );
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
                self.maybe_collect_garbage_prototype();
                self.stack.push(result);
                self.execute_return()
            }
            Value::Closure {
                ref proto, ref env, ..
            } => {
                let call_args = self.prepare_call_args(&args, proto)?;
                let this_value = match &callee {
                    Value::Closure {
                        lexical_this: Some(this),
                        ..
                    } => (**this).clone(),
                    _ => this_value,
                };
                if proto.is_generator {
                    let generator =
                        self.create_generator_object(proto, env, call_args, this_value, &callee)?;
                    self.stack.push(generator);
                    self.execute_return()
                } else if proto.is_async {
                    let promise =
                        self.invoke_async_closure(proto, env, call_args, this_value, &callee)?;
                    self.stack.push(promise);
                    self.execute_return()
                } else {
                    self.configure_call_frame(proto, env, call_args, this_value, callee.clone())
                }
            }
            _ => {
                if crate::trace::enabled() {
                    eprintln!("[trace] tail_call_value: callee not callable");
                    crate::trace::value_string("[trace]   callee", &callee);
                }
                self.dump_diag("Tail call failed: callee not callable", &callee);
                Err(self.throw_type_error(&format!("{} is not a function", callee.type_name())))
            }
        }
    }

    pub(super) fn execute_tail_call(&mut self) -> Result<(), RuntimeError> {
        self.execute_tail_call_with_nargs(None)
    }

    pub(super) fn execute_tail_call_method(&mut self) -> Result<(), RuntimeError> {
        let nargs = self.read_byte()? as usize;
        if crate::trace::enabled() {
            eprintln!(
                "[trace] execute_tail_call_method nargs={} stack_size={} ip={}",
                nargs,
                self.stack.len(),
                self.ip.saturating_sub(1)
            );
            for (i, v) in self.stack.iter_rev().take(6).enumerate() {
                crate::trace::value_string(&format!("[trace]   stack[{}]", i), v);
            }
        }
        let mut args: Vec<Value> = Vec::with_capacity(nargs);
        for _ in 0..nargs {
            args.push(self.pop()?);
        }
        args.reverse();
        let callee = self.pop()?;
        let receiver = self.pop()?;
        let receiver = match &callee {
            Value::Closure {
                lexical_this: Some(this),
                ..
            } => (**this).clone(),
            _ => receiver,
        };
        if crate::trace::enabled() {
            eprintln!(
                "[trace] execute_tail_call_method after pops stack_size={} receiver={:?} callee={:?}",
                self.stack.len(),
                receiver,
                callee
            );
        }
        self.tail_call_value(callee, args, receiver, true)
    }
    pub(super) fn execute_call_with_nargs(
        &mut self,
        nargs_override: Option<usize>,
    ) -> Result<(), RuntimeError> {
        let nargs = nargs_override.unwrap_or(self.read_byte()? as usize);
        let mut args: Vec<Value> = Vec::with_capacity(nargs);
        for _ in 0..nargs {
            args.push(self.pop()?);
        }
        args.reverse();
        let callee = self.pop()?;

        match callee {
            Value::Builtin(f) => {
                let prev_this = std::mem::replace(&mut self.this_value, Value::Undefined);
                let prev_builtin_this =
                    std::mem::replace(&mut self.builtin_this_arg_prepended, false);
                let prev_closure_env = self.current_closure_env.clone();
                let prev_slot_types = self.current_slot_types.clone();
                let prev_lexical_env = Rc::clone(&self.lexical_env);
                let result = (f.as_ref())(self, &args);
                self.builtin_this_arg_prepended = prev_builtin_this;
                self.this_value = prev_this;
                self.current_closure_env = prev_closure_env;
                self.current_slot_types = prev_slot_types;
                self.lexical_env = prev_lexical_env;
                let result = result?;
                if self.call_stack.is_empty() {
                    self.run_microtasks_if_needed()?;
                }
                self.maybe_collect_garbage_prototype();
                self.stack.push(result);
            }
            Value::BuiltinObject(f, _props) => {
                let prev_this = std::mem::replace(&mut self.this_value, Value::Undefined);
                let prev_builtin_this =
                    std::mem::replace(&mut self.builtin_this_arg_prepended, false);
                let prev_closure_env = self.current_closure_env.clone();
                let prev_slot_types = self.current_slot_types.clone();
                let prev_lexical_env = Rc::clone(&self.lexical_env);
                let result = (f.as_ref())(self, &args);
                self.builtin_this_arg_prepended = prev_builtin_this;
                self.this_value = prev_this;
                self.current_closure_env = prev_closure_env;
                self.current_slot_types = prev_slot_types;
                self.lexical_env = prev_lexical_env;
                let result = result?;
                if self.call_stack.is_empty() {
                    self.run_microtasks_if_needed()?;
                }
                self.maybe_collect_garbage_prototype();
                self.stack.push(result);
            }
            Value::Closure {
                ref proto, ref env, ..
            } => {
                let call_args = self.prepare_call_args(&args, proto)?;
                // Argument validation removed in JS-first mode.
                let this_arg = Self::closure_lexical_this(&callee).unwrap_or(Value::Undefined);
                if proto.is_generator {
                    let generator =
                        self.create_generator_object(proto, env, call_args, this_arg, &callee)?;
                    self.stack.push(generator);
                } else if proto.is_async {
                    let promise =
                        self.invoke_async_closure(proto, env, call_args, this_arg, &callee)?;
                    self.stack.push(promise);
                } else {
                    self.push_call_frame(proto, env, call_args, this_arg, callee.clone())?;
                }
            }
            _ => {
                if crate::trace::enabled() {
                    eprintln!("[trace] execute_call: callee not callable, dumping context...");
                    crate::trace::value_string("[trace]   callee", &callee);
                    for (i, v) in self.stack.iter_rev().take(8).enumerate() {
                        crate::trace::value_string(&format!("[trace]   stack[{}]", i), v);
                    }
                    // Try to locate the callee value inside any known constant
                    // pools referenced by the active frames to help map origins.
                    let matches = self.find_constants_containing_value(&callee);
                    if !matches.is_empty() {
                        for (fi, ci) in matches {
                            eprintln!(
                                "[trace]   callee appears in frame[{}] constants[{}]",
                                fi, ci
                            );
                        }
                    }
                }
                self.dump_diag("Call failed: callee not callable", &callee);
                return Err(
                    self.throw_type_error(&format!("{} is not a function", callee.type_name()))
                );
            }
        }

        Ok(())
    }

    pub(super) fn execute_call(&mut self) -> Result<(), RuntimeError> {
        self.execute_call_with_nargs(None)
    }

    pub(super) fn execute_construct(&mut self) -> Result<(), RuntimeError> {
        let nargs = self.read_byte()? as usize;
        let mut args: Vec<Value> = Vec::with_capacity(nargs);
        for _ in 0..nargs {
            args.push(self.pop()?);
        }
        args.reverse();
        let callee = self.pop()?;
        let result = self.construct_value(&callee, &args)?;
        self.stack.push(result);
        Ok(())
    }

    pub(super) fn execute_spread(&mut self) -> Result<(), RuntimeError> {
        self.read_byte()?; // nargs, consumed by VM dispatcher
        let mask = self.pop()?;
        let values = self.pop()?;
        let callee = self.pop()?;
        let flat = self.flatten_spread_values(values, mask)?;
        self.call_with_flat_args(callee, flat)?;
        Ok(())
    }

    pub(super) fn execute_array_from_spread(&mut self) -> Result<(), RuntimeError> {
        self.read_byte()?; // nargs, consumed by VM dispatcher
        let mask = self.pop()?;
        let values = self.pop()?;
        let flat = self.flatten_spread_values(values, mask)?;
        self.stack.push(array_from_values(flat));
        Ok(())
    }

    pub(super) fn flatten_spread_values(
        &mut self,
        values: Value,
        mask: Value,
    ) -> Result<Vec<Value>, RuntimeError> {
        let values_vec = match values {
            Value::Array(arr, _) => arr.borrow().clone(),
            _ => Vec::new(),
        };
        let mask_vec = match mask {
            Value::Array(arr, _) => arr.borrow().clone(),
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
                    let next_fn = self
                        .get_property_host(&iter_obj, &PropertyKey::String("next".to_string()))?;
                    let result = self.call_value_with_this(&next_fn, &iter_obj, &[])?;
                    let done_val =
                        self.get_property_host(&result, &PropertyKey::String("done".to_string()))?;
                    if self.to_boolean(&done_val) {
                        break;
                    }
                    let val =
                        self.get_property_host(&result, &PropertyKey::String("value".to_string()))?;
                    flat.push(val);
                }
            }
        }

        Ok(flat)
    }

    pub(super) fn call_with_flat_args(
        &mut self,
        callee: Value,
        flat: Vec<Value>,
    ) -> Result<(), RuntimeError> {
        match callee {
            Value::Builtin(f) => {
                let prev_this = std::mem::replace(&mut self.this_value, Value::Undefined);
                let prev_builtin_this =
                    std::mem::replace(&mut self.builtin_this_arg_prepended, false);
                let prev_closure_env = self.current_closure_env.clone();
                let prev_slot_types = self.current_slot_types.clone();
                let prev_lexical_env = Rc::clone(&self.lexical_env);
                let result = (f.as_ref())(self, &flat);
                self.builtin_this_arg_prepended = prev_builtin_this;
                self.this_value = prev_this;
                self.current_closure_env = prev_closure_env;
                self.current_slot_types = prev_slot_types;
                self.lexical_env = prev_lexical_env;
                let result = result?;
                if self.call_stack.is_empty() {
                    self.run_microtasks_if_needed()?;
                }
                self.maybe_collect_garbage_prototype();
                self.stack.push(result);
            }
            Value::BuiltinObject(f, _props) => {
                let prev_this = std::mem::replace(&mut self.this_value, Value::Undefined);
                let prev_builtin_this =
                    std::mem::replace(&mut self.builtin_this_arg_prepended, false);
                let prev_closure_env = self.current_closure_env.clone();
                let prev_slot_types = self.current_slot_types.clone();
                let prev_lexical_env = Rc::clone(&self.lexical_env);
                let result = (f.as_ref())(self, &flat);
                self.builtin_this_arg_prepended = prev_builtin_this;
                self.this_value = prev_this;
                self.current_closure_env = prev_closure_env;
                self.current_slot_types = prev_slot_types;
                self.lexical_env = prev_lexical_env;
                let result = result?;
                if self.call_stack.is_empty() {
                    self.run_microtasks_if_needed()?;
                }
                self.maybe_collect_garbage_prototype();
                self.stack.push(result);
            }
            Value::Closure {
                ref proto, ref env, ..
            } => {
                let call_args = self.prepare_call_args(&flat, proto)?;
                let this_arg = Self::closure_lexical_this(&callee).unwrap_or(Value::Undefined);
                if proto.is_generator {
                    let generator =
                        self.create_generator_object(proto, env, call_args, this_arg, &callee)?;
                    self.stack.push(generator);
                } else if proto.is_async {
                    let promise =
                        self.invoke_async_closure(proto, env, call_args, this_arg, &callee)?;
                    self.stack.push(promise);
                } else {
                    self.push_call_frame(proto, env, call_args, this_arg, callee.clone())?;
                }
            }
            _ => {
                self.dump_diag("Spread call failed: callee not callable", &callee);
                return Err(
                    self.throw_type_error(&format!("{} is not a function", callee.type_name()))
                );
            }
        }

        Ok(())
    }

    pub(super) fn execute_call_ic(&mut self) -> Result<(), RuntimeError> {
        self.execute_call_with_nargs(None)
    }

    pub(super) fn execute_call_method(&mut self) -> Result<(), RuntimeError> {
        let nargs = self.read_byte()? as usize;
        if crate::trace::enabled() {
            eprintln!(
                "[trace] execute_call_method nargs={} stack_size={} ip={}",
                nargs,
                self.stack.len(),
                self.ip.saturating_sub(1)
            );
            for (i, v) in self.stack.iter_rev().take(6).enumerate() {
                crate::trace::value_string(&format!("[trace]   stack[{}]", i), v);
            }
        }
        let mut args: Vec<Value> = Vec::with_capacity(nargs);
        for _ in 0..nargs {
            args.push(self.pop()?);
        }
        args.reverse();
        let callee = self.pop()?;
        let receiver = self.pop()?;
        if crate::trace::enabled() {
            eprintln!(
                "[trace] execute_call_method after pops stack_size={} receiver={:?} callee={:?}",
                self.stack.len(),
                receiver,
                callee
            );
        }
        match callee {
            Value::Builtin(f) => {
                // Builtins invoked as methods receive the receiver as the
                // first argument.
                let mut call_args: Vec<Value> = Vec::with_capacity(args.len() + 1);
                call_args.push(receiver.clone());
                call_args.extend_from_slice(&args);
                let prev_this = std::mem::replace(&mut self.this_value, receiver);
                let prev_builtin_this =
                    std::mem::replace(&mut self.builtin_this_arg_prepended, true);
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
                if self.call_stack.is_empty() {
                    self.run_microtasks_if_needed()?;
                }
                self.stack.push(result);
            }
            Value::BuiltinObject(f, _props) => {
                let mut call_args: Vec<Value> = Vec::with_capacity(args.len() + 1);
                call_args.push(receiver.clone());
                call_args.extend_from_slice(&args);
                let prev_this = std::mem::replace(&mut self.this_value, receiver);
                let prev_builtin_this =
                    std::mem::replace(&mut self.builtin_this_arg_prepended, true);
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
                if self.call_stack.is_empty() {
                    self.run_microtasks_if_needed()?;
                }
                self.stack.push(result);
            }
            Value::Closure {
                ref proto, ref env, ..
            } => {
                let call_args = self.prepare_call_args(&args, proto)?;
                // Argument validation removed in JS-first mode.
                let receiver = Self::closure_lexical_this(&callee).unwrap_or(receiver);
                if proto.is_generator {
                    let generator =
                        self.create_generator_object(proto, env, call_args, receiver, &callee)?;
                    self.stack.push(generator);
                } else if proto.is_async {
                    let promise =
                        self.invoke_async_closure(proto, env, call_args, receiver, &callee)?;
                    self.stack.push(promise);
                } else {
                    self.push_call_frame(proto, env, call_args, receiver, callee.clone())?;
                }
            }
            Value::Undefined => {
                self.dump_diag("CallWithThis failed: callee not callable", &callee);
                return Err(self.throw_type_error("undefined is not a function"));
            }
            Value::Null => {
                return Err(self.throw_type_error("null is not a function"));
            }
            _ => {
                // Debug: print what value type we got and nearby stack when tracing
                if crate::trace::enabled() {
                    eprintln!(
                        "[trace] execute_call_method: callee not callable, receiver and stack follow"
                    );
                    crate::trace::value_string("[trace]   callee", &callee);
                    crate::trace::value_string("[trace]   receiver", &receiver);
                    for (i, v) in self.stack.iter_rev().take(12).enumerate() {
                        crate::trace::value_string(&format!("[trace]   stack[{}]", i), v);
                    }
                    // Attempt to locate the callee in constants for active frames
                    let matches = self.find_constants_containing_value(&callee);
                    if !matches.is_empty() {
                        for (fi, ci) in matches {
                            eprintln!(
                                "[trace]   callee appears in frame[{}] constants[{}]",
                                fi, ci
                            );
                        }
                    }
                } else {
                    eprintln!(
                        "CallWithThis ERROR: callee={:?}, receiver={:?}",
                        callee, receiver
                    );
                }
                self.dump_diag("CallWithThis failed: callee not callable", &callee);
                return Err(
                    self.throw_type_error(&format!("{} is not a function", callee.type_name()))
                );
            }
        }
        Ok(())
    }
}
