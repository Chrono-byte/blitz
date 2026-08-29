// SPDX-License-Identifier: MPL-2.0

use super::*;

impl Vm {
  pub(super) fn try_numeric_compare_branch_sequence(
    &mut self,
    registers: &mut [Value; 16],
  ) -> Result<bool, RuntimeError> {
    let pc = self.execution.active.pc;
    let words = &self.execution.active.chunk.words[pc..pc + 3];
    let left_register = decode_rd(words[0]) as usize;
    let right_register = decode_rd(words[1]) as usize;
    let (condition, _, _, offset) = decode_compare_jump(words[2]);

    let left = {
      let slot = decode_aux16(words[0]) as usize;
      let Some(cell) = self.execution.active.locals.get(slot) else {
        return Ok(false);
      };
      match cell.value() {
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
      .get(decode_aux16(words[1]) as usize)
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
    self.execution.active.pc = if result {
      pc + 3
    } else {
      usize::try_from(pc as i64 + 3 + offset as i64)
        .map_err(|_| RuntimeError::from("negative compare target"))?
    };
    Ok(true)
  }

  /// Execute the compiler's one-argument method-call setup in one dispatch.
  pub(super) fn try_single_argument_method_call_sequence(
    &mut self,
    registers: &mut [Value; 16],
  ) -> Result<bool, RuntimeError> {
    let pc = self.execution.active.pc;
    let words = &self.execution.active.chunk.words[pc..pc + 12];
    let receiver_register = decode_rd(words[0]) as usize;
    let callee_register = decode_rd(words[3]) as usize;
    let argument_register = decode_rd(words[6]) as usize;
    let key_register = decode_rd(words[4]) as usize;

    let receiver_slot = decode_aux16(words[0]) as usize;
    let temporary_slot = decode_aux16(words[1]) as usize;
    let argument_slot = decode_aux16(words[6]) as usize;
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
      .map(BindingCell::value)
    else {
      return Ok(false);
    };
    let Some(argument) = self
      .execution
      .active
      .locals
      .get(argument_slot)
      .map(BindingCell::value)
    else {
      return Ok(false);
    };
    let Some(crate::bytecode::Constant::String(key)) = self
      .execution
      .active
      .chunk
      .constants
      .get(decode_aux16(words[4]) as usize)
    else {
      return Ok(false);
    };
    let key = PropertyKey::String(key.clone());

    ensure_slot(&mut self.execution.active.locals, temporary_slot);
    self.execution.active.locals[temporary_slot].set(receiver.clone());
    let callee = self.get_property_value_cached(pc + 5, &receiver, &key)?;
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
    self.execution.active.pc += 12;
    Ok(true)
  }

  /// Execute a guarded own-data-property numeric accumulation in one dispatch.
  pub(super) fn try_numeric_property_add_sequence(
    &mut self,
    registers: &mut [Value; 16],
  ) -> Result<bool, RuntimeError> {
    let pc = self.execution.active.pc;
    let words = &self.execution.active.chunk.words[pc..pc + 9];
    let accumulator_register = decode_rd(words[0]) as usize;
    let property_register = decode_rd(words[1]) as usize;
    let key_register = decode_rd(words[2]) as usize;
    let accumulator_slot = decode_aux16(words[0]) as usize;
    let object_slot = decode_aux16(words[1]) as usize;
    let temporary_slot = decode_aux16(words[5]) as usize;
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
      match cell.value() {
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
      match cell.value() {
        Value::Object(object) => Some(object),
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
      .get(decode_aux16(words[2]) as usize)
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
    self.execution.active.locals[temporary_slot].set(Value::Number(result));
    self.execution.active.locals[accumulator_slot].set(Value::Number(result));
    self.execution.active.pc += 9;
    Ok(true)
  }

  /// Execute the compiler's guarded numeric increment sequence in one dispatch.
  pub(super) fn try_numeric_increment_sequence(
    &mut self,
    registers: &mut [Value; 16],
  ) -> Result<bool, RuntimeError> {
    let pc = self.execution.active.pc;
    let words = &self.execution.active.chunk.words[pc..pc + 10];
    let source_register = decode_rd(words[0]) as usize;
    let constant_register = decode_rd(words[3]) as usize;
    let result_register = decode_rd(words[4]) as usize;
    let target_slot = decode_aux16(words[0]) as usize;
    let temporary_slot = decode_aux16(words[6]) as usize;
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
      match cell.value() {
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
      .get(decode_aux16(words[3]) as usize)
    else {
      return Ok(false);
    };
    let constant = *constant;
    let result = value + constant;

    ensure_slot(&mut self.execution.active.locals, temporary_slot);
    registers[source_register] = Value::Number(value);
    registers[constant_register] = Value::Number(constant);
    registers[result_register] = Value::Number(result);
    self.execution.active.locals[temporary_slot].set(Value::Number(result));
    self.execution.active.locals[target_slot].set(Value::Number(result));
    self.execution.active.pc += 10;
    Ok(true)
  }

  /// Execute the common `LoadLocal, LoadLocal, Add, StoreLocal` sequence in
  /// one interpreter dispatch when every value is already numeric.
  pub(super) fn try_numeric_add_local_sequence(
    &mut self,
    registers: &mut [Value; 16],
  ) -> Result<bool, RuntimeError> {
    let pc = self.execution.active.pc;
    let words = &self.execution.active.chunk.words[pc..pc + 4];
    let left_register = decode_rd(words[0]) as usize;
    let right_register = decode_rd(words[1]) as usize;

    let left_slot = decode_aux16(words[0]) as usize;
    let right_slot = decode_aux16(words[1]) as usize;
    let destination_slot = decode_aux16(words[3]) as usize;
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
      match (left_cell.value(), right_cell.value()) {
        (Value::Number(left), Value::Number(right)) => Some((left, right)),
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
    self.execution.active.locals[destination_slot].set(Value::Number(result));
    registers[left_register] = Value::Number(result);
    self.execution.active.pc += 4;
    Ok(true)
  }
}
