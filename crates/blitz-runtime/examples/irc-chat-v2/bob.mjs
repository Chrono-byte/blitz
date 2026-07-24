import * as Effect from "effect/Effect";
import * as Fiber from "effect/Fiber";
import * as Stream from "effect/Stream";
import * as Runtime from "blitz:runtime";
import * as Terminal from "blitz:terminal";
import * as WebSocket from "blitz:websocket";

const nickname = "Bob";
const url = process.env.BLITZ_CHAT_URL || "ws://127.0.0.1:8000/chat";

const closeTerminal = Terminal.close().pipe(
  Effect.catchAll(() => Effect.succeed(undefined)),
);

const closeSocket = (connection) =>
  connection.close({ code: 1000, reason: "terminal closed" }).pipe(
    Effect.catchAll(() => Effect.succeed(undefined)),
  );

const reportSocketError = (error) =>
  Terminal.writeLine(`Connection error: ${error.message}`).pipe(
    Effect.catchAll(() => Effect.succeed(undefined)),
  );

const sendLine = (connection, line) => {
  const message = line.trim();
  if (message.length === 0) {
    return Effect.succeed(undefined);
  }
  if (message === "/quit") {
    return connection.send("QUIT").pipe(
      Effect.catchAll(reportSocketError),
      Effect.zipRight(closeTerminal),
    );
  }
  return connection.send(`MSG ${message}`).pipe(
    Effect.catchAll(reportSocketError),
  );
};

const receiveMessages = (connection) =>
  Stream.runForEach(connection.messages, (message) =>
    message._tag === "Text"
      ? Terminal.writeLine(message.data)
      : Terminal.writeLine("* Received an unsupported binary frame."),
  ).pipe(Effect.ensuring(closeTerminal));

const runClient = (connection) =>
  Effect.fork(receiveMessages(connection)).pipe(
    Effect.flatMap((receiver) =>
      Stream.runForEach(Terminal.lines, (line) => sendLine(connection, line)).pipe(
        Effect.ensuring(Fiber.interrupt(receiver)),
      ),
    ),
  );

const program = Effect.scoped(
  WebSocket.connect({ url }).pipe(
    Effect.flatMap((connection) =>
      Terminal.setPrompt(`[${nickname}] > `).pipe(
        Effect.zipRight(Terminal.writeLine(`Connecting to ${url}...`)),
        Effect.zipRight(
          connection.send(`JOIN ${nickname}`).pipe(
            Effect.catchAll(reportSocketError),
          ),
        ),
        Effect.zipRight(runClient(connection)),
        Effect.ensuring(closeSocket(connection)),
      ),
    ),
  ),
);

Runtime.runMain(program);
