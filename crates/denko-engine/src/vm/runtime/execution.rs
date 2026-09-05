#![allow(trivial_numeric_casts)]
// SPDX-License-Identifier: MPL-2.0

mod baseline;
mod bindings;
mod patterns;
mod regions;
mod sequences;

use crate::binding::BindingCell;
use crate::bytecode::{
  Chunk, Opcode, VerifiedChunk, abi, decode_aux8, decode_aux16, decode_compare_jump,
  decode_conditional_branch_offset, decode_rd, decode_rs,
};
use crate::ecmascript::semantics::properties::own_property_descriptor;
use crate::environment::EnvironmentRecord;
use crate::errors::RuntimeError;
use crate::gc;
use crate::runtime_utils::{array_from_values, number_exponentiate, usize_to_f64};
#[cfg(test)]
use crate::value::PromiseState;
use crate::value::{ObjectData, PropertyDescriptor, PropertyKey, Value};
use crate::vm::runtime::property::{set_property, values_equal};
use crate::vm::state::*;
use js_string::JsStringBuilder;
use num_traits::{Signed, ToPrimitive, Zero};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

pub(super) enum YieldStarStep {
  Await(Value),
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
    self.run_root(Arc::new(chunk))
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
    let result = self.run_root(Arc::new(chunk))?;
    if self.current_module_specifier().is_none() {
      self.persist_root_bindings();
    }
    Ok(result)
  }

  /// Execute bytecode that has already passed structural verification.
  pub fn run_verified(
    &mut self,
    chunk: &VerifiedChunk,
  ) -> Result<Value, RuntimeError> {
    self.run_root(chunk.chunk())
  }

  fn run_root(
    &mut self,
    chunk_rc: Arc<Chunk>,
  ) -> Result<Value, RuntimeError> {
    self.prepare_root(chunk_rc);
    let result = self.run_loop(true);
    if result.is_ok() && self.current_module_specifier().is_none() {
      self.persist_root_bindings();
    }
    result
  }

  pub(super) fn prepare_root(
    &mut self,
    chunk_rc: Arc<Chunk>,
  ) {
    self.execution.root_chunk = Some(Arc::clone(&chunk_rc));
    self.execution.active.chunk = chunk_rc;
    self.execution.active.pc = 0;
    self.execution.active.locals.clear();
    self.execution.call_stack.clear();
    self.execution.active.strict = self.execution.active.chunk.root_strict;
    self.exceptions.handlers.clear();
    self.exceptions.pending = None;
    self.exceptions.pending_frame_count = None;
    self.exceptions.unwound_register_state = None;
    self.caches.module_reads.clear();
    self.caches.active_module_reads = None;
    self.caches.property_get.clear();
    self.caches.active_property_get = None;
    self.execution.active.slots = None;
    self.execution.direct_eval_outer = None;
    self.execution.active.closure_env = None;
    self.execution.pending_closure_env = None;
    self.memory.temporary.clear();
    self.memory.kept_alive.clear();
    self.execution.active.lexical_env = Rc::clone(&self.realm_state.global_env);
    self.execution.active.variable_env = Rc::clone(&self.realm_state.global_env);
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
    self.initialize_root_bindings(true);
    if self.current_module_specifier().is_none() {
      self.persist_root_bindings();
    }
    gc::reset_allocation_pressure();
    self.begin_opcode_profile_run();
  }

  pub(super) fn run_loop(
    &mut self,
    _root_execution: bool,
  ) -> Result<Value, RuntimeError> {
    let function_id = self.active_function_id();
    let profile = &mut self.profiling.functions[function_id.index()];
    let was_cold = profile.baseline == crate::vm::jit::BaselineState::Cold;
    profile.record_call();
    if was_cold && profile.baseline == crate::vm::jit::BaselineState::Requested {
      self
        .profiling
        .pending_baseline
        .push_back(function_id);
    }
    let mut state = RegisterState::new(
      self.execution.active.this_value.clone(),
      &[],
      self.execution.active.chunk.spill_slot_count as usize,
    );
    match self.run_state(&mut state, false)? {
      RunOutcome::Return(value) => Ok(value),
      RunOutcome::Yield(_) | RunOutcome::YieldStar(_) => {
        Err(RuntimeError::from("yield outside generator execution"))
      }
      RunOutcome::Await(_) => Err(RuntimeError::from("await outside async execution")),
      RunOutcome::Initialized => Err(RuntimeError::from(
        "generator boundary outside generator execution",
      )),
    }
  }

  pub(crate) fn discard_suspended_activation(
    &mut self,
    state: &mut RegisterState,
  ) {
    while let Some(frame) = self.execution.call_stack.pop() {
      self.execution.active = frame.context;
      self.leave_function_realm(frame.previous_realm);
    }
    self.execution.active.locals.clear();
    self.execution.active.closure_env = None;
    self.execution.active.slots = None;
    self.execution.active.yield_star = None;
    self.execution.active.lexical_env = Rc::clone(&self.realm_state.global_env);
    self.exceptions.handlers.clear();
    self.exceptions.pending = None;
    self.exceptions.pending_frame_count = None;
    self.exceptions.unwound_register_state = None;
    state.clear();
  }

  fn restore_pending_exception(
    &mut self,
    registers: &mut [Value; 16],
    spill_slots: &mut Vec<Value>,
    resume_destination: &mut Option<usize>,
    boundary_frame_count: usize,
  ) -> bool {
    if self.exceptions.pending.is_none()
      || !self
        .exceptions
        .pending_frame_count
        .is_some_and(|target| target >= boundary_frame_count)
    {
      return false;
    }
    if let Some(caller_state) = self.exceptions.unwound_register_state.take() {
      *registers = caller_state.registers;
      *spill_slots = caller_state.spill_slots;
      *resume_destination = caller_state.resume_destination;
    }
    self.exceptions.pending_frame_count = None;
    registers[abi::EXCEPTION.index()] = self
      .exceptions
      .pending
      .take()
      .unwrap_or(Value::Undefined);
    true
  }

  fn finish_return(
    &mut self,
    registers: &mut [Value; 16],
    spill_slots: &mut Vec<Value>,
    resume_destination: &mut Option<usize>,
    result: Value,
  ) -> Result<Option<RunOutcome>, RuntimeError> {
    if let Some(caller_state) = self
      .execution
      .call_stack
      .last_mut()
      .and_then(|frame| frame.register_state.take())
    {
      self.execute_return(result.clone())?;
      *registers = caller_state.registers;
      registers[0] = result;
      *spill_slots = caller_state.spill_slots;
      *resume_destination = caller_state.resume_destination;
      return Ok(None);
    }
    if self.execution.call_stack.is_empty()
      && self.execution.interpreter_depth == 1
      && !self.execution.defer_job_checkpoint
    {
      let temp_root_base = self.memory.temporary.len();
      self.memory.temporary.push(result.clone());
      self.memory.kept_alive.clear();
      self.run_microtasks_if_needed()?;
      self.maybe_collect_garbage_prototype();
      self.memory.temporary.truncate(temp_root_base);
      self.memory.kept_alive.clear();
      self.finish_opcode_profile_run("return");
    }
    Ok(Some(RunOutcome::Return(result)))
  }

  pub(super) fn run_state(
    &mut self,
    state: &mut RegisterState,
    stop_at_generator_init: bool,
  ) -> Result<RunOutcome, RuntimeError> {
    self.execution.interpreter_depth += 1;
    let result = (|| {
      if !stop_at_generator_init {
        self
          .try_run_baseline(state)?
          .map_or_else(|| self.run_state_inner(state, stop_at_generator_init), Ok)
      } else {
        self.run_state_inner(state, stop_at_generator_init)
      }
    })();
    self.process_baseline_requests();
    self.execution.interpreter_depth -= 1;
    result
  }

  fn run_state_inner(
    &mut self,
    state: &mut RegisterState,
    stop_at_generator_init: bool,
  ) -> Result<RunOutcome, RuntimeError> {
    const GC_POLL_INTERVAL: u8 = 64;

    let boundary_frame_count = self.execution.call_stack.len();
    let registers = &mut state.registers;
    let spill_slots = &mut state.spill_slots;
    let mut gc_poll_countdown = GC_POLL_INTERVAL;

    if self
      .exceptions
      .pending_frame_count
      .is_some_and(|target| target >= boundary_frame_count)
      && let Some(exception) = self.exceptions.pending.take()
    {
      self.exceptions.pending_frame_count = None;
      registers[abi::EXCEPTION.index()] = exception;
    }

    if let Err(error) = self.poll_execution_interrupt()
      && !self.restore_pending_exception(
        registers,
        spill_slots,
        &mut state.resume_destination,
        boundary_frame_count,
      )
    {
      return Err(error);
    }

    loop {
      macro_rules! boundary_try {
        ($operation:expr) => {
          match $operation {
            Ok(value) => value,
            Err(error) => {
              if self.restore_pending_exception(
                registers,
                spill_slots,
                &mut state.resume_destination,
                boundary_frame_count,
              ) {
                continue;
              }
              return Err(error);
            }
          }
        };
      }

      if let (Some(operation), Some(settlement)) = (
        state.pending_operation.take(),
        state.await_settlement.take(),
      ) {
        match boundary_try!(self.resume_pending_iterator_operation(operation, settlement)) {
          PendingOperationStep::Complete { destination, value } => {
            registers[destination] = value;
          }
          PendingOperationStep::Await { value, operation } => {
            state.pending_operation = Some(operation);
            return Ok(RunOutcome::Await(value));
          }
          PendingOperationStep::Throw(reason) => {
            let error = match self.throw_value(reason) {
              Ok(()) => RuntimeError::from("JS exception"),
              Err(error) => error,
            };
            if self.restore_pending_exception(
              registers,
              spill_slots,
              &mut state.resume_destination,
              boundary_frame_count,
            ) {
              continue;
            }
            return Err(error);
          }
        }
      }
      let action = if crate::trace::enabled() || !self.caches.optimized_execution_enabled {
        SiteAction::Interpret
      } else {
        self.site_action()
      };
      if let SiteAction::Region(region) = action
        && boundary_try!(self.try_native_region(region, registers))
      {
        self.record_jit_backedge(self.execution.active.pc.saturating_sub(1));
        let cost = self
          .caches
          .active_regions
          .get(region.index())
          .map_or(0, |region| region.cost().max_iterations);
        gc_poll_countdown = gc_poll_countdown.saturating_sub(cost);
        if gc_poll_countdown == 0 {
          gc_poll_countdown = GC_POLL_INTERVAL;
          boundary_try!(self.poll_execution_interrupt());
        }
        continue;
      }
      let quickened = match action {
        SiteAction::Legacy(kind) => kind,
        SiteAction::Interpret | SiteAction::Region(_) => QuickenedKind::Ordinary,
      };
      if quickened == QuickenedKind::NumericCompareBranch
        && boundary_try!(self.try_numeric_compare_branch_sequence(registers))
      {
        if self.profiling.enabled {
          self.profiling.opcode_counts[Opcode::LoadLocal as usize] =
            self.profiling.opcode_counts[Opcode::LoadLocal as usize].saturating_add(1);
          self.profiling.opcode_counts[Opcode::Const as usize] =
            self.profiling.opcode_counts[Opcode::Const as usize].saturating_add(1);
          self.profiling.opcode_counts[Opcode::CompareJump as usize] =
            self.profiling.opcode_counts[Opcode::CompareJump as usize].saturating_add(1);
        }
        gc_poll_countdown = gc_poll_countdown.saturating_sub(3);
        if gc_poll_countdown == 0 {
          gc_poll_countdown = GC_POLL_INTERVAL;
          boundary_try!(self.poll_execution_interrupt());
        }
        continue;
      }
      if quickened == QuickenedKind::SingleArgumentMethodCall
        && boundary_try!(self.try_single_argument_method_call_sequence(registers))
      {
        if self.profiling.enabled {
          for (op, count) in [
            (Opcode::LoadLocal, 4),
            (Opcode::StoreLocal, 1),
            (Opcode::Const, 1),
            (Opcode::GetProperty, 1),
            (Opcode::Move, 4),
            (Opcode::CallMethod, 1),
          ] {
            self.profiling.opcode_counts[op as usize] =
              self.profiling.opcode_counts[op as usize].saturating_add(count);
          }
        }
        gc_poll_countdown = gc_poll_countdown.saturating_sub(12);
        if gc_poll_countdown == 0 {
          gc_poll_countdown = GC_POLL_INTERVAL;
          boundary_try!(self.poll_execution_interrupt());
        }
        continue;
      }
      if quickened == QuickenedKind::NumericPropertyAdd
        && boundary_try!(self.try_numeric_property_add_sequence(registers))
      {
        if self.profiling.enabled {
          for (op, count) in [
            (Opcode::LoadLocal, 4),
            (Opcode::Const, 1),
            (Opcode::GetProperty, 1),
            (Opcode::Add, 1),
            (Opcode::StoreLocal, 2),
          ] {
            self.profiling.opcode_counts[op as usize] =
              self.profiling.opcode_counts[op as usize].saturating_add(count);
          }
        }
        gc_poll_countdown = gc_poll_countdown.saturating_sub(9);
        if gc_poll_countdown == 0 {
          gc_poll_countdown = GC_POLL_INTERVAL;
          boundary_try!(self.poll_execution_interrupt());
        }
        continue;
      }
      if quickened == QuickenedKind::NumericIncrement
        && boundary_try!(self.try_numeric_increment_sequence(registers))
      {
        if self.profiling.enabled {
          for (op, count) in [
            (Opcode::LoadLocal, 3),
            (Opcode::Neg, 2),
            (Opcode::Const, 1),
            (Opcode::Move, 1),
            (Opcode::Add, 1),
            (Opcode::StoreLocal, 2),
          ] {
            self.profiling.opcode_counts[op as usize] =
              self.profiling.opcode_counts[op as usize].saturating_add(count);
          }
        }
        gc_poll_countdown = gc_poll_countdown.saturating_sub(10);
        if gc_poll_countdown == 0 {
          gc_poll_countdown = GC_POLL_INTERVAL;
          boundary_try!(self.poll_execution_interrupt());
        }
        continue;
      }
      if quickened == QuickenedKind::NumericAddLocal
        && boundary_try!(self.try_numeric_add_local_sequence(registers))
      {
        if self.profiling.enabled {
          self.profiling.opcode_counts[Opcode::LoadLocal as usize] =
            self.profiling.opcode_counts[Opcode::LoadLocal as usize].saturating_add(2);
          self.profiling.opcode_counts[Opcode::Add as usize] =
            self.profiling.opcode_counts[Opcode::Add as usize].saturating_add(1);
          self.profiling.opcode_counts[Opcode::StoreLocal as usize] =
            self.profiling.opcode_counts[Opcode::StoreLocal as usize].saturating_add(1);
        }
        gc_poll_countdown = gc_poll_countdown.saturating_sub(4);
        if gc_poll_countdown == 0 {
          gc_poll_countdown = GC_POLL_INTERVAL;
          boundary_try!(self.poll_execution_interrupt());
        }
        continue;
      }

      let word = *self
        .execution
        .active
        .chunk
        .words
        .get(self.execution.active.pc)
        .ok_or_else(|| RuntimeError::from("instruction pointer out of bounds"))?;
      let instruction_pc = self.execution.active.pc;
      self.execution.active.pc += 1;
      // SAFETY: every public execution entry verifies the chunk recursively
      // before this loop runs, including nested function chunks.
      let op = unsafe { Opcode::from_byte_unchecked((word & 0xFF) as u8) };
      let rd = decode_rd(word) as usize;
      let rs = decode_rs(word) as usize;
      if self.profiling.enabled {
        self.profiling.opcode_counts[op as usize] =
          self.profiling.opcode_counts[op as usize].saturating_add(1);
      }
      if crate::trace::enabled() {
        eprintln!(
          "[trace] bytecode {}",
          self
            .execution
            .active
            .chunk
            .disassemble_at(instruction_pc)
            .unwrap_or_else(|_| format!("pc={instruction_pc}"))
        );
      }

      let mut switched_context = false;
      let step_result = (|| -> Result<Option<RunOutcome>, RuntimeError> {
        match op {
          Opcode::Const => {
            registers[rd] = self
              .execution
              .active
              .chunk
              .constants
              .get(decode_aux16(word) as usize)
              .map(crate::bytecode::Constant::to_value)
              .ok_or_else(|| RuntimeError::from("constant index out of bounds"))?;
          }
          Opcode::Null => registers[rd] = Value::Null,
          Opcode::True => registers[rd] = Value::Bool(true),
          Opcode::False => registers[rd] = Value::Bool(false),
          Opcode::Undefined => registers[rd] = Value::Undefined,
          Opcode::Move => registers[rd] = registers[rs].clone(),
          Opcode::LoadSpill => {
            registers[rd] = spill_slots
              .get(decode_aux16(word) as usize)
              .cloned()
              .ok_or_else(|| RuntimeError::from("spill load out of bounds"))?;
          }
          Opcode::StoreSpill => {
            let spill = spill_slots
              .get_mut(decode_aux16(word) as usize)
              .ok_or_else(|| RuntimeError::from("spill store out of bounds"))?;
            *spill = registers[rd].clone();
          }
          Opcode::MakeClosure => {
            registers[rd] = self.make_closure_value(decode_aux16(word) as usize)?;
          }
          Opcode::LoadLocal => {
            let slot = decode_aux16(word) as usize;
            let value = self
              .execution
              .active
              .locals
              .get(slot)
              .map(BindingCell::value)
              .ok_or_else(|| RuntimeError::from("bad bytecode local slot"))?;
            if matches!(value, Value::Uninitialized) {
              return Err(
                self.throw_reference_error("Cannot access lexical binding before initialization"),
              );
            }
            registers[rd] = value;
          }
          Opcode::StoreLocal => {
            let slot = decode_aux16(word) as usize;
            ensure_slot(&mut self.execution.active.locals, slot);
            let was_uninitialized = self.execution.active.locals[slot].is_uninitialized();
            let immutable = self
              .execution
              .active
              .slots
              .as_ref()
              .and_then(|slots| slots.immutable.get(slot))
              .copied()
              .unwrap_or(false);
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
            let initializes = immutable && was_uninitialized;
            if wrote_with_environment {
            } else if immutable && !initializes {
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
              self.execution.active.locals[slot].set(registers[rd].clone());
              if was_uninitialized {
                let cell = self.execution.active.locals[slot].clone();
                let _ = self
                  .execution
                  .active
                  .lexical_env
                  .borrow_mut()
                  .set_binding_initialized_for_cell_in_chain(&cell, true);
              }
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
          Opcode::CloneLocalCell => {
            let slot = decode_aux16(word) as usize;
            ensure_slot(&mut self.execution.active.locals, slot);
            let value = self.execution.active.locals[slot].value();
            self.execution.active.locals[slot] = BindingCell::new(value);
          }
          Opcode::ResetLocalCell => {
            let slot = decode_aux16(word) as usize;
            ensure_slot(&mut self.execution.active.locals, slot);
            self.execution.active.locals[slot] = BindingCell::new(Value::Uninitialized);
          }
          Opcode::GetUpvalue => {
            let index = decode_aux16(word) as usize;
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
                .map(BindingCell::value)
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
          Opcode::SetUpvalue => {
            let index = decode_aux16(word) as usize;
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
            if cell.is_uninitialized() {
              return Err(
                self.throw_reference_error("Cannot access lexical binding before initialization"),
              );
            }
            if immutable {
              if self.is_strict_mode() || !sloppy_immutable_silent {
                return Err(self.throw_type_error("Assignment to constant variable"));
              }
            } else {
              cell.set(registers[rd].clone());
              if let Some(name) = mirror_global_name {
                let current = cell.value();
                self.sync_named_binding_to_global_and_frames(&name, &current);
              }
            }
          }
          Opcode::GetGlobal | Opcode::TypeofGlobal => {
            let name = self.string_constant(decode_aux16(word) as usize)?;
            let value = self.resolve_identifier_value(name.as_str_lossy())?;
            registers[rd] = if matches!(op, Opcode::TypeofGlobal) {
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
          Opcode::SetGlobal => {
            let name = self.string_constant(decode_aux16(word) as usize)?;
            let value = registers[rd].clone();
            self.set_global_value(name.as_str_lossy(), value)?;
          }
          Opcode::GetBuiltin => {
            let name = self.string_constant(decode_aux16(word) as usize)?;
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
          Opcode::GetModuleExport => {
            let value = self.read_module_export_cached(decode_aux16(word) as usize)?;
            registers[rd] = value;
          }
          Opcode::SetModuleExport => {
            let (module_index, name) = self.module_export_descriptor(decode_aux16(word) as usize)?;
            while self.modules.table.len() <= module_index {
              self.modules.table.push(HashMap::new());
            }
            if let Some(slot) = self.modules.table[module_index].get(&name) {
              slot.set(registers[rd].clone());
            } else {
              self.modules.table[module_index]
                .insert(name, BindingCell::new(registers[rd].clone()));
            }
          }
          Opcode::GetThis => registers[rd] = self.get_this(),
          Opcode::GetNewTarget => registers[rd] = self.get_new_target(),
          Opcode::Neg => {
            registers[rd] = match registers[rs].clone() {
              Value::BigInt(value) => Value::BigInt(-value),
              value => Value::Number(-self.to_number_value(&value)?),
            };
          }
          Opcode::Not => registers[rd] = Value::Bool(!registers[rs].is_truthy()),
          Opcode::BitNot => {
            registers[rd] = match registers[rs].clone() {
              Value::BigInt(value) => Value::BigInt(!value),
              value => Value::Number((!self.to_int32_value(&value)?) as f64),
            };
          }
          Opcode::Typeof => {
            registers[rd] = Value::String(registers[rs].type_name().to_string().into())
          }
          Opcode::Increment | Opcode::Decrement => {
            let value = self.to_primitive(&registers[rd], Some("number"))?;
            registers[rd] = match value {
              Value::BigInt(value) => Value::BigInt(if matches!(op, Opcode::Increment) {
                value + 1
              } else {
                value - 1
              }),
              value => Value::Number(if matches!(op, Opcode::Increment) {
                self.to_number_value(&value)? + 1.0
              } else {
                self.to_number_value(&value)? - 1.0
              }),
            };
          }
          Opcode::Add => {
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
          Opcode::Sub | Opcode::Mul | Opcode::Div | Opcode::Mod | Opcode::Pow => {
            let left = registers[rd].clone();
            let right = registers[rs].clone();
            registers[rd] = match (left, right) {
              (Value::BigInt(left), Value::BigInt(right)) => match op {
                Opcode::Sub => Value::BigInt(left - right),
                Opcode::Mul => Value::BigInt(left * right),
                Opcode::Div if right.is_zero() => {
                  return Err(self.throw_range_error("Division by zero"));
                }
                Opcode::Div => Value::BigInt(left / right),
                Opcode::Mod if right.is_zero() => {
                  return Err(self.throw_range_error("Division by zero"));
                }
                Opcode::Mod => Value::BigInt(left % right),
                Opcode::Pow => {
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
                  Opcode::Sub => left - right,
                  Opcode::Mul => left * right,
                  Opcode::Div => left / right,
                  Opcode::Mod => left % right,
                  Opcode::Pow => number_exponentiate(left, right),
                  _ => unreachable!(),
                })
              }
            };
          }
          Opcode::BitAnd
          | Opcode::BitOr
          | Opcode::BitXor
          | Opcode::Shl
          | Opcode::Shr
          | Opcode::Ushr => {
            let left = self.to_numeric_value(&registers[rd])?;
            let right = self.to_numeric_value(&registers[rs])?;
            registers[rd] = match (&left, &right) {
              (Value::BigInt(left), Value::BigInt(right)) => match op {
                Opcode::BitAnd => Value::BigInt(left & right),
                Opcode::BitOr => Value::BigInt(left | right),
                Opcode::BitXor => Value::BigInt(left ^ right),
                Opcode::Shl | Opcode::Shr => {
                  let reverse = right.is_negative();
                  let shift = right
                    .abs()
                    .to_usize()
                    .ok_or_else(|| self.throw_range_error("BigInt shift count is too large"))?;
                  let shift_left = matches!(op, Opcode::Shl) != reverse;
                  Value::BigInt(if shift_left {
                    left << shift
                  } else {
                    left >> shift
                  })
                }
                Opcode::Ushr => {
                  return Err(self.throw_type_error("BigInts have no unsigned right shift"));
                }
                _ => unreachable!(),
              },
              (Value::BigInt(_), _) | (_, Value::BigInt(_)) => {
                return Err(self.throw_type_error("Cannot mix BigInt and other types"));
              }
              _ => match op {
                Opcode::BitAnd => {
                  Value::Number((self.to_int32_value(&left)? & self.to_int32_value(&right)?) as f64)
                }
                Opcode::BitOr => {
                  Value::Number((self.to_int32_value(&left)? | self.to_int32_value(&right)?) as f64)
                }
                Opcode::BitXor => {
                  Value::Number((self.to_int32_value(&left)? ^ self.to_int32_value(&right)?) as f64)
                }
                Opcode::Shl => Value::Number(
                  self
                    .to_int32_value(&left)?
                    .wrapping_shl(self.to_uint32_value(&right)? & 0x1f) as f64,
                ),
                Opcode::Shr => Value::Number(
                  self
                    .to_int32_value(&left)?
                    .wrapping_shr(self.to_uint32_value(&right)? & 0x1f) as f64,
                ),
                Opcode::Ushr => Value::Number(
                  self
                    .to_uint32_value(&left)?
                    .wrapping_shr(self.to_uint32_value(&right)? & 0x1f) as f64,
                ),
                _ => unreachable!(),
              },
            };
          }
          Opcode::And => {
            registers[rd] = Value::Bool(registers[rd].is_truthy() && registers[rs].is_truthy());
          }
          Opcode::Or => {
            registers[rd] = Value::Bool(registers[rd].is_truthy() || registers[rs].is_truthy());
          }
          Opcode::Jump => {
            let offset = ((word >> 8) & 0x00FF_FFFF) as i32;
            let offset = if offset & 0x0080_0000 != 0 {
              offset | !0x00FF_FFFF
            } else {
              offset
            };
            if offset < 0 {
              self.record_jit_backedge(instruction_pc);
            }
            self.execution.active.pc =
              usize::try_from(self.execution.active.pc as i64 + offset as i64)
                .map_err(|_| RuntimeError::from("negative bytecode jump target"))?;
          }
          Opcode::JumpIfFalse | Opcode::JumpIfTrue | Opcode::JumpIfNullish => {
            let value = &registers[rd];
            let take = match op {
              Opcode::JumpIfFalse => !value.is_truthy(),
              Opcode::JumpIfTrue => value.is_truthy(),
              Opcode::JumpIfNullish => matches!(value, Value::Null | Value::Undefined),
              _ => unreachable!(),
            };
            let offset = decode_conditional_branch_offset(word);
            if take && offset < 0 {
              self.record_jit_backedge(instruction_pc);
            }
            if take {
              self.execution.active.pc =
                usize::try_from(self.execution.active.pc as i64 + offset as i64)
                  .map_err(|_| RuntimeError::from("negative conditional target"))?;
            }
          }
          Opcode::CompareJump => {
            let (condition, left, right, offset) = decode_compare_jump(word);
            let left = &registers[left as usize];
            let right = &registers[right as usize];
            let result = match condition {
              0 => self.abstract_equality(left, right)?,
              1 => !self.abstract_equality(left, right)?,
              2 => self
                .abstract_relational_comparison(left, right, true)?
                .unwrap_or(false),
              3 => self
                .abstract_relational_comparison(right, left, false)?
                .is_some_and(|result| !result),
              4 => self
                .abstract_relational_comparison(right, left, false)?
                .unwrap_or(false),
              5 => self
                .abstract_relational_comparison(left, right, true)?
                .is_some_and(|result| !result),
              6 => values_equal(left, right),
              7 => !values_equal(left, right),
              _ => unreachable!(),
            };
            if !result {
              if offset < 0 {
                self.record_jit_backedge(instruction_pc);
              }
              self.execution.active.pc =
                usize::try_from(self.execution.active.pc as i64 + offset as i64)
                  .map_err(|_| RuntimeError::from("negative compare target"))?;
            }
          }
          Opcode::GetProperty => {
            let object = registers[rd].clone();
            let key = registers[rs].clone();
            let key = self.to_property_key(&key)?;
            if matches!(&key, PropertyKey::Symbol(symbol) if symbol.is_private())
              && (matches!(object, Value::Proxy(_))
                || own_property_descriptor(&object, &key).is_none())
            {
              return Err(self.throw_type_error("Cannot access private member on this object"));
            }
            registers[rd] = self.get_property_value_cached(instruction_pc, &object, &key)?;
          }
          Opcode::SetProperty => {
            let receiver = registers[rd].clone();
            let key = self.to_property_key(&registers[rs])?;
            let value = registers[(decode_aux8(word) & 0xF) as usize].clone();
            if matches!(&key, PropertyKey::Symbol(symbol) if symbol.is_private())
              && (matches!(receiver, Value::Proxy(_))
                || own_property_descriptor(&receiver, &key).is_none())
            {
              return Err(self.throw_type_error("Cannot access private member on this object"));
            }
            if receiver.is_object() {
              self.set_property_with_receiver(&receiver, &key, value.clone(), &receiver)?;
            } else {
              let object = self.to_object_value(receiver.clone())?;
              self.set_primitive_property(&object, &key, value.clone(), &receiver)?;
            }
            registers[rd] = value;
          }
          Opcode::DeleteProperty => {
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
          Opcode::HasProperty => {
            let object = registers[rd].clone();
            if !object.is_object() {
              return Err(self.throw_type_error("Right-hand side of 'in' is not an object"));
            }
            let key = self.to_property_key(&registers[rs])?;
            registers[rd] = Value::Bool(self.has_property_host(&object, &key)?);
          }
          Opcode::InstanceOf => {
            let value = registers[rd].clone();
            let constructor = registers[rs].clone();
            registers[rd] = Value::Bool(self.instance_of(&value, &constructor)?);
          }
          Opcode::GetSuperProperty => {
            let receiver = registers[rd].clone();
            let key = registers[rs].clone();
            registers[rd] = self.get_super_property(&receiver, &key)?;
          }
          Opcode::SetSuperProperty => {
            let value_register = (decode_aux8(word) & 0xF) as usize;
            let receiver = registers[rd].clone();
            let key = registers[rs].clone();
            let value = registers[value_register].clone();
            registers[rd] = self.set_super_property(&receiver, &key, value)?;
          }
          Opcode::BuildArray => {
            let count = decode_aux16(word) as usize;
            let base = self.execution.active.chunk.value_spill_slot_count as usize;
            let values = spill_slots
              .get(base..base + count)
              .ok_or_else(|| RuntimeError::from("array operand window out of bounds"))?
              .to_vec();
            registers[rd] = self.build_array_value(values)?;
          }
          Opcode::BuildObject => {
            let count = decode_aux16(word) as usize;
            let operand_count = count.saturating_mul(2);
            let base = self.execution.active.chunk.value_spill_slot_count as usize;
            let values = spill_slots
              .get(base..base + operand_count)
              .ok_or_else(|| RuntimeError::from("object operand window out of bounds"))?
              .to_vec();
            registers[rd] = self.build_object_value(values)?;
          }
          Opcode::ArrayFromSpread => {
            let values = registers[rd].clone();
            let mask = registers[rs].clone();
            let values = self.flatten_spread_values(values, mask)?;
            registers[rd] = self.build_array_value(values)?;
          }
          Opcode::Spread => {
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
          Opcode::Call | Opcode::TailCall | Opcode::CallMethod => {
            let count = decode_aux16(word) as usize;
            let fast_result = if op != Opcode::TailCall
              && (2..=abi::REGISTER_ARGUMENT_COUNT).contains(&count)
              && matches!(registers[abi::FIRST_ARGUMENT.index()], Value::Number(_))
              && matches!(registers[abi::FIRST_ARGUMENT.index() + 1], Value::Number(_))
            {
              self.try_cached_numeric_call(
                instruction_pc,
                &registers[abi::V2_CALL_OPERAND.index()],
                &registers[abi::FIRST_ARGUMENT.index()..abi::FIRST_ARGUMENT.index() + count],
              )?
            } else {
              None
            };
            if let Some(result) = fast_result {
              registers[abi::RETURN.index()] = result;
              return Ok(None);
            }
            let mut args: Vec<Value> = (0..count.min(abi::REGISTER_ARGUMENT_COUNT))
              .map(|index| {
                let register = abi::argument_register(index)
                  .expect("bounded call argument must have an ABI register");
                registers[register.index()].clone()
              })
              .collect();
            let overflow = count.saturating_sub(abi::REGISTER_ARGUMENT_COUNT);
            let base = self.execution.active.chunk.value_spill_slot_count as usize;
            if overflow > 0 {
              args.extend_from_slice(
                spill_slots
                  .get(base..base + overflow)
                  .ok_or_else(|| RuntimeError::from("call operand window out of bounds"))?,
              );
            }
            let callee = registers[abi::V2_CALL_OPERAND.index()].clone();
            if op != Opcode::TailCall
              && let Value::Closure { proto, env, .. } = &callee
              && !proto.is_generator
              && !proto.is_async
            {
              if proto.is_class_constructor && self.construction.new_target.is_none() {
                return Err(self.throw_class_constructor_call_error_for(&callee, proto));
              }
              let call_args = self.prepare_call_args(args, proto)?;
              let receiver = if op == Opcode::CallMethod {
                registers[abi::RECEIVER.index()].clone()
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
                self.try_direct_baseline_call(instruction_pc, proto, &this_arg, &call_args)?
              {
                registers[abi::RETURN.index()] = result;
              } else {
                let register_arguments = call_args
                  .iter()
                  .take(crate::bytecode::abi::REGISTER_ARGUMENT_COUNT)
                  .cloned()
                  .collect::<Vec<_>>();
                self.push_call_frame(proto, env, call_args, this_arg, callee.clone())?;
                let callee_this = self.execution.active.this_value.clone();
                let callee_spill_count = self.execution.active.chunk.spill_slot_count as usize;
                let caller_state = RegisterState {
                  registers: std::mem::replace(
                    registers,
                    std::array::from_fn(|_| Value::Undefined),
                  ),
                  spill_slots: std::mem::replace(
                    spill_slots,
                    vec![Value::Undefined; callee_spill_count],
                  ),
                  resume_destination: state.resume_destination,
                  pending_operation: None,
                  await_settlement: None,
                };
                self
                  .execution
                  .call_stack
                  .last_mut()
                  .expect("call frame was just pushed")
                  .register_state = Some(caller_state);
                *registers = RegisterState::call_registers(callee_this, &register_arguments);
                state.resume_destination = None;
                if let Some(result) = self.try_run_nested_baseline(registers, false)? {
                  let outcome = self.finish_return(
                    registers,
                    spill_slots,
                    &mut state.resume_destination,
                    result,
                  )?;
                  switched_context = outcome.is_none();
                  if outcome.is_some() {
                    return Ok(outcome);
                  }
                } else {
                  switched_context = true;
                }
              }
            } else if op != Opcode::TailCall {
              registers[abi::RETURN.index()] = if matches!(op, Opcode::CallMethod) {
                let receiver = registers[abi::RECEIVER.index()].clone();
                self.call_value_with_this(&callee, &receiver, &args)?
              } else {
                self.call_value(&callee, &args)?
              };
            } else if let Value::Closure { proto, env, .. } = &callee
              && !proto.is_generator
              && !proto.is_async
            {
              if proto.is_class_constructor && self.construction.new_target.is_none() {
                return Err(self.throw_class_constructor_call_error_for(&callee, proto));
              }
              let call_args = self.prepare_call_args(args, proto)?;
              let register_arguments = call_args
                .iter()
                .take(crate::bytecode::abi::REGISTER_ARGUMENT_COUNT)
                .cloned()
                .collect::<Vec<_>>();
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
                frame.new_target = None;
                frame.upvalues = proto.upvalues.clone();
              }
              self.configure_call_frame(proto, env, call_args, this_arg, callee.clone())?;
              *registers = RegisterState::call_registers(
                self.execution.active.this_value.clone(),
                &register_arguments,
              );
              spill_slots.clear();
              spill_slots.resize(
                self.execution.active.chunk.spill_slot_count as usize,
                Value::Undefined,
              );
              state.resume_destination = None;
              if let Some(result) = self.try_run_nested_baseline(registers, true)? {
                let outcome = self.finish_return(
                  registers,
                  spill_slots,
                  &mut state.resume_destination,
                  result,
                )?;
                switched_context = outcome.is_none();
                if outcome.is_some() {
                  return Ok(outcome);
                }
              } else {
                switched_context = true;
              }
            } else {
              let result = self.call_value(&callee, &args)?;
              let outcome = self.finish_return(
                registers,
                spill_slots,
                &mut state.resume_destination,
                result,
              )?;
              switched_context = outcome.is_none();
              return Ok(outcome);
            }
          }
          Opcode::Construct => {
            let count = decode_aux16(word) as usize;
            let mut args: Vec<Value> = (0..count.min(abi::REGISTER_ARGUMENT_COUNT))
              .map(|index| {
                let register = abi::argument_register(index)
                  .expect("bounded constructor argument must have an ABI register");
                registers[register.index()].clone()
              })
              .collect();
            let overflow = count.saturating_sub(abi::REGISTER_ARGUMENT_COUNT);
            let base = self.execution.active.chunk.value_spill_slot_count as usize;
            if overflow > 0 {
              args.extend_from_slice(
                spill_slots
                  .get(base..base + overflow)
                  .ok_or_else(|| RuntimeError::from("construct operand window out of bounds"))?,
              );
            }
            let callee = registers[abi::V2_CALL_OPERAND.index()].clone();
            registers[abi::RETURN.index()] =
              self.construct_value_with_new_target(&callee, &args, callee.clone())?;
          }
          Opcode::SuperConstruct => {
            let count = decode_aux16(word) as usize;
            let base = self.execution.active.chunk.value_spill_slot_count as usize;
            let args = spill_slots
              .get(base..base + count)
              .ok_or_else(|| RuntimeError::from("super operand window out of bounds"))?
              .to_vec();
            registers[rd] = self.construct_super_value(args)?;
          }
          Opcode::PushHandler => {
            let offset = ((word >> 8) & 0x00FF_FFFF) as i32;
            let offset = if offset & 0x0080_0000 != 0 {
              offset | !0x00FF_FFFF
            } else {
              offset
            };
            let handler_pc = usize::try_from(self.execution.active.pc as i64 + offset as i64)
              .map_err(|_| RuntimeError::from("negative handler target"))?;
            let finally = self
              .execution
              .active
              .chunk
              .finally_handlers
              .iter()
              .find(|handler| handler.push_pc == instruction_pc)
              .map(|handler| FinallyHandlerRecord {
                finalizer_pc: handler.finalizer_pc,
                return_slot: handler.return_slot,
                did_return_slot: handler.did_return_slot,
              });
            self.exceptions.handlers.push(HandlerRecord {
              chunk: Arc::clone(&self.execution.active.chunk),
              handler_pc,
              saved_locals_len: self.execution.active.locals.len(),
              frame_count: self.execution.call_stack.len(),
              saved_lexical_env: Rc::clone(&self.execution.active.lexical_env),
              finally,
            });
          }
          Opcode::PopHandler => {
            self.exceptions.handlers.pop();
          }
          Opcode::Throw => {
            self.throw_value(registers[rd].clone())?;
            return Err(RuntimeError::from("JS exception"));
          }
          Opcode::DeleteIdentifier => {
            registers[rd] = self.delete_identifier(registers[rd].clone())?;
          }
          Opcode::IteratorNext => {
            let iterator = registers[rd].clone();
            if let Some(step) = self.begin_async_iterator_next(&iterator, rd)? {
              match step {
                PendingOperationStep::Complete { destination, value } => {
                  registers[destination] = value;
                }
                PendingOperationStep::Await { value, operation } => {
                  state.pending_operation = Some(operation);
                  return Ok(Some(RunOutcome::Await(value)));
                }
                PendingOperationStep::Throw(reason) => {
                  self.throw_value(reason)?;
                  return Err(RuntimeError::from("JS exception"));
                }
              }
            } else {
              registers[rd] = self.iterator_next(iterator)?;
            }
          }
          Opcode::IteratorClose => {
            let iterator = registers[rd].clone();
            if let Some(step) = self.begin_async_iterator_close(&iterator, rd)? {
              match step {
                PendingOperationStep::Complete { destination, value } => {
                  registers[destination] = value;
                }
                PendingOperationStep::Await { value, operation } => {
                  state.pending_operation = Some(operation);
                  return Ok(Some(RunOutcome::Await(value)));
                }
                PendingOperationStep::Throw(reason) => {
                  self.throw_value(reason)?;
                  return Err(RuntimeError::from("JS exception"));
                }
              }
            } else {
              registers[rd] = self.iterator_close(iterator)?;
            }
          }
          Opcode::GetAsyncIterator => {
            registers[rd] = self.get_async_iterator(registers[rd].clone())?;
          }
          Opcode::PushWithEnv => {
            let object = self.to_object_value(registers[rd].clone())?;
            self.execution.active.lexical_env =
              Rc::new(RefCell::new(EnvironmentRecord::new_object(
                Some(Rc::clone(&self.execution.active.lexical_env)),
                object,
                true,
              )));
          }
          Opcode::PushLexicalEnv => {
            let name = match &registers[rd] {
              Value::String(name) => name.to_string(),
              _ => return Err(RuntimeError::from("lexical binding name must be a string")),
            };
            let slot = decode_aux16(word) as usize;
            let cell = self
              .execution
              .active
              .locals
              .get(slot)
              .cloned()
              .ok_or_else(|| RuntimeError::from("bad lexical environment local slot"))?;
            let initialized = !cell.is_uninitialized();
            let immutable = self
              .execution
              .active
              .slots
              .as_ref()
              .and_then(|slots| slots.immutable.get(slot))
              .copied()
              .unwrap_or(false);
            let mut environment =
              EnvironmentRecord::new(Some(Rc::clone(&self.execution.active.lexical_env)));
            if immutable {
              environment.create_immutable_binding(name, cell, initialized);
            } else {
              environment.create_mutable_lexical_binding(name, cell, initialized);
            }
            self.execution.active.lexical_env = Rc::new(RefCell::new(environment));
          }
          Opcode::PopEnv => {
            let outer = self.execution.active.lexical_env.borrow().outer();
            self.execution.active.lexical_env =
              outer.unwrap_or_else(|| Rc::clone(&self.realm_state.global_env));
          }
          Opcode::GeneratorInitDone if stop_at_generator_init => {
            return Ok(Some(RunOutcome::Initialized));
          }
          Opcode::GeneratorInitDone => {}
          Opcode::Yield => {
            state.resume_destination = Some(rd);
            return Ok(Some(RunOutcome::Yield(registers[rd].clone())));
          }
          Opcode::YieldStar => {
            if self.execution.active.yield_star.is_none() {
              self.execution.active.yield_star =
                Some(self.create_yield_star_state(registers[rd].clone(), decode_aux8(word) != 0)?);
            }
            match self.resume_yield_star()? {
              YieldStarStep::Await(value) => {
                self.execution.active.pc = self.execution.active.pc.saturating_sub(1);
                state.pending_operation = Some(PendingOperation::YieldStar { destination: rd });
                return Ok(Some(RunOutcome::Await(value)));
              }
              YieldStarStep::Yield(value) => {
                self.execution.active.pc = self.execution.active.pc.saturating_sub(1);
                return Ok(Some(RunOutcome::YieldStar(value)));
              }
              YieldStarStep::Complete(value) => registers[rd] = value,
              YieldStarStep::Return(value) => {
                if !self.begin_generator_return(value.clone()) {
                  return Ok(Some(RunOutcome::Return(value)));
                }
              }
            }
          }
          Opcode::AwaitPromise => {
            state.resume_destination = Some(rd);
            return Ok(Some(RunOutcome::Await(registers[rd].clone())));
          }
          Opcode::Return => {
            let result = registers[rd].clone();
            if self.execution.generator_return_frame_count == Some(self.execution.call_stack.len())
            {
              if self.begin_generator_return(result.clone()) {
                return Ok(None);
              }
              self.execution.generator_return_frame_count = None;
            }
            let outcome = self.finish_return(
              registers,
              spill_slots,
              &mut state.resume_destination,
              result,
            )?;
            switched_context = outcome.is_none();
            return Ok(outcome);
          }
        }
        Ok(None)
      })();
      match step_result {
        Ok(Some(result)) => return Ok(result),
        Ok(None) if switched_context => continue,
        Ok(None) => {}
        Err(_error)
          if self.restore_pending_exception(
            registers,
            spill_slots,
            &mut state.resume_destination,
            boundary_frame_count,
          ) =>
        {
          continue;
        }
        Err(error) => return Err(error),
      }
      gc_poll_countdown -= 1;
      if gc_poll_countdown == 0 {
        gc_poll_countdown = GC_POLL_INTERVAL;
        self.poll_execution_interrupt()?;
        if gc::allocation_pressure() >= gc_allocation_pressure_threshold() {
          let temp_root_base = self.memory.temporary.len();
          self
            .memory
            .temporary
            .extend(registers.iter().cloned());
          self
            .memory
            .temporary
            .extend(spill_slots.iter().cloned());
          self.maybe_collect_garbage_prototype();
          self.memory.temporary.truncate(temp_root_base);
        }
      }
    }
  }

  /// Execute a chunk without resetting the caller's live lexical bindings.
  ///
  /// This is used for direct `eval`, where the evaluated chunk must observe
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
    let saved_gc_temp_roots = self.memory.temporary.clone();
    let saved_recent_return_value = self.construction.recent_return_value.clone();
    let saved_last_uncaught_exception = self.exceptions.last_uncaught.take();
    let saved_pending_exception = self.exceptions.pending.clone();
    let saved_pending_exception_frame_count = self.exceptions.pending_frame_count;
    let saved_handlers = std::mem::take(&mut self.exceptions.handlers);
    let saved_unwound_state = self.exceptions.unwound_register_state.take();
    let saved_direct_eval_outer = self.execution.direct_eval_outer.clone();
    let saved_frame_states = self
      .execution
      .call_stack
      .iter_mut()
      .map(|frame| frame.register_state.take())
      .collect::<Vec<_>>();

    self.memory.temporary.extend(
      saved_context
        .locals
        .iter()
        .map(BindingCell::value),
    );
    self
      .memory
      .temporary
      .push(saved_context.this_value.clone());
    if let Some(arguments) = &saved_context.arguments.object {
      self.memory.temporary.push(arguments.clone());
    }
    if let Some(yield_star) = &saved_context.yield_star {
      self
        .memory
        .temporary
        .push(yield_star.iterator.clone());
      self
        .memory
        .temporary
        .push(yield_star.next_method.clone());
    }
    if let Some(env) = &saved_context.closure_env {
      self
        .memory
        .temporary
        .extend(env.borrow().iter().map(BindingCell::value));
    }
    saved_context
      .lexical_env
      .borrow()
      .collect_binding_values(&mut self.memory.temporary);
    for state in saved_frame_states.iter().flatten() {
      self
        .memory
        .temporary
        .extend(state.registers.iter().cloned());
      self
        .memory
        .temporary
        .extend(state.spill_slots.iter().cloned());
    }

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
    self.execution.active.pc = 0;
    self.execution.active.strict = self.execution.active.chunk.root_strict;
    if !direct_eval {
      self.execution.active.lexical_env = Rc::clone(&self.realm_state.global_env);
      self.execution.active.variable_env = Rc::clone(&self.realm_state.global_env);
    }
    self.execution.active.yield_star = None;
    if !direct_eval {
      self.execution.active.locals.clear();
      self.execution.active.slots = None;
    }
    if !self
      .execution
      .active
      .chunk
      .root_slot_names
      .is_empty()
    {
      self.initialize_root_bindings(false);
    }
    if self.execution.active.strict {
      self.execution.active.variable_env = Rc::clone(&self.execution.active.lexical_env);
    }
    if !self.execution.active.strict {
      let variable_env = Rc::clone(&self.execution.active.variable_env);
      let names = self
        .execution
        .active
        .chunk
        .root_slot_names
        .clone();
      let lexical = self
        .execution
        .active
        .chunk
        .root_lexical_tdz
        .clone();
      for (index, maybe_name) in names.into_iter().enumerate() {
        let Some(name) = maybe_name else {
          continue;
        };
        if lexical.get(index).copied().unwrap_or(false) {
          continue;
        }
        let Some(eval_cell) = self.execution.active.locals.get(index).cloned() else {
          continue;
        };
        let target_cell = variable_env
          .borrow()
          .own_binding_cell(&name)
          .unwrap_or_else(|| eval_cell.clone());
        if !variable_env.borrow().has_binding(&name) {
          variable_env
            .borrow_mut()
            .create_mutable_binding(name.clone(), target_cell.clone(), true);
        }
        if Rc::ptr_eq(&variable_env, &self.realm_state.global_env) {
          self
            .realm_state
            .globals
            .borrow_mut()
            .properties
            .entry(PropertyKey::String(name.clone().into()))
            .or_insert_with(|| PropertyDescriptor::ordinary_data(target_cell.value()));
        }
        self.execution.active.locals[index] = target_cell.clone();
        self
          .execution
          .active
          .lexical_env
          .borrow_mut()
          .create_mutable_binding(name, target_cell, true);
      }
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

    let result = self.run_loop(true);
    let nested_thrown = self.exceptions.last_uncaught.take();

    if !direct_eval && !self.execution.active.strict {
      self.persist_root_var_bindings();
    }

    self.execution.active = saved_context;
    self.execution.root_chunk = saved_root_chunk;
    self.execution.pending_closure_env = saved_pending_closure_env;
    self.memory.temporary = saved_gc_temp_roots;
    self.construction.recent_return_value = saved_recent_return_value;
    self.exceptions.last_uncaught = saved_last_uncaught_exception;
    self.exceptions.pending = saved_pending_exception;
    self.exceptions.pending_frame_count = saved_pending_exception_frame_count;
    self.exceptions.handlers = saved_handlers;
    self.exceptions.unwound_register_state = saved_unwound_state;
    self.execution.direct_eval_outer = saved_direct_eval_outer;
    for (frame, register_state) in self
      .execution
      .call_stack
      .iter_mut()
      .zip(saved_frame_states)
    {
      frame.register_state = register_state;
    }

    result.map_err(|error| NestedRunFailure {
      error,
      thrown: nested_thrown,
    })
  }

  /// Execute a script in the supplied realm without creating a second VM.
  ///
  /// Test262's realm host hook can be invoked while another realm is active.
  /// Keeping the nested execution in this VM preserves the complete root set
  /// for the shared prototype collector.
  pub(crate) fn run_nested_in_realm(
    &mut self,
    chunk: Chunk,
    globals: Rc<RefCell<ObjectData>>,
  ) -> Result<Value, NestedRunFailure> {
    if Rc::ptr_eq(&globals, &self.realm_state.globals) {
      return self.run_nested(chunk, false);
    }

    let Some((intrinsics, global_env)) = crate::runtime_utils::realm_kernel(&globals) else {
      return Err(NestedRunFailure {
        error: RuntimeError::from("Realm kernel is not initialized"),
        thrown: None,
      });
    };
    let temp_root_base = self.memory.temporary.len();
    self
      .memory
      .temporary
      .push(Value::Object(Rc::clone(&self.realm_state.globals)));
    let previous_realm = std::mem::replace(
      &mut self.realm_state,
      RealmState {
        realm: crate::realm::Realm {
          intrinsics: Rc::new(intrinsics),
          global_object: Rc::clone(&globals),
        },
        globals,
        global_env,
      },
    );
    let result = self.run_nested(chunk, false);
    self.realm_state = previous_realm;
    self.memory.temporary.truncate(temp_root_base);
    result
  }

  pub(super) fn make_iterator_result(
    &self,
    value: Value,
    done: bool,
  ) -> Value {
    let obj = gc::track_object(Rc::new(RefCell::new(ObjectData::with_prototype(
      self.realm_state.realm.object_prototype(),
    ))));
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

    self.execution.active = frame.context;
    self.leave_function_realm(frame.previous_realm);
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
  pub(super) fn module_export_descriptor(
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
    let start = self.execution.active.pc.saturating_sub(8);
    let end = usize::min(
      self.execution.active.pc + 8,
      self.execution.active.chunk.words.len(),
    );
    for pc in start..end {
      let instruction = self
        .execution
        .active
        .chunk
        .disassemble_at(pc)
        .unwrap_or_else(|error| format!("{pc:#06} <{error}>"));
      eprintln!("[diag] {instruction}");
    }
  }
}

use patterns::build_quickened_sites;

/// Grow a locals vector so that `slot` is a valid index.
#[inline(always)]
pub(super) fn ensure_slot(
  locals: &mut Vec<BindingCell>,
  slot: usize,
) {
  if slot >= locals.len() {
    locals.resize_with(slot + 1, || BindingCell::new(Value::Undefined));
  }
}

#[cfg(test)]
mod tests;
