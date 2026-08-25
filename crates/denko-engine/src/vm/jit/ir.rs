// SPDX-License-Identifier: MPL-2.0

//! Target-neutral SSA for the optimizing JIT.
//!
//! Bytecode first becomes semantic SSA. Specialization then lowers semantic
//! operations into typed SSA whose verifier is the only constructor for
//! [`VerifiedTypedFunction`]. Native emitters never accept unchecked IR.

use crate::bytecode::{Chunk, Op, decode_rd, decode_risc_aux16, decode_rs};
use std::hash::{Hash, Hasher};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct ValueId(u32);

impl ValueId {
  fn from_index(index: usize) -> Result<Self, IrError> {
    u32::try_from(index)
      .map(Self)
      .map_err(|_| IrError::TooManyValues)
  }

  fn index(self) -> usize {
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
    Ok(Self {
      code: chunk.code.clone().into_boxed_slice(),
      parameter_count: u16::try_from(parameter_count).map_err(|_| IrError::TooManyParameters)?,
    })
  }

  pub(crate) fn revision(&self) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    self.parameter_count.hash(&mut hasher);
    self.code.hash(&mut hasher);
    hasher.finish()
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
  JsAdd {
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
    let [left_load, right_load, add, return_word] = snapshot.code.as_ref() else {
      return Err(IrError::UnsupportedBody);
    };
    if opcode(*left_load) != Op::LoadLocal
      || opcode(*right_load) != Op::LoadLocal
      || opcode(*add) != Op::Add
      || opcode(*return_word) != Op::Return
    {
      return Err(IrError::UnsupportedBody);
    }
    let left_slot = decode_risc_aux16(*left_load);
    let right_slot = decode_risc_aux16(*right_load);
    if left_slot != 0
      || right_slot != 1
      || snapshot.parameter_count < 2
      || decode_rd(*add) != decode_rd(*left_load)
      || decode_rs(*add) != decode_rd(*right_load)
      || decode_rd(*return_word) != decode_rd(*add)
    {
      return Err(IrError::UnsupportedBody);
    }
    let left = ValueId(0);
    let right = ValueId(1);
    let sum = ValueId(2);
    Ok(Self {
      values: vec![
        SemanticValue::Parameter { index: left_slot },
        SemanticValue::Parameter { index: right_slot },
        SemanticValue::JsAdd {
          left,
          right,
          origin: 2,
        },
      ],
      return_value: sum,
    })
  }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TypedValue {
  GuardedNumberArgument { index: u16, failure: u16 },
  F64Add { left: ValueId, right: ValueId },
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
  pub(crate) fn specialize_numeric_add(semantic: &SemanticFunction) -> Result<Self, IrError> {
    let [
      SemanticValue::Parameter { index: left_index },
      SemanticValue::Parameter { index: right_index },
      SemanticValue::JsAdd { left, right, .. },
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
        TypedValue::F64Add {
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
        TypedValue::F64Add { left, right } => {
          if left.index() >= index
            || right.index() >= index
            || !matches!(
              function.values[left.index()],
              TypedValue::GuardedNumberArgument { .. } | TypedValue::F64Add { .. }
            )
            || !matches!(
              function.values[right.index()],
              TypedValue::GuardedNumberArgument { .. } | TypedValue::F64Add { .. }
            )
          {
            return Err(IrError::InvalidSsa);
          }
        }
      }
    }
    Ok(Self(function))
  }

  pub(crate) fn values(&self) -> &[TypedValue] {
    &self.0.values
  }

  pub(crate) fn return_value(&self) -> ValueId {
    self.0.return_value
  }

  pub(crate) fn exits(&self) -> &[ExitState] {
    &self.0.exits
  }

  pub(crate) fn numeric_add_arguments(&self) -> Option<(u16, u16)> {
    let [
      TypedValue::GuardedNumberArgument {
        index: left_index,
        failure: left_failure,
      },
      TypedValue::GuardedNumberArgument {
        index: right_index,
        failure: right_failure,
      },
      TypedValue::F64Add { left, right },
    ] = self.0.values.as_slice()
    else {
      return None;
    };
    (*left_failure == 0
      && *right_failure == 0
      && *left == ValueId(0)
      && *right == ValueId(1)
      && self.0.return_value == ValueId(2))
    .then_some((*left_index, *right_index))
  }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum IrError {
  UnsupportedBody,
  TooManyParameters,
  TooManyValues,
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
        SemanticValue::JsAdd {
          left: ValueId(0),
          right: ValueId(1),
          origin: 2,
        },
      ]
    );
    let typed = VerifiedTypedFunction::specialize_numeric_add(&semantic).expect("typed SSA");
    assert_eq!(typed.return_value(), ValueId(2));
    assert_eq!(typed.exits().len(), 1);
  }

  #[test]
  fn rejects_non_add_and_malformed_typed_ssa() {
    let mut snapshot = add_snapshot();
    snapshot.code[2] = (snapshot.code[2] & !0xff) | Op::Mul as u32;
    assert_eq!(
      SemanticFunction::decode(&snapshot),
      Err(IrError::UnsupportedBody)
    );
    let malformed = TypedFunction {
      values: vec![TypedValue::F64Add {
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

  #[test]
  fn value_ids_fail_when_the_ir_limit_is_exceeded() {
    assert_eq!(ValueId::from_index(usize::MAX), Err(IrError::TooManyValues));
  }
}
