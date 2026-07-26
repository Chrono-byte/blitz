/** The Node 24.18.0 builtin module inventory targeted by Blitz. */
export declare const builtinModules: readonly string[];

/** Returns true for a pinned inventory name or its `node:` form. */
export declare function isBuiltin(moduleName: string): boolean;

/** ESM facades have no mutable CommonJS namespace to synchronize. */
export declare function syncBuiltinESMExports(): void;

export interface CompileCacheStatus {
  readonly FAILED: 0;
  readonly ENABLED: 1;
  readonly ALREADY_ENABLED: 2;
  readonly DISABLED: 3;
}

export interface ModuleConstants {
  readonly compileCacheStatus: CompileCacheStatus;
}

export declare const constants: ModuleConstants;

export interface SourceMapsSupport {
  readonly enabled: boolean;
  readonly nodeModules: boolean;
  readonly generatedCode: boolean;
}

export declare function getSourceMapsSupport(): SourceMapsSupport;

export interface SetSourceMapsSupportOptions {
  readonly nodeModules?: boolean;
  readonly generatedCode?: boolean;
}

/** Enabling source maps throws ERR_NOT_IMPLEMENTED; disabling is supported. */
export declare function setSourceMapsSupport(
  enabled: boolean,
  options?: SetSourceMapsSupportOptions,
): void;

/** Throws an Error with code ERR_NOT_IMPLEMENTED; CommonJS is unavailable. */
export declare function createRequire(path: string | URL): never;
/** Throws an Error with code ERR_NOT_IMPLEMENTED; package resolution is unavailable. */
export declare function findPackageJSON(specifier: string | URL, base?: string | URL): never;
/** Throws an Error with code ERR_NOT_IMPLEMENTED; loader hooks are unavailable. */
export declare function register(specifier: string | URL, ...options: unknown[]): never;
/** Throws an Error with code ERR_NOT_IMPLEMENTED; loader hooks are unavailable. */
export declare function registerHooks(options: unknown): never;

export interface ModuleInstance {
  id: string;
  path: string;
  exports: unknown;
  parent: ModuleInstance | undefined;
  filename: string | null;
  loaded: boolean;
  children: ModuleInstance[];
  paths: string[];
}

export interface ModuleConstructor {
  new (id?: string, parent?: ModuleInstance): ModuleInstance;
  (id?: string, parent?: ModuleInstance): ModuleInstance;
  readonly Module: ModuleConstructor;
  readonly builtinModules: typeof builtinModules;
  readonly constants: typeof constants;
  readonly createRequire: typeof createRequire;
  readonly findPackageJSON: typeof findPackageJSON;
  readonly getSourceMapsSupport: typeof getSourceMapsSupport;
  readonly isBuiltin: typeof isBuiltin;
  readonly register: typeof register;
  readonly registerHooks: typeof registerHooks;
  readonly setSourceMapsSupport: typeof setSourceMapsSupport;
  readonly syncBuiltinESMExports: typeof syncBuiltinESMExports;
}

declare const Module: ModuleConstructor;
export { Module, Module as default };
