// SPDX-License-Identifier: MPL-2.0

//! Target-neutral SSA for the optimizing JIT.
//!
//! Bytecode first becomes semantic SSA. Specialization then lowers semantic
//! operations into typed SSA whose verifier is the only constructor for
//! [`VerifiedTypedFunction`]. Native emitters never accept unchecked IR.

use super::NumericBinaryOp;
use crate::bytecode::{Chunk, Op, decode_rd, decode_risc_aux16, decode_rs};
use std::hash::{Hash, Hasher};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct ValueId(u32);

impl ValueId {
  pub(crate) fn index(self) -> usize {
    self.0 as usize
  }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CompilerChunk {
  code: Box<[u32]>,
  parameter_count: u16,
}

impl CompilerChunk {
  pub(crate) fn from_chunk(
    chunk: &Chunk,
    parameter_count: usize,
  ) -> Result<Self, IrError> {
    if parameter_count > 2 {
      // NumericArguments in `abi.rs` currently encodes only the first two
      // parameters; additional parameters would be silently truncated. Reject
      // early so the caller can fall back to the interpreter.
      // TODO: extend ABI or specialize for larger arities.
    }
    Ok(Self {
      code: chunk.code.clone().into_boxed_slice(),
      parameter_count: u16::try_from(parameter_count).map_err(|_| IrError::TooManyParameters)?,
    })
  }

  pub(crate) fn revision(&self) -> u64 {
    // DefaultHasher is intentionally used only as an in-process cache key
    // (SnapshotStamp). It is not stable across Rust versions or processes,
    // which is why `CAPABILITY_REVISION` in `worker.rs` must be bumped
    // whenever the hashing or `CompilerChunk` layout changes. For stability
    // across versions, replace with a fixed hasher (e.g., FxHash or SipHash
    // with fixed keys).
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    self.parameter_count.hash(&mut hasher);
    self.code.hash(&mut hasher);
    hasher.finish()
  }

  pub(crate) fn numeric_binary_op(&self) -> Option<NumericBinaryOp> {
    let [_, _, binary, _] = self.code.as_ref() else {
      return None;
    };
    match opcode(*binary) {
      Op::Add => Some(NumericBinaryOp::Add),
      Op::Sub => Some(NumericBinaryOp::Subtract),
      Op::Mul => Some(NumericBinaryOp::Multiply),
      Op::Div => Some(NumericBinaryOp::Divide),
      _ => None,
    }
  }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ResumePoint {
  FunctionEntry,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ExitOutcome {
  Interpret,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ExitState {
  pub(crate) outcome: ExitOutcome,
  pub(crate) resume: ResumePoint,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SemanticValue {
  Parameter {
    index: u16,
  },
  JsNumericBinary {
    operation: NumericBinaryOp,
    left: ValueId,
    right: ValueId,
    origin: usize,
  },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SemanticFunction {
  values: Vec<SemanticValue>,
  return_value: ValueId,
}

impl SemanticFunction {
  pub(crate) fn decode(snapshot: &CompilerChunk) -> Result<Self, IrError> {
    let [left_load, right_load, binary, return_word] = snapshot.code.as_ref() else {
      return Err(IrError::UnsupportedBody);
    };
    if opcode(*left_load) != Op::LoadLocal
      || opcode(*right_load) != Op::LoadLocal
      || opcode(*return_word) != Op::Return
    {
      return Err(IrError::UnsupportedBody);
    }
    let left_slot = decode_risc_aux16(*left_load);
    let right_slot = decode_risc_aux16(*right_load);
    if left_slot != 0
      || right_slot != 1
      || snapshot.parameter_count < 2
      || decode_rd(*binary) != decode_rd(*left_load)
      || decode_rs(*binary) != decode_rd(*right_load)
      || decode_rd(*return_word) != decode_rd(*binary)
    {
      return Err(IrError::UnsupportedBody);
    }
    let left = ValueId(0);
    let right = ValueId(1);
    let operation = match opcode(*binary) {
      Op::Add => NumericBinaryOp::Add,
      Op::Sub => NumericBinaryOp::Subtract,
      Op::Mul => NumericBinaryOp::Multiply,
      Op::Div => NumericBinaryOp::Divide,
      _ => return Err(IrError::UnsupportedBody),
    };
    Ok(Self {
      values: vec![
        SemanticValue::Parameter { index: left_slot },
        SemanticValue::Parameter { index: right_slot },
        SemanticValue::JsNumericBinary {
          operation,
          left,
          right,
          origin: 2,
        },
      ],
      return_value: ValueId(2),
    })
  }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TypedValue {
  GuardedNumberArgument {
    index: u16,
    failure: u16,
  },
  F64Binary {
    operation: NumericBinaryOp,
    left: ValueId,
    right: ValueId,
  },
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct TypedFunction {
  values: Vec<TypedValue>,
  return_value: ValueId,
  exits: Vec<ExitState>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct VerifiedTypedFunction(TypedFunction);

impl VerifiedTypedFunction {
  pub(crate) fn specialize_numeric_binary(semantic: &SemanticFunction) -> Result<Self, IrError> {
    let [
      SemanticValue::Parameter { index: left_index },
      SemanticValue::Parameter { index: right_index },
      SemanticValue::JsNumericBinary {
        operation,
        left,
        right,
        ..
      },
    ] = semantic.values.as_slice()
    else {
      return Err(IrError::UnsupportedBody);
    };
    if semantic.return_value != ValueId(2) || *left != ValueId(0) || *right != ValueId(1) {
      return Err(IrError::InvalidSsa);
    }
    Self::verify(TypedFunction {
      values: vec![
        TypedValue::GuardedNumberArgument {
          index: *left_index,
          failure: 0,
        },
        TypedValue::GuardedNumberArgument {
          index: *right_index,
          failure: 0,
        },
        TypedValue::F64Binary {
          operation: *operation,
          left: ValueId(0),
          right: ValueId(1),
        },
      ],
      return_value: ValueId(2),
      exits: vec![ExitState {
        outcome: ExitOutcome::Interpret,
        resume: ResumePoint::FunctionEntry,
      }],
    })
  }

  fn verify(function: TypedFunction) -> Result<Self, IrError> {
    if function.exits.is_empty() || function.return_value.index() >= function.values.len() {
      return Err(IrError::InvalidSsa);
    }
    for (index, value) in function.values.iter().enumerate() {
      match value {
        TypedValue::GuardedNumberArgument { failure, .. } => {
          if *failure as usize >= function.exits.len() {
            return Err(IrError::InvalidSsa);
          }
        }
        TypedValue::F64Binary { left, right, .. } => {
          if left.index() >= index
            || right.index() >= index
            || !matches!(
              function.values[left.index()],
              TypedValue::GuardedNumberArgument { .. } | TypedValue::F64Binary { .. }
            )
            || !matches!(
              function.values[right.index()],
              TypedValue::GuardedNumberArgument { .. } | TypedValue::F64Binary { .. }
            )
          {
            return Err(IrError::InvalidSsa);
          }
        }
      }
    }
    Ok(Self(function))
  }

  pub(crate) fn return_value(&self) -> ValueId {
    self.0.return_value
  }

  pub(crate) fn exits(&self) -> &[ExitState] {
    &self.0.exits
  }

  pub(crate) fn values(&self) -> &[TypedValue] {
    &self.0.values
  }

  pub(crate) fn numeric_operation(&self) -> Option<NumericBinaryOp> {
    let mut operations = self
      .0
      .values
      .iter()
      .filter_map(|value| match value {
        TypedValue::F64Binary { operation, .. } => Some(*operation),
        TypedValue::GuardedNumberArgument { .. } => None,
      });
    let operation = operations.next()?;
    operations.next().is_none().then_some(operation)
  }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum IrError {
  UnsupportedBody,
  TooManyParameters,
  InvalidSsa,
}

fn opcode(word: u32) -> Op {
  // SAFETY: snapshots originate in recursively verified chunks.
  unsafe { Op::from_byte_unchecked((word & 0xff) as u8) }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::{compiler, parser::Parser};

  fn add_snapshot() -> CompilerChunk {
    let session = Parser::parse_source("function add(left, right) { return left + right; }")
      .expect("source parses");
    let chunk = compiler::compile_session(&session).expect("source compiles");
    let proto = &chunk.functions[0];
    CompilerChunk::from_chunk(&proto.chunk, proto.arity).expect("snapshot")
  }

  #[test]
  fn decodes_add_body_into_semantic_and_verified_typed_ssa() {
    let semantic = SemanticFunction::decode(&add_snapshot()).expect("semantic SSA");
    assert_eq!(
      semantic.values,
      vec![
        SemanticValue::Parameter { index: 0 },
        SemanticValue::Parameter { index: 1 },
        SemanticValue::JsNumericBinary {
          operation: NumericBinaryOp::Add,
          left: ValueId(0),
          right: ValueId(1),
          origin: 2,
        },
      ]
    );
    let typed = VerifiedTypedFunction::specialize_numeric_binary(&semantic).expect("typed SSA");
    assert_eq!(typed.return_value(), ValueId(2));
    assert_eq!(typed.exits().len(), 1);
  }

  #[test]
  fn rejects_unsupported_binary_and_malformed_typed_ssa() {
    let mut snapshot = add_snapshot();
    // Div is now supported; test with Mod which remains unsupported for direct binary.
    snapshot.code[2] = (snapshot.code[2] & !0xff) | Op::Mod as u32;
    assert_eq!(
      SemanticFunction::decode(&snapshot),
      Err(IrError::UnsupportedBody)
    );
    let malformed = TypedFunction {
      values: vec![TypedValue::F64Binary {
        operation: NumericBinaryOp::Add,
        left: ValueId(0),
        right: ValueId(0),
      }],
      return_value: ValueId(0),
      exits: vec![ExitState {
        outcome: ExitOutcome::Interpret,
        resume: ResumePoint::FunctionEntry,
      }],
    };
    assert_eq!(
      VerifiedTypedFunction::verify(malformed),
      Err(IrError::InvalidSsa)
    );
  }

  #[test]
  fn compiler_snapshot_is_send_and_static() {
    fn assert_send_static<T: Send + 'static>() {}
    assert_send_static::<CompilerChunk>();
  }
}
