# Blitz Effect IRC chat demo (v2)

Version two keeps the original terminal clients and WebSocket protocol, while
the server is an ES module built around `effect/Effect`. Server startup, HTTP
routing, connection changes, and message broadcasts cross explicit synchronous
Effect boundaries.

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

This example intentionally uses the existing `Blitz.serve` and WebSocket host
APIs at the transport boundary. Effect owns the application actions; a future
version can move transport ownership to `App.serve` once its public HTTP and
WebSocket routing surface is complete.
