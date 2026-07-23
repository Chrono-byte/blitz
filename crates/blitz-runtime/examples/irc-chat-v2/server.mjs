const MAX_NICKNAME_LENGTH = 20;
const MAX_MESSAGE_LENGTH = 400;
const MAX_FRAME_LENGTH = 4_096;
const RATE_LIMIT_WINDOW_MS = 2_000;
const RATE_LIMIT_MESSAGES = 8;

const clients = new Map();

function send(socket, message) {
  if (socket.readyState !== WebSocket.OPEN) {
    clients.delete(socket);
    return false;
  }

  try {
    socket.send(message);
    return true;
  } catch {
    clients.delete(socket);
    return false;
  }
}

function broadcast(message) {
  for (const socket of clients.keys()) {
    send(socket, message);
  }
  console.log(message);
}

function removeClient(socket) {
  const client = clients.get(socket);
  if (client === undefined) {
    return;
  }

  clients.delete(socket);
  broadcast(`* ${client.nickname} left the room`);
}

function normalizeNickname(input) {
  const nickname = input.trim();
  if (
    nickname.length === 0 ||
    nickname.length > MAX_NICKNAME_LENGTH ||
    !/^[A-Za-z0-9_-]+$/.test(nickname)
  ) {
    return null;
  }
  return nickname;
}

function nicknameIsTaken(nickname) {
  const key = nickname.toLowerCase();
  for (const client of clients.values()) {
    if (client.nickname.toLowerCase() === key) {
      return true;
    }
  }
  return false;
}

function sanitizeMessage(input) {
  return input
    .replace(/[\u0000-\u001f\u007f-\u009f]/g, "")
    .trim()
    .slice(0, MAX_MESSAGE_LENGTH);
}

function isRateLimited(client) {
  const now = Date.now();
  if (now - client.windowStartedAt >= RATE_LIMIT_WINDOW_MS) {
    client.windowStartedAt = now;
    client.messageCount = 0;
  }

  client.messageCount += 1;
  return client.messageCount > RATE_LIMIT_MESSAGES;
}

function close(socket, code, reason) {
  try {
    socket.close(code, reason);
  } catch {
    removeClient(socket);
  }
}

function handleChat(request) {
  const upgrade = request.headers.get("upgrade");
  if (upgrade === null || upgrade.toLowerCase() !== "websocket") {
    return new Response("Blitz IRC chat is available over WebSocket.\n", {
      status: 426,
      headers: {
        "content-type": "text/plain; charset=utf-8",
        upgrade: "websocket",
      },
    });
  }

  const { socket, response } = Blitz.upgradeWebSocket(request);

  socket.onmessage = (event) => {
    if (typeof event.data !== "string") {
      close(socket, 1003, "text frames only");
      return;
    }

    const message = event.data;
    if (message.length > MAX_FRAME_LENGTH) {
      close(socket, 1009, "message too large");
      return;
    }

    if (message.startsWith("JOIN ") && !clients.has(socket)) {
      const nickname = normalizeNickname(message.slice(5));
      if (nickname === null) {
        send(
          socket,
          `* Nicknames must be 1-${MAX_NICKNAME_LENGTH} letters, numbers, underscores, or hyphens.`,
        );
        return;
      }
      if (nicknameIsTaken(nickname)) {
        send(socket, "* That nickname is already in use.");
        return;
      }

      const client = {
        nickname,
        windowStartedAt: Date.now(),
        messageCount: 0,
      };
      clients.set(socket, client);
      if (!send(socket, `* Welcome, ${nickname}. Type /quit to leave.`)) {
        return;
      }
      broadcast(`* ${nickname} joined the room`);
      return;
    }

    if (message.startsWith("MSG ")) {
      const client = clients.get(socket);
      if (client !== undefined) {
        if (isRateLimited(client)) {
          send(socket, "* You are sending messages too quickly.");
          return;
        }

        const text = sanitizeMessage(message.slice(4));
        if (text.length > 0) {
          // Chat messages are authoritative server events, including for the sender.
          broadcast(`<${client.nickname}> ${text}`);
        }
        return;
      }
    }

    if (message === "QUIT") {
      close(socket, 1000, "client quit");
      return;
    }

    send(socket, "* Send JOIN <name> first, then MSG <text>.");
  };

  socket.onerror = () => {
    removeClient(socket);
    close(socket, 1011, "socket error");
  };

  socket.onclose = () => {
    removeClient(socket);
  };

  return response;
}

function handler(request) {
  const path = new URL(request.url).pathname;
  if (path === "/") {
    return new Response(
      "Blitz IRC chat v2 is running. Connect over /chat.\n",
      { headers: { "content-type": "text/plain; charset=utf-8" } },
    );
  }
  if (path === "/health") {
    return new Response("ok\n", {
      headers: { "content-type": "text/plain; charset=utf-8" },
    });
  }
  if (path === "/chat") {
    return handleChat(request);
  }
  return new Response("Not Found\n", {
    status: 404,
    headers: { "content-type": "text/plain; charset=utf-8" },
  });
}

const server = Blitz.serve({ hostname: "127.0.0.1", port: 8000 }, handler);

console.log(`Blitz IRC chat listening on http://${server.hostname}:${server.port}`);
console.log(`WebSocket endpoint: ws://${server.hostname}:${server.port}/chat`);
