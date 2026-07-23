export type Either<Right, Left = never> = Left<Left, Right> | Right<Left, Right>;
export interface Left<Left, Right> { readonly _tag: "Left"; readonly left: Left }
export interface Right<Left, Right> { readonly _tag: "Right"; readonly right: Right }

export declare const left: <Left>(value: Left) => Either<never, Left>;
export declare const right: <Right>(value: Right) => Either<Right>;
export declare const isEither: (value: unknown) => value is Either<unknown, unknown>;
export declare const isLeft: <Right, Left>(either: Either<Right, Left>) => either is Left<Left, Right>;
export declare const isRight: <Right, Left>(either: Either<Right, Left>) => either is Right<Left, Right>;
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
