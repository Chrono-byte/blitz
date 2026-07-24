import * as Context from "effect/Context";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as Stream from "effect/Stream";
import * as TerminalHost from "blitz-internal:terminal";

const hostError = (operation, error) => {
  if (
    error !== null &&
    typeof error === "object" &&
    error._tag === "TerminalError"
  ) {
    return error;
  }
  return {
    _tag: "TerminalError",
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
        TerminalHost.cancel(operationId);
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
    if (cancelled && operationId !== undefined) {
      TerminalHost.cancel(operationId);
    }
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

const lineStream = () => Stream.asyncScoped(
  (emit) => Effect.acquireRelease(
    Effect.try({
      try: () => TerminalHost.subscribeLines((event) => {
        if (event.type === "line") {
          emit.single(event.line);
        } else if (event.type === "close") {
          emit.end();
        } else if (event.type === "error") {
          emit.fail(hostError("lines", event.error));
        }
      }),
      catch: (error) => hostError("lines", error),
    }),
    (subscription) => Effect.sync(() => TerminalHost.cancel(subscription)),
  ),
  {
    bufferSize: 64,
    strategy: "suspend",
  },
);

const live = Object.freeze({
  write: (text) => hostEffect(
    "write",
    (completeResult) => TerminalHost.write(text, completeResult),
  ),
  writeLine: (text) => hostEffect(
    "writeLine",
    (completeResult) => TerminalHost.writeLine(text, completeResult),
  ),
  setPrompt: (prompt) => hostEffect(
    "setPrompt",
    (completeResult) => TerminalHost.setPrompt(prompt, completeResult),
  ),
  lines: lineStream(),
  close: () => hostEffect(
    "close",
    (completeResult) => TerminalHost.close(completeResult),
  ),
});

export const Terminal = Context.GenericTag("blitz:terminal/Terminal");

export const layerLive = Layer.succeed(Terminal, live);

export const write = Effect.serviceFunctionEffect(
  Terminal,
  (service) => service.write,
);

export const writeLine = Effect.serviceFunctionEffect(
  Terminal,
  (service) => service.writeLine,
);

export const setPrompt = Effect.serviceFunctionEffect(
  Terminal,
  (service) => service.setPrompt,
);

export const lines = Stream.unwrap(
  Effect.contextWith((context) => Context.get(context, Terminal).lines)
);

export const close = Effect.serviceFunctionEffect(
  Terminal,
  (service) => service.close,
);
