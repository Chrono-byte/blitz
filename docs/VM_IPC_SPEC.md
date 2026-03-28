VM IPC & Syscall Specification
=================================

Purpose
-------
This document defines the kernel↔scheduler IPC and external NIF worker IPC formats and
behaviour. It includes recommended Rust serde types and the bincode wire format layout,
message framing rules, versioning, and sequence flows for common operations: spawn,
send/recv, async_io, and nif_call.

Design principles
-----------------
- Use compact, fast binary encoding (serde + bincode recommended) for data exchanged
  between scheduler threads, kernel tokio runtime, and NIF workers.
- Always prefix stream messages with a fixed-size length frame (u32, little-endian)
  to support framed reads over Unix domain sockets or pipes.
- Include a small header with `version` and `msg_type` for forward/backward compatibility.
- All payloads MUST be self-contained (no raw pointers or process-local addresses).
- Use `call_id` (u64) to correlate requests and responses. `call_id == 0` is reserved.

Transport
---------
- Scheduler ↔ Kernel: in-process channels are preferred (crossbeam/tokio channels). For
  structured messages between kernel tasks and worker-manager tasks use serde types.
- NIF Worker IPC: Unix domain sockets (SOCK_SEQPACKET or SOCK_STREAM) with length-prefix
  framing. Prefer SOCK_SEQPACKET when available for message boundaries.
- Length prefix: 4 bytes (u32 LE) representing the number of bytes that follow (the
  serialized bincode payload).

Framing
-------
- Each framed message on the wire: [u32 len][bincode(payload)]
- The payload encodes a top-level envelope type (see below). Always read `len` bytes
  after the prefix before deserializing.

Versioning
----------
- Top-level envelopes include `version: u16`. Start with `version = 1`.
- If new fields are added, prefer adding optional fields or new enum variants so older
  peers can ignore unknown data where possible. Never remove fields.

Bincode configuration
---------------------
- Use bincode with little-endian, fixed-int encoding. Example config in Rust:

```rust
let config = bincode::config::standard()
    .with_little_endian()
    .with_fixed_int_encoding();
```

Top-level envelope types (Rust sketches)
----------------------------------------
The wire payload always deserializes to one of these envelope enums.

```rust
use serde::{Serialize, Deserialize};

#[derive(Serialize, Deserialize)]
pub struct EnvelopeV1 {
    pub version: u16, // = 1
    pub body: EnvelopeBodyV1,
}

#[derive(Serialize, Deserialize)]
pub enum EnvelopeBodyV1 {
    NifRequest(NifRequestV1),
    NifResponse(NifResponseV1),
    KernelReq(KernelReqV1),
    KernelRes(KernelResV1),
}
```

NIF IPC types
-------------
Messages used between kernel and NIF worker processes.

```rust
#[derive(Serialize, Deserialize)]
pub struct NifRequestV1 {
    pub call_id: u64,
    pub nif_name: String,
    /// Binary-encoded arguments. Interpret inside worker according to nif_name ABI.
    pub payload: Vec<u8>,
    pub timeout_ms: Option<u32>,
}

#[derive(Serialize, Deserialize)]
pub struct NifResponseV1 {
    pub call_id: u64,
    pub result: Result<Vec<u8>, NifErrorV1>,
}

#[derive(Serialize, Deserialize)]
pub enum NifErrorV1 {
    Timeout,
    WorkerCrashed,
    BadArgs(String),
    Other(String),
}
```

Kernel ↔ Scheduler types
------------------------
These are the in-process messages used between scheduler threads and the tokio kernel
runtime. They can be sent over bounded channels; they reuse the same serialization types
for logging/metrics but live in-process.

```rust
#[derive(Serialize, Deserialize)]
pub enum KernelReqV1 {
    Spawn(SpawnMetaV1),
    Send(SendMetaV1),
    Recv(RecvMetaV1),
    AsyncIo(IoRequestV1),
    NifCall(NifCallMetaV1),
    ResolveTimeout { call_id: u64 },
}

#[derive(Serialize, Deserialize)]
pub enum KernelResV1 {
    SpawnResult { call_id: u64, pid: u64, result: Result<(), String> },
    SendResult { call_id: u64, result: Result<(), String> },
    RecvResult { call_id: u64, message: Option<Vec<u8>> },
    IoResult { call_id: u64, result: Result<Vec<u8>, String> },
    NifResult(NifResponseV1),
}

#[derive(Serialize, Deserialize)]
pub struct SpawnMetaV1 {
    pub call_id: u64,
    pub module: String,
    pub function: String,
    pub args: Vec<u8>, // serialized args
    pub options: SpawnOptionsV1,
}

#[derive(Serialize, Deserialize)]
pub struct SpawnOptionsV1 { pub exec_level_user: bool /* true = user-level */ }

#[derive(Serialize, Deserialize)]
pub struct SendMetaV1 { pub call_id: u64, pub to_pid: u64, pub message: Vec<u8> }

#[derive(Serialize, Deserialize)]
pub struct RecvMetaV1 { pub call_id: u64, pub timeout_ms: Option<u32> }

#[derive(Serialize, Deserialize)]
pub struct IoRequestV1 { pub call_id: u64, pub op: IoOpV1 }

#[derive(Serialize, Deserialize)]
pub enum IoOpV1 { TcpConnect { addr: String, port: u16 }, TcpRead { fd: u32, max_bytes: u32 }, TcpWrite { fd: u32, data: Vec<u8> }, Close { fd: u32 } }

#[derive(Serialize, Deserialize)]
pub struct NifCallMetaV1 { pub call_id: u64, pub nif_name: String, pub payload: Vec<u8>, pub timeout_ms: Option<u32> }
```

Mailbox message format
----------------------
- Messages placed into a process mailbox are opaque byte blobs on the wire: `Vec<u8>`.
- When a process calls `recv`, the VM deserializes the blob into the process heap using
  the interpreter's value format. The kernel must never pass raw pointers across processes.

Sequencing / Flow diagrams
--------------------------
Below are step-by-step sequences for common operations. `->` indicates direction.

1) spawn(module,function,args)
   1. Scheduler: generate call_id, build `KernelReq::Spawn(SpawnMetaV1{...})`, send to kernel.
   2. Kernel: validate args, allocate PID, create Process struct with mmap heap, enqueue to
      appropriate scheduler's deque, send `KernelRes::SpawnResult { call_id, pid, Ok(()) }`.
   3. Scheduler: receive SpawnResult, resume caller with pid or error.

2) send(pid, message)
   1. Scheduler: build `KernelReq::Send(SendMetaV1{...})`, send to kernel.
   2. Kernel: locate recipient mailbox; if mailbox has quota, append message (copy or
      off-heap blob); respond `KernelRes::SendResult`.
   3. Recipient scheduler: when recipient is scheduled, it will read mailbox entries and
      deserialize messages into recipient heap.

3) recv(timeout)
   1. Scheduler: build `KernelReq::Recv(RecvMetaV1{call_id,...})`, send to kernel and mark
      process Suspended.
   2. Kernel: if message ready, respond `KernelRes::RecvResult { message: Some(vec) }`.
      If no message, register continuation with optional timeout.
   3. On message arrival or timeout, kernel sends RecvResult; scheduler resumes process and
      the value is deserialized into the process heap.

4) async_io (e.g., TcpConnect / Read / Write)
   1. Scheduler: build `KernelReq::AsyncIo(IoRequestV1{call_id,...})`, send to kernel and
      suspend process.
   2. Kernel: spawn tokio task to perform IO using kernel buffers; on completion send
      `KernelRes::IoResult { call_id, result }` back.
   3. Scheduler: receive IoResult, resume process with result (or error).

5) nif_call_async(name,args)
   1. Scheduler: build `KernelReq::NifCall(NifCallMetaV1{...})`, send to kernel and suspend.
   2. Kernel: pick a worker process, send framed NifRequest (envelope) over Unix socket.
   3. Worker: deserialize request, execute native code, serialize NifResponse and send back.
   4. Kernel: receive NifResponse, wrap as `KernelRes::NifResult` and send to scheduler.
   5. Scheduler: resume caller with result or NifError.

Error handling
--------------
- All KernelRes variants use `Result<..., String>` to encode recoverable errors with
  human-readable messages. NIF errors use `NifErrorV1` enum for typed error handling.
- Worker crash: NIF manager detects worker exit and treats outstanding calls as
  `NifErrorV1::WorkerCrashed` for callers; manager restarts workers per policy.
- Version mismatch: if envelope `version` is unsupported, return an error and do not
  attempt deserialization.

Security & validation
---------------------
- Never interpret payloads as pointers. All payloads are binary blobs interpreted by the
  receiving side according to agreed ABIs.
- Validate sizes and quotas before allocating memory from a process heap. Reject requests
  with overly large payloads with descriptive errors.
- For NIF workers, run workers with least privileges and use process-level sandboxing if
  available (seccomp, namespaces) for extra isolation.

Backpressure & quotas
---------------------
- Kernel and scheduler must use bounded channels to avoid unbounded memory growth.
- When a scheduler submits a KernelReq and the kernel queue is full, the scheduler should
  either apply backpressure (wait briefly) or return an error to caller depending on
  policy. Avoid blocking scheduler threads indefinitely.

Observability
-------------
- Include `call_id`, timestamps, pid, and `nif_name` in logs for traceability.
- Emit metrics for request latencies, worker pool status, queue lengths, and error rates.

Example Rust usage
------------------

```rust
use serde::{Serialize, Deserialize};
use bincode;

let req = EnvelopeV1 { version: 1, body: EnvelopeBodyV1::NifRequest(NifRequestV1 { call_id: 42, nif_name: "crypto_hash".into(), payload: vec![1,2,3], timeout_ms: Some(5000) }) };
let bytes = bincode::encode_to_vec(&req, bincode::config::standard()).unwrap();
// send [u32 len][bytes]
```

Notes
-----
- This spec intentionally stays small and versioned. Add new Envelope versions when
  making incompatible wire changes.
- Keep ABI documentation for each NIF so workers know how to interpret `payload` blobs.

Integration with existing codebase
----------------------------------
This spec targets the current `blitz` repository layout. Key existing files to integrate
with are:

- `src/vm.rs` — the synchronous interpreter and `Vm::run` loop. The scheduler will call
  into `Vm` instances to execute user processes; add small hooks where the interpreter
  decrements reductions and yields (e.g. at opcode dispatch in the main loop).
- `src/main.rs` — bootstraps a single `Vm` and runs the soft prelude. Replace the single
  REPL VM with a kernel-managed prelude VM and a scheduler that can spawn multiple
  isolated `Process` instances backed by separate `Vm` state/heap regions.
- `src/lib.rs` — crate module exports; consider splitting into workspace crates (`vm-core`,
  `vm-kernel`, `vm-alloc`, `vm-ipc`) as the design evolves. For an incremental approach,
  start by adding `vm-kernel` and `vm-ipc` modules inside the existing crate.

Minimal integration steps (incremental)
1. Add a `Process` struct that wraps a `Vm` instance plus process metadata. Place near
   `src/vm.rs` or in a new `src/process.rs`.
2. Modify `Vm::run` to support a `run_step(&mut self, budget: &mut Reductions) -> RunResult`
   variant that executes up to `N` reductions and returns control. This makes it easy to
   integrate with a pinned scheduler without large refactors.
3. Introduce a scheduler module (`src/scheduler.rs`) that owns pinned worker threads and
   a work-stealing deque. Each scheduler pulls `Process` instances and calls
   `process.vm.run_step(&mut proc.reductions_left)`.
4. Add kernel channels and a small async runtime dependency (tokio) behind a new
   `vm-kernel` module for async IO and NIF management. Keep the initial change isolated —
   schedulers talk to kernel via bounded crossbeam channels.
5. Implement mailbox handling as a per-Process bounded VecDeque in Rust first, then
   optimize to lock-free or off-heap blobs as needed.

Files to consider adding
- `src/process.rs` — Process struct and per-process allocator hooks
- `src/scheduler.rs` — pinned scheduler threads, work-stealing integration
- `src/kernel.rs` — tokio runtime bootstrap, IO/NIF manager, recovery thread
- `vm-ipc` (module/file) — serde types and framed socket helpers; or keep `docs/VM_IPC_SPEC.md`
  as the single source of truth until an on-disk crate is required.

Testing notes specific to this repo
- Add unit tests that exercise `Vm::run_step` semantics by calling it repeatedly to
  simulate scheduler slices; this avoids introducing scheduler threads for initial tests.
- Integrate fault-injection tests by adding small test helpers in `tests/` that deliberately
  cause runtime asserts or out-of-bounds behaviour inside user code and confirming the
  process-exit semantics once signal handling is implemented.

File: docs/VM_IPC_SPEC.md
