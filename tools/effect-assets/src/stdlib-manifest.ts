export type StdlibVisibility = "public" | "private";

export type HostPrimitiveInput = {
  readonly name: string;
  readonly abiVersion: number;
  readonly methods: readonly string[];
};

export type StdlibInput = {
  readonly specifier: string;
  readonly source: string;
  readonly declaration?: string;
  readonly visibility: StdlibVisibility;
  readonly hostPrimitives: readonly HostPrimitiveInput[];
};

/**
 * The source-backed Blitz modules currently implemented by blitz-runtime.
 * Keep this list data-only: adding an entry is an explicit stdlib contract
 * change and its source must already exist.
 */
export const STDLIB_INPUTS = [
  {
    specifier: "blitz:app",
    source: "facades/blitz-app.js",
    declaration: "blitz-app-runtime.d.ts",
    visibility: "public",
    hostPrimitives: [],
  },
  {
    specifier: "blitz-internal:app-transport",
    source: "facades/blitz-internal-app-transport.js",
    declaration: undefined,
    visibility: "private",
    hostPrimitives: [{
      name: "app-transport",
      abiVersion: 1,
      methods: ["openAppServer", "closeAppServer", "awaitShutdown"],
    }],
  },
  {
    specifier: "blitz:match",
    source: "facades/blitz-match.js",
    declaration: "blitz-match.d.ts",
    visibility: "public",
    hostPrimitives: [],
  },
  {
    specifier: "blitz:fs",
    source: "facades/blitz-fs.js",
    declaration: "blitz-fs.d.ts",
    visibility: "public",
    hostPrimitives: [],
  },
  {
    specifier: "blitz-internal:file-system",
    source: "facades/blitz-internal-file-system.js",
    declaration: undefined,
    visibility: "private",
    hostPrimitives: [{
      name: "file-system",
      abiVersion: 1,
      methods: ["readFile", "readTextFile", "writeFile", "writeTextFile"],
    }],
  },
  {
    specifier: "blitz:http",
    source: "facades/blitz-http.js",
    declaration: "blitz-http.d.ts",
    visibility: "public",
    hostPrimitives: [],
  },
  {
    specifier: "blitz-internal:http",
    source: "facades/blitz-internal-http.js",
    declaration: undefined,
    visibility: "private",
    hostPrimitives: [{
      name: "http",
      abiVersion: 1,
      methods: ["openServer", "closeServer", "awaitShutdown"],
    }],
  },
  {
    specifier: "blitz:websocket",
    source: "facades/blitz-websocket.js",
    declaration: "blitz-websocket.d.ts",
    visibility: "public",
    hostPrimitives: [],
  },
  {
    specifier: "blitz-internal:websocket",
    source: "facades/blitz-internal-websocket.js",
    declaration: undefined,
    visibility: "private",
    hostPrimitives: [{
      name: "websocket",
      abiVersion: 1,
      methods: ["connect", "prepareUpgrade", "accept", "send", "close", "subscribe", "cancel"],
    }],
  },
  {
    specifier: "blitz:terminal",
    source: "facades/blitz-terminal.js",
    declaration: "blitz-terminal.d.ts",
    visibility: "public",
    hostPrimitives: [],
  },
  {
    specifier: "blitz-internal:terminal",
    source: "facades/blitz-internal-terminal.js",
    declaration: undefined,
    visibility: "private",
    hostPrimitives: [{
      name: "terminal",
      abiVersion: 1,
      methods: ["write", "writeLine", "setPrompt", "subscribeLines", "close", "cancel"],
    }],
  },
  {
    specifier: "blitz:runtime",
    source: "facades/blitz-runtime.js",
    declaration: "blitz-runtime.d.ts",
    visibility: "public",
    hostPrimitives: [],
  },
] as const satisfies readonly StdlibInput[];

export const STDLIB_FORMAT_VERSION = 1;
export const STDLIB_VERSION = "1";
export const EFFECT_VERSION = "3.16.7";
export const RUNTIME_ABI_VERSION = 1;
export const HOST_ABI_VERSION = 1;
