#![allow(trivial_numeric_casts)]
// SPDX-License-Identifier: MPL-2.0

use crate::bytecode::{
  Chunk, Op, VerifiedChunk, decode_aux8, decode_compare_jump, decode_rd, decode_risc_aux16,
  decode_risc_cond_offset, decode_rs,
};
use crate::environment::EnvironmentRecord;
use crate::errors::RuntimeError;
use crate::gc;
use crate::runtime_utils::{array_from_values, number_exponentiate, usize_to_f64};
#[cfg(test)]
use crate::value::PromiseState;
use crate::value::{ObjectData, PropertyDescriptor, PropertyKey, SharedValue, Value};
use crate::vm::runtime::property::{set_property, values_equal};
use crate::vm::state::*;
use js_string::JsStringBuilder;
use num_traits::{ToPrimitive, Zero};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

pub(super) enum YieldStarStep {
  Yield(Value),
  Complete(Value),
  Return(Value),
}

pub(crate) struct NestedRunFailure {
  pub(crate) error: RuntimeError,
  pub(crate) thrown: Option<Value>,
}

impl Vm {
  pub fn run(
    &mut self,
    chunk: Chunk,
  ) -> Result<Value, RuntimeError> {
    chunk
      .verify()
      .map_err(|error| RuntimeError::from(format!("Invalid bytecode: {}", error)))?;
    self.run_risc_root(Arc::new(chunk))
  }

  /// Execute one interactive submission and retain its top-level bindings.
  ///
  /// Normal script execution receives a fresh top-level lexical environment.
  /// A REPL instead behaves as one continuing session, so declarations from a
  /// successful submission must remain visible to later submissions.
  pub fn run_repl(
    &mut self,
    chunk: Chunk,
  ) -> Result<Value, RuntimeError> {
    chunk
      .verify()
      .map_err(|error| RuntimeError::from(format!("Invalid bytecode: {}", error)))?;
    let result = self.run_risc_root(Arc::new(chunk))?;
    self.persist_root_bindings();
    Ok(result)
  }

  /// Execute bytecode that has already passed structural verification.
  pub fn run_verified(
    &mut self,
    chunk: &VerifiedChunk,
  ) -> Result<Value, RuntimeError> {
    self.run_risc_root(chunk.chunk())
  }

  fn run_risc_root(
    &mut self,
    chunk_rc: Arc<Chunk>,
  ) -> Result<Value, RuntimeError> {
    self.execution.root_chunk = Some(Arc::clone(&chunk_rc));
    self.execution.active.chunk = chunk_rc;
    self.execution.active.ip = 0;
    self.execution.active.locals.clear();
    self.execution.call_stack.clear();
    self.execution.active.strict = self.execution.active.chunk.root_strict;
    self.exceptions.handlers.clear();
    self.exceptions.pending = None;
    self.exceptions.pending_frame_count = None;
    self.exceptions.unwound_risc_state = None;
    self.caches.property_get.clear();
    self.execution.active.slots = None;
    self.execution.direct_eval_outer = None;
    self.execution.active.closure_env = None;
    self.execution.pending_closure_env = None;
    self.roots.temporary.clear();
    self.roots.kept_alive.clear();
    self.execution.active.lexical_env = Rc::clone(&self.realm_state.global_env);
    self.execution.active.yield_star = None;
    self.execution.generator_return_frame_count = None;
    self.construction.new_target = None;
    self.construction.recent_return_value = None;
    self.construction.derived_this.clear();
    self.exceptions.last_uncaught = None;
    self.execution.active.arguments.object = None;
    self.execution.active.arguments.mapped_count = 0;
    self.execution.active.this_value = self
      .realm_state
      .globals
      .borrow()
      .properties
      .get(&PropertyKey::String("globalThis".to_string().into()))
      .and_then(|descriptor| match descriptor {
        PropertyDescriptor::Data { value, .. } => Some(value.clone()),
        _ => None,
      })
      .unwrap_or(Value::Undefined);
    self.initialize_root_bindings();
    gc::reset_allocation_pressure();
    self.begin_opcode_profile_run();

    self.run_risc_loop(true)
  }

  pub(super) fn run_risc_loop(
    &mut self,
    _root_execution: bool,
  ) -> Result<Value, RuntimeError> {
    let function_id = self.active_function_id();
    self
      .profiling
      .functions
      .entry(function_id)
      .or_default()
      .record_call();
    let mut state = RiscExecutionState::new(
      self.execution.active.this_value.clone(),
      self.execution.active.chunk.risc_spill_count as usize,
    );
    match self.run_risc_state(&mut state, false)? {
      RiscRunOutcome::Return(value) => Ok(value),
      RiscRunOutcome::Yield(_) | RiscRunOutcome::YieldStar(_) => {
        Err(RuntimeError::from("yield outside generator execution"))
      }
      RiscRunOutcome::Initialized => Err(RuntimeError::from(
        "generator boundary outside generator execution",
      )),
    }
  }

  fn finish_risc_return(
    &mut self,
    registers: &mut [Value; 16],
    spills: &mut Vec<Value>,
    resume_register: &mut Option<usize>,
    result: Value,
  ) -> Result<Option<RiscRunOutcome>, RuntimeError> {
    if let Some(caller_state) = self
      .execution
      .call_stack
      .last_mut()
      .and_then(|frame| frame.risc_state.take())
    {
      self.execute_return(result.clone())?;
      *registers = caller_state.registers;
      registers[0] = result;
      *spills = caller_state.spills;
      *resume_register = caller_state.resume_register;
      return Ok(None);
    }
    if self.execution.call_stack.is_empty() {
      let temp_root_base = self.roots.temporary.len();
      self.roots.temporary.push(result.clone());
      self.roots.kept_alive.clear();
      self.run_microtasks_if_needed()?;
      self.maybe_collect_garbage_prototype();
      self.roots.temporary.truncate(temp_root_base);
      self.roots.kept_alive.clear();
      self.finish_opcode_profile_run("risc-return");
    }
    Ok(Some(RiscRunOutcome::Return(result)))
  }

  pub(super) fn run_risc_state(
    &mut self,
    state: &mut RiscExecutionState,
    stop_at_generator_init: bool,
  ) -> Result<RiscRunOutcome, RuntimeError> {
    self.execution.interpreter_depth += 1;
    let result = if !stop_at_generator_init {
      self.try_run_baseline(state)?.map_or_else(
        || self.run_risc_state_inner(state, stop_at_generator_init),
        Ok,
      )
    } else {
      self.run_risc_state_inner(state, stop_at_generator_init)
    };
    self.process_baseline_requests();
    self.execution.interpreter_depth -= 1;
    result
  }

  fn run_risc_state_inner(
    &mut self,
    state: &mut RiscExecutionState,
    stop_at_generator_init: bool,
  ) -> Result<RiscRunOutcome, RuntimeError> {
    const GC_POLL_INTERVAL: u8 = 64;

    let boundary_frame_count = self.execution.call_stack.len();
    let registers = &mut state.registers;
    let spills = &mut state.spills;
    let mut gc_poll_countdown = GC_POLL_INTERVAL;

    if self
      .exceptions
      .pending_frame_count
      .is_some_and(|target| target >= boundary_frame_count)
      && let Some(exception) = self.exceptions.pending.take()
    {
      self.exceptions.pending_frame_count = None;
      registers[7] = exception;
    }

    loop {
      // Portable RISC remains unchanged; the VM quickens recognized sites once
      // per chunk and only invokes the matching guarded operation here.
      let quickened = if crate::trace::enabled() {
        QuickenedKind::Ordinary
      } else {
        self.quickened_kind()
      };
      if quickened == QuickenedKind::DenseArrayPushLoop
        && self.try_dense_array_push_loop_sequence(registers)?
      {
        self.record_jit_backedge(self.execution.active.ip.saturating_sub(1));
        gc_poll_countdown = gc_poll_countdown.saturating_sub(27);
        if gc_poll_countdown == 0 {
          gc_poll_countdown = GC_POLL_INTERVAL;
        }
        continue;
      }
      if quickened == QuickenedKind::NumericPropertyLoop
        && self.try_numeric_property_loop_sequence(registers)?
      {
        self.record_jit_backedge(self.execution.active.ip.saturating_sub(1));
        gc_poll_countdown = gc_poll_countdown.saturating_sub(24);
        if gc_poll_countdown == 0 {
          gc_poll_countdown = GC_POLL_INTERVAL;
        }
        continue;
      }
      if quickened == QuickenedKind::NumericCallLoop
        && self.try_numeric_call_loop_sequence(registers)?
      {
        self.record_jit_backedge(self.execution.active.ip.saturating_sub(1));
        gc_poll_countdown = gc_poll_countdown.saturating_sub(64);
        if gc_poll_countdown == 0 {
          gc_poll_countdown = GC_POLL_INTERVAL;
        }
        continue;
      }
      if quickened == QuickenedKind::NumericLoop && self.try_numeric_loop_sequence(registers)? {
        self.record_jit_backedge(self.execution.active.ip.saturating_sub(1));
        gc_poll_countdown = gc_poll_countdown.saturating_sub(22);
        if gc_poll_countdown == 0 {
          gc_poll_countdown = GC_POLL_INTERVAL;
        }
        continue;
      }
      if quickened == QuickenedKind::NumericCompareBranch
        && self.try_numeric_compare_branch_sequence(registers)?
      {
        if self.profiling.enabled {
          self.profiling.opcode_counts[Op::LoadLocal as usize] =
            self.profiling.opcode_counts[Op::LoadLocal as usize].saturating_add(1);
          self.profiling.opcode_counts[Op::Const as usize] =
            self.profiling.opcode_counts[Op::Const as usize].saturating_add(1);
          self.profiling.opcode_counts[Op::CompareJump as usize] =
            self.profiling.opcode_counts[Op::CompareJump as usize].saturating_add(1);
        }
        gc_poll_countdown = gc_poll_countdown.saturating_sub(3);
        if gc_poll_countdown == 0 {
          gc_poll_countdown = GC_POLL_INTERVAL;
        }
        continue;
      }
      if quickened == QuickenedKind::SingleArgumentMethodCall
        && self.try_single_argument_method_call_sequence(registers)?
      {
        if self.profiling.enabled {
          for (op, count) in [
            (Op::LoadLocal, 4),
            (Op::StoreLocal, 1),
            (Op::Const, 1),
            (Op::GetProperty, 1),
            (Op::Move, 4),
            (Op::CallBuiltin, 1),
          ] {
            self.profiling.opcode_counts[op as usize] =
              self.profiling.opcode_counts[op as usize].saturating_add(count);
          }
        }
        gc_poll_countdown = gc_poll_countdown.saturating_sub(12);
        if gc_poll_countdown == 0 {
          gc_poll_countdown = GC_POLL_INTERVAL;
        }
        continue;
      }
      if quickened == QuickenedKind::NumericPropertyAdd
        && self.try_numeric_property_add_sequence(registers)?
      {
        if self.profiling.enabled {
          for (op, count) in [
            (Op::LoadLocal, 4),
            (Op::Const, 1),
            (Op::GetProperty, 1),
            (Op::Add, 1),
            (Op::StoreLocal, 2),
          ] {
            self.profiling.opcode_counts[op as usize] =
              self.profiling.opcode_counts[op as usize].saturating_add(count);
          }
        }
        gc_poll_countdown = gc_poll_countdown.saturating_sub(9);
        if gc_poll_countdown == 0 {
          gc_poll_countdown = GC_POLL_INTERVAL;
        }
        continue;
      }
      if quickened == QuickenedKind::NumericIncrement
        && self.try_numeric_increment_sequence(registers)?
      {
        if self.profiling.enabled {
          for (op, count) in [
            (Op::LoadLocal, 3),
            (Op::Neg, 2),
            (Op::Const, 1),
            (Op::Move, 1),
            (Op::Add, 1),
            (Op::StoreLocal, 2),
          ] {
            self.profiling.opcode_counts[op as usize] =
              self.profiling.opcode_counts[op as usize].saturating_add(count);
          }
        }
        gc_poll_countdown = gc_poll_countdown.saturating_sub(10);
        if gc_poll_countdown == 0 {
          gc_poll_countdown = GC_POLL_INTERVAL;
        }
        continue;
      }
      if quickened == QuickenedKind::NumericAddLocal
        && self.try_numeric_add_local_sequence(registers)?
      {
        if self.profiling.enabled {
          self.profiling.opcode_counts[Op::LoadLocal as usize] =
            self.profiling.opcode_counts[Op::LoadLocal as usize].saturating_add(2);
          self.profiling.opcode_counts[Op::Add as usize] =
            self.profiling.opcode_counts[Op::Add as usize].saturating_add(1);
          self.profiling.opcode_counts[Op::StoreLocal as usize] =
            self.profiling.opcode_counts[Op::StoreLocal as usize].saturating_add(1);
        }
        gc_poll_countdown = gc_poll_countdown.saturating_sub(4);
        if gc_poll_countdown == 0 {
          gc_poll_countdown = GC_POLL_INTERVAL;
        }
        continue;
      }

      let word = *self
        .execution
        .active
        .chunk
        .code
        .get(self.execution.active.ip)
        .ok_or_else(|| RuntimeError::from("RISC instruction pointer out of bounds"))?;
      let instruction_ip = self.execution.active.ip;
      self.execution.active.ip += 1;
      // SAFETY: every public execution entry verifies the chunk recursively
      // before this loop runs, including nested function chunks.
      let op = unsafe { Op::from_byte_unchecked((word & 0xFF) as u8) };
      let rd = decode_rd(word) as usize;
      let rs = decode_rs(word) as usize;
      if self.profiling.enabled {
        self.profiling.opcode_counts[op as usize] =
          self.profiling.opcode_counts[op as usize].saturating_add(1);
      }
      if crate::trace::enabled() {
        eprintln!(
          "[trace] risc {}",
          self
            .execution
            .active
            .chunk
            .disassemble_at(instruction_ip)
            .unwrap_or_else(|_| format!("ip={instruction_ip}"))
        );
      }

      let mut switched_risc_context = false;
      let step_result = (|| -> Result<Option<RiscRunOutcome>, RuntimeError> {
        match op {
          Op::Const => {
            registers[rd] = self
              .execution
              .active
              .chunk
              .constants
              .get(decode_risc_aux16(word) as usize)
              .map(crate::bytecode::Constant::to_value)
              .ok_or_else(|| RuntimeError::from("RISC constant index out of bounds"))?;
          }
          Op::Null => registers[rd] = Value::Null,
          Op::True => registers[rd] = Value::Bool(true),
          Op::False => registers[rd] = Value::Bool(false),
          Op::Undefined => registers[rd] = Value::Undefined,
          Op::Move => registers[rd] = registers[rs].clone(),
          Op::LoadSpill => {
            registers[rd] = spills
              .get(decode_risc_aux16(word) as usize)
              .cloned()
              .ok_or_else(|| RuntimeError::from("RISC spill load out of bounds"))?;
          }
          Op::StoreSpill => {
            let spill = spills
              .get_mut(decode_risc_aux16(word) as usize)
              .ok_or_else(|| RuntimeError::from("RISC spill store out of bounds"))?;
            *spill = registers[rd].clone();
          }
          Op::MakeClosure => {
            registers[rd] = self.make_closure_value(decode_risc_aux16(word) as usize)?;
          }
          Op::LoadLocal => {
            let slot = decode_risc_aux16(word) as usize;
            let value = self
              .execution
              .active
              .locals
              .get(slot)
              .map(|cell| cell.borrow().clone())
              .ok_or_else(|| RuntimeError::from("bad RISC local slot"))?;
            if matches!(value, Value::Uninitialized) {
              return Err(
                self.throw_reference_error("Cannot access lexical binding before initialization"),
              );
            }
            registers[rd] = value;
          }
          Op::StoreLocal => {
            let slot = decode_risc_aux16(word) as usize;
            ensure_slot(&mut self.execution.active.locals, slot);
            let was_uninitialized = matches!(
              *self.execution.active.locals[slot].borrow(),
              Value::Uninitialized
            );
            let immutable = self
              .execution
              .active
              .slots
              .as_ref()
              .and_then(|slots| slots.immutable.get(slot))
              .copied()
              .unwrap_or(false);
            let initializes = immutable && was_uninitialized;
            if immutable && !initializes {
              let sloppy_silent = self
                .execution
                .active
                .slots
                .as_ref()
                .and_then(|slots| slots.sloppy_immutable_silent.get(slot))
                .copied()
                .unwrap_or(false);
              if self.is_strict_mode() || !sloppy_silent {
                return Err(self.throw_type_error("Assignment to constant variable"));
              }
            } else {
              let has_object_environment = self
                .execution
                .active
                .lexical_env
                .borrow()
                .has_object_environment();
              let slot_name = (has_object_environment || was_uninitialized)
                .then(|| {
                  self
                    .execution
                    .active
                    .slots
                    .as_ref()
                    .and_then(|slots| slots.names.get(slot))
                    .and_then(Clone::clone)
                    .or_else(|| {
                      self
                        .execution
                        .active
                        .chunk
                        .root_slot_names
                        .get(slot)
                        .and_then(Clone::clone)
                    })
                })
                .flatten();
              let wrote_with_environment =
                if has_object_environment && let Some(name) = slot_name.as_ref() {
                  self.set_with_env_binding_value(name, registers[rd].clone())?
                } else {
                  false
                };
              if !wrote_with_environment {
                *self.execution.active.locals[slot].borrow_mut() = registers[rd].clone();
                if was_uninitialized && let Some(name) = slot_name {
                  let _ = self
                    .execution
                    .active
                    .lexical_env
                    .borrow_mut()
                    .set_binding_initialized_in_chain(&name, true);
                }
                if let Some(arguments_object) = self.execution.active.arguments.object.clone()
                  && slot < self.execution.active.arguments.mapped_count
                {
                  let key = PropertyKey::String(slot.to_string().into());
                  if let Some((PropertyDescriptor::Data { writable: true, .. }, _)) =
                    self.lookup_property_descriptor(&arguments_object, &key, 0)?
                  {
                    set_property(&arguments_object, &key, registers[rd].clone())?;
                    self.bump_object_generation(&arguments_object);
                  }
                }
              }
            }
          }
          Op::CloneLocalCell => {
            let slot = decode_risc_aux16(word) as usize;
            ensure_slot(&mut self.execution.active.locals, slot);
            let value = self.execution.active.locals[slot]
              .borrow()
              .clone();
            self.execution.active.locals[slot] = Rc::new(RefCell::new(value));
          }
          Op::ResetLocalCell => {
            let slot = decode_risc_aux16(word) as usize;
            ensure_slot(&mut self.execution.active.locals, slot);
            self.execution.active.locals[slot] = Rc::new(RefCell::new(Value::Uninitialized));
          }
          Op::GetUpvalue => {
            let index = decode_risc_aux16(word) as usize;
            let value = {
              let environment = self
                .execution
                .active
                .closure_env
                .as_ref()
                .ok_or("GetUpvalue without closure environment")?;
              let environment = environment.borrow();
              environment
                .get(index)
                .map(|cell| cell.borrow().clone())
                .ok_or_else(|| {
                  format!(
                    "Bad upvalue index {index} (closure environment has {} entries)",
                    environment.len()
                  )
                })?
            };
            if matches!(value, Value::Uninitialized) {
              return Err(
                self.throw_reference_error("Cannot access lexical binding before initialization"),
              );
            }
            registers[rd] = value;
          }
          Op::SetUpvalue => {
            let index = decode_risc_aux16(word) as usize;
            let (mirror_global_name, immutable, sloppy_immutable_silent) = self
              .execution
              .call_stack
              .last()
              .and_then(|frame| frame.upvalues.get(index))
              .map_or((None, false, false), |spec| {
                (
                  spec.mirror_global_name.clone(),
                  spec.immutable,
                  spec.sloppy_immutable_silent,
                )
              });
            if immutable {
              if self.is_strict_mode() || !sloppy_immutable_silent {
                return Err(self.throw_type_error("Assignment to constant variable"));
              }
            } else {
              let environment = self
                .execution
                .active
                .closure_env
                .as_ref()
                .ok_or("SetUpvalue without closure environment")?;
              let cell = {
                let environment = environment.borrow();
                environment.get(index).cloned().ok_or_else(|| {
                  format!(
                    "Bad upvalue index {index} (closure environment has {} entries)",
                    environment.len()
                  )
                })?
              };
              *cell.borrow_mut() = registers[rd].clone();
              if let Some(name) = mirror_global_name {
                let current = cell.borrow().clone();
                self.sync_named_binding_to_global_and_frames(&name, &current);
              }
            }
          }
          Op::GetGlobal | Op::TypeofGlobal => {
            let name = self.string_constant(decode_risc_aux16(word) as usize)?;
            let value = self.resolve_identifier_value(name.as_str_lossy())?;
            registers[rd] = if matches!(op, Op::TypeofGlobal) {
              Value::String(
                value
                  .as_ref()
                  .map_or_else(|| "undefined".to_string(), |value| self.typeof_value(value))
                  .into(),
              )
            } else {
              value.ok_or_else(|| self.throw_reference_error(&format!("{name} is not defined")))?
            };
          }
          Op::SetGlobal => {
            let name = self.string_constant(decode_risc_aux16(word) as usize)?;
            let value = registers[rd].clone();
            self.set_global_value(name.as_str_lossy(), value)?;
          }
          Op::GetBuiltin => {
            let name = self.string_constant(decode_risc_aux16(word) as usize)?;
            let value = self
              .realm_state
              .realm
              .get_hidden_builtin(name.as_str_lossy())
              .ok_or_else(|| RuntimeError::from(format!("GetBuiltin: '{name}' not found")))?;
            if !matches!(value, Value::Builtin(_) | Value::BuiltinObject(_, _)) {
              return Err(RuntimeError::from(format!(
                "GetBuiltin: '{name}' is not a builtin"
              )));
            }
            registers[rd] = value;
          }
          Op::GetModuleExport => {
            let (module_index, name) =
              self.module_export_descriptor(decode_risc_aux16(word) as usize)?;
            let value = self
              .modules
              .table
              .get(module_index)
              .and_then(|module| module.get(&name).cloned())
              .map_or(Value::Undefined, |slot| slot.borrow().clone());
            if matches!(value, Value::Uninitialized) {
              return Err(
                self
                  .throw_reference_error(&format!("Cannot access '{name}' before initialization")),
              );
            }
            registers[rd] = value;
          }
          Op::SetModuleExport => {
            let (module_index, name) =
              self.module_export_descriptor(decode_risc_aux16(word) as usize)?;
            while self.modules.table.len() <= module_index {
              self.modules.table.push(HashMap::new());
            }
            if let Some(slot) = self.modules.table[module_index].get(&name) {
              *slot.borrow_mut() = registers[rd].clone();
            } else {
              self.modules.table[module_index]
                .insert(name, Rc::new(RefCell::new(registers[rd].clone())));
            }
          }
          Op::GetThis => registers[rd] = self.get_this(),
          Op::GetNewTarget => registers[rd] = self.get_new_target(),
          Op::Neg => {
            registers[rd] = match registers[rs].clone() {
              Value::BigInt(value) => Value::BigInt(-value),
              value => Value::Number(-self.to_number_value(&value)?),
            };
          }
          Op::Not => registers[rd] = Value::Bool(!registers[rs].is_truthy()),
          Op::BitNot => {
            registers[rd] = match registers[rs].clone() {
              Value::BigInt(value) => Value::BigInt(!value),
              value => Value::Number((!self.to_int32_value(&value)?) as f64),
            };
          }
          Op::Typeof => registers[rd] = Value::String(registers[rs].type_name().to_string().into()),
          Op::Add => {
            let left = self.to_primitive(&registers[rd], None)?;
            let right = self.to_primitive(&registers[rs], None)?;
            registers[rd] = if matches!(left, Value::String(_)) || matches!(right, Value::String(_))
            {
              let left = self.to_string(&left)?;
              let right = self.to_string(&right)?;
              let mut result = JsStringBuilder::with_capacity(left.len() + right.len());
              result.push_string(&left);
              result.push_string(&right);
              Value::String(result.finish())
            } else {
              match (&left, &right) {
                (Value::BigInt(left), Value::BigInt(right)) => {
                  Value::BigInt(left.clone() + right.clone())
                }
                (Value::BigInt(_), _) | (_, Value::BigInt(_)) => {
                  return Err(self.throw_type_error("Cannot mix BigInt and other types"));
                }
                _ => Value::Number(self.to_number_value(&left)? + self.to_number_value(&right)?),
              }
            };
          }
          Op::Sub | Op::Mul | Op::Div | Op::Mod | Op::Pow => {
            let left = registers[rd].clone();
            let right = registers[rs].clone();
            registers[rd] = match (left, right) {
              (Value::BigInt(left), Value::BigInt(right)) => match op {
                Op::Sub => Value::BigInt(left - right),
                Op::Mul => Value::BigInt(left * right),
                Op::Div if right.is_zero() => {
                  return Err(self.throw_range_error("Division by zero"));
                }
                Op::Div => Value::BigInt(left / right),
                Op::Mod if right.is_zero() => {
                  return Err(self.throw_range_error("Division by zero"));
                }
                Op::Mod => Value::BigInt(left % right),
                Op::Pow => {
                  let exponent = right
                    .to_u32()
                    .ok_or_else(|| self.throw_range_error("Invalid BigInt exponent"))?;
                  Value::BigInt(left.pow(exponent))
                }
                _ => unreachable!(),
              },
              (Value::BigInt(_), _) | (_, Value::BigInt(_)) => {
                return Err(self.throw_type_error("Cannot mix BigInt and other types"));
              }
              (left, right) => {
                let left = self.to_number_value(&left)?;
                let right = self.to_number_value(&right)?;
                Value::Number(match op {
                  Op::Sub => left - right,
                  Op::Mul => left * right,
                  Op::Div => left / right,
                  Op::Mod => left % right,
                  Op::Pow => number_exponentiate(left, right),
                  _ => unreachable!(),
                })
              }
            };
          }
          Op::BitAnd | Op::BitOr | Op::BitXor | Op::Shl | Op::Shr | Op::Ushr => {
            let left = registers[rd].clone();
            let right = registers[rs].clone();
            registers[rd] = match op {
              Op::BitAnd => {
                Value::Number((self.to_int32_value(&left)? & self.to_int32_value(&right)?) as f64)
              }
              Op::BitOr => {
                Value::Number((self.to_int32_value(&left)? | self.to_int32_value(&right)?) as f64)
              }
              Op::BitXor => {
                Value::Number((self.to_int32_value(&left)? ^ self.to_int32_value(&right)?) as f64)
              }
              Op::Shl => Value::Number(
                self
                  .to_int32_value(&left)?
                  .wrapping_shl(self.to_uint32_value(&right)? & 0x1f) as f64,
              ),
              Op::Shr => Value::Number(
                self
                  .to_int32_value(&left)?
                  .wrapping_shr(self.to_uint32_value(&right)? & 0x1f) as f64,
              ),
              Op::Ushr => Value::Number(
                self
                  .to_uint32_value(&left)?
                  .wrapping_shr(self.to_uint32_value(&right)? & 0x1f) as f64,
              ),
              _ => unreachable!(),
            };
          }
          Op::And => {
            registers[rd] = Value::Bool(registers[rd].is_truthy() && registers[rs].is_truthy());
          }
          Op::Or => {
            registers[rd] = Value::Bool(registers[rd].is_truthy() || registers[rs].is_truthy());
          }
          Op::Jump => {
            let offset = ((word >> 8) & 0x00FF_FFFF) as i32;
            let offset = if offset & 0x0080_0000 != 0 {
              offset | !0x00FF_FFFF
            } else {
              offset
            };
            if offset < 0 {
              self.record_jit_backedge(instruction_ip);
            }
            self.execution.active.ip =
              usize::try_from(self.execution.active.ip as i64 + offset as i64)
                .map_err(|_| RuntimeError::from("negative RISC jump target"))?;
          }
          Op::JumpIfFalse | Op::JumpIfTrue | Op::JumpIfNullish => {
            let value = &registers[rd];
            let take = match op {
              Op::JumpIfFalse => !value.is_truthy(),
              Op::JumpIfTrue => value.is_truthy(),
              Op::JumpIfNullish => matches!(value, Value::Null | Value::Undefined),
              _ => unreachable!(),
            };
            let offset = decode_risc_cond_offset(word);
            if take && offset < 0 {
              self.record_jit_backedge(instruction_ip);
            }
            if take {
              self.execution.active.ip =
                usize::try_from(self.execution.active.ip as i64 + offset as i64)
                  .map_err(|_| RuntimeError::from("negative RISC conditional target"))?;
            }
          }
          Op::CompareJump => {
            let (condition, left, right, offset) = decode_compare_jump(word);
            let left = &registers[left as usize];
            let right = &registers[right as usize];
            let result = match condition {
              0 => self.abstract_equality(left, right)?,
              1 => !self.abstract_equality(left, right)?,
              2 => self
                .abstract_relational_comparison(left, right)?
                .unwrap_or(false),
              3 => self
                .abstract_relational_comparison(right, left)?
                .is_some_and(|result| !result),
              4 => self
                .abstract_relational_comparison(right, left)?
                .unwrap_or(false),
              5 => self
                .abstract_relational_comparison(left, right)?
                .is_some_and(|result| !result),
              6 => values_equal(left, right),
              7 => !values_equal(left, right),
              _ => unreachable!(),
            };
            if !result {
              if offset < 0 {
                self.record_jit_backedge(instruction_ip);
              }
              self.execution.active.ip =
                usize::try_from(self.execution.active.ip as i64 + offset as i64)
                  .map_err(|_| RuntimeError::from("negative RISC compare target"))?;
            }
          }
          Op::GetProperty => {
            let object = registers[rd].clone();
            let key = registers[rs].clone();
            let key = self.to_property_key(&key)?;
            if matches!(&key, PropertyKey::Symbol(symbol) if symbol.is_private())
              && !self.has_property_host(&object, &key)?
            {
              return Err(self.throw_type_error("Cannot access private member on this object"));
            }
            registers[rd] = self.get_property_value_cached(instruction_ip, &object, &key)?;
          }
          Op::SetProperty => {
            let receiver = registers[rd].clone();
            let key = self.to_property_key(&registers[rs])?;
            let value = registers[(decode_aux8(word) & 0xF) as usize].clone();
            if receiver.is_object() {
              self.set_property_with_receiver(&receiver, &key, value.clone(), &receiver)?;
            } else {
              let object = self.to_object_value(receiver.clone())?;
              self.set_primitive_property(&object, &key, value.clone(), &receiver)?;
            }
            registers[rd] = value;
          }
          Op::DeleteProperty => {
            let base = registers[rd].clone();
            if matches!(base, Value::Undefined | Value::Null) {
              return Err(self.throw_type_error("Cannot delete property of null or undefined"));
            }
            let object = if base.is_object() {
              base
            } else {
              self.to_object_value(base)?
            };
            let key = self.to_property_key(&registers[rs])?;
            let deleted = self.delete_property_host(&object, &key)?;
            if !deleted && self.is_strict_mode() {
              return Err(self.throw_type_error("Cannot delete property in strict mode"));
            }
            registers[rd] = Value::Bool(deleted);
          }
          Op::HasProperty => {
            let object = registers[rd].clone();
            if !object.is_object() {
              return Err(self.throw_type_error("Right-hand side of 'in' is not an object"));
            }
            let key = self.to_property_key(&registers[rs])?;
            registers[rd] = Value::Bool(self.has_property_host(&object, &key)?);
          }
          Op::InstanceOf => {
            let value = registers[rd].clone();
            let constructor = registers[rs].clone();
            registers[rd] = Value::Bool(self.instance_of(&value, &constructor)?);
          }
          Op::GetSuperProperty => {
            let receiver = registers[rd].clone();
            let key = registers[rs].clone();
            registers[rd] = self.get_super_property(&receiver, &key)?;
          }
          Op::SetSuperProperty => {
            let value_register = (decode_aux8(word) & 0xF) as usize;
            let receiver = registers[rd].clone();
            let key = registers[rs].clone();
            let value = registers[value_register].clone();
            registers[rd] = self.set_super_property(&receiver, &key, value)?;
          }
          Op::BuildArray => {
            let count = decode_risc_aux16(word) as usize;
            let base = self.execution.active.chunk.risc_value_spill_count as usize;
            let values = spills
              .get(base..base + count)
              .ok_or_else(|| RuntimeError::from("RISC array operand window out of bounds"))?
              .to_vec();
            registers[rd] = self.build_array_value(values)?;
          }
          Op::BuildObject => {
            let count = decode_risc_aux16(word) as usize;
            let operand_count = count.saturating_mul(2);
            let base = self.execution.active.chunk.risc_value_spill_count as usize;
            let values = spills
              .get(base..base + operand_count)
              .ok_or_else(|| RuntimeError::from("RISC object operand window out of bounds"))?
              .to_vec();
            registers[rd] = self.build_object_value(values)?;
          }
          Op::ArrayFromSpread => {
            let values = registers[rd].clone();
            let mask = registers[rs].clone();
            registers[rd] = array_from_values(self.flatten_spread_values(values, mask)?);
          }
          Op::Spread => {
            let callee = registers[rd].clone();
            let values = registers[rs].clone();
            let mut mask = registers[(decode_aux8(word) & 0xF) as usize].clone();
            let mut super_construct = false;
            let mut ordinary_construct = false;
            let receiver = match &mask {
              Value::Array(mask_values, _) => {
                let marker = mask_values.borrow().first().cloned().flatten();
                match marker {
                  Some(Value::Array(marker_values, _)) => {
                    let marker_values = marker_values.borrow();
                    let receiver = marker_values.first().cloned().flatten();
                    let spread = marker_values.get(1).cloned().flatten();
                    super_construct = matches!(
                      marker_values
                        .get(2)
                        .and_then(|value| value.as_ref()),
                      Some(Value::Bool(true))
                    );
                    ordinary_construct = matches!(
                      marker_values
                        .get(3)
                        .and_then(|value| value.as_ref()),
                      Some(Value::Bool(true))
                    );
                    drop(marker_values);
                    if let (Some(receiver), Some(spread)) = (receiver, spread) {
                      let mut normalized = mask_values.borrow().to_vec();
                      normalized[0] = Some(spread);
                      mask = array_from_values(
                        normalized
                          .into_iter()
                          .map(|value| value.unwrap_or(Value::Undefined))
                          .collect(),
                      );
                      Some(receiver)
                    } else {
                      None
                    }
                  }
                  _ => None,
                }
              }
              _ => None,
            };
            let arguments = self.flatten_spread_values(values, mask)?;
            registers[rd] = if super_construct {
              self.construct_super_value(arguments)?
            } else if ordinary_construct {
              self.construct_value(&callee, &arguments)?
            } else if let Some(receiver) = receiver {
              self.call_with_this(&callee, &receiver, &arguments)?
            } else {
              self.call_value(&callee, &arguments)?
            };
          }
          Op::Call | Op::TailCall | Op::CallBuiltin => {
            let count = decode_risc_aux16(word) as usize;
            let mut args: Vec<Value> = (0..count.min(6))
              .map(|index| registers[2 + index].clone())
              .collect();
            let overflow = count.saturating_sub(6);
            let base = self.execution.active.chunk.risc_value_spill_count as usize;
            if overflow > 0 {
              args.extend_from_slice(
                spills
                  .get(base..base + overflow)
                  .ok_or_else(|| RuntimeError::from("RISC call operand window out of bounds"))?,
              );
            }
            let callee = registers[0].clone();
            if op != Op::TailCall
              && let Value::Closure { proto, env, .. } = &callee
              && !proto.is_generator
              && !proto.is_async
            {
              if proto.is_class_constructor && self.construction.new_target.is_none() {
                return Err(self.throw_class_constructor_call_error(proto));
              }
              let call_args = self.prepare_call_args(args, proto)?;
              let receiver = if op == Op::CallBuiltin {
                registers[1].clone()
              } else {
                Value::Undefined
              };
              let this_arg = Self::closure_lexical_this(&callee).unwrap_or(receiver);
              let this_arg = if proto.is_derived_constructor {
                Value::Uninitialized
              } else {
                this_arg
              };
              if let Some(result) =
                self.try_direct_baseline_call(instruction_ip, proto, &this_arg, &call_args)?
              {
                registers[0] = result;
              } else {
                self.push_call_frame(proto, env, call_args, this_arg, callee.clone())?;
                let callee_this = self.execution.active.this_value.clone();
                let callee_spill_count = self.execution.active.chunk.risc_spill_count as usize;
                let caller_state = RiscExecutionState {
                  registers: std::mem::replace(
                    registers,
                    std::array::from_fn(|_| Value::Undefined),
                  ),
                  spills: std::mem::replace(spills, vec![Value::Undefined; callee_spill_count]),
                  resume_register: state.resume_register,
                };
                self
                  .execution
                  .call_stack
                  .last_mut()
                  .expect("call frame was just pushed")
                  .risc_state = Some(caller_state);
                registers[1] = callee_this;
                state.resume_register = None;
                if let Some(result) = self.try_run_nested_baseline(registers, false)? {
                  let outcome = self.finish_risc_return(
                    registers,
                    spills,
                    &mut state.resume_register,
                    result,
                  )?;
                  switched_risc_context = outcome.is_none();
                  if outcome.is_some() {
                    return Ok(outcome);
                  }
                } else {
                  switched_risc_context = true;
                }
              }
            } else if op != Op::TailCall {
              registers[0] = if matches!(op, Op::CallBuiltin) {
                let receiver = registers[1].clone();
                self.call_value_with_this(&callee, &receiver, &args)?
              } else {
                self.call_value(&callee, &args)?
              };
            } else if let Value::Closure { proto, env, .. } = &callee
              && !proto.is_generator
              && !proto.is_async
            {
              if proto.is_class_constructor && self.construction.new_target.is_none() {
                return Err(self.throw_class_constructor_call_error(proto));
              }
              let call_args = self.prepare_call_args(args, proto)?;
              let this_arg = Self::closure_lexical_this(&callee).unwrap_or(Value::Undefined);
              let this_arg = if proto.is_derived_constructor {
                Value::Uninitialized
              } else {
                this_arg
              };
              let active_depth = self.execution.call_stack.len();
              while self
                .exceptions
                .handlers
                .last()
                .is_some_and(|handler| handler.frame_count >= active_depth)
              {
                self.exceptions.handlers.pop();
              }
              if let Some(frame) = self.execution.call_stack.last_mut() {
                frame.callee = callee.clone();
                frame.new_target = None;
                frame.upvalues = proto.upvalues.clone();
              }
              self.configure_call_frame(proto, env, call_args, this_arg, callee.clone())?;
              registers.fill(Value::Undefined);
              registers[1] = self.execution.active.this_value.clone();
              spills.clear();
              spills.resize(
                self.execution.active.chunk.risc_spill_count as usize,
                Value::Undefined,
              );
              state.resume_register = None;
              if let Some(result) = self.try_run_nested_baseline(registers, true)? {
                let outcome =
                  self.finish_risc_return(registers, spills, &mut state.resume_register, result)?;
                switched_risc_context = outcome.is_none();
                if outcome.is_some() {
                  return Ok(outcome);
                }
              } else {
                switched_risc_context = true;
              }
            } else {
              let result = self.call_value(&callee, &args)?;
              let outcome =
                self.finish_risc_return(registers, spills, &mut state.resume_register, result)?;
              switched_risc_context = outcome.is_none();
              return Ok(outcome);
            }
          }
          Op::Construct => {
            let count = decode_risc_aux16(word) as usize;
            let mut args: Vec<Value> = (0..count.min(6))
              .map(|index| registers[2 + index].clone())
              .collect();
            let overflow = count.saturating_sub(6);
            let base = self.execution.active.chunk.risc_value_spill_count as usize;
            if overflow > 0 {
              args.extend_from_slice(spills.get(base..base + overflow).ok_or_else(|| {
                RuntimeError::from("RISC construct operand window out of bounds")
              })?);
            }
            let callee = registers[0].clone();
            registers[0] = self.construct_value_with_new_target(&callee, &args, callee.clone())?;
          }
          Op::SuperConstruct => {
            let count = decode_risc_aux16(word) as usize;
            let base = self.execution.active.chunk.risc_value_spill_count as usize;
            let args = spills
              .get(base..base + count)
              .ok_or_else(|| RuntimeError::from("RISC super operand window out of bounds"))?
              .to_vec();
            registers[rd] = self.construct_super_value(args)?;
          }
          Op::PushHandler => {
            let offset = ((word >> 8) & 0x00FF_FFFF) as i32;
            let offset = if offset & 0x0080_0000 != 0 {
              offset | !0x00FF_FFFF
            } else {
              offset
            };
            let handler_ip = usize::try_from(self.execution.active.ip as i64 + offset as i64)
              .map_err(|_| RuntimeError::from("negative RISC handler target"))?;
            let finally = self
              .execution
              .active
              .chunk
              .finally_handlers
              .iter()
              .find(|handler| handler.push_ip == instruction_ip)
              .map(|handler| FinallyHandlerRecord {
                finalizer_ip: handler.finalizer_ip,
                return_slot: handler.return_slot,
                did_return_slot: handler.did_return_slot,
              });
            self.exceptions.handlers.push(HandlerRecord {
              chunk: Arc::clone(&self.execution.active.chunk),
              handler_ip,
              saved_locals_len: self.execution.active.locals.len(),
              frame_count: self.execution.call_stack.len(),
              saved_lexical_env: Rc::clone(&self.execution.active.lexical_env),
              finally,
            });
          }
          Op::PopHandler => {
            self.exceptions.handlers.pop();
          }
          Op::Throw => {
            self.throw_value(registers[rd].clone())?;
            return Err(RuntimeError::from("JS exception"));
          }
          Op::DeleteIdentifier => {
            registers[rd] = self.delete_identifier(registers[rd].clone())?;
          }
          Op::IteratorNext => {
            registers[rd] = self.iterator_next(registers[rd].clone())?;
          }
          Op::IteratorClose => {
            registers[rd] = self.iterator_close(registers[rd].clone())?;
          }
          Op::GetAsyncIterator => {
            registers[rd] = self.get_async_iterator(registers[rd].clone())?;
          }
          Op::PushWithEnv => {
            let object = self.to_object_value(registers[rd].clone())?;
            self.execution.active.lexical_env =
              Rc::new(RefCell::new(EnvironmentRecord::new_object(
                Some(Rc::clone(&self.execution.active.lexical_env)),
                object,
                true,
              )));
          }
          Op::PopEnv => {
            let outer = self.execution.active.lexical_env.borrow().outer();
            self.execution.active.lexical_env =
              outer.unwrap_or_else(|| Rc::clone(&self.realm_state.global_env));
          }
          Op::GeneratorInitDone if stop_at_generator_init => {
            return Ok(Some(RiscRunOutcome::Initialized));
          }
          Op::GeneratorInitDone => {}
          Op::Yield => {
            state.resume_register = Some(rd);
            return Ok(Some(RiscRunOutcome::Yield(registers[rd].clone())));
          }
          Op::YieldStar => {
            if self.execution.active.yield_star.is_none() {
              self.execution.active.yield_star =
                Some(self.create_yield_star_state(registers[rd].clone())?);
            }
            match self.resume_yield_star()? {
              YieldStarStep::Yield(value) => {
                self.execution.active.ip = self.execution.active.ip.saturating_sub(1);
                return Ok(Some(RiscRunOutcome::YieldStar(value)));
              }
              YieldStarStep::Complete(value) => registers[rd] = value,
              YieldStarStep::Return(value) => {
                if !self.begin_generator_return(value.clone()) {
                  return Ok(Some(RiscRunOutcome::Return(value)));
                }
              }
            }
          }
          Op::AwaitPromise => {
            registers[rd] = self.await_value_or_throw(registers[rd].clone())?;
          }
          Op::Return => {
            let result = registers[rd].clone();
            if self.execution.generator_return_frame_count == Some(self.execution.call_stack.len())
            {
              if self.begin_generator_return(result.clone()) {
                return Ok(None);
              }
              self.execution.generator_return_frame_count = None;
            }
            let outcome =
              self.finish_risc_return(registers, spills, &mut state.resume_register, result)?;
            switched_risc_context = outcome.is_none();
            return Ok(outcome);
          }
        }
        Ok(None)
      })();
      match step_result {
        Ok(Some(result)) => return Ok(result),
        Ok(None) if switched_risc_context => continue,
        Ok(None) => {}
        Err(_error)
          if self.exceptions.pending.is_some()
            && self
              .exceptions
              .pending_frame_count
              .is_some_and(|target| target >= boundary_frame_count) =>
        {
          if let Some(caller_state) = self.exceptions.unwound_risc_state.take() {
            *registers = caller_state.registers;
            *spills = caller_state.spills;
            state.resume_register = caller_state.resume_register;
          }
          self.exceptions.pending_frame_count = None;
          registers[7] = self
            .exceptions
            .pending
            .take()
            .unwrap_or(Value::Undefined);
          continue;
        }
        Err(error) => return Err(error),
      }
      gc_poll_countdown -= 1;
      if gc_poll_countdown == 0 {
        gc_poll_countdown = GC_POLL_INTERVAL;
        if gc::allocation_pressure() >= gc_allocation_pressure_threshold() {
          let temp_root_base = self.roots.temporary.len();
          self
            .roots
            .temporary
            .extend(registers.iter().cloned());
          self
            .roots
            .temporary
            .extend(spills.iter().cloned());
          self.maybe_collect_garbage_prototype();
          self.roots.temporary.truncate(temp_root_base);
        }
      }
    }
  }

  #[allow(
    dead_code,
    reason = "explicit baseline controller used by tier integration"
  )]
  pub(super) fn install_baseline_for_function(
    &mut self,
    function_value: &Value,
  ) -> Result<(), RuntimeError> {
    let Value::Closure { proto, .. } = function_value else {
      return Err(RuntimeError::from("baseline target is not a closure"));
    };
    let chunk = std::sync::Arc::clone(&proto.chunk);
    let function = self.function_id_for_chunk(&chunk);
    let version = self
      .profiling
      .jit
      .install_const_prefix(function, &chunk, proto.arity)
      .map_err(RuntimeError::from)?;
    self
      .profiling
      .functions
      .entry(function)
      .or_default()
      .baseline = crate::vm::jit::BaselineState::Ready(version);
    Ok(())
  }

  #[allow(dead_code, reason = "compatibility helper for active root execution")]
  pub(super) fn install_baseline_const_return(&mut self) -> Result<(), RuntimeError> {
    let function = self.active_function_id();
    let version = self
      .profiling
      .jit
      .install_const_prefix(function, &self.execution.active.chunk, 0)
      .map_err(RuntimeError::from)?;
    self
      .profiling
      .functions
      .entry(function)
      .or_default()
      .baseline = crate::vm::jit::BaselineState::Ready(version);
    Ok(())
  }

  fn try_direct_baseline_call(
    &mut self,
    call_site: usize,
    proto: &Rc<crate::bytecode::FunctionProto>,
    this_value: &Value,
    arguments: &[Value],
  ) -> Result<Option<Value>, RuntimeError> {
    let cached = self
      .caches
      .active_direct_calls
      .borrow()
      .get(call_site)
      .and_then(Clone::clone)
      .filter(|entry| Rc::ptr_eq(&entry.expected_proto, proto));
    if let Some(entry) = cached
      && entry.target.is_valid()
      && let Ok(result) = entry.target.call(arguments)
    {
      self.profiling.jit_stats.baseline_entries = self
        .profiling
        .jit_stats
        .baseline_entries
        .saturating_add(1);
      self.profiling.jit_stats.baseline_returns = self
        .profiling
        .jit_stats
        .baseline_returns
        .saturating_add(1);
      return Ok(Some(result));
    }
    let chunk = &proto.chunk;
    let parameter_count = proto.arity;
    let function = self.function_id_for_chunk(chunk);
    let profile = self
      .profiling
      .functions
      .entry(function)
      .or_default();
    profile.parameter_count = parameter_count;
    profile.record_call();
    self.process_baseline_request(function);
    if !self
      .profiling
      .functions
      .get(&function)
      .is_some_and(|profile| matches!(profile.baseline, crate::vm::jit::BaselineState::Ready(_)))
    {
      return Ok(None);
    }
    if let Some(target) = self.profiling.jit.direct_call_target(function) {
      if let Some(slot) = self
        .caches
        .active_direct_calls
        .borrow_mut()
        .get_mut(call_site)
      {
        *slot = Some(crate::vm::state::DirectCallSite {
          expected_proto: Rc::clone(proto),
          target: target.clone(),
        });
      }
      let result = target
        .call(arguments)
        .map_err(RuntimeError::from)?;
      self.profiling.jit_stats.baseline_entries = self
        .profiling
        .jit_stats
        .baseline_entries
        .saturating_add(1);
      self.profiling.jit_stats.baseline_returns = self
        .profiling
        .jit_stats
        .baseline_returns
        .saturating_add(1);
      return Ok(Some(result));
    }
    let mut registers: [Value; 16] = std::array::from_fn(|_| Value::Undefined);
    registers[1] = this_value.clone();
    let mut result = Value::Undefined;
    let mut frame = crate::vm::jit::native::JitFrame {
      vm: self as *mut Vm,
      function_id: 0,
      vm_pc: 0,
      registers: registers.as_mut_ptr(),
      register_count: registers.len(),
      result: &mut result,
      arguments: arguments.as_ptr(),
      argument_count: arguments.len().min(parameter_count),
      locals: std::ptr::null_mut(),
      local_count: 0,
    };
    let Some(exit) = self.profiling.jit.execute(function, &mut frame) else {
      return Ok(None);
    };
    if crate::vm::jit::native::decode_exit(exit).map_err(RuntimeError::from)?
      != crate::vm::jit::native::JitExit::Return
    {
      return Ok(None);
    }
    self.profiling.jit_stats.baseline_entries = self
      .profiling
      .jit_stats
      .baseline_entries
      .saturating_add(1);
    self.profiling.jit_stats.baseline_returns = self
      .profiling
      .jit_stats
      .baseline_returns
      .saturating_add(1);
    Ok(Some(result))
  }

  fn try_run_nested_baseline(
    &mut self,
    registers: &mut [Value; 16],
    record_call: bool,
  ) -> Result<Option<Value>, RuntimeError> {
    let function = self.active_function_id();
    if record_call {
      self
        .profiling
        .functions
        .entry(function)
        .or_default()
        .record_call();
      self.process_baseline_request(function);
    }
    if !self
      .profiling
      .functions
      .get(&function)
      .is_some_and(|profile| matches!(profile.baseline, crate::vm::jit::BaselineState::Ready(_)))
    {
      return Ok(None);
    }
    let mut result = Value::Undefined;
    let locals = (&mut self.execution.active.locals as *mut Vec<SharedValue>).cast();
    let local_count = self.execution.active.locals.len();
    let mut frame = crate::vm::jit::native::JitFrame {
      vm: self as *mut Vm,
      function_id: 0,
      vm_pc: self.execution.active.ip,
      registers: registers.as_mut_ptr(),
      register_count: registers.len(),
      result: &mut result,
      arguments: std::ptr::null(),
      argument_count: 0,
      locals,
      local_count,
    };
    let Some(exit) = self.profiling.jit.execute(function, &mut frame) else {
      return Ok(None);
    };
    self.profiling.jit_stats.baseline_entries = self
      .profiling
      .jit_stats
      .baseline_entries
      .saturating_add(1);
    match crate::vm::jit::native::decode_exit(exit).map_err(RuntimeError::from)? {
      crate::vm::jit::native::JitExit::Return => {
        self.profiling.jit_stats.baseline_returns = self
          .profiling
          .jit_stats
          .baseline_returns
          .saturating_add(1);
        Ok(Some(result))
      }
      crate::vm::jit::native::JitExit::ResumeInterpreter
      | crate::vm::jit::native::JitExit::Invalidated => {
        self.profiling.jit_stats.interpreter_resumes = self
          .profiling
          .jit_stats
          .interpreter_resumes
          .saturating_add(1);
        self.execution.active.ip = frame.vm_pc;
        Ok(None)
      }
      crate::vm::jit::native::JitExit::NextBlock => {
        Err(RuntimeError::from("unresolved nested baseline block exit"))
      }
      crate::vm::jit::native::JitExit::Exception => {
        Err(RuntimeError::from("baseline JIT reported an exception"))
      }
    }
  }

  fn process_baseline_request(
    &mut self,
    function: crate::vm::jit::FunctionId,
  ) {
    if !self
      .profiling
      .functions
      .get(&function)
      .is_some_and(|profile| profile.baseline == crate::vm::jit::BaselineState::Requested)
    {
      return;
    }
    self
      .profiling
      .functions
      .entry(function)
      .or_default()
      .baseline = crate::vm::jit::BaselineState::Compiling;
    let chunk = self
      .profiling
      .chunk_ids
      .iter()
      .find_map(|(identity, id)| {
        (*id == function)
          .then(|| self.profiling.chunk_refs.get(identity).cloned())
          .flatten()
      });
    let parameter_count = self
      .profiling
      .functions
      .get(&function)
      .map_or(0, |profile| profile.parameter_count);
    let state = chunk
      .as_deref()
      .and_then(|chunk| {
        self
          .profiling
          .jit
          .install_const_prefix(function, chunk, parameter_count)
          .ok()
      })
      .map_or(
        crate::vm::jit::BaselineState::Disabled,
        crate::vm::jit::BaselineState::Ready,
      );
    self
      .profiling
      .functions
      .entry(function)
      .or_default()
      .baseline = state;
  }

  fn process_baseline_requests(&mut self) {
    let requested: Vec<_> = self
      .profiling
      .functions
      .iter()
      .filter_map(|(function, profile)| {
        (profile.baseline == crate::vm::jit::BaselineState::Requested).then_some(*function)
      })
      .collect();
    for function in requested {
      self.process_baseline_request(function);
    }
  }

  fn try_run_baseline(
    &mut self,
    state: &mut RiscExecutionState,
  ) -> Result<Option<RiscRunOutcome>, RuntimeError> {
    let function = self.active_function_id();
    let baseline_ready = self
      .profiling
      .functions
      .get(&function)
      .is_some_and(|profile| matches!(profile.baseline, crate::vm::jit::BaselineState::Ready(_)));
    if !baseline_ready {
      return Ok(None);
    }
    let mut result = Value::Undefined;
    let locals = (&mut self.execution.active.locals as *mut Vec<SharedValue>).cast();
    let local_count = self.execution.active.locals.len();
    let mut frame = crate::vm::jit::native::JitFrame {
      vm: self as *mut Vm,
      function_id: 0,
      vm_pc: self.execution.active.ip,
      registers: state.registers.as_mut_ptr(),
      register_count: state.registers.len(),
      result: &mut result,
      arguments: std::ptr::null(),
      argument_count: 0,
      locals,
      local_count,
    };
    let Some(exit) = self.profiling.jit.execute(function, &mut frame) else {
      return Ok(None);
    };
    self.profiling.jit_stats.baseline_entries = self
      .profiling
      .jit_stats
      .baseline_entries
      .saturating_add(1);
    match crate::vm::jit::native::decode_exit(exit).map_err(RuntimeError::from)? {
      crate::vm::jit::native::JitExit::Return => {
        self.profiling.jit_stats.baseline_returns = self
          .profiling
          .jit_stats
          .baseline_returns
          .saturating_add(1);
        Ok(Some(RiscRunOutcome::Return(result)))
      }
      crate::vm::jit::native::JitExit::ResumeInterpreter => {
        self.profiling.jit_stats.interpreter_resumes = self
          .profiling
          .jit_stats
          .interpreter_resumes
          .saturating_add(1);
        self.execution.active.ip = frame.vm_pc;
        Ok(None)
      }
      crate::vm::jit::native::JitExit::Invalidated => Ok(None),
      crate::vm::jit::native::JitExit::NextBlock => {
        Err(RuntimeError::from("unresolved baseline block exit"))
      }
      crate::vm::jit::native::JitExit::Exception => {
        Err(RuntimeError::from("baseline JIT reported an exception"))
      }
    }
  }

  fn function_id_for_chunk(
    &mut self,
    chunk: &std::sync::Arc<crate::bytecode::Chunk>,
  ) -> crate::vm::jit::FunctionId {
    let chunk_identity = std::sync::Arc::as_ptr(chunk) as usize;
    if let Some(id) = self
      .profiling
      .chunk_ids
      .get(&chunk_identity)
      .copied()
    {
      return id;
    }
    self.profiling.next_function_id = self.profiling.next_function_id.saturating_add(1);
    let id = crate::vm::jit::FunctionId::new(self.profiling.next_function_id);
    self
      .profiling
      .chunk_refs
      .insert(chunk_identity, std::sync::Arc::clone(chunk));
    self
      .profiling
      .chunk_ids
      .insert(chunk_identity, id);
    id
  }

  fn active_function_id(&mut self) -> crate::vm::jit::FunctionId {
    let identity = Arc::as_ptr(&self.execution.active.chunk) as usize;
    if self.profiling.active_function_chunk == identity
      && let Some(function) = self.profiling.active_function_id
    {
      return function;
    }
    let chunk = std::sync::Arc::clone(&self.execution.active.chunk);
    let function = self.function_id_for_chunk(&chunk);
    self.profiling.active_function_chunk = identity;
    self.profiling.active_function_id = Some(function);
    function
  }

  fn record_jit_backedge(
    &mut self,
    pc: usize,
  ) {
    let chunk_identity = Arc::as_ptr(&self.execution.active.chunk) as usize;
    if self.profiling.last_hot_chunk == chunk_identity
      || self
        .profiling
        .hot_chunks
        .contains(&chunk_identity)
    {
      self.profiling.last_hot_chunk = chunk_identity;
      return;
    }
    let function_id = self.active_function_id();
    let profile = self
      .profiling
      .functions
      .entry(function_id)
      .or_default();
    profile.record_backedge(pc);
    if matches!(profile.baseline, crate::vm::jit::BaselineState::Requested) {
      self.profiling.hot_chunks.insert(chunk_identity);
      self.profiling.last_hot_chunk = chunk_identity;
    }
  }

  fn quickened_kind(&mut self) -> QuickenedKind {
    let identity = Arc::as_ptr(&self.execution.active.chunk) as usize;
    if self.caches.active_quickened_chunk != identity {
      let sites = if let Some(plan) = self.caches.quickened.get(&identity) {
        self.caches.active_direct_calls = Rc::clone(&plan.direct_calls);
        Arc::clone(&plan.sites)
      } else {
        let chunk = Arc::clone(&self.execution.active.chunk);
        let sites = Arc::<[QuickenedKind]>::from(build_quickened_sites(&chunk.code));
        let direct_calls = Rc::new(RefCell::new(vec![None; chunk.code.len()]));
        self.caches.quickened.insert(
          identity,
          QuickenedPlan {
            _chunk: chunk,
            sites: Arc::clone(&sites),
            direct_calls: Rc::clone(&direct_calls),
          },
        );
        self.caches.active_direct_calls = direct_calls;
        sites
      };
      self.caches.active_quickened_chunk = identity;
      self.caches.active_quickened_sites = sites;
    }
    self
      .caches
      .active_quickened_sites
      .get(self.execution.active.ip)
      .copied()
      .unwrap_or(QuickenedKind::Ordinary)
  }

  fn try_dense_array_push_loop_sequence(
    &mut self,
    registers: &mut [Value; 16],
  ) -> Result<bool, RuntimeError> {
    let ip = self.execution.active.ip;
    let Some(words) = self.execution.active.chunk.code.get(ip..ip + 27) else {
      return Ok(false);
    };
    let index_slot = decode_risc_aux16(words[0]) as usize;
    let array_slot = decode_risc_aux16(words[3]) as usize;
    let call_temporary_slot = decode_risc_aux16(words[4]) as usize;
    let increment_temporary_slot = decode_risc_aux16(words[22]) as usize;
    let (condition, left, right, exit_offset) = decode_compare_jump(words[2]);
    if condition != 2
      || left != decode_rd(words[0])
      || right != decode_rd(words[1])
      || exit_offset <= 0
      || decode_risc_aux16(words[5]) as usize != call_temporary_slot
      || decode_risc_aux16(words[6]) as usize != call_temporary_slot
      || decode_risc_aux16(words[9]) as usize != index_slot
      || decode_risc_aux16(words[16]) as usize != index_slot
      || decode_risc_aux16(words[23]) as usize != index_slot
      || decode_risc_aux16(words[24]) as usize != increment_temporary_slot
      || decode_risc_aux16(words[25]) as usize != index_slot
      || decode_risc_aux16(words[19]) as usize >= self.execution.active.chunk.constants.len()
      || self.execution.active.arguments.object.is_some()
      || self
        .execution
        .active
        .lexical_env
        .borrow()
        .has_object_environment()
      || self
        .execution
        .active
        .locals
        .get(index_slot)
        .is_some_and(|cell| Rc::strong_count(cell) != 1)
      || [index_slot, call_temporary_slot, increment_temporary_slot]
        .into_iter()
        .any(|slot| {
          self
            .execution
            .active
            .slots
            .as_ref()
            .and_then(|slots| slots.immutable.get(slot))
            .copied()
            .unwrap_or(false)
        })
    {
      return Ok(false);
    }
    let number = |slot: usize| {
      self
        .execution
        .active
        .locals
        .get(slot)
        .and_then(|cell| match *cell.borrow() {
          Value::Number(value) => Some(value),
          _ => None,
        })
    };
    let Some(index) = number(index_slot) else {
      return Ok(false);
    };
    let Some(crate::bytecode::Constant::Number(limit)) = self
      .execution
      .active
      .chunk
      .constants
      .get(decode_risc_aux16(words[1]) as usize)
    else {
      return Ok(false);
    };
    let limit = *limit;
    if index >= limit {
      self.execution.active.ip = ip + 27;
      return Ok(true);
    }
    let receiver = self
      .execution
      .active
      .locals
      .get(array_slot)
      .map(|cell| cell.borrow().clone());
    let Some(Value::Array(elements, properties)) = receiver else {
      return Ok(false);
    };
    let Some(crate::bytecode::Constant::String(method_key)) = self
      .execution
      .active
      .chunk
      .constants
      .get(decode_risc_aux16(words[7]) as usize)
    else {
      return Ok(false);
    };
    if method_key.as_str_lossy() != "push" {
      return Ok(false);
    }
    let property_key_is_index = |key: &PropertyKey| match key {
      PropertyKey::String(key) => key.as_str_lossy().parse::<u32>().is_ok(),
      PropertyKey::Symbol(_) => false,
    };
    let mut prototype = {
      let properties = properties.borrow();
      if !properties.extensible
        || properties
          .properties
          .contains_key(&PropertyKey::String("push".into()))
        || properties
          .properties
          .keys()
          .any(&property_key_is_index)
        || properties
          .properties
          .get(&PropertyKey::String("length".into()))
          .is_some_and(|descriptor| {
            !matches!(descriptor, PropertyDescriptor::Data { writable: true, .. })
          })
      {
        return Ok(false);
      }
      properties.get_prototype()
    };
    let mut canonical_push = false;
    for _ in 0..MAX_PROTOTYPE_DEPTH {
      let Value::Object(object) = prototype else {
        break;
      };
      let object = object.borrow();
      if object
        .properties
        .keys()
        .any(&property_key_is_index)
      {
        return Ok(false);
      }
      if let Some(descriptor) = object
        .properties
        .get(&PropertyKey::String("push".into()))
      {
        canonical_push = matches!(descriptor, PropertyDescriptor::Data { value: Value::BuiltinObject(_, properties), .. } if matches!(properties.borrow().internal_slot("[[DenkoArrayPush]]"), Some(Value::Bool(true))));
        break;
      }
      prototype = object.get_prototype();
    }
    if !canonical_push {
      return Ok(false);
    }
    let Some(crate::bytecode::Constant::Number(step)) = self
      .execution
      .active
      .chunk
      .constants
      .get(decode_risc_aux16(words[19]) as usize)
    else {
      return Ok(false);
    };
    let step = *step;
    let mut next = index;
    let mut values = Vec::with_capacity(64);
    while values.len() < 64 && next < limit {
      values.push(Some(Value::Number(next)));
      next += step;
    }
    let new_length = elements
      .borrow()
      .len()
      .checked_add(values.len())
      .filter(|length| *length <= u32::MAX as usize);
    let Some(new_length) = new_length else {
      return Ok(false);
    };
    elements.borrow_mut().extend(values);
    let index_register = decode_rd(words[0]) as usize;
    let limit_register = decode_rd(words[1]) as usize;
    let step_register = decode_rd(words[19]) as usize;
    let receiver = Value::Array(Rc::clone(&elements), Rc::clone(&properties));
    self.bump_object_generation(&receiver);
    ensure_slot(&mut self.execution.active.locals, increment_temporary_slot);
    *self.execution.active.locals[increment_temporary_slot].borrow_mut() = Value::Number(next);
    *self.execution.active.locals[index_slot].borrow_mut() = Value::Number(next);
    registers[index_register] = Value::Number(next);
    registers[limit_register] = Value::Number(limit);
    registers[0] = Value::Number(usize_to_f64(new_length));
    registers[step_register] = Value::Number(step);
    self.execution.active.ip = if next < limit { ip } else { ip + 27 };
    Ok(true)
  }

  fn try_numeric_property_loop_sequence(
    &mut self,
    registers: &mut [Value; 16],
  ) -> Result<bool, RuntimeError> {
    let ip = self.execution.active.ip;
    let Some(words) = self.execution.active.chunk.code.get(ip..ip + 24) else {
      return Ok(false);
    };
    let index_slot = decode_risc_aux16(words[0]) as usize;
    let accumulator_slot = decode_risc_aux16(words[3]) as usize;
    let object_slot = decode_risc_aux16(words[4]) as usize;
    let temporary_slot = decode_risc_aux16(words[8]) as usize;
    let (condition, left, right, exit_offset) = decode_compare_jump(words[2]);
    if condition != 2
      || left != decode_rd(words[0])
      || right != decode_rd(words[1])
      || exit_offset <= 0
      || decode_rd(words[6]) != decode_rd(words[4])
      || decode_rs(words[6]) != decode_rd(words[5])
      || decode_risc_aux16(words[9]) as usize != accumulator_slot
      || decode_risc_aux16(words[10]) as usize != temporary_slot
      || decode_risc_aux16(words[11]) as usize != accumulator_slot
      || decode_risc_aux16(words[13]) as usize != index_slot
      || decode_risc_aux16(words[19]) as usize != temporary_slot
      || decode_risc_aux16(words[20]) as usize != index_slot
      || decode_risc_aux16(words[21]) as usize != temporary_slot
      || decode_risc_aux16(words[22]) as usize != index_slot
      || self.execution.active.arguments.object.is_some()
      || self
        .execution
        .active
        .lexical_env
        .borrow()
        .has_object_environment()
      || self
        .execution
        .active
        .locals
        .get(index_slot)
        .is_some_and(|cell| Rc::strong_count(cell) != 1)
      || [index_slot, accumulator_slot, temporary_slot]
        .into_iter()
        .any(|slot| {
          self
            .execution
            .active
            .slots
            .as_ref()
            .and_then(|slots| slots.immutable.get(slot))
            .copied()
            .unwrap_or(false)
        })
    {
      return Ok(false);
    }
    let number = |slot: usize| {
      self
        .execution
        .active
        .locals
        .get(slot)
        .and_then(|cell| match *cell.borrow() {
          Value::Number(value) => Some(value),
          _ => None,
        })
    };
    let Some(index) = number(index_slot) else {
      return Ok(false);
    };
    let Some(accumulator) = number(accumulator_slot) else {
      return Ok(false);
    };
    let object = self
      .execution
      .active
      .locals
      .get(object_slot)
      .and_then(|cell| match &*cell.borrow() {
        Value::Object(object) => Some(Rc::clone(object)),
        _ => None,
      });
    let Some(object) = object else {
      return Ok(false);
    };
    let Some(crate::bytecode::Constant::Number(limit)) = self
      .execution
      .active
      .chunk
      .constants
      .get(decode_risc_aux16(words[1]) as usize)
    else {
      return Ok(false);
    };
    if index >= *limit {
      self.execution.active.ip = ip + 24;
      return Ok(true);
    }
    let Some(crate::bytecode::Constant::String(key)) = self
      .execution
      .active
      .chunk
      .constants
      .get(decode_risc_aux16(words[5]) as usize)
    else {
      return Ok(false);
    };
    let property = {
      let object = object.borrow();
      match object
        .properties
        .get(&PropertyKey::String(key.clone()))
      {
        Some(PropertyDescriptor::Data {
          value: Value::Number(value),
          ..
        }) => Some(*value),
        _ => None,
      }
    };
    let Some(property) = property else {
      return Ok(false);
    };
    let Some(crate::bytecode::Constant::Number(step)) = self
      .execution
      .active
      .chunk
      .constants
      .get(decode_risc_aux16(words[16]) as usize)
    else {
      return Ok(false);
    };
    let mut next = index;
    let mut sum = accumulator;
    let mut iterations = 0;
    while iterations < 64 && next < *limit {
      sum += property;
      next += *step;
      iterations += 1;
    }
    ensure_slot(&mut self.execution.active.locals, temporary_slot);
    *self.execution.active.locals[temporary_slot].borrow_mut() = Value::Number(next);
    *self.execution.active.locals[accumulator_slot].borrow_mut() = Value::Number(sum);
    *self.execution.active.locals[index_slot].borrow_mut() = Value::Number(next);
    registers[decode_rd(words[0]) as usize] = Value::Number(next);
    registers[decode_rd(words[1]) as usize] = Value::Number(*limit);
    registers[decode_rd(words[3]) as usize] = Value::Number(sum);
    registers[decode_rd(words[16]) as usize] = Value::Number(*step);
    self.execution.active.ip = if next < *limit { ip } else { ip + 24 };
    Ok(true)
  }

  fn try_numeric_call_loop_sequence(
    &mut self,
    registers: &mut [Value; 16],
  ) -> Result<bool, RuntimeError> {
    let ip = self.execution.active.ip;
    let chunk = Arc::clone(&self.execution.active.chunk);
    let (words, call_offset, argument_constant_offset, increment_offset) = if let Some(words) =
      chunk.code.get(ip..ip + 22)
      && (words[6] & 0xff) as u8 == Op::Call as u8
    {
      (words, 6, None, 15)
    } else if let Some(words) = chunk.code.get(ip..ip + 24)
      && (words[8] & 0xff) as u8 == Op::Call as u8
    {
      (words, 8, Some(5), 17)
    } else {
      return Ok(false);
    };
    let index_slot = decode_risc_aux16(words[0]) as usize;
    let accumulator_slot = decode_risc_aux16(words[3]) as usize;
    let callee_slot = decode_risc_aux16(words[4]) as usize;
    let temporary_slot = decode_risc_aux16(words[call_offset + 3]) as usize;
    let (condition, left, right, exit_offset) = decode_compare_jump(words[2]);
    let loop_len = words.len();
    let jump_payload = ((words[loop_len - 1] >> 8) & 0x00ff_ffff) as i32;
    let jump_offset = if jump_payload & 0x0080_0000 != 0 {
      jump_payload | !0x00ff_ffff
    } else {
      jump_payload
    };
    let expected_exit = ip + loop_len;
    let actual_exit = (ip as i64 + 3 + exit_offset as i64) as usize;
    let actual_backedge = (ip as i64 + loop_len as i64 + jump_offset as i64) as usize;
    let accumulator_register = decode_rd(words[3]);
    let callee_register = decode_rd(words[4]);
    let result_register = decode_rd(words[call_offset + 1]);
    let accumulator_store = call_offset + 3;
    let increment_register = decode_rd(words[increment_offset - 1]);
    if condition != 2
      || left != decode_rd(words[0])
      || right != decode_rd(words[1])
      || actual_exit != expected_exit
      || actual_backedge != ip
      || decode_rd(words[call_offset - 1]) != 0
      || decode_rs(words[call_offset - 1]) != callee_register
      || decode_risc_aux16(words[call_offset]) as usize
        != argument_constant_offset.is_some() as usize
      || decode_rs(words[call_offset + 1]) != 0
      || decode_rd(words[call_offset + 2]) != accumulator_register
      || decode_rs(words[call_offset + 2]) != result_register
      || decode_rd(words[accumulator_store]) != accumulator_register
      || decode_risc_aux16(words[accumulator_store]) as usize != temporary_slot
      || decode_risc_aux16(words[accumulator_store + 1]) as usize != accumulator_slot
      || decode_risc_aux16(words[accumulator_store + 2]) as usize != temporary_slot
      || decode_rd(words[accumulator_store + 3]) != decode_rd(words[accumulator_store + 2])
      || decode_risc_aux16(words[accumulator_store + 3]) as usize != accumulator_slot
      || decode_risc_aux16(words[accumulator_store + 4]) as usize != index_slot
      || decode_risc_aux16(words[increment_offset - 1]) as usize != index_slot
      || decode_rd(words[increment_offset + 1]) != increment_register
      || decode_rs(words[increment_offset + 1]) != decode_rd(words[increment_offset])
      || decode_risc_aux16(words[increment_offset + 2]) as usize != temporary_slot
      || decode_risc_aux16(words[increment_offset + 3]) as usize != index_slot
      || decode_risc_aux16(words[increment_offset + 4]) as usize != temporary_slot
      || decode_rd(words[increment_offset + 5]) != decode_rd(words[increment_offset + 4])
      || decode_risc_aux16(words[loop_len - 2]) as usize != index_slot
      || index_slot == accumulator_slot
      || index_slot == temporary_slot
      || accumulator_slot == temporary_slot
      || self.execution.active.arguments.object.is_some()
      || self
        .execution
        .active
        .lexical_env
        .borrow()
        .has_object_environment()
      || self
        .execution
        .active
        .locals
        .get(index_slot)
        .is_some_and(|cell| Rc::strong_count(cell) != 1)
      || [index_slot, accumulator_slot, temporary_slot]
        .into_iter()
        .any(|slot| {
          self
            .execution
            .active
            .slots
            .as_ref()
            .and_then(|slots| slots.immutable.get(slot))
            .copied()
            .unwrap_or(false)
        })
    {
      return Ok(false);
    }
    if let Some(argument_offset) = argument_constant_offset
      && (decode_rd(words[argument_offset + 1]) != 2
        || decode_rs(words[argument_offset + 1]) != decode_rd(words[argument_offset]))
    {
      return Ok(false);
    }
    let number_local = |slot: usize| {
      self
        .execution
        .active
        .locals
        .get(slot)
        .and_then(|cell| match *cell.borrow() {
          Value::Number(value) => Some(value),
          _ => None,
        })
    };
    let number_constant = |word: u32| match chunk
      .constants
      .get(decode_risc_aux16(word) as usize)
    {
      Some(crate::bytecode::Constant::Number(value)) => Some(*value),
      _ => None,
    };
    let Some(index) = number_local(index_slot) else {
      return Ok(false);
    };
    let Some(accumulator) = number_local(accumulator_slot) else {
      return Ok(false);
    };
    let Some(limit) = number_constant(words[1]) else {
      return Ok(false);
    };
    let Some(step) = number_constant(words[increment_offset]) else {
      return Ok(false);
    };
    let callee = self
      .execution
      .active
      .locals
      .get(callee_slot)
      .map(|cell| cell.borrow().clone());
    let Some(Value::Closure { proto, .. }) = callee else {
      return Ok(false);
    };
    let call_site = ip + call_offset;
    let direct = self
      .caches
      .active_direct_calls
      .borrow()
      .get(call_site)
      .and_then(Clone::clone)
      .filter(|entry| Rc::ptr_eq(&entry.expected_proto, &proto));
    let Some(direct) = direct.filter(|entry| entry.target.is_valid()) else {
      return Ok(false);
    };
    let argument_value = argument_constant_offset.and_then(|offset| number_constant(words[offset]));
    let leaf_value = match direct.target.leaf() {
      crate::vm::jit::DirectLeaf::Number(value) => value,
      crate::vm::jit::DirectLeaf::Parameter(0) => {
        let Some(value) = argument_value else {
          return Ok(false);
        };
        value
      }
      crate::vm::jit::DirectLeaf::Parameter(_) => return Ok(false),
    };
    let mut frame = crate::vm::jit::native::NumericCallLoopFrame {
      index,
      limit,
      accumulator,
      step,
      leaf_value,
      iterations: 0,
    };
    self
      .profiling
      .jit
      .run_numeric_call_loop(&mut frame)
      .map_err(RuntimeError::from)?;
    if frame.iterations > 64 {
      return Err(RuntimeError::from(
        "native numeric call loop exceeded its batch bound",
      ));
    }
    if frame.iterations > 0 {
      *self.execution.active.locals[accumulator_slot].borrow_mut() =
        Value::Number(frame.accumulator);
      *self.execution.active.locals[index_slot].borrow_mut() = Value::Number(frame.index);
      *self.execution.active.locals[temporary_slot].borrow_mut() = Value::Number(frame.index);
      registers[0] = Value::Number(leaf_value);
    }
    registers[decode_rd(words[0]) as usize] = Value::Number(frame.index);
    registers[decode_rd(words[1]) as usize] = Value::Number(limit);
    if let Some(argument_value) = argument_value {
      registers[2] = Value::Number(argument_value);
    }
    self.execution.active.ip = if frame.index < frame.limit {
      ip
    } else {
      expected_exit
    };
    self.profiling.jit_stats.native_loop_entries = self
      .profiling
      .jit_stats
      .native_loop_entries
      .saturating_add(1);
    self.profiling.jit_stats.native_loop_iterations = self
      .profiling
      .jit_stats
      .native_loop_iterations
      .saturating_add(frame.iterations as u64);
    Ok(true)
  }

  fn try_numeric_loop_sequence(
    &mut self,
    registers: &mut [Value; 16],
  ) -> Result<bool, RuntimeError> {
    let ip = self.execution.active.ip;
    let Some(words) = self.execution.active.chunk.code.get(ip..ip + 22) else {
      return Ok(false);
    };
    let ops = [
      Op::LoadLocal,
      Op::Const,
      Op::CompareJump,
      Op::LoadLocal,
      Op::LoadLocal,
      Op::Add,
      Op::StoreLocal,
      Op::LoadLocal,
      Op::LoadLocal,
      Op::StoreLocal,
      Op::CloneLocalCell,
      Op::LoadLocal,
      Op::Neg,
      Op::Neg,
      Op::Const,
      Op::Move,
      Op::Add,
      Op::StoreLocal,
      Op::LoadLocal,
      Op::LoadLocal,
      Op::StoreLocal,
      Op::Jump,
    ];
    if !words
      .iter()
      .zip(ops)
      .all(|(word, op)| (*word & 0xff) as u8 == op as u8)
    {
      return Ok(false);
    }
    let index_slot = decode_risc_aux16(words[0]) as usize;
    let accumulator_slot = decode_risc_aux16(words[3]) as usize;
    let temporary_slot = decode_risc_aux16(words[6]) as usize;
    let (condition, left, right, exit_offset) = decode_compare_jump(words[2]);
    if condition != 2
      || left != decode_rd(words[0])
      || right != decode_rd(words[1])
      || exit_offset <= 0
      || decode_risc_aux16(words[7]) as usize != accumulator_slot
      || decode_risc_aux16(words[8]) as usize != temporary_slot
      || decode_risc_aux16(words[9]) as usize != accumulator_slot
      || decode_risc_aux16(words[11]) as usize != index_slot
      || decode_risc_aux16(words[17]) as usize != temporary_slot
      || decode_risc_aux16(words[18]) as usize != index_slot
      || decode_risc_aux16(words[19]) as usize != temporary_slot
      || decode_risc_aux16(words[20]) as usize != index_slot
      || self.execution.active.arguments.object.is_some()
      || self
        .execution
        .active
        .lexical_env
        .borrow()
        .has_object_environment()
      || self
        .execution
        .active
        .locals
        .get(index_slot)
        .is_some_and(|cell| Rc::strong_count(cell) != 1)
      || [index_slot, accumulator_slot, temporary_slot]
        .into_iter()
        .any(|slot| {
          self
            .execution
            .active
            .slots
            .as_ref()
            .and_then(|slots| slots.immutable.get(slot))
            .copied()
            .unwrap_or(false)
        })
    {
      return Ok(false);
    }
    let number = |slot: usize| {
      self
        .execution
        .active
        .locals
        .get(slot)
        .and_then(|cell| match *cell.borrow() {
          Value::Number(value) => Some(value),
          _ => None,
        })
    };
    let Some(index) = number(index_slot) else {
      return Ok(false);
    };
    let Some(accumulator) = number(accumulator_slot) else {
      return Ok(false);
    };
    let Some(crate::bytecode::Constant::Number(limit)) = self
      .execution
      .active
      .chunk
      .constants
      .get(decode_risc_aux16(words[1]) as usize)
    else {
      return Ok(false);
    };
    if index >= *limit {
      self.execution.active.ip = ip + 22;
      return Ok(true);
    }
    let Some(crate::bytecode::Constant::Number(step)) = self
      .execution
      .active
      .chunk
      .constants
      .get(decode_risc_aux16(words[14]) as usize)
    else {
      return Ok(false);
    };
    // This loop body has no calls, allocations, observable property accesses,
    // or object environments. Execute a small batch before returning to the
    // dispatch/GC boundary, preserving exact left-to-right f64 operations.
    let mut next = index;
    let mut sum = accumulator;
    let mut iterations = 0;
    while iterations < 64 && next < *limit {
      sum += next;
      next += *step;
      iterations += 1;
    }
    ensure_slot(&mut self.execution.active.locals, temporary_slot);
    *self.execution.active.locals[temporary_slot].borrow_mut() = Value::Number(next);
    *self.execution.active.locals[accumulator_slot].borrow_mut() = Value::Number(sum);
    *self.execution.active.locals[index_slot].borrow_mut() = Value::Number(next);
    registers[decode_rd(words[0]) as usize] = Value::Number(next);
    registers[decode_rd(words[1]) as usize] = Value::Number(*limit);
    registers[decode_rd(words[3]) as usize] = Value::Number(sum);
    registers[decode_rd(words[14]) as usize] = Value::Number(*step);
    self.execution.active.ip = if next < *limit { ip } else { ip + 22 };
    Ok(true)
  }

  /// Execute `LoadLocal, Const, CompareJump` with numeric guards in one dispatch.
  fn try_numeric_compare_branch_sequence(
    &mut self,
    registers: &mut [Value; 16],
  ) -> Result<bool, RuntimeError> {
    let ip = self.execution.active.ip;
    let words = &self.execution.active.chunk.code[ip..ip + 3];
    let left_register = decode_rd(words[0]) as usize;
    let right_register = decode_rd(words[1]) as usize;
    let (condition, _, _, offset) = decode_compare_jump(words[2]);

    let left = {
      let slot = decode_risc_aux16(words[0]) as usize;
      let Some(cell) = self.execution.active.locals.get(slot) else {
        return Ok(false);
      };
      match *cell.borrow() {
        Value::Number(value) => Some(value),
        _ => None,
      }
    };
    let Some(left) = left else {
      return Ok(false);
    };
    let Some(crate::bytecode::Constant::Number(right)) = self
      .execution
      .active
      .chunk
      .constants
      .get(decode_risc_aux16(words[1]) as usize)
    else {
      return Ok(false);
    };
    let right = *right;
    let ordered = !left.is_nan() && !right.is_nan();
    let result = match condition {
      0 | 6 => left == right,
      1 | 7 => left != right,
      2 => ordered && left < right,
      3 => ordered && left <= right,
      4 => ordered && left > right,
      5 => ordered && left >= right,
      _ => return Ok(false),
    };

    registers[left_register] = Value::Number(left);
    registers[right_register] = Value::Number(right);
    self.execution.active.ip = if result {
      ip + 3
    } else {
      usize::try_from(ip as i64 + 3 + offset as i64)
        .map_err(|_| RuntimeError::from("negative RISC compare target"))?
    };
    Ok(true)
  }

  /// Execute the compiler's one-argument method-call setup in one dispatch.
  fn try_single_argument_method_call_sequence(
    &mut self,
    registers: &mut [Value; 16],
  ) -> Result<bool, RuntimeError> {
    let ip = self.execution.active.ip;
    // The quickening pass has already validated this complete instruction
    // shape and all register relationships.
    let words = &self.execution.active.chunk.code[ip..ip + 12];
    let receiver_register = decode_rd(words[0]) as usize;
    let callee_register = decode_rd(words[3]) as usize;
    let argument_register = decode_rd(words[6]) as usize;
    let key_register = decode_rd(words[4]) as usize;

    let receiver_slot = decode_risc_aux16(words[0]) as usize;
    let temporary_slot = decode_risc_aux16(words[1]) as usize;
    let argument_slot = decode_risc_aux16(words[6]) as usize;
    if self.execution.active.arguments.object.is_some()
      || self
        .execution
        .active
        .lexical_env
        .borrow()
        .has_object_environment()
      || self
        .execution
        .active
        .slots
        .as_ref()
        .and_then(|slots| slots.immutable.get(temporary_slot))
        .copied()
        .unwrap_or(false)
    {
      return Ok(false);
    }

    let Some(receiver) = self
      .execution
      .active
      .locals
      .get(receiver_slot)
      .map(|cell| cell.borrow().clone())
    else {
      return Ok(false);
    };
    let Some(argument) = self
      .execution
      .active
      .locals
      .get(argument_slot)
      .map(|cell| cell.borrow().clone())
    else {
      return Ok(false);
    };
    let Some(crate::bytecode::Constant::String(key)) = self
      .execution
      .active
      .chunk
      .constants
      .get(decode_risc_aux16(words[4]) as usize)
    else {
      return Ok(false);
    };
    let key = PropertyKey::String(key.clone());

    ensure_slot(&mut self.execution.active.locals, temporary_slot);
    *self.execution.active.locals[temporary_slot].borrow_mut() = receiver.clone();
    let callee = self.get_property_value_cached(ip + 5, &receiver, &key)?;
    let canonical_push = matches!(
      &callee,
      Value::BuiltinObject(_, properties)
        if matches!(properties.borrow().internal_slot("[[DenkoArrayPush]]"), Some(Value::Bool(true)))
    );
    let direct_push = if canonical_push {
      if let Value::Array(elements, properties) = &receiver {
        let length = elements.borrow().len();
        let index_key = PropertyKey::String(length.to_string().into());
        let properties_allow_push = {
          let properties = properties.borrow();
          properties.extensible
            && properties
              .properties
              .get(&PropertyKey::String("length".into()))
              .is_none_or(|descriptor| {
                matches!(descriptor, PropertyDescriptor::Data { writable: true, .. })
              })
        };
        if properties_allow_push
          && self
            .lookup_property_descriptor(&receiver, &index_key, 0)?
            .is_none()
        {
          elements.borrow_mut().push(Some(argument.clone()));
          self.bump_object_generation(&receiver);
          Some(Value::Number(usize_to_f64(length + 1)))
        } else {
          None
        }
      } else {
        None
      }
    } else {
      None
    };
    let result = if let Some(result) = direct_push {
      result
    } else {
      self.call_value_with_this(&callee, &receiver, std::slice::from_ref(&argument))?
    };

    registers[receiver_register] = result.clone();
    registers[callee_register] = callee;
    registers[argument_register] = argument.clone();
    registers[key_register] = match key {
      PropertyKey::String(key) => Value::String(key),
      PropertyKey::Symbol(_) => unreachable!(),
    };
    registers[0] = result;
    registers[1] = receiver;
    registers[2] = argument;
    self.execution.active.ip += 12;
    Ok(true)
  }

  /// Execute a guarded own-data-property numeric accumulation in one dispatch.
  fn try_numeric_property_add_sequence(
    &mut self,
    registers: &mut [Value; 16],
  ) -> Result<bool, RuntimeError> {
    let ip = self.execution.active.ip;
    // The quickening pass has already validated this complete instruction
    // shape, register flow, and slot flow.
    let words = &self.execution.active.chunk.code[ip..ip + 9];
    let accumulator_register = decode_rd(words[0]) as usize;
    let property_register = decode_rd(words[1]) as usize;
    let key_register = decode_rd(words[2]) as usize;
    let accumulator_slot = decode_risc_aux16(words[0]) as usize;
    let object_slot = decode_risc_aux16(words[1]) as usize;
    let temporary_slot = decode_risc_aux16(words[5]) as usize;
    if self.execution.active.arguments.object.is_some()
      || self
        .execution
        .active
        .lexical_env
        .borrow()
        .has_object_environment()
      || [temporary_slot, accumulator_slot]
        .into_iter()
        .any(|slot| {
          self
            .execution
            .active
            .slots
            .as_ref()
            .and_then(|slots| slots.immutable.get(slot))
            .copied()
            .unwrap_or(false)
        })
    {
      return Ok(false);
    }

    let accumulator = {
      let Some(cell) = self.execution.active.locals.get(accumulator_slot) else {
        return Ok(false);
      };
      match *cell.borrow() {
        Value::Number(value) => Some(value),
        _ => None,
      }
    };
    let Some(accumulator) = accumulator else {
      return Ok(false);
    };
    let object = {
      let Some(cell) = self.execution.active.locals.get(object_slot) else {
        return Ok(false);
      };
      match &*cell.borrow() {
        Value::Object(object) => Some(Rc::clone(object)),
        _ => None,
      }
    };
    let Some(object) = object else {
      return Ok(false);
    };
    let Some(crate::bytecode::Constant::String(key)) = self
      .execution
      .active
      .chunk
      .constants
      .get(decode_risc_aux16(words[2]) as usize)
    else {
      return Ok(false);
    };
    let key = PropertyKey::String(key.clone());
    let property = {
      let object = object.borrow();
      match object.properties.get(&key) {
        Some(PropertyDescriptor::Data {
          value: Value::Number(value),
          ..
        }) => Some(*value),
        _ => None,
      }
    };
    let Some(property) = property else {
      return Ok(false);
    };
    let result = accumulator + property;

    ensure_slot(&mut self.execution.active.locals, temporary_slot);
    registers[accumulator_register] = Value::Number(result);
    registers[property_register] = Value::Number(result);
    registers[key_register] = Value::String(match key {
      PropertyKey::String(key) => key,
      PropertyKey::Symbol(_) => unreachable!(),
    });
    *self.execution.active.locals[temporary_slot].borrow_mut() = Value::Number(result);
    *self.execution.active.locals[accumulator_slot].borrow_mut() = Value::Number(result);
    self.execution.active.ip += 9;
    Ok(true)
  }

  /// Execute the compiler's guarded numeric increment sequence in one dispatch.
  fn try_numeric_increment_sequence(
    &mut self,
    registers: &mut [Value; 16],
  ) -> Result<bool, RuntimeError> {
    let ip = self.execution.active.ip;
    let words = &self.execution.active.chunk.code[ip..ip + 10];
    let source_register = decode_rd(words[0]) as usize;
    let constant_register = decode_rd(words[3]) as usize;
    let result_register = decode_rd(words[4]) as usize;
    let target_slot = decode_risc_aux16(words[0]) as usize;
    let temporary_slot = decode_risc_aux16(words[6]) as usize;
    if self.execution.active.arguments.object.is_some()
      || self
        .execution
        .active
        .lexical_env
        .borrow()
        .has_object_environment()
      || [temporary_slot, target_slot]
        .into_iter()
        .any(|slot| {
          self
            .execution
            .active
            .slots
            .as_ref()
            .and_then(|slots| slots.immutable.get(slot))
            .copied()
            .unwrap_or(false)
        })
    {
      return Ok(false);
    }

    let value = {
      let Some(cell) = self.execution.active.locals.get(target_slot) else {
        return Ok(false);
      };
      match *cell.borrow() {
        Value::Number(value) => Some(value),
        _ => None,
      }
    };
    let Some(value) = value else {
      return Ok(false);
    };
    let Some(crate::bytecode::Constant::Number(constant)) = self
      .execution
      .active
      .chunk
      .constants
      .get(decode_risc_aux16(words[3]) as usize)
    else {
      return Ok(false);
    };
    let constant = *constant;
    let result = value + constant;

    ensure_slot(&mut self.execution.active.locals, temporary_slot);
    registers[source_register] = Value::Number(value);
    registers[constant_register] = Value::Number(constant);
    registers[result_register] = Value::Number(result);
    *self.execution.active.locals[temporary_slot].borrow_mut() = Value::Number(result);
    *self.execution.active.locals[target_slot].borrow_mut() = Value::Number(result);
    self.execution.active.ip += 10;
    Ok(true)
  }

  /// Execute the common `LoadLocal, LoadLocal, Add, StoreLocal` sequence in
  /// one interpreter dispatch when every value is already numeric.
  fn try_numeric_add_local_sequence(
    &mut self,
    registers: &mut [Value; 16],
  ) -> Result<bool, RuntimeError> {
    let ip = self.execution.active.ip;
    let words = &self.execution.active.chunk.code[ip..ip + 4];
    let left_register = decode_rd(words[0]) as usize;
    let right_register = decode_rd(words[1]) as usize;

    let left_slot = decode_risc_aux16(words[0]) as usize;
    let right_slot = decode_risc_aux16(words[1]) as usize;
    let destination_slot = decode_risc_aux16(words[3]) as usize;
    if self.execution.active.arguments.object.is_some()
      || self
        .execution
        .active
        .lexical_env
        .borrow()
        .has_object_environment()
      || self
        .execution
        .active
        .slots
        .as_ref()
        .and_then(|slots| slots.immutable.get(destination_slot))
        .copied()
        .unwrap_or(false)
    {
      return Ok(false);
    }

    let operands = {
      let Some(left_cell) = self.execution.active.locals.get(left_slot) else {
        return Ok(false);
      };
      let Some(right_cell) = self.execution.active.locals.get(right_slot) else {
        return Ok(false);
      };
      let left = left_cell.borrow();
      let right = right_cell.borrow();
      match (&*left, &*right) {
        (Value::Number(left), Value::Number(right)) => Some((*left, *right)),
        _ => None,
      }
    };
    let Some((left, right)) = operands else {
      return Ok(false);
    };
    let result = left + right;

    ensure_slot(&mut self.execution.active.locals, destination_slot);
    registers[left_register] = Value::Number(left);
    registers[right_register] = Value::Number(right);
    *self.execution.active.locals[destination_slot].borrow_mut() = Value::Number(result);
    registers[left_register] = Value::Number(result);
    self.execution.active.ip += 4;
    Ok(true)
  }

  /// Execute a chunk without resetting the caller's live lexical bindings.
  ///
  /// This is used for direct `eval`, where the evaluated code must observe
  /// and update the current execution context.
  #[allow(clippy::too_many_lines)]
  pub(crate) fn run_nested(
    &mut self,
    chunk: Chunk,
    direct_eval: bool,
  ) -> Result<Value, NestedRunFailure> {
    chunk.verify().map_err(|error| NestedRunFailure {
      error: RuntimeError::from(format!("Invalid bytecode: {}", error)),
      thrown: None,
    })?;
    let saved_context = self.execution.active.clone();
    let saved_root_chunk = self.execution.root_chunk.clone();
    let saved_pending_closure_env = self.execution.pending_closure_env.clone();
    let saved_gc_temp_roots = self.roots.temporary.clone();
    let saved_recent_return_value = self.construction.recent_return_value.clone();
    let saved_last_uncaught_exception = self.exceptions.last_uncaught.take();
    let saved_pending_exception = self.exceptions.pending.clone();
    let saved_pending_exception_frame_count = self.exceptions.pending_frame_count;
    let saved_direct_eval_outer = self.execution.direct_eval_outer.clone();

    if direct_eval {
      self.execution.direct_eval_outer =
        self
          .execution
          .active
          .slots
          .as_ref()
          .map(|slots| DirectEvalState {
            names: slots.names.clone(),
            immutable: slots.immutable.clone(),
            sloppy_immutable_silent: slots.sloppy_immutable_silent.clone(),
          });
    }

    let chunk_rc = Arc::new(chunk);
    self.execution.root_chunk = Some(Arc::clone(&chunk_rc));
    self.execution.active.chunk = chunk_rc;
    self.execution.active.ip = 0;
    self.execution.active.strict = self.execution.active.chunk.root_strict;
    if !direct_eval {
      self.execution.active.lexical_env = Rc::clone(&self.realm_state.global_env);
    }
    self.roots.temporary.clear();
    self.execution.active.yield_star = None;
    if self
      .execution
      .active
      .chunk
      .root_slot_names
      .is_empty()
    {
      if !direct_eval {
        self.execution.active.locals.clear();
        self.execution.active.slots = None;
      }
    } else {
      self.initialize_root_bindings();
    }
    if !direct_eval {
      self.execution.active.this_value = self
        .realm_state
        .globals
        .borrow()
        .properties
        .get(&PropertyKey::String("globalThis".to_string().into()))
        .and_then(|d| match d {
          PropertyDescriptor::Data { value, .. } => Some(value.clone()),
          _ => None,
        })
        .unwrap_or(Value::Undefined);
    }
    self.begin_opcode_profile_run();

    let result = self.run_risc_loop(true);
    let nested_thrown = self.exceptions.last_uncaught.take();

    if !direct_eval {
      self.persist_root_bindings();
    }

    self.execution.active = saved_context;
    self.execution.root_chunk = saved_root_chunk;
    self.execution.pending_closure_env = saved_pending_closure_env;
    self.roots.temporary = saved_gc_temp_roots;
    self.construction.recent_return_value = saved_recent_return_value;
    self.exceptions.last_uncaught = saved_last_uncaught_exception;
    self.exceptions.pending = saved_pending_exception;
    self.exceptions.pending_frame_count = saved_pending_exception_frame_count;
    self.execution.direct_eval_outer = saved_direct_eval_outer;

    result.map_err(|error| NestedRunFailure {
      error,
      thrown: nested_thrown,
    })
  }

  fn persist_root_bindings(&mut self) {
    let Some(slots) = self.execution.active.slots.as_ref() else {
      return;
    };
    let names = slots.names.clone();
    let immutable = slots.immutable.clone();
    let locals = self.execution.active.locals.clone();
    let mut global_env = self.realm_state.global_env.borrow_mut();

    for (index, name) in names.into_iter().enumerate() {
      let (Some(name), Some(cell)) = (name, locals.get(index)) else {
        continue;
      };
      let initialized = !matches!(*cell.borrow(), Value::Uninitialized);
      if immutable.get(index).copied().unwrap_or(false) {
        global_env.create_immutable_binding(name, Rc::clone(cell), initialized);
      } else {
        global_env.create_mutable_binding(name, Rc::clone(cell), initialized);
      }
    }
  }

  fn initialize_root_bindings(&mut self) {
    let slot_names = self
      .execution
      .active
      .chunk
      .root_slot_names
      .clone();
    let slot_tdz = self
      .execution
      .active
      .chunk
      .root_lexical_tdz
      .clone();
    let slot_types = self
      .execution
      .active
      .chunk
      .root_slot_types
      .clone();
    let slot_immutable = self
      .execution
      .active
      .chunk
      .root_slot_immutable
      .clone();
    let slot_sloppy_immutable_silent = vec![false; slot_names.len()];
    let slot_count = slot_names
      .len()
      .max(slot_tdz.len())
      .max(slot_types.len())
      .max(slot_immutable.len());
    if slot_count == 0 {
      return;
    }

    self.execution.active.locals =
      std::iter::repeat_with(|| Rc::new(RefCell::new(Value::Undefined)))
        .take(slot_count)
        .collect();
    for (idx, is_tdz) in slot_tdz.iter().copied().enumerate() {
      if is_tdz && idx < self.execution.active.locals.len() {
        *self.execution.active.locals[idx].borrow_mut() = Value::Uninitialized;
      }
    }

    let root_env = Rc::new(RefCell::new(EnvironmentRecord::new(Some(Rc::clone(
      &self.execution.active.lexical_env,
    )))));
    for (idx, maybe_name) in slot_names.iter().enumerate() {
      if let Some(name) = maybe_name
        && idx < self.execution.active.locals.len()
      {
        let initialized = !slot_tdz.get(idx).copied().unwrap_or(false);
        if slot_immutable.get(idx).copied().unwrap_or(false) {
          root_env.borrow_mut().create_immutable_binding(
            name.clone(),
            Rc::clone(&self.execution.active.locals[idx]),
            initialized,
          );
        } else {
          root_env.borrow_mut().create_mutable_binding(
            name.clone(),
            Rc::clone(&self.execution.active.locals[idx]),
            initialized,
          );
        }
        if initialized {
          let value = self.execution.active.locals[idx].borrow().clone();
          self
            .realm_state
            .globals
            .borrow_mut()
            .properties
            .insert(
              PropertyKey::String(name.clone().into()),
              PropertyDescriptor::ordinary_data(value),
            );
        }
      }
    }
    self.execution.active.lexical_env = root_env;
    self.execution.active.slots = Some(SlotMetadata {
      types: slot_types,
      names: slot_names,
      immutable: slot_immutable,
      sloppy_immutable_silent: slot_sloppy_immutable_silent,
    });
  }

  // -----------------------------------------------------------------------
  // Opcode helpers (called from the main loop)
  // -----------------------------------------------------------------------

  pub(super) fn make_iterator_result(
    value: Value,
    done: bool,
  ) -> Value {
    let obj = gc::track_object(Rc::new(RefCell::new(ObjectData::new())));
    obj.borrow_mut().properties.insert(
      PropertyKey::String("value".to_string().into()),
      PropertyDescriptor::ordinary_data(value),
    );
    obj.borrow_mut().properties.insert(
      PropertyKey::String("done".to_string().into()),
      PropertyDescriptor::ordinary_data(Value::Bool(done)),
    );
    Value::Object(obj)
  }

  // Iterator factories own their state in iterator objects; this helper only
  // creates the `{ value, done }` result returned by generator operations.

  /// Restore the caller's frame and return the callee's result.
  pub(super) fn execute_return(
    &mut self,
    result: Value,
  ) -> Result<Value, RuntimeError> {
    let frame = self
      .execution
      .call_stack
      .pop()
      .ok_or("Return without call frame")?;

    // Runtime return-type check intentionally disabled in JS-first mode.
    // TypeScript return annotations are erased at runtime.

    self.execution.active = frame.context;
    self.prune_stale_handlers();

    Ok(result)
  }

  #[inline(always)]
  fn string_constant(
    &self,
    idx: usize,
  ) -> Result<js_string::JsString, RuntimeError> {
    match self.execution.active.chunk.constants.get(idx) {
      Some(crate::bytecode::Constant::String(value)) => Ok(value.clone()),
      Some(_) => Err(RuntimeError::from("Constant is not a string")),
      None => Err(RuntimeError::from("String constant index out of bounds")),
    }
  }

  #[inline(always)]
  fn module_export_descriptor(
    &self,
    index: usize,
  ) -> Result<(usize, String), RuntimeError> {
    let descriptor = self.string_constant(index)?;
    let (module, name) = descriptor
      .as_str_lossy()
      .split_once('\0')
      .ok_or_else(|| RuntimeError::from("Invalid module export descriptor"))?;
    let module = module
      .parse::<usize>()
      .map_err(|_| RuntimeError::from("Invalid module export index"))?;
    Ok((module, name.to_string()))
  }

  pub(super) fn dump_diag(
    &self,
    label: &str,
    offender: &Value,
  ) {
    if !crate::trace::enabled() {
      return;
    }
    eprintln!("[diag] {label}: actual={}", offender.type_name());
    crate::trace::value_string("[diag] offending value", offender);
    let start = self.execution.active.ip.saturating_sub(8);
    let end = usize::min(
      self.execution.active.ip + 8,
      self.execution.active.chunk.code.len(),
    );
    for ip in start..end {
      let instruction = self
        .execution
        .active
        .chunk
        .disassemble_at(ip)
        .unwrap_or_else(|error| format!("{ip:#06} <{error}>"));
      eprintln!("[diag] {instruction}");
    }
  }
}

// ---------------------------------------------------------------------------
// Free functions
// ---------------------------------------------------------------------------

fn build_quickened_sites(code: &[u32]) -> Vec<QuickenedKind> {
  fn matches_ops(
    code: &[u32],
    ip: usize,
    ops: &[Op],
  ) -> bool {
    code
      .get(ip..ip.saturating_add(ops.len()))
      .is_some_and(|words| {
        words
          .iter()
          .zip(ops)
          .all(|(word, op)| (*word & 0xff) as u8 == *op as u8)
      })
  }

  fn is_single_argument_method_call(
    code: &[u32],
    ip: usize,
    ops: &[Op],
  ) -> bool {
    let Some(words) = code.get(ip..ip.saturating_add(ops.len())) else {
      return false;
    };
    if !words
      .iter()
      .zip(ops)
      .all(|(word, op)| (*word & 0xff) as u8 == *op as u8)
      || decode_risc_aux16(words[10]) != 1
    {
      return false;
    }

    let receiver_register = decode_rd(words[0]);
    let callee_register = decode_rd(words[3]);
    let argument_register = decode_rd(words[6]);
    let key_register = decode_rd(words[4]);
    decode_rd(words[1]) == receiver_register
      && decode_rd(words[2]) == receiver_register
      && decode_rd(words[5]) == callee_register
      && decode_rs(words[5]) == key_register
      && decode_rd(words[7]) == 2
      && decode_rs(words[7]) == argument_register
      && decode_rd(words[8]) == 1
      && decode_rs(words[8]) == receiver_register
      && decode_rd(words[9]) == 0
      && decode_rs(words[9]) == callee_register
      && decode_rd(words[11]) == receiver_register
      && decode_rs(words[11]) == 0
      && decode_risc_aux16(words[2]) == decode_risc_aux16(words[1])
      && decode_risc_aux16(words[3]) == decode_risc_aux16(words[1])
  }

  fn is_numeric_property_add(
    code: &[u32],
    ip: usize,
    ops: &[Op],
  ) -> bool {
    let Some(words) = code.get(ip..ip.saturating_add(ops.len())) else {
      return false;
    };
    if !words
      .iter()
      .zip(ops)
      .all(|(word, op)| (*word & 0xff) as u8 == *op as u8)
    {
      return false;
    }

    let accumulator_register = decode_rd(words[0]);
    let property_register = decode_rd(words[1]);
    let key_register = decode_rd(words[2]);
    let accumulator_slot = decode_risc_aux16(words[0]);
    let temporary_slot = decode_risc_aux16(words[5]);
    decode_rd(words[3]) == property_register
      && decode_rs(words[3]) == key_register
      && decode_rd(words[4]) == accumulator_register
      && decode_rs(words[4]) == property_register
      && decode_rd(words[5]) == accumulator_register
      && decode_rd(words[6]) == property_register
      && decode_rd(words[7]) == property_register
      && decode_rd(words[8]) == property_register
      && decode_risc_aux16(words[6]) == accumulator_slot
      && decode_risc_aux16(words[7]) == temporary_slot
      && decode_risc_aux16(words[8]) == accumulator_slot
  }

  fn is_numeric_compare(
    code: &[u32],
    ip: usize,
    ops: &[Op],
  ) -> bool {
    let Some(words) = code.get(ip..ip.saturating_add(ops.len())) else {
      return false;
    };
    if !words
      .iter()
      .zip(ops)
      .all(|(word, op)| (*word & 0xff) as u8 == *op as u8)
    {
      return false;
    }
    let (_, left, right, _) = decode_compare_jump(words[2]);
    left == decode_rd(words[0]) && right == decode_rd(words[1])
  }

  fn is_numeric_increment(
    code: &[u32],
    ip: usize,
    ops: &[Op],
  ) -> bool {
    let Some(words) = code.get(ip..ip.saturating_add(ops.len())) else {
      return false;
    };
    if !words
      .iter()
      .zip(ops)
      .all(|(word, op)| (*word & 0xff) as u8 == *op as u8)
    {
      return false;
    }
    let source = decode_rd(words[0]);
    let constant = decode_rd(words[3]);
    let result = decode_rd(words[4]);
    let target_slot = decode_risc_aux16(words[0]);
    let temporary_slot = decode_risc_aux16(words[6]);
    decode_rd(words[1]) == source
      && decode_rs(words[1]) == source
      && decode_rd(words[2]) == source
      && decode_rs(words[2]) == source
      && decode_rs(words[4]) == source
      && decode_rd(words[5]) == result
      && decode_rs(words[5]) == constant
      && decode_rd(words[6]) == result
      && decode_rd(words[7]) == result
      && decode_rd(words[8]) == result
      && decode_rd(words[9]) == result
      && decode_risc_aux16(words[7]) == target_slot
      && decode_risc_aux16(words[8]) == temporary_slot
      && decode_risc_aux16(words[9]) == target_slot
  }

  fn is_numeric_add_local(
    code: &[u32],
    ip: usize,
    ops: &[Op],
  ) -> bool {
    let Some(words) = code.get(ip..ip.saturating_add(ops.len())) else {
      return false;
    };
    matches_ops(code, ip, ops)
      && decode_rd(words[2]) == decode_rd(words[0])
      && decode_rs(words[2]) == decode_rd(words[1])
      && decode_rd(words[3]) == decode_rd(words[0])
  }

  const ARRAY_PUSH_LOOP: &[Op] = &[
    Op::LoadLocal,
    Op::Const,
    Op::CompareJump,
    Op::LoadLocal,
    Op::StoreLocal,
    Op::LoadLocal,
    Op::LoadLocal,
    Op::Const,
    Op::GetProperty,
    Op::LoadLocal,
    Op::Move,
    Op::Move,
    Op::Move,
    Op::CallBuiltin,
    Op::Move,
    Op::CloneLocalCell,
    Op::LoadLocal,
    Op::Neg,
    Op::Neg,
    Op::Const,
    Op::Move,
    Op::Add,
    Op::StoreLocal,
    Op::LoadLocal,
    Op::LoadLocal,
    Op::StoreLocal,
    Op::Jump,
  ];
  const PROPERTY_LOOP: &[Op] = &[
    Op::LoadLocal,
    Op::Const,
    Op::CompareJump,
    Op::LoadLocal,
    Op::LoadLocal,
    Op::Const,
    Op::GetProperty,
    Op::Add,
    Op::StoreLocal,
    Op::LoadLocal,
    Op::LoadLocal,
    Op::StoreLocal,
    Op::CloneLocalCell,
    Op::LoadLocal,
    Op::Neg,
    Op::Neg,
    Op::Const,
    Op::Move,
    Op::Add,
    Op::StoreLocal,
    Op::LoadLocal,
    Op::LoadLocal,
    Op::StoreLocal,
    Op::Jump,
  ];
  const CONSTANT_CALL_LOOP: &[Op] = &[
    Op::LoadLocal,
    Op::Const,
    Op::CompareJump,
    Op::LoadLocal,
    Op::LoadLocal,
    Op::Move,
    Op::Call,
    Op::Move,
    Op::Add,
    Op::StoreLocal,
    Op::LoadLocal,
    Op::LoadLocal,
    Op::StoreLocal,
    Op::CloneLocalCell,
    Op::LoadLocal,
    Op::Const,
    Op::Add,
    Op::StoreLocal,
    Op::LoadLocal,
    Op::LoadLocal,
    Op::StoreLocal,
    Op::Jump,
  ];
  const IDENTITY_CALL_LOOP: &[Op] = &[
    Op::LoadLocal,
    Op::Const,
    Op::CompareJump,
    Op::LoadLocal,
    Op::LoadLocal,
    Op::Const,
    Op::Move,
    Op::Move,
    Op::Call,
    Op::Move,
    Op::Add,
    Op::StoreLocal,
    Op::LoadLocal,
    Op::LoadLocal,
    Op::StoreLocal,
    Op::CloneLocalCell,
    Op::LoadLocal,
    Op::Const,
    Op::Add,
    Op::StoreLocal,
    Op::LoadLocal,
    Op::LoadLocal,
    Op::StoreLocal,
    Op::Jump,
  ];
  const LOOP: &[Op] = &[
    Op::LoadLocal,
    Op::Const,
    Op::CompareJump,
    Op::LoadLocal,
    Op::LoadLocal,
    Op::Add,
    Op::StoreLocal,
    Op::LoadLocal,
    Op::LoadLocal,
    Op::StoreLocal,
    Op::CloneLocalCell,
    Op::LoadLocal,
    Op::Neg,
    Op::Neg,
    Op::Const,
    Op::Move,
    Op::Add,
    Op::StoreLocal,
    Op::LoadLocal,
    Op::LoadLocal,
    Op::StoreLocal,
    Op::Jump,
  ];
  const COMPARE: &[Op] = &[Op::LoadLocal, Op::Const, Op::CompareJump];
  const METHOD_CALL: &[Op] = &[
    Op::LoadLocal,
    Op::StoreLocal,
    Op::LoadLocal,
    Op::LoadLocal,
    Op::Const,
    Op::GetProperty,
    Op::LoadLocal,
    Op::Move,
    Op::Move,
    Op::Move,
    Op::CallBuiltin,
    Op::Move,
  ];
  const PROPERTY_ADD: &[Op] = &[
    Op::LoadLocal,
    Op::LoadLocal,
    Op::Const,
    Op::GetProperty,
    Op::Add,
    Op::StoreLocal,
    Op::LoadLocal,
    Op::LoadLocal,
    Op::StoreLocal,
  ];
  const INCREMENT: &[Op] = &[
    Op::LoadLocal,
    Op::Neg,
    Op::Neg,
    Op::Const,
    Op::Move,
    Op::Add,
    Op::StoreLocal,
    Op::LoadLocal,
    Op::LoadLocal,
    Op::StoreLocal,
  ];
  const ADD_LOCAL: &[Op] = &[Op::LoadLocal, Op::LoadLocal, Op::Add, Op::StoreLocal];

  (0..code.len())
    .map(|ip| {
      if matches_ops(code, ip, CONSTANT_CALL_LOOP) || matches_ops(code, ip, IDENTITY_CALL_LOOP) {
        QuickenedKind::NumericCallLoop
      } else if matches_ops(code, ip, ARRAY_PUSH_LOOP) {
        QuickenedKind::DenseArrayPushLoop
      } else if matches_ops(code, ip, PROPERTY_LOOP) {
        QuickenedKind::NumericPropertyLoop
      } else if matches_ops(code, ip, LOOP) {
        QuickenedKind::NumericLoop
      } else if is_numeric_compare(code, ip, COMPARE) {
        QuickenedKind::NumericCompareBranch
      } else if is_single_argument_method_call(code, ip, METHOD_CALL) {
        QuickenedKind::SingleArgumentMethodCall
      } else if is_numeric_property_add(code, ip, PROPERTY_ADD) {
        QuickenedKind::NumericPropertyAdd
      } else if is_numeric_increment(code, ip, INCREMENT) {
        QuickenedKind::NumericIncrement
      } else if is_numeric_add_local(code, ip, ADD_LOCAL) {
        QuickenedKind::NumericAddLocal
      } else {
        QuickenedKind::Ordinary
      }
    })
    .collect()
}

/// Grow a locals vector so that `slot` is a valid index.
#[inline(always)]
pub(super) fn ensure_slot(
  locals: &mut Vec<SharedValue>,
  slot: usize,
) {
  if slot >= locals.len() {
    locals.resize_with(slot + 1, || Rc::new(RefCell::new(Value::Undefined)));
  }
}

#[cfg(test)]
mod tests;
