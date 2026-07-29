export type Either<RightValue, LeftValue = never> = Left<LeftValue, RightValue> | Right<LeftValue, RightValue>;
export interface Left<out LeftValue, out RightValue> { readonly _tag: "Left"; readonly left: LeftValue }
export interface Right<out LeftValue, out RightValue> { readonly _tag: "Right"; readonly right: RightValue }

export declare const left: <Left>(value: Left) => Either<never, Left>;
export declare const right: <Right>(value: Right) => Either<Right>;
export declare const isEither: (value: unknown) => value is Either<unknown, unknown>;
export declare const isLeft: <RightValue, LeftValue>(either: Either<RightValue, LeftValue>) => either is Left<LeftValue, RightValue>;
export declare const isRight: <RightValue, LeftValue>(either: Either<RightValue, LeftValue>) => either is Right<LeftValue, RightValue>;
export declare const map: {
  <Right, Output>(f: (value: Right) => Output): <Left>(either: Either<Right, Left>) => Either<Output, Left>;
  <Right, Left, Output>(either: Either<Right, Left>, f: (value: Right) => Output): Either<Output, Left>;
};
export declare const mapLeft: {
  <Left, Output>(f: (value: Left) => Output): <Right>(either: Either<Right, Left>) => Either<Right, Output>;
  <Right, Left, Output>(either: Either<Right, Left>, f: (value: Left) => Output): Either<Right, Output>;
};
export declare const match: {
  <Left, Right, Output>(options: { readonly onLeft: (value: Left) => Output; readonly onRight: (value: Right) => Output }): (either: Either<Right, Left>) => Output;
  <Right, Left, Output>(either: Either<Right, Left>, options: { readonly onLeft: (value: Left) => Output; readonly onRight: (value: Right) => Output }): Output;
};
export declare const merge: <Right, Left>(either: Either<Right, Left>) => Right | Left;
