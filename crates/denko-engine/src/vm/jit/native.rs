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
}

pub(crate) type JitExitCode = u32;
pub(crate) const JIT_EXIT_RETURN: JitExitCode = 0;
pub(crate) const JIT_EXIT_INTERPRETER: JitExitCode = 1;
pub(crate) const JIT_EXIT_EXCEPTION: JitExitCode = 2;
pub(crate) const JIT_EXIT_INVALIDATED: JitExitCode = 3;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RuntimeHelperId {
  SetNumberResult,
  ResumeNumber,
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
pub(crate) unsafe extern "C" fn denko_jit_resume_number(
  frame: *mut JitFrame,
  register: u32,
  vm_pc: u32,
  value: f64,
) -> JitExitCode {
  // SAFETY: generated code passes a valid frame and a checked register index.
  unsafe {
    if register as usize >= (*frame).register_count {
      return JIT_EXIT_EXCEPTION;
    }
    *(*frame).registers.add(register as usize) = Value::Number(value);
    (*frame).vm_pc = vm_pc as usize;
  }
  JIT_EXIT_INTERPRETER
}

#[derive(Debug)]
pub(crate) struct NativeCode {
  ptr: NonNull<u8>,
  len: usize,
}

impl NativeCode {
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
  pub(crate) fn resume_number(
    register: u32,
    vm_pc: u32,
    value: f64,
  ) -> Result<Self, String> {
    let mut bytes = vec![0x48, 0x83, 0xEC, 0x08, 0xBE];
    bytes.extend_from_slice(&register.to_le_bytes());
    bytes.push(0xBA);
    bytes.extend_from_slice(&vm_pc.to_le_bytes());
    bytes.extend_from_slice(&[0x48, 0xB8]);
    bytes.extend_from_slice(&value.to_bits().to_le_bytes());
    bytes.extend_from_slice(&[0x66, 0x48, 0x0F, 0x6E, 0xC0, 0x48, 0xB8]);
    let relocation_offset = bytes.len() as u32;
    bytes.extend_from_slice(&[0; 8]);
    bytes.extend_from_slice(&[0xFF, 0xD0, 0x48, 0x83, 0xC4, 0x08, 0xC3]);
    Self::from_bytes(
      &bytes,
      &[Relocation {
        offset: relocation_offset,
        helper: RuntimeHelperId::ResumeNumber,
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
        RuntimeHelperId::ResumeNumber => denko_jit_resume_number as *const () as usize,
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
  fn emits_and_executes_return_constant() {
    let code = NativeCode::return_u64(42).expect("native allocation");
    // SAFETY: the code object owns executable memory containing a valid
    // x86-64 function until it is dropped after this call.
    assert_eq!(unsafe { code.call_u64() }, 42);
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
    };
    // SAFETY: the frame points to live Rust-owned result storage and the code
    // was emitted by return_number for this exact ABI.
    assert_eq!(unsafe { code.call_frame(&mut frame) }, JIT_EXIT_RETURN);
    assert!(matches!(result, Value::Number(value) if value == 42.0));
  }
}
