/**
 * JavaScript Runtime: Type coercion and operations.
 * Implements JavaScript runtime behavior without TypeScript 'any' type.
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
  if (value === false || value === 0 || value === "" || value === null || value === undefined) {
    return false;
  }
  if (isNumber(value)) {
    return !isNaN(value);
  }
  return true;
}

export function toNumber(value: Value): number {
  if (isNumber(value)) return value;
  if (isBoolean(value)) return value ? 1 : 0;
  if (isNull(value)) return 0;
  if (isUndefined(value)) return NaN;
  if (isString(value)) {
    const trimmed = value.trim();
    if (trimmed === "") return 0;
    const parsed = Number(trimmed);
    return isNaN(parsed) ? NaN : parsed;
  }
  if (isObject(value)) {
    // Would implement object-to-primitive conversion
    return NaN;
  }
  if (isSymbol(value)) {
    throw new TypeError("Cannot convert a Symbol value to a number");
  }
  return NaN;
}

export function toString(value: Value): string {
  if (isString(value)) return value;
  if (isNull(value)) return "null";
  if (isUndefined(value)) return "undefined";
  if (isBoolean(value)) return value ? "true" : "false";
  if (isNumber(value)) {
    if (isNaN(value)) return "NaN";
    if (value === Infinity) return "Infinity";
    if (value === -Infinity) return "-Infinity";
    return String(value);
  }
  if (isSymbol(value)) {
    throw new TypeError("Cannot convert a Symbol value to a string");
  }
  if (isObject(value)) {
    // Would implement object-to-primitive conversion
    return "[object Object]";
  }
  return String(value);
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
  // SameValue algorithm for strict equality
  if (typeof x === typeof y) {
    if (x === y) return true;
    if (isNumber(x) && isNaN(x) && isNumber(y) && isNaN(y)) return true;
    return false;
  }

  // Type coercion
  if (x === null && y === undefined) return true;
  if (x === undefined && y === null) return true;

  if (isNumber(x) && isString(y)) return abstractEqualityComparison(x, toNumber(y));
  if (isString(x) && isNumber(y)) return abstractEqualityComparison(toNumber(x), y);

  if (isBoolean(x)) return abstractEqualityComparison(toNumber(x), y);
  if (isBoolean(y)) return abstractEqualityComparison(x, toNumber(y));

  if ((isString(x) || isNumber(x)) && isObject(y)) {
    // Would implement ToPrimitive
    return abstractEqualityComparison(x, y);
  }

  if (isObject(x) && (isString(y) || isNumber(y))) {
    // Would implement ToPrimitive
    return abstractEqualityComparison(x, y);
  }

  return false;
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

  // Handle NaN === NaN
  return isNumber(x) && isNaN(x) && isNumber(y) && isNaN(y);
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
  if (!isObject(value)) return false;

  let prototype = (constructor as unknown as BlitzObject).get("prototype");
  if (!isObject(prototype)) return false;

  let current = (value as BlitzObject).getPrototypeOf();
  while (current !== null) {
    if (current === prototype) return true;
    current = (current as BlitzObject).getPrototypeOf();
  }

  return false;
}

// In operator
export function inProperty(property: Value, object: Value): boolean {
  if (!isObject(object)) throw new TypeError("in operator expects an object");

  const propertyKey = toString(property);
  return (object as BlitzObject).hasProperty(propertyKey);
}

// Typeof operator
export function typeofValue(value: Value): string {
  if (isNull(value)) return "object";
  if (isFunction(value)) return "function";
  return typeof value;
}
