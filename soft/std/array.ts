/**
 * Standard library: Array helpers.
 * Written in TypeScript, runs on the Rust VM.
 */

// Use the runtime's Array global if present. The bundler/parser rejects
// `declare` function forms, so rely on the runtime-supplied `Array` symbol.
// When running under the VM the `Array` global will be provided by the
// kernel bootstrap we added in Rust.
// eslint-disable-next-line @typescript-eslint/no-explicit-any
const Array: any =
  typeof globalThis !== "undefined" && (globalThis as any).Array ? (globalThis as any).Array : {};
import * as host from "../host.js";

const __native_print: any = host.native_print
  ? host.native_print
  : function (..._args: any[]) {
      /* noop */
    };

// Diagnostic trace to help VM-side debugging: print types of Array and its prototype
try {
  __native_print(
    "[std/array] loading, typeof Array=" +
      typeof Array +
      " typeof Array.prototype=" +
      typeof (Array as any).prototype,
  );
} catch (e) {
  // ignore if native print not available yet
}

// Extend Array prototype with helper methods
declare global {
  interface Array<T> {
    map<U>(callbackfn: (value: T, index: number, array: T[]) => U, thisArg?: any): U[];
    filter<S extends T>(
      predicate: (value: T, index: number, array: T[]) => value is S,
      thisArg?: any,
    ): S[];
    filter(predicate: (value: T, index: number, array: T[]) => unknown, thisArg?: any): T[];
    reduce<U>(
      callbackfn: (previousValue: U, currentValue: T, currentIndex: number, array: T[]) => U,
      initialValue: U,
    ): U;
    forEach(callbackfn: (value: T, index: number, array: T[]) => void, thisArg?: any): void;
    find<S extends T>(
      predicate: (value: T, index: number, obj: T[]) => value is S,
      thisArg?: any,
    ): S | undefined;
    find(predicate: (value: T, index: number, obj: T[]) => unknown, thisArg?: any): T | undefined;
    includes(searchElement: T, fromIndex?: number): boolean;
    indexOf(searchElement: T, fromIndex?: number): number;
    push(...items: T[]): number;
    pop(): T | undefined;
    shift(): T | undefined;
    unshift(...items: T[]): number;
    slice(start?: number, end?: number): T[];
    splice(start: number, deleteCount?: number, ...items: T[]): T[];
    join(separator?: string): string;
    reverse(): T[];
    sort(compareFn?: (a: T, b: T) => number): T[];
    length: number;
  }
}

// Basic Array prototype extensions
if (!Array.prototype.map) {
  Array.prototype.map = function <U, T>(
    this: T[],
    callbackfn: (value: T, index: number, array: T[]) => U,
    thisArg?: any,
  ): U[] {
    const result: U[] = [];
    let i = 0;
    while (i < (this as any).length) {
      if (Object.prototype.hasOwnProperty.call(this, String(i))) {
        if (
          callbackfn &&
          (callbackfn as any).call &&
          typeof (callbackfn as any).call === "function"
        ) {
          result[i] = (callbackfn as any).call(thisArg, this[i], i, this);
        } else {
          result[i] = (callbackfn as any)(this[i], i, this);
        }
      }
      i = i + 1;
    }
    return result;
  };
}

if (!Array.prototype.filter) {
  Array.prototype.filter = function <T, S extends T>(
    this: T[],
    predicate: (value: T, index: number, array: T[]) => unknown,
    thisArg?: any,
  ): T[] {
    const result: T[] = [];
    let i = 0;
    while (i < (this as any).length) {
      if (Object.prototype.hasOwnProperty.call(this, String(i))) {
        const value = this[i];
        let ok;
        if (predicate && (predicate as any).call && typeof (predicate as any).call === "function") {
          ok = (predicate as any).call(thisArg, value, i, this);
        } else {
          ok = (predicate as any)(value, i, this);
        }
        if (ok) {
          result.push(value);
        }
      }
      i = i + 1;
    }
    return result;
  };
}

if (!Array.prototype.reduce) {
  Array.prototype.reduce = function <U, T>(
    this: T[],
    callbackfn: (previousValue: U, currentValue: T, currentIndex: number, array: T[]) => U,
    initialValue: U,
  ): U {
    let accumulator = initialValue;
    let startIndex = 0;

    if (arguments.length < 2) {
      accumulator = this[0] as unknown as U;
      startIndex = 1;
    }

    let i = startIndex;
    while (i < (this as any).length) {
      if (Object.prototype.hasOwnProperty.call(this, String(i))) {
        accumulator = callbackfn(accumulator, this[i], i, this);
      }
      i = i + 1;
    }

    return accumulator;
  };
}

if (!Array.prototype.forEach) {
  Array.prototype.forEach = function <T>(
    this: T[],
    callbackfn: (value: T, index: number, array: T[]) => void,
    thisArg?: any,
  ): void {
    let i = 0;
    while (i < (this as any).length) {
      if (Object.prototype.hasOwnProperty.call(this, String(i))) {
        if (
          callbackfn &&
          (callbackfn as any).call &&
          typeof (callbackfn as any).call === "function"
        ) {
          (callbackfn as any).call(thisArg, this[i], i, this);
        } else {
          (callbackfn as any)(this[i], i, this);
        }
      }
      i = i + 1;
    }
  };
}

if (!Array.prototype.includes) {
  Array.prototype.includes = function <T>(
    this: T[],
    searchElement: T,
    fromIndex: number = 0,
  ): boolean {
    const start = fromIndex < 0 ? Math.max(this.length + fromIndex, 0) : fromIndex;
    let i = start;
    while (i < (this as any).length) {
      if (Object.prototype.hasOwnProperty.call(this, String(i)) && this[i] === searchElement) {
        return true;
      }
      i = i + 1;
    }
    return false;
  };
}

if (!Array.prototype.join) {
  Array.prototype.join = function <T>(this: T[], separator?: string): string {
    const sep = separator === undefined ? "," : String(separator);
    let out = "";
    let i = 0;
    while (i < (this as any).length) {
      if (i > 0) {
        out = out + sep;
      }
      const v = this[i];
      if (v === null || v === undefined) {
        out = out + "";
      } else {
        out = out + String(v);
      }
      i = i + 1;
    }
    return out;
  };
}

export {};
