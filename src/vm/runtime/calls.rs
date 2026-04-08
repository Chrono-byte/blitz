use crate::errors::RuntimeError;
use crate::value::Value;
use crate::vm::Vm;

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
        self.tail_call_value(callee, args, Value::Undefined)
    }

    fn tail_call_value(
        &mut self,
        callee: Value,
        args: Vec<Value>,
        this_value: Value,
    ) -> Result<(), RuntimeError> {
        self.prune_handlers_for_tail_call();
        match callee {
            Value::Builtin(f) => {
                let prev_this = std::mem::replace(&mut self.this_value, this_value);
                let prev_closure_env = self.current_closure_env.clone();
                let prev_slot_types = self.current_slot_types.clone();
                let prev_lexical_env = std::rc::Rc::clone(&self.lexical_env);
                let result = (f.as_ref())(self, &args)?;
                self.this_value = prev_this;
                self.current_closure_env = prev_closure_env;
                self.current_slot_types = prev_slot_types;
                self.lexical_env = prev_lexical_env;
                self.maybe_collect_garbage_prototype();
                self.stack.push(result);
                self.execute_return()
            }
            Value::BuiltinObject(f, _props) => {
                let prev_this = std::mem::replace(&mut self.this_value, this_value);
                let prev_closure_env = self.current_closure_env.clone();
                let prev_slot_types = self.current_slot_types.clone();
                let prev_lexical_env = std::rc::Rc::clone(&self.lexical_env);
                let result = (f.as_ref())(self, &args)?;
                self.this_value = prev_this;
                self.current_closure_env = prev_closure_env;
                self.current_slot_types = prev_slot_types;
                self.lexical_env = prev_lexical_env;
                self.maybe_collect_garbage_prototype();
                self.stack.push(result);
                self.execute_return()
            }
            Value::Closure {
                ref proto, ref env, ..
            } => {
                let call_args = self.prepare_call_args(&args, proto)?;
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
        if crate::trace::enabled() {
            eprintln!(
                "[trace] execute_tail_call_method after pops stack_size={} receiver={:?} callee={:?}",
                self.stack.len(),
                receiver,
                callee
            );
        }
        self.tail_call_value(callee, args, receiver)
    }
}
