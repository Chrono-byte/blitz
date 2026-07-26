export type AssertPredicate =
  | RegExp
  | (new () => object)
  | ((thrown: unknown) => boolean)
  | object
  | Error;

export declare class AssertionError extends Error {
  actual: unknown;
  expected: unknown;
  operator: string;
  generatedMessage: boolean;
  code: "ERR_ASSERTION";
  constructor(options?: {
    message?: string;
    actual?: unknown;
    expected?: unknown;
    operator?: string;
    stackStartFn?: Function;
  });
}

export declare function fail(message?: string | Error): never;
export declare function fail(
  actual: unknown,
  expected: unknown,
  message?: string | Error,
  operator?: string,
  stackStartFn?: Function,
): never;

export declare function ok(value: unknown, message?: string | Error): asserts value;
export declare function equal(actual: unknown, expected: unknown, message?: string | Error): void;
export declare function notEqual(actual: unknown, expected: unknown, message?: string | Error): void;
export declare function strictEqual<T>(actual: unknown, expected: T): asserts actual is T;
export declare function strictEqual<T>(actual: unknown, expected: T, message: string | Error): asserts actual is T;
export declare function notStrictEqual(actual: unknown, expected: unknown, message?: string | Error): void;
export declare function deepEqual(actual: unknown, expected: unknown, message?: string | Error): void;
export declare function notDeepEqual(actual: unknown, expected: unknown, message?: string | Error): void;
export declare function deepStrictEqual<T>(actual: unknown, expected: T): asserts actual is T;
export declare function deepStrictEqual<T>(actual: unknown, expected: T, message: string | Error): asserts actual is T;
export declare function notDeepStrictEqual(actual: unknown, expected: unknown, message?: string | Error): void;

export declare function throws(block: () => unknown, message?: string | Error): void;
export declare function throws(block: () => unknown, error: AssertPredicate, message?: string | Error): void;
export declare function doesNotThrow(block: () => unknown, message?: string | Error): void;
export declare function doesNotThrow(block: () => unknown, error: AssertPredicate, message?: string | Error): void;
export declare function rejects(
  block: (() => Promise<unknown>) | Promise<unknown>,
  message?: string | Error,
): Promise<void>;
export declare function rejects(
  block: (() => Promise<unknown>) | Promise<unknown>,
  error: AssertPredicate,
  message?: string | Error,
): Promise<void>;
export declare function doesNotReject(
  block: (() => Promise<unknown>) | Promise<unknown>,
  message?: string | Error,
): Promise<void>;
export declare function doesNotReject(
  block: (() => Promise<unknown>) | Promise<unknown>,
  error: AssertPredicate,
  message?: string | Error,
): Promise<void>;
export declare function ifError(value: unknown): asserts value is null | undefined;
export declare function match(value: string, regExp: RegExp, message?: string | Error): void;
export declare function doesNotMatch(value: string, regExp: RegExp, message?: string | Error): void;

interface AssertionMethods {
  readonly AssertionError: typeof AssertionError;
  readonly fail: typeof fail;
  readonly throws: typeof throws;
  readonly doesNotThrow: typeof doesNotThrow;
  readonly rejects: typeof rejects;
  readonly doesNotReject: typeof doesNotReject;
  readonly match: typeof match;
  readonly doesNotMatch: typeof doesNotMatch;
}

export interface StrictAssert extends AssertionMethods {
  (value: unknown, message?: string | Error): asserts value;
  readonly ok: typeof ok;
  readonly equal: typeof strictEqual;
  readonly notEqual: typeof notStrictEqual;
  readonly strictEqual: typeof strictEqual;
  readonly notStrictEqual: typeof notStrictEqual;
  readonly deepEqual: typeof deepStrictEqual;
  readonly notDeepEqual: typeof notDeepStrictEqual;
  readonly deepStrictEqual: typeof deepStrictEqual;
  readonly notDeepStrictEqual: typeof notDeepStrictEqual;
  readonly ifError: typeof ifError;
  readonly strict: StrictAssert;
}

export interface Assert extends AssertionMethods {
  (value: unknown, message?: string | Error): asserts value;
  readonly ok: typeof ok;
  readonly equal: typeof equal;
  readonly notEqual: typeof notEqual;
  readonly strictEqual: typeof strictEqual;
  readonly notStrictEqual: typeof notStrictEqual;
  readonly deepEqual: typeof deepEqual;
  readonly notDeepEqual: typeof notDeepEqual;
  readonly deepStrictEqual: typeof deepStrictEqual;
  readonly notDeepStrictEqual: typeof notDeepStrictEqual;
  readonly ifError: typeof ifError;
  readonly strict: StrictAssert;
}

declare const assert: Assert;
export { assert as default };
export declare const strict: StrictAssert;
