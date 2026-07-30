// SPDX-License-Identifier: MPL-2.0
import * as Effect from "effect/Effect";
import * as Ref from "effect/Ref";
import * as Stream from "effect/Stream";
import * as Http from "blitz:http";
import * as Runtime from "blitz:runtime";
import * as WebSocket from "blitz:websocket";
import * as Sync from "./shared/sync.ts";

const initialMarkdown = `# Welcome to Blitz Docs

This is a **realtime collaborative document** powered by a sync engine running on Blitz.

## Try it together

Open this page in two browser windows. Changes appear live, and edits to different parts of the document are rebased automatically.

- Use the toolbar for rich text
- Switch to Markdown whenever you want
- Watch collaborator presence in the top-right

> The first slice stores documents in memory. Restarting the Blitz server resets this page.
`;

type Client = {
  readonly documentId: string;
  readonly clientId: string;
  readonly name: string;
};

type State = {
  readonly documents: ReadonlyMap<string, Sync.DocumentSnapshot>;
  readonly clients: ReadonlyMap<WebSocket.Connection, Client>;
};

type SubscribeMessage = {
  readonly type: "subscribe";
  readonly documentId: string;
  readonly clientId: string;
  readonly name: string;
};

type EditMessage = Sync.EditRequest & { readonly type: "edit" };
type ClientMessage = SubscribeMessage | EditMessage;

const makeDocument = (id: string): Sync.DocumentSnapshot => ({
  id,
  title: "Untitled document",
  markdown: initialMarkdown,
  version: 0,
  changes: [],
});

const mapSet = <Key, Value>(map: ReadonlyMap<Key, Value>, key: Key, value: Value) =>
  new Map([...map, [key, value]]);

const mapDelete = <Key, Value>(map: ReadonlyMap<Key, Value>, key: Key) =>
  new Map([...map].filter(([entry]) => entry !== key));

const isRecord = (value: unknown): value is Record<string, unknown> =>
  typeof value === "object" && value !== null;

const readChange = (value: unknown): Sync.TextChange | null => {
  if (!isRecord(value)) {
    return null;
  }
  return typeof value.from === "number" &&
    Number.isInteger(value.from) &&
    typeof value.deleteCount === "number" &&
    Number.isInteger(value.deleteCount) &&
    typeof value.insert === "string"
    ? { from: value.from, deleteCount: value.deleteCount, insert: value.insert }
    : null;
};

const decodeMessage = (text: string): ClientMessage | null => {
  let value: unknown;
  try {
    value = JSON.parse(text);
  } catch {
    return null;
  }
  if (!isRecord(value) || typeof value.type !== "string") {
    return null;
  }
  if (
    value.type === "subscribe" &&
    typeof value.documentId === "string" &&
    typeof value.clientId === "string" &&
    typeof value.name === "string"
  ) {
    return {
      type: "subscribe",
      documentId: value.documentId.slice(0, 80),
      clientId: value.clientId.slice(0, 80),
      name: value.name.trim().slice(0, 40) || "Anonymous",
    };
  }
  const change = readChange(value.change);
  if (
    value.type === "edit" &&
    typeof value.documentId === "string" &&
    typeof value.clientId === "string" &&
    typeof value.mutationId === "string" &&
    typeof value.baseVersion === "number" &&
    Number.isInteger(value.baseVersion) &&
    change !== null
  ) {
    return {
      type: "edit",
      documentId: value.documentId,
      clientId: value.clientId,
      mutationId: value.mutationId,
      baseVersion: value.baseVersion,
      change,
    };
  }
  return null;
};

const sendJson = (connection: WebSocket.Connection, value: unknown) =>
  connection.send(JSON.stringify(value)).pipe(
    Effect.map(() => true),
    Effect.catchAll(() => Effect.succeed(false)),
  );

const subscribers = (state: State, documentId: string) =>
  [...state.clients.entries()].filter(
    ([, client]) => client.documentId === documentId,
  );

const broadcast = (stateRef: Ref.Ref<State>, documentId: string, value: unknown) =>
  Ref.get(stateRef).pipe(
    Effect.flatMap((state) =>
      Effect.all(
        subscribers(state, documentId).map(([connection]) =>
          sendJson(connection, value),
        ),
      ),
    ),
    Effect.asVoid,
  );

const publishPresence = (stateRef: Ref.Ref<State>, documentId: string) =>
  Ref.get(stateRef).pipe(
    Effect.flatMap((state) =>
      broadcast(stateRef, documentId, {
        type: "presence",
        documentId,
        users: subscribers(state, documentId).map(([, client]) => ({
          clientId: client.clientId,
          name: client.name,
        })),
      }),
    ),
  );

const subscribe = (
  stateRef: Ref.Ref<State>,
  connection: WebSocket.Connection,
  message: SubscribeMessage,
) =>
  Ref.modify(stateRef, (state) => {
    const document = state.documents.get(message.documentId) ??
      makeDocument(message.documentId);
    return [
      document,
      {
        documents: mapSet(state.documents, message.documentId, document),
        clients: mapSet(state.clients, connection, {
          documentId: message.documentId,
          clientId: message.clientId,
          name: message.name,
        }),
      },
    ];
  }).pipe(
    Effect.flatMap((document) =>
      sendJson(connection, { type: "snapshot", document }).pipe(
        Effect.zipRight(publishPresence(stateRef, message.documentId)),
      ),
    ),
  );

const edit = (
  stateRef: Ref.Ref<State>,
  connection: WebSocket.Connection,
  message: EditMessage,
) =>
  Ref.modify(stateRef, (state) => {
    const client = state.clients.get(connection);
    const document = state.documents.get(message.documentId);
    if (
      client === undefined ||
      document === undefined ||
      client.clientId !== message.clientId ||
      client.documentId !== message.documentId
    ) {
      return [null, state];
    }

    const result = Sync.applyEdit(document, message);
    return result._tag === "Applied"
      ? [
          result,
          {
            documents: mapSet(state.documents, document.id, result.document),
            clients: state.clients,
          },
        ]
      : [result, state];
  }).pipe(
    Effect.flatMap((result) => {
      if (result === null) {
        return sendJson(connection, {
          type: "error",
          message: "Subscribe before editing",
        });
      }
      if (result._tag === "Conflict") {
        return sendJson(connection, {
          type: "conflict",
          document: result.document,
          mutationId: message.mutationId,
          reason: result.reason,
        });
      }
      return broadcast(stateRef, message.documentId, {
        type: "patch",
        document: result.document,
        change: result.change,
      });
    }),
    Effect.asVoid,
  );

const handleMessage = (
  stateRef: Ref.Ref<State>,
  connection: WebSocket.Connection,
  message: WebSocket.Message,
) => {
  if (message._tag === "Binary") {
    return connection.close({ code: 1003, reason: "text frames only" });
  }
  if (message.data.length > 262_144) {
    return connection.close({ code: 1009, reason: "message too large" });
  }

  const decoded = decodeMessage(message.data);
  if (decoded === null) {
    return sendJson(connection, { type: "error", message: "Invalid message" }).pipe(
      Effect.asVoid,
    );
  }
  return decoded.type === "subscribe"
    ? subscribe(stateRef, connection, decoded).pipe(Effect.asVoid)
    : edit(stateRef, connection, decoded);
};

const detach = (stateRef: Ref.Ref<State>, connection: WebSocket.Connection) =>
  Ref.modify(stateRef, (state) => [
    state.clients.get(connection),
    {
      documents: state.documents,
      clients: mapDelete(state.clients, connection),
    },
  ]).pipe(
    Effect.flatMap((client) =>
      client === undefined
        ? Effect.succeed(undefined)
        : publishPresence(stateRef, client.documentId),
    ),
  );

const processConnection = (
  stateRef: Ref.Ref<State>,
  connection: WebSocket.Connection,
) =>
  Stream.runForEach(connection.messages, (message) =>
    handleMessage(stateRef, connection, message),
  ).pipe(
    Effect.catchAll((error) =>
      Effect.sync(() => console.error(`WebSocket error: ${error.message}`)),
    ),
    Effect.ensuring(detach(stateRef, connection)),
  );

const handleSync = (
  stateRef: Ref.Ref<State>,
  request: Request,
  context: Http.RequestContext,
) =>
  WebSocket.upgrade(request).pipe(
    Effect.flatMap((upgrade) =>
      context.fork(
        Effect.scoped(
          upgrade.accept.pipe(
            Effect.flatMap((connection) => processConnection(stateRef, connection)),
          ),
        ),
      ).pipe(Effect.as(upgrade.response)),
    ),
    Effect.catchAll(() =>
      Effect.succeed(
        new Response("WebSocket upgrade required\n", {
          status: 426,
          headers: { upgrade: "websocket" },
        }),
      ),
    ),
  );

const handler = (
  stateRef: Ref.Ref<State>,
  request: Request,
  context: Http.RequestContext,
) => {
  const path = new URL(request.url).pathname;
  if (path === "/health") {
    return Effect.succeed(
      new Response(JSON.stringify({ status: "ok" }), {
        headers: { "content-type": "application/json" },
      }),
    );
  }
  if (path === "/sync") {
    return handleSync(stateRef, request, context);
  }
  return Effect.succeed(new Response("Not Found\n", { status: 404 }));
};

const program = Ref.make<State>({
  documents: new Map(),
  clients: new Map(),
}).pipe(
  Effect.flatMap((stateRef) =>
    Effect.sync(() => {
      console.log("Blitz Docs sync server listening on http://0.0.0.0:8000");
      console.log("WebSocket endpoint: ws://<host>:8000/sync");
    }).pipe(
      Effect.zipRight(
        Http.serve({
          hostname: "0.0.0.0",
          port: 8000,
          maxRequestBodyBytes: 262_144,
          handler: (request, context) => handler(stateRef, request, context),
        }),
      ),
    ),
  ),
);

Runtime.runMain(Runtime.provideLive(program));
