/**
 * Object Model: Core types and interfaces for the Blitz runtime.
 * Implements JavaScript object behavior without TypeScript 'any' type.
 */

// Forward-declare the BlitzObject shape used across the soft layer. This
// interface describes the methods the soft-layer expects on host-backed
// or soft implementations. The concrete `BlitzObject` class in
// `core/objects.ts` will implement these methods and merge with this
// declaration at compile time.
export interface BlitzObject {
  getOwnProperty(key: string): PropertyDescriptor | undefined;
  get(key: string): Value;
  set(key: string, value: Value): boolean;
  hasProperty(key: string): boolean;
  deleteProperty(key: string): boolean;
  getOwnPropertyKeys(): string[];
  getEnumerableKeys(): string[];
  getPrototypeOf(): Value;
  setPrototypeOf(proto: Value): boolean;
  isExtensible(): boolean;
  preventExtensions(): void;
  defineProperty(key: string, descriptor: PropertyDescriptor): void;
}

// Value types represent all possible JavaScript values
export type Value =
  | undefined
  | null
  | boolean
  | number
  | string
  | symbol
  | BlitzObject
  | BlitzFunction;

// Property descriptor following JavaScript specification
export interface PropertyDescriptor {
  value?: Value;
  writable?: boolean;
  enumerable?: boolean;
  configurable?: boolean;
  get?(): Value;
  set?(value: Value): void;
}

// Property descriptor map for object definitions
export type PropertyDescriptorMap = { [key: string]: PropertyDescriptor };

// Object internal slots
export interface ObjectInternalSlots {
  Prototype: object | null;
  Extensible: boolean;
  OwnPropertyKeys(): string[];
}

/// Function interface
export interface BlitzFunction {
  Call(thisArg: Value, ...args: Value[]): Value;
  Construct(...args: Value[]): BlitzObject;
  length: number;
  name: string;
  prototype: BlitzObject;
}

// Basic constructors
export interface ObjectConstructor {
  new (value?: Value): BlitzObject;
  (value?: Value): Value;
  keys(o: BlitzObject): string[];
  values(o: BlitzObject): Value[];
  entries(o: BlitzObject): [string, Value][];
  assign(target: BlitzObject, ...sources: BlitzObject[]): BlitzObject;
  create(o: BlitzObject | null, properties?: PropertyDescriptorMap): BlitzObject;
  defineProperty(o: BlitzObject, p: string, attributes: PropertyDescriptor): BlitzObject;
  defineProperties(o: BlitzObject, properties: PropertyDescriptorMap): BlitzObject;
  getOwnPropertyDescriptor(o: BlitzObject, p: string): PropertyDescriptor | undefined;
  getOwnPropertyDescriptors(o: BlitzObject): PropertyDescriptorMap;
  getOwnPropertyNames(o: BlitzObject): string[];
  getPrototypeOf(o: BlitzObject): BlitzObject | null;
  setPrototypeOf(o: BlitzObject, proto: BlitzObject | null): BlitzObject;
  is(x: Value, y: Value): boolean;
  freeze(o: BlitzObject): BlitzObject;
  seal(o: BlitzObject): BlitzObject;
  preventExtensions(o: BlitzObject): BlitzObject;
  isFrozen(o: BlitzObject): boolean;
  isSealed(o: BlitzObject): boolean;
  isExtensible(o: BlitzObject): boolean;
}

export interface ArrayConstructor {
  new (...items: Value[]): Value[];
  (...items: Value[]): Value[];
  isArray(arg: any): arg is Value[];
  from<T>(arrayLike: ArrayLike<T>): T[];
  of<T>(...items: T[]): T[];
}

export interface StringConstructor {
  new (value?: Value): string;
  (value?: Value): string;
  fromCharCode(...codes: number[]): string;
  fromCodePoint(...codePoints: number[]): string;
  raw(template: TemplateStringsArray, ...substitutions: Value[]): string;
}

export interface NumberConstructor {
  new (value?: Value): number;
  (value?: Value): number;
  EPSILON: number;
  MAX_SAFE_INTEGER: number;
  MIN_SAFE_INTEGER: number;
  NaN: number;
  Infinity: number;
  isFinite(number: Value): boolean;
  isInteger(number: Value): boolean;
  isNaN(number: Value): boolean;
  isSafeInteger(number: Value): boolean;
  parseFloat(string: string): number;
  parseInt(string: string, radix?: number): number;
}

export interface BooleanConstructor {
  new (value?: Value): boolean;
  (value?: Value): boolean;
}

export interface FunctionConstructor {
  new (...args: string[]): BlitzFunction;
  (...args: string[]): BlitzFunction;
  prototype: BlitzFunction;
}

export interface SymbolConstructor {
  new (description?: string): symbol;
  (description?: string): symbol;
  for(key: string): symbol;
  keyFor(sym: symbol): string | undefined;
  asyncIterator: symbol;
  hasInstance: symbol;
  isConcatSpreadable: symbol;
  iterator: symbol;
  match: symbol;
  matchAll: symbol;
  replace: symbol;
  search: symbol;
  species: symbol;
  split: symbol;
  toPrimitive: symbol;
  toStringTag: symbol;
  unscopables: symbol;
}

// Console interface
export interface Console {
  log(...args: Value[]): void;
  warn(...args: Value[]): void;
  error(...args: Value[]): void;
  info(...args: Value[]): void;
  debug(...args: Value[]): void;
}

// Global scope interface
export interface GlobalScope {
  // Global values
  undefined: undefined;
  null: null;
  Infinity: number;
  NaN: number;

  // Global constructors
  Object: ObjectConstructor;
  Array: ArrayConstructor;
  String: StringConstructor;
  Number: NumberConstructor;
  Boolean: BooleanConstructor;
  Function: FunctionConstructor;
  Symbol: SymbolConstructor;

  // Global objects
  console: Console;
  Math: Math;
  JSON: JSON;

  // Global functions
  eval(x: string): Value;
  parseInt(string: string, radix?: number): number;
  parseFloat(string: string): number;
  isNaN(number: Value): boolean;
  isFinite(number: Value): boolean;
  encodeURI(uri: string): string;
  encodeURIComponent(uriComponent: string): string;
  decodeURI(encodedURI: string): string;
  decodeURIComponent(encodedURIComponent: string): string;
  setTimeout(handler: BlitzFunction, timeout?: number, ...args: Value[]): number;
  clearTimeout(handle: number): void;
  setInterval(handler: BlitzFunction, timeout?: number, ...args: Value[]): number;
  clearInterval(handle: number): void;
}
