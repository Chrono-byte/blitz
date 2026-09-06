// SPDX-License-Identifier: MPL-2.0
//! Exact integer batching for straight-line additive prepared loops.
use super::NumericBodyFrame;
use crate::bytecode::Opcode;
use crate::vm::region::numeric_body::NumericOperation as Op;

#[derive(Clone, Copy)]
struct Expression {
  origin: Option<usize>,
  offset: i64,
}

pub(super) struct AffinePlan {
  intermediates: Vec<Expression>,
  registers: [Option<Expression>; 16],
  locals: [Expression; 16],
  deltas: [i64; 16],
  writes: u16,
  origins: u16,
  header: [(usize, Expression); 2],
  index: usize,
  bound: Expression,
}

fn integer(value: f64) -> Option<i64> {
  (value.is_finite()
    && value.fract() == 0.0
    && value >= f64::from(i32::MIN)
    && value <= f64::from(i32::MAX)
    && value.to_bits() != (-0.0f64).to_bits())
  .then_some(value as i64)
}

impl Expression {
  fn combine(
    self,
    right: Self,
    subtract: bool,
  ) -> Option<Self> {
    let origin = match (self.origin, right.origin) {
      (Some(left), None) => Some(left),
      (None, Some(right)) if !subtract => Some(right),
      (None, None) => None,
      _ => return None,
    };
    Some(Self {
      origin,
      offset: if subtract {
        self.offset.checked_sub(right.offset)?
      } else {
        self.offset.checked_add(right.offset)?
      },
    })
  }
}

impl AffinePlan {
  pub(super) fn decode(
    operations: &[Op],
    comparison: (usize, usize),
  ) -> Option<Self> {
    let mut locals = std::array::from_fn(|origin| Expression {
      origin: Some(origin),
      offset: 0,
    });
    let mut registers = [None; 16];
    let mut intermediates = Vec::with_capacity(operations.len());
    let mut writes = 0u16;
    let mut header = [(
      0,
      Expression {
        origin: None,
        offset: 0,
      },
    ); 2];
    for (position, operation) in operations.iter().copied().enumerate() {
      let (destination, expression) = match operation {
        Op::Constant(register, value) => (
          Some(register),
          Expression {
            origin: None,
            offset: integer(value)?,
          },
        ),
        Op::Load(register, local) => (Some(register), locals[local]),
        Op::Store(local, register) => {
          let value = registers[register]?;
          locals[local] = value;
          writes |= 1 << local;
          (None, value)
        }
        Op::Copy(register, source) => (Some(register), registers[source]?),
        Op::Binary(op, register, source) if matches!(op, Opcode::Add | Opcode::Sub) => (
          Some(register),
          registers[register]?.combine(registers[source]?, op == Opcode::Sub)?,
        ),
        _ => return None,
      };
      intermediates.push(expression);
      if let Some(register) = destination {
        registers[register] = Some(expression);
      }
      if position < 2 {
        if !matches!(operation, Op::Load(..) | Op::Constant(..)) {
          return None;
        }
        header[position] = (destination?, expression);
      }
    }
    if operations.len() < 2 || header[0].0 != comparison.0 || header[1].0 != comparison.1 {
      return None;
    }
    let index = header[0].1.origin?;
    if header[0].1.offset != 0 {
      return None;
    }
    let mut deltas = [0; 16];
    for (local, expression) in locals.iter().enumerate() {
      if expression.origin != Some(local) {
        return None;
      }
      deltas[local] = expression.offset;
    }
    if deltas[index] <= 0 {
      return None;
    }
    let bound = header[1].1;
    if bound
      .origin
      .is_some_and(|origin| deltas[origin] != 0)
    {
      return None;
    }
    let origins = intermediates
      .iter()
      .filter_map(|expression| expression.origin)
      .fold(0u16, |mask, origin| mask | (1 << origin));
    Some(Self {
      intermediates,
      registers,
      locals,
      deltas,
      writes,
      origins,
      header,
      index,
      bound,
    })
  }

  pub(super) fn execute(
    &self,
    frame: &mut NumericBodyFrame,
  ) -> bool {
    let mut initial = [0i64; 16];
    for (index, value) in initial.iter_mut().enumerate() {
      if self.origins & (1 << index) != 0 {
        let Some(number) = integer(frame.locals[index]) else {
          return false;
        };
        *value = number;
      }
    }
    let value_at = |expression: Expression, iteration: u32| -> Option<i64> {
      let value = expression.origin.map_or(0, |origin| {
        i128::from(initial[origin]) + i128::from(iteration) * i128::from(self.deltas[origin])
      }) + i128::from(expression.offset);
      i32::try_from(value).ok().map(i64::from)
    };
    let Some(bound) = value_at(self.bound, 0) else {
      return false;
    };
    let distance = bound - initial[self.index];
    let iterations = if distance <= 0 {
      0
    } else {
      ((distance - 1) / self.deltas[self.index] + 1).min(64) as u32
    };
    let last = iterations.saturating_sub(1);
    // Every expression is affine in iteration number. Endpoint bounds therefore
    // prove every intermediate lies in i32, where binary64 Add/Sub are exact.
    if self
      .intermediates
      .iter()
      .any(|expression| value_at(*expression, 0).is_none() || value_at(*expression, last).is_none())
    {
      return false;
    }
    let completed = iterations < 64;
    let mut registers = frame.registers;
    let mut locals = frame.locals;
    if iterations > 0 {
      for (index, expression) in self.registers.iter().enumerate() {
        if let Some(expression) = expression {
          let Some(value) = value_at(*expression, last) else {
            return false;
          };
          registers[index] = value as f64;
        }
      }
      for (index, expression) in self.locals.iter().enumerate() {
        if self.writes & (1 << index) != 0 {
          let Some(value) = value_at(*expression, last) else {
            return false;
          };
          locals[index] = value as f64;
        }
      }
    }
    if completed {
      for (register, expression) in self.header {
        let Some(value) = value_at(expression, iterations) else {
          return false;
        };
        registers[register] = value as f64;
      }
    }
    frame.locals = locals;
    frame.registers = registers;
    frame.iterations = iterations;
    frame.completed = u32::from(completed);
    true
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn portable(
    operations: &[Op],
    frame: &mut NumericBodyFrame,
  ) {
    frame.iterations = 0;
    frame.completed = 0;
    for _ in 0..64 {
      for operation in &operations[..2] {
        operation.execute(&mut frame.registers, &mut frame.locals);
      }
      if frame.registers[0].partial_cmp(&frame.registers[1]) != Some(std::cmp::Ordering::Less) {
        frame.completed = 1;
        break;
      }
      for operation in &operations[2..] {
        operation.execute(&mut frame.registers, &mut frame.locals);
      }
      frame.iterations += 1;
    }
  }

  #[test]
  fn additive_batch_matches_every_register_and_local_at_bounded_exits() {
    for start in [-5.0, 0.0, 5.0] {
      for bound in [-10.0, 0.0, 1.0, 5.0, 64.0, 65.0, 130.0] {
        for step in [1.0, 2.0, 5.0] {
          let operations = [
            Op::Load(0, 0),
            Op::Load(1, 3),
            Op::Load(2, 1),
            Op::Constant(3, 7.0),
            Op::Binary(Opcode::Add, 2, 3),
            Op::Store(1, 2),
            Op::Load(4, 2),
            Op::Constant(5, 3.0),
            Op::Binary(Opcode::Sub, 4, 5),
            Op::Store(2, 4),
            Op::Load(6, 0),
            Op::Constant(7, step),
            Op::Binary(Opcode::Add, 6, 7),
            Op::Store(0, 6),
          ];
          let plan = AffinePlan::decode(&operations, (0, 1)).expect("affine plan");
          let mut actual = NumericBodyFrame {
            registers: [99.0; 16],
            locals: [123.0; 16],
            iterations: 0,
            completed: 0,
          };
          actual.locals[0] = start;
          actual.locals[3] = bound;
          let mut expected = NumericBodyFrame {
            registers: actual.registers,
            locals: actual.locals,
            iterations: 0,
            completed: 0,
          };
          assert!(plan.execute(&mut actual));
          portable(&operations, &mut expected);
          assert_eq!(actual.iterations, expected.iterations);
          assert_eq!(actual.completed, expected.completed);
          for (left, right) in actual
            .registers
            .iter()
            .chain(&actual.locals)
            .zip(expected.registers.iter().chain(&expected.locals))
          {
            assert_eq!(
              left.to_bits(),
              right.to_bits(),
              "start={start} bound={bound} step={step}"
            );
          }
        }
      }
    }
  }

  #[test]
  fn additive_batch_declines_rounding_negative_zero_and_intermediate_overflow() {
    let operations = [
      Op::Load(0, 0),
      Op::Constant(1, 1.0),
      Op::Load(2, 1),
      Op::Constant(3, 1.0),
      Op::Binary(Opcode::Add, 2, 3),
      Op::Binary(Opcode::Sub, 2, 3),
      Op::Store(1, 2),
      Op::Binary(Opcode::Add, 0, 3),
      Op::Store(0, 0),
    ];
    let plan = AffinePlan::decode(&operations, (0, 1)).expect("affine plan");
    for accumulator in [-0.0, 1.5, f64::NAN, f64::INFINITY, f64::from(i32::MAX)] {
      let mut frame = NumericBodyFrame {
        registers: [77.0; 16],
        locals: [0.0; 16],
        iterations: 0,
        completed: 0,
      };
      frame.locals[1] = accumulator;
      assert!(!plan.execute(&mut frame));
      assert_eq!(frame.registers, [77.0; 16]);
      assert_eq!(frame.locals[1].to_bits(), accumulator.to_bits());
    }
    let mut invalid = operations;
    invalid[3] = Op::Constant(3, -0.0);
    assert!(AffinePlan::decode(&invalid, (0, 1)).is_none());
    invalid = operations;
    invalid[6] = Op::Store(2, 2);
    assert!(AffinePlan::decode(&invalid, (0, 1)).is_none());
  }
}
