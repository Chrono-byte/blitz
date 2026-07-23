interface Pipeable<Self> {
  readonly pipe: {
    <A>(ab: (self: Self) => A): A;
    <A, B>(ab: (self: Self) => A, bc: (value: A) => B): B;
    <A, B, C>(ab: (self: Self) => A, bc: (value: A) => B, cd: (value: B) => C): C;
  };
}

export interface HashSet<Success> extends Iterable<Success>, Pipeable<HashSet<Success>> {}

export declare const isHashSet: {
  <Success>(value: Iterable<Success>): value is HashSet<Success>;
  (value: unknown): value is HashSet<unknown>;
};
export declare const empty: <Success = never>() => HashSet<Success>;
export declare const fromIterable: <Success>(values: Iterable<Success>) => HashSet<Success>;
export declare const make: <Values extends ReadonlyArray<unknown>>(...values: Values) => HashSet<Values[number]>;
export declare const has: {
  <Success>(value: Success): (set: HashSet<Success>) => boolean;
  <Success>(set: HashSet<Success>, value: Success): boolean;
};
