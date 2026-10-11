// SPDX-License-Identifier: MPL-2.0
pub(crate) use super::native::numeric_body::NumericBodyFrame;
use super::*;
use crate::vm::region::numeric_body::{NumericBodyProgram, NumericBodyRegion};
use std::sync::Arc;
mod affine;

pub(crate) struct CompiledNumericBody {
  _owner: Arc<Chunk>,
  pub(crate) plan: NumericBodyProgram,
  native: Option<NativeCode>,
}

impl CompiledNumericBody {
  pub(crate) fn execute(
    &self,
    frame: &mut NumericBodyFrame,
  ) {
    frame.iterations = 0;
    frame.completed = 0;
    #[cfg(all(unix, target_arch = "x86_64"))]
    if let Some(native) = &self.native {
      unsafe {
        native.call_numeric_body(frame);
      }
      return;
    }
    for _ in 0..64 {
      for operation in &self.plan.operations[..2] {
        operation.execute(&mut frame.registers, &mut frame.locals);
      }
      if frame.registers[self.plan.comparison.0]
        .partial_cmp(&frame.registers[self.plan.comparison.1])
        != Some(std::cmp::Ordering::Less)
      {
        frame.completed = 1;
        break;
      }
      for operation in &self.plan.operations[2..self.plan.operation_count] {
        operation.execute(&mut frame.registers, &mut frame.locals);
      }
      frame.iterations += 1;
    }
  }
}

impl JitRuntime {
  pub(crate) fn numeric_body(
    &mut self,
    owner: &Arc<Chunk>,
    region: NumericBodyRegion,
  ) -> Option<Rc<CompiledNumericBody>> {
    let key = (Arc::as_ptr(owner) as usize, region.entry_pc);
    if let Some(cached) = self.numeric_bodies.get(&key) {
      return Some(Rc::clone(cached));
    }
    let plan = region.program(owner)?;
    let native =
      NativeCode::numeric_body(&plan.operations[..plan.operation_count], plan.comparison).ok();
    let compiled = Rc::new(CompiledNumericBody {
      _owner: Arc::clone(owner),
      plan,
      native,
    });
    self
      .numeric_bodies
      .insert(key, Rc::clone(&compiled));
    Some(compiled)
  }
}

pub(crate) struct PreparedNumericBody {
  _owner: Arc<Chunk>,
  operations: Vec<crate::vm::region::numeric_body::NumericOperation>,
  comparison: (usize, usize),
  native: Option<NativeCode>,
  affine: Option<affine::AffinePlan>,
}

impl PreparedNumericBody {
  pub(crate) fn execute(
    &self,
    frame: &mut NumericBodyFrame,
  ) {
    frame.iterations = 0;
    frame.completed = 0;
    if self
      .affine
      .as_ref()
      .is_some_and(|plan| plan.execute(frame))
    {
      return;
    }
    #[cfg(all(unix, target_arch = "x86_64"))]
    if let Some(native) = &self.native {
      unsafe {
        native.call_numeric_body(frame);
      }
      return;
    }
    for _ in 0..64 {
      for operation in &self.operations[..2] {
        operation.execute(&mut frame.registers, &mut frame.locals);
      }
      if frame.registers[self.comparison.0].partial_cmp(&frame.registers[self.comparison.1])
        != Some(std::cmp::Ordering::Less)
      {
        frame.completed = 1;
        break;
      }
      for operation in &self.operations[2..] {
        operation.execute(&mut frame.registers, &mut frame.locals);
      }
      frame.iterations += 1;
    }
  }
}

fn same_operation_bits(
  left: &crate::vm::region::numeric_body::NumericOperation,
  right: &crate::vm::region::numeric_body::NumericOperation,
) -> bool {
  use crate::vm::region::numeric_body::NumericOperation as Op;
  match (*left, *right) {
    (Op::Constant(a, x), Op::Constant(b, y)) => a == b && x.to_bits() == y.to_bits(),
    (Op::Load(a, x), Op::Load(b, y))
    | (Op::Store(a, x), Op::Store(b, y))
    | (Op::Copy(a, x), Op::Copy(b, y))
    | (Op::Negate(a, x), Op::Negate(b, y)) => a == b && x == y,
    (Op::Binary(a, x, y), Op::Binary(b, z, w)) => a == b && x == z && y == w,
    _ => false,
  }
}

impl JitRuntime {
  pub(crate) fn prepared_numeric_body(
    &mut self,
    owner: &Arc<Chunk>,
    entry_pc: usize,
    operations: &[crate::vm::region::numeric_body::NumericOperation],
    comparison: (usize, usize),
  ) -> Option<Rc<PreparedNumericBody>> {
    if operations.len() < 2 || operations.len() > 256 || comparison.0 >= 16 || comparison.1 >= 16 {
      return None;
    }
    use crate::vm::region::numeric_body::NumericOperation as Op;
    if operations
      .iter()
      .any(|operation| match *operation {
        Op::Constant(register, _) => register >= 16,
        Op::Load(register, slot)
        | Op::Store(slot, register)
        | Op::Copy(register, slot)
        | Op::Negate(register, slot) => register >= 16 || slot >= 16,
        Op::Binary(operation, left, right) => {
          left >= 16
            || right >= 16
            || !matches!(
              operation,
              Opcode::Add | Opcode::Sub | Opcode::Mul | Opcode::Div | Opcode::Mod
            )
        }
      })
    {
      return None;
    }
    let key = (Arc::as_ptr(owner) as usize, entry_pc);
    if let Some(cached) = self.prepared_numeric_bodies.get(&key)
      && cached.comparison == comparison
      && cached.operations.len() == operations.len()
      && cached
        .operations
        .iter()
        .zip(operations)
        .all(|(left, right)| same_operation_bits(left, right))
    {
      return Some(Rc::clone(cached));
    }
    let native = NativeCode::numeric_body(operations, comparison).ok();
    let compiled = Rc::new(PreparedNumericBody {
      _owner: Arc::clone(owner),
      operations: operations.to_vec(),
      comparison,
      native,
      affine: affine::AffinePlan::decode(operations, comparison),
    });
    self
      .prepared_numeric_bodies
      .insert(key, Rc::clone(&compiled));
    Some(compiled)
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::vm::region::numeric_body::NumericOperation as Op;

  #[test]
  fn prepared_native_cache_tracks_constant_bits_and_replaces_one_site() {
    let session = crate::parser::Parser::parse_source(crate::js!("0;")).expect("parse");
    let owner = Arc::new(crate::compiler::compile_session(&session).expect("compile"));
    let mut runtime = JitRuntime::default();
    let mut operations = [
      Op::Load(0, 0),
      Op::Constant(1, 2.0),
      Op::Constant(2, 1.0),
      Op::Binary(Opcode::Add, 0, 2),
      Op::Store(0, 0),
    ];
    let first = runtime
      .prepared_numeric_body(&owner, 0, &operations, (0, 1))
      .expect("first plan");
    let same = runtime
      .prepared_numeric_body(&owner, 0, &operations, (0, 1))
      .expect("same plan");
    assert!(Rc::ptr_eq(&first, &same));
    operations[2] = Op::Constant(2, 2.0);
    let changed = runtime
      .prepared_numeric_body(&owner, 0, &operations, (0, 1))
      .expect("changed plan");
    assert!(!Rc::ptr_eq(&first, &changed));
    let mut frame = NumericBodyFrame {
      registers: [0.0; 16],
      locals: [0.0; 16],
      iterations: 0,
      completed: 0,
    };
    changed.execute(&mut frame);
    assert_eq!(frame.iterations, 1);
    assert_eq!(frame.locals[0], 2.0);
    operations[1] = Op::Constant(1, f64::NAN);
    let nan = runtime
      .prepared_numeric_body(&owner, 0, &operations, (0, 1))
      .expect("nan plan");
    let same_nan = runtime
      .prepared_numeric_body(&owner, 0, &operations, (0, 1))
      .expect("same nan plan");
    assert!(Rc::ptr_eq(&nan, &same_nan));
    assert_eq!(runtime.prepared_numeric_bodies.len(), 1);
    assert!(!same_operation_bits(
      &Op::Constant(2, 0.0),
      &Op::Constant(2, -0.0)
    ));
  }
}
