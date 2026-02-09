/**
 * Standard library: BigInt primitive.
 * Written in TypeScript, runs on the Rust VM.
 */

declare var BigInt: BigIntConstructor;

interface BigIntConstructor {
  new (value?: any): bigint;
  (value?: any): bigint;
  asIntN(bits: number, int: bigint): bigint;
  asUintN(bits: number, int: bigint): bigint;
}

declare global {
  interface BigInt {
    toString(radix?: number): string;
    valueOf(): bigint;
    constructor: BigIntConstructor;
  }
}

const BigIntImpl: BigIntConstructor = function (value?: any): bigint {
  try {
    if ((this as any) && (this as any).__is_constructor_call) {
      return new BigInt(value);
    }
  } catch (_) {}
  if (value === undefined) return BigInt(0);
  if (typeof value === "bigint") return value;
  if (typeof value === "number") {
    if (!Number.isFinite(value) || !Number.isInteger(value)) {
      throw new RangeError("The number cannot be converted to a BigInt");
    }
    return BigInt(value);
  }
  if (typeof value === "string") {
    const trimmed = value.trim();
    if (trimmed === "") throw new SyntaxError("Cannot convert empty string to BigInt");
    const sign = trimmed[0] === "-" ? BigInt(-1) : BigInt(1);
    const absStr = trimmed[0] === "-" || trimmed[0] === "+" ? trimmed.slice(1) : trimmed;
    if (!/^\d+$/.test(absStr)) {
      throw new SyntaxError("Cannot convert string to BigInt");
    }
    let result = BigInt(0);
    const digits = absStr.split("");
    let i = 0;
    while (i < digits.length) {
      const digit = BigInt(parseInt(digits[i], 10));
      result = result * BigInt(10) + digit;
      i = i + 1;
    }
    return (sign as any) * result;
  }
  if (typeof value === "boolean") {
    return value ? BigInt(1) : BigInt(0);
  }
  throw new TypeError("Cannot convert value to BigInt");
} as any;

BigIntImpl.asIntN = function asIntN(bits: number, int: bigint): bigint {
  if (bits < 0 || bits > 536870912) {
    throw new RangeError("BigInt.asIntN bits must be between 0 and 536,870,912");
  }
  const mask = (BigInt(1) << BigInt(bits)) - BigInt(1);
  let result = int & mask;
  if (result >= BigInt(1) << BigInt(bits - 1)) {
    result = result - (BigInt(1) << BigInt(bits));
  }
  return result;
};

BigIntImpl.asUintN = function (bits: number, int: bigint): bigint {
  if (bits < 0 || bits > 536870912) {
    throw new RangeError("BigInt.asUintN bits must be between 0 and 536,870,912");
  }
  const mask = (BigInt(1) << BigInt(bits)) - BigInt(1);
  return int & mask;
};

if (typeof BigInt !== "undefined") {
  const prototype = BigInt.prototype;
  if (prototype) {
    if (!prototype.toString) {
      prototype.toString = function (radix?: number): string {
        if (radix !== undefined && (radix < 2 || radix > 36)) {
          throw new RangeError("toString() radix argument must be between 2 and 36");
        }
        const n = this.valueOf();
        if (radix === undefined || radix === 10) {
          return n.toString();
        }
        const isNegative = n < BigInt(0);
        const absN = n < BigInt(0) ? -n : n;
        let result = "";
        while (absN > BigInt(0)) {
          const digit = Number(absN % BigInt(radix));
          result =
            (digit < 10 ? String.fromCharCode(48 + digit) : String.fromCharCode(87 + digit)) +
            result;
        }
        return isNegative ? "-" + result : result || "0";
      };
    }
    if (!prototype.valueOf) {
      prototype.valueOf = function (): bigint {
        return this.valueOf();
      };
    }
  }
}

(BigInt as any) = BigIntImpl;
