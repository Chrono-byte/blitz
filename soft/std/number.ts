/**
 * Standard library: Number primitive.
 * Written in TypeScript, runs on the Rust VM.
 */

import * as host from "../host.js";

declare var Number: NumberConstructor;

interface NumberConstructor {
  new (value?: any): number;
  (value?: any): number;
  readonly EPSILON: number;
  readonly MAX_SAFE_INTEGER: number;
  readonly MIN_SAFE_INTEGER: number;
  readonly MAX_VALUE: number;
  readonly MIN_VALUE: number;
  readonly NaN: number;
  readonly POSITIVE_INFINITY: number;
  readonly NEGATIVE_INFINITY: number;
  isFinite(number: any): boolean;
  isInteger(number: any): boolean;
  isNaN(number: any): boolean;
  isSafeInteger(number: any): boolean;
  parseFloat(string: string): number;
  parseInt(string: string, radix?: number): number;
}

declare global {
  interface Number {
    toString(radix?: number): string;
    valueOf(): number;
    toFixed(fractionDigits?: number): string;
    toExponential(fractionDigits?: number): string;
    toPrecision(precision?: number): string;
  }
}

const NumberImpl: NumberConstructor = function (value?: any): number {
  try {
    if ((this as any) && (this as any).__is_constructor_call) {
      return new Number(value);
    }
  } catch (_) {}
  const n = Number(value);
  return typeof n === "number" ? n : NaN;
} as any;

NumberImpl.EPSILON = 2.220446049250313e-16;
NumberImpl.MAX_SAFE_INTEGER = 9007199254740991;
NumberImpl.MIN_SAFE_INTEGER = -9007199254740991;
NumberImpl.MAX_VALUE = 1.7976931348623157e308;
NumberImpl.MIN_VALUE = 5e-324;
NumberImpl.NaN = NaN;
NumberImpl.POSITIVE_INFINITY = Infinity;
NumberImpl.NEGATIVE_INFINITY = -Infinity;

NumberImpl.isFinite = function (number: any): boolean {
  return typeof number === "number" && number === number && number !== Infinity && number !== -Infinity;
};

NumberImpl.isInteger = function (number: any): boolean {
  return typeof number === "number" && number === number && !Number.isFinite(number) ? false : Math.floor(number) === number;
};

NumberImpl.isNaN = function (number: any): boolean {
  return typeof number === "number" && number !== number;
};

NumberImpl.isSafeInteger = function (number: any): boolean {
  return typeof number === "number" && Math.abs(number) <= NumberImpl.MAX_SAFE_INTEGER;
};

NumberImpl.parseFloat = function (string: string): number {
  if (typeof string !== "string") {
    string = String(string);
  }
  const trimmed = string.trim();
  if (trimmed === "") return NaN;
  const firstChar = trimmed[0];
  if (firstChar === "+" || firstChar === "-") {
    const rest = trimmed.slice(1);
    if (rest.length === 0 || !/[0-9]/.test(rest[0])) {
      return NaN;
    }
  }
  const parsed = parseFloat(trimmed);
  return isNaN(parsed) ? NaN : parsed;
};

NumberImpl.parseInt = function (string: string, radix?: number): number {
  if (typeof string !== "string") {
    string = String(string);
  }
  const trimmed = string.trim();
  if (trimmed === "") return NaN;
  if (radix === undefined) {
    if (trimmed.startsWith("0x") || trimmed.startsWith("0X")) {
      radix = 16;
    } else {
      radix = 10;
    }
  }
  if (radix < 2 || radix > 36) return NaN;
  if (radix === 16 && (trimmed.startsWith("0x") || trimmed.startsWith("0X"))) {
    return parseInt(trimmed.slice(2), 16);
  }
  const parsed = parseInt(trimmed, radix);
  return isNaN(parsed) ? NaN : parsed;
};

if (typeof Number !== "undefined") {
  const prototype = Number.prototype;
  if (prototype) {
    if (!prototype.toString) {
      prototype.toString = function (radix?: number): string {
        if (radix === undefined) {
          return this.valueOf().toString();
        }
        if (radix < 2 || radix > 36) {
          throw new RangeError("toString() radix argument must be between 2 and 36");
        }
        const n = this.valueOf();
        if (isNaN(n)) return "NaN";
        if (n === Infinity) return "Infinity";
        if (n === -Infinity) return "-Infinity";
        if (n === 0) return "0";
        const isNegative = n < 0;
        const absN = Math.abs(n);
        let result = "";
        while (absN > 0) {
          const digit = Math.floor(absN % radix);
          result = (digit < 10 ? String.fromCharCode(48 + digit) : String.fromCharCode(87 + digit)) + result;
          absN = Math.floor(absN / radix);
        }
        return isNegative ? "-" + result : result;
      };
    }
    if (!prototype.valueOf) {
      prototype.valueOf = function (): number {
        return this.valueOf();
      };
    }
    if (!prototype.toFixed) {
      prototype.toFixed = function (fractionDigits?: number): string {
        const digits = fractionDigits === undefined ? 0 : fractionDigits;
        if (digits < 0 || digits > 100) {
          throw new RangeError("toFixed() digits argument must be between 0 and 100");
        }
        const n = this.valueOf();
        if (isNaN(n)) return "NaN";
        if (n === Infinity) return "Infinity";
        if (n === -Infinity) return "-Infinity";
        const factor = Math.pow(10, digits);
        const rounded = Math.round(n * factor);
        return String(rounded / factor);
      };
    }
    if (!prototype.toExponential) {
      prototype.toExponential = function (fractionDigits?: number): string {
        const n = this.valueOf();
        if (isNaN(n)) return "NaN";
        if (n === Infinity) return "Infinity";
        if (n === -Infinity) return "-Infinity";
        if (fractionDigits !== undefined && (fractionDigits < 0 || fractionDigits > 100)) {
          throw new RangeError("toExponential() fractionDigits argument must be between 0 and 100");
        }
        return n.toExponential(fractionDigits);
      };
    }
    if (!prototype.toPrecision) {
      prototype.toPrecision = function (precision?: number): string {
        const n = this.valueOf();
        if (isNaN(n)) return "NaN";
        if (n === Infinity) return "Infinity";
        if (n === -Infinity) return "-Infinity";
        if (precision !== undefined && (precision < 1 || precision > 100)) {
          throw new RangeError("toPrecision() precision argument must be between 1 and 100");
        }
        return n.toPrecision(precision);
      };
    }
  }
}

(Number as any) = NumberImpl;
