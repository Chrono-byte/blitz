/**
 * JavaScript Runtime: Type coercion and operations.
 */

import type { Value, BlitzObject, BlitzFunction } from "./types.js";

// Type checking utilities
export function isUndefined(value: Value): value is undefined {
  return value === undefined;
}

export function isNull(value: Value): value is null {
  return value === null;
}

export function isBoolean(value: Value): value is boolean {
  return typeof value === "boolean";
}

export function isNumber(value: Value): value is number {
  return typeof value === "number";
}

export function isString(value: Value): value is string {
  return typeof value === "string";
}

export function isSymbol(value: Value): value is symbol {
  return typeof value === "symbol";
}

export function isObject(value: Value): value is BlitzObject {
  return typeof value === "object" && value !== null;
}

export function isFunction(value: Value): value is BlitzFunction {
  return typeof value === "function";
}

// Type conversion utilities
export function toBoolean(value: Value): boolean {
  const hostFn = (globalThis as any).__native_toBoolean;
  if (typeof hostFn !== "function") {
    throw new Error("Kernel native '__native_toBoolean' not available");
  }
  const res = hostFn(value);
  // Expect kernel to return a `Value::Bool`-like JS boolean or boxed bool.
  if (typeof res === "boolean") return res;
  if (res && typeof res === "object" && typeof (res as any).valueOf === "function") {
    return !!(res as any).valueOf();
  }
  return !!res;
}

export function toNumber(value: Value): number {
  const hostFn = (globalThis as any).__native_toNumber;
  if (typeof hostFn !== "function") {
    throw new Error("Kernel native '__native_toNumber' not available");
  }
  return hostFn(value) as number;
}

export function toString(value: Value): string {
  const hostFn = (globalThis as any).__native_toString;
  if (typeof hostFn !== "function") {
    throw new Error("Kernel native '__native_toString' not available");
  }
  return hostFn(value) as string;
}

export function toObject(value: Value): BlitzObject {
  if (isObject(value)) return value;
  if (isNull(value) || isUndefined(value)) {
    throw new TypeError("Cannot convert null or undefined to object");
  }

  // Would create wrapper objects
  throw new Error("Wrapper objects not implemented");
}

// JavaScript operations
export function abstractEqualityComparison(x: Value, y: Value): boolean {
  const hostFn = (globalThis as any).__native_abstractEquality;
  if (typeof hostFn !== "function") {
    throw new Error("Kernel native '__native_abstractEquality' not available");
  }
  return !!hostFn(x, y);
}

export function strictEqualityComparison(x: Value, y: Value): boolean {
  // SameValue algorithm
  if (typeof x !== typeof y) return false;
  if (x === y) {
    // If both are numbers, handle +0 !== -0 by comparing reciprocals.
    if (isNumber(x) && isNumber(y)) {
      if (x !== 0) return true;
      return 1 / x === 1 / y;
    }
    return true;
  }

  // Handle NaN === NaN using local NaN detection (x !== x).
  return isNumber(x) && x !== x && isNumber(y) && y !== y;
}

// Arithmetic operations
export function add(x: Value, y: Value): Value {
  if (isString(x) || isString(y)) {
    return toString(x) + toString(y);
  }
  return (toNumber(x) as number) + (toNumber(y) as number);
}

export function subtract(x: Value, y: Value): number {
  return toNumber(x) - toNumber(y);
}

export function multiply(x: Value, y: Value): number {
  return toNumber(x) * toNumber(y);
}

export function divide(x: Value, y: Value): number {
  return toNumber(x) / toNumber(y);
}

export function remainder(x: Value, y: Value): number {
  return toNumber(x) % toNumber(y);
}

export function exponentiate(x: Value, y: Value): number {
  return toNumber(x) ** toNumber(y);
}

// Comparison operations
export function lessThan(x: Value, y: Value): boolean {
  if (isString(x) && isString(y)) {
    return x < y;
  }
  return toNumber(x) < toNumber(y);
}

export function lessThanOrEqual(x: Value, y: Value): boolean {
  if (isString(x) && isString(y)) {
    return x <= y;
  }
  return toNumber(x) <= toNumber(y);
}

export function greaterThan(x: Value, y: Value): boolean {
  if (isString(x) && isString(y)) {
    return x > y;
  }
  return toNumber(x) > toNumber(y);
}

export function greaterThanOrEqual(x: Value, y: Value): boolean {
  if (isString(x) && isString(y)) {
    return x >= y;
  }
  return toNumber(x) >= toNumber(y);
}

// Logical operations
export function logicalNot(value: Value): boolean {
  return !toBoolean(value);
}

export function logicalAnd(x: Value, y: Value): Value {
  return toBoolean(x) ? y : x;
}

export function logicalOr(x: Value, y: Value): Value {
  return toBoolean(x) ? x : y;
}

// Instanceof operator
export function instanceOf(value: Value, constructor: BlitzFunction): boolean {
  const hostFn = (globalThis as any).__native_instanceof;
  if (typeof hostFn !== "function") {
    throw new Error("Kernel native '__native_instanceof' not available");
  }
  return !!hostFn(value, constructor as any);
}

// In operator
export function inProperty(property: Value, object: Value): boolean {
  const hostFn = (globalThis as any).__native_inProperty;
  if (typeof hostFn !== "function") {
    throw new Error("Kernel native '__native_inProperty' not available");
  }
  return !!hostFn(property, object);
}

// Typeof operator
export function typeofValue(value: Value): string {
  const hostFn = (globalThis as any).__native_typeof;
  if (typeof hostFn !== "function") {
    throw new Error("Kernel native '__native_typeof' not available");
  }
  return hostFn(value) as string;
}
