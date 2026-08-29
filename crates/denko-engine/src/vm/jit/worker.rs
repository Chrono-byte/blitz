// SPDX-License-Identifier: MPL-2.0

//! Private optimizing-compiler worker.

use super::artifact::{ArtifactError, NativeArtifact, emit_numeric_binary};
use super::ir::{FunctionSnapshot, IrError, SemanticFunction, VerifiedTypedFunction};
use super::{FunctionId, NumericBinaryOp};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc::{Receiver, Sender, SyncSender, TryRecvError};
use std::thread::JoinHandle;

/// Bump this whenever `abi.rs` encoding, `ir::FunctionSnapshot` layout, or
/// `artifact::NATIVE_ARTIFACT_ABI` changes. It invalidates stale
/// `SnapshotStamp`s so the worker does not reuse an artifact built with an
/// incompatible ABI/hasher. Current value 2 reflects the ShortBranchCode
/// rel32 fix (previously rel8, limited to ±127).
const CAPABILITY_REVISION: CapabilityRevision = CapabilityRevision(2);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) enum SpecializationKey {
  NumericBinary(NumericBinaryOp),
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) struct CompilationKey {
  pub(super) function: FunctionId,
  pub(super) specialization: SpecializationKey,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct CapabilityRevision(u16);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) struct SnapshotStamp {
  bytecode_revision: u64,
  capability_revision: CapabilityRevision,
}

impl SnapshotStamp {
  pub(super) fn for_chunk(code: &FunctionSnapshot) -> Self {
    Self {
      bytecode_revision: code.revision(),
      capability_revision: CAPABILITY_REVISION,
    }
  }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) struct CompileTicket(u64);

impl CompileTicket {
  pub(super) const fn new(value: u64) -> Self {
    Self(value)
  }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct CompileRequest {
  pub(super) ticket: CompileTicket,
  pub(super) key: CompilationKey,
  pub(super) stamp: SnapshotStamp,
  pub(super) code: FunctionSnapshot,
}

impl CompileRequest {
  pub(super) fn numeric_binary(
    ticket: CompileTicket,
    function: FunctionId,
    code: FunctionSnapshot,
    operation: NumericBinaryOp,
  ) -> Self {
    let stamp = SnapshotStamp::for_chunk(&code);
    Self {
      ticket,
      key: CompilationKey {
        function,
        specialization: SpecializationKey::NumericBinary(operation),
      },
      stamp,
      code,
    }
  }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RejectionReason {
  UnsupportedBody,
  InvalidIr,
  UnsupportedTarget,
}

#[derive(Debug)]
pub(super) enum CompileCompletion {
  Compiled {
    ticket: CompileTicket,
    key: CompilationKey,
    stamp: SnapshotStamp,
    artifact: NativeArtifact,
  },
  Rejected {
    ticket: CompileTicket,
    key: CompilationKey,
    stamp: SnapshotStamp,
  },
  Failed {
    ticket: CompileTicket,
    key: CompilationKey,
    stamp: SnapshotStamp,
  },
}

enum WorkerCommand {
  Compile(CompileRequest),
  Shutdown,
}

pub(super) struct CompilerWorker {
  requests: SyncSender<WorkerCommand>,
  completions: Receiver<CompileCompletion>,
  join: Option<JoinHandle<()>>,
}

impl CompilerWorker {
  pub(super) fn start() -> Result<Self, String> {
    let (request_tx, request_rx) = std::sync::mpsc::sync_channel(64);
    let (completion_tx, completion_rx) = std::sync::mpsc::channel();
    let join = std::thread::Builder::new()
      .name("denko-jit".to_string())
      .spawn(move || worker_main(request_rx, completion_tx))
      .map_err(|error| error.to_string())?;
    Ok(Self {
      requests: request_tx,
      completions: completion_rx,
      join: Some(join),
    })
  }

  pub(super) fn submit(
    &self,
    request: CompileRequest,
  ) -> Result<(), String> {
    use std::sync::mpsc::TrySendError;
    match self
      .requests
      .try_send(WorkerCommand::Compile(request))
    {
      Ok(()) => Ok(()),
      Err(TrySendError::Full(_)) => Err("optimizing compiler queue full".to_string()),
      Err(TrySendError::Disconnected(_)) => {
        Err("optimizing compiler worker disconnected".to_string())
      }
    }
  }

  pub(super) fn try_recv(&self) -> Result<Option<CompileCompletion>, String> {
    match self.completions.try_recv() {
      Ok(completion) => Ok(Some(completion)),
      Err(TryRecvError::Empty) => Ok(None),
      Err(TryRecvError::Disconnected) => Err("optimizing compiler worker disconnected".to_string()),
    }
  }
}

impl Drop for CompilerWorker {
  fn drop(&mut self) {
    let _ = self.requests.send(WorkerCommand::Shutdown);
    if let Some(join) = self.join.take() {
      let _ = join.join();
    }
  }
}

fn worker_main(
  requests: Receiver<WorkerCommand>,
  completions: Sender<CompileCompletion>,
) {
  while let Ok(command) = requests.recv() {
    let WorkerCommand::Compile(request) = command else {
      break;
    };
    let ticket = request.ticket;
    let key = request.key;
    let stamp = request.stamp;
    let completion = catch_unwind(AssertUnwindSafe(|| compile(request)))
      .unwrap_or(CompileCompletion::Failed { ticket, key, stamp });
    if completions.send(completion).is_err() {
      break;
    }
  }
}

fn compile(request: CompileRequest) -> CompileCompletion {
  let SpecializationKey::NumericBinary(expected_operation) = request.key.specialization;
  let result = SemanticFunction::decode(&request.code)
    .and_then(|semantic| VerifiedTypedFunction::specialize_numeric_binary(&semantic))
    .map_err(map_ir_error)
    .and_then(|typed| {
      (typed.numeric_operation() == Some(expected_operation))
        .then_some(typed)
        .ok_or(RejectionReason::InvalidIr)
    })
    .and_then(|typed| emit_numeric_binary(&typed).map_err(map_artifact_error));
  match result {
    Ok(artifact) => CompileCompletion::Compiled {
      ticket: request.ticket,
      key: request.key,
      stamp: request.stamp,
      artifact,
    },
    Err(_) => CompileCompletion::Rejected {
      ticket: request.ticket,
      key: request.key,
      stamp: request.stamp,
    },
  }
}

fn map_ir_error(error: IrError) -> RejectionReason {
  match error {
    IrError::UnsupportedBody => RejectionReason::UnsupportedBody,
    IrError::TooManyParameters | IrError::InvalidSsa => RejectionReason::InvalidIr,
  }
}

fn map_artifact_error(error: ArtifactError) -> RejectionReason {
  match error {
    ArtifactError::InvalidContract if !cfg!(all(unix, target_arch = "x86_64")) => {
      RejectionReason::UnsupportedTarget
    }
    ArtifactError::UnsupportedTypedFunction
    | ArtifactError::TooLarge
    | ArtifactError::InvalidContract
    | ArtifactError::InvalidRelocation => RejectionReason::InvalidIr,
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::vm::jit::ir::FunctionSnapshot;
  use crate::{compiler, parser::Parser};

  fn snapshot(source: &str) -> FunctionSnapshot {
    let session = Parser::parse_source(source).expect("source parses");
    let code = compiler::compile_session(&session).expect("source compiles");
    let proto = &code.functions[0];
    FunctionSnapshot::from_code(&proto.chunk, proto.arity).expect("snapshot")
  }

  #[test]
  fn request_protocol_is_send_and_static() {
    fn assert_send_static<T: Send + 'static>() {}
    assert_send_static::<CompileRequest>();
    assert_send_static::<CompileCompletion>();
  }

  #[test]
  fn worker_compiles_supported_numeric_binary_bodies() {
    let worker = CompilerWorker::start().expect("worker starts");
    let add = CompileRequest::numeric_binary(
      CompileTicket::new(1),
      FunctionId::new(1),
      snapshot("function add(a, b) { return a + b; }"),
      NumericBinaryOp::Add,
    );
    worker.submit(add).expect("request accepted");
    let completion = worker.completions.recv().expect("completion");
    assert!(matches!(completion, CompileCompletion::Compiled { .. }));

    let multiply = CompileRequest::numeric_binary(
      CompileTicket::new(2),
      FunctionId::new(2),
      snapshot("function multiply(a, b) { return a * b; }"),
      NumericBinaryOp::Multiply,
    );
    worker.submit(multiply).expect("request accepted");
    let completion = worker.completions.recv().expect("completion");
    assert!(matches!(completion, CompileCompletion::Compiled { .. }));
  }
}
