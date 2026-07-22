# Networking integration plan

## Goal

Add production-grade networking without moving host policy into
`blitz-engine` or allowing I/O tasks to mutate VM state. The first implemented
slice includes runtime-owned WebSocket client/server and HTTP server transports
plus a VM-thread `Blitz.serve` route API. Outbound `fetch`, TLS, and Node
compatibility build on the same runtime event boundary.

## Decisions

1. `blitz-engine` remains host-neutral. Networking belongs to
   `blitz-runtime`; `blitz-cli` only constructs and drives the runtime.
2. Tokio is the runtime I/O executor for sockets, DNS, timers, signals,
   filesystem work, and outbound HTTP.
3. FastWebSockets is the WebSocket framing implementation. Hyper performs
   HTTP/1.1 upgrade handling.
4. Socket tasks exchange bounded commands and owned events with the runtime.
   JavaScript `Value`, `Rc`, and `RefCell` instances never cross into Tokio
   tasks.
5. The public JavaScript API remains backend-independent. FastWebSockets types
   stop at `crates/blitz-runtime/src/net`.
6. Limits and shutdown are runtime policy, not dependency defaults.

## Current implementation

`crates/blitz-runtime/src/net/websocket.rs` now provides:

- loopback or externally bound HTTP/1.1 WebSocket upgrades;
- numeric connection IDs and owned text/binary/close/error events;
- a bounded command channel per connection;
- configurable complete-message and command-queue limits;
- fragmentation collection with an aggregate message-size check;
- server-initiated close commands;
- graceful server shutdown using close code 1001; and
- accounting that waits for active upgraded connections to finish.

`crates/blitz-runtime/src/net/http.rs` and the host adapter now provide:

- bounded HTTP request handoff to the VM thread;
- Deno-style `Blitz.serve(handler)` and `Blitz.serve(options, handler)` calls;
- handlers receiving `(request, info)` with remote TCP address metadata;
- request objects with `url`, `method`, `path`, `query`, UTF-8 `body`, `text()`,
  `json()`, and mutable `Headers`;
- returned `Response` objects, including synchronously settling async handlers,
  with body, status, status text, headers, `ok`, `text()`, and `json()`;
- Deno-style `Blitz.upgradeWebSocket(request)` on the HTTP listener;
- VM-thread WebSocket `open`, `message`, `close`, and `error` dispatch through
  both `on*` properties and `addEventListener`;
- bounded `send()`, validated `close()`, binary byte arrays, lifecycle state,
  and buffered-byte accounting;
- outbound `new WebSocket("ws://...")` clients over the same event path;
- prompt-safe buffered terminal input/output through `Blitz.stdin` and
  `Blitz.stdout`;
- a CLI runtime driver that keeps scripts with an HTTP server alive; and
- `BLITZ_HOST`/`BLITZ_PORT` configuration, defaulting to `127.0.0.1:8000`.

Pass the handler function directly:

```js
function callbackFunc(request, info) {
  return new Response(
    `Hello from ${request.url} via ${info.remoteAddr.transport}`,
  );
}

Blitz.serve({ hostname: "127.0.0.1", port: 8000 }, callbackFunc);
```

```js
Blitz.serve((request) => {
  const { socket, response } = Blitz.upgradeWebSocket(request);
  socket.onmessage = (event) => socket.send(event.data);
  return response;
});
```

The engine supplies the host-neutral WebSocket ready-state and buffered-byte
state machine. Concrete sockets and all I/O remain in `blitz-runtime`.

## Target architecture

```text
JavaScript callbacks and promises
              |
              v
single VM thread and runtime resource registry
              |
              +---- bounded owned events <---- Tokio socket tasks
              |
              +---- bounded commands --------> FastWebSockets connections
                                                |
                                                +-- Hyper HTTP upgrade
                                                +-- TCP listener
```

The runtime event queue is the only route back into JavaScript. Each event
contains owned `Send` data and a generational or numeric resource ID. The VM
thread resolves that ID, invokes callbacks, and drains microtasks before the
next macrotask.

## Runtime contracts

### Event-loop ordering

For each turn:

1. dequeue one runtime event;
2. resolve its resource ID;
3. invoke the corresponding JavaScript callback or settle a promise;
4. drain microtasks;
5. process deferred close/cancellation work; and
6. continue while referenced resources or queued jobs remain.

The CLI should drive this loop after entry evaluation. Test262-only execution
can retain its focused harness loop until the runtime scheduler replaces it.

### Resource ownership

- VM-visible state stays in the runtime registry on the VM thread.
- Tokio tasks own listeners, streams, protocol state, and byte buffers.
- Connection commands are bounded `mpsc` messages.
- Dropping a connection command sender ends that connection.
- Closing a server stops accepts, signals all upgraded connections, and waits
  for them to finish.
- Later public handles need `ref()`/`unref()` and idempotent `close()` behavior.

### Limits and backpressure

- Keep event queues, connection command queues, HTTP metadata, request bodies,
  WebSocket messages, and pending writes bounded.
- Apply the message limit to the aggregate fragmented message, not merely each
  frame.
- The current transport blocks reads when its bounded runtime event queue is
  full. The VM adapter should add high/low watermarks and explicit drain
  events for writes before the JavaScript API is stabilized.
- Use conservative defaults and reject invalid zero-capacity configurations at
  the future public configuration boundary.

## Component order

### 1. Runtime scheduler and resources

- Add the VM-thread macrotask queue and generational resource registry.
- Return a runtime handle from host installation.
- Drive the handle from script/module execution until no referenced resources
  remain.
- Prove callback/microtask ordering, cancellation races, and `ref`/`unref`.

### 2. WebSocket server adapter

- Wrap the implemented transport in the resource registry.
- Convert owned text to JavaScript strings and binary data to the runtime's
  eventual `ArrayBuffer`/typed-array representation.
- Add backend-independent open, message, error, drain, and close callbacks.
- Add listen permissions before non-loopback binding is public.

### 3. HTTP server

- Generalize the Hyper connection service to emit bounded request metadata and
  streaming body chunks.
- Keep upgrade decisions on the VM thread and execute accepted upgrades on the
  owning connection task.
- Add response deadlines, abort invalidation, streaming writes, and graceful
  shutdown.

### 4. Foundational Web APIs

- `URL` and `URLSearchParams`;
- `AbortController` and `AbortSignal`;
- `EventTarget` or a smaller compatible listener primitive;
- `ArrayBuffer`, typed arrays, and explicit byte-copy helpers; and
- readable/writable stream primitives with cancellation and backpressure.

### 5. Outbound networking

- Tokio DNS, TCP, UDP, and TLS resources;
- Hyper-based HTTP client and connection pooling for `fetch`;
- redirects, decompression, proxy, cookie, and certificate policy owned by the
  runtime; and
- a FastWebSockets client over the same Tokio transport.

### 6. Compatibility and hardening

- Build `node:net`, `node:tls`, `node:dgram`, `node:http`, and `node:https`
  adapters over the shared resources.
- Add TLS listeners and certificate reload behavior.
- Add permissions, stable error codes, metrics, tracing, and load baselines.
- Only consider a different specialized server backend if profiling shows a
  measured bottleneck that cannot be addressed in this Rust-native stack.

## Validation gates

The server transport must retain loopback tests for:

- successful upgrade and open event;
- text and binary messages in both directions;
- fragmented aggregate size limits;
- peer and server close codes/reasons;
- bounded queue behavior;
- invalid UTF-8 and malformed control frames;
- upgrade rejection;
- shutdown with active connections; and
- repeated bind/start/stop cycles.

Before calling the WebSocket layer production-ready, run the Autobahn server
suite and add fuzz targets for the runtime event decoder and fragmentation
state machine. Broader workspace tests remain the regression gate for engine
semantics.

## Known gaps after the first slice

- HTTP requests are currently buffered up to 1 MiB; streaming bodies and
  responses are not implemented yet.
- WebSocket clients currently support unencrypted `ws://` URLs only.
- WebSocket subprotocol negotiation, compression, and idle-timeout options are
  not exposed to JavaScript yet.
- No listen/connect permission model.
- FastWebSockets' per-frame guard and the runtime's aggregate fragmented-message
  guard are separate. Keep both layers covered as configuration evolves.
