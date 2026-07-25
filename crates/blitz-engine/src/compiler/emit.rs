// SPDX-License-Identifier: MPL-2.0
//! Compiler-only operations accepted by the AST emitter.
//!
//! Four operations model virtual-value lifetime/reordering and deliberately have
//! no representation in RISC v2 bytecode. Keeping them here prevents the public
//! bytecode contract from growing stack-machine instructions.

use crate::bytecode::Op as BytecodeOp;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum FrontendOp {
  Bytecode(BytecodeOp),
  Discard,
  Copy,
  Exchange,
}

#[allow(non_upper_case_globals)]
impl FrontendOp {
  pub(crate) const Const: Self = Self::Bytecode(BytecodeOp::Const);
  pub(crate) const Null: Self = Self::Bytecode(BytecodeOp::Null);
  pub(crate) const True: Self = Self::Bytecode(BytecodeOp::True);
  pub(crate) const False: Self = Self::Bytecode(BytecodeOp::False);
  pub(crate) const Undefined: Self = Self::Bytecode(BytecodeOp::Undefined);
  pub(crate) const LoadLocal: Self = Self::Bytecode(BytecodeOp::LoadLocal);
  pub(crate) const StoreLocal: Self = Self::Bytecode(BytecodeOp::StoreLocal);
  pub(crate) const Move: Self = Self::Bytecode(BytecodeOp::Move);
  pub(crate) const Add: Self = Self::Bytecode(BytecodeOp::Add);
  pub(crate) const Sub: Self = Self::Bytecode(BytecodeOp::Sub);
  pub(crate) const Mul: Self = Self::Bytecode(BytecodeOp::Mul);
  pub(crate) const Div: Self = Self::Bytecode(BytecodeOp::Div);
  pub(crate) const Mod: Self = Self::Bytecode(BytecodeOp::Mod);
  pub(crate) const Pow: Self = Self::Bytecode(BytecodeOp::Pow);
  pub(crate) const Neg: Self = Self::Bytecode(BytecodeOp::Neg);
  pub(crate) const BitAnd: Self = Self::Bytecode(BytecodeOp::BitAnd);
  pub(crate) const BitOr: Self = Self::Bytecode(BytecodeOp::BitOr);
  pub(crate) const BitXor: Self = Self::Bytecode(BytecodeOp::BitXor);
  pub(crate) const Shl: Self = Self::Bytecode(BytecodeOp::Shl);
  pub(crate) const Shr: Self = Self::Bytecode(BytecodeOp::Shr);
  pub(crate) const Ushr: Self = Self::Bytecode(BytecodeOp::Ushr);
  pub(crate) const CompareJump: Self = Self::Bytecode(BytecodeOp::CompareJump);
  pub(crate) const Jump: Self = Self::Bytecode(BytecodeOp::Jump);
  pub(crate) const JumpIfTrue: Self = Self::Bytecode(BytecodeOp::JumpIfTrue);
  pub(crate) const JumpIfFalse: Self = Self::Bytecode(BytecodeOp::JumpIfFalse);
  pub(crate) const JumpIfNullish: Self = Self::Bytecode(BytecodeOp::JumpIfNullish);
  pub(crate) const And: Self = Self::Bytecode(BytecodeOp::And);
  pub(crate) const Or: Self = Self::Bytecode(BytecodeOp::Or);
  pub(crate) const Not: Self = Self::Bytecode(BytecodeOp::Not);
  pub(crate) const Typeof: Self = Self::Bytecode(BytecodeOp::Typeof);
  pub(crate) const GetProperty: Self = Self::Bytecode(BytecodeOp::GetProperty);
  pub(crate) const SetProperty: Self = Self::Bytecode(BytecodeOp::SetProperty);
  pub(crate) const DeleteProperty: Self = Self::Bytecode(BytecodeOp::DeleteProperty);
  pub(crate) const HasProperty: Self = Self::Bytecode(BytecodeOp::HasProperty);
  pub(crate) const GetGlobal: Self = Self::Bytecode(BytecodeOp::GetGlobal);
  pub(crate) const TypeofGlobal: Self = Self::Bytecode(BytecodeOp::TypeofGlobal);
  pub(crate) const SetGlobal: Self = Self::Bytecode(BytecodeOp::SetGlobal);
  pub(crate) const GetUpvalue: Self = Self::Bytecode(BytecodeOp::GetUpvalue);
  pub(crate) const SetUpvalue: Self = Self::Bytecode(BytecodeOp::SetUpvalue);
  pub(crate) const Call: Self = Self::Bytecode(BytecodeOp::Call);
  pub(crate) const TailCall: Self = Self::Bytecode(BytecodeOp::TailCall);
  pub(crate) const Construct: Self = Self::Bytecode(BytecodeOp::Construct);
  pub(crate) const CallBuiltin: Self = Self::Bytecode(BytecodeOp::CallBuiltin);
  pub(crate) const MakeClosure: Self = Self::Bytecode(BytecodeOp::MakeClosure);
  pub(crate) const GetThis: Self = Self::Bytecode(BytecodeOp::GetThis);
  pub(crate) const GetNewTarget: Self = Self::Bytecode(BytecodeOp::GetNewTarget);
  pub(crate) const GetSuperProperty: Self = Self::Bytecode(BytecodeOp::GetSuperProperty);
  pub(crate) const BuildArray: Self = Self::Bytecode(BytecodeOp::BuildArray);
  pub(crate) const BuildObject: Self = Self::Bytecode(BytecodeOp::BuildObject);
  pub(crate) const ArrayFromSpread: Self = Self::Bytecode(BytecodeOp::ArrayFromSpread);
  pub(crate) const Throw: Self = Self::Bytecode(BytecodeOp::Throw);
  pub(crate) const PushHandler: Self = Self::Bytecode(BytecodeOp::PushHandler);
  pub(crate) const PopHandler: Self = Self::Bytecode(BytecodeOp::PopHandler);
  pub(crate) const IteratorNext: Self = Self::Bytecode(BytecodeOp::IteratorNext);
  pub(crate) const IteratorClose: Self = Self::Bytecode(BytecodeOp::IteratorClose);
  pub(crate) const Yield: Self = Self::Bytecode(BytecodeOp::Yield);
  pub(crate) const YieldStar: Self = Self::Bytecode(BytecodeOp::YieldStar);
  pub(crate) const Return: Self = Self::Bytecode(BytecodeOp::Return);
  pub(crate) const PushWithEnv: Self = Self::Bytecode(BytecodeOp::PushWithEnv);
  pub(crate) const PopEnv: Self = Self::Bytecode(BytecodeOp::PopEnv);
  pub(crate) const AwaitPromise: Self = Self::Bytecode(BytecodeOp::AwaitPromise);
  pub(crate) const SuperConstruct: Self = Self::Bytecode(BytecodeOp::SuperConstruct);
  pub(crate) const GeneratorInitDone: Self = Self::Bytecode(BytecodeOp::GeneratorInitDone);
  pub(crate) const GetModuleExport: Self = Self::Bytecode(BytecodeOp::GetModuleExport);
  pub(crate) const SetModuleExport: Self = Self::Bytecode(BytecodeOp::SetModuleExport);
  pub(crate) const CloneLocalCell: Self = Self::Bytecode(BytecodeOp::CloneLocalCell);
  pub(crate) const SetSuperProperty: Self = Self::Bytecode(BytecodeOp::SetSuperProperty);
  pub(crate) const DeleteIdentifier: Self = Self::Bytecode(BytecodeOp::DeleteIdentifier);
  pub(crate) const GetBuiltin: Self = Self::Bytecode(BytecodeOp::GetBuiltin);
  pub(crate) const InstanceOf: Self = Self::Bytecode(BytecodeOp::InstanceOf);
  pub(crate) const Spread: Self = Self::Bytecode(BytecodeOp::Spread);
  pub(crate) const ResetLocalCell: Self = Self::Bytecode(BytecodeOp::ResetLocalCell);
  pub(crate) const LoadSpill: Self = Self::Bytecode(BytecodeOp::LoadSpill);
  pub(crate) const StoreSpill: Self = Self::Bytecode(BytecodeOp::StoreSpill);
  pub(crate) const BitNot: Self = Self::Bytecode(BytecodeOp::BitNot);

  pub(crate) fn bytecode(self) -> Option<BytecodeOp> {
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
