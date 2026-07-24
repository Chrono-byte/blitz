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

## Effect boundary

This example intentionally stays synchronous because `Blitz.serve`, WebSocket
callbacks, and terminal callbacks are synchronous compatibility APIs. The
currently published Effect facade exposes `Ref`, synchronous typed failure
boundaries, and scoped `blitz:app` listener acquisition. It does not yet expose
the generic scoped WebSocket-handler and terminal APIs needed to model this
interactive chat faithfully.

Following the Effect guidelines, this example therefore:

- keeps nickname and message transformations pure;
- uses ordinary `try`/`catch` only for direct synchronous WebSocket host calls,
  not for Effect failures;
- does not wrap `Map` operations or pure transformations in `Effect.sync`;
- does not start detached fibers from synchronous callbacks; and
- leaves listener and socket ownership with the compatibility runtime.

Adding `Effect` wrappers here would hide the lifecycle boundary without adding
typed failures, testable services, or scoped cleanup. Migrate this example when
the Effect-native WebSocket-handler and terminal APIs are public; listener
ownership should use the scoped server boundary, chat state can use `Ref`
inside an explicit service, and the executable should have one supervised root
Effect.
