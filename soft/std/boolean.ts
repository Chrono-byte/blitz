/**
 * Standard library: Boolean primitive.
 * Written in TypeScript, runs on the Rust VM.
 */

declare var Boolean: BooleanConstructor;

interface BooleanConstructor {
  new (value?: any): boolean;
  (value?: any): boolean;
}

declare global {
  interface Boolean {
    valueOf(): boolean;
    toString(): string;
  }
}

const BooleanImpl: BooleanConstructor = function (value?: any): boolean {
  return Boolean(value);
} as any;

if (typeof Boolean !== "undefined") {
  const prototype = Boolean.prototype;
  if (prototype) {
    if (!prototype.valueOf) {
      prototype.valueOf = function (): boolean {
        return Boolean(this as any).valueOf();
      };
    }
    if (!prototype.toString) {
      prototype.toString = function (): string {
        return Boolean(this as any).valueOf() ? "true" : "false";
      };
    }
  }
}

void BooleanImpl;
