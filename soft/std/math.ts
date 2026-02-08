/**
 * Standard library: Math utilities.
 * Written in TypeScript, runs on the Rust VM.
 */

interface Math {
  readonly E: number;
  readonly LN2: number;
  readonly LN10: number;
  readonly LOG2E: number;
  readonly LOG10E: number;
  readonly PI: number;
  readonly SQRT1_2: number;
  readonly SQRT2: number;
  readonly [Symbol.toStringTag]: string;
  abs(x: number): number;
  acos(x: number): number;
  acosh(x: number): number;
  asin(x: number): number;
  asinh(x: number): number;
  atan(x: number): number;
  atanh(x: number): number;
  atan2(y: number, x: number): number;
  cbrt(x: number): number;
  ceil(x: number): number;
  clz32(x: number): number;
  cos(x: number): number;
  cosh(x: number): number;
  exp(x: number): number;
  expm1(x: number): number;
  floor(x: number): number;
  fround(x: number): number;
  hypot(...values: number[]): number;
  imul(a: number, b: number): number;
  log(x: number): number;
  log1p(x: number): number;
  log2(x: number): number;
  log10(x: number): number;
  max(...values: number[]): number;
  min(...values: number[]): number;
  pow(x: number, y: number): number;
  random(): number;
  round(x: number): number;
  sign(x: number): number;
  sin(x: number): number;
  sinh(x: number): number;
  sqrt(x: number): number;
  tan(x: number): number;
  tanh(x: number): number;
  trunc(x: number): number;
}

const MathImpl: Math = {
  E: Math.E || 2.718281828459045,
  LN2: Math.LN2 || 0.6931471805599453,
  LN10: Math.LN10 || 2.302585092994046,
  LOG2E: Math.LOG2E || 1.4426950408889634,
  LOG10E: Math.LOG10E || 0.4342944819032518,
  PI: Math.PI || 3.141592653589793,
  SQRT1_2: Math.SQRT1_2 || 0.7071067811865476,
  SQRT2: Math.SQRT2 || 1.4142135623730951,
  [Symbol.toStringTag]: "Math",
  abs(x: number): number {
    return Math.abs(x);
  },
  acos(x: number): number {
    return Math.acos(x);
  },
  acosh(x: number): number {
    return Math.acosh(x);
  },
  asin(x: number): number {
    return Math.asin(x);
  },
  asinh(x: number): number {
    return Math.asinh(x);
  },
  atan(x: number): number {
    return Math.atan(x);
  },
  atanh(x: number): number {
    return Math.atanh(x);
  },
  atan2(y: number, x: number): number {
    return Math.atan2(y, x);
  },
  cbrt(x: number): number {
    return Math.cbrt(x);
  },
  ceil(x: number): number {
    return Math.ceil(x);
  },
  clz32(x: number): number {
    return Math.clz32(x);
  },
  cos(x: number): number {
    return Math.cos(x);
  },
  cosh(x: number): number {
    return Math.cosh(x);
  },
  exp(x: number): number {
    return Math.exp(x);
  },
  expm1(x: number): number {
    return Math.expm1(x);
  },
  floor(x: number): number {
    return Math.floor(x);
  },
  fround(x: number): number {
    return Math.fround(x);
  },
  hypot(...values: number[]): number {
    return Math.hypot(...values);
  },
  imul(a: number, b: number): number {
    return Math.imul(a, b);
  },
  log(x: number): number {
    return Math.log(x);
  },
  log1p(x: number): number {
    return Math.log1p(x);
  },
  log2(x: number): number {
    return Math.log2(x);
  },
  log10(x: number): number {
    return Math.log10(x);
  },
  max(...values: number[]): number {
    return Math.max(...values);
  },
  min(...values: number[]): number {
    return Math.min(...values);
  },
  pow(x: number, y: number): number {
    return Math.pow(x, y);
  },
  random(): number {
    return Math.random();
  },
  round(x: number): number {
    return Math.round(x);
  },
  sign(x: number): number {
    return Math.sign(x);
  },
  sin(x: number): number {
    return Math.sin(x);
  },
  sinh(x: number): number {
    return Math.sinh(x);
  },
  sqrt(x: number): number {
    return Math.sqrt(x);
  },
  tan(x: number): number {
    return Math.tan(x);
  },
  tanh(x: number): number {
    return Math.tanh(x);
  },
  trunc(x: number): number {
    return Math.trunc(x);
  },
};

(Math as any) = MathImpl;
