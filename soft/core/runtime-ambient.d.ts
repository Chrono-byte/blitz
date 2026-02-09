/**
 * Minimal ambient runtime declarations for the soft/std build.
 *
 * This file intentionally defines very small, permissive global types
 * (mostly `any`) so the stdlib can be authored against a known set of
 * globals while `noLib` is enabled in TS compilation.
 *
 * Keep this file small and conservative: prefer widening to `any` rather
 * than creating conflicting readonly/strict signatures that the std runtime
 * needs to overwrite.
 */

/* Basic global reference */
declare const globalThis: any;

/* Fundamental global types missing when `noLib` is enabled.
   Provide minimal `Function` and `IArguments` types so the compiler
   doesn't complain about their absence. These are intentionally
   permissive — keep them small to avoid conflicting with the runtime
   implementations in `std/`. */
interface Function {
  apply(thisArg: any, argArray?: any): any;
  call(thisArg: any, ...args: any[]): any;
  bind(thisArg: any, ...args: any[]): any;
}

interface IArguments {
  length: number;
  callee?: Function;
  [index: number]: any;
}

/* Primitive wrapper constructors (permissive) */
declare var Object: any;
declare var Function: any;
declare var Array: any;
declare var String: any;
declare var Number: any;
declare var Boolean: any;
declare var Symbol: any;
declare var BigInt: any;

/* Collections and common globals */
declare var Map: any;
declare var Set: any;
declare var WeakMap: any;
declare var WeakSet: any;
declare var Promise: any;
declare var Date: any;
declare var RegExp: any;

/* Runtime singletons */
declare var Math: any;
declare var JSON: any;
declare var console: any;

/* Timer APIs */
declare function setTimeout(handler: any, timeout?: number, ...args: any[]): number;
declare function clearTimeout(handle?: number): void;
declare function setInterval(handler: any, timeout?: number, ...args: any[]): number;
declare function clearInterval(handle?: number): void;

/* Minimal iterator/iterable shapes used by std modules */
interface IteratorResult<T> {
  value: T;
  done: boolean;
}
interface Iterator<T> {
  next(...args: any[]): IteratorResult<T>;
}
interface Iterable<T> {
  [Symbol.iterator](): Iterator<T>;
}
interface IterableIterator<T> extends Iterator<T>, Iterable<T> {}

/* Minimal ambient container shapes so `declare global { interface X }` merges work. */
interface Array<T> {
  length: number;
  push(...items: T[]): number;
  pop(): T | undefined;
  slice(start?: number, end?: number): T[];
  splice(start: number, deleteCount?: number, ...items: T[]): T[];
  [n: number]: T;
}

interface String {
  length: number;
  toString(): string;
  valueOf(): string;
}

interface Number {
  toString(radix?: number): string;
  valueOf(): number;
}

interface Boolean {
  valueOf(): boolean;
}

/* Map/Set minimal shapes */
interface Map<K = any, V = any> {
  size: number;
  get(key: K): V | undefined;
  set(key: K, value: V): this;
  has(key: K): boolean;
  delete(key: K): boolean;
  clear(): void;
  entries(): IterableIterator<[K, V]>;
  keys(): IterableIterator<K>;
  values(): IterableIterator<V>;
  forEach(callbackfn: (value: V, key: K, map: Map<K, V>) => void, thisArg?: any): void;
  [Symbol.iterator](): IterableIterator<[K, V]>;
}

interface Set<T = any> {
  size: number;
  add(value: T): this;
  has(value: T): boolean;
  delete(value: T): boolean;
  clear(): void;
  entries(): IterableIterator<[T, T]>;
  values(): IterableIterator<T>;
  keys(): IterableIterator<T>;
  forEach(callbackfn: (value: T, value2: T, set: Set<T>) => void, thisArg?: any): void;
  [Symbol.iterator](): IterableIterator<T>;
}

/* Minimal Promise shape */
interface PromiseConstructor {
  new <T>(executor: (resolve: (value?: T | PromiseLike<T>) => void, reject: (reason?: any) => void) => void): Promise<any>;
  resolve<T>(value: T | PromiseLike<T>): Promise<T>;
  reject<T = never>(reason?: any): Promise<T>;
  all<T>(values: readonly (T | PromiseLike<T>)[]): Promise<T[]>;
  race<T>(values: readonly PromiseLike<T>[]): Promise<T>;
  allSettled<T>(values: readonly (T | PromiseLike<T>)[]): Promise<PromiseSettledResult<T>[]>;
  any<T>(values: readonly PromiseLike<T>[]): Promise<T>;
}
interface Promise<T> {
  then<TResult1 = T, TResult2 = never>(
    onfulfilled?: ((value: T) => TResult1 | PromiseLike<TResult1>) | null,
    onrejected?: ((reason: any) => TResult2 | PromiseLike<TResult2>) | null
  ): Promise<TResult1 | TResult2>;
  catch<TResult = never>(onrejected?: ((reason: any) => TResult | PromiseLike<TResult>) | null): Promise<T | TResult>;
  finally(onfinally?: (() => void) | null): Promise<T>;
}

/* Minimal Symbol constructor shape: intentionally permissive so std runtime can overwrite */
interface SymbolConstructor {
  (description?: string | number): symbol;
  new (description?: string | number): symbol;
  for(key: string): symbol;
  keyFor(sym: symbol): string | undefined;

  /* well-known symbols (not readonly here so runtime may assign) */
  iterator: symbol;
  asyncIterator: symbol;
  match: symbol;
  matchAll: symbol;
  replace: symbol;
  search: symbol;
  species: symbol;
  split: symbol;
  toPrimitive: symbol;
  toStringTag: symbol;
  hasInstance: symbol;
  isConcatSpreadable: symbol;
  unscopables: symbol;
}

/* Provide a small global declaration so std code that uses `declare global` will find the symbols/types. */
declare global {
  var Object: any;
  var Array: any;
  var String: any;
  var Number: any;
  var Boolean: any;
  var Symbol: any;
  var BigInt: any;
  var Map: any;
  var Set: any;
  var Promise: any;
  var Date: any;
  var RegExp: any;
  var console: any;
  var Math: any;
  var JSON: any;

  function setTimeout(handler: any, timeout?: number, ...args: any[]): number;
  function clearTimeout(handle?: number): void;
  function setInterval(handler: any, timeout?: number, ...args: any[]): number;
  function clearInterval(handle?: number): void;
}

/* Intentionally do NOT export anything: this file is a global ambient declaration. */