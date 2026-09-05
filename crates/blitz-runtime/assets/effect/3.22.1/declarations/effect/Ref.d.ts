import type * as Effect from "effect/Effect";

export interface Ref<Value> extends Effect.Effect<Value> {
  readonly _Value: (_: Value) => Value;
  modify<Result>(f: (value: Value) => readonly [Result, Value]): Effect.Effect<Result>;
}

export declare const make: <Value>(value: Value) => Effect.Effect<Ref<Value>>;
export declare const get: <Value>(self: Ref<Value>) => Effect.Effect<Value>;
export declare const modify: {
  <Value, Result>(f: (value: Value) => readonly [Result, Value]): (self: Ref<Value>) => Effect.Effect<Result>;
  <Value, Result>(self: Ref<Value>, f: (value: Value) => readonly [Result, Value]): Effect.Effect<Result>;
};
export declare const set: {
  <Value>(value: Value): (self: Ref<Value>) => Effect.Effect<void>;
  <Value>(self: Ref<Value>, value: Value): Effect.Effect<void>;
};
export declare const update: {
  <Value>(f: (value: Value) => Value): (self: Ref<Value>) => Effect.Effect<void>;
  <Value>(self: Ref<Value>, f: (value: Value) => Value): Effect.Effect<void>;
};
