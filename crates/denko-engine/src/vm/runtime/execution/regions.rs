// SPDX-License-Identifier: MPL-2.0

use super::*;

impl Vm {
  fn try_dense_array_push_region(
    &mut self,
    region: crate::vm::region::DenseArrayPushRegion,
    registers: &mut [Value; 16],
  ) -> Result<bool, RuntimeError> {
    if self.execution.active.pc != region.entry_pc
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
        .get(region.index_slot)
        .is_some_and(|cell| cell.strong_count() != 1)
      || [
        region.index_slot,
        region.call_temporary_slot,
        region.increment_temporary_slot,
      ]
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
        .and_then(|cell| match cell.value() {
          Value::Number(value) => Some(value),
          _ => None,
        })
    };
    let Some(index) = number(region.index_slot) else {
      return Ok(false);
    };
    if index.partial_cmp(&region.limit) != Some(std::cmp::Ordering::Less) {
      registers[region.index_register] = Value::Number(index);
      registers[region.limit_register] = Value::Number(region.limit);
      self.execution.active.pc = region.completed_pc;
      return Ok(true);
    }
    let receiver = self
      .execution
      .active
      .locals
      .get(region.array_slot)
      .map(BindingCell::value);
    let Some(Value::Array(elements, properties)) = receiver else {
      return Ok(false);
    };
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
    let mut prepared = crate::vm::region::PreparedArrayPushRegion {
      index,
      limit: region.limit,
      step: region.step,
      iterations: 0,
      values: [0.0; 64],
    };
    if !self
      .profiling
      .jit
      .run_dense_array_push_region(&mut prepared)
    {
      return Ok(false);
    }
    if prepared.iterations > 64 {
      return Err(RuntimeError::from(
        "native dense array loop exceeded its batch bound",
      ));
    }
    let values = prepared.values[..prepared.iterations as usize]
      .iter()
      .map(|value| Some(Value::Number(*value)))
      .collect::<Vec<_>>();
    let new_length = elements
      .borrow()
      .len()
      .checked_add(values.len())
      .filter(|length| *length <= u32::MAX as usize);
    let Some(new_length) = new_length else {
      return Ok(false);
    };
    elements.borrow_mut().extend(values);
    if let Some(PropertyDescriptor::Data { value, .. }) = properties
      .borrow_mut()
      .properties
      .get_mut(&PropertyKey::String("length".into()))
    {
      *value = Value::Number(usize_to_f64(new_length));
    }
    let receiver = Value::Array(Rc::clone(&elements), Rc::clone(&properties));
    self.bump_object_generation(&receiver);
    ensure_slot(
      &mut self.execution.active.locals,
      region.increment_temporary_slot,
    );
    self.execution.active.locals[region.increment_temporary_slot]
      .set(Value::Number(prepared.index));
    self.execution.active.locals[region.index_slot].set(Value::Number(prepared.index));
    registers[region.index_register] = Value::Number(prepared.index);
    registers[region.limit_register] = Value::Number(region.limit);
    registers[0] = Value::Number(usize_to_f64(new_length));
    registers[region.step_register] = Value::Number(region.step);
    self.execution.active.pc = if prepared.index < region.limit {
      region.entry_pc
    } else {
      region.completed_pc
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
      .saturating_add(prepared.iterations as u64);
    Ok(true)
  }

  fn try_dense_masked_read_region(
    &mut self,
    region: crate::vm::region::DenseMaskedReadRegion,
    registers: &mut [Value; 16],
  ) -> Result<bool, RuntimeError> {
    if self.execution.active.pc != region.entry_pc
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
        .get(region.index_slot)
        .is_some_and(|cell| cell.strong_count() != 1)
      || [
        region.index_slot,
        region.accumulator_slot,
        region.temporary_slot,
      ]
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
    let number_local = |slot: usize| {
      self
        .execution
        .active
        .locals
        .get(slot)
        .and_then(|cell| match cell.value() {
          Value::Number(value) => Some(value),
          _ => None,
        })
    };
    let Some(index) = number_local(region.index_slot) else {
      return Ok(false);
    };
    let Some(accumulator) = number_local(region.accumulator_slot) else {
      return Ok(false);
    };
    if index.partial_cmp(&region.limit) != Some(std::cmp::Ordering::Less) {
      registers[region.index_register] = Value::Number(index);
      registers[region.limit_register] = Value::Number(region.limit);
      self.execution.active.pc = region.completed_pc;
      return Ok(true);
    }
    const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;
    if !index.is_finite()
      || index < 0.0
      || (index == 0.0 && index.is_sign_negative())
      || index.fract() != 0.0
      || index > MAX_SAFE_INTEGER
      || !region.limit.is_finite()
      || region.limit > MAX_SAFE_INTEGER
    {
      return Ok(false);
    }
    let receiver = self
      .execution
      .active
      .locals
      .get(region.array_slot)
      .map(BindingCell::value);
    let Some(Value::Array(elements, properties)) = receiver else {
      return Ok(false);
    };
    let snapshot = {
      let elements = elements.borrow();
      let Some(values) = elements.get(..4) else {
        return Ok(false);
      };
      let mut snapshot = [0.0; 4];
      for (destination, value) in snapshot.iter_mut().zip(values) {
        let Some(Value::Number(number)) = value else {
          return Ok(false);
        };
        *destination = *number;
      }
      snapshot
    };
    let indexed_key = |key: &PropertyKey| match key {
      PropertyKey::String(key) => key
        .as_str_lossy()
        .parse::<u32>()
        .is_ok_and(|index| index < 4),
      PropertyKey::Symbol(_) => false,
    };
    {
      let properties = properties.borrow();
      if properties.properties.keys().any(&indexed_key) {
        return Ok(false);
      }
    }
    let mut prepared = crate::vm::region::PreparedDenseMaskedReadRegion {
      index,
      limit: region.limit,
      accumulator,
      elements: snapshot,
      iterations: 0,
    };
    if !self
      .profiling
      .jit
      .run_dense_masked_read_region(&mut prepared)
    {
      return Ok(false);
    }
    if prepared.iterations > 64 {
      return Err(RuntimeError::from(
        "native dense masked read loop exceeded its batch bound",
      ));
    }
    ensure_slot(&mut self.execution.active.locals, region.temporary_slot);
    self.execution.active.locals[region.temporary_slot].set(Value::Number(prepared.index));
    self.execution.active.locals[region.accumulator_slot].set(Value::Number(prepared.accumulator));
    self.execution.active.locals[region.index_slot].set(Value::Number(prepared.index));
    registers[region.previous_index_register] = Value::Number(prepared.index - 1.0);
    registers[region.next_index_register] = Value::Number(prepared.index);
    registers[region.step_register] = Value::Number(1.0);
    registers[region.mask_register] = Value::Number(3.0);
    self.execution.active.pc = if prepared.index < region.limit {
      region.entry_pc
    } else {
      region.completed_pc
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
      .saturating_add(prepared.iterations as u64);
    Ok(true)
  }

  fn try_own_method_binary_loop_region(
    &mut self,
    region: crate::vm::region::OwnMethodBinaryLoopRegion,
    registers: &mut [Value; 16],
  ) -> Result<bool, RuntimeError> {
    if self.execution.active.pc != region.entry_pc
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
        .get(region.index_slot)
        .is_some_and(|cell| cell.strong_count() != 1)
      || [
        region.index_slot,
        region.accumulator_slot,
        region.call_temporary_slot,
        region.arithmetic_temporary_slot,
      ]
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
    let number_local = |slot: usize| {
      self
        .execution
        .active
        .locals
        .get(slot)
        .and_then(|cell| match cell.value() {
          Value::Number(value) => Some(value),
          _ => None,
        })
    };
    let Some(index) = number_local(region.index_slot) else {
      return Ok(false);
    };
    let Some(accumulator) = number_local(region.accumulator_slot) else {
      return Ok(false);
    };
    if index.partial_cmp(&region.limit) != Some(std::cmp::Ordering::Less) {
      registers[region.index_register] = Value::Number(index);
      registers[region.limit_register] = Value::Number(region.limit);
      self.execution.active.pc = region.completed_pc;
      return Ok(true);
    }
    const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;
    if !index.is_finite()
      || index < 0.0
      || (index == 0.0 && index.is_sign_negative())
      || index.fract() != 0.0
      || index > MAX_SAFE_INTEGER
      || !region.limit.is_finite()
      || region.limit > MAX_SAFE_INTEGER
    {
      return Ok(false);
    }
    let receiver = self
      .execution
      .active
      .locals
      .get(region.receiver_slot)
      .map(BindingCell::value);
    let Some(Value::Object(object)) = receiver else {
      return Ok(false);
    };
    let receiver = Value::Object(Rc::clone(&object));
    let Some(crate::bytecode::Constant::String(method_key)) = self
      .execution
      .active
      .chunk
      .constants
      .get(region.method_key_constant)
    else {
      return Ok(false);
    };
    let callee = {
      let object = object.borrow();
      if object.module_namespace_exports.is_some() {
        return Ok(false);
      }
      match object
        .properties
        .get(&PropertyKey::String(method_key.clone()))
      {
        Some(PropertyDescriptor::Data { value, .. }) => value.clone(),
        _ => return Ok(false),
      }
    };
    let Value::Closure { proto, .. } = &callee else {
      return Ok(false);
    };
    let Some(parameters) = crate::vm::region::NumericBinaryParameters::decode(proto) else {
      return Ok(false);
    };
    if parameters.left_slot != 0 || parameters.right_slot != 1 {
      return Ok(false);
    }
    let addend = region.left_argument + region.right_argument;
    let mut prepared = crate::vm::region::PreparedNumericRegion {
      index,
      limit: region.limit,
      accumulator,
      step: region.step,
      addend,
      iterations: 0,
      shape: region.program.shape(),
    };
    if !self
      .profiling
      .jit
      .run_numeric_region(&mut prepared)
      .map_err(RuntimeError::from)?
    {
      return Ok(false);
    }
    if prepared.iterations > u32::from(region.program.cost().max_iterations) {
      return Err(RuntimeError::from(
        "native own-method binary loop exceeded its batch bound",
      ));
    }
    ensure_slot(
      &mut self.execution.active.locals,
      region.call_temporary_slot,
    );
    ensure_slot(
      &mut self.execution.active.locals,
      region.arithmetic_temporary_slot,
    );
    self.execution.active.locals[region.call_temporary_slot].set(receiver.clone());
    self.execution.active.locals[region.arithmetic_temporary_slot]
      .set(Value::Number(prepared.index));
    self.execution.active.locals[region.accumulator_slot].set(Value::Number(prepared.accumulator));
    self.execution.active.locals[region.index_slot].set(Value::Number(prepared.index));
    registers[0] = Value::Number(addend);
    registers[1] = receiver;
    registers[2] = Value::Number(region.left_argument);
    registers[3] = Value::Number(region.right_argument);
    registers[region.left_argument_register] = Value::Number(region.left_argument);
    registers[region.right_argument_register] = Value::Number(region.right_argument);
    registers[region.previous_index_register] = Value::Number(prepared.index - region.step);
    registers[region.step_register] = Value::Number(region.step);
    registers[region.next_index_register] = Value::Number(prepared.index);
    self.execution.active.pc = if prepared.index < region.limit {
      region.entry_pc
    } else {
      region.completed_pc
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
      .saturating_add(prepared.iterations as u64);
    Ok(true)
  }

  fn try_own_method_read_loop_region(
    &mut self,
    region: crate::vm::region::OwnMethodReadLoopRegion,
    registers: &mut [Value; 16],
  ) -> Result<bool, RuntimeError> {
    if self.execution.active.pc != region.entry_pc
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
        .get(region.index_slot)
        .is_some_and(|cell| cell.strong_count() != 1)
      || [
        region.index_slot,
        region.accumulator_slot,
        region.call_temporary_slot,
        region.arithmetic_temporary_slot,
      ]
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
    let number_local = |slot: usize| {
      self
        .execution
        .active
        .locals
        .get(slot)
        .and_then(|cell| match cell.value() {
          Value::Number(value) => Some(value),
          _ => None,
        })
    };
    let Some(index) = number_local(region.index_slot) else {
      return Ok(false);
    };
    let Some(accumulator) = number_local(region.accumulator_slot) else {
      return Ok(false);
    };
    if index.partial_cmp(&region.limit) != Some(std::cmp::Ordering::Less) {
      registers[region.index_register] = Value::Number(index);
      registers[region.limit_register] = Value::Number(region.limit);
      self.execution.active.pc = region.completed_pc;
      return Ok(true);
    }
    const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;
    if !index.is_finite()
      || index < 0.0
      || (index == 0.0 && index.is_sign_negative())
      || index.fract() != 0.0
      || index > MAX_SAFE_INTEGER
      || !region.limit.is_finite()
      || region.limit > MAX_SAFE_INTEGER
    {
      return Ok(false);
    }
    let receiver = self
      .execution
      .active
      .locals
      .get(region.receiver_slot)
      .map(BindingCell::value);
    let Some(Value::Object(object)) = receiver else {
      return Ok(false);
    };
    let receiver = Value::Object(Rc::clone(&object));
    let Some(crate::bytecode::Constant::String(method_key)) = self
      .execution
      .active
      .chunk
      .constants
      .get(region.method_key_constant)
    else {
      return Ok(false);
    };
    let callee = {
      let object = object.borrow();
      if object.module_namespace_exports.is_some() {
        return Ok(false);
      }
      match object
        .properties
        .get(&PropertyKey::String(method_key.clone()))
      {
        Some(PropertyDescriptor::Data { value, .. }) => value.clone(),
        _ => return Ok(false),
      }
    };
    let Value::Closure { proto, .. } = &callee else {
      return Ok(false);
    };
    let Some(read) = crate::vm::region::ReceiverNumericRead::decode(proto) else {
      return Ok(false);
    };
    let Some(crate::bytecode::Constant::String(property_key)) = proto
      .chunk
      .constants
      .get(read.property_key_constant)
    else {
      return Ok(false);
    };
    let addend = {
      let object = object.borrow();
      match object
        .properties
        .get(&PropertyKey::String(property_key.clone()))
      {
        Some(PropertyDescriptor::Data {
          value: Value::Number(value),
          ..
        }) => *value,
        _ => return Ok(false),
      }
    };
    let mut prepared = crate::vm::region::PreparedNumericRegion {
      index,
      limit: region.limit,
      accumulator,
      step: region.step,
      addend,
      iterations: 0,
      shape: region.program.shape(),
    };
    if !self
      .profiling
      .jit
      .run_numeric_region(&mut prepared)
      .map_err(RuntimeError::from)?
    {
      return Ok(false);
    }
    if prepared.iterations > u32::from(region.program.cost().max_iterations) {
      return Err(RuntimeError::from(
        "native own-method read loop exceeded its batch bound",
      ));
    }
    ensure_slot(
      &mut self.execution.active.locals,
      region.call_temporary_slot,
    );
    ensure_slot(
      &mut self.execution.active.locals,
      region.arithmetic_temporary_slot,
    );
    self.execution.active.locals[region.call_temporary_slot].set(receiver.clone());
    self.execution.active.locals[region.arithmetic_temporary_slot]
      .set(Value::Number(prepared.index));
    self.execution.active.locals[region.accumulator_slot].set(Value::Number(prepared.accumulator));
    self.execution.active.locals[region.index_slot].set(Value::Number(prepared.index));
    registers[0] = Value::Number(addend);
    registers[1] = receiver;
    registers[region.previous_index_register] = Value::Number(prepared.index - region.step);
    registers[region.step_register] = Value::Number(region.step);
    registers[region.next_index_register] = Value::Number(prepared.index);
    self.execution.active.pc = if prepared.index < region.limit {
      region.entry_pc
    } else {
      region.completed_pc
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
      .saturating_add(prepared.iterations as u64);
    Ok(true)
  }

  fn try_own_method_effect_loop_region(
    &mut self,
    region: crate::vm::region::OwnMethodEffectLoopRegion,
    registers: &mut [Value; 16],
  ) -> Result<bool, RuntimeError> {
    if self.execution.active.pc != region.entry_pc
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
        .get(region.index_slot)
        .is_some_and(|cell| cell.strong_count() != 1)
      || [
        region.index_slot,
        region.call_temporary_slot,
        region.arithmetic_temporary_slot,
      ]
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
    let Some(index) = self
      .execution
      .active
      .locals
      .get(region.index_slot)
      .and_then(|cell| match cell.value() {
        Value::Number(value) => Some(value),
        _ => None,
      })
    else {
      return Ok(false);
    };
    if index.partial_cmp(&region.limit) != Some(std::cmp::Ordering::Less) {
      registers[region.index_register] = Value::Number(index);
      registers[region.limit_register] = Value::Number(region.limit);
      self.execution.active.pc = region.completed_pc;
      return Ok(true);
    }
    const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;
    if !index.is_finite()
      || index < 0.0
      || (index == 0.0 && index.is_sign_negative())
      || index.fract() != 0.0
      || index > MAX_SAFE_INTEGER
      || !region.limit.is_finite()
      || region.limit > MAX_SAFE_INTEGER
    {
      return Ok(false);
    }
    let receiver = self
      .execution
      .active
      .locals
      .get(region.receiver_slot)
      .map(BindingCell::value);
    let Some(Value::Object(object)) = receiver else {
      return Ok(false);
    };
    let receiver = Value::Object(Rc::clone(&object));
    let Some(crate::bytecode::Constant::String(method_key)) = self
      .execution
      .active
      .chunk
      .constants
      .get(region.method_key_constant)
    else {
      return Ok(false);
    };
    let callee = {
      let object = object.borrow();
      if object.module_namespace_exports.is_some() {
        return Ok(false);
      }
      match object
        .properties
        .get(&PropertyKey::String(method_key.clone()))
      {
        Some(PropertyDescriptor::Data { value, .. }) => value.clone(),
        _ => return Ok(false),
      }
    };
    let Value::Closure { proto, .. } = &callee else {
      return Ok(false);
    };
    let Some(effect) = crate::vm::region::ReceiverNumericEffect::decode(proto) else {
      return Ok(false);
    };
    if effect.argument_slot != 0
      || effect.result != crate::vm::region::MethodResultSemantics::Undefined
    {
      return Ok(false);
    }
    let Some(crate::bytecode::Constant::String(property_key)) = proto
      .chunk
      .constants
      .get(effect.property_key_constant)
    else {
      return Ok(false);
    };
    if property_key == method_key {
      return Ok(false);
    }
    let initial_value = {
      let object = object.borrow();
      match object
        .properties
        .get(&PropertyKey::String(property_key.clone()))
      {
        Some(PropertyDescriptor::Data {
          value: Value::Number(value),
          writable: true,
          ..
        }) => *value,
        _ => return Ok(false),
      }
    };
    let mut prepared = crate::vm::region::PreparedNumericRegion {
      index,
      limit: region.limit,
      accumulator: initial_value,
      step: region.step,
      addend: region.argument,
      iterations: 0,
      shape: region.program.shape(),
    };
    if !self
      .profiling
      .jit
      .run_numeric_region(&mut prepared)
      .map_err(RuntimeError::from)?
    {
      return Ok(false);
    }
    if prepared.iterations > u32::from(region.program.cost().max_iterations) {
      return Err(RuntimeError::from(
        "native own-method effect loop exceeded its batch bound",
      ));
    }
    {
      let mut object = object.borrow_mut();
      let Some(PropertyDescriptor::Data {
        value,
        writable: true,
        ..
      }) = object
        .properties
        .get_mut(&PropertyKey::String(property_key.clone()))
      else {
        return Err(RuntimeError::from(
          "own-method effect descriptor changed during native execution",
        ));
      };
      *value = Value::Number(prepared.accumulator);
    }
    self.bump_object_generation(&receiver);
    ensure_slot(
      &mut self.execution.active.locals,
      region.call_temporary_slot,
    );
    ensure_slot(
      &mut self.execution.active.locals,
      region.arithmetic_temporary_slot,
    );
    self.execution.active.locals[region.call_temporary_slot].set(receiver.clone());
    self.execution.active.locals[region.arithmetic_temporary_slot]
      .set(Value::Number(prepared.index));
    self.execution.active.locals[region.index_slot].set(Value::Number(prepared.index));
    registers[0] = Value::Undefined;
    registers[1] = receiver;
    registers[2] = Value::Number(region.argument);
    registers[region.argument_register] = Value::Number(region.argument);
    registers[region.previous_index_register] = Value::Number(prepared.index - region.step);
    registers[region.step_register] = Value::Number(region.step);
    registers[region.next_index_register] = Value::Number(prepared.index);
    self.execution.active.pc = if prepared.index < region.limit {
      region.entry_pc
    } else {
      region.completed_pc
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
      .saturating_add(prepared.iterations as u64);
    Ok(true)
  }

  fn try_own_method_numeric_loop_region(
    &mut self,
    region: crate::vm::region::OwnMethodNumericLoopRegion,
    registers: &mut [Value; 16],
  ) -> Result<bool, RuntimeError> {
    if self.execution.active.pc != region.entry_pc
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
        .get(region.index_slot)
        .is_some_and(|cell| cell.strong_count() != 1)
      || [
        region.index_slot,
        region.accumulator_slot,
        region.call_temporary_slot,
        region.arithmetic_temporary_slot,
      ]
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
    let number_local = |slot: usize| {
      self
        .execution
        .active
        .locals
        .get(slot)
        .and_then(|cell| match cell.value() {
          Value::Number(value) => Some(value),
          _ => None,
        })
    };
    let Some(index) = number_local(region.index_slot) else {
      return Ok(false);
    };
    let Some(accumulator) = number_local(region.accumulator_slot) else {
      return Ok(false);
    };
    if index.partial_cmp(&region.limit) != Some(std::cmp::Ordering::Less) {
      registers[region.index_register] = Value::Number(index);
      registers[region.limit_register] = Value::Number(region.limit);
      self.execution.active.pc = region.completed_pc;
      return Ok(true);
    }
    const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;
    if !index.is_finite()
      || index < 0.0
      || (index == 0.0 && index.is_sign_negative())
      || index.fract() != 0.0
      || index > MAX_SAFE_INTEGER
      || !region.limit.is_finite()
      || region.limit > MAX_SAFE_INTEGER
    {
      return Ok(false);
    }
    let receiver = self
      .execution
      .active
      .locals
      .get(region.receiver_slot)
      .map(BindingCell::value);
    let Some(Value::Object(object)) = receiver else {
      return Ok(false);
    };
    let receiver = Value::Object(Rc::clone(&object));
    let Some(crate::bytecode::Constant::String(key)) = self
      .execution
      .active
      .chunk
      .constants
      .get(region.key_constant)
    else {
      return Ok(false);
    };
    let callee = {
      let object = object.borrow();
      match object
        .properties
        .get(&PropertyKey::String(key.clone()))
      {
        Some(PropertyDescriptor::Data { value, .. }) => value.clone(),
        _ => return Ok(false),
      }
    };
    let Value::Closure { proto, .. } = &callee else {
      return Ok(false);
    };
    let direct = self
      .caches
      .active_direct_calls
      .borrow()
      .get(region.call_site)
      .and_then(Clone::clone)
      .filter(|entry| Rc::ptr_eq(&entry.expected_proto, proto));
    let Some(direct) = direct else {
      return Ok(false);
    };
    let addend = match (direct.target.leaf(), direct.target.contract().arguments) {
      (crate::vm::jit::DirectLeaf::Number(value), crate::vm::jit::DirectArgumentUse::None) => value,
      (
        crate::vm::jit::DirectLeaf::Parameter(0),
        crate::vm::jit::DirectArgumentUse::Parameter(0),
      ) => region.argument,
      (
        crate::vm::jit::DirectLeaf::NumericBinary(_),
        crate::vm::jit::DirectArgumentUse::NumericPair,
      ) => {
        return Ok(false);
      }
      _ => return Ok(false),
    };
    let mut prepared = crate::vm::region::PreparedNumericRegion {
      index,
      limit: region.limit,
      accumulator,
      step: region.step,
      addend,
      iterations: 0,
      shape: region.program.shape(),
    };
    if !self
      .profiling
      .jit
      .run_numeric_region(&mut prepared)
      .map_err(RuntimeError::from)?
    {
      return Ok(false);
    }
    if prepared.iterations > u32::from(region.program.cost().max_iterations) {
      return Err(RuntimeError::from(
        "native own-method loop exceeded its batch bound",
      ));
    }
    ensure_slot(
      &mut self.execution.active.locals,
      region.call_temporary_slot,
    );
    ensure_slot(
      &mut self.execution.active.locals,
      region.arithmetic_temporary_slot,
    );
    self.execution.active.locals[region.call_temporary_slot].set(receiver.clone());
    self.execution.active.locals[region.arithmetic_temporary_slot]
      .set(Value::Number(prepared.index));
    self.execution.active.locals[region.accumulator_slot].set(Value::Number(prepared.accumulator));
    self.execution.active.locals[region.index_slot].set(Value::Number(prepared.index));
    registers[0] = Value::Number(addend);
    registers[1] = receiver;
    registers[2] = Value::Number(region.argument);
    registers[region.argument_register] = Value::Number(region.argument);
    registers[region.previous_index_register] = Value::Number(prepared.index - region.step);
    registers[region.step_register] = Value::Number(region.step);
    registers[region.next_index_register] = Value::Number(prepared.index);
    self.execution.active.pc = if prepared.index < region.limit {
      region.entry_pc
    } else {
      region.completed_pc
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
      .saturating_add(prepared.iterations as u64);
    Ok(true)
  }

  pub(super) fn try_native_region(
    &mut self,
    id: crate::vm::region::RegionId,
    registers: &mut [Value; 16],
  ) -> Result<bool, RuntimeError> {
    let Some(region) = self
      .caches
      .active_regions
      .get(id.index())
      .copied()
    else {
      return Ok(false);
    };
    match region {
      crate::vm::region::NativeRegion::NumericLoop(region) => {
        self.try_numeric_loop_region(region, registers)
      }
      crate::vm::region::NativeRegion::DenseArrayPush(region) => {
        self.try_dense_array_push_region(region, registers)
      }
      crate::vm::region::NativeRegion::DenseMaskedRead(region) => {
        self.try_dense_masked_read_region(region, registers)
      }
      crate::vm::region::NativeRegion::OwnMethodNumericLoop(region) => {
        self.try_own_method_numeric_loop_region(region, registers)
      }
      crate::vm::region::NativeRegion::OwnMethodEffectLoop(region) => {
        self.try_own_method_effect_loop_region(region, registers)
      }
      crate::vm::region::NativeRegion::OwnMethodReadLoop(region) => {
        self.try_own_method_read_loop_region(region, registers)
      }
      crate::vm::region::NativeRegion::OwnMethodBinaryLoop(region) => {
        self.try_own_method_binary_loop_region(region, registers)
      }
    }
  }

  fn try_numeric_loop_region(
    &mut self,
    region: crate::vm::region::NumericLoopRegion,
    registers: &mut [Value; 16],
  ) -> Result<bool, RuntimeError> {
    if self.execution.active.pc != region.entry_pc
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
        .get(region.index_slot)
        .is_some_and(|cell| cell.strong_count() != 1)
      || [
        region.index_slot,
        region.accumulator_slot,
        region.temporary_slot,
      ]
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
    let number_local = |slot: usize| {
      self
        .execution
        .active
        .locals
        .get(slot)
        .and_then(|cell| match cell.value() {
          Value::Number(value) => Some(value),
          _ => None,
        })
    };
    let Some(index) = number_local(region.index_slot) else {
      return Ok(false);
    };
    let Some(accumulator) = number_local(region.accumulator_slot) else {
      return Ok(false);
    };
    let mut numeric_shape = region.program.shape();
    let addend = match region.prerequisites {
      crate::vm::region::NumericLoopPrerequisites::AddIndex => 0.0,
      crate::vm::region::NumericLoopPrerequisites::AddProperty {
        object_slot,
        key_constant,
      } => {
        let object = self
          .execution
          .active
          .locals
          .get(object_slot)
          .and_then(|cell| match cell.value() {
            Value::Object(object) => Some(object),
            _ => None,
          });
        let Some(object) = object else {
          return Ok(false);
        };
        let Some(crate::bytecode::Constant::String(key)) = self
          .execution
          .active
          .chunk
          .constants
          .get(key_constant)
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
        property
      }
      crate::vm::region::NumericLoopPrerequisites::DirectCall {
        callee_slot,
        call_site,
        arguments,
      } => {
        let callee = self
          .execution
          .active
          .locals
          .get(callee_slot)
          .map(BindingCell::value);
        let Some(Value::Closure { proto, .. }) = callee else {
          return Ok(false);
        };
        let direct = self
          .caches
          .active_direct_calls
          .borrow()
          .get(call_site)
          .and_then(Clone::clone)
          .filter(|entry| Rc::ptr_eq(&entry.expected_proto, &proto));
        let Some(direct) = direct else {
          return Ok(false);
        };
        match (direct.target.leaf(), arguments) {
          (
            crate::vm::jit::DirectLeaf::Number(value),
            crate::vm::region::NumericLeafArguments::None,
          ) => value,
          (
            crate::vm::jit::DirectLeaf::Parameter(0),
            crate::vm::region::NumericLeafArguments::OneConstant { value, .. },
          ) => value,
          (
            crate::vm::jit::DirectLeaf::NumericBinary(operation),
            crate::vm::region::NumericLeafArguments::AccumulatorAndConstant { value, .. },
          ) => {
            numeric_shape = match operation {
              crate::vm::jit::NumericBinaryOp::Add => {
                crate::vm::region::NumericRegionShape::AddPrepared
              }
              crate::vm::jit::NumericBinaryOp::Subtract => {
                crate::vm::region::NumericRegionShape::SubtractPrepared
              }
              crate::vm::jit::NumericBinaryOp::Multiply => {
                crate::vm::region::NumericRegionShape::MultiplyPrepared
              }
              crate::vm::jit::NumericBinaryOp::Divide => {
                crate::vm::region::NumericRegionShape::DividePrepared
              }
            };
            value
          }
          _ => return Ok(false),
        }
      }
    };
    let mut prepared = crate::vm::region::PreparedNumericRegion {
      index,
      limit: region.limit,
      accumulator,
      step: region.step,
      addend,
      iterations: 0,
      shape: numeric_shape,
    };
    let execution = self
      .profiling
      .jit
      .execute_numeric_region(
        &mut prepared,
        self.interrupts.native_poll(),
        crate::vm::jit::NUMERIC_ACTIVATION_LIMIT,
      )
      .map_err(RuntimeError::from)?;
    let outcome = match execution {
      crate::vm::jit::NumericRegionExecution::Unavailable => return Ok(false),
      crate::vm::jit::NumericRegionExecution::Executed(outcome) => outcome,
    };
    let total_iterations = u64::from(outcome.iterations);
    if total_iterations > 0 {
      ensure_slot(&mut self.execution.active.locals, region.temporary_slot);
      self.execution.active.locals[region.temporary_slot].set(Value::Number(prepared.index));
      self.execution.active.locals[region.accumulator_slot]
        .set(Value::Number(prepared.accumulator));
      self.execution.active.locals[region.index_slot].set(Value::Number(prepared.index));
      match region.exits.body {
        crate::vm::region::NumericBodyWriteback::Numeric {
          accumulator_register,
          step_register,
        } => {
          registers[accumulator_register] = Value::Number(prepared.accumulator);
          registers[step_register] = Value::Number(region.step);
        }
        crate::vm::region::NumericBodyWriteback::DirectCall { arguments } => match arguments {
          crate::vm::region::NumericLeafArguments::None => {
            registers[0] = Value::Number(addend);
          }
          crate::vm::region::NumericLeafArguments::OneConstant { value, register } => {
            registers[0] = Value::Number(addend);
            registers[register] = Value::Number(value);
          }
          crate::vm::region::NumericLeafArguments::AccumulatorAndConstant {
            accumulator_register,
            value,
            value_register,
            result_register,
          } => {
            registers[0] = Value::Number(prepared.accumulator);
            registers[accumulator_register] = Value::Number(prepared.accumulator);
            registers[value_register] = Value::Number(value);
            registers[result_register] = Value::Number(prepared.accumulator);
          }
        },
      }
    }
    registers[region.exits.index_register] = Value::Number(prepared.index);
    registers[region.exits.limit_register] = Value::Number(region.limit);
    self.execution.active.pc = if prepared.index < prepared.limit {
      region.entry_pc
    } else {
      region.completed_pc
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
      .saturating_add(total_iterations);
    if let crate::vm::jit::NumericRegionExit::Interrupted {
      observed_generation,
    } = outcome.exit
    {
      self.interrupts.acknowledge(observed_generation);
      return Err(RuntimeError::execution_interrupted());
    }
    self.poll_execution_interrupt()?;
    Ok(true)
  }
}
