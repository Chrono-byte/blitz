import * as promises from "node:timers/promises";

/** Blitz timer handles are the numeric IDs returned by the global scheduler. */
export type Timeout = number;
export type Immediate = number;

export function setTimeout<TArgs extends any[]>(
  callback: (...args: TArgs) => void,
  delay?: number,
  ...args: TArgs
): Timeout;
export function setInterval<TArgs extends any[]>(
  callback: (...args: TArgs) => void,
  delay?: number,
  ...args: TArgs
): Timeout;
export function setImmediate<TArgs extends any[]>(
  callback: (...args: TArgs) => void,
  ...args: TArgs
): Immediate;

export function clearTimeout(timeout: Timeout | string | number | undefined): void;
export function clearInterval(timeout: Timeout | string | number | undefined): void;
export function clearImmediate(immediate: Immediate | string | number | undefined): void;

export { promises };

declare const timers: {
  readonly clearImmediate: typeof clearImmediate;
  readonly clearInterval: typeof clearInterval;
  readonly clearTimeout: typeof clearTimeout;
  readonly promises: typeof promises;
  readonly setImmediate: typeof setImmediate;
  readonly setInterval: typeof setInterval;
  readonly setTimeout: typeof setTimeout;
};

export { timers as default };
