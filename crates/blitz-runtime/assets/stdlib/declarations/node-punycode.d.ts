/**
 * The deprecated Punycode implementation bundled with Node.js 24.
 * Prefer a userland Punycode package for new code.
 */

export declare function decode(input: string): string;
export declare function encode(input: string): string;
export declare function toASCII(input: string): string;
export declare function toUnicode(input: string): string;

export interface UCS2 {
  decode(input: string): number[];
  encode(codePoints: readonly number[]): string;
}

export declare const ucs2: UCS2;
export declare const version: string;

declare const punycode: {
  version: typeof version;
  ucs2: typeof ucs2;
  decode: typeof decode;
  encode: typeof encode;
  toASCII: typeof toASCII;
  toUnicode: typeof toUnicode;
};

export { punycode as default };
