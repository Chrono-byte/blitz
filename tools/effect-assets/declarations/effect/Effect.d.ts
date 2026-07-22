export interface Effect<Success, Error = never, Requirements = never> {
  readonly _Success: (_: never) => Success;
  readonly _Error: (_: never) => Error;
  readonly _Requirements: (_: Requirements) => void;
  readonly pipe: {
    <A>(ab: (self: Effect<Success, Error, Requirements>) => A): A;
    <A, B>(ab: (self: Effect<Success, Error, Requirements>) => A, bc: (value: A) => B): B;
    <A, B, C>(ab: (self: Effect<Success, Error, Requirements>) => A, bc: (value: A) => B, cd: (value: B) => C): C;
  };
}

export interface Fiber<Success, Error = never> {
  readonly id: unknown;
  readonly await: Effect<Success, Error>;
}

export declare const succeed: <Success>(value: Success) => Effect<Success>;
export declare const fail: <Error>(error: Error) => Effect<never, Error>;
export declare const sync: <Success>(evaluate: () => Success) => Effect<Success>;
export declare const suspend: <Success, Error, Requirements>(evaluate: () => Effect<Success, Error, Requirements>) => Effect<Success, Error, Requirements>;
export declare const never: Effect<never>;
export declare const sleep: (duration: number | string) => Effect<void>;

export declare const map: {
  <Success, Output>(f: (value: Success) => Output): <Error, Requirements>(self: Effect<Success, Error, Requirements>) => Effect<Output, Error, Requirements>;
  <Success, Error, Requirements, Output>(self: Effect<Success, Error, Requirements>, f: (value: Success) => Output): Effect<Output, Error, Requirements>;
};

export declare const flatMap: {
  <Success, Output, Error2, Requirements2>(f: (value: Success) => Effect<Output, Error2, Requirements2>): <Error, Requirements>(self: Effect<Success, Error, Requirements>) => Effect<Output, Error | Error2, Requirements | Requirements2>;
  <Success, Error, Requirements, Output, Error2, Requirements2>(self: Effect<Success, Error, Requirements>, f: (value: Success) => Effect<Output, Error2, Requirements2>): Effect<Output, Error | Error2, Requirements | Requirements2>;
};

export declare const catchAll: <Error, Output, Error2, Requirements2>(f: (error: Error) => Effect<Output, Error2, Requirements2>) => <Success, Requirements>(self: Effect<Success, Error, Requirements>) => Effect<Success | Output, Error2, Requirements | Requirements2>;
export declare const acquireRelease: <Resource, AcquireError, AcquireRequirements, ReleaseError, ReleaseRequirements>(acquire: Effect<Resource, AcquireError, AcquireRequirements>, release: (resource: Resource) => Effect<unknown, ReleaseError, ReleaseRequirements>) => Effect<Resource, AcquireError | ReleaseError, AcquireRequirements | ReleaseRequirements | import("effect/Scope").Scope>;
export declare const scoped: <Success, Error, Requirements>(effect: Effect<Success, Error, Requirements | import("effect/Scope").Scope>) => Effect<Success, Error, Exclude<Requirements, import("effect/Scope").Scope>>;
export declare const fork: <Success, Error, Requirements>(effect: Effect<Success, Error, Requirements>) => Effect<Fiber<Success, Error>, never, Requirements>;
export declare const join: <Success, Error>(fiber: Fiber<Success, Error>) => Effect<Success, Error>;
export declare const interrupt: <Success, Error>(fiber: Fiber<Success, Error>) => Effect<void>;
export declare const runFork: <Success, Error>(effect: Effect<Success, Error, never>) => Fiber<Success, Error>;
export declare const runPromise: <Success, Error>(effect: Effect<Success, Error, never>) => Promise<Success>;
export declare const provide: <Provided>(context: import("effect/Context").Context<Provided>) => <Success, Error, Requirements>(effect: Effect<Success, Error, Requirements>) => Effect<Success, Error, Exclude<Requirements, Provided>>;
