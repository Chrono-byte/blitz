// SPDX-License-Identifier: MPL-2.0
//! Minimal executable-code proof for the baseline JIT boundary.
//!
//! This intentionally supports only a host function returning a u64 constant.
//! It is not connected to JavaScript execution yet; the boundary is validated
//! before VM frames and Value ABI are exposed to generated code.

use crate::value::Value;
use std::ptr::NonNull;

#[repr(C)]
pub(crate) struct JitFrame {
  pub(crate) vm: *mut crate::vm::state::Vm,
  pub(crate) function_id: u64,
  pub(crate) vm_pc: usize,
  pub(crate) registers: *mut Value,
  pub(crate) register_count: usize,
  pub(crate) result: *mut Value,
  /// Prepared call arguments. Direct entry uses these before VM local cells exist.
  pub(crate) arguments: *const Value,
  pub(crate) argument_count: usize,
  /// Opaque pointer to the VM-owned `Vec<SharedValue>` for helper-only access.
  pub(crate) locals: *mut std::ffi::c_void,
  pub(crate) local_count: usize,
}

pub(crate) type JitExitCode = u32;
pub(crate) const JIT_EXIT_RETURN: JitExitCode = 0;
pub(crate) const JIT_EXIT_INTERPRETER: JitExitCode = 1;
pub(crate) const JIT_EXIT_EXCEPTION: JitExitCode = 2;
pub(crate) const JIT_EXIT_INVALIDATED: JitExitCode = 3;
pub(crate) const JIT_EXIT_NEXT_BLOCK: JitExitCode = 4;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum JitExit {
  Return,
  ResumeInterpreter,
  Exception,
  Invalidated,
  NextBlock,
}

pub(crate) fn decode_exit(code: JitExitCode) -> Result<JitExit, String> {
  match code {
    JIT_EXIT_RETURN => Ok(JitExit::Return),
    JIT_EXIT_INTERPRETER => Ok(JitExit::ResumeInterpreter),
    JIT_EXIT_EXCEPTION => Ok(JitExit::Exception),
    JIT_EXIT_INVALIDATED => Ok(JitExit::Invalidated),
    JIT_EXIT_NEXT_BLOCK => Ok(JitExit::NextBlock),
    _ => Err(format!("invalid baseline JIT exit code {code}")),
  }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RuntimeHelperId {
  SetNumberResult,
  MaterializeNumberRegister,
  SetResumePc,
  SetNextBlock,
  SetResultFromRegister,
  SelectConditionalBlock,
  SetResultFromLocal,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Relocation {
  pub(crate) offset: u32,
  pub(crate) helper: RuntimeHelperId,
}

#[unsafe(no_mangle)]
pub(crate) unsafe extern "C" fn denko_jit_set_number_result(
  frame: *mut JitFrame,
  value: f64,
) {
  // SAFETY: generated code receives a frame whose result points to VM-owned
  // Value storage for the duration of the call.
  unsafe { *(*frame).result = Value::Number(value) };
}

#[unsafe(no_mangle)]
pub(crate) unsafe extern "C" fn denko_jit_materialize_number_register(
  frame: *mut JitFrame,
  register: u32,
  value: f64,
) {
  // SAFETY: generated code passes a valid frame and a checked register index.
  unsafe {
    if register as usize >= (*frame).register_count {
      return;
    }
    *(*frame).registers.add(register as usize) = Value::Number(value);
  }
}

#[unsafe(no_mangle)]
pub(crate) unsafe extern "C" fn denko_jit_set_resume_pc(
  frame: *mut JitFrame,
  vm_pc: u32,
) -> JitExitCode {
  // SAFETY: generated code passes a valid frame for the duration of the call.
  unsafe { (*frame).vm_pc = vm_pc as usize };
  JIT_EXIT_INTERPRETER
}

#[unsafe(no_mangle)]
pub(crate) unsafe extern "C" fn denko_jit_set_result_from_local(
  frame: *mut JitFrame,
  slot: u32,
) -> JitExitCode {
  unsafe {
    if !(*frame).locals.is_null() && (slot as usize) < (*frame).local_count {
      let locals = &*((*frame)
        .locals
        .cast::<Vec<crate::value::SharedValue>>());
      *(*frame).result = locals[slot as usize].borrow().clone();
      return JIT_EXIT_RETURN;
    }
    if (*frame).arguments.is_null() || slot as usize >= (*frame).argument_count {
      return JIT_EXIT_EXCEPTION;
    }
    *(*frame).result = (*(*frame).arguments.add(slot as usize)).clone();
  }
  JIT_EXIT_RETURN
}

#[unsafe(no_mangle)]
pub(crate) unsafe extern "C" fn denko_jit_select_conditional_block(
  frame: *mut JitFrame,
  register: u32,
  condition: u32,
  taken_pc: u32,
  fallthrough_pc: u32,
) -> JitExitCode {
  unsafe {
    if register as usize >= (*frame).register_count {
      return JIT_EXIT_EXCEPTION;
    }
    let value = &*(*frame).registers.add(register as usize);
    let take = match condition {
      0 => !value.is_truthy(),
      1 => value.is_truthy(),
      2 => matches!(value, Value::Null | Value::Undefined),
      _ => return JIT_EXIT_EXCEPTION,
    };
    (*frame).vm_pc = if take { taken_pc } else { fallthrough_pc } as usize;
  }
  JIT_EXIT_NEXT_BLOCK
}

#[unsafe(no_mangle)]
pub(crate) unsafe extern "C" fn denko_jit_set_result_from_register(
  frame: *mut JitFrame,
  register: u32,
) -> JitExitCode {
  unsafe {
    if register as usize >= (*frame).register_count {
      return JIT_EXIT_EXCEPTION;
    }
    *(*frame).result = (*(*frame).registers.add(register as usize)).clone();
  }
  JIT_EXIT_RETURN
}

#[unsafe(no_mangle)]
pub(crate) unsafe extern "C" fn denko_jit_set_next_block(
  frame: *mut JitFrame,
  vm_pc: u32,
) -> JitExitCode {
  unsafe { (*frame).vm_pc = vm_pc as usize };
  JIT_EXIT_NEXT_BLOCK
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PatchSite {
  offset: u32,
}

#[derive(Debug)]
pub(crate) struct NativeCode {
  ptr: NonNull<u8>,
  len: usize,
}

impl NativeCode {
  pub(crate) fn entry_address(&self) -> *const u8 {
    self.ptr.as_ptr()
  }

  #[cfg(all(unix, target_arch = "x86_64"))]
  pub(crate) fn patchable_jump() -> Result<(Self, PatchSite), String> {
    // movabs rax, target; jmp rax
    let mut bytes = vec![0x48, 0xB8];
    let site = PatchSite {
      offset: bytes.len() as u32,
    };
    bytes.extend_from_slice(&[0; 8]);
    bytes.extend_from_slice(&[0xFF, 0xE0]);
    Ok((Self::from_bytes(&bytes, &[])?, site))
  }

  #[cfg(all(unix, target_arch = "x86_64"))]
  pub(crate) fn patch_jump(
    &mut self,
    site: PatchSite,
    target: *const u8,
  ) -> Result<(), String> {
    let start = site.offset as usize;
    if start
      .checked_add(8)
      .is_none_or(|end| end > self.len)
    {
      return Err("native jump patch is out of bounds".to_string());
    }
    if unsafe {
      libc::mprotect(
        self.ptr.as_ptr().cast(),
        self.len,
        libc::PROT_READ | libc::PROT_WRITE,
      )
    } != 0
    {
      return Err(std::io::Error::last_os_error().to_string());
    }
    unsafe {
      std::ptr::copy_nonoverlapping(
        (target as usize as u64).to_le_bytes().as_ptr(),
        self.ptr.as_ptr().add(start),
        8,
      );
    }
    if unsafe {
      libc::mprotect(
        self.ptr.as_ptr().cast(),
        self.len,
        libc::PROT_READ | libc::PROT_EXEC,
      )
    } != 0
    {
      return Err(std::io::Error::last_os_error().to_string());
    }
    Ok(())
  }

  #[cfg(all(unix, target_arch = "x86_64"))]
  pub(crate) fn return_number(value: f64) -> Result<Self, String> {
    // SysV entry has rsp % 16 == 8. Subtracting eight aligns the helper call.
    // movabs rax, value; movq xmm0, rax; movabs rax, helper; call rax;
    // mov eax, 0; ret
    let mut bytes = vec![0x48, 0x83, 0xEC, 0x08, 0x48, 0xB8];
    bytes.extend_from_slice(&value.to_bits().to_le_bytes());
    bytes.extend_from_slice(&[0x66, 0x48, 0x0F, 0x6E, 0xC0, 0x48, 0xB8]);
    let relocation_offset = bytes.len() as u32;
    bytes.extend_from_slice(&[0; 8]);
    bytes.extend_from_slice(&[0xFF, 0xD0, 0x48, 0x83, 0xC4, 0x08, 0xB8, 0, 0, 0, 0, 0xC3]);
    Self::from_bytes(
      &bytes,
      &[Relocation {
        offset: relocation_offset,
        helper: RuntimeHelperId::SetNumberResult,
      }],
    )
  }

  #[cfg(all(unix, target_arch = "x86_64"))]
  pub(crate) fn return_u64(value: u64) -> Result<Self, String> {
    // mov rax, imm64; ret
    let mut bytes = [0u8; 11];
    bytes[0] = 0x48;
    bytes[1] = 0xB8;
    bytes[2..10].copy_from_slice(&value.to_le_bytes());
    bytes[10] = 0xC3;

    // SAFETY: mmap requests private anonymous writable memory. The region is
    // changed to read/execute after emission and before it is called.
    Self::from_bytes(&bytes, &[])
  }

  #[cfg(all(unix, target_arch = "x86_64"))]
  pub(crate) fn resume_numbers(
    values: &[(u32, f64)],
    vm_pc: u32,
  ) -> Result<Self, String> {
    Self::materialize_numbers(values, vm_pc, RuntimeHelperId::SetResumePc)
  }

  #[cfg(all(unix, target_arch = "x86_64"))]
  pub(crate) fn next_block_numbers(
    values: &[(u32, f64)],
    vm_pc: u32,
  ) -> Result<Self, String> {
    Self::materialize_numbers(values, vm_pc, RuntimeHelperId::SetNextBlock)
  }

  #[cfg(all(unix, target_arch = "x86_64"))]
  fn materialize_numbers(
    values: &[(u32, f64)],
    vm_pc: u32,
    exit_helper: RuntimeHelperId,
  ) -> Result<Self, String> {
    // Preserve the frame pointer in callee-saved r12. `push r12` also aligns
    // the SysV stack before every helper call.
    let mut bytes = vec![0x41, 0x54, 0x49, 0x89, 0xFC];
    let mut relocations = Vec::with_capacity(values.len() + 1);
    for (register, value) in values {
      bytes.extend_from_slice(&[0x4C, 0x89, 0xE7, 0xBE]);
      bytes.extend_from_slice(&register.to_le_bytes());
      bytes.extend_from_slice(&[0x48, 0xB8]);
      bytes.extend_from_slice(&value.to_bits().to_le_bytes());
      bytes.extend_from_slice(&[0x66, 0x48, 0x0F, 0x6E, 0xC0, 0x48, 0xB8]);
      let offset = bytes.len() as u32;
      bytes.extend_from_slice(&[0; 8]);
      bytes.extend_from_slice(&[0xFF, 0xD0]);
      relocations.push(Relocation {
        offset,
        helper: RuntimeHelperId::MaterializeNumberRegister,
      });
    }
    bytes.extend_from_slice(&[0x4C, 0x89, 0xE7, 0xBE]);
    bytes.extend_from_slice(&vm_pc.to_le_bytes());
    bytes.extend_from_slice(&[0x48, 0xB8]);
    let offset = bytes.len() as u32;
    bytes.extend_from_slice(&[0; 8]);
    bytes.extend_from_slice(&[0xFF, 0xD0, 0x41, 0x5C, 0xC3]);
    relocations.push(Relocation {
      offset,
      helper: exit_helper,
    });
    Self::from_bytes(&bytes, &relocations)
  }

  #[cfg(all(unix, target_arch = "x86_64"))]
  pub(crate) fn return_local(slot: u32) -> Result<Self, String> {
    let mut bytes = vec![0x48, 0x83, 0xEC, 0x08, 0xBE];
    bytes.extend_from_slice(&slot.to_le_bytes());
    bytes.extend_from_slice(&[0x48, 0xB8]);
    let offset = bytes.len() as u32;
    bytes.extend_from_slice(&[0; 8]);
    bytes.extend_from_slice(&[0xFF, 0xD0, 0x48, 0x83, 0xC4, 0x08, 0xC3]);
    Self::from_bytes(
      &bytes,
      &[Relocation {
        offset,
        helper: RuntimeHelperId::SetResultFromLocal,
      }],
    )
  }

  #[cfg(all(unix, target_arch = "x86_64"))]
  pub(crate) fn conditional_next_block(
    register: u32,
    condition: u32,
    taken_pc: u32,
    fallthrough_pc: u32,
  ) -> Result<Self, String> {
    let mut bytes = vec![0x48, 0x83, 0xEC, 0x08, 0xBE];
    bytes.extend_from_slice(&register.to_le_bytes());
    bytes.push(0xBA);
    bytes.extend_from_slice(&condition.to_le_bytes());
    bytes.push(0xB9);
    bytes.extend_from_slice(&taken_pc.to_le_bytes());
    bytes.extend_from_slice(&[0x41, 0xB8]);
    bytes.extend_from_slice(&fallthrough_pc.to_le_bytes());
    bytes.extend_from_slice(&[0x48, 0xB8]);
    let offset = bytes.len() as u32;
    bytes.extend_from_slice(&[0; 8]);
    bytes.extend_from_slice(&[0xFF, 0xD0, 0x48, 0x83, 0xC4, 0x08, 0xC3]);
    Self::from_bytes(
      &bytes,
      &[Relocation {
        offset,
        helper: RuntimeHelperId::SelectConditionalBlock,
      }],
    )
  }

  #[cfg(all(unix, target_arch = "x86_64"))]
  pub(crate) fn return_register(register: u32) -> Result<Self, String> {
    let mut bytes = vec![0x48, 0x83, 0xEC, 0x08, 0xBE];
    bytes.extend_from_slice(&register.to_le_bytes());
    bytes.extend_from_slice(&[0x48, 0xB8]);
    let offset = bytes.len() as u32;
    bytes.extend_from_slice(&[0; 8]);
    bytes.extend_from_slice(&[0xFF, 0xD0, 0x48, 0x83, 0xC4, 0x08, 0xC3]);
    Self::from_bytes(
      &bytes,
      &[Relocation {
        offset,
        helper: RuntimeHelperId::SetResultFromRegister,
      }],
    )
  }

  #[cfg(all(unix, target_arch = "x86_64"))]
  pub(crate) fn next_block(vm_pc: u32) -> Result<Self, String> {
    let mut bytes = vec![0x48, 0x83, 0xEC, 0x08, 0xBE];
    bytes.extend_from_slice(&vm_pc.to_le_bytes());
    bytes.extend_from_slice(&[0x48, 0xB8]);
    let offset = bytes.len() as u32;
    bytes.extend_from_slice(&[0; 8]);
    bytes.extend_from_slice(&[0xFF, 0xD0, 0x48, 0x83, 0xC4, 0x08, 0xC3]);
    Self::from_bytes(
      &bytes,
      &[Relocation {
        offset,
        helper: RuntimeHelperId::SetNextBlock,
      }],
    )
  }

  #[cfg(all(unix, target_arch = "x86_64"))]
  fn from_bytes(
    bytes: &[u8],
    relocations: &[Relocation],
  ) -> Result<Self, String> {
    let mut bytes = bytes.to_vec();
    for relocation in relocations {
      let address = match relocation.helper {
        RuntimeHelperId::SetNumberResult => denko_jit_set_number_result as *const () as usize,
        RuntimeHelperId::MaterializeNumberRegister => {
          denko_jit_materialize_number_register as *const () as usize
        }
        RuntimeHelperId::SetResumePc => denko_jit_set_resume_pc as *const () as usize,
        RuntimeHelperId::SetNextBlock => denko_jit_set_next_block as *const () as usize,
        RuntimeHelperId::SetResultFromRegister => {
          denko_jit_set_result_from_register as *const () as usize
        }
        RuntimeHelperId::SelectConditionalBlock => {
          denko_jit_select_conditional_block as *const () as usize
        }
        RuntimeHelperId::SetResultFromLocal => {
          denko_jit_set_result_from_local as *const () as usize
        }
      } as u64;
      let start = relocation.offset as usize;
      bytes
        .get_mut(start..start + 8)
        .ok_or_else(|| "native relocation is out of bounds".to_string())?
        .copy_from_slice(&address.to_le_bytes());
    }
    let ptr = unsafe {
      libc::mmap(
        std::ptr::null_mut(),
        bytes.len(),
        libc::PROT_READ | libc::PROT_WRITE,
        libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
        -1,
        0,
      )
    };
    if ptr == libc::MAP_FAILED {
      return Err(std::io::Error::last_os_error().to_string());
    }
    // SAFETY: mmap returned a writable region of exactly bytes.len() bytes.
    unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr.cast(), bytes.len()) };
    if unsafe { libc::mprotect(ptr, bytes.len(), libc::PROT_READ | libc::PROT_EXEC) } != 0 {
      unsafe { libc::munmap(ptr, bytes.len()) };
      return Err(std::io::Error::last_os_error().to_string());
    }
    let ptr = NonNull::new(ptr.cast()).ok_or_else(|| "mmap returned null".to_string())?;
    Ok(Self {
      ptr,
      len: bytes.len(),
    })
  }

  #[cfg(not(all(unix, target_arch = "x86_64")))]
  pub(crate) fn return_number(_value: f64) -> Result<Self, String> {
    Err("minimal native backend requires Unix x86-64".to_string())
  }

  #[cfg(not(all(unix, target_arch = "x86_64")))]
  pub(crate) fn return_u64(_value: u64) -> Result<Self, String> {
    Err("minimal native backend requires Unix x86-64".to_string())
  }

  #[cfg(all(unix, target_arch = "x86_64"))]
  pub(crate) unsafe fn call_frame(
    &self,
    frame: *mut JitFrame,
  ) -> JitExitCode {
    let entry: unsafe extern "C" fn(*mut JitFrame) -> JitExitCode =
      unsafe { std::mem::transmute(self.ptr.as_ptr()) };
    unsafe { entry(frame) }
  }

  #[cfg(all(unix, target_arch = "x86_64"))]
  pub(crate) unsafe fn call_u64(&self) -> u64 {
    // SAFETY: callers must only invoke code produced by return_u64.
    let entry: unsafe extern "C" fn() -> u64 = unsafe { std::mem::transmute(self.ptr.as_ptr()) };
    unsafe { entry() }
  }
}

impl Drop for NativeCode {
  fn drop(&mut self) {
    #[cfg(all(unix, target_arch = "x86_64"))]
    {
      // SAFETY: this pointer and length came from the successful mmap above.
      unsafe { libc::munmap(self.ptr.as_ptr().cast(), self.len) };
    }
  }
}

#[cfg(all(test, unix, target_arch = "x86_64"))]
mod tests {
  use super::{JIT_EXIT_RETURN, JitFrame, NativeCode};
  use crate::value::Value;

  #[test]
  fn patches_a_direct_jump_between_native_blocks() {
    let target = NativeCode::return_u64(42).expect("target block");
    let (mut source, site) = NativeCode::patchable_jump().expect("source block");
    source
      .patch_jump(site, target.entry_address())
      .expect("link blocks");
    // SAFETY: source is linked to a live target with the same function ABI.
    assert_eq!(unsafe { source.call_u64() }, 42);
  }

  #[test]
  fn emits_and_executes_return_constant() {
    let code = NativeCode::return_u64(42).expect("native allocation");
    // SAFETY: the code object owns executable memory containing a valid
    // x86-64 function until it is dropped after this call.
    assert_eq!(unsafe { code.call_u64() }, 42);
  }

  #[test]
  fn returns_a_vm_owned_local_through_the_opaque_frame_boundary() {
    let code = NativeCode::return_local(0).expect("native allocation");
    let mut locals = vec![std::rc::Rc::new(std::cell::RefCell::new(Value::Number(
      17.0,
    )))];
    let mut result = Value::Undefined;
    let mut frame = JitFrame {
      vm: std::ptr::null_mut(),
      function_id: 0,
      vm_pc: 0,
      registers: std::ptr::null_mut(),
      register_count: 0,
      result: &mut result,
      arguments: std::ptr::null(),
      argument_count: 0,
      locals: (&mut locals as *mut Vec<crate::value::SharedValue>).cast(),
      local_count: 1,
    };
    assert_eq!(unsafe { code.call_frame(&mut frame) }, JIT_EXIT_RETURN);
    assert!(matches!(result, Value::Number(value) if value == 17.0));
  }

  #[test]
  fn returns_a_prepared_argument_before_local_cells_exist() {
    let code = NativeCode::return_local(0).expect("native allocation");
    let arguments = [Value::Number(23.0)];
    let mut result = Value::Undefined;
    let mut frame = JitFrame {
      vm: std::ptr::null_mut(),
      function_id: 0,
      vm_pc: 0,
      registers: std::ptr::null_mut(),
      register_count: 0,
      result: &mut result,
      arguments: arguments.as_ptr(),
      argument_count: arguments.len(),
      locals: std::ptr::null_mut(),
      local_count: 0,
    };
    assert_eq!(unsafe { code.call_frame(&mut frame) }, JIT_EXIT_RETURN);
    assert!(matches!(result, Value::Number(value) if value == 23.0));
  }

  #[test]
  fn writes_a_denko_number_through_the_frame_abi() {
    let code = NativeCode::return_number(42.0).expect("native allocation");
    let mut result = Value::Undefined;
    let mut frame = JitFrame {
      vm: std::ptr::null_mut(),
      function_id: 0,
      vm_pc: 0,
      registers: std::ptr::null_mut(),
      register_count: 0,
      result: &mut result,
      arguments: std::ptr::null(),
      argument_count: 0,
      locals: std::ptr::null_mut(),
      local_count: 0,
    };
    // SAFETY: the frame points to live Rust-owned result storage and the code
    // was emitted by return_number for this exact ABI.
    assert_eq!(unsafe { code.call_frame(&mut frame) }, JIT_EXIT_RETURN);
    assert!(matches!(result, Value::Number(value) if value == 42.0));
  }
}
