# Blitz IRC chat demo (v2)

Version two keeps the original terminal clients and WebSocket protocol while
adding nickname validation, message limits, basic rate limiting, and robust
connection cleanup.

The server exposes:

- `GET /` — status and connection instructions
- `GET /health` — a minimal health response
- `GET /chat` — the WebSocket chat endpoint

From the repository root, launch the server, Alice, and Bob:

```sh
./crates/blitz-runtime/examples/irc-chat-v2/run-demo.sh
```

On Windows:

```powershell
.\crates\blitz-runtime\examples\irc-chat-v2\run-demo.ps1
```

Or run each process in a separate terminal:

```sh
cargo run -p blitz-cli -- run --module crates/blitz-runtime/examples/irc-chat-v2/server.mjs
cargo run -p blitz-cli -- crates/blitz-runtime/examples/irc-chat-v2/alice.js
cargo run -p blitz-cli -- crates/blitz-runtime/examples/irc-chat-v2/bob.js
```

Type a message and press Enter to broadcast it. Type `/quit`, press Ctrl-D, or
close the server to disconnect. Set `BLITZ_CHAT_URL` before starting a client
to use another `ws://` endpoint.

This example intentionally stays synchronous because `Blitz.serve` and its
WebSocket callbacks are synchronous host APIs. It does not wrap mutable
JavaScript operations in `Effect.sync`; a larger application should introduce
Effect at boundaries where typed errors, services, scopes, or asynchronous
lifecycle management provide a concrete benefit.
