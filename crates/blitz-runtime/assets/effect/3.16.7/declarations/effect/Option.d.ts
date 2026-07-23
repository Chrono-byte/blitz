export type Option<Success> = None<Success> | Some<Success>;
export interface None<Success = never> { readonly _tag: "None" }
export interface Some<Success> { readonly _tag: "Some"; readonly value: Success }

export declare const none: <Success = never>() => Option<Success>;
export declare const some: <Success>(value: Success) => Option<Success>;
export declare const isOption: (value: unknown) => value is Option<unknown>;
export declare const isNone: <Success>(option: Option<Success>) => option is None<Success>;
export declare const isSome: <Success>(option: Option<Success>) => option is Some<Success>;
export declare const fromNullable: <Success>(value: Success | null | undefined) => Option<NonNullable<Success>>;
export declare const getOrElse: {
  <Output>(onNone: () => Output): <Success>(option: Option<Success>) => Success | Output;
  <Success, Output>(option: Option<Success>, onNone: () => Output): Success | Output;
};
export declare const getOrUndefined: <Success>(option: Option<Success>) => Success | undefined;
export declare const map: {
  <Success, Output>(f: (value: Success) => Output): (option: Option<Success>) => Option<Output>;
  <Success, Output>(option: Option<Success>, f: (value: Success) => Output): Option<Output>;
};
export declare const flatMap: {
  <Success, Output>(f: (value: Success) => Option<Output>): (option: Option<Success>) => Option<Output>;
  <Success, Output>(option: Option<Success>, f: (value: Success) => Option<Output>): Option<Output>;
};
export declare const match: {
  <Success, Output, Output2 = Output>(options: { readonly onNone: () => Output; readonly onSome: (value: Success) => Output2 }): (option: Option<Success>) => Output | Output2;
  <Success, Output, Output2 = Output>(option: Option<Success>, options: { readonly onNone: () => Output; readonly onSome: (value: Success) => Output2 }): Output | Output2;
};
