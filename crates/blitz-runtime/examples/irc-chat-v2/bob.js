const nickname = "Bob";
const url = process.env.BLITZ_CHAT_URL || "ws://127.0.0.1:8000/chat";
const socket = new WebSocket(url);

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

Blitz.stdin.onLine((line) => {
  const message = line.trim();
  if (message === "/quit") {
    if (socket.readyState === WebSocket.OPEN) {
      socket.send("QUIT");
    }
    return;
  }
  if (message.length === 0) {
    return;
  }
  if (socket.readyState !== WebSocket.OPEN) {
    Blitz.stdout.writeLine("Still connecting; try again in a moment.");
    return;
  }
  socket.send(`MSG ${message}`);
});

Blitz.stdin.onClose(() => {
  if (socket.readyState === WebSocket.OPEN) {
    socket.close(1000, "stdin closed");
  }
});
