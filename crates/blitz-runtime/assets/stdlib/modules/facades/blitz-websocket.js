import * as Context from "effect/Context";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as Stream from "effect/Stream";
import * as Transport from "blitz-internal:websocket";

const hostError = (operation, error) => {
  if (
    error !== null &&
    typeof error === "object" &&
    error._tag === "WebSocketError"
  ) {
    return error;
  }
  return {
    _tag: "WebSocketError",
    reason: "Host",
    operation,
    message: error instanceof Error ? error.message : String(error),
  };
};

const complete = (operation, resume, result) => {
  if (result !== null && typeof result === "object" && result.ok === true) {
    resume(Effect.succeed(result.value));
    return;
  }
  const error = result !== null && typeof result === "object"
    ? result.error
    : undefined;
  resume(Effect.fail(hostError(operation, error)));
};

const hostEffect = (operation, start) => Effect.async((resume, signal) => {
  let operationId;
  let cancelled = false;
  let settled = false;

  const cancel = () => {
    if (!cancelled && !settled) {
      cancelled = true;
      if (operationId !== undefined) {
        Transport.cancel(operationId);
      }
    }
  };

  const onAbort = () => cancel();
  signal.addEventListener("abort", onAbort, { once: true });

  try {
    operationId = start((result) => {
      if (cancelled || settled) return;
      settled = true;
      signal.removeEventListener("abort", onAbort);
      complete(operation, resume, result);
    });
    if (cancelled) Transport.cancel(operationId);
  } catch (error) {
    if (cancelled || settled) return;
    settled = true;
    signal.removeEventListener("abort", onAbort);
    complete(operation, resume, { ok: false, error });
  }

  return Effect.sync(() => {
    signal.removeEventListener("abort", onAbort);
    cancel();
  });
});

const hostSync = (operation, evaluate) => Effect.try({
  try: evaluate,
  catch: (error) => hostError(operation, error),
});

const acceptConnection = (upgrade) => hostSync(
  "accept",
  () => Transport.accept(upgrade),
);

const closeConnection = (resource, options) => hostSync(
  "close",
  () => Transport.close(resource, options),
);

const releaseConnection = (connection) =>
  connection.close().pipe(Effect.catchAll(() => Effect.succeed(undefined)));

const messageStream = (resource, options) => Stream.asyncScoped(
    (emit) => Effect.acquireRelease(
      Effect.try({
        try: () => Transport.subscribe(resource, (event) => {
          if (event.type === "text") {
            emit.single({ _tag: "Text", data: event.data });
          } else if (event.type === "binary") {
            emit.single({ _tag: "Binary", data: event.data });
          } else if (event.type === "close") {
            emit.end();
          } else if (event.type === "error") {
            emit.fail(hostError("messages", event.error));
          }
        }),
        catch: (error) => hostError("messages", error),
      }),
      (subscription) => Effect.sync(() => Transport.cancel(subscription)),
    ),
    {
      bufferSize: options?.queueCapacity ?? 64,
      strategy: "suspend",
    },
  );

const makeConnection = (resource, options) => {
  const connection = {
    messages: messageStream(resource, options),
    send: (message) => hostSync(
      "send",
      () => Transport.send(resource, message),
    ),
    close: (closeOptions = {}) => closeConnection(resource, closeOptions),
  };
  return Object.freeze(connection);
};

const service = Object.freeze({
  connect: (options) => Effect.acquireRelease(
    hostEffect(
      "connect",
      (completeResult) => Transport.connect(options, completeResult),
    ).pipe(Effect.map((resource) => makeConnection(resource, options))),
    releaseConnection,
  ),
  upgrade: (request, options = {}) => hostSync(
    "prepareUpgrade",
    () => Transport.prepareUpgrade(request, options),
  ).pipe(Effect.map((upgrade) => Object.freeze({
    _tag: "WebSocketUpgrade",
    response: upgrade.response,
    accept: Effect.acquireRelease(
      acceptConnection(upgrade).pipe(Effect.map((resource) => makeConnection(resource, options))),
      releaseConnection,
    ),
  }))),
});

export const WebSocket = Context.GenericTag("blitz:websocket");

export const connect = service.connect;

export const upgrade = service.upgrade;

export const layerLive = Layer.succeed(WebSocket, service);
export const layer = layerLive;
