import type * as Context from "effect/Context";
import type * as Effect from "effect/Effect";
import type * as Scope from "effect/Scope";

interface Pipeable<Self> {
  readonly pipe: {
    <A>(ab: (self: Self) => A): A;
    <A, B>(ab: (self: Self) => A, bc: (value: A) => B): B;
    <A, B, C>(ab: (self: Self) => A, bc: (value: A) => B, cd: (value: B) => C): C;
  };
}

export interface Layer<in Success, out Error = never, out Requirements = never> extends Pipeable<Layer<Success, Error, Requirements>> {
  readonly _Success: (_: Success) => void;
  readonly _Error: (_: never) => Error;
  readonly _Requirements: (_: never) => Requirements;
}

type SuccessOf<LayerValue> = LayerValue extends Layer<infer Success, infer _Error, infer _Requirements> ? Success : never;
type ErrorOf<LayerValue> = LayerValue extends Layer<infer _Success, infer Error, infer _Requirements> ? Error : never;
type RequirementsOf<LayerValue> = LayerValue extends Layer<infer _Success, infer _Error, infer Requirements> ? Requirements : never;
type MergeableLayer = Layer<never, unknown, unknown>;
type LayerCollection = readonly [MergeableLayer, ...ReadonlyArray<MergeableLayer>];

export declare const empty: Layer<never>;
export declare const succeed: {
  <Id, Service>(tag: Context.Tag<Id, Service>): (service: Service) => Layer<Id>;
  <Id, Service>(tag: Context.Tag<Id, Service>, service: Service): Layer<Id>;
};
export declare const effect: {
  <Id, Service>(tag: Context.Tag<Id, Service>): <Error, Requirements>(service: Effect.Effect<Service, Error, Requirements>) => Layer<Id, Error, Requirements>;
  <Id, Service, Error, Requirements>(tag: Context.Tag<Id, Service>, service: Effect.Effect<Service, Error, Requirements>): Layer<Id, Error, Requirements>;
};
export declare const mergeAll: <const Layers extends LayerCollection>(...layers: Layers) => Layer<SuccessOf<Layers[number]>, ErrorOf<Layers[number]>, RequirementsOf<Layers[number]>>;
export declare const build: <Success, Error, Requirements>(layer: Layer<Success, Error, Requirements>) => Effect.Effect<Context.Context<Success>, Error, Requirements | Scope.Scope>;
export declare const provide: {
  <Requirements, LayerError, Provided>(that: Layer<Provided, LayerError, Requirements>): <InputRequirements, Error, Success>(self: Layer<Success, Error, InputRequirements>) => Layer<Success, Error | LayerError, Requirements | Exclude<InputRequirements, Provided>>;
  <const Layers extends LayerCollection>(that: Layers): <InputRequirements, Error, Success>(self: Layer<Success, Error, InputRequirements>) => Layer<Success, Error | ErrorOf<Layers[number]>, RequirementsOf<Layers[number]> | Exclude<InputRequirements, SuccessOf<Layers[number]>>>;
  <InputRequirements, Error, Success, Requirements, LayerError, Provided>(self: Layer<Success, Error, InputRequirements>, that: Layer<Provided, LayerError, Requirements>): Layer<Success, Error | LayerError, Requirements | Exclude<InputRequirements, Provided>>;
  <InputRequirements, Error, Success, const Layers extends LayerCollection>(self: Layer<Success, Error, InputRequirements>, that: Layers): Layer<Success, Error | ErrorOf<Layers[number]>, RequirementsOf<Layers[number]> | Exclude<InputRequirements, SuccessOf<Layers[number]>>>;
};
export declare const provideMerge: {
  <Requirements, LayerError, Provided>(self: Layer<Provided, LayerError, Requirements>): <InputRequirements, Error, Success>(that: Layer<Success, Error, InputRequirements>) => Layer<Provided | Success, LayerError | Error, Requirements | Exclude<InputRequirements, Provided>>;
  <InputRequirements, Error, Success, Requirements, LayerError, Provided>(that: Layer<Success, Error, InputRequirements>, self: Layer<Provided, LayerError, Requirements>): Layer<Success | Provided, Error | LayerError, Requirements | Exclude<InputRequirements, Provided>>;
};
