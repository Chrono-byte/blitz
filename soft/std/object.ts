/**
 * Standard library: Object helpers.
 * Written in TypeScript, runs on the Rust VM.
 */

import * as host from "../host.js";

// const Object: any = typeof globalThis !== "undefined" && (globalThis as any).Object ? (globalThis as any).Object : {};

// Debug: report Object runtime value
try {
  host.debugValue("std/object global Object", Object);
} catch (_) {}

interface ObjectConstructor {
  keys(o: any): string[];
  values(o: any): any[];
  entries(o: any): [string, any][];
  assign(target: any, ...sources: any[]): any;
  create(o: any, properties?: PropertyDescriptorMap): any;
  defineProperty(o: any, p: string | symbol, attributes: PropertyDescriptor): any;
  defineProperties(o: any, properties: PropertyDescriptorMap): any;
  getOwnPropertyDescriptor(o: any, p: string | symbol): PropertyDescriptor | undefined;
  getOwnPropertyDescriptors(o: any): PropertyDescriptorMap;
  getOwnPropertyNames(o: any): string[];
  getOwnPropertySymbols(o: any): symbol[];
  getPrototypeOf(o: any): any;
  setPrototypeOf(o: any, proto: any): any;
  is(value1: any, value2: any): boolean;
  freeze(o: any): any;
  seal(o: any): any;
  preventExtensions(o: any): any;
  isFrozen(o: any): boolean;
  isSealed(o: any): boolean;
  isExtensible(o: any): boolean;
}

// Basic Object implementation
const ObjectImpl: ObjectConstructor = {
  keys(o: any): string[] {
    const result: string[] = [];
    const ownKeys = Object.keys(o);
    let i = 0;
    while (i < ownKeys.length) {
      result.push(ownKeys[i]);
      i = i + 1;
    }
    return result;
  },

  values(o: any): any[] {
    const result: any[] = [];
    const ownKeys = Object.keys(o);
    let i = 0;
    while (i < ownKeys.length) {
      result.push(o[ownKeys[i]]);
      i = i + 1;
    }
    return result;
  },

  entries(o: any): [string, any][] {
    const result: [string, any][] = [];
    const ownKeys = Object.keys(o);
    let i = 0;
    while (i < ownKeys.length) {
      const key = ownKeys[i];
      result.push([key, o[key]]);
      i = i + 1;
    }
    return result;
  },

  assign(target: any, ...sources: any[]): any {
    if (target === null || target === undefined) {
      throw new TypeError("Cannot convert undefined or null to object");
    }

    const to = Object(target);

    let si = 0;
    while (si < sources.length) {
      const source = sources[si];
      if (source !== null && source !== undefined) {
        const keys = Object.keys(source);
        let ki = 0;
        while (ki < keys.length) {
          const key = keys[ki];
          to[key] = source[key];
          ki = ki + 1;
        }
      }
      si = si + 1;
    }

    return to;
  },

  create(o: any, properties?: PropertyDescriptorMap): any {
    if (o !== null && typeof o !== "object") {
      throw new TypeError("Object prototype may only be an Object or null");
    }

    function F() {}
    F.prototype = o;
    const obj = new F();

    if (properties !== undefined) {
      ObjectImpl.defineProperties(obj, properties);
    }

    return obj;
  },

  defineProperty(o: any, p: string | symbol, attributes: PropertyDescriptor): any {
    // Basic implementation - full property descriptors would need more work
    o[p] = attributes.value;
    return o;
  },

  defineProperties(o: any, properties: PropertyDescriptorMap): any {
    const keys = Object.keys(properties as any);
    let i = 0;
    while (i < keys.length) {
      const key = keys[i];
      ObjectImpl.defineProperty(o, key, (properties as any)[key]);
      i = i + 1;
    }
    return o;
  },

  getOwnPropertyDescriptor(o: any, p: string | symbol): PropertyDescriptor | undefined {
    // Basic implementation
    if (Object.prototype.hasOwnProperty.call(o, p)) {
      return {
        value: o[p],
        writable: true,
        enumerable: true,
        configurable: true,
      };
    }
    return undefined;
  },

  getOwnPropertyDescriptors(o: any): PropertyDescriptorMap {
    const result: PropertyDescriptorMap = {};
    const keys = Object.keys(o);
    let i = 0;
    while (i < keys.length) {
      const key = keys[i];
      const descriptor = ObjectImpl.getOwnPropertyDescriptor(o, key);
      if (descriptor) {
        result[key] = descriptor;
      }
      i = i + 1;
    }
    return result;
  },

  getOwnPropertyNames(o: any): string[] {
    return ObjectImpl.keys(o);
  },

  getOwnPropertySymbols(o: any): symbol[] {
    // Basic implementation - symbols not fully supported
    return [];
  },

  getPrototypeOf(o: any): any {
    // Basic implementation
    return o.__proto__ || Object.prototype;
  },

  setPrototypeOf(o: any, proto: any): any {
    // Basic implementation
    o.__proto__ = proto;
    return o;
  },

  is(value1: any, value2: any): boolean {
    // SameValue algorithm
    if (value1 === value2) {
      // Handle +0 !== -0
      return value1 !== 0 || 1 / value1 === 1 / value2;
    } else {
      // Handle NaN === NaN
      return value1 !== value1 && value2 !== value2;
    }
  },

  freeze(o: any): any {
    return o; // Basic implementation
  },

  seal(o: any): any {
    return o; // Basic implementation
  },

  preventExtensions(o: any): any {
    return o; // Basic implementation
  },

  isFrozen(o: any): boolean {
    return false; // Basic implementation
  },

  isSealed(o: any): boolean {
    return false; // Basic implementation
  },

  isExtensible(o: any): boolean {
    return true; // Basic implementation
  },
};

// Assign to global Object
(Object as any) = ObjectImpl;
