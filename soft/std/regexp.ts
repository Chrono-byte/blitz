/**
 * Standard library: RegExp primitive.
 * Written in TypeScript, runs on the Rust VM.
 */

declare var RegExp: RegExpConstructor;

interface RegExpConstructor {
  new (pattern: string | RegExp, flags?: string): RegExp;
  (pattern: string | RegExp, flags?: string): RegExp;
  readonly prototype: RegExp;
}

interface RegExp {
  readonly source: string;
  readonly flags: string;
  readonly global: boolean;
  readonly ignoreCase: boolean;
  readonly multiline: boolean;
  readonly dotAll: boolean;
  readonly unicode: boolean;
  readonly sticky: boolean;
  readonly lastIndex: number;
  exec(string: string): RegExpExecArray | null;
  test(string: string): boolean;
  toString(): string;
  [Symbol.match](string: string): RegExpExecArray | null;
  [Symbol.replace](string: string, replaceValue: string | ((...args: any[]) => string)): string;
  [Symbol.search](string: string): number;
  [Symbol.split](string: string, limit?: number): string[];
}

interface RegExpExecArray extends Array<string | undefined> {
  index: number;
  input: string;
  groups?: { [key: string]: string | undefined };
}

const RegExpImpl: RegExpConstructor = function (pattern: string | RegExp, flags?: string): RegExp {
  if (this instanceof RegExpImpl === false) {
    return new RegExpImpl(pattern, flags);
  }
  if (pattern instanceof RegExp) {
    return new RegExp(pattern.source, pattern.flags);
  }
  const patternStr = String(pattern);
  const flagsStr = flags !== undefined ? String(flags) : "";
  const regex = RegExp(patternStr, flagsStr);
  return regex;
} as any;

if (typeof RegExp !== "undefined") {
  const prototype = RegExp.prototype;
  if (prototype) {
    if (!prototype.test) {
      prototype.test = function (string: string): boolean {
        const result = this.exec(string);
        return result !== null;
      };
    }
    if (!prototype.toString) {
      prototype.toString = function (): string {
        return "/" + this.source + "/" + this.flags;
      };
    }
    if (!prototype[Symbol.match]) {
      prototype[Symbol.match] = function (string: string): RegExpExecArray | null {
        if (this.global) {
          const results: RegExpExecArray = [] as any;
          let match: RegExpExecArray | null = null;
          let lastIndex = 0;
          while ((match = this.exec(string)) !== null) {
            results.push(match[0]);
            lastIndex = this.lastIndex;
            if (!this.global) break;
            if (match[0] === "") {
              this.lastIndex = lastIndex + 1;
            }
          }
          return results.length > 0 ? results : null;
        }
        return this.exec(string);
      };
    }
    if (!prototype[Symbol.replace]) {
      prototype[Symbol.replace] = function (string: string, replaceValue: string | ((...args: any[]) => string)): string {
        if (typeof replaceValue === "string") {
          return string.replace(this, replaceValue);
        }
        return string.replace(this, replaceValue as (...args: any[]) => string);
      };
    }
    if (!prototype[Symbol.search]) {
      prototype[Symbol.search] = function (string: string): number {
        const index = string.search(this);
        return index;
      };
    }
    if (!prototype[Symbol.split]) {
      prototype[Symbol.split] = function (string: string, limit?: number): string[] {
        if (typeof limit === "number") {
          return string.split(this, limit);
        }
        return string.split(this);
      };
    }
  }
}

(RegExp as any) = RegExpImpl;
