import type * as Cause from "effect/Cause";
import type * as Effect from "effect/Effect";
import type * as Layer from "effect/Layer";

interface RuntimeFileSystem {
  readonly readFile: (...arguments_: never[]) => unknown;
  readonly readTextFile: (...arguments_: never[]) => unknown;
  readonly writeFile: (...arguments_: never[]) => unknown;
  readonly writeTextFile: (...arguments_: never[]) => unknown;
}

interface RuntimeHttpServer {
  readonly listen: (...arguments_: never[]) => unknown;
  readonly serve: (...arguments_: never[]) => unknown;
}

interface RuntimeTerminal {
  readonly write: (...arguments_: never[]) => unknown;
  readonly writeLine: (...arguments_: never[]) => unknown;
  readonly setPrompt: (...arguments_: never[]) => unknown;
  readonly close: (...arguments_: never[]) => unknown;
}

interface RuntimeWebSocket {
  readonly connect: (...arguments_: never[]) => unknown;
  readonly upgrade: (...arguments_: never[]) => unknown;
}

/** Services supplied by the aggregate live Layer. */
export type RuntimeServices =
  | RuntimeFileSystem
  | RuntimeHttpServer
  | RuntimeTerminal
  | RuntimeWebSocket;

export interface RunMainOptions {
  readonly disableSignalHandlers?: boolean;
  readonly reportFailure?: (cause: Cause.Cause<unknown>) => void;
}

export declare const layerLive: Layer.Layer<RuntimeServices>;

export declare const provideLive: <Success, Error, Requirements>(
  program: Effect.Effect<Success, Error, Requirements>,
) => Effect.Effect<
  Success,
  Error,
  Exclude<Requirements, RuntimeServices>
>;

export declare const runMain: <Success, Error>(
  program: Effect.Effect<Success, Error, RuntimeServices>,
  options?: RunMainOptions,
) => void;
