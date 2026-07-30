// SPDX-License-Identifier: MPL-2.0
// Declarations for globals installed by denko-engine. Keep this file aligned
// with intrinsics/bootstrap.rs; deliberately absent: Intl and Temporal.

type PropertyKey = string | number | symbol;
interface Iterator<T> { next(...args: any[]): IteratorResult<T>; }
interface IteratorResult<T> { value: T; done: boolean; }
interface Iterable<T> { [Symbol.iterator](): Iterator<T>; }
interface IterableIterator<T> extends Iterator<T>, Iterable<T> {}
interface ArrayLike<T> { readonly length: number; [n: number]: T; }
interface PropertyDescriptor { configurable?: boolean; enumerable?: boolean; value?: any; writable?: boolean; get?: () => any; set?: (value: any) => void; }
interface PropertyDescriptorMap { [key: string]: PropertyDescriptor; }
interface ProxyHandler<T extends object> { [key: string]: any; }

declare var NaN: number;
declare var Infinity: number;
declare var undefined: undefined;
declare var globalThis: any;
declare function eval(source: string): any;
declare function parseInt(string: string, radix?: number): number;
declare function parseFloat(string: string): number;
declare function isNaN(number: number): boolean;
declare function isFinite(number: number): boolean;
declare function decodeURI(encodedURI: string): string;
declare function decodeURIComponent(encodedURIComponent: string): string;
declare function encodeURI(uri: string): string;
declare function encodeURIComponent(uriComponent: string): string;
declare var Atomics: AtomicsNamespace;

interface Object {
  constructor: Function;
  hasOwnProperty(property: PropertyKey): boolean;
  isPrototypeOf(object: any): boolean;
  propertyIsEnumerable(property: PropertyKey): boolean;
  toLocaleString(): string;
  toString(): string;
  valueOf(): Object;
}
interface ObjectConstructor {
  new (value?: any): Object;
  (value?: any): any;
  readonly prototype: Object;
  assign<T extends object, U extends object>(target: T, source: U): T & U;
  create(proto: object | null, properties?: PropertyDescriptorMap): any;
  defineProperties<T extends object>(object: T, properties: PropertyDescriptorMap): T;
  defineProperty<T extends object>(object: T, property: PropertyKey, attributes: PropertyDescriptor): T;
  entries<T extends object>(object: T): [string, any][];
  freeze<T>(object: T): T;
  fromEntries(entries: Iterable<readonly [PropertyKey, any]>): any;
  getOwnPropertyDescriptor(object: any, property: PropertyKey): PropertyDescriptor | undefined;
  getOwnPropertyDescriptors(object: any): PropertyDescriptorMap;
  getOwnPropertyNames(object: any): string[];
  getOwnPropertySymbols(object: any): symbol[];
  getPrototypeOf(object: any): object | null;
  groupBy<T>(items: Iterable<T>, callback: (value: T, index: number) => PropertyKey): { [key: string]: T[] };
  hasOwn(object: any, property: PropertyKey): boolean;
  is(value1: any, value2: any): boolean;
  isExtensible(object: any): boolean;
  isFrozen(object: any): boolean;
  isSealed(object: any): boolean;
  keys(object: object): string[];
  preventExtensions<T>(object: T): T;
  seal<T>(object: T): T;
  setPrototypeOf<T>(object: T, proto: object | null): T;
  values<T extends object>(object: T): any[];
}
declare var Object: ObjectConstructor;

interface Function extends Object {
  readonly length: number;
  readonly name: string;
  apply(this: Function, thisArg: any, argArray?: ArrayLike<any> | null): any;
  bind(this: Function, thisArg: any, ...args: any[]): Function;
  call(this: Function, thisArg: any, ...args: any[]): any;
  toString(): string;
}
interface FunctionConstructor { new (...args: string[]): Function; (...args: string[]): Function; readonly prototype: Function; }
declare var Function: FunctionConstructor;

interface Array<T> extends Object, Iterable<T> {
  readonly length: number;
  [n: number]: T;
  at(index: number): T | undefined;
  concat(...items: any[]): T[];
  copyWithin(target: number, start: number, end?: number): this;
  entries(): IterableIterator<[number, T]>;
  every(callback: (value: T, index: number, array: T[]) => unknown): boolean;
  fill(value: T, start?: number, end?: number): this;
  filter(callback: (value: T, index: number, array: T[]) => unknown): T[];
  find(callback: (value: T, index: number, array: T[]) => unknown): T | undefined;
  findIndex(callback: (value: T, index: number, array: T[]) => unknown): number;
  findLast(callback: (value: T, index: number, array: T[]) => unknown): T | undefined;
  findLastIndex(callback: (value: T, index: number, array: T[]) => unknown): number;
  flat(depth?: number): any[];
  flatMap<U>(callback: (value: T, index: number, array: T[]) => U | readonly U[]): U[];
  forEach(callback: (value: T, index: number, array: T[]) => void): void;
  includes(value: T, fromIndex?: number): boolean;
  indexOf(value: T, fromIndex?: number): number;
  join(separator?: string): string;
  keys(): IterableIterator<number>;
  lastIndexOf(value: T, fromIndex?: number): number;
  map<U>(callback: (value: T, index: number, array: T[]) => U): U[];
  pop(): T | undefined;
  push(...items: T[]): number;
  reduce<U>(callback: (previous: U, current: T, index: number, array: T[]) => U, initial: U): U;
  reduceRight<U>(callback: (previous: U, current: T, index: number, array: T[]) => U, initial: U): U;
  reverse(): this;
  shift(): T | undefined;
  slice(start?: number, end?: number): T[];
  some(callback: (value: T, index: number, array: T[]) => unknown): boolean;
  sort(compareFn?: (a: T, b: T) => number): this;
  splice(start: number, deleteCount?: number, ...items: T[]): T[];
  toLocaleString(): string;
  toReversed(): T[];
  toSorted(compareFn?: (a: T, b: T) => number): T[];
  toSpliced(start: number, deleteCount?: number, ...items: T[]): T[];
  unshift(...items: T[]): number;
  values(): IterableIterator<T>;
  with(index: number, value: T): T[];
  [Symbol.iterator](): IterableIterator<T>;
}
interface ArrayConstructor {
  new <T>(...items: T[]): T[];
  <T>(...items: T[]): T[];
  readonly prototype: any[];
  from<T>(iterable: Iterable<T> | ArrayLike<T>, mapfn?: (value: T, index: number) => T): T[];
  fromAsync<T>(iterable: Iterable<T> | ArrayLike<T>): Promise<T[]>;
  isArray(value: any): value is any[];
  of<T>(...items: T[]): T[];
}
declare var Array: ArrayConstructor;

interface String extends Object, Iterable<string> {
  readonly length: number;
  at(index: number): string | undefined; charAt(pos: number): string; charCodeAt(index: number): number; codePointAt(index: number): number | undefined;
  concat(...strings: string[]): string; endsWith(search: string, endPosition?: number): boolean; includes(search: string, position?: number): boolean; indexOf(search: string, position?: number): number;
  isWellFormed(): boolean; lastIndexOf(search: string, position?: number): number; localeCompare(that: string): number; match(regexp: RegExp | string): any; matchAll(regexp: RegExp | string): IterableIterator<any>; normalize(form?: string): string;
  padEnd(maxLength: number, fillString?: string): string; padStart(maxLength: number, fillString?: string): string; repeat(count: number): string; replace(search: string | RegExp, replaceValue: string | ((...args: any[]) => string)): string;
  replaceAll(search: string | RegExp, replaceValue: string | ((...args: any[]) => string)): string; search(regexp: RegExp | string): number; slice(start?: number, end?: number): string; split(separator?: string | RegExp, limit?: number): string[];
  startsWith(search: string, position?: number): boolean; substring(start: number, end?: number): string; toLocaleLowerCase(): string; toLocaleUpperCase(): string; toLowerCase(): string; toString(): string; toUpperCase(): string; toWellFormed(): string; trim(): string; trimEnd(): string; trimStart(): string; valueOf(): string;
  [Symbol.iterator](): IterableIterator<string>;
}
interface StringConstructor { new (value?: any): String; (value?: any): string; readonly prototype: String; fromCharCode(...codes: number[]): string; fromCodePoint(...codePoints: number[]): string; raw(template: { raw: readonly string[] }, ...substitutions: any[]): string; }
declare var String: StringConstructor;

interface Number extends Object { toExponential(fractionDigits?: number): string; toFixed(fractionDigits?: number): string; toLocaleString(): string; toPrecision(precision?: number): string; toString(radix?: number): string; valueOf(): number; }
interface NumberConstructor {
  new (value?: any): Number; (value?: any): number; readonly prototype: Number; readonly EPSILON: number; readonly MAX_SAFE_INTEGER: number; readonly MIN_SAFE_INTEGER: number; readonly MAX_VALUE: number; readonly MIN_VALUE: number; readonly NaN: number; readonly NEGATIVE_INFINITY: number; readonly POSITIVE_INFINITY: number;
  isFinite(number: unknown): boolean; isInteger(number: unknown): boolean; isNaN(number: unknown): boolean; isSafeInteger(number: unknown): boolean; parseFloat(string: string): number; parseInt(string: string, radix?: number): number;
}
declare var Number: NumberConstructor;
interface Boolean extends Object { toString(): string; valueOf(): boolean; }
interface BooleanConstructor { new (value?: any): Boolean; (value?: any): boolean; readonly prototype: Boolean; }
declare var Boolean: BooleanConstructor;

interface Symbol extends Object { readonly description: string | undefined; toString(): string; valueOf(): symbol; }
interface SymbolConstructor {
  (description?: string | number): symbol; readonly prototype: Symbol; for(key: string): symbol; keyFor(sym: symbol): string | undefined;
  readonly asyncIterator: unique symbol; readonly hasInstance: unique symbol; readonly isConcatSpreadable: unique symbol; readonly iterator: unique symbol; readonly match: unique symbol; readonly matchAll: unique symbol; readonly replace: unique symbol; readonly search: unique symbol; readonly species: unique symbol; readonly split: unique symbol; readonly toPrimitive: unique symbol; readonly toStringTag: unique symbol; readonly unscopables: unique symbol;
}
declare var Symbol: SymbolConstructor;

interface RegExp extends Object { readonly dotAll: boolean; readonly flags: string; readonly global: boolean; readonly hasIndices: boolean; readonly ignoreCase: boolean; readonly multiline: boolean; readonly source: string; readonly sticky: boolean; readonly unicode: boolean; readonly unicodeSets: boolean; lastIndex: number; exec(string: string): RegExpExecArray | null; test(string: string): boolean; toString(): string; }
interface RegExpExecArray extends Array<string> { index: number; input: string; groups?: { [key: string]: string }; }
interface RegExpConstructor { new (pattern: string | RegExp, flags?: string): RegExp; (pattern: string | RegExp, flags?: string): RegExp; readonly prototype: RegExp; }
declare var RegExp: RegExpConstructor;

interface Date extends Object { getDate(): number; getDay(): number; getFullYear(): number; getHours(): number; getMilliseconds(): number; getMinutes(): number; getMonth(): number; getSeconds(): number; getTime(): number; getTimezoneOffset(): number; getUTCDate(): number; getUTCDay(): number; getUTCFullYear(): number; getUTCHours(): number; getUTCMilliseconds(): number; getUTCMinutes(): number; getUTCMonth(): number; getUTCSeconds(): number; setTime(time: number): number; toDateString(): string; toISOString(): string; toJSON(key?: any): string | null; toLocaleDateString(): string; toLocaleString(): string; toLocaleTimeString(): string; toString(): string; toTimeString(): string; toUTCString(): string; valueOf(): number; }
interface Date {
  getYear(): number;
  setDate(date: number): number; setFullYear(year: number, month?: number, date?: number): number; setHours(hours: number, minutes?: number, seconds?: number, ms?: number): number; setMilliseconds(ms: number): number; setMinutes(minutes: number, seconds?: number, ms?: number): number; setMonth(month: number, date?: number): number; setSeconds(seconds: number, ms?: number): number;
  setUTCDate(date: number): number; setUTCFullYear(year: number, month?: number, date?: number): number; setUTCHours(hours: number, minutes?: number, seconds?: number, ms?: number): number; setUTCMilliseconds(ms: number): number; setUTCMinutes(minutes: number, seconds?: number, ms?: number): number; setUTCMonth(month: number, date?: number): number; setUTCSeconds(seconds: number, ms?: number): number; setYear(year: number): number;
}
interface DateConstructor { new (): Date; new (value: number | string): Date; new (year: number, month: number, date?: number, hours?: number, minutes?: number, seconds?: number, ms?: number): Date; (): string; readonly prototype: Date; now(): number; parse(value: string): number; UTC(year: number, month: number, date?: number, hours?: number, minutes?: number, seconds?: number, ms?: number): number; }
declare var Date: DateConstructor;

interface ErrorOptions { cause?: unknown; }
interface Error extends Object { name: string; message: string; stack?: string; cause?: unknown; toString(): string; }
interface ErrorConstructor { new (message?: string, options?: ErrorOptions): Error; (message?: string, options?: ErrorOptions): Error; readonly prototype: Error; }
declare var Error: ErrorConstructor;
interface EvalError extends Error {} interface RangeError extends Error {} interface ReferenceError extends Error {} interface SyntaxError extends Error {} interface TypeError extends Error {} interface URIError extends Error {}
interface NativeErrorConstructor { new (message?: string, options?: ErrorOptions): Error; (message?: string, options?: ErrorOptions): Error; readonly prototype: Error; }
declare var EvalError: NativeErrorConstructor; declare var RangeError: NativeErrorConstructor; declare var ReferenceError: NativeErrorConstructor; declare var SyntaxError: NativeErrorConstructor; declare var TypeError: NativeErrorConstructor; declare var URIError: NativeErrorConstructor;
interface AggregateError extends Error { errors: any[]; }
interface AggregateErrorConstructor { new (errors: Iterable<any>, message?: string, options?: ErrorOptions): AggregateError; (errors: Iterable<any>, message?: string, options?: ErrorOptions): AggregateError; readonly prototype: AggregateError; }
declare var AggregateError: AggregateErrorConstructor;

interface PromiseLike<T> { then<TResult1 = T, TResult2 = never>(onfulfilled?: ((value: T) => TResult1 | PromiseLike<TResult1>) | null, onrejected?: ((reason: any) => TResult2 | PromiseLike<TResult2>) | null): PromiseLike<TResult1 | TResult2>; }
interface Promise<T> extends PromiseLike<T> { catch<TResult = never>(onrejected?: ((reason: any) => TResult | PromiseLike<TResult>) | null): Promise<T | TResult>; finally(onfinally?: (() => void | PromiseLike<void>) | null): Promise<T>; }
interface PromiseFulfilledResult<T> { status: "fulfilled"; value: T; }
interface PromiseRejectedResult { status: "rejected"; reason: any; }
type PromiseSettledResult<T> = PromiseFulfilledResult<T> | PromiseRejectedResult;
interface PromiseConstructor {
  new <T>(executor: (resolve: (value: T | PromiseLike<T>) => void, reject: (reason?: any) => void) => void): Promise<T>; readonly prototype: Promise<any>;
  all<T>(values: Iterable<T | PromiseLike<T>>): Promise<T[]>; allSettled<T>(values: Iterable<T | PromiseLike<T>>): Promise<PromiseSettledResult<T>[]>; any<T>(values: Iterable<T | PromiseLike<T>>): Promise<T>; race<T>(values: Iterable<T | PromiseLike<T>>): Promise<T>;
  reject<T = never>(reason?: any): Promise<T>; resolve<T>(value?: T | PromiseLike<T>): Promise<T>; withResolvers<T>(): { promise: Promise<T>; resolve: (value: T | PromiseLike<T>) => void; reject: (reason?: any) => void; };
}
declare var Promise: PromiseConstructor;

interface Map<K, V> extends Iterable<[K, V]> { readonly size: number; clear(): void; delete(key: K): boolean; forEach(callbackfn: (value: V, key: K, map: Map<K, V>) => void): void; get(key: K): V | undefined; has(key: K): boolean; set(key: K, value: V): this; entries(): IterableIterator<[K, V]>; keys(): IterableIterator<K>; values(): IterableIterator<V>; [Symbol.iterator](): IterableIterator<[K, V]>; }
interface MapConstructor { new (): Map<any, any>; new <K, V>(entries?: Iterable<readonly [K, V]> | null): Map<K, V>; readonly prototype: Map<any, any>; groupBy<T, K>(items: Iterable<T>, callback: (value: T, index: number) => K): Map<K, T[]>; }
declare var Map: MapConstructor;
interface Set<T> extends Iterable<T> { readonly size: number; add(value: T): this; clear(): void; delete(value: T): boolean; has(value: T): boolean; entries(): IterableIterator<[T, T]>; keys(): IterableIterator<T>; values(): IterableIterator<T>; [Symbol.iterator](): IterableIterator<T>; }
interface SetConstructor { new <T = any>(values?: Iterable<T> | null): Set<T>; readonly prototype: Set<any>; }
declare var Set: SetConstructor;
interface WeakMap<K extends object | symbol, V> { delete(key: K): boolean; get(key: K): V | undefined; getOrInsert(key: K, value: V): V; getOrInsertComputed(key: K, callback: (key: K) => V): V; has(key: K): boolean; set(key: K, value: V): this; }
interface WeakMapConstructor { new <K extends object | symbol = object, V = any>(entries?: Iterable<readonly [K, V]> | null): WeakMap<K, V>; readonly prototype: WeakMap<object | symbol, any>; }
declare var WeakMap: WeakMapConstructor;
interface WeakSet<T extends object | symbol> { add(value: T): this; delete(value: T): boolean; has(value: T): boolean; }
interface WeakSetConstructor { new <T extends object | symbol = object>(values?: Iterable<T> | null): WeakSet<T>; readonly prototype: WeakSet<object | symbol>; }
declare var WeakSet: WeakSetConstructor;

interface WeakRef<T extends object> { deref(): T | undefined; }
interface WeakRefConstructor { new <T extends object>(target: T): WeakRef<T>; readonly prototype: WeakRef<object>; }
declare var WeakRef: WeakRefConstructor;

interface FinalizationRegistry<T> {
  register(target: object, holdings: T, unregisterToken?: object): void;
  unregister(unregisterToken: object): boolean;
}
interface FinalizationRegistryConstructor {
  new <T>(cleanupCallback: (heldValue: T) => void): FinalizationRegistry<T>;
  readonly prototype: FinalizationRegistry<any>;
}
declare var FinalizationRegistry: FinalizationRegistryConstructor;

interface ArrayBuffer { readonly byteLength: number; slice(begin: number, end?: number): ArrayBuffer; }
interface ArrayBufferConstructor { new (byteLength: number): ArrayBuffer; readonly prototype: ArrayBuffer; isView(value: any): boolean; }
declare var ArrayBuffer: ArrayBufferConstructor;
interface SharedArrayBuffer { readonly byteLength: number; slice(begin: number, end?: number): SharedArrayBuffer; }
interface SharedArrayBufferConstructor { new (byteLength: number): SharedArrayBuffer; readonly prototype: SharedArrayBuffer; }
declare var SharedArrayBuffer: SharedArrayBufferConstructor;
interface DataView { readonly buffer: ArrayBuffer | SharedArrayBuffer; readonly byteLength: number; readonly byteOffset: number; getInt8(byteOffset: number): number; getUint8(byteOffset: number): number; getInt16(byteOffset: number, littleEndian?: boolean): number; getUint16(byteOffset: number, littleEndian?: boolean): number; getInt32(byteOffset: number, littleEndian?: boolean): number; getUint32(byteOffset: number, littleEndian?: boolean): number; getFloat32(byteOffset: number, littleEndian?: boolean): number; getFloat64(byteOffset: number, littleEndian?: boolean): number; getBigInt64(byteOffset: number, littleEndian?: boolean): bigint; getBigUint64(byteOffset: number, littleEndian?: boolean): bigint; setInt8(byteOffset: number, value: number): void; setUint8(byteOffset: number, value: number): void; setInt16(byteOffset: number, value: number, littleEndian?: boolean): void; setUint16(byteOffset: number, value: number, littleEndian?: boolean): void; setInt32(byteOffset: number, value: number, littleEndian?: boolean): void; setUint32(byteOffset: number, value: number, littleEndian?: boolean): void; setFloat32(byteOffset: number, value: number, littleEndian?: boolean): void; setFloat64(byteOffset: number, value: number, littleEndian?: boolean): void; setBigInt64(byteOffset: number, value: bigint, littleEndian?: boolean): void; setBigUint64(byteOffset: number, value: bigint, littleEndian?: boolean): void; }
interface DataViewConstructor { new (buffer: ArrayBuffer | SharedArrayBuffer, byteOffset?: number, byteLength?: number): DataView; readonly prototype: DataView; }
declare var DataView: DataViewConstructor;
interface TypedArray<T extends number | bigint> extends Iterable<T> {
  readonly buffer: ArrayBuffer | SharedArrayBuffer; readonly byteLength: number; readonly byteOffset: number; readonly length: number; [n: number]: T;
  at(index: number): T | undefined; copyWithin(target: number, start: number, end?: number): this; entries(): IterableIterator<[number, T]>;
  every(callback: (value: T, index: number, array: this) => unknown): boolean; fill(value: T, start?: number, end?: number): this;
  filter(callback: (value: T, index: number, array: this) => unknown): this; find(callback: (value: T, index: number, array: this) => unknown): T | undefined;
  findIndex(callback: (value: T, index: number, array: this) => unknown): number; findLast(callback: (value: T, index: number, array: this) => unknown): T | undefined;
  findLastIndex(callback: (value: T, index: number, array: this) => unknown): number; forEach(callback: (value: T, index: number, array: this) => void): void;
  includes(value: T, fromIndex?: number): boolean; indexOf(value: T, fromIndex?: number): number; join(separator?: string): string; keys(): IterableIterator<number>;
  lastIndexOf(value: T, fromIndex?: number): number; map(callback: (value: T, index: number, array: this) => T): this;
  reduce<U>(callback: (previous: U, current: T, index: number, array: this) => U, initial: U): U;
  reduceRight<U>(callback: (previous: U, current: T, index: number, array: this) => U, initial: U): U;
  reverse(): this; set(array: ArrayLike<T>, offset?: number): void; slice(start?: number, end?: number): this; some(callback: (value: T, index: number, array: this) => unknown): boolean;
  sort(compareFn?: (a: T, b: T) => number): this; subarray(begin?: number, end?: number): this; toLocaleString(): string; toReversed(): this; toSorted(compareFn?: (a: T, b: T) => number): this; toString(): string;
  values(): IterableIterator<T>; with(index: number, value: T): this; [Symbol.iterator](): IterableIterator<T>;
}
interface TypedArrayConstructor<T extends number | bigint, A extends TypedArray<T>> { new (length: number): A; new (array: ArrayLike<T> | Iterable<T>): A; new (buffer: ArrayBuffer | SharedArrayBuffer, byteOffset?: number, length?: number): A; readonly prototype: A; readonly BYTES_PER_ELEMENT: number; from(values: Iterable<T> | ArrayLike<T>): A; of(...items: T[]): A; }
interface Uint8Array extends TypedArray<number> {} interface Int8Array extends TypedArray<number> {} interface Uint8ClampedArray extends TypedArray<number> {} interface Uint16Array extends TypedArray<number> {} interface Int16Array extends TypedArray<number> {} interface Uint32Array extends TypedArray<number> {} interface Int32Array extends TypedArray<number> {} interface Float32Array extends TypedArray<number> {} interface Float64Array extends TypedArray<number> {} interface BigInt64Array extends TypedArray<bigint> {} interface BigUint64Array extends TypedArray<bigint> {}
declare var Uint8Array: TypedArrayConstructor<number, Uint8Array>; declare var Int8Array: TypedArrayConstructor<number, Int8Array>; declare var Uint8ClampedArray: TypedArrayConstructor<number, Uint8ClampedArray>; declare var Uint16Array: TypedArrayConstructor<number, Uint16Array>; declare var Int16Array: TypedArrayConstructor<number, Int16Array>; declare var Uint32Array: TypedArrayConstructor<number, Uint32Array>; declare var Int32Array: TypedArrayConstructor<number, Int32Array>; declare var Float32Array: TypedArrayConstructor<number, Float32Array>; declare var Float64Array: TypedArrayConstructor<number, Float64Array>; declare var BigInt64Array: TypedArrayConstructor<bigint, BigInt64Array>; declare var BigUint64Array: TypedArrayConstructor<bigint, BigUint64Array>;

interface BigInt extends Object { toString(radix?: number): string; valueOf(): bigint; }
interface BigIntConstructor { (value: bigint | boolean | number | string): bigint; readonly prototype: BigInt; asIntN(bits: number, value: bigint): bigint; asUintN(bits: number, value: bigint): bigint; }
declare var BigInt: BigIntConstructor;
interface AtomicsNamespace { add(array: any, index: number, value: any): any; and(array: any, index: number, value: any): any; compareExchange(array: any, index: number, expected: any, replacement: any): any; exchange(array: any, index: number, value: any): any; isLockFree(size: number): boolean; load(array: any, index: number): any; notify(array: any, index: number, count?: number): number; or(array: any, index: number, value: any): any; store(array: any, index: number, value: any): any; sub(array: any, index: number, value: any): any; wait(array: any, index: number, value: any, timeout?: number): string; waitAsync(array: any, index: number, value: any, timeout?: number): { async: boolean; value: string | Promise<string> }; xor(array: any, index: number, value: any): any; }

declare var JSON: { parse(text: string, reviver?: (this: any, key: string, value: any, context: { source?: string }) => any): any; stringify(value: any, replacer?: ((this: any, key: string, value: any) => any) | readonly (string | number)[] | null, space?: string | number): string | undefined; };
declare var Math: { readonly E: number; readonly LN10: number; readonly LN2: number; readonly LOG10E: number; readonly LOG2E: number; readonly PI: number; readonly SQRT1_2: number; readonly SQRT2: number; abs(x: number): number; acos(x: number): number; acosh(x: number): number; asin(x: number): number; asinh(x: number): number; atan(x: number): number; atanh(x: number): number; atan2(y: number, x: number): number; cbrt(x: number): number; ceil(x: number): number; clz32(x: number): number; cos(x: number): number; cosh(x: number): number; exp(x: number): number; expm1(x: number): number; floor(x: number): number; fround(x: number): number; hypot(...values: number[]): number; imul(x: number, y: number): number; log(x: number): number; log10(x: number): number; log1p(x: number): number; log2(x: number): number; max(...values: number[]): number; min(...values: number[]): number; pow(x: number, y: number): number; random(): number; round(x: number): number; sign(x: number): number; sin(x: number): number; sinh(x: number): number; sqrt(x: number): number; tan(x: number): number; tanh(x: number): number; trunc(x: number): number; };
declare var Reflect: { apply(target: Function, thisArgument: any, argumentsList: ArrayLike<any>): any; construct(target: Function, argumentsList: ArrayLike<any>, newTarget?: Function): object; defineProperty(target: object, propertyKey: PropertyKey, attributes: PropertyDescriptor): boolean; deleteProperty(target: object, propertyKey: PropertyKey): boolean; get(target: object, propertyKey: PropertyKey, receiver?: any): any; getOwnPropertyDescriptor(target: object, propertyKey: PropertyKey): PropertyDescriptor | undefined; getPrototypeOf(target: object): object | null; has(target: object, propertyKey: PropertyKey): boolean; isExtensible(target: object): boolean; ownKeys(target: object): PropertyKey[]; preventExtensions(target: object): boolean; set(target: object, propertyKey: PropertyKey, value: any, receiver?: any): boolean; setPrototypeOf(target: object, proto: object | null): boolean; };
interface ProxyConstructor { new <T extends object>(target: T, handler: ProxyHandler<T>): T; revocable<T extends object>(target: T, handler: ProxyHandler<T>): { proxy: T; revoke: () => void }; }
declare var Proxy: ProxyConstructor;

interface ShadowRealm { evaluate(sourceText: string): any; importValue(specifier: string, bindingName: string): Promise<any>; }
interface ShadowRealmConstructor { new (): ShadowRealm; readonly prototype: ShadowRealm; }
declare var ShadowRealm: ShadowRealmConstructor;
