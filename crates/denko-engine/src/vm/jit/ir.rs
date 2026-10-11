// SPDX-License-Identifier: MPL-2.0

use super::NumericBinaryOp;
use crate::bytecode::{Chunk, Instruction, Opcode, decode_aux16, decode_rd, decode_rs};
use std::hash::{Hash, Hasher};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct VirtualValue(u32);

impl VirtualValue {
  pub(crate) fn index(self) -> usize {
    self.0 as usize
  }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct FunctionSnapshot {
  code: Box<[u32]>,
  parameter_count: u16,
}

impl FunctionSnapshot {
  pub(crate) fn from_code(
    code: &Chunk,
    parameter_count: usize,
  ) -> Result<Self, IrError> {
    if parameter_count > 2 {
      return Err(IrError::TooManyParameters);
    }
    Ok(Self {
      code: code.words.clone().into_boxed_slice(),
      parameter_count: u16::try_from(parameter_count).map_err(|_| IrError::TooManyParameters)?,
    })
  }

  pub(crate) fn revision(&self) -> u64 {
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
      Some(Opcode::Add) => Some(NumericBinaryOp::Add),
      Some(Opcode::Sub) => Some(NumericBinaryOp::Subtract),
      Some(Opcode::Mul) => Some(NumericBinaryOp::Multiply),
      Some(Opcode::Div) => Some(NumericBinaryOp::Divide),
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
    left: VirtualValue,
    right: VirtualValue,
    origin: usize,
  },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SemanticFunction {
  values: Vec<SemanticValue>,
  return_value: VirtualValue,
}

impl SemanticFunction {
  pub(crate) fn decode(snapshot: &FunctionSnapshot) -> Result<Self, IrError> {
    let [left_load, right_load, binary, return_word] = snapshot.code.as_ref() else {
      return Err(IrError::UnsupportedBody);
    };
    if opcode(*left_load) != Some(Opcode::LoadLocal)
      || opcode(*right_load) != Some(Opcode::LoadLocal)
      || opcode(*return_word) != Some(Opcode::Return)
    {
      return Err(IrError::UnsupportedBody);
    }
    let left_slot = decode_aux16(*left_load);
    let right_slot = decode_aux16(*right_load);
    if left_slot != 0
      || right_slot != 1
      || snapshot.parameter_count < 2
      || decode_rd(*binary) != decode_rd(*left_load)
      || decode_rs(*binary) != decode_rd(*right_load)
      || decode_rd(*return_word) != decode_rd(*binary)
    {
      return Err(IrError::UnsupportedBody);
    }
    let left = VirtualValue(0);
    let right = VirtualValue(1);
    let operation = match opcode(*binary) {
      Some(Opcode::Add) => NumericBinaryOp::Add,
      Some(Opcode::Sub) => NumericBinaryOp::Subtract,
      Some(Opcode::Mul) => NumericBinaryOp::Multiply,
      Some(Opcode::Div) => NumericBinaryOp::Divide,
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
      return_value: VirtualValue(2),
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
    left: VirtualValue,
    right: VirtualValue,
  },
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct TypedFunction {
  values: Vec<TypedValue>,
  return_value: VirtualValue,
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
    if semantic.return_value != VirtualValue(2)
      || *left != VirtualValue(0)
      || *right != VirtualValue(1)
    {
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
          left: VirtualValue(0),
          right: VirtualValue(1),
        },
      ],
      return_value: VirtualValue(2),
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

  pub(crate) fn return_value(&self) -> VirtualValue {
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

fn opcode(word: u32) -> Option<Opcode> {
  Instruction::decode(word)
    .ok()
    .map(|instruction| instruction.opcode)
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::{compiler, parser::Parser};

  fn add_snapshot() -> FunctionSnapshot {
    let session = Parser::parse_source("function add(left, right) { return left + right; }")
      .expect("source parses");
    let code = compiler::compile_session(&session).expect("source compiles");
    let proto = &code.functions[0];
    FunctionSnapshot::from_code(&proto.chunk, proto.arity).expect("snapshot")
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
          left: VirtualValue(0),
          right: VirtualValue(1),
          origin: 2,
        },
      ]
    );
    let typed = VerifiedTypedFunction::specialize_numeric_binary(&semantic).expect("typed SSA");
    assert_eq!(typed.return_value(), VirtualValue(2));
    assert_eq!(typed.exits().len(), 1);
  }

  #[test]
  fn rejects_unsupported_binary_and_malformed_typed_ssa() {
    let mut snapshot = add_snapshot();
    snapshot.code[2] = (snapshot.code[2] & !0xff) | Opcode::Mod as u32;
    assert_eq!(
      SemanticFunction::decode(&snapshot),
      Err(IrError::UnsupportedBody)
    );
    let malformed = TypedFunction {
      values: vec![TypedValue::F64Binary {
        operation: NumericBinaryOp::Add,
        left: VirtualValue(0),
        right: VirtualValue(0),
      }],
      return_value: VirtualValue(0),
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
  fn rejects_malformed_opcode_without_unchecked_decoding() {
    let mut snapshot = add_snapshot();
    snapshot.code[0] = (snapshot.code[0] & !0xff) | 0xff;
    assert_eq!(
      SemanticFunction::decode(&snapshot),
      Err(IrError::UnsupportedBody)
    );
  }

  #[test]
  fn rejects_numeric_binaries_with_more_than_two_parameters() {
    let session = Parser::parse_source("function add(left, right, extra) { return left + right; }")
      .expect("source parses");
    let code = compiler::compile_session(&session).expect("source compiles");
    let proto = &code.functions[0];
    assert_eq!(
      FunctionSnapshot::from_code(&proto.chunk, proto.arity),
      Err(IrError::TooManyParameters)
    );
  }

  #[test]
  fn compiler_snapshot_is_send_and_static() {
    fn assert_send_static<T: Send + 'static>() {}
    assert_send_static::<FunctionSnapshot>();
  }
}
