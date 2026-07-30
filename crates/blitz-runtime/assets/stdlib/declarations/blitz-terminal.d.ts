import type * as Context from "effect/Context";
import type * as Effect from "effect/Effect";
import type * as Layer from "effect/Layer";
import type * as Stream from "effect/Stream";

export interface TerminalError {
  readonly _tag: "TerminalError";
  readonly reason: "Host";
  readonly operation: "write" | "writeLine" | "setPrompt" | "lines" | "close";
  readonly message: string;
}

export interface TerminalService {
  readonly write: (
    text: string,
  ) => Effect.Effect<void, TerminalError>;
  readonly writeLine: (
    text: string,
  ) => Effect.Effect<void, TerminalError>;
  readonly setPrompt: (
    prompt: string,
  ) => Effect.Effect<void, TerminalError>;
  readonly lines: Stream.Stream<string, TerminalError>;
  readonly close: () => Effect.Effect<void, TerminalError>;
}

export type Terminal = "blitz:terminal/Terminal";

export declare const Terminal: Context.Tag<Terminal, TerminalService>;

export declare const layerLive: Layer.Layer<Terminal>;

export declare const write: (
  text: string,
) => Effect.Effect<void, TerminalError, Terminal>;

export declare const writeLine: (
  text: string,
) => Effect.Effect<void, TerminalError, Terminal>;

export declare const setPrompt: (
  prompt: string,
) => Effect.Effect<void, TerminalError, Terminal>;

export declare const lines: Stream.Stream<string, TerminalError, Terminal>;

export declare const close: () => Effect.Effect<
  void,
  TerminalError,
  Terminal
>;
