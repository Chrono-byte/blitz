// SPDX-License-Identifier: MPL-2.0

//! Private optimizing-compiler worker.

use super::FunctionId;
use super::artifact::{ArtifactError, NativeArtifact, emit_numeric_add};
use super::ir::{CompilerChunk, IrError, SemanticFunction, VerifiedTypedFunction};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::thread::JoinHandle;

const CAPABILITY_REVISION: CapabilityRevision = CapabilityRevision(1);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) enum SpecializationKey {
  NumericBinaryAdd,
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
  pub(super) fn for_chunk(chunk: &CompilerChunk) -> Self {
    Self {
      bytecode_revision: chunk.revision(),
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
  pub(super) chunk: CompilerChunk,
}

impl CompileRequest {
  pub(super) fn numeric_add(
    ticket: CompileTicket,
    function: FunctionId,
    chunk: CompilerChunk,
  ) -> Self {
    let stamp = SnapshotStamp::for_chunk(&chunk);
    Self {
      ticket,
      key: CompilationKey {
        function,
        specialization: SpecializationKey::NumericBinaryAdd,
      },
      stamp,
      chunk,
    }
  }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RejectionReason {
  UnsupportedBody,
  InvalidIr,
  UnsupportedTarget,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CompilerFailure {
  Panicked,
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
    reason: RejectionReason,
  },
  Failed {
    ticket: CompileTicket,
    key: CompilationKey,
    stamp: SnapshotStamp,
    error: CompilerFailure,
  },
}

impl CompileCompletion {
  pub(super) fn ticket(&self) -> CompileTicket {
    match self {
      Self::Compiled { ticket, .. }
      | Self::Rejected { ticket, .. }
      | Self::Failed { ticket, .. } => *ticket,
    }
  }
}

enum WorkerCommand {
  Compile(CompileRequest),
  Shutdown,
}

pub(super) struct CompilerWorker {
  requests: Sender<WorkerCommand>,
  completions: Receiver<CompileCompletion>,
  join: Option<JoinHandle<()>>,
}

impl CompilerWorker {
  pub(super) fn start() -> Result<Self, String> {
    let (request_tx, request_rx) = std::sync::mpsc::channel();
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
    self
      .requests
      .send(WorkerCommand::Compile(request))
      .map_err(|_| "optimizing compiler worker disconnected".to_string())
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
    let completion =
      catch_unwind(AssertUnwindSafe(|| compile(request))).unwrap_or(CompileCompletion::Failed {
        ticket,
        key,
        stamp,
        error: CompilerFailure::Panicked,
      });
    if completions.send(completion).is_err() {
      break;
    }
  }
}

fn compile(request: CompileRequest) -> CompileCompletion {
  let result = SemanticFunction::decode(&request.chunk)
    .and_then(|semantic| VerifiedTypedFunction::specialize_numeric_add(&semantic))
    .map_err(map_ir_error)
    .and_then(|typed| emit_numeric_add(&typed).map_err(map_artifact_error));
  match result {
    Ok(artifact) => CompileCompletion::Compiled {
      ticket: request.ticket,
      key: request.key,
      stamp: request.stamp,
      artifact,
    },
    Err(reason) => CompileCompletion::Rejected {
      ticket: request.ticket,
      key: request.key,
      stamp: request.stamp,
      reason,
    },
  }
}

fn map_ir_error(error: IrError) -> RejectionReason {
  match error {
    IrError::UnsupportedBody => RejectionReason::UnsupportedBody,
    IrError::TooManyParameters | IrError::TooManyValues | IrError::InvalidSsa => {
      RejectionReason::InvalidIr
    }
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
  use crate::vm::jit::ir::CompilerChunk;
  use crate::{compiler, parser::Parser};

  fn snapshot(source: &str) -> CompilerChunk {
    let session = Parser::parse_source(source).expect("source parses");
    let chunk = compiler::compile_session(&session).expect("source compiles");
    let proto = &chunk.functions[0];
    CompilerChunk::from_chunk(&proto.chunk, proto.arity).expect("snapshot")
  }

  #[test]
  fn request_protocol_is_send_and_static() {
    fn assert_send_static<T: Send + 'static>() {}
    assert_send_static::<CompileRequest>();
    assert_send_static::<CompileCompletion>();
  }

  #[test]
  fn worker_compiles_numeric_add_and_rejects_other_bodies() {
    let worker = CompilerWorker::start().expect("worker starts");
    let add = CompileRequest::numeric_add(
      CompileTicket::new(1),
      FunctionId::new(1),
      snapshot("function add(a, b) { return a + b; }"),
    );
    worker.submit(add).expect("request accepted");
    let completion = worker.completions.recv().expect("completion");
    assert!(matches!(completion, CompileCompletion::Compiled { .. }));

    let multiply = CompileRequest::numeric_add(
      CompileTicket::new(2),
      FunctionId::new(2),
      snapshot("function multiply(a, b) { return a * b; }"),
    );
    worker.submit(multiply).expect("request accepted");
    let completion = worker.completions.recv().expect("completion");
    assert!(matches!(
      completion,
      CompileCompletion::Rejected {
        reason: RejectionReason::UnsupportedBody,
        ..
      }
    ));
  }
}
