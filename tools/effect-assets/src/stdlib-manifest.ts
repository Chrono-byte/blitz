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
] as const satisfies readonly StdlibInput[];

export const STDLIB_FORMAT_VERSION = 1;
export const STDLIB_VERSION = "1";
export const EFFECT_VERSION = "3.16.7";
export const RUNTIME_ABI_VERSION = 1;
export const HOST_ABI_VERSION = 1;
