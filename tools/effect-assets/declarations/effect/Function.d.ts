export type LazyArg<Success> = () => Success;
export type AnyFunction = (...arguments_: never[]) => unknown;

export declare const identity: <Success>(value: Success) => Success;
export declare const constVoid: () => void;
export declare const constTrue: () => true;
export declare const constFalse: () => false;
export declare const isFunction: (value: unknown) => value is Function;
export declare const dual: {
  <DataLast extends AnyFunction, DataFirst extends AnyFunction>(arity: number, body: DataFirst): DataLast & DataFirst;
  <DataLast extends AnyFunction, DataFirst extends AnyFunction>(isDataFirst: (arguments_: ReadonlyArray<unknown>) => boolean, body: DataFirst): DataLast & DataFirst;
};
export declare const pipe: {
  <Value>(value: Value): Value;
  <Value, Output>(value: Value, ab: (value: Value) => Output): Output;
  <Value, A, Output>(value: Value, ab: (value: Value) => A, bc: (value: A) => Output): Output;
  <Value, A, B, Output>(value: Value, ab: (value: Value) => A, bc: (value: A) => B, cd: (value: B) => Output): Output;
};
