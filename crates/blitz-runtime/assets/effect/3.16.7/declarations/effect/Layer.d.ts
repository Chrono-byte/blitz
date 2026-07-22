import type * as Context from "effect/Context";
import type * as Effect from "effect/Effect";

export interface Layer<Success, Error = never, Requirements = never> {
  readonly _Success: (_: never) => Success;
  readonly _Error: (_: never) => Error;
  readonly _Requirements: (_: Requirements) => void;
}

export declare const empty: Layer<never>;
export declare const succeed: <Service, Identifier>(tag: Context.Tag<Service, Identifier>, service: Service) => Layer<Identifier>;
export declare const effect: <Service, Identifier, Error, Requirements>(tag: Context.Tag<Service, Identifier>, service: Effect.Effect<Service, Error, Requirements>) => Layer<Identifier, Error, Requirements>;
export declare const mergeAll: <Layers extends ReadonlyArray<Layer<unknown, unknown, unknown>>>(...layers: Layers) => Layer<unknown, unknown, unknown>;
export declare const build: <Success, Error, Requirements>(layer: Layer<Success, Error, Requirements>) => Effect.Effect<Context.Context<Success>, Error, Requirements | import("effect/Scope").Scope>;
export declare const provide: <Provided, LayerError, LayerRequirements>(layer: Layer<Provided, LayerError, LayerRequirements>) => <Success, Error, Requirements>(effect: Effect.Effect<Success, Error, Requirements>) => Effect.Effect<Success, Error | LayerError, Exclude<Requirements, Provided> | LayerRequirements>;
