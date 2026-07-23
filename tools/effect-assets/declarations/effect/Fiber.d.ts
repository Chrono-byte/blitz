import type * as Effect from "effect/Effect";
import type * as Exit from "effect/Exit";
import type * as FiberId from "effect/FiberId";
import type * as Option from "effect/Option";

export interface Fiber<Success, Error = never> {
  id(): FiberId.FiberId;
  readonly await: Effect.Effect<Exit.Exit<Success, Error>>;
  readonly poll: Effect.Effect<Option.Option<Exit.Exit<Success, Error>>>;
  readonly children: Effect.Effect<Array<RuntimeFiber<unknown, unknown>>>;
}
export interface RuntimeFiber<Success, Error = never> extends Fiber<Success, Error> {
  readonly status: Effect.Effect<unknown>;
}

export declare const isFiber: (value: unknown) => value is Fiber<unknown, unknown>;
export declare const isRuntimeFiber: <Success, Error>(fiber: Fiber<Success, Error>) => fiber is RuntimeFiber<Success, Error>;
export declare const id: <Success, Error>(fiber: Fiber<Success, Error>) => FiberId.FiberId;
declare const _await: <Success, Error>(fiber: Fiber<Success, Error>) => Effect.Effect<Exit.Exit<Success, Error>>;
export { _await as await };
export declare const awaitAll: <Fibers extends Iterable<Fiber<unknown, unknown>>>(fibers: Fibers) => Effect.Effect<ReadonlyArray<Exit.Exit<unknown, unknown>>>;
export declare const join: <Success, Error>(fiber: Fiber<Success, Error>) => Effect.Effect<Success, Error>;
export declare const interrupt: <Success, Error>(fiber: Fiber<Success, Error>) => Effect.Effect<Exit.Exit<Success, Error>>;
export declare const interruptFork: <Success, Error>(fiber: Fiber<Success, Error>) => Effect.Effect<void>;
