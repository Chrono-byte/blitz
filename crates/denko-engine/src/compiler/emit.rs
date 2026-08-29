// SPDX-License-Identifier: MPL-2.0
//! Compiler-only operations accepted by the AST emitter.
//!
//! Four operations model virtual-value lifetime/reordering and deliberately have
//! no representation in bytecode v2. Keeping them here prevents the public
//! bytecode contract from growing stack-machine instructions.

use crate::bytecode::Opcode;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum LoweringOpcode {
  Bytecode(Opcode),
  Discard,
  Copy,
  Exchange,
}

#[allow(non_upper_case_globals)]
impl LoweringOpcode {
  pub(crate) const Const: Self = Self::Bytecode(Opcode::Const);
  pub(crate) const Null: Self = Self::Bytecode(Opcode::Null);
  pub(crate) const True: Self = Self::Bytecode(Opcode::True);
  pub(crate) const False: Self = Self::Bytecode(Opcode::False);
  pub(crate) const Undefined: Self = Self::Bytecode(Opcode::Undefined);
  pub(crate) const LoadLocal: Self = Self::Bytecode(Opcode::LoadLocal);
  pub(crate) const StoreLocal: Self = Self::Bytecode(Opcode::StoreLocal);
  pub(crate) const Move: Self = Self::Bytecode(Opcode::Move);
  pub(crate) const Add: Self = Self::Bytecode(Opcode::Add);
  pub(crate) const Sub: Self = Self::Bytecode(Opcode::Sub);
  pub(crate) const Mul: Self = Self::Bytecode(Opcode::Mul);
  pub(crate) const Div: Self = Self::Bytecode(Opcode::Div);
  pub(crate) const Mod: Self = Self::Bytecode(Opcode::Mod);
  pub(crate) const Pow: Self = Self::Bytecode(Opcode::Pow);
  pub(crate) const Neg: Self = Self::Bytecode(Opcode::Neg);
  pub(crate) const BitAnd: Self = Self::Bytecode(Opcode::BitAnd);
  pub(crate) const BitOr: Self = Self::Bytecode(Opcode::BitOr);
  pub(crate) const BitXor: Self = Self::Bytecode(Opcode::BitXor);
  pub(crate) const Shl: Self = Self::Bytecode(Opcode::Shl);
  pub(crate) const Shr: Self = Self::Bytecode(Opcode::Shr);
  pub(crate) const Ushr: Self = Self::Bytecode(Opcode::Ushr);
  pub(crate) const CompareJump: Self = Self::Bytecode(Opcode::CompareJump);
  pub(crate) const Jump: Self = Self::Bytecode(Opcode::Jump);
  pub(crate) const JumpIfTrue: Self = Self::Bytecode(Opcode::JumpIfTrue);
  pub(crate) const JumpIfFalse: Self = Self::Bytecode(Opcode::JumpIfFalse);
  pub(crate) const JumpIfNullish: Self = Self::Bytecode(Opcode::JumpIfNullish);
  pub(crate) const And: Self = Self::Bytecode(Opcode::And);
  pub(crate) const Or: Self = Self::Bytecode(Opcode::Or);
  pub(crate) const Not: Self = Self::Bytecode(Opcode::Not);
  pub(crate) const Typeof: Self = Self::Bytecode(Opcode::Typeof);
  pub(crate) const GetProperty: Self = Self::Bytecode(Opcode::GetProperty);
  pub(crate) const SetProperty: Self = Self::Bytecode(Opcode::SetProperty);
  pub(crate) const DeleteProperty: Self = Self::Bytecode(Opcode::DeleteProperty);
  pub(crate) const HasProperty: Self = Self::Bytecode(Opcode::HasProperty);
  pub(crate) const GetGlobal: Self = Self::Bytecode(Opcode::GetGlobal);
  pub(crate) const TypeofGlobal: Self = Self::Bytecode(Opcode::TypeofGlobal);
  pub(crate) const SetGlobal: Self = Self::Bytecode(Opcode::SetGlobal);
  pub(crate) const GetUpvalue: Self = Self::Bytecode(Opcode::GetUpvalue);
  pub(crate) const SetUpvalue: Self = Self::Bytecode(Opcode::SetUpvalue);
  pub(crate) const Call: Self = Self::Bytecode(Opcode::Call);
  pub(crate) const TailCall: Self = Self::Bytecode(Opcode::TailCall);
  pub(crate) const Construct: Self = Self::Bytecode(Opcode::Construct);
  pub(crate) const CallMethod: Self = Self::Bytecode(Opcode::CallMethod);
  pub(crate) const MakeClosure: Self = Self::Bytecode(Opcode::MakeClosure);
  pub(crate) const GetThis: Self = Self::Bytecode(Opcode::GetThis);
  pub(crate) const GetNewTarget: Self = Self::Bytecode(Opcode::GetNewTarget);
  pub(crate) const GetSuperProperty: Self = Self::Bytecode(Opcode::GetSuperProperty);
  pub(crate) const BuildArray: Self = Self::Bytecode(Opcode::BuildArray);
  pub(crate) const BuildObject: Self = Self::Bytecode(Opcode::BuildObject);
  pub(crate) const ArrayFromSpread: Self = Self::Bytecode(Opcode::ArrayFromSpread);
  pub(crate) const Throw: Self = Self::Bytecode(Opcode::Throw);
  pub(crate) const PushHandler: Self = Self::Bytecode(Opcode::PushHandler);
  pub(crate) const PopHandler: Self = Self::Bytecode(Opcode::PopHandler);
  pub(crate) const IteratorNext: Self = Self::Bytecode(Opcode::IteratorNext);
  pub(crate) const IteratorClose: Self = Self::Bytecode(Opcode::IteratorClose);
  pub(crate) const Yield: Self = Self::Bytecode(Opcode::Yield);
  pub(crate) const YieldStar: Self = Self::Bytecode(Opcode::YieldStar);
  pub(crate) const Return: Self = Self::Bytecode(Opcode::Return);
  pub(crate) const PushWithEnv: Self = Self::Bytecode(Opcode::PushWithEnv);
  pub(crate) const PopEnv: Self = Self::Bytecode(Opcode::PopEnv);
  pub(crate) const AwaitPromise: Self = Self::Bytecode(Opcode::AwaitPromise);
  pub(crate) const SuperConstruct: Self = Self::Bytecode(Opcode::SuperConstruct);
  pub(crate) const GeneratorInitDone: Self = Self::Bytecode(Opcode::GeneratorInitDone);
  pub(crate) const GetModuleExport: Self = Self::Bytecode(Opcode::GetModuleExport);
  pub(crate) const SetModuleExport: Self = Self::Bytecode(Opcode::SetModuleExport);
  pub(crate) const CloneLocalCell: Self = Self::Bytecode(Opcode::CloneLocalCell);
  pub(crate) const SetSuperProperty: Self = Self::Bytecode(Opcode::SetSuperProperty);
  pub(crate) const DeleteIdentifier: Self = Self::Bytecode(Opcode::DeleteIdentifier);
  pub(crate) const GetBuiltin: Self = Self::Bytecode(Opcode::GetBuiltin);
  pub(crate) const InstanceOf: Self = Self::Bytecode(Opcode::InstanceOf);
  pub(crate) const Spread: Self = Self::Bytecode(Opcode::Spread);
  pub(crate) const ResetLocalCell: Self = Self::Bytecode(Opcode::ResetLocalCell);
  pub(crate) const LoadSpill: Self = Self::Bytecode(Opcode::LoadSpill);
  pub(crate) const StoreSpill: Self = Self::Bytecode(Opcode::StoreSpill);
  pub(crate) const BitNot: Self = Self::Bytecode(Opcode::BitNot);
  pub(crate) const GetAsyncIterator: Self = Self::Bytecode(Opcode::GetAsyncIterator);
  pub(crate) const Increment: Self = Self::Bytecode(Opcode::Increment);
  pub(crate) const Decrement: Self = Self::Bytecode(Opcode::Decrement);
  pub(crate) const PushLexicalEnv: Self = Self::Bytecode(Opcode::PushLexicalEnv);

  pub(crate) fn bytecode(self) -> Option<Opcode> {
    match self {
      Self::Bytecode(op) => Some(op),
      Self::Discard | Self::Copy | Self::Exchange => None,
    }
  }

  pub(crate) fn name(self) -> &'static str {
    match self {
      Self::Bytecode(op) => op.name(),
      Self::Discard => "discard-value",
      Self::Copy => "copy-value",
      Self::Exchange => "exchange-values",
    }
  }

  pub(crate) fn is_jump(self) -> bool {
    self.bytecode().is_some_and(|op| op.is_jump())
  }
}
