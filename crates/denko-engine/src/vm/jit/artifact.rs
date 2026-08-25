// SPDX-License-Identifier: MPL-2.0

//! Inert native artifacts produced away from the VM thread.

use super::ir::VerifiedTypedFunction;
use super::native::RuntimeHelperId;

pub(crate) const NATIVE_ARTIFACT_ABI: AbiRevision = AbiRevision(1);
const MAX_ARTIFACT_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct AbiRevision(u16);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TargetIsa {
  SysVx86_64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CodeOffset(u32);

impl CodeOffset {
  fn from_index(index: usize) -> Result<Self, ArtifactError> {
    u32::try_from(index)
      .map(Self)
      .map_err(|_| ArtifactError::TooLarge)
  }

  pub(super) fn index(self) -> usize {
    self.0 as usize
  }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ArtifactRelocation {
  pub(crate) offset: CodeOffset,
  pub(crate) helper: RuntimeHelperId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct NativeArtifact {
  target: TargetIsa,
  abi: AbiRevision,
  entry: CodeOffset,
  bytes: Box<[u8]>,
  relocations: Box<[ArtifactRelocation]>,
}

#[derive(Debug)]
pub(crate) struct ValidatedArtifact(NativeArtifact);

impl ValidatedArtifact {
  pub(super) fn into_parts(self) -> (Box<[u8]>, Box<[ArtifactRelocation]>) {
    (self.0.bytes, self.0.relocations)
  }
}

impl NativeArtifact {
  pub(crate) fn validate(self) -> Result<ValidatedArtifact, ArtifactError> {
    if self.target != TargetIsa::SysVx86_64
      || self.abi != NATIVE_ARTIFACT_ABI
      || self.entry.index() != 0
      || self.bytes.is_empty()
      || self.bytes.len() > MAX_ARTIFACT_BYTES
    {
      return Err(ArtifactError::InvalidContract);
    }
    let mut previous_end = 0;
    for relocation in &self.relocations {
      let start = relocation.offset.index();
      let end = start
        .checked_add(8)
        .ok_or(ArtifactError::InvalidRelocation)?;
      if start < previous_end || end > self.bytes.len() {
        return Err(ArtifactError::InvalidRelocation);
      }
      previous_end = end;
    }
    Ok(ValidatedArtifact(self))
  }

  #[cfg(test)]
  pub(crate) fn bytes(&self) -> &[u8] {
    &self.bytes
  }

  #[cfg(test)]
  pub(crate) fn relocations(&self) -> &[ArtifactRelocation] {
    &self.relocations
  }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ArtifactError {
  UnsupportedTypedFunction,
  TooLarge,
  InvalidContract,
  InvalidRelocation,
}

#[cfg(all(unix, target_arch = "x86_64"))]
pub(crate) fn emit_numeric_add(
  function: &VerifiedTypedFunction
) -> Result<NativeArtifact, ArtifactError> {
  let (left, right) = function
    .numeric_add_arguments()
    .ok_or(ArtifactError::UnsupportedTypedFunction)?;
  let mut bytes = vec![
    0x41, 0x54, // push r12; align stack and retain the frame
    0x49, 0x89, 0xFC, // mov r12, rdi
    0x48, 0x83, 0xEC, 0x10, // sub rsp, 16
  ];
  let mut relocations = Vec::with_capacity(3);
  emit_argument_guard(&mut bytes, &mut relocations, left, 0)?;
  emit_argument_guard(&mut bytes, &mut relocations, right, 8)?;
  bytes.extend_from_slice(&[
    0xF2, 0x0F, 0x10, 0x04, 0x24, // movsd xmm0, [rsp]
    0xF2, 0x0F, 0x58, 0x44, 0x24, 0x08, // addsd xmm0, [rsp+8]
    0x4C, 0x89, 0xE7, // mov rdi, r12
    0x48, 0xB8,
  ]);
  let result_relocation = CodeOffset::from_index(bytes.len())?;
  bytes.extend_from_slice(&[0; 8]);
  bytes.extend_from_slice(&[
    0xFF, 0xD0, // call rax
    0x48, 0x83, 0xC4, 0x10, // add rsp, 16
    0x41, 0x5C, // pop r12
    0xC3, // ret
  ]);
  relocations.push(ArtifactRelocation {
    offset: result_relocation,
    helper: RuntimeHelperId::SetDirectNumberResult,
  });
  let guard_failure = bytes.len();
  bytes.extend_from_slice(&[
    0xB8, 1, 0, 0, 0, // mov eax, JIT_EXIT_INTERPRETER
    0x48, 0x83, 0xC4, 0x10, // add rsp, 16
    0x41, 0x5C, // pop r12
    0xC3,
  ]);
  let guard_branches: Vec<_> = guard_branch_indices(&bytes).collect();
  for branch_index in guard_branches {
    bytes[branch_index] = relative_i8(branch_index, guard_failure)?;
  }
  Ok(NativeArtifact {
    target: TargetIsa::SysVx86_64,
    abi: NATIVE_ARTIFACT_ABI,
    entry: CodeOffset(0),
    bytes: bytes.into_boxed_slice(),
    relocations: relocations.into_boxed_slice(),
  })
}

#[cfg(all(unix, target_arch = "x86_64"))]
fn emit_argument_guard(
  bytes: &mut Vec<u8>,
  relocations: &mut Vec<ArtifactRelocation>,
  argument: u16,
  stack_offset: u8,
) -> Result<(), ArtifactError> {
  bytes.extend_from_slice(&[0x4C, 0x89, 0xE7, 0xBE]);
  bytes.extend_from_slice(&(argument as u32).to_le_bytes());
  if stack_offset == 0 {
    bytes.extend_from_slice(&[0x48, 0x89, 0xE2]); // mov rdx, rsp
  } else {
    bytes.extend_from_slice(&[0x48, 0x8D, 0x54, 0x24, stack_offset]); // lea rdx, [rsp+offset]
  }
  bytes.extend_from_slice(&[0x48, 0xB8]);
  let offset = CodeOffset::from_index(bytes.len())?;
  bytes.extend_from_slice(&[0; 8]);
  bytes.extend_from_slice(&[
    0xFF, 0xD0, // call rax
    0x85, 0xC0, // test eax, eax
    0x75, 0, // jne guard_failure; patched after emission
  ]);
  relocations.push(ArtifactRelocation {
    offset,
    helper: RuntimeHelperId::ReadDirectNumberArgument,
  });
  Ok(())
}

#[cfg(all(unix, target_arch = "x86_64"))]
fn guard_branch_indices(bytes: &[u8]) -> impl Iterator<Item = usize> + '_ {
  bytes
    .windows(2)
    .enumerate()
    .filter_map(|(index, pair)| (pair == [0x75, 0]).then_some(index + 1))
}

#[cfg(all(unix, target_arch = "x86_64"))]
fn relative_i8(
  from: usize,
  to: usize,
) -> Result<u8, ArtifactError> {
  i8::try_from(to as isize - (from + 1) as isize)
    .map(|offset| offset as u8)
    .map_err(|_| ArtifactError::TooLarge)
}

#[cfg(not(all(unix, target_arch = "x86_64")))]
pub(crate) fn emit_numeric_add(
  _function: &VerifiedTypedFunction
) -> Result<NativeArtifact, ArtifactError> {
  Err(ArtifactError::InvalidContract)
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::vm::jit::ir::{CompilerChunk, SemanticFunction};
  use crate::{compiler, parser::Parser};

  fn artifact() -> NativeArtifact {
    let session = Parser::parse_source("function add(left, right) { return left + right; }")
      .expect("source parses");
    let chunk = compiler::compile_session(&session).expect("source compiles");
    let proto = &chunk.functions[0];
    let snapshot = CompilerChunk::from_chunk(&proto.chunk, proto.arity).expect("snapshot");
    let semantic = SemanticFunction::decode(&snapshot).expect("semantic SSA");
    let typed = VerifiedTypedFunction::specialize_numeric_add(&semantic).expect("typed SSA");
    emit_numeric_add(&typed).expect("artifact")
  }

  #[cfg(all(unix, target_arch = "x86_64"))]
  #[test]
  fn emits_inert_numeric_add_artifact() {
    let artifact = artifact();
    assert!(!artifact.bytes().is_empty());
    assert_eq!(artifact.relocations().len(), 3);
    assert!(artifact.validate().is_ok());
  }

  #[cfg(all(unix, target_arch = "x86_64"))]
  #[test]
  fn validation_rejects_overlapping_relocations() {
    let mut artifact = artifact();
    artifact.relocations[1].offset = artifact.relocations[0].offset;
    assert_eq!(
      artifact
        .validate()
        .expect_err("overlap must fail"),
      ArtifactError::InvalidRelocation
    );
  }

  #[cfg(all(unix, target_arch = "x86_64"))]
  #[test]
  fn installed_artifact_adds_numbers_and_rejects_other_values() {
    use crate::value::Value;
    use crate::vm::jit::native::{
      DirectCallFrame, JIT_EXIT_INTERPRETER, JIT_EXIT_RETURN, NativeCode,
    };

    let code = NativeCode::install_artifact(artifact().validate().expect("valid artifact"))
      .expect("native installation");
    let mut result = Value::Undefined;
    let numbers = [Value::Number(2.0), Value::Number(3.0)];
    let mut frame = DirectCallFrame {
      result: &mut result,
      arguments: numbers.as_ptr(),
      argument_count: numbers.len(),
    };
    assert_eq!(
      unsafe { code.call_direct_frame(&mut frame) },
      JIT_EXIT_RETURN
    );
    assert_eq!(result, Value::Number(5.0));

    let mixed = [Value::String("value".into()), Value::Number(3.0)];
    frame.arguments = mixed.as_ptr();
    frame.argument_count = mixed.len();
    assert_eq!(
      unsafe { code.call_direct_frame(&mut frame) },
      JIT_EXIT_INTERPRETER
    );
  }
}
