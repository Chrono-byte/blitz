/**
 * Standard library: Map collection.
 * Written in TypeScript, runs on the Rust VM.
 */

declare var Map: MapConstructor;

interface MapConstructor {
  new (): Map;
  new (entries?: readonly (readonly [any, any])[]): Map;
  readonly prototype: Map;
}

interface Map<K, V> {
  readonly size: number;
  get(key: K): V | undefined;
  set(key: K, value: V): this;
  has(key: K): boolean;
  delete(key: K): boolean;
  clear(): void;
  forEach(callbackfn: (value: V, key: K, map: this) => void, thisArg?: any): void;
  keys(): IterableIterator<K>;
  values(): IterableIterator<V>;
  entries(): IterableIterator<[K, V]>;
  [Symbol.iterator](): IterableIterator<[K, V]>;
}

const MapImpl: MapConstructor = function <K, V>(this: Map<K, V>, entries?: readonly (readonly [K, V])[]): Map<K, V> {
  if (this instanceof MapImpl === false) {
    return new MapImpl(entries);
  }
  this._map = new Map();
  if (entries !== undefined) {
    let i = 0;
    while (i < entries.length) {
      const entry = entries[i];
      this._map.set(entry[0], entry[1]);
      i = i + 1;
    }
  }
  return this;
} as any;

MapImpl.prototype.get = function <K, V>(this: Map<K, V>, key: K): V | undefined {
  return this._map.get(key);
};

MapImpl.prototype.set = function <K, V>(this: Map<K, V>, key: K, value: V): Map<K, V> {
  this._map.set(key, value);
  return this;
};

MapImpl.prototype.has = function <K, V>(this: Map<K, V>, key: K): boolean {
  return this._map.has(key);
};

MapImpl.prototype.delete = function <K, V>(this: Map<K, V>, key: K): boolean {
  return this._map.delete(key);
};

MapImpl.prototype.clear = function <K, V>(this: Map<K, V>): void {
  this._map.clear();
};

MapImpl.prototype.forEach = function <K, V>(this: Map<K, V>, callbackfn: (value: V, key: K, map: Map<K, V>) => void, thisArg?: any): void {
  this._map.forEach((value: V, key: K) => {
    callbackfn.call(thisArg, value, key, this);
  });
};

MapImpl.prototype.keys = function <K, V>(this: Map<K, V>): IterableIterator<K> {
  return this._map.keys();
};

MapImpl.prototype.values = function <K, V>(this: Map<K, V>): IterableIterator<V> {
  return this._map.values();
};

MapImpl.prototype.entries = function <K, V>(this: Map<K, V>): IterableIterator<[K, V]> {
  return this._map.entries();
};

MapImpl.prototype[Symbol.iterator] = function <K, V>(this: Map<K, V>): IterableIterator<[K, V]> {
  return this._map[Symbol.iterator]();
};

MapImpl.prototype.size = Object.getOwnPropertyDescriptor(MapImpl.prototype, "size")
  ? Object.getOwnPropertyDescriptor(MapImpl.prototype, "size")!.get!.call(this)
  : 0;

(Map as any) = MapImpl;
