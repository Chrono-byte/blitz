// SPDX-License-Identifier: MPL-2.0
use super::*;
use crate::ecmascript::semantics::byte_views::{array_buffer_is_shared, byte_view_details};
use crate::runtime_utils::strict_equal;
use crate::vm::region::typed_view::{TypedViewOperation as Op, TypedViewProgram};
use num_traits::ToPrimitive;

impl Vm {
  #[allow(clippy::too_many_lines)]
  pub(super) fn try_typed_view_region(
    &mut self,
    region: crate::vm::region::TypedViewRegion,
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
    let Some(view) = self
      .execution
      .active
      .locals
      .get(region.view_slot)
      .map(BindingCell::value)
    else {
      return Ok(false);
    };
    let Value::Object(object) = &view else {
      return Ok(false);
    };
    if !matches!(object.borrow().internal_slot(crate::ecmascript::semantics::byte_views::TYPED_ARRAY_KIND_SLOT),
      Some(Value::String(kind)) if kind == "Float64Array")
    {
      return Ok(false);
    }
    let Some(details) = byte_view_details(&view) else {
      return Ok(false);
    };
    if details.kind != "Float64Array" || array_buffer_is_shared(&details.buffer) != Some(false) {
      return Ok(false);
    }
    let Some(end) = details
      .length
      .checked_mul(8)
      .and_then(|size| details.byte_offset.checked_add(size))
    else {
      return Ok(false);
    };
    if end > details.data.borrow().len() {
      return Ok(false);
    }
    let mut current = view.clone();
    let mut valid_length = false;
    for depth in 0..crate::vm::state::MAX_PROTOTYPE_DEPTH {
      let Value::Object(object) = current else {
        break;
      };
      let object = object.borrow();
      if object.module_namespace_exports.is_some()
        || (depth > 0 && !object.internal_slots.is_empty())
      {
        return Ok(false);
      }
      if let Some(descriptor) = object
        .properties
        .get(&PropertyKey::String("length".into()))
      {
        valid_length = match descriptor {
          PropertyDescriptor::Data {
            value: Value::Number(length),
            ..
          } => *length == usize_to_f64(details.length),
          PropertyDescriptor::Accessor {
            get: Some(getter), ..
          } => self
            .realm()
            .intrinsics
            .typed_array_length_getter
            .as_ref()
            .is_some_and(|original| strict_equal(original, getter)),
          _ => false,
        };
        break;
      }
      current = object.get_prototype();
    }
    if !valid_length {
      return Ok(false);
    }
    let owner = Arc::clone(&self.execution.active.chunk);
    let Some(compiled) = self.profiling.jit.typed_view(&owner, region) else {
      return Ok(false);
    };
    let plan = &compiled.plan;
    let mut locals = [0.0; 16];
    for (index, slot) in plan.locals.iter().copied().enumerate() {
      let Some(cell) = self.execution.active.locals.get(slot) else {
        return Ok(false);
      };
      if plan
        .locals
        .iter()
        .copied()
        .chain(std::iter::once(plan.view_slot))
        .any(|other| {
          other != slot
            && self
              .execution
              .active
              .locals
              .get(other)
              .is_some_and(|other| cell.same_storage(other))
        })
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
        let Value::Number(value) = cell.value() else {
          return Ok(false);
        };
        locals[index] = value;
      }
    }
    if compiled.is_native() {
      return self.execute_native_typed_view(region, &compiled, locals, &details, &view, registers);
    }
    let mut numbers = [0.0; 16];
    let mut tags = [0u8; 16];
    let mut iterations = 0u64;
    let mut side_exit = false;
    let mut written_view = false;
    self.poll_execution_interrupt()?;
    loop {
      let before = (locals, numbers, tags);
      let mut entered = false;
      for (position, operation) in plan.operations.iter().copied().enumerate() {
        if position == 4 {
          if numbers[plan.comparison.0].partial_cmp(&numbers[plan.comparison.1])
            != Some(std::cmp::Ordering::Less)
          {
            break;
          }
          entered = true;
        }
        match operation {
          Op::Numeric(operation, destination) => {
            operation.execute(&mut numbers, &mut locals);
            if let Some(rd) = destination {
              tags[rd] = 1;
            }
          }
          Op::View(rd) => tags[rd] = 2,
          Op::Key(rd) => tags[rd] = 3,
          Op::Length(rd) => {
            numbers[rd] = usize_to_f64(details.length);
            tags[rd] = 1;
          }
          Op::Guard(condition, left, right) => {
            let a = numbers[left];
            let b = numbers[right];
            let result = match condition {
              0 | 6 => a == b,
              1 | 7 => a != b,
              2 => a < b,
              3 => a <= b,
              4 => a > b,
              5 => a >= b,
              _ => unreachable!(),
            };
            if result {
              side_exit = true;
              break;
            }
          }
          Op::Read(rd, index) | Op::Write(rd, index, _) => {
            let value = numbers[index];
            if !value.is_finite()
              || value < 0.0
              || value.is_sign_negative()
              || value.fract() != 0.0
              || value >= usize_to_f64(details.length)
            {
              side_exit = true;
              break;
            }
            let start = details.byte_offset + (value as usize) * 8;
            if let Op::Write(_, _, source) = operation {
              if !written_view {
                self.bump_object_generation(&view);
                written_view = true;
              }
              let value = numbers[source];
              let mut data = details.data.borrow_mut();
              let slots: &mut Vec<Option<Value>> = &mut data;
              for (slot, byte) in slots[start..start + 8]
                .iter_mut()
                .zip(value.to_le_bytes())
              {
                *slot = Some(Value::Number(f64::from(byte)));
              }
              numbers[rd] = value;
            } else {
              let data = details.data.borrow();
              let mut bytes = [0; 8];
              for (offset, byte) in bytes.iter_mut().enumerate() {
                *byte = data
                  .numeric_element(start + offset)
                  .and_then(|n| n.to_u8())
                  .unwrap_or(0);
              }
              numbers[rd] = f64::from_le_bytes(bytes);
            }
            tags[rd] = 1;
          }
        }
      }
      if side_exit {
        (locals, numbers, tags) = before;
        self.execution.active.pc = region.entry_pc;
        break;
      }
      if !entered {
        self.execution.active.pc = region.completed_pc;
        break;
      }
      iterations += 1;
      self.execution.active.pc = region.entry_pc;
      self.profiling.jit_stats.native_loop_iterations = self
        .profiling
        .jit_stats
        .native_loop_iterations
        .saturating_add(1);
      if iterations.is_multiple_of(64) {
        self.commit_typed_view_state(plan, &locals, &numbers, &tags, &view, registers, true);
        self.poll_execution_interrupt()?;
      }
      if iterations == 4096 {
        break;
      }
    }
    if iterations == 0 && side_exit {
      return Ok(false);
    }
    self.commit_typed_view_state(
      plan,
      &locals,
      &numbers,
      &tags,
      &view,
      registers,
      iterations > 0,
    );
    self.profiling.jit_stats.native_loop_entries = self
      .profiling
      .jit_stats
      .native_loop_entries
      .saturating_add(1);
    Ok(true)
  }

  #[allow(clippy::too_many_arguments)]
  fn execute_native_typed_view(
    &mut self,
    region: crate::vm::region::TypedViewRegion,
    compiled: &crate::vm::jit::typed_view::CompiledTypedView,
    locals: [f64; 16],
    details: &crate::ecmascript::semantics::byte_views::ByteViewDetails,
    view: &Value,
    registers: &mut [Value; 16],
  ) -> Result<bool, RuntimeError> {
    if details.data.borrow().packed_bytes().is_none() {
      return Ok(false);
    }
    let end = details.byte_offset + details.length * 8;
    let mut frame = crate::vm::jit::typed_view::TypedViewFrame::new(locals, details.length);
    let mut tags = [0u8; 16];
    let mut iterations = 0u64;
    let mut mutated = false;
    loop {
      self.poll_execution_interrupt()?;
      {
        let mut data = details.data.borrow_mut();
        let bytes = data
          .packed_bytes_mut()
          .expect("validated packed backing");
        compiled.execute(&mut frame, &mut bytes[details.byte_offset..end]);
      }
      if frame.iterations > 0 {
        for operation in &compiled.plan.operations {
          match *operation {
            Op::Numeric(_, Some(rd)) | Op::Length(rd) | Op::Read(rd, _) | Op::Write(rd, _, _) => {
              tags[rd] = 1
            }
            Op::View(rd) => tags[rd] = 2,
            Op::Key(rd) => tags[rd] = 3,
            _ => {}
          }
        }
      }
      if frame.exit == 1 {
        for operation in &compiled.plan.operations[..4] {
          match *operation {
            Op::Numeric(_, Some(rd)) | Op::Length(rd) => tags[rd] = 1,
            Op::View(rd) => tags[rd] = 2,
            Op::Key(rd) => tags[rd] = 3,
            _ => {}
          }
        }
      }
      if frame.dirty_end > 0 && !mutated {
        self.bump_object_generation(view);
        mutated = true;
      }
      iterations += u64::from(frame.iterations);
      if iterations == 0 && frame.exit == 2 {
        return Ok(false);
      }
      self.commit_typed_view_state(
        &compiled.plan,
        &frame.locals,
        &frame.registers,
        &tags,
        view,
        registers,
        iterations > 0,
      );
      self.execution.active.pc = if frame.exit == 1 {
        region.completed_pc
      } else {
        region.entry_pc
      };
      self.profiling.jit_stats.native_loop_iterations = self
        .profiling
        .jit_stats
        .native_loop_iterations
        .saturating_add(u64::from(frame.iterations));
      if frame.exit != 0 || iterations >= 4096 {
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

  #[allow(clippy::too_many_arguments)]
  fn commit_typed_view_state(
    &mut self,
    plan: &TypedViewProgram,
    locals: &[f64; 16],
    numbers: &[f64; 16],
    tags: &[u8; 16],
    view: &Value,
    registers: &mut [Value; 16],
    write_locals: bool,
  ) {
    for (index, slot) in plan.locals.iter().copied().enumerate() {
      if write_locals && plan.writes & (1 << index) != 0 {
        self.execution.active.locals[slot].set(Value::Number(locals[index]));
      }
    }
    for index in 0..16 {
      match tags[index] {
        1 => registers[index] = Value::Number(numbers[index]),
        2 => registers[index] = view.clone(),
        3 => registers[index] = Value::String("length".into()),
        _ => {}
      }
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::cell::RefCell;
  use std::rc::Rc;

  #[test]
  fn typed_numeric_loops_preserve_fill_reads_and_guarded_throw() {
    let source = crate::js!(
      r#"
      function fill(values) {
        for (var i = 0; i < values.length; i++) values[i] = i * 3 % 17;
        return i;
      }
      function sum(values) {
        var result = 0;
        for (var i = 0; i < values.length; i++) {
          if (values[i] < 0) throw new Error('negative');
          result += values[i];
        }
        return result;
      }
      var values = new Float64Array(130);
      var count = fill(values);
      var total = sum(values);
      values[70] = -1;
      var thrown = false;
      try { sum(values); } catch (error) { thrown = error.message === 'negative'; }
      var getterCalls = 0;
      Object.defineProperty(values, 'length', { get: function() { getterCalls++; return 2; }});
      var limited = fill(values);
      globalThis.result = [count, total, thrown, limited, getterCalls, values[1]].join(',');
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
        .expect("run");
      (
        vm.get_global("result"),
        vm.profiling.jit_stats.native_loop_iterations,
      )
    };
    let (optimized, iterations) = run(true);
    assert_eq!(optimized, run(false).0);
    assert_eq!(optimized, Some(Value::String("130,1032,true,2,3,3".into())));
    assert!(iterations >= 260);
  }
}

#[cfg(test)]
mod native_boundary_tests {
  use super::*;
  use std::cell::RefCell;
  use std::rc::Rc;

  #[test]
  fn typed_native_indices_offsets_and_exceptional_numbers_match_interpreter() {
    let source = crate::js!(
      r#"
      function fill(values, start, step) {
        for (var i = start; i < values.length; i = i + step) values[i] = i / -2;
        return i;
      }
      function read(values) {
        var sum = 0;
        for (var i = 0; i < values.length; i = i + 1) {
          if (values[i] < 0) throw new Error('negative');
          sum = sum + values[i];
        }
        return sum;
      }
      var buffer = new ArrayBuffer(40);
      var offset = new Float64Array(buffer, 8, 3);
      var negative = fill(offset, -1, 1);
      var full = new Float64Array(buffer);
      var fractional = new Float64Array(3);
      var fractionalEnd = fill(fractional, 0.5, 1);
      var empty = fill(new Float64Array(0), 0, 1);
      var nan = read(new Float64Array([NaN, Infinity]));
      var thrown = false;
      try { read(new Float64Array([-1])); } catch (error) { thrown = error.message === 'negative'; }
      globalThis.result = [negative, Object.is(offset[0], -0), offset[2], full[0], full[4],
        fractionalEnd, fractional[1], empty, Number.isNaN(nan), thrown].join(',');
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
        .expect("run");
      vm.get_global("result")
    };
    let actual = run(true);
    assert_eq!(actual, run(false));
    assert_eq!(
      actual,
      Some(Value::String("3,true,-1,0,0,3.5,0,0,true,true".into()))
    );
  }
}

#[cfg(all(test, unix, target_arch = "x86_64"))]
mod packed_backing_tests {
  use super::*;

  #[test]
  fn native_batches_preserve_aliases_cached_values_and_generic_backing() {
    for packed in [true, false] {
      let mut vm = Vm::new(Rc::new(RefCell::new(ObjectData::new())));
      let source = r#"
        var buffer = new ArrayBuffer(8197 * 8);
        var full = new Float64Array(buffer);
        full[0] = 123;
        full[8196] = 456;
        var values = new Float64Array(buffer, 8, 8195);
        function fill(values) {
          for (var i = 0; i < values.length; i = i + 1) values[i] = i / -2;
          return i;
        }
        function sum(values) {
          var total = 0;
          for (var i = 0; i < values.length; i = i + 1) total = total + values[i];
          return total;
        }
      "#;
      let session = crate::parser::Parser::parse_source(source).expect("parse");
      vm.run(crate::compiler::compile_repl_session(&session).expect("compile"))
        .expect("setup");
      let values = vm.get_global("values").expect("view");
      let details = byte_view_details(&values).expect("backing");
      assert_eq!(
        details.data.borrow().get(8),
        Some(&Some(Value::Number(0.0)))
      );
      if !packed {
        details.data.borrow_mut()[8] = Some(Value::Number(0.0));
        assert!(details.data.borrow().packed_bytes().is_none());
      }
      let fill = vm.get_global("fill").expect("fill");
      assert_eq!(
        vm.call_value_with_this(&fill, &Value::Undefined, std::slice::from_ref(&values))
          .expect("fill"),
        Value::Number(8195.0)
      );
      let sum = vm.get_global("sum").expect("sum");
      assert_eq!(
        vm.call_value_with_this(&sum, &Value::Undefined, &[values])
          .expect("sum"),
        Value::Number(-16_787_457.5)
      );
      let data = details.data.borrow();
      assert_eq!(data.packed_bytes().is_some(), packed);
      for (index, expected) in [(0, 123.0f64), (1, -0.0), (8195, -4097.0), (8196, 456.0)] {
        let mut bytes = [0; 8];
        assert!(data.read_bytes(index * 8, &mut bytes));
        assert_eq!(bytes, expected.to_le_bytes());
        for (offset, byte) in bytes.into_iter().enumerate() {
          assert_eq!(
            data.get(index * 8 + offset),
            Some(&Some(Value::Number(f64::from(byte))))
          );
        }
      }
      if packed {
        assert!(
          vm.native_execution_statistics()
            .native_loop_iterations
            >= 16390
        );
      }
    }
  }
}
