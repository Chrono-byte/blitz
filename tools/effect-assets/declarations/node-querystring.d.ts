export interface StringifyOptions {
  /** The function used to percent-encode keys and values. */
  encodeURIComponent?: ((value: string) => string) | undefined;
}

export interface ParseOptions {
  /** The maximum number of key/value pairs to parse. `0` disables the limit. */
  maxKeys?: number | undefined;
  /** The function used to decode percent-encoded keys and values. */
  decodeURIComponent?: ((value: string) => string) | undefined;
}

export interface ParsedUrlQuery {
  [key: string]: string | string[] | undefined;
}

export interface ParsedUrlQueryInput {
  [key: string]:
    | string
    | number
    | boolean
    | bigint
    | ReadonlyArray<string | number | boolean | bigint>
    | null
    | undefined;
}

export declare function stringify(): string;
export declare function stringify(object: ParsedUrlQueryInput): string;
export declare function stringify(
  object: ParsedUrlQueryInput,
  separator: string | undefined,
  equals: string | undefined,
  options: StringifyOptions | undefined,
): string;

export declare function parse(query: string): ParsedUrlQuery;
export declare function parse(
  query: string,
  separator: string | undefined,
  equals: string | undefined,
  options: ParseOptions | undefined,
): ParsedUrlQuery;

export declare const encode: typeof stringify;
export declare const decode: typeof parse;
export declare function escape(value: string): string;
export declare function unescape(value: string): string;

declare const querystring: {
  stringify: typeof stringify;
  encode: typeof encode;
  parse: typeof parse;
  decode: typeof decode;
  escape: typeof escape;
  unescape: typeof unescape;
};

export { querystring as default };
