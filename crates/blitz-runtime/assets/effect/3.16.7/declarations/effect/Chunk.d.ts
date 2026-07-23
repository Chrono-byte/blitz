export interface Chunk<Success> extends Iterable<Success> {
  readonly length: number;
}
export interface NonEmptyChunk<Success> extends Chunk<Success> {}

export declare const empty: <Success = never>() => Chunk<Success>;
export declare const make: <Values extends readonly [unknown, ...ReadonlyArray<unknown>]>(...values: Values) => NonEmptyChunk<Values[number]>;
export declare const of: <Success>(value: Success) => NonEmptyChunk<Success>;
export declare const fromIterable: <Success>(values: Iterable<Success>) => Chunk<Success>;
export declare const isChunk: (value: unknown) => value is Chunk<unknown>;
export declare const toArray: <Success>(chunk: Chunk<Success>) => ReadonlyArray<Success>;
export declare const get: {
  <Success>(index: number): (chunk: Chunk<Success>) => import("effect/Option").Option<Success>;
  <Success>(chunk: Chunk<Success>, index: number): import("effect/Option").Option<Success>;
};
export declare const map: {
  <Success, Output>(f: (value: Success) => Output): (chunk: Chunk<Success>) => Chunk<Output>;
  <Success, Output>(chunk: Chunk<Success>, f: (value: Success) => Output): Chunk<Output>;
};
