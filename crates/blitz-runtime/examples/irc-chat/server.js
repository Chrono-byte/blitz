const clients = [];

function broadcast(message, excludedSocket) {
  for (let index = 0; index < clients.length; index += 1) {
    const client = clients[index];
    if (
      client.socket !== excludedSocket &&
      client.socket.readyState === WebSocket.OPEN
    ) {
      client.socket.send(message);
    }
  }
  console.log(message);
}

function removeClient(socket) {
  for (let index = 0; index < clients.length; index += 1) {
    if (clients[index].socket === socket) {
      return clients.splice(index, 1)[0];
    }
  }
  return null;
}

function requestPath(url) {
  const authorityStart = url.indexOf("://");
  const pathStart = url.indexOf("/", authorityStart === -1 ? 0 : authorityStart + 3);
  if (pathStart === -1) {
    return "/";
  }
  const queryStart = url.indexOf("?", pathStart);
  return queryStart === -1 ? url.slice(pathStart) : url.slice(pathStart, queryStart);
}

function handler(request) {
  if (requestPath(request.url) !== "/chat") {
    return new Response("Not Found\n", {
      status: 404,
      headers: { "content-type": "text/plain; charset=utf-8" },
    });
  }

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
      clients.push({ socket, nickname });
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
