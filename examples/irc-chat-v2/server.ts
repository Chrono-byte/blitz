// SPDX-License-Identifier: MPL-2.0
import * as Effect from "effect/Effect";
import * as Ref from "effect/Ref";
import * as Stream from "effect/Stream";
import * as Http from "blitz:http";
import * as Runtime from "blitz:runtime";
import * as WebSocket from "blitz:websocket";

const MAX_NICKNAME_LENGTH: number = 20;
const MAX_MESSAGE_LENGTH: number = 400;
const MAX_FRAME_LENGTH: number = 4_096;
const RATE_LIMIT_WINDOW_MS: number = 2_000;
const RATE_LIMIT_MESSAGES: number = 8;

type Client = {
  readonly nickname: string;
  readonly windowStartedAt: number;
  readonly messageCount: number;
};

type Clients = Map<WebSocket.Connection, Client>;
type ClientsRef = Ref.Ref<Clients>;

type JoinResult =
  | { readonly _tag: "AlreadyJoined" }
  | { readonly _tag: "Taken" }
  | { readonly _tag: "Joined"; readonly client: Client };

const mapSet = <Key, Value>(map: Map<Key, Value>, key: Key, value: Value) =>
  new Map([...map, [key, value]]);

const mapDelete = <Key, Value>(map: Map<Key, Value>, key: Key) =>
  new Map([...map].filter(([entry]) => entry !== key));

const normalizeNickname = (input: string) => {
  const nickname = input.trim();
  if (
    nickname.length === 0 ||
    nickname.length > MAX_NICKNAME_LENGTH ||
    !/^[A-Za-z0-9_-]+$/.test(nickname)
  ) {
    return null;
  }
  return nickname;
};

const sanitizeMessage = (input: string) =>
  input
    .replace(/[\u0000-\u001f\u007f-\u009f]/g, "")
    .trim()
    .slice(0, MAX_MESSAGE_LENGTH);

const nicknameIsTaken = (clients: Clients, nickname: string) => {
  const key = nickname.toLowerCase();
  for (const client of clients.values()) {
    if (client.nickname.toLowerCase() === key) {
      return true;
    }
  }
  return false;
};

const detach = (clientsRef: ClientsRef, connection: WebSocket.Connection) =>
  Ref.modify(clientsRef, (clients) => [
    clients.get(connection),
    mapDelete(clients, connection),
  ]);

const closeQuietly = (
  connection: WebSocket.Connection,
  options: WebSocket.CloseOptions,
) =>
  connection.close(options).pipe(
    Effect.catchAll(() => Effect.succeed(undefined)),
  );

const send = (
  clientsRef: ClientsRef,
  connection: WebSocket.Connection,
  message: string,
) =>
  connection.send(message).pipe(
    Effect.map(() => true),
    Effect.catchAll(() => detach(clientsRef, connection).pipe(Effect.as(false))),
  );

const broadcast = (clientsRef: ClientsRef, message: string) =>
  Ref.get(clientsRef).pipe(
    Effect.flatMap((clients) =>
      Effect.all(
        [...clients.keys()].map((connection) =>
          send(clientsRef, connection, message),
        ),
      ).pipe(Effect.asVoid),
    ),
    Effect.tap(() => Effect.sync(() => console.log(message))),
  );

const removeClient = (
  clientsRef: ClientsRef,
  connection: WebSocket.Connection,
) =>
  detach(clientsRef, connection).pipe(
    Effect.flatMap((client) =>
      client === undefined
        ? Effect.succeed(undefined)
        : broadcast(clientsRef, `* ${client.nickname} left the room`),
    ),
  );

const joinClient = (
  clientsRef: ClientsRef,
  connection: WebSocket.Connection,
  nickname: string,
) =>
  Ref.modify<Clients, JoinResult>(clientsRef, (clients) => {
    if (clients.has(connection)) {
      return [{ _tag: "AlreadyJoined" }, clients];
    }
    if (nicknameIsTaken(clients, nickname)) {
      return [{ _tag: "Taken" }, clients];
    }

    const client = {
      nickname,
      windowStartedAt: Date.now(),
      messageCount: 0,
    };
    return [
      { _tag: "Joined", client },
      mapSet(clients, connection, client),
    ];
  });

const registerMessage = (
  clientsRef: ClientsRef,
  connection: WebSocket.Connection,
) =>
  Ref.modify(clientsRef, (clients) => {
    const client = clients.get(connection);
    if (client === undefined) {
      return [undefined, clients];
    }

    const now = Date.now();
    const reset = now - client.windowStartedAt >= RATE_LIMIT_WINDOW_MS;
    const updatedClient = {
      ...client,
      windowStartedAt: reset ? now : client.windowStartedAt,
      messageCount: reset ? 1 : client.messageCount + 1,
    };
    return [
      {
        client: updatedClient,
        limited: updatedClient.messageCount > RATE_LIMIT_MESSAGES,
      },
      mapSet(clients, connection, updatedClient),
    ];
  });

const handleMessage = (
  clientsRef: ClientsRef,
  connection: WebSocket.Connection,
  message: WebSocket.Message,
) => {
  if (message._tag === "Binary") {
    return closeQuietly(connection, { code: 1003, reason: "text frames only" });
  }

  if (message.data.length > MAX_FRAME_LENGTH) {
    return closeQuietly(connection, { code: 1009, reason: "message too large" });
  }

  if (message.data.startsWith("JOIN ")) {
    const nickname = normalizeNickname(message.data.slice(5));
    if (nickname === null) {
      return send(
        clientsRef,
        connection,
        `* Nicknames must be 1-${MAX_NICKNAME_LENGTH} letters, numbers, underscores, or hyphens.`,
      ).pipe(Effect.asVoid);
    }

    return joinClient(clientsRef, connection, nickname).pipe(
      Effect.flatMap((result) => {
        if (result._tag === "AlreadyJoined") {
          return send(clientsRef, connection, "* You already joined the room.").pipe(
            Effect.asVoid,
          );
        }
        if (result._tag === "Taken") {
          return send(clientsRef, connection, "* That nickname is already in use.").pipe(
            Effect.asVoid,
          );
        }

        return send(
          clientsRef,
          connection,
          `* Welcome, ${result.client.nickname}. Type /quit to leave.`,
        ).pipe(
          Effect.flatMap((sent) =>
            sent
              ? broadcast(clientsRef, `* ${result.client.nickname} joined the room`)
              : Effect.succeed(undefined),
          ),
        );
      }),
    );
  }

  if (message.data.startsWith("MSG ")) {
    return registerMessage(clientsRef, connection).pipe(
      Effect.flatMap((result) => {
        if (result === undefined) {
          return send(
            clientsRef,
            connection,
            "* Send JOIN <name> first, then MSG <text>.",
          ).pipe(Effect.asVoid);
        }
        if (result.limited) {
          return send(
            clientsRef,
            connection,
            "* You are sending messages too quickly.",
          ).pipe(Effect.asVoid);
        }

        const text = sanitizeMessage(message.data.slice(4));
        return text.length === 0
          ? Effect.succeed(undefined)
          : broadcast(clientsRef, `<${result.client.nickname}> ${text}`);
      }),
    );
  }

  if (message.data === "QUIT") {
    return closeQuietly(connection, { code: 1000, reason: "client quit" });
  }

  return send(
    clientsRef,
    connection,
    "* Send JOIN <name> first, then MSG <text>.",
  ).pipe(Effect.asVoid);
};

const processConnection = (
  clientsRef: ClientsRef,
  connection: WebSocket.Connection,
) =>
  Stream.runForEach(
    connection.messages,
    (message) => handleMessage(clientsRef, connection, message),
  ).pipe(
    Effect.catchAll((error) =>
      Effect.sync(() => console.error(`WebSocket error: ${error.message}`)),
    ),
    Effect.ensuring(removeClient(clientsRef, connection)),
  );

const handleChat = (
  clientsRef: ClientsRef,
  request: Request,
  context: Http.RequestContext,
) => {
  const upgradeHeader = request.headers.get("upgrade");
  if (upgradeHeader === null || upgradeHeader.toLowerCase() !== "websocket") {
    return Effect.succeed(
      new Response("Blitz IRC chat is available over WebSocket.\n", {
        status: 426,
        headers: {
          "content-type": "text/plain; charset=utf-8",
          upgrade: "websocket",
        },
      }),
    );
  }

  return WebSocket.upgrade(request).pipe(
    Effect.flatMap((upgrade) =>
      context.fork(
        Effect.scoped(
          upgrade.accept.pipe(
            Effect.flatMap((connection) => processConnection(clientsRef, connection)),
          ),
        ),
      ).pipe(Effect.as(upgrade.response)),
    ),
  );
};

const handler = (
  clientsRef: ClientsRef,
  request: Request,
  context: Http.RequestContext,
) => {
  const path = new URL(request.url).pathname;
  if (path === "/") {
    return Effect.succeed(
      new Response("Blitz IRC chat v2 is running. Connect over /chat.\n", {
        headers: { "content-type": "text/plain; charset=utf-8" },
      }),
    );
  }
  if (path === "/health") {
    return Effect.succeed(
      new Response("ok\n", {
        headers: { "content-type": "text/plain; charset=utf-8" },
      }),
    );
  }
  if (path === "/chat") {
    return handleChat(clientsRef, request, context);
  }
  return Effect.succeed(
    new Response("Not Found\n", {
      status: 404,
      headers: { "content-type": "text/plain; charset=utf-8" },
    }),
  );
};

const serverProgram = Ref.make(new Map<WebSocket.Connection, Client>()).pipe(
  Effect.flatMap((clientsRef) => {
    const announce = Effect.sync(() => {
      console.log("Blitz IRC chat listening on http://127.0.0.1:8000");
      console.log("WebSocket endpoint: ws://127.0.0.1:8000/chat");
    });
    const serve = Http.serve({
      hostname: "127.0.0.1",
      port: 8000,
      handler: (request, context) => handler(clientsRef, request, context),
    });
    return announce.pipe(Effect.zipRight(serve));
  }),
);

Runtime.runMain(Runtime.provideLive(serverProgram));
