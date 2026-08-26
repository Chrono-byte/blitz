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

/// Frame for leaf calls that cannot resume in the interpreter.
#[repr(C)]
pub(crate) struct DirectCallFrame {
  pub(crate) result: *mut Value,
  pub(crate) arguments: *const Value,
  pub(crate) argument_count: usize,
}

/// Compact direct-call frame used by typed numeric code.
#[repr(C)]
pub(crate) struct CompactDirectCallFrame {
  pub(crate) result_bits: u64,
  pub(crate) arguments: *const u64,
  pub(crate) argument_count: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum NumericLoopAddend {
  Constant,
  Index,
  SubtractConstant,
  MultiplyConstant,
}

pub(crate) const NUMERIC_EXIT_COMPLETED: u32 = 0;
pub(crate) const NUMERIC_EXIT_ACTIVATION_LIMIT: u32 = 1;
pub(crate) const NUMERIC_EXIT_INTERRUPTED: u32 = 2;

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub(crate) struct NativeNumericLoopFrame {
  pub(crate) index: f64,
  pub(crate) limit: f64,
  pub(crate) accumulator: f64,
  pub(crate) step: f64,
  pub(crate) addend: f64,
  pub(crate) iteration_budget: u32,
  pub(crate) iterations: u32,
  pub(crate) exit_code: u32,
  pub(crate) requested_generation: *const std::sync::atomic::AtomicU64,
  pub(crate) acknowledged_generation: u64,
  pub(crate) observed_generation: u64,
}

const _: () = {
  assert!(std::mem::offset_of!(NativeNumericLoopFrame, index) == 0);
  assert!(std::mem::offset_of!(NativeNumericLoopFrame, iteration_budget) == 40);
  assert!(std::mem::offset_of!(NativeNumericLoopFrame, iterations) == 44);
  assert!(std::mem::offset_of!(NativeNumericLoopFrame, exit_code) == 48);
  assert!(std::mem::offset_of!(NativeNumericLoopFrame, requested_generation) == 56);
  assert!(std::mem::offset_of!(NativeNumericLoopFrame, acknowledged_generation) == 64);
  assert!(std::mem::offset_of!(NativeNumericLoopFrame, observed_generation) == 72);
  assert!(std::mem::size_of::<NativeNumericLoopFrame>() == 80);
};

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
  SetDirectNumberResult,
  SetDirectArgumentResult,
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
pub(crate) unsafe extern "C" fn denko_jit_set_direct_number_result(
  frame: *mut DirectCallFrame,
  value: f64,
) -> JitExitCode {
  unsafe { *(*frame).result = Value::Number(value) };
  JIT_EXIT_RETURN
}

#[unsafe(no_mangle)]
pub(crate) unsafe extern "C" fn denko_jit_set_direct_argument_result(
  frame: *mut DirectCallFrame,
  slot: u32,
) -> JitExitCode {
  unsafe {
    *(*frame).result = if (*frame).arguments.is_null() || slot as usize >= (*frame).argument_count {
      Value::Undefined
    } else {
      (*(*frame).arguments.add(slot as usize)).clone()
    };
  }
  JIT_EXIT_RETURN
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
  pub(crate) fn direct_return_number(value: f64) -> Result<Self, String> {
    let mut bytes = vec![0x48, 0x83, 0xEC, 0x08, 0x48, 0xB8];
    bytes.extend_from_slice(&value.to_bits().to_le_bytes());
    bytes.extend_from_slice(&[0x66, 0x48, 0x0F, 0x6E, 0xC0, 0x48, 0xB8]);
    let offset = bytes.len() as u32;
    bytes.extend_from_slice(&[0; 8]);
    bytes.extend_from_slice(&[0xFF, 0xD0, 0x48, 0x83, 0xC4, 0x08, 0xC3]);
    Self::from_bytes(
      &bytes,
      &[Relocation {
        offset,
        helper: RuntimeHelperId::SetDirectNumberResult,
      }],
    )
  }

  #[cfg(all(unix, target_arch = "x86_64"))]
  pub(crate) fn direct_return_argument(slot: u32) -> Result<Self, String> {
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
        helper: RuntimeHelperId::SetDirectArgumentResult,
      }],
    )
  }

  #[cfg(all(unix, target_arch = "x86_64"))]
  pub(crate) fn numeric_loop(addend: NumericLoopAddend) -> Result<Self, String> {
    let mut bytes = vec![
      0xF2, 0x0F, 0x10, 0x07, // movsd xmm0, [rdi]       index
      0xF2, 0x0F, 0x10, 0x4F, 0x08, // movsd xmm1, [rdi+8]   limit
      0xF2, 0x0F, 0x10, 0x57, 0x10, // movsd xmm2, [rdi+16]  accumulator
      0xF2, 0x0F, 0x10, 0x5F, 0x18, // movsd xmm3, [rdi+24]  step
      0xF2, 0x0F, 0x10, 0x67, 0x20, // movsd xmm4, [rdi+32]  leaf value
      0x44, 0x8B, 0x47, 0x28, // mov r8d, [rdi+40]       iteration budget
      0x48, 0x8B, 0x77, 0x38, // mov rsi, [rdi+56]      interrupt generation
      0x4C, 0x8B, 0x4F, 0x40, // mov r9, [rdi+64]       acknowledged generation
      0x4D, 0x89, 0xCA, // mov r10, r9             observed generation
      0x31, 0xC0, // xor eax, eax             iterations
    ];

    // A VM-owned Arc keeps the aligned AtomicU64 alive for this synchronous
    // call. On x86-64, the aligned load below supplies the acquire-side
    // hardware ordering paired with the producer's Release increment.
    let entry_poll = bytes.len();
    bytes.extend_from_slice(&[0x4C, 0x8B, 0x16]); // mov r10, [rsi]
    bytes.extend_from_slice(&[0x4D, 0x39, 0xCA]); // cmp r10, r9
    bytes.extend_from_slice(&[0x0F, 0x85]); // jne interrupted
    let entry_interrupt = bytes.len();
    bytes.extend_from_slice(&[0; 4]);

    let loop_start = bytes.len();
    bytes.extend_from_slice(&[0x66, 0x0F, 0x2E, 0xC1]); // ucomisd xmm0, xmm1
    bytes.extend_from_slice(&[0x0F, 0x8A]); // jp completed, unordered means index < limit is false
    let unordered_exit = bytes.len();
    bytes.extend_from_slice(&[0; 4]);
    bytes.extend_from_slice(&[0x0F, 0x83]); // jae completed
    let completed_exit = bytes.len();
    bytes.extend_from_slice(&[0; 4]);
    match addend {
      NumericLoopAddend::Constant => {
        bytes.extend_from_slice(&[0xF2, 0x0F, 0x58, 0xD4]); // addsd xmm2, xmm4
      }
      NumericLoopAddend::Index => {
        bytes.extend_from_slice(&[0xF2, 0x0F, 0x58, 0xD0]); // addsd xmm2, xmm0
      }
      NumericLoopAddend::SubtractConstant => {
        bytes.extend_from_slice(&[0xF2, 0x0F, 0x5C, 0xD4]); // subsd xmm2, xmm4
      }
      NumericLoopAddend::MultiplyConstant => {
        bytes.extend_from_slice(&[0xF2, 0x0F, 0x59, 0xD4]); // mulsd xmm2, xmm4
      }
    }
    bytes.extend_from_slice(&[
      0xF2, 0x0F, 0x58, 0xC3, // addsd xmm0, xmm3
      0xFF, 0xC0, // inc eax
      0xA8, 0x3F, // test al, 63
      0x75, // jne budget check
    ]);
    let skip_poll = bytes.len();
    bytes.push(0);
    bytes.extend_from_slice(&[0x4C, 0x8B, 0x16]); // mov r10, [rsi]
    bytes.extend_from_slice(&[0x4D, 0x39, 0xCA]); // cmp r10, r9
    bytes.extend_from_slice(&[0x0F, 0x85]); // jne interrupted
    let loop_interrupt = bytes.len();
    bytes.extend_from_slice(&[0; 4]);
    bytes.extend_from_slice(&[0x44, 0x39, 0xC0]); // cmp eax, r8d
    bytes.extend_from_slice(&[0x0F, 0x82]); // jb loop
    let backedge = bytes.len();
    bytes.extend_from_slice(&[0; 4]);

    bytes.extend_from_slice(&[0xB9]); // mov ecx, activation limit
    bytes.extend_from_slice(&NUMERIC_EXIT_ACTIVATION_LIMIT.to_le_bytes());
    bytes.push(0xE9); // jmp exit
    let budget_exit = bytes.len();
    bytes.extend_from_slice(&[0; 4]);

    let completed = bytes.len();
    bytes.extend_from_slice(&[0x31, 0xC9]); // xor ecx, ecx
    bytes.push(0xE9); // jmp exit
    let completed_jump = bytes.len();
    bytes.extend_from_slice(&[0; 4]);

    let interrupted = bytes.len();
    bytes.extend_from_slice(&[0xB9]); // mov ecx, interrupted
    bytes.extend_from_slice(&NUMERIC_EXIT_INTERRUPTED.to_le_bytes());

    let exit = bytes.len();
    bytes.extend_from_slice(&[
      0xF2, 0x0F, 0x11, 0x07, // movsd [rdi], xmm0
      0xF2, 0x0F, 0x11, 0x57, 0x10, // movsd [rdi+16], xmm2
      0x89, 0x47, 0x2C, // mov [rdi+44], eax      iterations
      0x89, 0x4F, 0x30, // mov [rdi+48], ecx      exit code
      0x4C, 0x89, 0x57, 0x48, // mov [rdi+72], r10      observed generation
      0xC3,
    ]);
    let relative32 = |_from: usize, instruction_end: usize, to: usize| -> Result<[u8; 4], String> {
      i32::try_from(to as isize - instruction_end as isize)
        .map(i32::to_le_bytes)
        .map_err(|_| "numeric loop branch is out of range".to_string())
    };
    bytes[entry_interrupt..entry_interrupt + 4].copy_from_slice(&relative32(
      entry_interrupt,
      entry_interrupt + 4,
      interrupted,
    )?);
    bytes[unordered_exit..unordered_exit + 4].copy_from_slice(&relative32(
      unordered_exit,
      unordered_exit + 4,
      completed,
    )?);
    bytes[completed_exit..completed_exit + 4].copy_from_slice(&relative32(
      completed_exit,
      completed_exit + 4,
      completed,
    )?);
    bytes[skip_poll] = i8::try_from(loop_start as isize - (skip_poll + 1) as isize)
      .map(|offset| offset as u8)
      .map_err(|_| "numeric loop poll branch is out of range".to_string())?;
    bytes[loop_interrupt..loop_interrupt + 4].copy_from_slice(&relative32(
      loop_interrupt,
      loop_interrupt + 4,
      interrupted,
    )?);
    bytes[backedge..backedge + 4].copy_from_slice(&relative32(backedge, backedge + 4, loop_start)?);
    bytes[budget_exit..budget_exit + 4].copy_from_slice(&relative32(
      budget_exit,
      budget_exit + 4,
      exit,
    )?);
    bytes[completed_jump..completed_jump + 4].copy_from_slice(&relative32(
      completed_jump,
      completed_jump + 4,
      exit,
    )?);
    debug_assert!(entry_poll < loop_start);
    Self::from_bytes(&bytes, &[])
  }

  #[cfg(all(unix, target_arch = "x86_64"))]
  pub(crate) fn dense_array_push_loop() -> Result<Self, String> {
    let mut bytes = vec![
      0xF2, 0x0F, 0x10, 0x07, // movsd xmm0, [rdi]       index
      0xF2, 0x0F, 0x10, 0x4F, 0x08, // movsd xmm1, [rdi+8]   limit
      0xF2, 0x0F, 0x10, 0x57, 0x10, // movsd xmm2, [rdi+16]  step
      0x48, 0x8D, 0x57, 0x20, // lea rdx, [rdi+32]       values
      0x31, 0xC0, // xor eax, eax             iterations
    ];
    let loop_start = bytes.len();
    bytes.extend_from_slice(&[0x66, 0x0F, 0x2E, 0xC1]); // ucomisd xmm0, xmm1
    bytes.push(0x7A); // jp exit
    let unordered_exit = bytes.len();
    bytes.push(0);
    bytes.push(0x73); // jae exit
    let completed_exit = bytes.len();
    bytes.push(0);
    bytes.extend_from_slice(&[
      0xF2, 0x0F, 0x11, 0x02, // movsd [rdx], xmm0
      0x48, 0x83, 0xC2, 0x08, // add rdx, 8
      0xF2, 0x0F, 0x58, 0xC2, // addsd xmm0, xmm2
      0xFF, 0xC0, // inc eax
      0x83, 0xF8, 0x40, // cmp eax, 64
      0x72, // jb loop
    ]);
    let backedge = bytes.len();
    bytes.push(0);
    let exit = bytes.len();
    bytes.extend_from_slice(&[
      0xF2, 0x0F, 0x11, 0x07, // movsd [rdi], xmm0
      0x89, 0x47, 0x18, // mov [rdi+24], eax
      0xC3,
    ]);
    let relative = |from: usize, to: usize| -> Result<u8, String> {
      i8::try_from(to as isize - (from + 1) as isize)
        .map(|offset| offset as u8)
        .map_err(|_| "dense array loop branch is out of range".to_string())
    };
    bytes[unordered_exit] = relative(unordered_exit, exit)?;
    bytes[completed_exit] = relative(completed_exit, exit)?;
    bytes[backedge] = relative(backedge, loop_start)?;
    Self::from_bytes(&bytes, &[])
  }

  #[cfg(all(unix, target_arch = "x86_64"))]
  pub(crate) fn dense_masked_read_loop() -> Result<Self, String> {
    let mut bytes = vec![
      0xF2, 0x0F, 0x10, 0x07, // movsd xmm0, [rdi]       index
      0xF2, 0x0F, 0x10, 0x4F, 0x08, // movsd xmm1, [rdi+8]   limit
      0xF2, 0x0F, 0x10, 0x57, 0x10, // movsd xmm2, [rdi+16]  accumulator
      0x48, 0x8D, 0x57, 0x18, // lea rdx, [rdi+24]       elements
      0xF2, 0x48, 0x0F, 0x2C, 0xC8, // cvttsd2si rcx, xmm0   phase
      0x83, 0xE1, 0x03, // and ecx, 3
      0x49, 0xB8, // movabs r8, 1.0 bits
    ];
    bytes.extend_from_slice(&1.0f64.to_bits().to_le_bytes());
    bytes.extend_from_slice(&[
      0x66, 0x49, 0x0F, 0x6E, 0xE0, // movq xmm4, r8
      0x31, 0xC0, // xor eax, eax             iterations
    ]);
    let loop_start = bytes.len();
    bytes.extend_from_slice(&[0x66, 0x0F, 0x2E, 0xC1]); // ucomisd xmm0, xmm1
    bytes.push(0x7A); // jp exit
    let unordered_exit = bytes.len();
    bytes.push(0);
    bytes.push(0x73); // jae exit
    let completed_exit = bytes.len();
    bytes.push(0);
    bytes.extend_from_slice(&[
      0xF2, 0x0F, 0x10, 0x1C, 0xCA, // movsd xmm3, [rdx+rcx*8]
      0xF2, 0x0F, 0x58, 0xD3, // addsd xmm2, xmm3
      0xFF, 0xC1, // inc ecx
      0x83, 0xE1, 0x03, // and ecx, 3
      0xF2, 0x0F, 0x58, 0xC4, // addsd xmm0, xmm4
      0xFF, 0xC0, // inc eax
      0x83, 0xF8, 0x40, // cmp eax, 64
      0x72, // jb loop
    ]);
    let backedge = bytes.len();
    bytes.push(0);
    let exit = bytes.len();
    bytes.extend_from_slice(&[
      0xF2, 0x0F, 0x11, 0x07, // movsd [rdi], xmm0
      0xF2, 0x0F, 0x11, 0x57, 0x10, // movsd [rdi+16], xmm2
      0x89, 0x47, 0x38, // mov [rdi+56], eax
      0xC3,
    ]);
    let relative = |from: usize, to: usize| -> Result<u8, String> {
      i8::try_from(to as isize - (from + 1) as isize)
        .map(|offset| offset as u8)
        .map_err(|_| "dense masked read loop branch is out of range".to_string())
    };
    bytes[unordered_exit] = relative(unordered_exit, exit)?;
    bytes[completed_exit] = relative(completed_exit, exit)?;
    bytes[backedge] = relative(backedge, loop_start)?;
    Self::from_bytes(&bytes, &[])
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
        RuntimeHelperId::SetDirectNumberResult => {
          denko_jit_set_direct_number_result as *const () as usize
        }
        RuntimeHelperId::SetDirectArgumentResult => {
          denko_jit_set_direct_argument_result as *const () as usize
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

  #[cfg(all(unix, target_arch = "x86_64"))]
  pub(crate) fn install_artifact(
    artifact: crate::vm::jit::artifact::ValidatedArtifact
  ) -> Result<Self, String> {
    let (bytes, relocations) = artifact.into_parts();
    let relocations: Vec<_> = relocations
      .into_vec()
      .into_iter()
      .map(|relocation| Relocation {
        offset: relocation.offset.index() as u32,
        helper: relocation.helper,
      })
      .collect();
    Self::from_bytes(&bytes, &relocations)
  }

  #[cfg(not(all(unix, target_arch = "x86_64")))]
  pub(crate) fn install_artifact(
    _artifact: crate::vm::jit::artifact::ValidatedArtifact
  ) -> Result<Self, String> {
    Err("optimizing JIT requires Unix x86-64".to_string())
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
  pub(crate) unsafe fn call_direct_frame(
    &self,
    frame: *mut DirectCallFrame,
  ) -> JitExitCode {
    let entry: unsafe extern "C" fn(*mut DirectCallFrame) -> JitExitCode =
      unsafe { std::mem::transmute(self.ptr.as_ptr()) };
    unsafe { entry(frame) }
  }

  #[cfg(all(unix, target_arch = "x86_64"))]
  pub(crate) unsafe fn call_compact_direct_frame(
    &self,
    frame: *mut CompactDirectCallFrame,
  ) -> JitExitCode {
    let entry: unsafe extern "C" fn(*mut CompactDirectCallFrame) -> JitExitCode =
      unsafe { std::mem::transmute(self.ptr.as_ptr()) };
    unsafe { entry(frame) }
  }

  #[cfg(all(unix, target_arch = "x86_64"))]
  pub(crate) unsafe fn call_numeric_loop_frame(
    &self,
    frame: *mut NativeNumericLoopFrame,
  ) {
    let entry: unsafe extern "C" fn(*mut NativeNumericLoopFrame) =
      unsafe { std::mem::transmute(self.ptr.as_ptr()) };
    unsafe { entry(frame) };
  }

  #[cfg(all(unix, target_arch = "x86_64"))]
  pub(crate) unsafe fn call_dense_array_push_frame(
    &self,
    frame: &mut crate::vm::region::PreparedArrayPushRegion,
  ) {
    let function: unsafe extern "C" fn(*mut crate::vm::region::PreparedArrayPushRegion) =
      unsafe { std::mem::transmute(self.ptr.as_ptr()) };
    unsafe { function(frame) };
  }

  #[cfg(all(unix, target_arch = "x86_64"))]
  pub(crate) unsafe fn call_dense_masked_read_frame(
    &self,
    frame: &mut crate::vm::region::PreparedDenseMaskedReadRegion,
  ) {
    let function: unsafe extern "C" fn(*mut crate::vm::region::PreparedDenseMaskedReadRegion) =
      unsafe { std::mem::transmute(self.ptr.as_ptr()) };
    unsafe { function(frame) };
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
  use super::{
    JIT_EXIT_RETURN, JitFrame, NUMERIC_EXIT_ACTIVATION_LIMIT, NUMERIC_EXIT_COMPLETED,
    NUMERIC_EXIT_INTERRUPTED, NativeCode, NativeNumericLoopFrame, NumericLoopAddend,
  };
  use crate::value::Value;
  use std::sync::atomic::AtomicU64;

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

  #[test]
  fn executes_a_bounded_numeric_call_loop_batch() {
    let code = NativeCode::numeric_loop(NumericLoopAddend::Constant).expect("native allocation");
    let generation = AtomicU64::new(0);
    let mut frame = NativeNumericLoopFrame {
      index: 0.0,
      limit: 100.0,
      accumulator: 0.0,
      step: 1.0,
      addend: 42.0,
      iteration_budget: 4_096,
      iterations: 0,
      exit_code: u32::MAX,
      requested_generation: &generation,
      acknowledged_generation: 0,
      observed_generation: 0,
    };
    unsafe { code.call_numeric_loop_frame(&mut frame) };
    assert_eq!(frame.index, 100.0);
    assert_eq!(frame.accumulator, 4_200.0);
    assert_eq!(frame.iterations, 100);
    assert_eq!(frame.exit_code, NUMERIC_EXIT_COMPLETED);
  }

  #[test]
  fn prepares_a_bounded_dense_array_push_batch() {
    let code = NativeCode::dense_array_push_loop().expect("native allocation");
    let mut frame = crate::vm::region::PreparedArrayPushRegion {
      index: 3.0,
      limit: 100.0,
      step: 2.0,
      iterations: 0,
      values: [0.0; 64],
    };
    unsafe { code.call_dense_array_push_frame(&mut frame) };
    assert_eq!(frame.iterations, 49);
    assert_eq!(frame.index, 101.0);
    assert_eq!(frame.values[0], 3.0);
    assert_eq!(frame.values[48], 99.0);
  }

  #[test]
  fn executes_a_bounded_dense_masked_read_batch() {
    let code = NativeCode::dense_masked_read_loop().expect("native allocation");
    let mut frame = crate::vm::region::PreparedDenseMaskedReadRegion {
      index: 0.0,
      limit: 100.0,
      accumulator: 0.0,
      elements: [1.0, 2.0, 3.0, 4.0],
      iterations: 0,
    };
    unsafe { code.call_dense_masked_read_frame(&mut frame) };
    assert_eq!(frame.iterations, 64);
    assert_eq!(frame.index, 64.0);
    assert_eq!(frame.accumulator, 160.0);
  }

  #[test]
  fn executes_a_bounded_numeric_index_loop_batch() {
    let code = NativeCode::numeric_loop(NumericLoopAddend::Index).expect("native allocation");
    let generation = AtomicU64::new(0);
    let mut frame = NativeNumericLoopFrame {
      index: 0.0,
      limit: f64::INFINITY,
      accumulator: 0.0,
      step: 1.0,
      addend: 0.0,
      iteration_budget: 4_096,
      iterations: 0,
      exit_code: u32::MAX,
      requested_generation: &generation,
      acknowledged_generation: 0,
      observed_generation: 0,
    };
    unsafe { code.call_numeric_loop_frame(&mut frame) };
    assert_eq!(frame.index, 4_096.0);
    assert_eq!(frame.iterations, 4_096);
    assert_eq!(frame.exit_code, NUMERIC_EXIT_ACTIVATION_LIMIT);
  }

  #[test]
  fn numeric_loop_observes_a_pending_interrupt_before_work() {
    let code = NativeCode::numeric_loop(NumericLoopAddend::Constant).expect("native allocation");
    let generation = AtomicU64::new(1);
    let mut frame = NativeNumericLoopFrame {
      index: 0.0,
      limit: 100.0,
      accumulator: 0.0,
      step: 1.0,
      addend: 1.0,
      iteration_budget: 4_096,
      iterations: 0,
      exit_code: u32::MAX,
      requested_generation: &generation,
      acknowledged_generation: 0,
      observed_generation: 0,
    };
    unsafe { code.call_numeric_loop_frame(&mut frame) };
    assert_eq!(frame.iterations, 0);
    assert_eq!(frame.exit_code, NUMERIC_EXIT_INTERRUPTED);
    assert_eq!(frame.observed_generation, 1);
  }

  #[test]
  fn numeric_loop_polls_an_asynchronous_interrupt_every_64_iterations() {
    use std::sync::Arc;
    use std::sync::atomic::Ordering;

    let code = NativeCode::numeric_loop(NumericLoopAddend::Constant).expect("native allocation");
    let generation = Arc::new(AtomicU64::new(0));
    let requester = Arc::clone(&generation);
    let thread = std::thread::spawn(move || {
      std::thread::sleep(std::time::Duration::from_millis(1));
      requester.store(1, Ordering::Release);
    });
    let mut frame = NativeNumericLoopFrame {
      index: 0.0,
      limit: f64::INFINITY,
      accumulator: 0.0,
      step: 1.0,
      addend: 1.0,
      iteration_budget: u32::MAX,
      iterations: 0,
      exit_code: u32::MAX,
      requested_generation: Arc::as_ptr(&generation),
      acknowledged_generation: 0,
      observed_generation: 0,
    };
    unsafe { code.call_numeric_loop_frame(&mut frame) };
    thread.join().expect("interrupt requester");

    assert_eq!(frame.exit_code, NUMERIC_EXIT_INTERRUPTED);
    assert!(frame.iterations > 0);
    assert_eq!(frame.iterations % 64, 0);
    assert_eq!(frame.observed_generation, 1);
  }
}
