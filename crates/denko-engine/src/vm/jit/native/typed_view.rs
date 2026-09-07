// SPDX-License-Identifier: MPL-2.0
use super::NativeCode;
use crate::vm::region::typed_view::{TypedViewOperation as Op, TypedViewProgram};

#[repr(C)]
pub(crate) struct TypedViewFrame {
  pub registers: [f64; 16],
  pub locals: [f64; 16],
  saved_registers: [f64; 16],
  saved_locals: [f64; 16],
  elements: *mut u8,
  pub length: f64,
  pub iterations: u32,
  pub exit: u32,
  pub dirty_start: usize,
  pub dirty_end: usize,
  elements_len: usize,
}
impl TypedViewFrame {
  pub(crate) fn new(
    locals: [f64; 16],
    length: usize,
  ) -> Self {
    Self {
      registers: [0.0; 16],
      locals,
      saved_registers: [0.0; 16],
      saved_locals: [0.0; 16],
      elements: std::ptr::null_mut(),
      length: length as f64,
      iterations: 0,
      exit: 0,
      dirty_start: usize::MAX,
      dirty_end: 0,
      elements_len: 0,
    }
  }
}
#[cfg(all(unix, target_arch = "x86_64"))]
const _: () = {
  assert!(std::mem::offset_of!(TypedViewFrame, saved_registers) == 256);
  assert!(std::mem::offset_of!(TypedViewFrame, elements) == 512);
  assert!(std::mem::offset_of!(TypedViewFrame, iterations) == 528);
  assert!(std::mem::offset_of!(TypedViewFrame, exit) == 532);
  assert!(std::mem::offset_of!(TypedViewFrame, dirty_start) == 536);
  assert!(std::mem::offset_of!(TypedViewFrame, dirty_end) == 544);
  assert!(std::mem::offset_of!(TypedViewFrame, elements_len) == 552);
};

impl NativeCode {
  #[cfg(all(unix, target_arch = "x86_64"))]
  #[allow(clippy::too_many_lines)]
  pub(crate) fn typed_view(plan: &TypedViewProgram) -> Result<Self, String> {
    use super::numeric_body::Emitter;
    use crate::vm::region::numeric_body::NumericOperation;
    if plan.operations.len() < 5
      || plan.operations.len() > 128
      || plan.comparison.0 >= 16
      || plan.comparison.1 >= 16
      || plan.locals.len() > 16
    {
      return Err("invalid typed view program bounds".into());
    }
    let mut touched = [false; 32];
    for operation in &plan.operations {
      let (left, right, third, changed) = match *operation {
        Op::Numeric(operation, _) => match operation {
          NumericOperation::Constant(rd, _) => (rd, 0, 0, Some(rd)),
          NumericOperation::Load(rd, source)
          | NumericOperation::Copy(rd, source)
          | NumericOperation::Negate(rd, source)
          | NumericOperation::Binary(_, rd, source) => (rd, source, 0, Some(rd)),
          NumericOperation::Store(slot, source) => {
            if slot >= 16 {
              return Err("typed view local out of bounds".into());
            }
            (slot, source, 0, Some(16 + slot))
          }
        },
        Op::Length(rd) => (rd, 0, 0, Some(rd)),
        Op::Read(rd, index) => (rd, index, 0, Some(rd)),
        Op::Write(rd, index, value) => (rd, index, value, Some(rd)),
        Op::Guard(_, left, right) => (left, right, 0, None),
        Op::View(rd) | Op::Key(rd) => (rd, 0, 0, None),
      };
      if left >= 16 || right >= 16 || third >= 16 {
        return Err("typed view operand out of bounds".into());
      }
      if let Some(changed) = changed {
        touched[changed] = true;
      }
    }
    // Failed guards restore the start of the iteration. Only cells that the
    // decoded operations can overwrite need snapshots; view/key markers do not
    // write the numeric frame at all.
    let backup_offsets: Vec<_> = touched
      .iter()
      .enumerate()
      .filter_map(|(index, touched)| touched.then_some(index * 8))
      .collect();
    let mut out = Emitter {
      bytes: vec![0x41, 0x54, 0x49, 0x89, 0xfc],
    };
    let mut failures = Vec::new();
    let mut completed_exits = Vec::new();
    let entry = out.bytes.len();
    for offset in backup_offsets.iter().copied() {
      out.memory(0x10, 0, offset);
      out.memory(0x11, 0, 256 + offset);
    }
    for (position, operation) in plan.operations.iter().copied().enumerate() {
      if position == 4 {
        out.memory(0x10, 0, plan.comparison.0 * 8);
        out.memory(0x10, 1, plan.comparison.1 * 8);
        out.bytes.extend([0x66, 0x0f, 0x2e, 0xc1]);
        completed_exits.push(out.jump(0x8a));
        completed_exits.push(out.jump(0x83));
      }
      match operation {
        Op::Numeric(operation, _) => out.operation(operation)?,
        Op::View(_) | Op::Key(_) => {}
        Op::Length(rd) => {
          out.memory(0x10, 0, 520);
          out.memory(0x11, 0, rd * 8);
        }
        Op::Guard(condition, left, right) => {
          out.memory(0x10, 0, left * 8);
          out.memory(0x10, 1, right * 8);
          out.bytes.extend([0x66, 0x0f, 0x2e, 0xc1]);
          let mut ordered_skip = None;
          if matches!(condition, 0 | 6 | 2 | 3) {
            ordered_skip = Some(out.jump(0x8a));
          }
          if matches!(condition, 1 | 7) {
            failures.push(out.jump(0x8a));
          }
          let jump = match condition {
            0 | 6 => 0x84,
            1 | 7 => 0x85,
            2 => 0x82,
            3 => 0x86,
            4 => 0x87,
            5 => 0x83,
            _ => return Err("invalid typed view guard".into()),
          };
          failures.push(out.jump(jump));
          if let Some(skip) = ordered_skip {
            let end = out.bytes.len();
            out.patch(skip, end);
          }
        }
        Op::Read(rd, index) | Op::Write(rd, index, _) => {
          out.memory(0x10, 0, index * 8);
          out.immediate(1, 0);
          out.bytes.extend([0x66, 0x0f, 0x2e, 0xc1]); // index >= 0, ordered
          failures.push(out.jump(0x8a));
          failures.push(out.jump(0x82));
          out.memory(0x10, 1, 520);
          out.bytes.extend([0x66, 0x0f, 0x2e, 0xc1]);
          failures.push(out.jump(0x83));
          out.bytes.extend([0xf2, 0x48, 0x0f, 0x2c, 0xc0]); // cvttsd2si rax,xmm0
          out.bytes.extend([0xf2, 0x48, 0x0f, 0x2a, 0xc8]); // cvtsi2sd xmm1,rax
          out.bytes.extend([0x66, 0x0f, 0x2e, 0xc1]);
          failures.push(out.jump(0x85));
          out.bytes.extend([0x49, 0x3b, 0x84, 0x24]);
          out.bytes.extend(552u32.to_le_bytes());
          failures.push(out.jump(0x83));
          out.bytes.extend([0x49, 0x8b, 0x94, 0x24]);
          out.bytes.extend(512u32.to_le_bytes()); // rdx=elements
          if let Op::Write(_, _, source) = operation {
            out.memory(0x10, 0, source * 8);
            out.bytes.extend([0xf2, 0x0f, 0x11, 0x04, 0xc2]);
            out.bytes.extend([0x49, 0x3b, 0x84, 0x24]);
            out.bytes.extend(536u32.to_le_bytes());
            let keep_min = out.jump(0x83);
            out.bytes.extend([0x49, 0x89, 0x84, 0x24]);
            out.bytes.extend(536u32.to_le_bytes());
            let end = out.bytes.len();
            out.patch(keep_min, end);
            out.bytes.extend([0x48, 0xff, 0xc0]);
            out.bytes.extend([0x49, 0x3b, 0x84, 0x24]);
            out.bytes.extend(544u32.to_le_bytes());
            let keep_max = out.jump(0x86);
            out.bytes.extend([0x49, 0x89, 0x84, 0x24]);
            out.bytes.extend(544u32.to_le_bytes());
            let end = out.bytes.len();
            out.patch(keep_max, end);
          } else {
            out.bytes.extend([0xf2, 0x0f, 0x10, 0x04, 0xc2]);
          }
          out.memory(0x11, 0, rd * 8);
        }
      }
    }
    out.bytes.extend([0x41, 0xff, 0x84, 0x24]);
    out.bytes.extend(528u32.to_le_bytes());
    out.bytes.extend([0x41, 0x83, 0xbc, 0x24]);
    out.bytes.extend(528u32.to_le_bytes());
    out.bytes.push(64);
    let backedge = out.jump(0x85);
    out.patch(backedge, entry);
    out.bytes.extend([0x41, 0x5c, 0xc3]);
    let complete = out.bytes.len();
    for jump in completed_exits {
      out.patch(jump, complete);
    }
    out.bytes.extend([0x41, 0xc7, 0x84, 0x24]);
    out.bytes.extend(532u32.to_le_bytes());
    out.bytes.extend(1u32.to_le_bytes());
    out.bytes.extend([0x41, 0x5c, 0xc3]);
    let failure = out.bytes.len();
    for jump in failures {
      out.patch(jump, failure);
    }
    for offset in backup_offsets.iter().copied() {
      out.memory(0x10, 0, 256 + offset);
      out.memory(0x11, 0, offset);
    }
    out.bytes.extend([0x41, 0xc7, 0x84, 0x24]);
    out.bytes.extend(532u32.to_le_bytes());
    out.bytes.extend(2u32.to_le_bytes());
    out.bytes.extend([0x41, 0x5c, 0xc3]);
    Self::from_bytes(&out.bytes, &[])
  }
  #[cfg(not(all(unix, target_arch = "x86_64")))]
  pub(crate) fn typed_view(_plan: &TypedViewProgram) -> Result<Self, String> {
    Err("native typed view unavailable".into())
  }

  #[cfg(all(unix, target_arch = "x86_64"))]
  pub(crate) unsafe fn call_typed_view(
    &self,
    frame: &mut TypedViewFrame,
    elements: &mut [u8],
  ) {
    frame.elements = elements.as_mut_ptr();
    frame.elements_len = elements.len() / 8;
    frame.iterations = 0;
    frame.exit = 0;
    frame.dirty_start = usize::MAX;
    frame.dirty_end = 0;
    let entry: unsafe extern "C" fn(*mut TypedViewFrame) =
      unsafe { std::mem::transmute(self.ptr.as_ptr()) };
    unsafe { entry(frame) };
  }
}
