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
  try {
    if ((this as any) && (this as any).__is_constructor_call) {
      return new Boolean(value);
    }
  } catch (_) {}
  return Boolean(value);
} as any;

if (typeof Boolean !== "undefined") {
  const prototype = Boolean.prototype;
  if (prototype) {
    if (!prototype.valueOf) {
      prototype.valueOf = function (): boolean {
        return this.valueOf();
      };
    }
    if (!prototype.toString) {
      prototype.toString = function (): string {
        return this.valueOf() ? "true" : "false";
      };
    }
  }
}

(Boolean as any) = BooleanImpl;
