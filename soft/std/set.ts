/**
 * Standard library: Set collection.
 * Written in TypeScript, runs on the Rust VM.
 */

declare var Set: any;

interface SetConstructor {
  new <T>(): any;
  new <T>(values?: readonly T[] | null): any;
  readonly prototype: any;
}

interface Set<T = any> {
  size: number;
  add(value: T): this;
  has(value: T): boolean;
  delete(value: T): boolean;
  clear(): void;
  forEach(callbackfn: (value: T, value2: T, set: Set<T>) => void, thisArg?: any): void;
  values(): IterableIterator<T>;
  keys(): IterableIterator<T>;
  entries(): IterableIterator<[T, T]>;
  [Symbol.iterator](): IterableIterator<T>;
}

const SetImpl: SetConstructor = function <T>(this: Set<T>, values?: readonly T[]): Set<T> {
  if (this instanceof SetImpl === false) {
    return new SetImpl(values);
  }
  (this as any)._set = new Set();
  if (values !== undefined) {
    let i = 0;
    while (i < values.length) {
    (this as any)._set.add(values[i]);
      i = i + 1;
    }
  }
  return this;
} as any;

SetImpl.prototype.add = function <T>(this: Set<T>, value: T): Set<T> {
  (this as any)._set.add(value);
  return this;
};

SetImpl.prototype.has = function <T>(this: Set<T>, value: T): boolean {
  return (this as any)._set.has(value);
};

SetImpl.prototype.delete = function <T>(this: Set<T>, value: T): boolean {
  return (this as any)._set.delete(value);
};

SetImpl.prototype.clear = function <T>(this: Set<T>): void {
  (this as any)._set.clear();
};

SetImpl.prototype.forEach = function <T>(
  this: Set<T>,
  callbackfn: (value: T, value2: T, set: Set<T>) => void,
  thisArg?: any,
): void {
  (this as any)._set.forEach((value: T) => {
    callbackfn.call(thisArg, value, value, this);
  });
};

SetImpl.prototype.values = function <T>(this: Set<T>): IterableIterator<T> {
  return (this as any)._set.values();
};

SetImpl.prototype.keys = function <T>(this: Set<T>): IterableIterator<T> {
  return (this as any)._set.keys();
};

SetImpl.prototype.entries = function <T>(this: Set<T>): IterableIterator<[T, T]> {
  return (this as any)._set.entries();
};

SetImpl.prototype[Symbol.iterator] = function <T>(this: Set<T>): IterableIterator<T> {
  return (this as any)._set[Symbol.iterator]();
};

SetImpl.prototype.size = Object.getOwnPropertyDescriptor(SetImpl.prototype, "size")
  ? Object.getOwnPropertyDescriptor(SetImpl.prototype, "size")!.get!.call(this)
  : 0;

(Set as any) = SetImpl;
