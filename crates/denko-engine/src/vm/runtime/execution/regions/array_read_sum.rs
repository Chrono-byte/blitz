// SPDX-License-Identifier: MPL-2.0
use super::*;

struct ExactIntegerSum {
  index: i64,
  limit: i64,
  step: i64,
  accumulator: i64,
  addend: i64,
}

impl ExactIntegerSum {
  fn prepare(
    index: f64,
    limit: f64,
    step: f64,
    accumulator: f64,
    addends: &[f64],
  ) -> Option<Self> {
    let integer = |value: f64| {
      (value >= f64::from(i32::MIN)
        && value <= f64::from(i32::MAX)
        && value.fract() == 0.0
        && value.to_bits() != (-0.0_f64).to_bits())
      .then_some(value as i64)
    };
    let index = integer(index)?;
    let limit = integer(limit)?;
    let step = integer(step)?;
    let accumulator = integer(accumulator)?;
    let mut addend = 0;
    let mut absolute_sum = 0;
    for value in addends {
      let value = integer(*value)?;
      addend += value;
      absolute_sum += value.abs();
    }
    // Prove every intermediate addition and index update is exact, including
    // cancelling addends. The floating path retains per-operation rounding.
    if step <= 0
      || index.abs() + 4096 * step > i64::from(i32::MAX)
      || accumulator.abs() + 4096 * absolute_sum > i64::from(i32::MAX)
    {
      return None;
    }
    Some(Self {
      index,
      limit,
      step,
      accumulator,
      addend,
    })
  }
}

impl Vm {
  #[allow(clippy::too_many_lines)]
  pub(super) fn try_array_read_sum_region(
    &mut self,
    region: crate::vm::region::ArrayReadSumRegion,
    registers: &mut [Value; 16],
  ) -> Result<bool, RuntimeError> {
    if self.execution.active.pc != region.entry_pc
      || self
        .execution
        .active
        .lexical_env
        .borrow()
        .has_object_environment()
      || [region.index_slot, region.accumulator_slot]
        .into_iter()
        .any(|slot| {
          slot < self.execution.active.arguments.mapped_count
            || self
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
    let slots = [
      region.index_slot,
      region.accumulator_slot,
      region.array_slot,
    ];
    for (position, slot) in slots.iter().enumerate() {
      let Some(cell) = self.execution.active.locals.get(*slot) else {
        return Ok(false);
      };
      if slots[..position]
        .iter()
        .any(|previous| self.execution.active.locals[*previous].same_storage(cell))
      {
        return Ok(false);
      }
    }
    let number = |slot| {
      self
        .execution
        .active
        .locals
        .get(slot)
        .and_then(|cell: &BindingCell| match cell.value() {
          Value::Number(value) => Some(value),
          _ => None,
        })
    };
    let Some(mut index) = number(region.index_slot) else {
      return Ok(false);
    };
    if index.partial_cmp(&region.limit) != Some(std::cmp::Ordering::Less) {
      registers[region.index_register] = Value::Number(index);
      registers[region.limit_register] = Value::Number(region.limit);
      self.execution.active.pc = region.completed_pc;
      return Ok(true);
    }
    let Some(mut accumulator) = number(region.accumulator_slot) else {
      return Ok(false);
    };
    let Some(Value::Array(elements, properties)) = self
      .execution
      .active
      .locals
      .get(region.array_slot)
      .map(BindingCell::value)
    else {
      return Ok(false);
    };
    let mut addends = [0.0; 8];
    {
      let properties = properties.borrow();
      if !properties.internal_slots.is_empty() {
        return Ok(false);
      }
      let length = super::super::super::property::array_length(&properties);
      let elements = elements.borrow();
      for (position, array_index) in region.indices[..region.count]
        .iter()
        .copied()
        .enumerate()
      {
        let key = PropertyKey::String(array_index.to_string().into());
        if array_index >= length || properties.properties.contains_key(&key) {
          return Ok(false);
        }
        let Some(number) = elements.numeric_element(array_index) else {
          return Ok(false);
        };
        addends[position] = number;
      }
    }
    let mut exact = ExactIntegerSum::prepare(
      index,
      region.limit,
      region.step,
      accumulator,
      &addends[..region.count],
    );
    let mut total = 0u64;
    while index < region.limit && total < 4096 {
      self.poll_execution_interrupt()?;
      let completed = if let Some(exact) = &mut exact {
        let count = ((exact.limit - exact.index + exact.step - 1) / exact.step).min(64);
        exact.accumulator += count * exact.addend;
        exact.index += count * exact.step;
        index = exact.index as f64;
        accumulator = exact.accumulator as f64;
        count as u64
      } else {
        let mut completed = 0;
        while index < region.limit && completed < 64 {
          // Preserve each floating-point addition and its rounding.
          for addend in &addends[..region.count] {
            accumulator += addend;
          }
          index += region.step;
          completed += 1;
        }
        completed
      };
      total += completed;
      self.execution.active.locals[region.index_slot].set(Value::Number(index));
      self.execution.active.locals[region.accumulator_slot].set(Value::Number(accumulator));
      registers[region.index_register] = Value::Number(index);
      registers[region.limit_register] = Value::Number(region.limit);
      registers[region.key_register] =
        Value::Number(usize_to_f64(region.indices[region.count - 1]));
      self.execution.active.pc = if index < region.limit {
        region.entry_pc
      } else {
        region.completed_pc
      };
      self.profiling.jit_stats.native_loop_iterations = self
        .profiling
        .jit_stats
        .native_loop_iterations
        .saturating_add(completed);
    }
    self.profiling.jit_stats.native_loop_entries = self
      .profiling
      .jit_stats
      .native_loop_entries
      .saturating_add(1);
    Ok(true)
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::cell::RefCell;
  use std::rc::Rc;

  #[test]
  fn array_sum_region_preserves_order_and_observable_fallbacks() {
    let source = crate::js!(
      r#"
      function sum(array) {
        var total = 1;
        for (var i = 0; i < 10000; i = i + 1) total = total + array[0] + array[2];
        return total;
      }
      var ordered = sum([1e20, 0, -1e20]);
      var ordinary = sum([2, 0, 3]);
      var calls = 0;
      var accessor = [2, 0, 3];
      Object.defineProperty(accessor, '0', { get: function() { calls = calls + 1; return 2; }});
      var observed = sum(accessor);
      var inherited = [2, 0];
      var prototype = Object.create(Array.prototype);
      prototype[2] = 3;
      Object.setPrototypeOf(inherited, prototype);
      inherited.length = 3;
      var holes = sum(inherited);
      var traps = 0;
      var proxy = new Proxy([2, 0, 3], { get: function(target, key) { traps = traps + 1; return target[key]; }});
      var proxied = sum(proxy);
      function edge(array, initial) {
        var result = initial;
        for (var i = -5; i < 130; i = i + 3) result = result + array[0] + array[1];
        return result;
      }
      var exactEdges = edge([7, -7], 9) === 9
        && edge([1, 0], 2147483646) === 2147483691
        && Object.is(edge([-0, -0], -0), -0)
        && Number.isNaN(edge([Infinity, -Infinity], 0));
      globalThis.result = [ordered, ordinary, observed, holes, proxied, calls, traps, exactEdges].join(',');
    "#
    );
    let run = |enabled| {
      let globals = Rc::new(RefCell::new(ObjectData::new()));
      globals.borrow_mut().properties.insert(
        PropertyKey::String("globalThis".into()),
        PropertyDescriptor::ordinary_data(Value::Object(Rc::clone(&globals))),
      );
      crate::runtime_utils::install_minimal_kernel_constructors(&globals);
      let mut vm = Vm::new(globals);
      vm.caches.native_regions_enabled = enabled;
      let session = crate::parser::Parser::parse_source(source).expect("parse");
      vm.run(crate::compiler::compile_session(&session).expect("compile"))
        .expect("execute");
      (
        vm.get_global("result"),
        vm.profiling.jit_stats.native_loop_iterations,
      )
    };
    let (optimized, iterations) = run(true);
    assert_eq!(optimized, run(false).0);
    assert_eq!(
      optimized,
      Some(Value::String(
        "0,50001,50001,50001,50001,10000,20000,true".into()
      ))
    );
    assert!(iterations >= 20000);
  }
}
