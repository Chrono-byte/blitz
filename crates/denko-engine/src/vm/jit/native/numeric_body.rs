// SPDX-License-Identifier: MPL-2.0
use super::NativeCode;
use crate::bytecode::Opcode;
use crate::vm::region::numeric_body::NumericOperation;

#[repr(C)]
pub(crate) struct NumericBodyFrame {
  pub(crate) registers: [f64; 16],
  pub(crate) locals: [f64; 16],
  pub(crate) iterations: u32,
  pub(crate) completed: u32,
}

const _: () = {
  assert!(std::mem::offset_of!(NumericBodyFrame, registers) == 0);
  assert!(std::mem::offset_of!(NumericBodyFrame, locals) == 128);
  assert!(std::mem::offset_of!(NumericBodyFrame, iterations) == 256);
  assert!(std::mem::offset_of!(NumericBodyFrame, completed) == 260);
};

// This helper has no VM access, allocation, coercion, or observable side effects.
// Rust f64 remainder preserves ECMAScript Number remainder including -0/NaN.
extern "C" fn numeric_remainder(
  left: f64,
  right: f64,
) -> f64 {
  left % right
}

#[cfg(all(unix, target_arch = "x86_64"))]
pub(super) struct Emitter {
  pub(super) bytes: Vec<u8>,
}

#[cfg(all(unix, target_arch = "x86_64"))]
impl Emitter {
  pub(super) fn memory(
    &mut self,
    opcode: u8,
    xmm: u8,
    offset: usize,
  ) {
    // movsd xmm, [r12 + disp32], or the corresponding store.
    self
      .bytes
      .extend([0xf2, 0x41, 0x0f, opcode, 0x84 | (xmm << 3), 0x24]);
    self.bytes.extend((offset as u32).to_le_bytes());
  }
  pub(super) fn immediate(
    &mut self,
    xmm: u8,
    bits: u64,
  ) {
    self.bytes.extend([0x48, 0xb8]);
    self.bytes.extend(bits.to_le_bytes());
    self
      .bytes
      .extend([0x66, 0x48, 0x0f, 0x6e, 0xc0 | (xmm << 3)]);
  }
  pub(super) fn jump(
    &mut self,
    condition: u8,
  ) -> usize {
    self.bytes.extend([0x0f, condition]);
    let offset = self.bytes.len();
    self.bytes.extend([0; 4]);
    offset
  }
  pub(super) fn patch(
    &mut self,
    offset: usize,
    target: usize,
  ) {
    let relative = i32::try_from(target as i64 - (offset + 4) as i64).expect("bounded native body");
    self.bytes[offset..offset + 4].copy_from_slice(&relative.to_le_bytes());
  }
  pub(super) fn operation(
    &mut self,
    operation: NumericOperation,
  ) -> Result<(), String> {
    match operation {
      NumericOperation::Constant(rd, value) => {
        self.immediate(0, value.to_bits());
        self.memory(0x11, 0, rd * 8);
      }
      NumericOperation::Load(rd, slot) => {
        self.memory(0x10, 0, 128 + slot * 8);
        self.memory(0x11, 0, rd * 8);
      }
      NumericOperation::Store(slot, rd) => {
        self.memory(0x10, 0, rd * 8);
        self.memory(0x11, 0, 128 + slot * 8);
      }
      NumericOperation::Copy(rd, rs) => {
        self.memory(0x10, 0, rs * 8);
        self.memory(0x11, 0, rd * 8);
      }
      NumericOperation::Negate(rd, rs) => {
        self.memory(0x10, 0, rs * 8);
        self.immediate(1, 1 << 63);
        self.bytes.extend([0x66, 0x0f, 0xef, 0xc1]);
        self.memory(0x11, 0, rd * 8);
      }
      NumericOperation::Binary(op, rd, rs) => {
        self.memory(0x10, 0, rd * 8);
        self.memory(0x10, 1, rs * 8);
        if op == Opcode::Mod {
          self.bytes.extend([0x48, 0xb8]);
          self
            .bytes
            .extend((numeric_remainder as *const () as usize as u64).to_le_bytes());
          self.bytes.extend([0xff, 0xd0]);
        } else {
          let opcode = match op {
            Opcode::Add => 0x58,
            Opcode::Sub => 0x5c,
            Opcode::Mul => 0x59,
            Opcode::Div => 0x5e,
            _ => return Err("unsupported numeric body operation".into()),
          };
          self.bytes.extend([0xf2, 0x0f, opcode, 0xc1]);
        }
        self.memory(0x11, 0, rd * 8);
      }
    }
    Ok(())
  }
}

impl NativeCode {
  #[cfg(all(unix, target_arch = "x86_64"))]
  pub(crate) fn numeric_body(
    operations: &[NumericOperation],
    comparison: (usize, usize),
  ) -> Result<Self, String> {
    if operations.len() < 2 || operations.len() > 256 || comparison.0 >= 16 || comparison.1 >= 16 {
      return Err("invalid numeric body bounds".into());
    }
    // Validate every address before emitting code that accesses the compact frame.
    for operation in operations {
      let (left, right) = match *operation {
        NumericOperation::Constant(rd, _) => (rd, 0),
        NumericOperation::Load(rd, slot)
        | NumericOperation::Copy(rd, slot)
        | NumericOperation::Negate(rd, slot)
        | NumericOperation::Binary(_, rd, slot) => (rd, slot),
        NumericOperation::Store(slot, rd) => (slot, rd),
      };
      if left >= 16 || right >= 16 {
        return Err("numeric body operand out of bounds".into());
      }
    }
    let mut out = Emitter {
      bytes: vec![0x41, 0x54, 0x49, 0x89, 0xfc],
    }; // preserve r12; r12 = frame
    let entry = out.bytes.len();
    for operation in &operations[..2] {
      out.operation(*operation)?;
    }
    out.memory(0x10, 0, comparison.0 * 8);
    out.memory(0x10, 1, comparison.1 * 8);
    out.bytes.extend([0x66, 0x0f, 0x2e, 0xc1]); // unordered is also a completed comparison
    let unordered = out.jump(0x8a);
    let completed = out.jump(0x83);
    for operation in &operations[2..] {
      out.operation(*operation)?;
    }
    out.bytes.extend([0x41, 0xff, 0x84, 0x24]);
    out.bytes.extend(256u32.to_le_bytes());
    out.bytes.extend([0x41, 0x83, 0xbc, 0x24]);
    out.bytes.extend(256u32.to_le_bytes());
    out.bytes.push(64);
    let backedge = out.jump(0x85);
    out.patch(backedge, entry);
    out.bytes.extend([0x41, 0x5c, 0xc3]);
    let exit = out.bytes.len();
    out.patch(unordered, exit);
    out.patch(completed, exit);
    out.bytes.extend([0x41, 0xc7, 0x84, 0x24]);
    out.bytes.extend(260u32.to_le_bytes());
    out.bytes.extend(1u32.to_le_bytes());
    out.bytes.extend([0x41, 0x5c, 0xc3]);
    Self::from_bytes(&out.bytes, &[])
  }

  #[cfg(not(all(unix, target_arch = "x86_64")))]
  pub(crate) fn numeric_body(
    _operations: &[NumericOperation],
    _comparison: (usize, usize),
  ) -> Result<Self, String> {
    Err("native numeric bodies unavailable on this target".into())
  }

  /// Caller must use code created by `numeric_body` and initialize iterations and
  /// completed to zero. The frame remains exclusively borrowed for the call.
  #[cfg(all(unix, target_arch = "x86_64"))]
  pub(crate) unsafe fn call_numeric_body(
    &self,
    frame: &mut NumericBodyFrame,
  ) {
    let entry: unsafe extern "C" fn(*mut NumericBodyFrame) =
      unsafe { std::mem::transmute(self.ptr.as_ptr()) };
    unsafe { entry(frame) };
  }
}

#[cfg(all(test, unix, target_arch = "x86_64"))]
mod tests {
  use super::*;

  #[test]
  fn emitted_numeric_body_matches_ordered_operations_and_bounded_exits() {
    use NumericOperation::{Binary, Constant, Copy, Load, Negate, Store};
    let operations = [
      Load(0, 0),
      Constant(1, 65.0),
      Load(2, 1),
      Constant(3, -2.0),
      Binary(Opcode::Div, 2, 3),
      Store(1, 2),
      Load(4, 2),
      Constant(5, 3.0),
      Binary(Opcode::Mod, 4, 5),
      Store(2, 4),
      Negate(8, 4),
      Copy(9, 8),
      Store(3, 9),
      Load(6, 0),
      Constant(7, 1.0),
      Binary(Opcode::Add, 6, 7),
      Store(0, 6),
    ];
    let code = NativeCode::numeric_body(&operations, (0, 1)).expect("compile");
    for value in [
      -0.0,
      0.0,
      -17.0,
      23.5,
      f64::INFINITY,
      f64::NEG_INFINITY,
      f64::NAN,
    ] {
      let mut actual = NumericBodyFrame {
        registers: [0.0; 16],
        locals: [0.0; 16],
        iterations: 0,
        completed: 0,
      };
      actual.locals[1] = value;
      actual.locals[2] = value;
      let mut expected = NumericBodyFrame {
        registers: actual.registers,
        locals: actual.locals,
        iterations: 0,
        completed: 0,
      };
      for batch in 0..2 {
        actual.iterations = 0;
        actual.completed = 0;
        expected.iterations = 0;
        expected.completed = 0;
        for _ in 0..64 {
          for operation in &operations[..2] {
            operation.execute(&mut expected.registers, &mut expected.locals);
          }
          if expected.registers[0].partial_cmp(&expected.registers[1])
            != Some(std::cmp::Ordering::Less)
          {
            expected.completed = 1;
            break;
          }
          for operation in &operations[2..] {
            operation.execute(&mut expected.registers, &mut expected.locals);
          }
          expected.iterations += 1;
        }
        // SAFETY: matching emitted frame ABI and reset iteration fields.
        unsafe {
          code.call_numeric_body(&mut actual);
        }
        assert_eq!(actual.iterations, if batch == 0 { 64 } else { 1 });
        assert_eq!(actual.completed, expected.completed);
        for (left, right) in actual
          .registers
          .iter()
          .chain(&actual.locals)
          .zip(expected.registers.iter().chain(&expected.locals))
        {
          assert!(
            left.is_nan() && right.is_nan() || left.to_bits() == right.to_bits(),
            "{left:?} versus {right:?}"
          );
        }
      }
    }
  }

  #[test]
  fn emitted_numeric_body_rejects_out_of_bounds_operands() {
    assert!(
      NativeCode::numeric_body(
        &[
          NumericOperation::Load(0, 16),
          NumericOperation::Constant(1, 2.0)
        ],
        (0, 1)
      )
      .is_err()
    );
  }
}
