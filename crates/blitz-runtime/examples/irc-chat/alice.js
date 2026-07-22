const nickname = "Alice";
const url = process.env.BLITZ_CHAT_URL || "ws://127.0.0.1:8000/chat";
const socket = new WebSocket(url);
globalThis.chatSocket = socket;

Blitz.stdin.setPrompt(`[${nickname}] > `);
Blitz.stdout.writeLine(`Connecting to ${url}...`);

socket.onopen = () => {
  socket.send(`JOIN ${nickname}`);
};

socket.onmessage = (event) => {
  Blitz.stdout.writeLine(String(event.data));
};

socket.onerror = (event) => {
  Blitz.stdout.writeLine(`Connection error: ${event.message}`);
};

socket.onclose = (event) => {
  Blitz.stdout.writeLine(`Disconnected (${event.code} ${event.reason})`);
  Blitz.stdin.close();
};

function handleInput(line) {
  const message = line;
  const activeSocket = globalThis.chatSocket;
  if (message === "/quit") {
    if (activeSocket.readyState === activeSocket.OPEN) {
      activeSocket.send("QUIT");
    }
    return;
  }
  if (message.length === 0) {
    return;
  }
  if (activeSocket.readyState !== activeSocket.OPEN) {
    Blitz.stdout.writeLine("Still connecting; try again in a moment.");
    return;
  }
  activeSocket.send(`MSG ${message}`);
}

function handleInputClose() {
  const activeSocket = globalThis.chatSocket;
  if (activeSocket.readyState === activeSocket.OPEN) {
    activeSocket.close(1000, "stdin closed");
  }
}

Blitz.stdin.onLine(handleInput);
Blitz.stdin.onClose(handleInputClose);
