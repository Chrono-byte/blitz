import type * as Cause from "effect/Cause";
import type * as Effect from "effect/Effect";
import type * as Layer from "effect/Layer";

/** Services supplied by the aggregate live Layer. */
export type RuntimeServices =
  | "blitz:fs/FileSystem"
  | "blitz:http/HttpServer"
  | "blitz:terminal/Terminal"
  | "blitz:websocket/WebSocket";

export interface RuntimeInitError {
  readonly _tag: "RuntimeInitError";
  readonly reason:
    | "AlreadyRunning"
    | "MissingHostPrimitive"
    | "AbiMismatch"
    | "PermissionConfiguration"
    | "Initialization";
  readonly message: string;
}

export interface RunMainOptions {
  readonly disableSignalHandlers?: boolean;
  readonly reportFailure?: (cause: Cause.Cause<unknown>) => void;
}

export declare const layerLive: Layer.Layer<RuntimeServices, RuntimeInitError>;

export declare const provideLive: <Success, Error, Requirements>(
  program: Effect.Effect<Success, Error, Requirements>,
) => Effect.Effect<
  Success,
  Error | RuntimeInitError,
  Exclude<Requirements, RuntimeServices>
>;

export declare const runMain: {
  <Success, Error>(
    program: Effect.Effect<Success, Error, never>,
  ): void;
  <Success, Error>(
    program: Effect.Effect<Success, Error, never>,
    options: RunMainOptions,
  ): void;
};
