import type * as Effect from "effect/Effect";
export interface Fiber<A, E = never> {
  readonly await: Effect.Effect<Exit<A, E>>;
}
export interface Exit<A, E> {
  readonly _tag: "Success" | "Failure";
  readonly value: A;
  readonly error: E;
}
export declare const _await: <A, E>(fiber: Fiber<A, E>) => Effect.Effect<Exit<A, E>>;
