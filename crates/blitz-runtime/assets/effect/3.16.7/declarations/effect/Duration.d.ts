import type * as Option from "effect/Option";

export interface Duration { readonly _tag: "Duration"; readonly millis: number }
export type Unit = "nano" | "nanos" | "micro" | "micros" | "milli" | "millis" | "second" | "seconds" | "minute" | "minutes" | "hour" | "hours" | "day" | "days" | "week" | "weeks";
export type DurationInput = Duration | number | bigint | readonly [seconds: number, nanos: number] | `${number} ${Unit}`;

export declare const decode: (input: DurationInput) => Duration;
export declare const isDuration: (value: unknown) => value is Duration;
export declare const zero: Duration;
export declare const infinity: Duration;
export declare const nanos: (value: bigint) => Duration;
export declare const millis: (value: number) => Duration;
export declare const seconds: (value: number) => Duration;
export declare const toMillis: (input: DurationInput) => number;
export declare const toNanos: (input: DurationInput) => Option.Option<bigint>;
export declare const format: (input: DurationInput) => string;
export declare const sum: {
  (right: DurationInput): (left: DurationInput) => Duration;
  (left: DurationInput, right: DurationInput): Duration;
};
