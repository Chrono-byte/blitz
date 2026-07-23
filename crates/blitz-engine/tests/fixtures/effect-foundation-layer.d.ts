import type * as Context from "effect/Context";
import type * as Effect from "effect/Effect";
export interface Layer<Out, Err = never, Req = never> {
  readonly _Out: (_: never) => Out;
  readonly _Err: (_: never) => Err;
  readonly _Req: (_: Req) => void;
}
export declare const succeed: <Id, Service>(tag: Context.Tag<Id, Service>, service: Service) => Layer<Id>;
export declare const effect: <Id, Service, E, R>(tag: Context.Tag<Id, Service>, service: Effect.Effect<Service, E, R>) => Layer<Id, E, R>;
