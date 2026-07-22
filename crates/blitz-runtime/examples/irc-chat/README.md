# Blitz IRC chat demo

This demo runs one Blitz WebSocket server and two terminal clients. Incoming
messages are printed through Rustyline's external-printer path, so they appear
above the prompt without destroying text currently being edited.

From the repository root, either build Blitz once in the parent shell and
launch all three executables in separate Windows terminals. The launcher uses
a cached build directory and copies the executable to a unique per-run temp
directory, so an earlier chat process cannot lock the next Cargo build:

```powershell
.\crates\blitz-runtime\examples\irc-chat\run-demo.ps1
```

Or run each process in its own terminal:

```powershell
cargo run -p blitz-cli -- crates/blitz-runtime/examples/irc-chat/server.js
cargo run -p blitz-cli -- crates/blitz-runtime/examples/irc-chat/alice.js
cargo run -p blitz-cli -- crates/blitz-runtime/examples/irc-chat/bob.js
```

Type a message and press Enter to broadcast it. Type `/quit`, press Ctrl-D, or
close the server to disconnect. Set `BLITZ_CHAT_URL` before starting a client to
connect it to a different `ws://` endpoint.
