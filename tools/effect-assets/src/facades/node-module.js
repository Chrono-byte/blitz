// Node 24.18.0's builtin inventory is static in Node. Keep the pinned
// inventory here instead of consulting the host Node process: Blitz may be
// running without Node, and the list describes the targeted compatibility
// release rather than the host that generated these assets.
export const builtinModules = Object.freeze([
  "_http_agent",
  "_http_client",
  "_http_common",
  "_http_incoming",
  "_http_outgoing",
  "_http_server",
  "_stream_duplex",
  "_stream_passthrough",
  "_stream_readable",
  "_stream_transform",
  "_stream_wrap",
  "_stream_writable",
  "_tls_common",
  "_tls_wrap",
  "assert",
  "assert/strict",
  "async_hooks",
  "buffer",
  "child_process",
  "cluster",
  "console",
  "constants",
  "crypto",
  "dgram",
  "diagnostics_channel",
  "dns",
  "dns/promises",
  "domain",
  "events",
  "fs",
  "fs/promises",
  "http",
  "http2",
  "https",
  "inspector",
  "inspector/promises",
  "module",
  "net",
  "os",
  "path",
  "path/posix",
  "path/win32",
  "perf_hooks",
  "process",
  "punycode",
  "querystring",
  "readline",
  "readline/promises",
  "repl",
  "stream",
  "stream/consumers",
  "stream/promises",
  "stream/web",
  "string_decoder",
  "sys",
  "timers",
  "timers/promises",
  "tls",
  "trace_events",
  "tty",
  "url",
  "util",
  "util/types",
  "v8",
  "vm",
  "wasi",
  "worker_threads",
  "zlib",
  "node:sea",
  "node:sqlite",
  "node:test",
  "node:test/reporters",
]);

const builtinSet = new Set(builtinModules);

// `isBuiltin` accepts both the unprefixed inventory names and canonical
// `node:` specifiers. Non-strings are not module specifiers and return false,
// matching Node's permissive runtime behavior.
export const isBuiltin = moduleName => {
  if (typeof moduleName !== "string") return false;
  return builtinSet.has(moduleName)
    || (moduleName.startsWith("node:") && builtinSet.has(moduleName.slice(5)));
};

// Blitz exposes ESM facades rather than mutable CommonJS builtin namespaces.
// There is therefore no CJS namespace to synchronize with these bindings.
export const syncBuiltinESMExports = () => undefined;

// Node 24 exposes only compile-cache status constants from module.constants.
// Blitz has no compile cache, but these documented enum values are still
// meaningful and do not claim that the cache itself is available.
const compileCacheStatus = Object.freeze(Object.assign(Object.create(null), {
  FAILED: 0,
  ENABLED: 1,
  ALREADY_ENABLED: 2,
  DISABLED: 3,
}));

export const constants = Object.freeze(Object.assign(Object.create(null), {
  compileCacheStatus,
}));

// `sourceMapsEnabled` is a `process` property in Node 24, not a
// node:module export; it is intentionally not invented in this facade.
const sourceMapsSupport = {
  enabled: false,
  nodeModules: false,
  generatedCode: false,
};

export const getSourceMapsSupport = () => Object.freeze(Object.assign(Object.create(null), sourceMapsSupport));

const invalidArgument = (name, expected, value) => {
  const received = value === null
    ? "null"
    : value === undefined
      ? "undefined"
      : typeof value === "object"
        ? "an instance of Object"
        : `type ${typeof value} (${String(value)})`;
  const error = new TypeError(`The \"${name}\" argument must be ${expected}. Received ${received}`);
  error.code = "ERR_INVALID_ARG_TYPE";
  throw error;
};

const unsupported = operation => {
  const error = new Error(`node:module.${operation} is not implemented by Blitz`);
  error.code = "ERR_NOT_IMPLEMENTED";
  throw error;
};

// Source-map rewriting is not implemented by the current Blitz stack-trace
// machinery. Disabling support is already the runtime state and is safe; an
// attempt to enable it fails instead of reporting support that has no effect.
export const setSourceMapsSupport = (enabled, options = {}) => {
  if (typeof enabled !== "boolean") invalidArgument("enabled", "of type boolean", enabled);
  if (options === null || typeof options !== "object") invalidArgument("options", "of type object", options);
  if (typeof options.nodeModules !== "undefined" && typeof options.nodeModules !== "boolean") {
    invalidArgument("options.nodeModules", "of type boolean", options.nodeModules);
  }
  if (typeof options.generatedCode !== "undefined" && typeof options.generatedCode !== "boolean") {
    invalidArgument("options.generatedCode", "of type boolean", options.generatedCode);
  }
  if (enabled) unsupported("setSourceMapsSupport");

  sourceMapsSupport.nodeModules = options.nodeModules ?? false;
  sourceMapsSupport.generatedCode = options.generatedCode ?? false;
};

// These APIs require a CommonJS loader or loader hooks, neither of which is
// present in Blitz. Keep the names explicit so callers get a stable failure,
// never a fabricated require function or partially active hook.
export const createRequire = (..._args) => unsupported("createRequire");
export const findPackageJSON = (..._args) => unsupported("findPackageJSON");
export const register = (..._args) => unsupported("register");
export const registerHooks = (..._args) => unsupported("registerHooks");

function Module(id = "", parent) {
  if (!(this instanceof Module)) return new Module(id, parent);
  this.id = id;
  this.path = "";
  this.exports = {};
  this.parent = parent;
  this.filename = null;
  this.loaded = false;
  this.children = [];
  this.paths = [];
}

Object.assign(Module, {
  Module,
  builtinModules,
  constants,
  createRequire,
  findPackageJSON,
  getSourceMapsSupport,
  isBuiltin,
  register,
  registerHooks,
  setSourceMapsSupport,
  syncBuiltinESMExports,
});

export { Module, Module as default };
