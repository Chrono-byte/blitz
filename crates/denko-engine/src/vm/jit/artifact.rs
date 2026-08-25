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
  if left != 0 || right != 1 {
    return Err(ArtifactError::UnsupportedTypedFunction);
  }
  let mut bytes = vec![
    0x48, 0x83, 0x7F, 0x10, 0x02, // cmp qword [rdi+16], 2
    0x72, 0, // jb guard_failure
    0x48, 0x8B, 0x4F, 0x08, // mov rcx, [rdi+8]
    0x48, 0x8B, 0x01, // mov rax, [rcx]
    0x48, 0x89, 0xC2, // mov rdx, rax
    0x48, 0xC1, 0xEA, 0x30, // shr rdx, 48
    0x66, 0x81, 0xFA, 0xF9, 0x7F, // cmp dx, 0x7ff9
    0x73, 0, // jae guard_failure
    0x66, 0x48, 0x0F, 0x6E, 0xC0, // movq xmm0, rax
    0x48, 0x8B, 0x41, 0x08, // mov rax, [rcx+8]
    0x48, 0x89, 0xC2, // mov rdx, rax
    0x48, 0xC1, 0xEA, 0x30, // shr rdx, 48
    0x66, 0x81, 0xFA, 0xF9, 0x7F, // cmp dx, 0x7ff9
    0x73, 0, // jae guard_failure
    0x66, 0x48, 0x0F, 0x6E, 0xC8, // movq xmm1, rax
    0xF2, 0x0F, 0x58, 0xC1, // addsd xmm0, xmm1
    0x66, 0x48, 0x0F, 0x7E, 0xC0, // movq rax, xmm0
    0x48, 0x89, 0x07, // mov [rdi], rax
    0x31, 0xC0, // xor eax, eax
    0xC3, // ret
  ];
  let guard_branches = [6, 28, 50];
  let guard_failure = bytes.len();
  bytes.extend_from_slice(&[
    0xB8, 1, 0, 0, 0, // mov eax, JIT_EXIT_INTERPRETER
    0xC3,
  ]);
  for branch_index in guard_branches {
    bytes[branch_index] = relative_i8(branch_index, guard_failure)?;
  }
  Ok(NativeArtifact {
    target: TargetIsa::SysVx86_64,
    abi: NATIVE_ARTIFACT_ABI,
    entry: CodeOffset(0),
    bytes: bytes.into_boxed_slice(),
    relocations: Box::new([]),
  })
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
