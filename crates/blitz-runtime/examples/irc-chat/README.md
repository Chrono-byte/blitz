# Legacy Blitz IRC chat demo

This example uses the legacy `Blitz.*` compatibility globals. The default CLI
does not install them; use the current Effect-native `irc-chat-v2` example for
the supported runtime path, or provide explicit compatibility host options.

This demo runs one Blitz WebSocket server and two terminal clients. Incoming
messages are printed through Rustyline's external-printer path, so they appear
above the prompt without destroying text currently being edited.

From the repository root, use the platform launcher to build Blitz once and
launch all three processes. The launchers use a cached build directory and copy
the executable to a unique per-run temp directory, so an earlier chat process
cannot lock the next Cargo build.

On macOS, the shell launcher uses Ghostty 1.3.0 or newer to open a split layout:
the server on the left, with Alice and Bob stacked on the right. macOS may ask
for Automation permission the first time the script controls Ghostty.

```sh
./crates/blitz-runtime/examples/irc-chat/run-demo.sh
```

On Windows:

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
