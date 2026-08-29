// SPDX-License-Identifier: MPL-2.0

//! Inert native artifacts produced away from the VM thread.

use super::ir::{ExitOutcome, ResumePoint, TypedValue, VerifiedTypedFunction};
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
pub(crate) fn emit_numeric_binary(
  function: &VerifiedTypedFunction
) -> Result<NativeArtifact, ArtifactError> {
  let lowering = NumericBinaryLowering::from_verified(function)?;
  let mut code = ShortBranchCode::default();
  code.emit(&[0x48, 0x83, 0x7F, 0x10, 0x02]); // cmp qword [rdi+16], 2
  code.guard_branch(0x72)?; // jb guard_failure
  code.emit(&[0x48, 0x8B, 0x4F, 0x08]); // mov rcx, [rdi+8]
  code.emit_number_argument(0, 0xC0)?;
  code.emit_number_argument(1, 0xC8)?;
  code.emit(&[0xF2, 0x0F]);
  code.emit(&[match lowering.operation {
    super::NumericBinaryOp::Add => 0x58,
    super::NumericBinaryOp::Subtract => 0x5C,
    super::NumericBinaryOp::Multiply => 0x59,
    super::NumericBinaryOp::Divide => 0x5E,
  }]);
  code.emit(&[0xC1]); // scalar binary operation xmm0, xmm1
  code.emit(&[0x66, 0x48, 0x0F, 0x7E, 0xC0]); // movq rax, xmm0
  code.emit(&[0x48, 0x89, 0x07]); // mov [rdi], rax
  code.emit(&[0x31, 0xC0, 0xC3]); // return JIT_EXIT_RETURN
  code.bind_guard_failure()?;
  code.emit(&[
    0xB8, 1, 0, 0, 0, // mov eax, JIT_EXIT_INTERPRETER
    0xC3,
  ]);
  Ok(NativeArtifact {
    target: TargetIsa::SysVx86_64,
    abi: NATIVE_ARTIFACT_ABI,
    entry: CodeOffset(0),
    bytes: code.finish().into_boxed_slice(),
    relocations: Box::new([]),
  })
}

#[cfg(all(unix, target_arch = "x86_64"))]
struct NumericBinaryLowering {
  operation: super::NumericBinaryOp,
}

#[cfg(all(unix, target_arch = "x86_64"))]
impl NumericBinaryLowering {
  fn from_verified(function: &VerifiedTypedFunction) -> Result<Self, ArtifactError> {
    let mut guards = Vec::new();
    let mut binary = None;
    for (value_index, value) in function.values().iter().enumerate() {
      match *value {
        TypedValue::GuardedNumberArgument { index, failure } => {
          guards.push((value_index, index, failure));
        }
        TypedValue::F64Binary {
          operation,
          left,
          right,
        } => {
          if binary.is_some() {
            return Err(ArtifactError::UnsupportedTypedFunction);
          }
          binary = Some((value_index, operation, left.index(), right.index()));
        }
      }
    }
    let [
      (left_value, 0, left_failure),
      (right_value, 1, right_failure),
    ] = guards.as_slice()
    else {
      return Err(ArtifactError::UnsupportedTypedFunction);
    };
    let Some((result_value, operation, left, right)) = binary else {
      return Err(ArtifactError::UnsupportedTypedFunction);
    };
    let Some(exit) = function.exits().get(*left_failure as usize) else {
      return Err(ArtifactError::InvalidContract);
    };
    if left != *left_value
      || right != *right_value
      || left_failure != right_failure
      || function.return_value().index() != result_value
      || exit.outcome != ExitOutcome::Interpret
      || exit.resume != ResumePoint::FunctionEntry
    {
      return Err(ArtifactError::UnsupportedTypedFunction);
    }
    Ok(Self { operation })
  }
}

#[cfg(all(unix, target_arch = "x86_64"))]
#[derive(Default)]
struct ShortBranchCode {
  bytes: Vec<u8>,
  guard_fixups: Vec<usize>,
}

#[cfg(all(unix, target_arch = "x86_64"))]
impl ShortBranchCode {
  fn emit(
    &mut self,
    bytes: &[u8],
  ) {
    self.bytes.extend_from_slice(bytes);
  }

  /// Emit a conditional branch to the guard failure block using a 32-bit
  /// displacement. The previous 8-bit form (`jb`/`jae` rel8) only allowed
  /// ±127 bytes and would fail with `TooLarge` for larger artifacts. Using
  /// the `0F 82/83` rel32 form supports the full 64 KiB artifact limit.
  fn guard_branch(
    &mut self,
    opcode: u8,
  ) -> Result<(), ArtifactError> {
    let long_opcode = match opcode {
      0x72 => 0x82, // jb rel32
      0x73 => 0x83, // jae rel32
      _ => return Err(ArtifactError::InvalidContract),
    };
    self.bytes.push(0x0F);
    self.bytes.push(long_opcode);
    self.guard_fixups.push(self.bytes.len());
    self.bytes.extend_from_slice(&[0, 0, 0, 0]);
    Ok(())
  }

  fn emit_number_argument(
    &mut self,
    index: u16,
    xmm_move: u8,
  ) -> Result<(), ArtifactError> {
    match index {
      0 => self.emit(&[0x48, 0x8B, 0x01]),       // mov rax, [rcx]
      1 => self.emit(&[0x48, 0x8B, 0x41, 0x08]), // mov rax, [rcx+8]
      _ => return Err(ArtifactError::UnsupportedTypedFunction),
    }
    self.emit(&[0x48, 0x89, 0xC2, 0x48, 0xC1, 0xEA, 0x30]); // high word into dx
    self.emit(&[0x66, 0x81, 0xE2, 0xFF, 0x7F]); // ignore IEEE sign
    self.emit(&[0x66, 0x81, 0xFA]);
    self.emit(&super::abi::FIRST_NON_NUMBER_HIGH_WORD.to_le_bytes());
    self.guard_branch(0x73)?; // jae guard_failure
    self.emit(&[0x66, 0x48, 0x0F, 0x6E, xmm_move]); // movq xmm, rax
    Ok(())
  }

  fn bind_guard_failure(&mut self) -> Result<(), ArtifactError> {
    let target = self.bytes.len();
    for fixup in self.guard_fixups.drain(..) {
      let rel = relative_i32(fixup, target)?;
      self.bytes[fixup..fixup + 4].copy_from_slice(&rel.to_le_bytes());
    }
    Ok(())
  }

  fn finish(self) -> Vec<u8> {
    self.bytes
  }
}

#[cfg(all(unix, target_arch = "x86_64"))]
fn relative_i32(
  from: usize,
  to: usize,
) -> Result<i32, ArtifactError> {
  i32::try_from(to as isize - (from + 4) as isize).map_err(|_| ArtifactError::TooLarge)
}

#[cfg(not(all(unix, target_arch = "x86_64")))]
pub(crate) fn emit_numeric_binary(
  _function: &VerifiedTypedFunction
) -> Result<NativeArtifact, ArtifactError> {
  Err(ArtifactError::InvalidContract)
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::vm::jit::ir::{CompilerChunk, SemanticFunction};
  use crate::{compiler, parser::Parser};

  fn artifact_for(source: &str) -> NativeArtifact {
    let session = Parser::parse_source(source).expect("source parses");
    let chunk = compiler::compile_session(&session).expect("source compiles");
    let proto = &chunk.functions[0];
    let snapshot = CompilerChunk::from_chunk(&proto.chunk, proto.arity).expect("snapshot");
    let semantic = SemanticFunction::decode(&snapshot).expect("semantic SSA");
    let typed = VerifiedTypedFunction::specialize_numeric_binary(&semantic).expect("typed SSA");
    emit_numeric_binary(&typed).expect("artifact")
  }

  fn artifact() -> NativeArtifact {
    artifact_for("function add(left, right) { return left + right; }")
  }

  #[cfg(all(unix, target_arch = "x86_64"))]
  #[test]
  fn emits_inert_numeric_add_artifact() {
    let artifact = artifact();
    assert!(!artifact.bytes().is_empty());
    assert!(artifact.relocations().is_empty());
    assert!(artifact.validate().is_ok());
  }

  #[cfg(all(unix, target_arch = "x86_64"))]
  #[test]
  fn validation_rejects_overlapping_relocations() {
    let mut artifact = artifact();
    artifact.relocations = vec![
      ArtifactRelocation {
        offset: CodeOffset(0),
        helper: RuntimeHelperId::SetNumberResult,
      },
      ArtifactRelocation {
        offset: CodeOffset(0),
        helper: RuntimeHelperId::SetNumberResult,
      },
    ]
    .into_boxed_slice();
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
    use crate::vm::jit::abi::NumericArguments;
    use crate::vm::jit::native::{
      CompactDirectCallFrame, JIT_EXIT_INTERPRETER, JIT_EXIT_RETURN, NativeCode,
    };

    let code = NativeCode::install_artifact(artifact().validate().expect("valid artifact"))
      .expect("native installation");
    let numbers = NumericArguments::from_values(&[Value::Number(2.0), Value::Number(3.0)]);
    let mut frame = CompactDirectCallFrame {
      result_bits: 0,
      arguments: numbers.as_ptr(),
      argument_count: numbers.argument_count(),
    };
    assert_eq!(
      unsafe { code.call_compact_direct_frame(&mut frame) },
      JIT_EXIT_RETURN
    );
    assert_eq!(f64::from_bits(frame.result_bits), 5.0);

    let negative_numbers = [(-2.0f64).to_bits(), 3.0f64.to_bits()];
    frame.arguments = negative_numbers.as_ptr();
    assert_eq!(
      unsafe { code.call_compact_direct_frame(&mut frame) },
      JIT_EXIT_RETURN
    );
    assert_eq!(f64::from_bits(frame.result_bits), 1.0);

    let mixed = NumericArguments::from_values(&[Value::Undefined, Value::Number(3.0)]);
    frame.arguments = mixed.as_ptr();
    frame.argument_count = mixed.argument_count();
    assert_eq!(
      unsafe { code.call_compact_direct_frame(&mut frame) },
      JIT_EXIT_INTERPRETER
    );
  }

  #[cfg(all(unix, target_arch = "x86_64"))]
  #[test]
  fn installed_artifact_divides_numbers_with_ieee754_results() {
    use crate::value::Value;
    use crate::vm::jit::abi::NumericArguments;
    use crate::vm::jit::native::{CompactDirectCallFrame, JIT_EXIT_RETURN, NativeCode};

    let artifact = artifact_for("function divide(left, right) { return left / right; }");
    let code = NativeCode::install_artifact(artifact.validate().expect("valid artifact"))
      .expect("native installation");

    let cases = [
      (
        &[Value::Number(6.0), Value::Number(2.0)][..],
        3.0f64.to_bits(),
      ),
      (
        &[Value::Number(1.0), Value::Number(0.0)][..],
        f64::INFINITY.to_bits(),
      ),
      (
        &[Value::Number(-0.0), Value::Number(2.0)][..],
        (-0.0f64).to_bits(),
      ),
    ];
    for (values, expected_bits) in cases {
      let numbers = NumericArguments::from_values(values);
      let mut frame = CompactDirectCallFrame {
        result_bits: 0,
        arguments: numbers.as_ptr(),
        argument_count: numbers.argument_count(),
      };
      assert_eq!(
        unsafe { code.call_compact_direct_frame(&mut frame) },
        JIT_EXIT_RETURN
      );
      assert_eq!(frame.result_bits, expected_bits);
    }
  }

  #[cfg(all(unix, target_arch = "x86_64"))]
  #[test]
  fn guard_branch_supports_large_displacement() {
    let mut code = ShortBranchCode::default();
    code
      .guard_branch(0x72)
      .expect("known guard opcode"); // jb
    code.emit(&[0x90; 200]); // nop
    code
      .bind_guard_failure()
      .expect("large guard should succeed with rel32");
    assert!(code.bytes.len() > 200);
    let typed_artifact = artifact();
    assert!(typed_artifact.validate().is_ok());
  }
}
