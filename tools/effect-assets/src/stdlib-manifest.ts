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
    specifier: "blitz-internal:host",
    source: "facades/blitz-internal-host.js",
    declaration: undefined,
    visibility: "private",
    hostPrimitives: [],
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
  {
    specifier: "node:assert",
    source: "facades/node-assert.js",
    declaration: "node-assert.d.ts",
    visibility: "public",
    hostPrimitives: [],
  },
  {
    specifier: "node:assert/strict",
    source: "facades/node-assert-strict.js",
    declaration: "node-assert-strict.d.ts",
    visibility: "public",
    hostPrimitives: [],
  },
  {
    specifier: "node:async_hooks",
    source: "facades/node-async-hooks.js",
    declaration: "node-async-hooks.d.ts",
    visibility: "public",
    hostPrimitives: [],
  },
  {
    specifier: "node:console",
    source: "facades/node-console.js",
    declaration: "node-console.d.ts",
    visibility: "public",
    hostPrimitives: [],
  },
  {
    specifier: "node:constants",
    source: "facades/node-constants.js",
    declaration: "node-constants.d.ts",
    visibility: "public",
    hostPrimitives: [],
  },
  {
    specifier: "blitz-internal:node-crypto-random",
    source: "facades/blitz-internal-node-crypto-random.js",
    declaration: undefined,
    visibility: "private",
    hostPrimitives: [{
      name: "node-crypto-random",
      abiVersion: 1,
      methods: ["randomUUID", "randomBytes", "randomFill", "randomInt", "timingSafeEqual"],
    }],
  },
  {
    specifier: "node:crypto",
    source: "facades/node-crypto.js",
    declaration: "node-crypto.d.ts",
    visibility: "public",
    hostPrimitives: [],
  },
  {
    specifier: "node:diagnostics_channel",
    source: "facades/node-diagnostics-channel.js",
    declaration: "node-diagnostics-channel.d.ts",
    visibility: "public",
    hostPrimitives: [],
  },
  {
    specifier: "node:domain",
    source: "facades/node-domain.js",
    declaration: "node-domain.d.ts",
    visibility: "public",
    hostPrimitives: [],
  },
  {
    specifier: "node:events",
    source: "facades/node-events.js",
    declaration: "node-events.d.ts",
    visibility: "public",
    hostPrimitives: [],
  },
  {
    specifier: "blitz-internal:node-path-shared",
    source: "facades/node-path-shared.js",
    declaration: undefined,
    visibility: "private",
    hostPrimitives: [],
  },
  {
    specifier: "blitz-internal:node-file-system",
    source: "facades/blitz-internal-node-file-system.js",
    declaration: undefined,
    visibility: "private",
    hostPrimitives: [{
      name: "node-file-system",
      abiVersion: 1,
      methods: ["readFile", "writeFile", "stat", "mkdir", "readdir", "rm", "unlink", "rename", "exists"],
    }],
  },
  {
    specifier: "node:fs",
    source: "facades/node-fs.js",
    declaration: "node-fs.d.ts",
    visibility: "public",
    hostPrimitives: [],
  },
  {
    specifier: "node:fs/promises",
    source: "facades/node-fs-promises.js",
    declaration: "node-fs-promises.d.ts",
    visibility: "public",
    hostPrimitives: [],
  },
  {
    specifier: "node:module",
    source: "facades/node-module.js",
    declaration: "node-module.d.ts",
    visibility: "public",
    hostPrimitives: [],
  },
  {
    specifier: "node:os",
    source: "facades/node-os.js",
    declaration: "node-os.d.ts",
    visibility: "public",
    hostPrimitives: [],
  },
  {
    specifier: "node:path",
    source: "facades/node-path.js",
    declaration: "node-path.d.ts",
    visibility: "public",
    hostPrimitives: [],
  },
  {
    specifier: "node:path/posix",
    source: "facades/node-path-posix.js",
    declaration: "node-path-posix.d.ts",
    visibility: "public",
    hostPrimitives: [],
  },
  {
    specifier: "node:path/win32",
    source: "facades/node-path-win32.js",
    declaration: "node-path-win32.d.ts",
    visibility: "public",
    hostPrimitives: [],
  },
  {
    specifier: "node:perf_hooks",
    source: "facades/node-perf-hooks.js",
    declaration: "node-perf-hooks.d.ts",
    visibility: "public",
    hostPrimitives: [],
  },
  {
    specifier: "node:process",
    source: "facades/node-process.js",
    declaration: "node-process.d.ts",
    visibility: "public",
    hostPrimitives: [],
  },
  {
    specifier: "node:punycode",
    source: "facades/node-punycode.js",
    declaration: "node-punycode.d.ts",
    visibility: "public",
    hostPrimitives: [],
  },
  {
    specifier: "node:querystring",
    source: "facades/node-querystring.js",
    declaration: "node-querystring.d.ts",
    visibility: "public",
    hostPrimitives: [],
  },
  {
    specifier: "node:readline",
    source: "facades/node-readline.js",
    declaration: "node-readline.d.ts",
    visibility: "public",
    hostPrimitives: [],
  },
  {
    specifier: "node:readline/promises",
    source: "facades/node-readline-promises.js",
    declaration: "node-readline-promises.d.ts",
    visibility: "public",
    hostPrimitives: [],
  },
  {
    specifier: "node:sea",
    source: "facades/node-sea.js",
    declaration: "node-sea.d.ts",
    visibility: "public",
    hostPrimitives: [],
  },
  {
    specifier: "node:stream",
    source: "facades/node-stream.js",
    declaration: "node-stream.d.ts",
    visibility: "public",
    hostPrimitives: [],
  },
  {
    specifier: "node:stream/consumers",
    source: "facades/node-stream-consumers.js",
    declaration: "node-stream-consumers.d.ts",
    visibility: "public",
    hostPrimitives: [],
  },
  {
    specifier: "node:stream/promises",
    source: "facades/node-stream-promises.js",
    declaration: "node-stream-promises.d.ts",
    visibility: "public",
    hostPrimitives: [],
  },
  {
    specifier: "node:string_decoder",
    source: "facades/node-string-decoder.js",
    declaration: "node-string-decoder.d.ts",
    visibility: "public",
    hostPrimitives: [],
  },
  {
    specifier: "node:sys",
    source: "facades/node-sys.js",
    declaration: "node-sys.d.ts",
    visibility: "public",
    hostPrimitives: [],
  },
  {
    specifier: "node:timers",
    source: "facades/node-timers.js",
    declaration: "node-timers.d.ts",
    visibility: "public",
    hostPrimitives: [],
  },
  {
    specifier: "node:timers/promises",
    source: "facades/node-timers-promises.js",
    declaration: "node-timers-promises.d.ts",
    visibility: "public",
    hostPrimitives: [],
  },
  {
    specifier: "node:trace_events",
    source: "facades/node-trace-events.js",
    declaration: "node-trace-events.d.ts",
    visibility: "public",
    hostPrimitives: [],
  },
  {
    specifier: "node:tty",
    source: "facades/node-tty.js",
    declaration: "node-tty.d.ts",
    visibility: "public",
    hostPrimitives: [],
  },
  {
    specifier: "node:url",
    source: "facades/node-url.js",
    declaration: "node-url.d.ts",
    visibility: "public",
    hostPrimitives: [],
  },
  {
    specifier: "node:util",
    source: "facades/node-util.js",
    declaration: "node-util.d.ts",
    visibility: "public",
    hostPrimitives: [],
  },
  {
    specifier: "node:util/types",
    source: "facades/node-util-types.js",
    declaration: "node-util-types.d.ts",
    visibility: "public",
    hostPrimitives: [],
  },
] as const satisfies readonly StdlibInput[];

export const STDLIB_FORMAT_VERSION = 1;
export const STDLIB_VERSION = "1";
export const EFFECT_VERSION = "3.16.7";
export const RUNTIME_ABI_VERSION = 1;
export const HOST_ABI_VERSION = 1;
