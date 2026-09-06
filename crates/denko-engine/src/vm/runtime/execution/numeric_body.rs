// SPDX-License-Identifier: MPL-2.0
use super::*;

impl Vm {
  pub(super) fn try_numeric_body_region(
    &mut self,
    region: crate::vm::region::NumericBodyRegion,
    registers: &mut [Value; 16],
  ) -> Result<bool, RuntimeError> {
    if self.execution.active.pc != region.entry_pc
      || self
        .execution
        .active
        .lexical_env
        .borrow()
        .has_object_environment()
    {
      return Ok(false);
    }
    let Some(compiled) = self
      .profiling
      .jit
      .numeric_body(&self.execution.active.chunk, region)
    else {
      return Ok(false);
    };
    let plan = &compiled.plan;
    let mut locals = [0.0; 16];
    for (index, slot) in plan.locals[..plan.local_count]
      .iter()
      .copied()
      .enumerate()
    {
      let Some(cell) = self.execution.active.locals.get(slot) else {
        return Ok(false);
      };
      // Separate portable slots can share a binding cell in host-created frames.
      // A numeric local snapshot must not hide writes through such aliases.
      if plan.locals[..index]
        .iter()
        .any(|previous| self.execution.active.locals[*previous].same_storage(cell))
      {
        return Ok(false);
      }
      if plan.writes & (1 << index) != 0
        && (slot < self.execution.active.arguments.mapped_count
          || self
            .execution
            .active
            .slots
            .as_ref()
            .and_then(|slots| slots.immutable.get(slot))
            .copied()
            .unwrap_or(false)
          || matches!(cell.value(), Value::Uninitialized))
      {
        return Ok(false);
      }
      if plan.reads & (1 << index) != 0 {
        let Value::Number(number) = cell.value() else {
          return Ok(false);
        };
        locals[index] = number;
      }
    }
    self.poll_execution_interrupt()?;
    let mut frame = crate::vm::jit::numeric_body::NumericBodyFrame {
      registers: [0.0; 16],
      locals,
      iterations: 0,
      completed: 0,
    };
    let mut iterations = 0u64;
    loop {
      compiled.execute(&mut frame);
      iterations += u64::from(frame.iterations);
      if iterations > 0 {
        for (index, slot) in plan.locals[..plan.local_count]
          .iter()
          .copied()
          .enumerate()
        {
          if plan.writes & (1 << index) != 0 {
            self.execution.active.locals[slot].set(Value::Number(frame.locals[index]));
          }
        }
      }
      let written_registers = if iterations > 0 {
        plan.registers
      } else {
        (1 << plan.comparison.0) | (1 << plan.comparison.1)
      };
      for index in 0..16 {
        if written_registers & (1 << index) != 0 {
          registers[index] = Value::Number(frame.registers[index]);
        }
      }
      self.execution.active.pc = if frame.completed != 0 {
        region.completed_pc
      } else {
        region.entry_pc
      };
      self.profiling.jit_stats.native_loop_iterations = self
        .profiling
        .jit_stats
        .native_loop_iterations
        .saturating_add(u64::from(frame.iterations));
      self.poll_execution_interrupt()?;
      if frame.completed != 0 || iterations == 4096 {
        break;
      }
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
  fn evaluate(
    source: &str,
    optimized: bool,
  ) -> Vm {
    let session = crate::parser::Parser::parse_source(source).expect("parse");
    let code = crate::compiler::compile_repl_session(&session).expect("compile");
    let mut vm = Vm::new(Rc::new(RefCell::new(ObjectData::new())));
    vm.caches.optimized_execution_enabled = optimized;
    vm.run(code).expect("evaluate");
    vm
  }

  #[test]
  fn numeric_body_preserves_multiple_accumulators_and_remainder() {
    let source = crate::js!(
      r#"
      function compute(limit) {
        var sum = 0; var prod = 1;
        for (var i = 1; i < limit; i = i + 1) {
          sum = sum + i; prod = prod * ((i % 10) + 1);
        }
        return sum === 499500 && prod === Infinity && i === 1000;
      }
      var result = compute(1000);
    "#
    );
    for optimized in [false, true] {
      let vm = evaluate(source, optimized);
      assert_eq!(vm.get_global("result"), Some(Value::Bool(true)));
      if optimized {
        assert!(
          vm.native_execution_statistics()
            .native_loop_iterations
            >= 999
        );
      }
    }
  }

  #[test]
  fn numeric_body_preserves_exceptional_numbers_and_coercion_fallbacks() {
    let source = crate::js!(
      r#"
      function compute(value, bound) {
        var sum = -0; var quotient = value;
        for (var i = 0; i < bound; i = i + 1) {
          sum = sum * 2; quotient = quotient / -2; value = value % 3;
        }
        return [sum, quotient, value, i];
      }
      var a = compute(Infinity, 131);
      var b = compute(-0, 2);
      var calls = 0;
      var c = compute({ valueOf() { calls++; return 6; } }, 2);
      var d = compute(5, NaN);
      var result = Object.is(a[0], -0) && a[1] === -Infinity && Number.isNaN(a[2])
        && Object.is(b[1], -0) && Object.is(b[2], -0)
        && c[1] === 1.5 && calls === 2 && d[2] === 5 && d[3] === 0;
    "#
    );
    for optimized in [false, true] {
      let vm = evaluate(source, optimized);
      assert_eq!(vm.get_global("result"), Some(Value::Bool(true)));
    }
  }

  #[test]
  fn numeric_body_rejects_calls_and_uninitialized_register_reads() {
    let session = crate::parser::Parser::parse_source(
      "function f() { var s=0; for(var i=0;i<20;i=i+1) { s=s+(i%3); } return s; }",
    )
    .expect("parse");
    let root = crate::compiler::compile_session(&session).expect("compile");
    let code = &root.functions[0].chunk;
    let region = (0..code.words.len())
      .find_map(|pc| crate::vm::region::NumericBodyRegion::decode(code, pc))
      .expect("numeric body");
    let mut changed = (**code).clone();
    changed.words[region.entry_pc + 3] = Opcode::Call as u32;
    assert!(crate::vm::region::NumericBodyRegion::decode(&changed, region.entry_pc).is_none());
    changed.words[region.entry_pc + 3] = Opcode::Add as u32 | (15 << 8) | (14 << 12);
    assert!(crate::vm::region::NumericBodyRegion::decode(&changed, region.entry_pc).is_none());
  }
}
