import * as Effect from "effect/Effect";
import * as Exit from "effect/Exit";
import * as Fiber from "effect/Fiber";
import * as Layer from "effect/Layer";
import { layerLive as fileSystemLayer } from "blitz:fs";
import { layerLive as httpLayer } from "blitz:http";
import { layerLive as terminalLayer } from "blitz:terminal";
import { layerLive as webSocketLayer } from "blitz:websocket";

export const layerLive = Layer.mergeAll(
  fileSystemLayer,
  httpLayer,
  terminalLayer,
  webSocketLayer
);

export const provideLive = (program) => Effect.scoped(
  Effect.flatMap(
    Layer.build(layerLive),
    context => Effect.provide(program, context)
  )
);

let rootStarted = false;

const processValue = () => {
  const value = globalThis.process;
  return value !== undefined && typeof value === "object" ? value : undefined;
};

const setExitStatus = (status) => {
  const value = processValue();
  if (value !== undefined) {
    value.exitCode = status;
  }
};

const installSignalHandlers = (onSignal) => {
  const value = processValue();
  if (value === undefined || typeof value.on !== "function") {
    return () => undefined;
  }

  const handleInterrupt = () => onSignal(130);
  const handleTerminate = () => onSignal(143);
  value.on("SIGINT", handleInterrupt);
  value.on("SIGTERM", handleTerminate);

  return () => {
    if (typeof value.removeListener === "function") {
      value.removeListener("SIGINT", handleInterrupt);
      value.removeListener("SIGTERM", handleTerminate);
    }
  };
};

const reportDefault = (cause) => {
  const value = globalThis.console;
  if (value !== undefined && typeof value.error === "function") {
    value.error(cause);
  }
};

export const runMain = (program, options = {}) => {
  if (rootStarted) {
    throw new Error("blitz:runtime runMain already has a root");
  }
  rootStarted = true;

  let signalStatus;
  let completed = false;
  let reportFailure = options.reportFailure;
  if (reportFailure === undefined) {
    reportFailure = reportDefault;
  }

  const root = Effect.runFork(provideLive(program));
  const removeSignalHandlers = options.disableSignalHandlers === true
    ? () => undefined
    : installSignalHandlers(status => {
        if (completed) {
          return;
        }
        signalStatus = status;
        Fiber.interruptFork(root);
      });

  Effect.runCallback(Fiber.await(root), {
    onExit: exit => {
      completed = true;
      removeSignalHandlers();
      if (Exit.isInterrupted(exit)) {
        setExitStatus(signalStatus === undefined ? 1 : signalStatus);
        return;
      }
      if (Exit.isFailure(exit)) {
        setExitStatus(1);
        reportFailure(exit.cause);
        return;
      }
      setExitStatus(0);
    }
  });
};
