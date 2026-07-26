import type * as Effect from "effect/Effect";
import type * as Layer from "effect/Layer";

export interface Router {
  readonly [key: string]: unknown;
}

export interface ServeOptions {
  readonly hostname?: string;
  readonly port?: number;
  readonly rpcPath?: string;
}

export interface ServeError {
  readonly _tag: "ServeError";
  readonly reason: "InvalidOptions" | "Bind" | "Transport";
  readonly message: string;
}

export type RequirementReduction<Provided, Required> = Exclude<Required, Provided>;

export interface AppDescription<Services = unknown> {
  readonly rpc: Router;
  readonly layers: ReadonlyArray<Layer.Layer<Services, unknown, unknown>>;
  provide<Provided>(
    layer: Layer.Layer<Services & Provided, unknown, Provided>
  ): AppDescription<Services & Provided>;
  serve(
    options?: ServeOptions
  ): Effect.Effect<never, ServeError, Services>;
}

export declare const App: {
  make<Services = unknown>(input: {
    readonly rpc: Router;
  }): AppDescription<Services>;
  provide<Provided, Services>(
    layer: Layer.Layer<Services & Provided, unknown, Provided>
  ): (app: AppDescription<Services>) => AppDescription<Services & Provided>;
};
