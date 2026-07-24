import type * as Chunk from "effect/Chunk";
import type * as Either from "effect/Either";
import type * as FiberId from "effect/FiberId";
import type * as Option from "effect/Option";

export type Cause<Error> = Empty | Fail<Error> | Die | Interrupt | Sequential<Error> | Parallel<Error>;
export interface UnknownException { readonly _tag: "UnknownException"; readonly error: unknown }
export interface Empty { readonly _tag: "Empty" }
export interface Fail<Error> { readonly _tag: "Fail"; readonly error: Error }
export interface Die { readonly _tag: "Die"; readonly defect: unknown }
export interface Interrupt { readonly _tag: "Interrupt"; readonly fiberId: FiberId.FiberId }
export interface Sequential<Error> { readonly _tag: "Sequential"; readonly left: Cause<Error>; readonly right: Cause<Error> }
export interface Parallel<Error> { readonly _tag: "Parallel"; readonly left: Cause<Error>; readonly right: Cause<Error> }

export declare const empty: Cause<never>;
export declare const fail: <Error>(error: Error) => Cause<Error>;
export declare const die: (defect: unknown) => Cause<never>;
export declare const interrupt: (fiberId: FiberId.FiberId) => Cause<never>;
export declare const sequential: <LeftError, RightError>(left: Cause<LeftError>, right: Cause<RightError>) => Cause<LeftError | RightError>;
export declare const parallel: <LeftError, RightError>(left: Cause<LeftError>, right: Cause<RightError>) => Cause<LeftError | RightError>;
export declare const isCause: (value: unknown) => value is Cause<unknown>;
export declare const isEmptyType: <Error>(cause: Cause<Error>) => cause is Empty;
export declare const isFailure: <Error>(cause: Cause<Error>) => boolean;
export declare const isInterrupted: <Error>(cause: Cause<Error>) => boolean;
export declare const failures: <Error>(cause: Cause<Error>) => Chunk.Chunk<Error>;
export declare const failureOption: <Error>(cause: Cause<Error>) => Option.Option<Error>;
export declare const failureOrCause: <Error>(cause: Cause<Error>) => Either.Either<Cause<never>, Error>;
export declare const map: {
  <Error, Output>(f: (error: Error) => Output): (cause: Cause<Error>) => Cause<Output>;
  <Error, Output>(cause: Cause<Error>, f: (error: Error) => Output): Cause<Output>;
};
export declare const squash: <Error>(cause: Cause<Error>) => unknown;
