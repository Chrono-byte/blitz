globalThis.chatClients = null;

function broadcast(message, excludedSocket) {
  let client = globalThis.chatClients;
  while (client !== null) {
    if (
      client.socket !== excludedSocket &&
      client.socket.readyState === client.socket.OPEN
    ) {
      client.socket.send(message);
    }
    client = client.next;
  }
  console.log(message);
}

function removeClient(socket) {
  let previous = null;
  let client = globalThis.chatClients;
  while (client !== null) {
    if (client.socket === socket) {
      if (previous === null) {
        globalThis.chatClients = client.next;
      } else {
        previous.next = client.next;
      }
      return client;
    }
    previous = client;
    client = client.next;
  }
  return null;
}

function handler(request) {
  if (request.headers.get("upgrade") !== "websocket") {
    return new Response("Blitz IRC chat is available over WebSocket.\n", {
      status: 426,
      headers: { "content-type": "text/plain; charset=utf-8" },
    });
  }

  const { socket, response } = Blitz.upgradeWebSocket(request);
  let nickname = null;

  socket.onmessage = (event) => {
    const message = event.data;
    if (message.slice(0, 5) === "JOIN " && nickname === null) {
      nickname = message.slice(5, 25) || "anonymous";
      globalThis.chatClients = {
        socket,
        nickname,
        next: globalThis.chatClients,
      };
      socket.send(`* Welcome, ${nickname}. Type /quit to leave.`);
      broadcast(`* ${nickname} joined the room`, socket);
      return;
    }

    if (message.slice(0, 4) === "MSG " && nickname !== null) {
      const text = message.slice(4, 404);
      if (text.length > 0) {
        broadcast(`<${nickname}> ${text}`, socket);
      }
      return;
    }

    if (message === "QUIT") {
      socket.close(1000, "client quit");
      return;
    }

    socket.send("* Send JOIN <name> first, then MSG <text>.");
  };

  socket.onclose = () => {
    const client = removeClient(socket);
    if (client !== null) {
      broadcast(`* ${client.nickname} left the room`, socket);
    }
  };

  return response;
}

const server = Blitz.serve({ hostname: "127.0.0.1", port: 8000 }, handler);
console.log(`Blitz IRC chat listening on ws://${server.hostname}:${server.port}/chat`);
