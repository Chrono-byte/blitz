// SPDX-License-Identifier: MPL-2.0
declare var NaN: number;
declare var Infinity: number;
declare var undefined: undefined;
declare function eval(source: string): any;
declare function parseInt(string: string, radix?: number): number;
declare function parseFloat(string: string): number;
declare function isNaN(number: number): boolean;
declare function isFinite(number: number): boolean;
declare function decodeURI(encodedURI: string): string;
declare function decodeURIComponent(encodedURIComponent: string): string;
declare function encodeURI(uri: string): string;
declare function encodeURIComponent(uriComponent: string): string;

interface Map<K, V> {
  readonly size: number;
  clear(): void;
  delete(key: K): boolean;
  forEach(callbackfn: (value: V, key: K, map: Map<K, V>) => void): void;
  get(key: K): V | undefined;
  has(key: K): boolean;
  set(key: K, value: V): this;
  entries(): IterableIterator<[K, V]>;
  keys(): IterableIterator<K>;
  values(): IterableIterator<V>;
  [Symbol.iterator](): IterableIterator<[K, V]>;
}
interface MapConstructor {
  new (): Map<any, any>;
  new <K, V>(entries?: readonly (readonly [K, V])[] | null): Map<K, V>;
  readonly prototype: Map<any, any>;
}
declare var Map: MapConstructor;

interface Set<T> {
  readonly size: number;
  add(value: T): this;
  clear(): void;
  delete(value: T): boolean;
  has(value: T): boolean;
  entries(): IterableIterator<[T, T]>;
  keys(): IterableIterator<T>;
  values(): IterableIterator<T>;
  [Symbol.iterator](): IterableIterator<T>;
}
interface SetConstructor {
  new <T = any>(values?: readonly T[] | null): Set<T>;
  readonly prototype: Set<any>;
}
declare var Set: SetConstructor;

interface WeakMap<K extends object | symbol, V> {
  delete(key: K): boolean;
  get(key: K): V | undefined;
  getOrInsert(key: K, value: V): V;
  getOrInsertComputed(key: K, callback: (key: K) => V): V;
  has(key: K): boolean;
  set(key: K, value: V): this;
}
interface WeakMapConstructor {
  new <K extends object | symbol = object, V = any>(entries?: readonly (readonly [K, V])[] | null): WeakMap<K, V>;
  readonly prototype: WeakMap<object | symbol, any>;
}
declare var WeakMap: WeakMapConstructor;

interface WeakSet<T extends object> {
  add(value: T): this;
  delete(value: T): boolean;
  has(value: T): boolean;
}
interface WeakSetConstructor {
  new <T extends object = object>(values?: readonly T[] | null): WeakSet<T>;
  readonly prototype: WeakSet<object>;
}
declare var WeakSet: WeakSetConstructor;

interface PromiseLike<T> {
  then<TResult1 = T, TResult2 = never>(onfulfilled?: (value: T) => TResult1 | PromiseLike<TResult1>, onrejected?: (reason: any) => TResult2 | PromiseLike<TResult2>): PromiseLike<TResult1 | TResult2>;
}
interface Promise<T> extends PromiseLike<T> {}
interface PromiseConstructor {
  new <T>(executor: (resolve: (value: T | PromiseLike<T>) => void, reject: (reason?: any) => void) => void): Promise<T>;
  resolve<T>(value: T | PromiseLike<T>): Promise<T>;
  reject<T = never>(reason?: any): Promise<T>;
  all<T extends readonly unknown[]>(values: T): Promise<T>;
}
declare var Promise: PromiseConstructor;

interface SymbolConstructor {
  (description?: string | number): symbol;
  readonly iterator: unique symbol;
}
declare var Symbol: SymbolConstructor;

interface ObjectConstructor { new (value?: any): Object; (value?: any): any; keys(value: object): string[]; }
declare var Object: ObjectConstructor;
interface FunctionConstructor { new (...args: string[]): Function; (...args: string[]): Function; }
declare var Function: FunctionConstructor;
interface ArrayConstructor { new <T>(...items: T[]): T[]; <T>(...items: T[]): T[]; isArray(value: any): value is any[]; }
declare var Array: ArrayConstructor;
interface StringConstructor { new (value?: any): String; (value?: any): string; }
declare var String: StringConstructor;
interface NumberConstructor { new (value?: any): Number; (value?: any): number; readonly NaN: number; }
declare var Number: NumberConstructor;
interface BooleanConstructor { new (value?: any): Boolean; (value?: any): boolean; }
declare var Boolean: BooleanConstructor;
interface RegExpConstructor { new (pattern: string | RegExp, flags?: string): RegExp; (pattern: string | RegExp, flags?: string): RegExp; }
declare var RegExp: RegExpConstructor;
interface DateConstructor { new (): Date; new (value: number | string): Date; now(): number; }
declare var Date: DateConstructor;
interface ErrorConstructor { new (message?: string): Error; (message?: string): Error; }
declare var Error: ErrorConstructor;
declare var EvalError: ErrorConstructor;
declare var RangeError: ErrorConstructor;
declare var ReferenceError: ErrorConstructor;
declare var SyntaxError: ErrorConstructor;
declare var TypeError: ErrorConstructor;
declare var URIError: ErrorConstructor;
interface ArrayBufferConstructor { new (byteLength: number): ArrayBuffer; readonly prototype: ArrayBuffer; }
declare var ArrayBuffer: ArrayBufferConstructor;
interface ProxyConstructor { new <T extends object>(target: T, handler: ProxyHandler<T>): T; revocable<T extends object>(target: T, handler: ProxyHandler<T>): { proxy: T; revoke: () => void }; }
declare var Proxy: ProxyConstructor;
declare var JSON: { parse(text: string): any; stringify(value: any): string | undefined };
declare var Math: { readonly PI: number; abs(x: number): number; max(...values: number[]): number; min(...values: number[]): number; random(): number };
declare var Reflect: { get(target: object, propertyKey: PropertyKey, receiver?: any): any; set(target: object, propertyKey: PropertyKey, value: any, receiver?: any): boolean };
