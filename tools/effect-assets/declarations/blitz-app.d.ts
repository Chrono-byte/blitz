import type * as Effect from "effect/Effect";
import type * as Layer from "effect/Layer";
import type { Router } from "./blitz-rpc";

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

export declare const App: {
  readonly make: (options: { readonly rpc: Router }) => AppDescription;
  readonly provide: (app: AppDescription, layer: Layer.Layer<unknown, unknown, unknown>) => AppDescription;
  readonly serve: (app: AppDescription, options?: ServeOptions) => Effect.Effect<never, ServeError, never>;
};
