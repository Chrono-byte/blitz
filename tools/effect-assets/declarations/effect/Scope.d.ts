import type * as Effect from "effect/Effect";

export interface Scope {
  readonly _tag: "Scope";
}

export declare const addFinalizer: (finalizer: Effect.Effect<unknown, never, never>) => Effect.Effect<void, never, Scope>;
export declare const make: () => Effect.Effect<Scope>;
export declare const close: (scope: Scope, exit: unknown) => Effect.Effect<void>;
