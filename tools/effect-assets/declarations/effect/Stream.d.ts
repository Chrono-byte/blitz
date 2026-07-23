import type * as Chunk from "effect/Chunk";
import type * as Effect from "effect/Effect";

interface Pipeable<Self> {
  readonly pipe: {
    <A>(ab: (self: Self) => A): A;
    <A, B>(ab: (self: Self) => A, bc: (value: A) => B): B;
    <A, B, C>(ab: (self: Self) => A, bc: (value: A) => B, cd: (value: B) => C): C;
  };
}

export interface Stream<Success, Error = never, Requirements = never> extends Pipeable<Stream<Success, Error, Requirements>> {
  readonly _Success: (_: never) => Success;
  readonly _Error: (_: never) => Error;
  readonly _Requirements: (_: never) => Requirements;
}

export declare const make: <Values extends ReadonlyArray<unknown>>(...values: Values) => Stream<Values[number]>;
export declare const empty: Stream<never>;
export declare const fail: <Error>(error: Error) => Stream<never, Error>;
export declare const fromIterable: <Success>(values: Iterable<Success>) => Stream<Success>;
export declare const map: {
  <Success, Output>(f: (value: Success) => Output): <Error, Requirements>(stream: Stream<Success, Error, Requirements>) => Stream<Output, Error, Requirements>;
  <Success, Error, Requirements, Output>(stream: Stream<Success, Error, Requirements>, f: (value: Success) => Output): Stream<Output, Error, Requirements>;
};
export declare const runCollect: <Success, Error, Requirements>(stream: Stream<Success, Error, Requirements>) => Effect.Effect<Chunk.Chunk<Success>, Error, Requirements>;
