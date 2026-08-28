// SPDX-License-Identifier: MPL-2.0
//! JIT-owned metadata. This module deliberately contains no native code yet.
//!
//! The interpreter and future JIT share these stable, VM-PC keyed identities and
//! feedback records. Generated machine code must never be the owner of feedback.
use crate::value::Value;
mod abi;
pub(crate) mod artifact;
pub(crate) mod ir;
pub(crate) mod native;
mod worker;

use crate::bytecode::{Chunk, Constant, Op};
use native::NativeCode;
use std::collections::HashMap;
use std::rc::Rc;
use worker::{
  CompilationKey, CompileCompletion, CompileRequest, CompileTicket, CompilerWorker, SnapshotStamp,
  SpecializationKey,
};

struct BaselineCompiler<'a> {
  chunk: &'a Chunk,
}

impl<'a> BaselineCompiler<'a> {
  fn compile_direct_leaf(
    &self,
    parameter_count: usize,
  ) -> Result<(NativeCode, DirectLeaf), String> {
    let Some([load, return_word]) = self.chunk.code.get(..2) else {
      return Err("direct leaf requires a load and return".to_string());
    };
    let load_register = (load >> 8) & 0xf;
    if (*return_word & 0xff) as u8 != Op::Return as u8
      || ((*return_word >> 8) & 0xf) != load_register
    {
      return Err("direct leaf must return its loaded register".to_string());
    }
    if (*load & 0xff) as u8 == Op::Const as u8 {
      let Constant::Number(value) = self
        .chunk
        .constants
        .get(((load >> 16) & 0xffff) as usize)
        .ok_or_else(|| "constant index out of bounds".to_string())?
      else {
        return Err("direct leaf constants currently support numbers only".to_string());
      };
      return Ok((
        NativeCode::direct_return_number(*value)?,
        DirectLeaf::Number(*value),
      ));
    }
    if (*load & 0xff) as u8 == Op::LoadLocal as u8 {
      let slot = ((load >> 16) & 0xffff) as usize;
      if slot >= parameter_count {
        return Err("direct leaf local is not a parameter".to_string());
      }
      return Ok((
        NativeCode::direct_return_argument(slot as u32)?,
        DirectLeaf::Parameter(slot as u16),
      ));
    }
    Err("unsupported direct leaf load".to_string())
  }

  fn compile_const_prefix(self) -> Result<NativeCode, String> {
    if self.chunk.code.is_empty() {
      return Err("baseline requires at least one instruction".to_string());
    }
    let mut registers = [None; 16];
    let mut visited = vec![false; self.chunk.code.len()];
    let mut pc = 0;
    loop {
      if pc >= self.chunk.code.len() {
        return Err("baseline control flow left the chunk".to_string());
      }
      if visited[pc] {
        return self.resume_materialized(&registers, pc);
      }
      visited[pc] = true;
      let word = self.chunk.code[pc];
      let op = (word & 0xff) as u8;
      let rd = ((word >> 8) & 0xf) as usize;
      let rs = ((word >> 12) & 0xf) as usize;
      if op == Op::Const as u8 {
        let constant_index = ((word >> 16) & 0xffff) as usize;
        let Constant::Number(value) = self
          .chunk
          .constants
          .get(constant_index)
          .ok_or_else(|| "constant index out of bounds".to_string())?
        else {
          return Err("baseline constants currently support numbers only".to_string());
        };
        registers[rd] = Some(*value);
        pc += 1;
      } else if op == Op::Move as u8 {
        registers[rd] = registers[rs];
        pc += 1;
      } else if op == Op::Jump as u8 {
        let payload = ((word >> 8) & 0x00ff_ffff) as i32;
        let offset = if payload & 0x0080_0000 != 0 {
          payload | !0x00ff_ffff
        } else {
          payload
        };
        pc = usize::try_from(pc as i64 + 1 + offset as i64)
          .map_err(|_| "negative baseline jump target".to_string())?;
      } else if matches!(
        Op::from_byte(op),
        Some(Op::JumpIfFalse | Op::JumpIfTrue | Op::JumpIfNullish)
      ) {
        let Some(value) = registers[rd] else {
          return self.resume_materialized(&registers, pc);
        };
        // Baseline only tracks numeric constants (`Option<f64>`). Numbers are
        // never Null/Undefined, so `JumpIfNullish` on a known number is
        // always not taken. Unknown values deopt via `resume_materialized`
        // above, where the interpreter handles the correct nullish semantics.
        let truthy = value != 0.0 && !value.is_nan();
        let take = match Op::from_byte(op) {
          Some(Op::JumpIfFalse) => !truthy,
          Some(Op::JumpIfTrue) => truthy,
          Some(Op::JumpIfNullish) => false,
          _ => unreachable!(),
        };
        if take {
          pc =
            usize::try_from(pc as i64 + 1 + crate::bytecode::decode_risc_cond_offset(word) as i64)
              .map_err(|_| "negative baseline conditional target".to_string())?;
        } else {
          pc += 1;
        }
      } else if op == Op::Return as u8 {
        return registers[rd]
          .map(NativeCode::return_number)
          .unwrap_or_else(|| NativeCode::return_register(rd as u32));
      } else {
        return self.resume_materialized(&registers, pc);
      }
    }
  }

  fn compile_block(
    &self,
    block: &CompiledBlock,
  ) -> Result<NativeCode, String> {
    let mut registers = [None; 16];
    let mut local_registers = [None; 16];
    let mut pc = block.start_pc;
    while pc < block.end_pc {
      let word = self.chunk.code[pc];
      let op = (word & 0xff) as u8;
      let rd = ((word >> 8) & 0xf) as usize;
      let rs = ((word >> 12) & 0xf) as usize;
      if op == Op::Const as u8 {
        let Constant::Number(value) = self
          .chunk
          .constants
          .get(((word >> 16) & 0xffff) as usize)
          .ok_or_else(|| "constant index out of bounds".to_string())?
        else {
          return Err("native blocks currently support numeric constants only".to_string());
        };
        registers[rd] = Some(*value);
        local_registers[rd] = None;
      } else if op == Op::LoadLocal as u8 {
        registers[rd] = None;
        local_registers[rd] = Some(((word >> 16) & 0xffff) as u16);
      } else if op == Op::Move as u8 {
        registers[rd] = registers[rs];
        local_registers[rd] = local_registers[rs];
        if registers[rd].is_none() && local_registers[rd].is_none() {
          return Err("block Move depends on incoming VM state".to_string());
        }
      } else if op == Op::Return as u8 {
        if let Some(value) = registers[rd] {
          return NativeCode::return_number(value);
        }
        if let Some(slot) = local_registers[rd] {
          return NativeCode::return_local(slot as u32);
        }
        return NativeCode::return_register(rd as u32);
      } else if matches!(
        Op::from_byte(op),
        Some(Op::JumpIfFalse | Op::JumpIfTrue | Op::JumpIfNullish)
      ) {
        let [taken, fallthrough] = block.successors.as_slice() else {
          return Err("conditional block does not have two successors".to_string());
        };
        let condition = match Op::from_byte(op) {
          Some(Op::JumpIfFalse) => 0,
          Some(Op::JumpIfTrue) => 1,
          Some(Op::JumpIfNullish) => 2,
          _ => unreachable!(),
        };
        if let Some(value) = registers[rd] {
          // See comment in `compile_const_prefix`: numeric constants are never
          // nullish.
          let truthy = value != 0.0 && !value.is_nan();
          let take = match condition {
            0 => !truthy,
            1 => truthy,
            2 => false,
            _ => unreachable!(),
          };
          return self
            .next_block_materialized(&registers, if take { *taken } else { *fallthrough });
        }
        if registers.iter().any(Option::is_some) {
          return Err("conditional block has unmaterialized constants".to_string());
        }
        return NativeCode::conditional_next_block(
          rd as u32,
          condition,
          *taken as u32,
          *fallthrough as u32,
        );
      } else if op == Op::Jump as u8 {
        let Some(target) = block.successors.first().copied() else {
          return Err("jump block has no successor".to_string());
        };
        return self.next_block_materialized(&registers, target);
      } else {
        return Err(format!("unsupported opcode in native block at {pc}"));
      }
      pc += 1;
    }
    let Some(target) = block.successors.first().copied() else {
      return Err("unterminated native block".to_string());
    };
    self.next_block_materialized(&registers, target)
  }

  fn materialized_values(registers: &[Option<f64>; 16]) -> Vec<(u32, f64)> {
    registers
      .iter()
      .enumerate()
      .filter_map(|(register, value)| value.map(|value| (register as u32, value)))
      .collect()
  }

  fn resume_materialized(
    &self,
    registers: &[Option<f64>; 16],
    pc: usize,
  ) -> Result<NativeCode, String> {
    let values = Self::materialized_values(registers);
    if values.is_empty() {
      return Err("baseline prefix has no materializable state".to_string());
    }
    NativeCode::resume_numbers(&values, pc as u32)
  }

  fn next_block_materialized(
    &self,
    registers: &[Option<f64>; 16],
    pc: usize,
  ) -> Result<NativeCode, String> {
    NativeCode::next_block_numbers(&Self::materialized_values(registers), pc as u32)
  }
}

pub(crate) const BASELINE_CALL_THRESHOLD: u64 = 1_000;
pub(crate) const BASELINE_LOOP_THRESHOLD: u64 = 1_000;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct FunctionId(u64);

impl FunctionId {
  pub(crate) const fn new(value: u64) -> Self {
    Self(value)
  }

  const fn get(self) -> u64 {
    self.0
  }
}

pub(crate) type VmPc = usize;

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct JitStats {
  pub(crate) baseline_entries: u64,
  pub(crate) baseline_returns: u64,
  pub(crate) interpreter_resumes: u64,
  pub(crate) native_loop_entries: u64,
  pub(crate) native_loop_iterations: u64,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct CodeVersionId(u64);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum JitCodeState {
  Valid,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CompiledBlock {
  pub(crate) start_pc: VmPc,
  pub(crate) end_pc: VmPc,
  pub(crate) successors: Vec<VmPc>,
}

fn discover_blocks(chunk: &Chunk) -> Result<Vec<CompiledBlock>, String> {
  use std::collections::BTreeSet;

  if chunk.code.is_empty() {
    return Ok(Vec::new());
  }
  let mut leaders = BTreeSet::from([0]);
  for (pc, word) in chunk.code.iter().copied().enumerate() {
    let op = Op::from_byte((word & 0xff) as u8).ok_or_else(|| format!("invalid opcode at {pc}"))?;
    let offset = match op {
      Op::Jump => {
        let payload = ((word >> 8) & 0x00ff_ffff) as i32;
        Some(if payload & 0x0080_0000 != 0 {
          payload | !0x00ff_ffff
        } else {
          payload
        })
      }
      Op::JumpIfFalse | Op::JumpIfTrue | Op::JumpIfNullish => {
        Some(crate::bytecode::decode_risc_cond_offset(word))
      }
      Op::CompareJump => Some(crate::bytecode::decode_compare_jump(word).3 as i32),
      _ => None,
    };
    if let Some(offset) = offset {
      let target = usize::try_from(pc as i64 + 1 + offset as i64)
        .map_err(|_| format!("negative branch target at {pc}"))?;
      if target >= chunk.code.len() {
        return Err(format!("branch target {target} is outside the chunk"));
      }
      leaders.insert(target);
      if pc + 1 < chunk.code.len() {
        leaders.insert(pc + 1);
      }
    } else if matches!(op, Op::Return | Op::Throw) && pc + 1 < chunk.code.len() {
      leaders.insert(pc + 1);
    }
  }

  let leaders: Vec<_> = leaders.into_iter().collect();
  let mut blocks = Vec::with_capacity(leaders.len());
  for (index, start_pc) in leaders.iter().copied().enumerate() {
    let end_pc = leaders
      .get(index + 1)
      .copied()
      .unwrap_or(chunk.code.len());
    let terminal_pc = end_pc - 1;
    let word = chunk.code[terminal_pc];
    let op =
      Op::from_byte((word & 0xff) as u8).ok_or_else(|| "invalid terminal opcode".to_string())?;
    let mut successors = Vec::with_capacity(2);
    match op {
      Op::Jump => {
        let payload = ((word >> 8) & 0x00ff_ffff) as i32;
        let offset = if payload & 0x0080_0000 != 0 {
          payload | !0x00ff_ffff
        } else {
          payload
        };
        successors.push((terminal_pc as i64 + 1 + offset as i64) as usize);
      }
      Op::JumpIfFalse | Op::JumpIfTrue | Op::JumpIfNullish => {
        let target =
          (terminal_pc as i64 + 1 + crate::bytecode::decode_risc_cond_offset(word) as i64) as usize;
        successors.push(target);
        if end_pc < chunk.code.len() {
          successors.push(end_pc);
        }
      }
      Op::CompareJump => {
        let target =
          (terminal_pc as i64 + 1 + crate::bytecode::decode_compare_jump(word).3 as i64) as usize;
        successors.push(target);
        if end_pc < chunk.code.len() {
          successors.push(end_pc);
        }
      }
      Op::Return | Op::Throw => {}
      _ if end_pc < chunk.code.len() => successors.push(end_pc),
      _ => {}
    }
    blocks.push(CompiledBlock {
      start_pc,
      end_pc,
      successors,
    });
  }
  Ok(blocks)
}

#[derive(Debug)]
pub(crate) struct NativeBlock {
  code: NativeCode,
  #[cfg(test)]
  direct_exit: Option<native::PatchSite>,
}

#[derive(Debug)]
pub(crate) struct JitCode {
  pub(crate) version: CodeVersionId,
  pub(crate) state: JitCodeState,
  pub(crate) preferred_entry: VmPc,
  native_blocks: HashMap<VmPc, NativeBlock>,
  direct_call: Option<DirectCallTarget>,
}

#[derive(Clone, Debug)]
pub(crate) struct DirectCallTarget {
  code: Rc<NativeCode>,
  leaf: DirectLeaf,
  contract: DirectCallContract,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum DirectLeaf {
  Number(f64),
  Parameter(u16),
  NumericBinary(NumericBinaryOp),
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum NumericBinaryOp {
  Add,
  Subtract,
  Multiply,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DirectArgumentUse {
  None,
  Parameter(u16),
  NumericPair,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct DirectCallContract {
  pub(crate) arguments: DirectArgumentUse,
}

pub(crate) struct DirectCallInputs<'a> {
  pub(crate) arguments: &'a [Value],
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum DirectCallOutcome {
  Returned(Value),
  Fallback,
}

pub(crate) struct BaselineInvocation<'a> {
  pub(crate) vm: *mut crate::vm::state::Vm,
  pub(crate) start_pc: VmPc,
  pub(crate) registers: &'a mut [Value],
  pub(crate) arguments: &'a [Value],
  pub(crate) locals: &'a mut [crate::binding::BindingCell],
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum BaselineOutcome {
  Unavailable,
  Returned(Value),
  Interpret { resume_pc: VmPc },
  Invalidated { resume_pc: VmPc },
}

impl DirectCallTarget {
  pub(crate) fn leaf(&self) -> DirectLeaf {
    self.leaf
  }

  pub(crate) fn contract(&self) -> DirectCallContract {
    self.contract
  }

  pub(crate) fn call(
    &self,
    inputs: DirectCallInputs<'_>,
  ) -> Result<DirectCallOutcome, String> {
    let arguments = inputs.arguments;
    if matches!(self.leaf, DirectLeaf::NumericBinary(_)) {
      let compact_arguments = abi::NumericArguments::from_values(arguments);
      let mut frame = native::CompactDirectCallFrame {
        result_bits: 0,
        arguments: compact_arguments.as_ptr(),
        argument_count: compact_arguments.argument_count(),
      };
      let exit = unsafe { self.code.call_compact_direct_frame(&mut frame) };
      if native::decode_exit(exit)? != native::JitExit::Return {
        return Ok(DirectCallOutcome::Fallback);
      }
      return abi::decode_numeric_result(frame.result_bits)
        .map(DirectCallOutcome::Returned)
        .map_err(|_| "native numeric call returned a reserved value encoding".to_string());
    }
    let mut result = Value::Undefined;
    let mut frame = native::DirectCallFrame {
      result: &mut result,
      arguments: arguments.as_ptr(),
      argument_count: arguments.len(),
    };
    let exit = unsafe { self.code.call_direct_frame(&mut frame) };
    if native::decode_exit(exit)? != native::JitExit::Return {
      return Ok(DirectCallOutcome::Fallback);
    }
    Ok(DirectCallOutcome::Returned(result))
  }
}

#[derive(Default)]
pub(crate) struct JitRuntime {
  pub(crate) code: HashMap<FunctionId, Vec<JitCode>>,
  next_version: u64,
  numeric_regions: [NumericRegionCode; 4],
  dense_array_push_region: NumericRegionCode,
  dense_masked_read_region: NumericRegionCode,
  optimizing: OptimizingTier,
}

enum CompilationState {
  Pending {
    ticket: CompileTicket,
    stamp: SnapshotStamp,
  },
  Installed {
    version: CodeVersionId,
    stamp: SnapshotStamp,
  },
  Rejected {
    stamp: SnapshotStamp,
  },
}

struct OptimizingTier {
  worker: Option<CompilerWorker>,
  states: HashMap<CompilationKey, CompilationState>,
  next_ticket: u64,
  disabled: bool,
  compile_attempts: u64,
  compile_installs: u64,
  compile_rejections: u64,
  stale_results: u64,
  native_calls: u64,
  guard_failures: u64,
  worker_failures: u64,
}

impl Default for OptimizingTier {
  fn default() -> Self {
    Self {
      worker: None,
      states: HashMap::new(),
      next_ticket: 0,
      disabled: std::env::var_os("DENKO_DISABLE_OPT_JIT").is_some()
        || !cfg!(all(unix, target_arch = "x86_64")),
      compile_attempts: 0,
      compile_installs: 0,
      compile_rejections: 0,
      stale_results: 0,
      native_calls: 0,
      guard_failures: 0,
      worker_failures: 0,
    }
  }
}

#[derive(Default)]
enum NumericRegionCode {
  #[default]
  Uncompiled,
  Ready(Rc<NativeCode>),
  Unavailable,
}

pub(crate) const NUMERIC_ACTIVATION_LIMIT: u32 = 4_096;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum NumericRegionExit {
  Completed,
  ActivationLimit,
  Interrupted { observed_generation: u64 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct NumericRegionOutcome {
  pub(crate) exit: NumericRegionExit,
  pub(crate) iterations: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum NumericRegionExecution {
  Unavailable,
  Executed(NumericRegionOutcome),
}

impl JitRuntime {
  pub(crate) fn allocate_version(&mut self) -> CodeVersionId {
    self.next_version = self.next_version.saturating_add(1);
    CodeVersionId(self.next_version)
  }

  pub(crate) fn install_const_prefix(
    &mut self,
    function: FunctionId,
    chunk: &Chunk,
    parameter_count: usize,
  ) -> Result<CodeVersionId, String> {
    let blocks = discover_blocks(chunk)?;
    let compiler = BaselineCompiler { chunk };
    let direct_call = compiler
      .compile_direct_leaf(parameter_count)
      .ok()
      .map(|(code, leaf)| DirectCallTarget {
        code: Rc::new(code),
        contract: DirectCallContract {
          arguments: match leaf {
            DirectLeaf::Number(_) => DirectArgumentUse::None,
            DirectLeaf::Parameter(slot) => DirectArgumentUse::Parameter(slot),
            DirectLeaf::NumericBinary(_) => DirectArgumentUse::NumericPair,
          },
        },
        leaf,
      });
    let mut native_blocks = HashMap::new();
    for block in &blocks {
      if let Ok(code) = compiler.compile_block(block) {
        native_blocks.insert(
          block.start_pc,
          NativeBlock {
            code,
            #[cfg(test)]
            direct_exit: None,
          },
        );
      }
    }
    if let std::collections::hash_map::Entry::Vacant(entry) = native_blocks.entry(0) {
      let code = compiler.compile_const_prefix()?;
      entry.insert(NativeBlock {
        code,
        #[cfg(test)]
        direct_exit: None,
      });
    }
    let version = self.allocate_version();
    self
      .code
      .entry(function)
      .or_default()
      .push(JitCode {
        version,
        state: JitCodeState::Valid,
        preferred_entry: 0,
        native_blocks,
        direct_call,
      });
    Ok(version)
  }

  pub(crate) fn direct_call_target(
    &self,
    function: FunctionId,
  ) -> Option<DirectCallTarget> {
    self
      .code
      .get(&function)?
      .iter()
      .find(|code| code.state == JitCodeState::Valid)
      .and_then(|code| code.direct_call.clone())
  }

  pub(crate) fn optimizing_numeric_binary_target(
    &mut self,
    function: FunctionId,
    chunk: &Chunk,
    parameter_count: usize,
  ) -> Result<Option<DirectCallTarget>, String> {
    self.poll_optimizing_completions();
    if self.optimizing.disabled {
      return Ok(None);
    }
    let snapshot = ir::CompilerChunk::from_chunk(chunk, parameter_count)
      .map_err(|error| format!("optimizing snapshot rejected: {error:?}"))?;
    let Some(operation) = snapshot.numeric_binary_op() else {
      return Ok(None);
    };
    let key = CompilationKey {
      function,
      specialization: SpecializationKey::NumericBinary(operation),
    };
    let stamp = SnapshotStamp::for_chunk(&snapshot);
    match self.optimizing.states.get(&key) {
      Some(CompilationState::Installed {
        version,
        stamp: installed_stamp,
      }) if *installed_stamp == stamp => {
        return Ok(self.optimizing_target(function, *version));
      }
      Some(CompilationState::Pending {
        stamp: pending_stamp,
        ..
      }) if *pending_stamp == stamp => return Ok(None),
      Some(CompilationState::Rejected {
        stamp: rejected_stamp,
        ..
      }) if *rejected_stamp == stamp => return Ok(None),
      _ => {
        self.optimizing.states.remove(&key);
      }
    }
    if self.optimizing.worker.is_none() {
      match CompilerWorker::start() {
        Ok(worker) => self.optimizing.worker = Some(worker),
        Err(_) => {
          self.disable_optimizing_worker();
          return Ok(None);
        }
      }
    }
    self.optimizing.next_ticket = self.optimizing.next_ticket.saturating_add(1);
    let ticket = CompileTicket::new(self.optimizing.next_ticket);
    let request = CompileRequest::numeric_binary(ticket, function, snapshot, operation);
    self
      .optimizing
      .states
      .insert(key, CompilationState::Pending { ticket, stamp });
    self.optimizing.compile_attempts = self.optimizing.compile_attempts.saturating_add(1);
    let submitted = self
      .optimizing
      .worker
      .as_ref()
      .is_some_and(|worker| worker.submit(request).is_ok());
    if !submitted {
      self
        .optimizing
        .states
        .insert(key, CompilationState::Rejected { stamp });
      self.disable_optimizing_worker();
    }
    Ok(None)
  }

  fn optimizing_target(
    &self,
    function: FunctionId,
    version: CodeVersionId,
  ) -> Option<DirectCallTarget> {
    self
      .code
      .get(&function)?
      .iter()
      .find(|code| code.version == version && code.state == JitCodeState::Valid)?
      .direct_call
      .clone()
  }

  fn poll_optimizing_completions(&mut self) {
    while let Some(worker) = self.optimizing.worker.as_ref() {
      let completion = match worker.try_recv() {
        Ok(Some(completion)) => completion,
        Ok(None) => break,
        Err(_) => {
          self.disable_optimizing_worker();
          break;
        }
      };
      self.accept_optimizing_completion(completion);
    }
  }

  fn accept_optimizing_completion(
    &mut self,
    completion: CompileCompletion,
  ) {
    let (ticket, key, stamp) = match &completion {
      CompileCompletion::Compiled {
        ticket, key, stamp, ..
      }
      | CompileCompletion::Rejected {
        ticket, key, stamp, ..
      }
      | CompileCompletion::Failed {
        ticket, key, stamp, ..
      } => (*ticket, *key, *stamp),
    };
    if !matches!(
      self.optimizing.states.get(&key),
      Some(CompilationState::Pending {
        ticket: pending_ticket,
        stamp: pending_stamp,
      }) if *pending_ticket == ticket && *pending_stamp == stamp
    ) {
      self.optimizing.stale_results = self.optimizing.stale_results.saturating_add(1);
      return;
    }
    match completion {
      CompileCompletion::Compiled { artifact, .. } => {
        let installed = artifact
          .validate()
          .map_err(|_| ())
          .and_then(|artifact| NativeCode::install_artifact(artifact).map_err(|_| ()));
        match installed {
          Ok(code) => {
            let version = self.allocate_version();
            let target = DirectCallTarget {
              code: Rc::new(code),
              leaf: match key.specialization {
                SpecializationKey::NumericBinary(operation) => DirectLeaf::NumericBinary(operation),
              },
              contract: DirectCallContract {
                arguments: DirectArgumentUse::NumericPair,
              },
            };
            self
              .code
              .entry(key.function)
              .or_default()
              .push(JitCode {
                version,
                state: JitCodeState::Valid,
                preferred_entry: 0,
                native_blocks: HashMap::new(),
                direct_call: Some(target),
              });
            self
              .optimizing
              .states
              .insert(key, CompilationState::Installed { version, stamp });
            self.optimizing.compile_installs = self.optimizing.compile_installs.saturating_add(1);
          }
          Err(()) => {
            self
              .optimizing
              .states
              .insert(key, CompilationState::Rejected { stamp });
            self.optimizing.compile_rejections = self
              .optimizing
              .compile_rejections
              .saturating_add(1);
          }
        }
      }
      CompileCompletion::Rejected { .. } => {
        self
          .optimizing
          .states
          .insert(key, CompilationState::Rejected { stamp });
        self.optimizing.compile_rejections = self
          .optimizing
          .compile_rejections
          .saturating_add(1);
      }
      CompileCompletion::Failed { .. } => self.disable_optimizing_worker(),
    }
  }

  fn disable_optimizing_worker(&mut self) {
    self.optimizing.disabled = true;
    self.optimizing.worker_failures = self.optimizing.worker_failures.saturating_add(1);
    self.optimizing.worker.take();
    for state in self.optimizing.states.values_mut() {
      if let CompilationState::Pending { stamp, .. } = *state {
        *state = CompilationState::Rejected { stamp };
      }
    }
  }

  pub(crate) fn record_optimizing_call(
    &mut self,
    returned: bool,
  ) {
    if returned {
      self.optimizing.native_calls = self.optimizing.native_calls.saturating_add(1);
    } else {
      self.optimizing.guard_failures = self.optimizing.guard_failures.saturating_add(1);
    }
  }

  #[cfg(test)]
  pub(crate) fn optimizing_stats(&self) -> (u64, u64, u64, u64) {
    (
      self.optimizing.compile_attempts,
      self.optimizing.compile_installs,
      self.optimizing.native_calls,
      self.optimizing.guard_failures,
    )
  }

  pub(crate) fn run_numeric_region(
    &mut self,
    prepared: &mut crate::vm::region::PreparedNumericRegion,
  ) -> Result<bool, String> {
    static NO_INTERRUPT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    self
      .execute_numeric_region(
        prepared,
        crate::vm::interrupt::NativeInterruptPoll {
          requested_generation: &NO_INTERRUPT,
          acknowledged_generation: 0,
        },
        u32::from(64_u8),
      )
      .map(|execution| matches!(execution, NumericRegionExecution::Executed(_)))
  }

  pub(crate) fn execute_numeric_region(
    &mut self,
    prepared: &mut crate::vm::region::PreparedNumericRegion,
    poll: crate::vm::interrupt::NativeInterruptPoll,
    iteration_budget: u32,
  ) -> Result<NumericRegionExecution, String> {
    if iteration_budget == 0 || !iteration_budget.is_multiple_of(64) {
      return Err("native numeric loop budget must be a nonzero multiple of 64".to_string());
    }
    let addend = match prepared.shape {
      crate::vm::region::NumericRegionShape::AddIndex => native::NumericLoopAddend::Index,
      crate::vm::region::NumericRegionShape::AddPrepared => native::NumericLoopAddend::Constant,
      crate::vm::region::NumericRegionShape::SubtractPrepared => {
        native::NumericLoopAddend::SubtractConstant
      }
      crate::vm::region::NumericRegionShape::MultiplyPrepared => {
        native::NumericLoopAddend::MultiplyConstant
      }
    };
    let slot = &mut self.numeric_regions[prepared.shape.index()];
    if matches!(slot, NumericRegionCode::Uncompiled) {
      *slot = match NativeCode::numeric_loop(addend) {
        Ok(code) => NumericRegionCode::Ready(Rc::new(code)),
        Err(_) => NumericRegionCode::Unavailable,
      };
    }
    let NumericRegionCode::Ready(code) = slot else {
      return Ok(NumericRegionExecution::Unavailable);
    };
    let mut frame = native::NativeNumericLoopFrame {
      index: prepared.index,
      limit: prepared.limit,
      accumulator: prepared.accumulator,
      step: prepared.step,
      addend: prepared.addend,
      iteration_budget,
      iterations: 0,
      exit_code: u32::MAX,
      requested_generation: poll.requested_generation,
      acknowledged_generation: poll.acknowledged_generation,
      observed_generation: poll.acknowledged_generation,
    };
    unsafe { code.call_numeric_loop_frame(&mut frame) };
    if frame.iterations > iteration_budget {
      return Err("native numeric loop exceeded its activation bound".to_string());
    }
    prepared.index = frame.index;
    prepared.accumulator = frame.accumulator;
    prepared.iterations = frame.iterations;
    let exit = match frame.exit_code {
      native::NUMERIC_EXIT_COMPLETED => NumericRegionExit::Completed,
      native::NUMERIC_EXIT_ACTIVATION_LIMIT => NumericRegionExit::ActivationLimit,
      native::NUMERIC_EXIT_INTERRUPTED => NumericRegionExit::Interrupted {
        observed_generation: frame.observed_generation,
      },
      code => {
        return Err(format!(
          "native numeric loop returned invalid exit code {code}"
        ));
      }
    };
    Ok(NumericRegionExecution::Executed(NumericRegionOutcome {
      exit,
      iterations: frame.iterations,
    }))
  }

  pub(crate) fn run_dense_array_push_region(
    &mut self,
    prepared: &mut crate::vm::region::PreparedArrayPushRegion,
  ) -> bool {
    if matches!(self.dense_array_push_region, NumericRegionCode::Uncompiled) {
      self.dense_array_push_region = match NativeCode::dense_array_push_loop() {
        Ok(code) => NumericRegionCode::Ready(Rc::new(code)),
        Err(_) => NumericRegionCode::Unavailable,
      };
    }
    let NumericRegionCode::Ready(code) = &self.dense_array_push_region else {
      return false;
    };
    unsafe { code.call_dense_array_push_frame(prepared) };
    true
  }

  pub(crate) fn run_dense_masked_read_region(
    &mut self,
    prepared: &mut crate::vm::region::PreparedDenseMaskedReadRegion,
  ) -> bool {
    if matches!(self.dense_masked_read_region, NumericRegionCode::Uncompiled) {
      self.dense_masked_read_region = match NativeCode::dense_masked_read_loop() {
        Ok(code) => NumericRegionCode::Ready(Rc::new(code)),
        Err(_) => NumericRegionCode::Unavailable,
      };
    }
    let NumericRegionCode::Ready(code) = &self.dense_masked_read_region else {
      return false;
    };
    unsafe { code.call_dense_masked_read_frame(prepared) };
    true
  }

  pub(crate) fn invoke_baseline(
    &self,
    function: FunctionId,
    invocation: BaselineInvocation<'_>,
  ) -> Result<BaselineOutcome, String> {
    let BaselineInvocation {
      vm,
      start_pc,
      registers,
      arguments,
      locals,
    } = invocation;
    let mut result = Value::Undefined;
    let mut frame = native::JitFrame {
      vm,
      function_id: function.get(),
      vm_pc: start_pc,
      registers: registers.as_mut_ptr(),
      register_count: registers.len(),
      result: &mut result,
      arguments: arguments.as_ptr(),
      argument_count: arguments.len(),
      locals: locals.as_mut_ptr(),
      local_count: locals.len(),
    };
    let code = self.code.get(&function).and_then(|versions| {
      versions
        .iter()
        .find(|code| code.state == JitCodeState::Valid && !code.native_blocks.is_empty())
    });
    let Some(code) = code else {
      return Ok(BaselineOutcome::Unavailable);
    };
    let mut next_pc = frame.vm_pc;
    if !code.native_blocks.contains_key(&next_pc) {
      next_pc = code.preferred_entry;
    }
    let exit = loop {
      let Some(block) = code.native_blocks.get(&next_pc) else {
        frame.vm_pc = next_pc;
        break native::JitExit::ResumeInterpreter;
      };
      let exit = unsafe { block.code.call_frame(&mut frame) };
      if exit != native::JIT_EXIT_NEXT_BLOCK {
        break native::decode_exit(exit)?;
      }
      next_pc = frame.vm_pc;
    };
    match exit {
      native::JitExit::Return => Ok(BaselineOutcome::Returned(result)),
      native::JitExit::ResumeInterpreter => Ok(BaselineOutcome::Interpret {
        resume_pc: frame.vm_pc,
      }),
      native::JitExit::Invalidated => Ok(BaselineOutcome::Invalidated {
        resume_pc: frame.vm_pc,
      }),
      native::JitExit::NextBlock => Err("unresolved baseline block exit".to_string()),
      native::JitExit::Exception => Err("baseline JIT reported an exception".to_string()),
    }
  }

  #[cfg(all(unix, target_arch = "x86_64"))]
  #[cfg(test)]
  pub(crate) fn link_direct(
    &mut self,
    function: FunctionId,
    version: CodeVersionId,
    source_pc: VmPc,
    target_pc: VmPc,
  ) -> Result<(), String> {
    let code = self
      .code
      .get_mut(&function)
      .and_then(|versions| {
        versions
          .iter_mut()
          .find(|code| code.version == version)
      })
      .ok_or_else(|| "JIT code version is unavailable".to_string())?;
    let target = code
      .native_blocks
      .get(&target_pc)
      .ok_or_else(|| "direct-link target is unavailable".to_string())?
      .code
      .entry_address();
    let source = code
      .native_blocks
      .get_mut(&source_pc)
      .ok_or_else(|| "direct-link source is unavailable".to_string())?;
    let site = source
      .direct_exit
      .ok_or_else(|| "native block has no patchable exit".to_string())?;
    source.code.patch_jump(site, target)
  }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum BaselineState {
  #[default]
  Cold,
  Requested,
  Compiling,
  Ready(CodeVersionId),
  Disabled,
}

#[cfg(test)]
mod tests {
  use super::native::JitFrame;
  use super::*;
  use crate::bytecode::{Chunk, Constant, Op, encode_risc_cond_jump};
  use crate::value::Value;
  use std::sync::Arc;

  fn invoke(
    runtime: &JitRuntime,
    function: FunctionId,
    registers: &mut [Value],
  ) -> BaselineOutcome {
    let mut locals = [];
    runtime
      .invoke_baseline(
        function,
        BaselineInvocation {
          vm: std::ptr::null_mut(),
          start_pc: 0,
          registers,
          arguments: &[],
          locals: &mut locals,
        },
      )
      .expect("baseline invocation")
  }

  #[cfg(all(unix, target_arch = "x86_64"))]
  #[test]
  fn dispatches_unknown_conditional_to_compiled_successors() {
    let mut chunk = Chunk::new(Arc::new(String::new()));
    chunk.constants.push(Constant::Number(7.0));
    chunk.constants.push(Constant::Number(42.0));
    chunk.write_instr(
      encode_risc_cond_jump(Op::JumpIfTrue, 0, 2).expect("encoding"),
      0,
    );
    chunk.write_instr(Op::Const as u32 | (1 << 8), 0);
    chunk.write_instr(Op::Return as u32 | (1 << 8), 0);
    chunk.write_instr(Op::Const as u32 | (1 << 8) | (1 << 16), 0);
    chunk.write_instr(Op::Return as u32 | (1 << 8), 0);
    let function = FunctionId::new(1);
    let mut runtime = JitRuntime::default();
    runtime
      .install_const_prefix(function, &chunk, 0)
      .expect("install blocks");
    for (condition, expected) in [(Value::Bool(false), 7.0), (Value::Bool(true), 42.0)] {
      let mut registers: [Value; 16] = std::array::from_fn(|_| Value::Undefined);
      registers[0] = condition;
      assert!(matches!(
        invoke(&runtime, function, &mut registers),
        BaselineOutcome::Returned(Value::Number(value)) if value == expected
      ));
    }
  }

  #[cfg(all(unix, target_arch = "x86_64"))]
  #[test]
  fn compiles_and_dispatches_discovered_risc_blocks() {
    let mut chunk = Chunk::new(Arc::new(String::new()));
    chunk.constants.push(Constant::Number(42.0));
    chunk.constants.push(Constant::Number(7.0));
    chunk.write_instr(Op::Const as u32, 0);
    chunk.write_instr(Op::Jump as u32 | (1 << 8), 0);
    chunk.write_instr(Op::Const as u32 | (1 << 16), 0);
    chunk.write_instr(Op::Return as u32, 0);
    let function = FunctionId::new(1);
    let mut runtime = JitRuntime::default();
    runtime
      .install_const_prefix(function, &chunk, 0)
      .expect("install blocks");
    assert!(runtime.code[&function][0].native_blocks.len() >= 2);
    let mut registers: [Value; 16] = std::array::from_fn(|_| Value::Undefined);
    assert_eq!(
      invoke(&runtime, function, &mut registers),
      BaselineOutcome::Returned(Value::Number(42.0))
    );
  }

  #[cfg(all(unix, target_arch = "x86_64"))]
  #[test]
  fn links_native_blocks_with_a_patchable_direct_jump() {
    let function = FunctionId::new(1);
    let version = CodeVersionId(1);
    let (source_code, site) = NativeCode::patchable_jump().expect("source block");
    let mut runtime = JitRuntime::default();
    runtime.code.insert(
      function,
      vec![JitCode {
        version,
        state: JitCodeState::Valid,
        preferred_entry: 0,
        native_blocks: HashMap::from([
          (
            0,
            NativeBlock {
              code: source_code,
              direct_exit: Some(site),
            },
          ),
          (
            1,
            NativeBlock {
              code: NativeCode::return_number(42.0).expect("target"),
              direct_exit: None,
            },
          ),
        ]),
        direct_call: None,
      }],
    );
    runtime
      .link_direct(function, version, 0, 1)
      .expect("link blocks");
    assert_eq!(
      invoke(&runtime, function, &mut []),
      BaselineOutcome::Returned(Value::Number(42.0))
    );
  }

  #[cfg(all(unix, target_arch = "x86_64"))]
  #[test]
  fn dispatches_between_independently_owned_native_blocks() {
    let function = FunctionId::new(1);
    let mut runtime = JitRuntime::default();
    runtime.code.insert(
      function,
      vec![JitCode {
        version: CodeVersionId(1),
        state: JitCodeState::Valid,
        preferred_entry: 0,
        native_blocks: HashMap::from([
          (
            0,
            NativeBlock {
              code: NativeCode::next_block(1).expect("block zero"),
              direct_exit: None,
            },
          ),
          (
            1,
            NativeBlock {
              code: NativeCode::return_number(42.0).expect("block one"),
              direct_exit: None,
            },
          ),
        ]),
        direct_call: None,
      }],
    );
    assert_eq!(
      invoke(&runtime, function, &mut []),
      BaselineOutcome::Returned(Value::Number(42.0))
    );
  }

  #[test]
  fn discovers_basic_blocks_and_successors() {
    let mut chunk = Chunk::new(Arc::new(String::new()));
    chunk.write_instr(Op::Const as u32, 0);
    chunk.write_instr(Op::Jump as u32 | (1 << 8), 0);
    chunk.write_instr(Op::Const as u32, 0);
    chunk.write_instr(Op::Return as u32, 0);

    assert_eq!(
      super::discover_blocks(&chunk).expect("block discovery"),
      vec![
        super::CompiledBlock {
          start_pc: 0,
          end_pc: 2,
          successors: vec![3]
        },
        super::CompiledBlock {
          start_pc: 2,
          end_pc: 3,
          successors: vec![3]
        },
        super::CompiledBlock {
          start_pc: 3,
          end_pc: 4,
          successors: vec![]
        },
      ]
    );
  }

  #[test]
  fn follows_constant_control_flow_without_interpreter_exit() {
    let mut jump = Chunk::new(Arc::new(String::new()));
    jump.constants.push(Constant::Number(42.0));
    jump.constants.push(Constant::Number(7.0));
    jump.write_instr(Op::Const as u32, 0);
    jump.write_instr(Op::Jump as u32 | (1 << 8), 0);
    jump.write_instr(Op::Const as u32 | (1 << 16), 0);
    jump.write_instr(Op::Return as u32, 0);

    let mut conditional = Chunk::new(Arc::new(String::new()));
    conditional.constants.push(Constant::Number(1.0));
    conditional.constants.push(Constant::Number(9.0));
    conditional.write_instr(Op::Const as u32, 0);
    conditional.write_instr(
      encode_risc_cond_jump(Op::JumpIfTrue, 0, 1).expect("encoding"),
      0,
    );
    conditional.write_instr(Op::Const as u32 | (1 << 16), 0);
    conditional.write_instr(Op::Return as u32, 0);

    for chunk in [&jump, &conditional] {
      let code = BaselineCompiler { chunk }
        .compile_const_prefix()
        .expect("baseline compile");
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
      assert_eq!(unsafe { code.call_frame(&mut frame) }, 0);
      assert!(
        matches!(result, Value::Number(value) if value == if std::ptr::eq(chunk, &jump) { 42.0 } else { 1.0 })
      );
    }
  }

  #[test]
  fn propagates_move_without_materializing_a_vm_register() {
    let mut chunk = Chunk::new(Arc::new(String::new()));
    chunk.constants.push(Constant::Number(-13.5));
    chunk.write_instr(Op::Const as u32 | (3 << 8), 0);
    chunk.write_instr(Op::Move as u32 | (7 << 8) | (3 << 12), 0);
    chunk.write_instr(Op::Return as u32 | (7 << 8), 0);
    let code = BaselineCompiler { chunk: &chunk }
      .compile_const_prefix()
      .expect("baseline compile");
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
    assert_eq!(unsafe { code.call_frame(&mut frame) }, 0);
    assert!(matches!(result, Value::Number(value) if value == -13.5));
  }

  #[test]
  fn exits_to_interpreter_at_next_risc_pc() {
    let mut chunk = Chunk::new(Arc::new(String::new()));
    chunk.constants.push(Constant::Number(42.0));
    chunk.write_instr(Op::Const as u32, 0);
    chunk.write_instr(Op::Neg as u32, 0);
    let code = BaselineCompiler { chunk: &chunk }
      .compile_const_prefix()
      .expect("baseline compile");
    let mut registers: [Value; 16] = std::array::from_fn(|_| Value::Undefined);
    let mut result = Value::Undefined;
    let mut frame = JitFrame {
      vm: std::ptr::null_mut(),
      function_id: 0,
      vm_pc: 0,
      registers: registers.as_mut_ptr(),
      register_count: registers.len(),
      result: &mut result,
      arguments: std::ptr::null(),
      argument_count: 0,
      locals: std::ptr::null_mut(),
      local_count: 0,
    };
    assert_eq!(unsafe { code.call_frame(&mut frame) }, 1);
    assert_eq!(frame.vm_pc, 1);
    assert!(matches!(registers[0], Value::Number(value) if value == 42.0));
  }

  #[cfg(all(unix, target_arch = "x86_64"))]
  #[test]
  fn unavailable_numeric_shape_falls_back_without_mutating_inputs() {
    let shape = crate::vm::region::NumericRegionShape::AddPrepared;
    let mut runtime = JitRuntime::default();
    runtime.numeric_regions[shape.index()] = NumericRegionCode::Unavailable;
    let mut prepared = crate::vm::region::PreparedNumericRegion {
      index: 2.0,
      limit: 10.0,
      accumulator: 7.0,
      step: 1.0,
      addend: 3.0,
      iterations: 0,
      shape,
    };
    let generation = std::sync::atomic::AtomicU64::new(0);
    let result = runtime
      .execute_numeric_region(
        &mut prepared,
        crate::vm::interrupt::NativeInterruptPoll {
          requested_generation: &generation,
          acknowledged_generation: 0,
        },
        NUMERIC_ACTIVATION_LIMIT,
      )
      .expect("valid unavailable result");
    assert_eq!(result, NumericRegionExecution::Unavailable);
    assert_eq!(prepared.index, 2.0);
    assert_eq!(prepared.accumulator, 7.0);
    assert_eq!(prepared.iterations, 0);
  }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct FunctionProfile {
  pub(crate) calls: u64,
  pub(crate) parameter_count: usize,
  pub(crate) loop_counts: HashMap<VmPc, u64>,
  pub(crate) baseline: BaselineState,
}

impl FunctionProfile {
  pub(crate) fn record_call(&mut self) {
    self.calls = self.calls.saturating_add(1);
    if self.baseline == BaselineState::Cold && self.calls >= BASELINE_CALL_THRESHOLD {
      self.baseline = BaselineState::Requested;
    }
  }

  pub(crate) fn record_backedge(
    &mut self,
    pc: VmPc,
  ) {
    let count = self.loop_counts.entry(pc).or_default();
    *count = count.saturating_add(1);
    if self.baseline == BaselineState::Cold && *count >= BASELINE_LOOP_THRESHOLD {
      self.baseline = BaselineState::Requested;
    }
  }
}
