import type * as Cause from "effect/Cause";
import type * as Option from "effect/Option";

export type Exit<SuccessValue, ErrorValue = never> = Success<SuccessValue, ErrorValue> | Failure<SuccessValue, ErrorValue>;
export interface Success<out SuccessValue, out ErrorValue> {
  readonly _tag: "Success";
  readonly _op: "Success";
  readonly value: SuccessValue;
}
export interface Failure<out SuccessValue, out ErrorValue> {
  readonly _tag: "Failure";
  readonly _op: "Failure";
  readonly cause: Cause.Cause<ErrorValue>;
}

export declare const isExit: (value: unknown) => value is Exit<unknown, unknown>;
export declare const isSuccess: <SuccessValue, ErrorValue>(exit: Exit<SuccessValue, ErrorValue>) => exit is Success<SuccessValue, ErrorValue>;
export declare const isFailure: <SuccessValue, ErrorValue>(exit: Exit<SuccessValue, ErrorValue>) => exit is Failure<SuccessValue, ErrorValue>;
export declare const isInterrupted: <Success, Error>(exit: Exit<Success, Error>) => boolean;
export declare const succeed: <Success>(value: Success) => Exit<Success>;
export declare const fail: <Error>(error: Error) => Exit<never, Error>;
export declare const failCause: <Error>(cause: Cause.Cause<Error>) => Exit<never, Error>;
export declare const die: (defect: unknown) => Exit<never>;
export declare const causeOption: <Success, Error>(exit: Exit<Success, Error>) => Option.Option<Cause.Cause<Error>>;
export declare const asVoid: <Success, Error>(exit: Exit<Success, Error>) => Exit<void, Error>;
export declare const map: {
  <Success, Output>(f: (value: Success) => Output): <Error>(exit: Exit<Success, Error>) => Exit<Output, Error>;
  <Success, Error, Output>(exit: Exit<Success, Error>, f: (value: Success) => Output): Exit<Output, Error>;
};
export declare const match: {
  <Success, Error, Output, Output2 = Output>(options: { readonly onFailure: (cause: Cause.Cause<Error>) => Output; readonly onSuccess: (value: Success) => Output2 }): (exit: Exit<Success, Error>) => Output | Output2;
  <Success, Error, Output, Output2 = Output>(exit: Exit<Success, Error>, options: { readonly onFailure: (cause: Cause.Cause<Error>) => Output; readonly onSuccess: (value: Success) => Output2 }): Output | Output2;
};
