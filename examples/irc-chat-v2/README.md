# Blitz IRC chat demo (v2)

Version two keeps the original terminal clients and WebSocket protocol while
adding nickname validation, message limits, basic rate limiting, and robust
connection cleanup. The server and both clients are written in TypeScript.

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
cargo run -p blitz-cli -- run crates/blitz-runtime/examples/irc-chat-v2/server.ts
cargo run -p blitz-cli -- run crates/blitz-runtime/examples/irc-chat-v2/alice.ts
cargo run -p blitz-cli -- run crates/blitz-runtime/examples/irc-chat-v2/bob.ts
```

Type a message and press Enter to broadcast it. Type `/quit`, press Ctrl-D, or
close the server to disconnect. Set `BLITZ_CHAT_URL` before starting a client
to use another `ws://` endpoint.

## Effect design

The server and clients use only Effect-native public modules. The dormant
`Blitz.*` compatibility namespace is not installed.

- `blitz:runtime` starts one root Effect and supplies the live Layers.
- `blitz:http` owns the listener in Scope.
- `blitz:websocket` exposes scoped connections, Effectful sends/closes, and a
  bounded message Stream.
- `blitz:terminal` exposes Effectful output and a scoped line Stream.
- Server client state lives in `Ref<Map<Connection, Client>>`; updates replace
  the Map rather than mutating module-global state.
- HTTP request contexts fork connection processors into the listener Scope, so
  accepted sockets remain supervised and are interrupted during shutdown.
- Expected transport failures stay in typed Effect error channels.

The integration test runs the complete join and message protocol with
compatibility globals disabled.
