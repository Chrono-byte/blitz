use crate::bytecode::FunctionProto;
use crate::environment::EnvironmentRecord;
use crate::errors::RuntimeError;
use crate::gc;
use crate::mutex_compat::MutexBorrowExt;
use crate::runtime_utils::{array_from_values, usize_to_decimal_string};
use crate::value::{ObjectData, PropertyDescriptor, PropertyKey, SymbolId, Value, ValueSlot};
use crate::vm::Vm;
use crate::vm::runtime::execution::get_property;
use crate::vm::state::{Frame, GeneratorRunner, LOCALS_INITIAL_CAPACITY, StepOutcome};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Mutex;

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
                let result_root_base = self.gc_temp_roots.len();
                self.gc_temp_roots.push(result.clone());
                self.maybe_collect_garbage_prototype();
                self.gc_temp_roots.truncate(result_root_base);
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
                let result_root_base = self.gc_temp_roots.len();
                self.gc_temp_roots.push(result.clone());
                self.maybe_collect_garbage_prototype();
                self.gc_temp_roots.truncate(result_root_base);
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
                let result_root_base = self.gc_temp_roots.len();
                self.gc_temp_roots.push(result.clone());
                if self.call_stack.is_empty() {
                    self.run_microtasks_if_needed()?;
                }
                self.maybe_collect_garbage_prototype();
                self.gc_temp_roots.truncate(result_root_base);
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
                let result_root_base = self.gc_temp_roots.len();
                self.gc_temp_roots.push(result.clone());
                if self.call_stack.is_empty() {
                    self.run_microtasks_if_needed()?;
                }
                self.maybe_collect_garbage_prototype();
                self.gc_temp_roots.truncate(result_root_base);
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
                let result_root_base = self.gc_temp_roots.len();
                self.gc_temp_roots.push(result.clone());
                if self.call_stack.is_empty() {
                    self.run_microtasks_if_needed()?;
                }
                self.maybe_collect_garbage_prototype();
                self.gc_temp_roots.truncate(result_root_base);
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
                let result_root_base = self.gc_temp_roots.len();
                self.gc_temp_roots.push(result.clone());
                if self.call_stack.is_empty() {
                    self.run_microtasks_if_needed()?;
                }
                self.maybe_collect_garbage_prototype();
                self.gc_temp_roots.truncate(result_root_base);
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
    pub(super) fn create_generator_object(
        &mut self,
        proto: &Rc<FunctionProto>,
        closure_env: &Rc<Mutex<Vec<ValueSlot>>>,
        call_args: Vec<Value>,
        this_value: Value,
        callee: &Value,
    ) -> Result<Value, RuntimeError> {
        let mut child = self.fork_execution();
        child.push_call_frame(proto, closure_env, call_args, this_value, callee.clone())?;
        let runner = Rc::new(RefCell::new(GeneratorRunner {
            vm: child,
            started: false,
            completed: false,
        }));
        let next_runner = Rc::clone(&runner);
        let throw_runner = Rc::clone(&runner);
        let return_runner = Rc::clone(&runner);
        let gen_obj = gc::track_object(Rc::new(RefCell::new(ObjectData::new())));
        gen_obj
            .borrow_mut()
            .set_prototype_value(self.realm.generator_prototype());
        gen_obj.borrow_mut().properties.insert(
            PropertyKey::String("next".to_string()),
            PropertyDescriptor::data(Value::Builtin(Rc::new(move |_vm, args| {
                let mut runner = next_runner
                    .try_borrow_mut()
                    .map_err(|_| RuntimeError::from("TypeError: generator is already executing"))?;
                let first_arg = args.first().cloned().unwrap_or(Value::Undefined);

                if runner.completed {
                    Ok(Self::make_iterator_result(Value::Undefined, true))
                } else {
                    if !runner.started {
                        runner.started = true;
                    } else {
                        runner.vm.stack.push(first_arg);
                    }

                    loop {
                        match runner.vm.step(Some(1)) {
                            Ok(StepOutcome::Continue) => {
                                if let Err(err) = runner.vm.run_microtasks_if_needed() {
                                    break Err(err);
                                }
                                runner.vm.maybe_collect_garbage_prototype();
                            }
                            Ok(StepOutcome::Yield(v)) => {
                                break Ok(Self::make_iterator_result(v, false));
                            }
                            Ok(StepOutcome::Return(v)) => {
                                runner.completed = true;
                                break Ok(Self::make_iterator_result(v, true));
                            }
                            Err(err) => break Err(err),
                        }
                    }
                }
            }))),
        );
        gen_obj.borrow_mut().properties.insert(
            PropertyKey::String("throw".to_string()),
            PropertyDescriptor::data(Value::Builtin(Rc::new(move |_vm, args| {
                let mut runner = throw_runner
                    .try_borrow_mut()
                    .map_err(|_| RuntimeError::from("TypeError: generator is already executing"))?;
                let thrown = args.first().cloned().unwrap_or(Value::Undefined);

                if runner.completed {
                    Err(RuntimeError::from(format!(
                        "Uncaught: {}",
                        runner.vm.format_uncaught_thrown_value(&thrown)
                    )))
                } else {
                    if let Err(err) = runner.vm.throw_value(thrown) {
                        runner.completed = true;
                        Err(err)
                    } else {
                        loop {
                            match runner.vm.step(Some(1)) {
                                Ok(StepOutcome::Continue) => {
                                    if let Err(e) = runner.vm.run_microtasks_if_needed() {
                                        runner.completed = true;
                                        break Err(e);
                                    }
                                }
                                Ok(StepOutcome::Yield(v)) => {
                                    break Ok(Self::make_iterator_result(v, false));
                                }
                                Ok(StepOutcome::Return(v)) => {
                                    runner.completed = true;
                                    break Ok(Self::make_iterator_result(v, true));
                                }
                                Err(e) => {
                                    runner.completed = true;
                                    break Err(e);
                                }
                            }
                        }
                    }
                }
            }))),
        );
        gen_obj.borrow_mut().properties.insert(
            PropertyKey::String("return".to_string()),
            PropertyDescriptor::data(Value::Builtin(Rc::new(move |_vm, args| {
                let mut runner = return_runner
                    .try_borrow_mut()
                    .map_err(|_| RuntimeError::from("TypeError: generator is already executing"))?;
                let result_value = args.first().cloned().unwrap_or(Value::Undefined);
                runner.completed = true;
                Ok(Self::make_iterator_result(result_value, true))
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
                    eprintln!("[trace]     {:04} 0x{:02x}", i, self.chunk.code[i]);
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
            Value::Closure { .. } => {
                let proto = self
                    .get_property_value(callee, &PropertyKey::String("prototype".to_string()))
                    .unwrap_or(Value::Null);
                let instance_proto = match proto {
                    Value::Object(_)
                    | Value::Array(..)
                    | Value::Closure { .. }
                    | Value::BuiltinObject(_, _) => proto,
                    _ => Value::Null,
                };
                let instance_obj = gc::track_object(Rc::new(RefCell::new(
                    ObjectData::with_prototype(instance_proto),
                )));
                self.property_get_ic_cache.clear();
                instance_obj.borrow_mut().properties.insert(
                    PropertyKey::String("constructor".to_string()),
                    PropertyDescriptor::data(callee.clone()),
                );
                let instance = Value::Object(instance_obj);
                let prev_new_target = self.construct_new_target.clone();
                self.construct_new_target = Some(callee.clone());
                let result = self.call_with_this(callee, &instance, args);
                self.construct_new_target = prev_new_target;
                let result = result?;
                let result = match result {
                    Value::Object(_)
                    | Value::Array(..)
                    | Value::Closure { .. }
                    | Value::BuiltinObject(_, _)
                    | Value::Promise(_) => {
                        let _ = set_constructor_property(&result, callee);
                        result
                    }
                    _ => instance,
                };
                let _ = set_constructor_property(&result, callee);
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
                self.gc_temp_roots.truncate(temp_root_base);
                Ok(result)
            }
            Value::BuiltinObject(_, _) => {
                let proto = self
                    .get_property_value(callee, &PropertyKey::String("prototype".to_string()))
                    .unwrap_or(Value::Null);
                let instance_proto = match proto {
                    Value::Object(_)
                    | Value::Array(..)
                    | Value::Closure { .. }
                    | Value::BuiltinObject(_, _) => proto,
                    _ => Value::Null,
                };
                let instance_obj = gc::track_object(Rc::new(RefCell::new(
                    ObjectData::with_prototype(instance_proto),
                )));
                self.property_get_ic_cache.clear();
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
                self.construct_new_target = Some(callee.clone());
                let result = self.call_with_this(callee, &instance, args);
                self.construct_new_target = prev_new_target;
                let result = result?;
                let result = match result {
                    Value::Object(_)
                    | Value::Array(..)
                    | Value::Closure { .. }
                    | Value::BuiltinObject(_, _)
                    | Value::Promise(_) => {
                        let _ = set_constructor_property(&result, callee);
                        result
                    }
                    _ => instance,
                };
                let _ = set_constructor_property(&result, callee);
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
                self.gc_temp_roots.truncate(temp_root_base);
                Ok(result)
            }
            Value::Builtin(f) => {
                let prev_builtin_this =
                    std::mem::replace(&mut self.builtin_this_arg_prepended, false);
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
        let result = (|| match callee {
            Value::Builtin(f) => {
                // For builtins invoked with an explicit `this`, pass the receiver
                // as the first argument so native functions that expect the
                // receiver can access it (used by accessor getters/setters).
                let mut call_args: Vec<Value> = Vec::with_capacity(args.len() + 1);
                call_args.push(this_arg_owned.clone());
                call_args.extend_from_slice(args);
                let prev_this = std::mem::replace(&mut self.this_value, this_arg_owned);
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
                let mut call_args: Vec<Value> = Vec::with_capacity(args.len());
                call_args.extend_from_slice(args);
                let call_args = self.prepare_call_args(&call_args, proto)?;
                self.validate_call_args(&call_args, proto)?;
                let closure_env = match callee {
                    Value::Closure { env, .. } => Rc::clone(env),
                    _ => unreachable!(),
                };
                let this_arg = Self::closure_lexical_this(callee).unwrap_or(this_arg.clone());
                if proto.is_generator {
                    Ok(self.create_generator_object(
                        proto,
                        &closure_env,
                        call_args,
                        this_arg,
                        callee,
                    )?)
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
                    let caller_frame_len = self.call_stack.len();
                    let caller_handler_len = self.handlers.len();
                    self.push_call_frame(proto, &closure_env, call_args, this_arg, callee.clone())?;
                    let result = self.run_until_return(stack_base, true);
                    if result.is_ok() || !self.pending_exception {
                        // Restore caller state (run_until_return already truncated stack)
                        self.chunk = caller_chunk;
                        self.ip = caller_ip;
                        self.locals = caller_locals;
                        self.this_value = caller_this;
                        self.current_closure_env = caller_env;
                        self.current_slot_types = caller_slot_types;
                        self.stack.restore_from(saved_stack);
                        self.call_stack.truncate(caller_frame_len);
                        self.current_strict_mode =
                            self.call_stack.last().is_some_and(|f| f.is_strict);
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
                Err(RuntimeError::from("CallWithThis: not a function"))
            }
        })();
        self.gc_temp_roots.truncate(temp_root_base);
        result
    }

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

        let arr_rc = Rc::new(RefCell::new(rest_slice));
        let props: Rc<RefCell<ObjectData>> =
            gc::track_object(Rc::new(RefCell::new(ObjectData::new())));
        props.borrow_mut().properties.insert(
            PropertyKey::String("length".to_string()),
            PropertyDescriptor::data(Value::Number(arr_rc.borrow().len() as f64)),
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
            if let Value::Array(_arr, props_rc) = fixed.last().cloned().unwrap() {
                eprintln!(
                    "[trace] prepare_call_args: created rest-Array props_ptr={:p} proto='{}'",
                    Rc::as_ptr(&props_rc),
                    proto.name
                );
            }
        }
        Ok(fixed)
    }

    fn validate_call_args(
        &self,
        _call_args: &[Value],
        _proto: &FunctionProto,
    ) -> Result<(), RuntimeError> {
        // Argument type validation is intentionally disabled in JS-first mode.
        // TypeScript annotations are treated as comments and should not affect
        // runtime behaviour. Keep this function for tooling and future opt-in
        // checked modes.
        Ok(())
    }

    pub(super) fn configure_call_frame(
        &mut self,
        proto: &Rc<FunctionProto>,
        closure_env: &Rc<Mutex<Vec<ValueSlot>>>,
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
        self.yield_star_queue = None;
        self.this_value = this_value;

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
                let initialized = !proto.lexical_tdz.get(idx).copied().unwrap_or(false);
                fn_env.borrow_mut().create_mutable_binding(
                    name.clone(),
                    Rc::clone(&self.locals[idx]),
                    initialized,
                );
            }
        }
        self.lexical_env = fn_env;
        for (i, value) in call_args.iter().cloned().enumerate().take(proto.arity) {
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
            let args_for_arguments: Vec<Option<Value>> =
                call_args.iter().map(|v| Some(v.clone())).collect();
            if proto.has_simple_parameter_list && !proto.is_strict {
                let mapped_count = proto.arity.min(args_for_arguments.len());
                let args_obj =
                    self.build_mapped_arguments_object(proto, callee, &args_for_arguments);
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
                        Err(vm.throw_type_error(
                            "Access to strict arguments callee/caller is forbidden",
                        ))
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
                        value: Value::Number(args_for_arguments.len() as f64),
                        writable: true,
                        enumerable: false,
                        configurable: true,
                    },
                );
                let object_proto = self.realm.object_prototype();
                if !matches!(object_proto, Value::Null) {
                    props.borrow_mut().set_prototype_value(object_proto);
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
        closure_env: &Rc<Mutex<Vec<ValueSlot>>>,
        call_args: Vec<Value>,
        this_value: Value,
        callee: Value,
    ) -> Result<(), RuntimeError> {
        let caller_stack = self.stack.take_all();
        let stack_base = caller_stack.len();

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
            new_target: self.construct_new_target.clone(),
            closure_env: self.current_closure_env.take(),
            upvalues: proto.upvalues.clone(),
            slot_types: self.current_slot_types.take(),
            slot_names: self.current_slot_names.take(),
            stack_base,
            is_strict: proto.is_strict,
            lexical_env: Rc::clone(&self.lexical_env),
            yield_star_queue: self.yield_star_queue.take(),
            arguments_object: self.current_arguments_object.take(),
            arguments_mapped_count: self.current_arguments_mapped_count,
        });
        self.current_strict_mode = proto.is_strict;

        self.configure_call_frame(proto, closure_env, call_args, this_value, callee)?;

        Ok(())
    }

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
                value: Value::Number(arg_count as f64),
                writable: true,
                enumerable: false,
                configurable: true,
            },
        );
        let object_proto = self.realm.object_prototype();
        if !matches!(object_proto, Value::Null) {
            obj_rc.borrow_mut().set_prototype_value(object_proto);
        }
        self.install_arguments_iterator_property(&obj_rc);
        Value::Object(obj_rc)
    }

    fn install_arguments_iterator_property(&mut self, obj_rc: &Rc<RefCell<ObjectData>>) {
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
