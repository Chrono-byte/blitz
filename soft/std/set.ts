/**
 * Standard library: Set collection.
 * Written in TypeScript, runs on the Rust VM.
 */

declare var Set: SetConstructor;

interface SetConstructor {
  new (): Set;
  new (values?: readonly any[]): Set;
  readonly prototype: Set;
}

interface Set<T> {
  readonly size: number;
  add(value: T): this;
  has(value: T): boolean;
  delete(value: T): boolean;
  clear(): void;
  forEach(callbackfn: (value: T, value2: T, set: this) => void, thisArg?: any): void;
  values(): IterableIterator<T>;
  keys(): IterableIterator<T>;
  entries(): IterableIterator<[T, T]>;
  [Symbol.iterator](): IterableIterator<T>;
}

const SetImpl: SetConstructor = function <T>(this: Set<T>, values?: readonly T[]): Set<T> {
  if (this instanceof SetImpl === false) {
    return new SetImpl(values);
  }
  this._set = new Set();
  if (values !== undefined) {
    let i = 0;
    while (i < values.length) {
      this._set.add(values[i]);
      i = i + 1;
    }
  }
  return this;
} as any;

SetImpl.prototype.add = function <T>(this: Set<T>, value: T): Set<T> {
  this._set.add(value);
  return this;
};

SetImpl.prototype.has = function <T>(this: Set<T>, value: T): boolean {
  return this._set.has(value);
};

SetImpl.prototype.delete = function <T>(this: Set<T>, value: T): boolean {
  return this._set.delete(value);
};

SetImpl.prototype.clear = function <T>(this: Set<T>): void {
  this._set.clear();
};

SetImpl.prototype.forEach = function <T>(
  this: Set<T>,
  callbackfn: (value: T, value2: T, set: Set<T>) => void,
  thisArg?: any,
): void {
  this._set.forEach((value: T) => {
    callbackfn.call(thisArg, value, value, this);
  });
};

SetImpl.prototype.values = function <T>(this: Set<T>): IterableIterator<T> {
  return this._set.values();
};

SetImpl.prototype.keys = function <T>(this: Set<T>): IterableIterator<T> {
  return this._set.keys();
};

SetImpl.prototype.entries = function <T>(this: Set<T>): IterableIterator<[T, T]> {
  return this._set.entries();
};

SetImpl.prototype[Symbol.iterator] = function <T>(this: Set<T>): IterableIterator<T> {
  return this._set[Symbol.iterator]();
};

SetImpl.prototype.size = Object.getOwnPropertyDescriptor(SetImpl.prototype, "size")
  ? Object.getOwnPropertyDescriptor(SetImpl.prototype, "size")!.get!.call(this)
  : 0;

(Set as any) = SetImpl;
