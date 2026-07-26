/** Options that select the platform rules used by file URL conversions. */
export interface URLPathOptions {
  readonly windows?: boolean | undefined;
}

/** The null-prototype options object returned by urlToHttpOptions. */
export interface UrlToHttpOptions {
  readonly protocol: string;
  readonly hostname: string;
  readonly hash: string;
  readonly search: string;
  readonly pathname: string;
  readonly path: string;
  readonly href: string;
  readonly port?: number;
  readonly auth?: string;
}

export interface ParsedUrlQuery {
  readonly [key: string]: string | readonly string[] | undefined;
}

/** The supported legacy Url record returned by parse and resolveObject. */
export interface Url {
  readonly protocol: string | null;
  readonly slashes: boolean | null;
  readonly auth: string | null;
  readonly host: string | null;
  readonly port: string | null;
  readonly hostname: string | null;
  readonly hash: string | null;
  readonly search: string | null;
  readonly query: string | ParsedUrlQuery | null;
  readonly pathname: string | null;
  readonly path: string | null;
  readonly href: string;
}

/** Aliases of the realm's globals. They are undefined when Blitz does not provide that global. */
export declare const URL: typeof globalThis.URL | undefined;
export declare const URLSearchParams: typeof globalThis.URLSearchParams | undefined;

export declare const domainToASCII: (domain: string) => string;
export declare const domainToUnicode: (domain: string) => string;

export declare function fileURLToPath(url: unknown): string;
export declare function pathToFileURL(path: string): globalThis.URL;

export declare function urlToHttpOptions(url: globalThis.URL): UrlToHttpOptions;

export declare function format(url: string | Url | globalThis.URL): string;
export declare function parse(url: string, parseQueryString?: boolean): Url;
export declare function resolve(from: string, to: string): string;
export declare function resolveObject(from: string, to: string): Url;

declare const url: {
  readonly URL: typeof URL;
  readonly URLSearchParams: typeof URLSearchParams;
  readonly domainToASCII: typeof domainToASCII;
  readonly domainToUnicode: typeof domainToUnicode;
  readonly fileURLToPath: typeof fileURLToPath;
  readonly pathToFileURL: typeof pathToFileURL;
  readonly urlToHttpOptions: typeof urlToHttpOptions;
  readonly format: typeof format;
  readonly parse: typeof parse;
  readonly resolve: typeof resolve;
  readonly resolveObject: typeof resolveObject;
};

export { url as default };
