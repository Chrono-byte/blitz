import * as Effect from "effect/Effect";
import * as Exit from "effect/Exit";
import * as Fiber from "effect/Fiber";
import * as Layer from "effect/Layer";
import * as RuntimeHost from "blitz-internal:runtime";
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
  const rootId = RuntimeHost.reserveRoot();
  let root;
  try {
    root = Effect.runFork(program);
    RuntimeHost.attachRoot(rootId, () => {
      Effect.runSync(Fiber.interruptFork(root));
    });
  } catch (error) {
    if (root !== undefined) {
      Effect.runSync(Fiber.interruptFork(root));
    }
    RuntimeHost.failRoot(rootId);
    throw error;
  }

  let signalStatus;
  let completed = false;
  let reportFailure = options.reportFailure;
  if (reportFailure === undefined) {
    reportFailure = reportDefault;
  }

  const removeSignalHandlers = options.disableSignalHandlers === true
    ? () => undefined
    : installSignalHandlers(status => {
        if (completed) {
          return;
        }
        signalStatus = status;
        RuntimeHost.interruptRoot(status);
      });

  Effect.runCallback(Fiber.join(root), {
    onExit: exit => {
      completed = true;
      removeSignalHandlers();
      if (Exit.isInterrupted(exit)) {
        const status = signalStatus === undefined ? 1 : signalStatus;
        setExitStatus(status);
        RuntimeHost.completeRoot(rootId, status);
        return;
      }
      if (Exit.isFailure(exit)) {
        setExitStatus(1);
        try {
          reportFailure(exit.cause);
        } finally {
          RuntimeHost.completeRoot(rootId, 1);
        }
        return;
      }
      setExitStatus(0);
      RuntimeHost.completeRoot(rootId, 0);
    }
  });
};
