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

export const syncBuiltinESMExports = () => undefined;

const compileCacheStatus = Object.freeze(Object.assign(Object.create(null), {
  FAILED: 0,
  ENABLED: 1,
  ALREADY_ENABLED: 2,
  DISABLED: 3,
}));

export const constants = Object.freeze(Object.assign(Object.create(null), {
  compileCacheStatus,
}));

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
