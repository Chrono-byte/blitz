export interface Effect<A, E = never, R = never> {
  readonly _Success: (_: never) => A;
  readonly _Error: (_: never) => E;
  readonly _Requirements: (_: R) => void;
}
export interface Exit<A, E> {
  readonly _tag: "Success" | "Failure";
  readonly value: A;
  readonly error: E;
}
export declare const succeed: <A>(value: A) => Effect<A>;
export declare const acquireRelease: <A, E, R, X, R2>(
  acquire: Effect<A, E, R>,
  release: (resource: A, exit: Exit<unknown, unknown>) => Effect<X, never, R2>,
) => Effect<A, E, R>;
export declare const overloaded: {
  (value: string): number;
  (value: number): string;
};
