export type AssertPredicate =
  | RegExp
  | (new () => object)
  | ((thrown: unknown) => boolean)
  | object
  | Error;

export declare const AssertionError: {
  new (options?: {
    message?: string;
    actual?: unknown;
    expected?: unknown;
    operator?: string;
    stackStartFn?: Function;
  }): Error & {
    actual: unknown;
    expected: unknown;
    operator: string;
    generatedMessage: boolean;
    code: "ERR_ASSERTION";
  };
};
export declare function fail(message?: string | Error): never;
export declare function ok(value: unknown, message?: string | Error): asserts value;
export declare function equal<T>(actual: unknown, expected: T): asserts actual is T;
export declare function equal<T>(actual: unknown, expected: T, message: string | Error): asserts actual is T;
export declare function notEqual(actual: unknown, expected: unknown, message?: string | Error): void;
export declare const strictEqual: typeof equal;
export declare const notStrictEqual: typeof notEqual;
export declare function deepEqual<T>(actual: unknown, expected: T): asserts actual is T;
export declare function deepEqual<T>(actual: unknown, expected: T, message: string | Error): asserts actual is T;
export declare function notDeepEqual(actual: unknown, expected: unknown, message?: string | Error): void;
export declare const deepStrictEqual: typeof deepEqual;
export declare const notDeepStrictEqual: typeof notDeepEqual;
export declare function throws(block: () => unknown, error?: AssertPredicate | string | Error, message?: string | Error): void;
export declare function doesNotThrow(block: () => unknown, error?: AssertPredicate | string | Error, message?: string | Error): void;
export declare function rejects(block: (() => Promise<unknown>) | Promise<unknown>, error?: AssertPredicate | string | Error, message?: string | Error): Promise<void>;
export declare function doesNotReject(block: (() => Promise<unknown>) | Promise<unknown>, error?: AssertPredicate | string | Error, message?: string | Error): Promise<void>;
export declare function ifError(value: unknown): asserts value is null | undefined;
export declare function match(value: string, regExp: RegExp, message?: string | Error): void;
export declare function doesNotMatch(value: string, regExp: RegExp, message?: string | Error): void;

export interface StrictAssert {
  (value: unknown, message?: string | Error): asserts value;
  readonly AssertionError: typeof AssertionError;
  readonly fail: typeof fail;
  readonly ok: typeof ok;
  readonly equal: typeof equal;
  readonly notEqual: typeof notEqual;
  readonly strictEqual: typeof strictEqual;
  readonly notStrictEqual: typeof notStrictEqual;
  readonly deepEqual: typeof deepEqual;
  readonly notDeepEqual: typeof notDeepEqual;
  readonly deepStrictEqual: typeof deepStrictEqual;
  readonly notDeepStrictEqual: typeof notDeepStrictEqual;
  readonly throws: typeof throws;
  readonly doesNotThrow: typeof doesNotThrow;
  readonly rejects: typeof rejects;
  readonly doesNotReject: typeof doesNotReject;
  readonly ifError: typeof ifError;
  readonly match: typeof match;
  readonly doesNotMatch: typeof doesNotMatch;
  readonly strict: StrictAssert;
}

declare const strict: StrictAssert;
export { strict as default, strict };
