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
// Use the host-provided globalThis (we still declare it to satisfy the compiler)
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
/* Ambient constructor shapes (permissive but typed to allow generics) */
interface ObjectConstructorAmbient {
  new (value?: any): any;
  (value?: any): any;
  keys(o: any): string[];
  values(o: any): any[];
  entries(o: any): [string, any][];
  assign(target: any, ...sources: any[]): any;
  create(o: any, properties?: any): any;
  defineProperty(o: any, p: string, attributes: any): any;
  defineProperties(o: any, properties: any): any;
  getOwnPropertyDescriptor(o: any, p: string): any;
  getOwnPropertyDescriptors(o: any): any;
  getOwnPropertyNames(o: any): string[];
  getPrototypeOf(o: any): any;
  setPrototypeOf(o: any, proto: any): any;
  is(x: any, y: any): boolean;
  freeze(o: any): any;
  seal(o: any): any;
  preventExtensions(o: any): any;
  isFrozen(o: any): boolean;
  isSealed(o: any): boolean;
  isExtensible(o: any): boolean;
}
declare var Object: ObjectConstructorAmbient;

interface FunctionConstructorAmbient {
  new (...args: string[]): any;
  (...args: string[]): any;
  prototype: any;
}
declare var Function: FunctionConstructorAmbient;

interface ArrayConstructorAmbient {
  new <T>(...items: T[]): T[];
  <T>(...items: T[]): T[];
  isArray<T>(arg: any): arg is T[];
  from<T>(arrayLike: ArrayLike<T>): T[];
  of<T>(...items: T[]): T[];
  prototype: any;
}
declare var Array: ArrayConstructorAmbient;

interface StringConstructorAmbient {
  new (value?: any): string;
  (value?: any): string;
  fromCharCode(...codes: number[]): string;
  fromCodePoint(...codePoints: number[]): string;
  raw(template: TemplateStringsArray, ...substitutions: any[]): string;
  prototype: any;
}
declare var String: StringConstructorAmbient;

interface NumberConstructorAmbient {
  new (value?: any): number;
  (value?: any): number;
  EPSILON: number;
  MAX_SAFE_INTEGER: number;
  MIN_SAFE_INTEGER: number;
  NaN: number;
  Infinity: number;
  isFinite(number: any): boolean;
  isInteger(number: any): boolean;
  isNaN(number: any): boolean;
  isSafeInteger(number: any): boolean;
  parseFloat(string: string): number;
  parseInt(string: string, radix?: number): number;
  prototype: any;
}
declare var Number: NumberConstructorAmbient;

interface BooleanConstructorAmbient {
  new (value?: any): boolean;
  (value?: any): boolean;
  prototype: any;
}
declare var Boolean: BooleanConstructorAmbient;

interface SymbolConstructorAmbient {
  new (description?: string | number): symbol;
  (description?: string | number): symbol;
  for(key: string): symbol;
  keyFor(sym: symbol): string | undefined;
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
declare var Symbol: SymbolConstructorAmbient;

declare var BigInt: any;

/* Collections and common globals */
declare var Map: any;
declare var Set: any;
declare var WeakMap: any;
declare var WeakSet: any;
declare var Promise: any;
declare var Date: any;
declare var RegExp: any;

/* Global numeric constants */
declare const NaN: number;
declare const Infinity: number;

/* Error constructors (permissive) */
declare var Error: any;
declare var EvalError: any;
declare var RangeError: any;
declare var ReferenceError: any;
declare var SyntaxError: any;
declare var TypeError: any;
declare var AggregateError: any;

/* Utility / helper types often used by code */
type Awaited<T> = any;
interface TemplateStringsArray {
  [n: number]: string;
  length: number;
  raw: string[];
}
interface ArrayLike<T> {
  length: number;
  [n: number]: T;
}
interface RegExpMatchArray extends Array<string> {}

/* Minimal property descriptor shapes used in std code */
type PropertyDescriptor = any;
type PropertyDescriptorMap = { [key: string]: PropertyDescriptor };

/* Generic helpers */
type Record<K extends keyof any, T> = { [P in K]: T };

/* Provide a type for Math/JSON so they can be used as types in code. */
interface Math { [k: string]: any }
interface JSON { [k: string]: any }

/* Minimal global types that appear as type names in code. */
interface Object {}
interface RegExp {
  test?(s: string): boolean;
  exec?(s: string): any;
  toString(): string;
}

/* Runtime singletons */
declare var Math: any;
declare var JSON: any;
declare var console: any;
/* Global helper functions provided by the runtime layer */
declare function toString(v: any): string;
declare function parseInt(s: any, radix?: number): number;
declare function parseFloat(s: any): number;
declare function isNaN(value: any): boolean;
declare function isFinite(value: any): boolean;

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
  [Symbol.iterator](): IterableIterator<T>;
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
