export interface InspectOptions {
  showHidden?: boolean;
  depth?: number | null;
  colors?: boolean;
  customInspect?: boolean;
  showProxy?: boolean;
  maxArrayLength?: number | null;
  maxStringLength?: number | null;
  breakLength?: number;
  compact?: boolean | number;
  sorted?: boolean | ((a: string, b: string) => number);
  getters?: boolean | "get" | "set";
  numericSeparator?: boolean;
}

export interface InspectOptionsStylized extends InspectOptions {
  stylize(text: string, styleType: string): string;
}

export type CustomInspectFunction = (depth: number, options: InspectOptionsStylized, inspect: InspectFunction) => unknown;
export type DebugLogger = (message: string, ...args: unknown[]) => void;

export interface InspectFunction {
  (value: unknown, options?: InspectOptions | number): string;
  readonly custom: unique symbol;
  defaultOptions: InspectOptions;
}

export interface CallbackifiedFunction {
  (...args: unknown[]): void;
}

export interface PromisifiedFunction {
  (...args: unknown[]): Promise<unknown>;
}

export interface UtilModule {
  readonly format: typeof format;
  readonly formatWithOptions: typeof formatWithOptions;
  readonly inspect: InspectFunction;
  readonly inherits: typeof inherits;
  readonly callbackify: typeof callbackify;
  readonly promisify: typeof promisify;
  readonly deprecate: typeof deprecate;
  readonly debuglog: typeof debuglog;
  readonly debug: typeof debug;
  readonly stripVTControlCharacters: typeof stripVTControlCharacters;
  readonly types: typeof types;
}

export declare function format(value: unknown, ...args: unknown[]): string;
export declare function formatWithOptions(options: InspectOptions, value: unknown, ...args: unknown[]): string;
export declare const inspect: InspectFunction;
export declare function inherits(constructor: Function, superConstructor: Function): Function;
export declare function callbackify<T extends (...args: any[]) => Promise<unknown>>(fn: T): CallbackifiedFunction;
export declare namespace callbackify {
  const custom: unique symbol;
}
export declare function promisify<T extends (...args: any[]) => any>(fn: T): PromisifiedFunction;
export declare namespace promisify {
  const custom: unique symbol;
}
export declare function deprecate<T extends Function>(fn: T, message: string, code?: string): T;
export declare function debuglog(section: string, callback?: (logger: DebugLogger) => void): DebugLogger;
export declare const debug: typeof debuglog;
export declare function stripVTControlCharacters(value: string): string;

import type { types } from "node:util/types";
export { types };

declare const util: UtilModule;
export { util as default, util };
