/**
 * Standard library: String helpers.
 * Written in TypeScript, runs on the Rust VM.
 */

declare var String: StringConstructor;

interface StringConstructor {
  new (value?: any): string;
  (value?: any): string;
  fromCharCode(...codes: number[]): string;
  fromCodePoint(...codePoints: number[]): string;
  raw(template: TemplateStringsArray, ...substitutions: any[]): string;
}

// Extend String prototype with helper methods
declare global {
  interface String {
    readonly length: number;
    charAt(pos: number): string;
    charCodeAt(index: number): number;
    concat(...strings: string[]): string;
    includes(searchString: string, position?: number): boolean;
    endsWith(searchString: string, position?: number): boolean;
    startsWith(searchString: string, position?: number): boolean;
    indexOf(searchString: string, position?: number): number;
    lastIndexOf(searchString: string, position?: number): number;
    match(regexp: string | RegExp): RegExpMatchArray | null;
    padEnd(maxLength: number, fillString?: string): string;
    padStart(maxLength: number, fillString?: string): string;
    repeat(count: number): string;
    replace(
      searchValue: string | RegExp,
      replacer: string | ((substring: string, ...args: any[]) => string),
    ): string;
    replaceAll(
      searchValue: string | RegExp,
      replacer: string | ((substring: string, ...args: any[]) => string),
    ): string;
    search(regexp: string | RegExp): number;
    slice(start?: number, end?: number): string;
    split(separator: string | RegExp, limit?: number): string[];
    substring(start: number, end?: number): string;
    toLowerCase(): string;
    toUpperCase(): string;
    trim(): string;
    toString(): string;
    valueOf(): string;
  }
}

// Basic String prototype extensions
if (!String.prototype.includes) {
  String.prototype.includes = function (
    this: string,
    searchString: string,
    position: number = 0,
  ): boolean {
    return this.indexOf(searchString, position) !== -1;
  };
}

if (!String.prototype.startsWith) {
  String.prototype.startsWith = function (
    this: string,
    searchString: string,
    position: number = 0,
  ): boolean {
    return this.substr(position, searchString.length) === searchString;
  };
}

if (!String.prototype.endsWith) {
  String.prototype.endsWith = function (
    this: string,
    searchString: string,
    position?: number,
  ): boolean {
    const end = position !== undefined ? Math.min(position, this.length) : this.length;
    return this.substr(end - searchString.length, searchString.length) === searchString;
  };
}

if (!String.prototype.repeat) {
  String.prototype.repeat = function (this: string, count: number): string {
    if (count < 0 || count === Infinity) {
      throw new RangeError("Invalid count value");
    }
    if (count === 0) return "";
    let result = "";
    let i = 0;
    while (i < count) {
      result = result + this;
      i = i + 1;
    }
    return result;
  };
}

if (!String.prototype.padStart) {
  String.prototype.padStart = function (
    this: string,
    maxLength: number,
    fillString: string = " ",
  ): string {
    if (this.length >= maxLength) return this;
    const fillLength = maxLength - this.length;
    let pad = "";
    let i = 0;
    while (i < fillLength) {
      pad = pad + fillString[i % fillString.length];
      i = i + 1;
    }
    return pad + this;
  };
}

if (!String.prototype.padEnd) {
  String.prototype.padEnd = function (
    this: string,
    maxLength: number,
    fillString: string = " ",
  ): string {
    if (this.length >= maxLength) return this;
    const fillLength = maxLength - this.length;
    let pad = "";
    let i = 0;
    while (i < fillLength) {
      pad = pad + fillString[i % fillString.length];
      i = i + 1;
    }
    return this + pad;
  };
}

// Basic String constructor implementation
const StringImpl: StringConstructor = function (value?: any): string {
  // Avoid `instanceof` since parser subset doesn't support it. Heuristically
  // detect constructor call by checking if `this` has a `__is_constructor_call` flag
  // that the runtime may set; otherwise assume function call form.
  try {
    if ((this as any) && (this as any).__is_constructor_call) {
      return new String(value);
    }
  } catch (_) {}
  return value !== undefined ? String(value) : "";
} as any;

StringImpl.fromCharCode = function (...codes: number[]): string {
  return String.fromCharCode(...codes);
};

StringImpl.fromCodePoint = function (...codePoints: number[]): string {
  return String.fromCodePoint(...codePoints);
};

StringImpl.raw = function (template: TemplateStringsArray, ...substitutions: any[]): string {
  return String.raw(template, ...substitutions);
};

// Assign to global String
(String as any) = StringImpl;
