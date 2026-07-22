import type * as Layer from "effect/Layer";

/** The intentionally small host boundary consumed by the later App phase. */
export interface Router {
  readonly _tag: "BlitzRouter";
}

export interface AppDescription {
  readonly rpc: Router;
  readonly layers: ReadonlyArray<Layer.Layer<unknown, unknown, unknown>>;
}

export interface ServeOptions {
  readonly hostname?: string;
  readonly port?: number;
  readonly rpcPath?: string;
}

export interface ServeError {
  readonly _tag: "ServeError";
  readonly message: string;
}

/** Phase 1 keeps the application source boundary type-only and side-effect free. */
export const phase = "asset-generation" as const;
