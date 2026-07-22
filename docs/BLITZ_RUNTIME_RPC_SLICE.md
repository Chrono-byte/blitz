# Blitz runtime RPC slice

`blitz-runtime` now exposes a Rust-side application/RPC description layer:

- `App` is an immutable router/layer description.
- `Rpc::procedure()` is a type-state builder that requires an explicit
  `Decoder` (use `Rpc::no_input()` for null input).
- `Router` supports validated dotted paths, nesting, middleware, and immutable
  copies.
- `DirectCaller` executes procedures in process without serializing values.
- `RpcRequest`/`RpcResponse` and `parse_requests` validate the JSON protocol,
  including tagged errors and the default 32-request batch limit.

This is deliberately a first vertical slice. It does not yet evaluate Effect
layers, invoke engine procedures, bind an `App::serve` transport, or implement
HTTP batching/WebSocket subscriptions. `App::serve` creates a lazy plan only;
the existing low-level `Blitz.serve` and WebSocket APIs remain the active
transport until adapters are wired to these types.
